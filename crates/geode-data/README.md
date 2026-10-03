# geode-data

`DataService` owns data-source ingestion, persistent DuckDB storage, and
query execution. Modules submit requests through `DataHandle`; results,
publication events, and health updates return through an event sink. Source
transports and database connections stay outside the UI modules.

Its only production workspace dependency is `geode-core`. The app supplies
concrete document parsers, adapters, and pricers through shared traits, keeping
the data crate independent of the shell and feature modules.

Current behavior and rationale:
[`docs/current/data-path.md`](../../docs/current/data-path.md).

## Threading

The ingest runner owns the only writer connection. `DataService` runs the
request loop with its own reader for catalog and coverage lookups; the
query pool has independent readers. `DataHandle` is the `Clone + Send + Sync`
door: request submission uses `try_send` and returns `Result<(), Refusal>`
rather than waiting. `Refusal::Busy` (a full queue; a later submission can
succeed) is counted in `dropped_requests`; `Refusal::Stopped` (the request
loop panicked, failed to open, or was shut down; no retry can succeed) is
not. The handle's stopped flag is set on a failed open and as the loop
unwinds, before it joins its workers, so a submission racing a dying loop is
refused rather than admitted to a queue nothing will read. Results and health come back as
`DataEvent`s through an `EventSink`; a sink returning `false` means "not
delivered" and no producer stops on it.

`DataHandle::set_context_columns` replaces the context columns every later
view query carries as `ViewSpec::context` (hidden unanimity columns the shell
reads a row's dimension context from; see
[data path](../../docs/current/data-path.md)); `context_columns` reads them
back. `geode-app` sets them from the module roster at startup.

View replacements retain the latest configuration even under request-channel
pressure. Shutdown and final-handle drop join workers and must run off the UI
thread. Admission, cancellation, and completion have distinct guarantees; see
[requests and UI delivery](../../docs/current/request-delivery.md).

The request loop contains each request's arm and the view-replacement step:
a panicking request is answered once, with `<kind> request panicked:
<payload>`, through the route that answers its success, and the loop serves
on. Read, pricing, ingest, egress, and position-command paths contain panics
at their operation boundaries; an ingest load panic reports that operation as
`Failed`, egress contains encoding and transport separately on the target's
worker, and the position worker answers a panicking command and goes on.
Identity listings, the stale check, the local sweep, discovery, and
result-event building report their panics as error diagnostics, health, or
the key's error. Containment does not interrupt blocked adapter or filesystem calls.

Every long-lived thread is spawned through `supervise::spawn_supervised`,
which declares an unwinding body once as `DataEvent::ThreadStopped { thread,
reason }` and never restarts it; the crash file is still written. A new
long-lived thread must use it, or its death is silent. Two threads are
deliberately outside it: the channel adapter's dispatcher
(`geode-channel-<name>`) and `geode-app`'s demo bus. They are transport-tier
threads standing in for a vendor client's own threads, which Geode will not
own either, and are created without an event sink. See
[containment and liveness](../../docs/current/data-path.md#containment-and-liveness).

The bounded request and adapter channels do not bound the ingest queues.
Documents precede series, which precede files, with no preemption of running
work. Sustained higher-priority traffic can starve lower-priority jobs.
See [queues and shutdown](../../docs/current/data-path.md#queues-and-shutdown)
for capacity, coalescing, and worker shutdown behavior.

## What lives here

| Module | Holds |
|---|---|
| `supervise` | `spawn_supervised`, the one door for long-lived data threads, and `REQUEST_LOOP` (`geode-data`), the request loop's thread name. |
| `service` | `DataService`, `DataServiceConfig`, `DataEvent`, and the `HealthTracker` (two lanes per source, `discovery` and `load`; the worse by `Health::severity` wins). |
| `handle` | `DataHandle`, `Refusal`, and `Request`: queries, distinct values, the catalog, a document by key, a series fetch, identities, an upload, a Move LHU command, a local publish, and a local forget. |
| `source` | Directory discovery, sentinel parsing, and readiness classification. Configuration types are shared with `geode-core`; stable-mtime readiness is accepted by configuration but unsupported at runtime. |
| `adapter` | Subscription, upload, fetch, and position-command (`PositionCommands`, through `Adapter::positions`, `None` by default) capabilities; a registry, bounded message sink, and topic matching. Includes the in-process `ChannelAdapter`; the app can register additional implementations such as its demo series adapter. |
| `ingest` | The discovery scheduler, the cold-start priority ladder, the per-file load pipeline, the grain split and conflict detector, the ingest runner (one thread, one writer connection, three queues), the subscribed-source receiver, the `Coalescer`, and the fetch worker. |
| `store` | The DuckDB store: DDL generated from the schema, the per-file publish transaction and backfill guard, document publish, reference snapshot publish (an unchanged snapshot is skipped) and read, the series family's bitemporal append (`append_series`, the one door series rows enter by), retention, the freshness catalog in source time, and the payload-table drift check made at open. |
| `query` | Scope lowering, grain-aware view compilation, distinct values, document and series queries, catalog reads, and the read pool. View/document planning, provenance, and execution share a worker transaction; superseded results are dropped. |
| `pricing` | App-supplied pricer registry and a separate bounded worker queue. Queued batches coalesce by key; cancellation stops a running batch at the next line boundary. |
| `vol` | App-supplied vol model registry and a bounded worker queue shaped like `pricing`'s: batches coalesce by key, cancellation stops a running batch at the next job boundary, a panicking job fails alone. A `Grid::Job(j)` slice is resolved to the strikes earlier job `j` evaluated at, or fails naming why; `evaluate` runs a batch in place under the same rules. |
| `documents` | The `DocumentKind` registry the app fills. |
| `egress` | Startup target resolution and per-target workers that encode and send, with eight waiting jobs. Refusals answer from the service thread; encoding and transport results, including contained panics, answer from the worker as keyed/tagged upload outcomes. |
| `positions` | Startup resolution of the one position service and its `PositionWorker`: one supervised thread (`geode-positions`), one command at a time, eight waiting. Refusals (`no position service configured`, `position service unavailable: …`, `position service busy`, `position service stopped`) are decided synchronously and answered from the service thread; the worker answers each command once, a contained transport panic as `position service panicked`. Every admitted command answers one `DataEvent::Command`. See [position commands](../../docs/current/data-path.md#position-commands). |
| `health` | Re-export of `geode_core::health::Health`. |

## Features

- `test-support` exposes `DataHandle::for_tests()`,
  `DataHandle::fill_for_tests()` (the next submission is refused `Busy`), and
  the other service-thread-free fixtures the module crates' tests use.

## Commands

```sh
cargo test -p geode-data
cargo bench -p geode-data      # ingestion, document/series writes, view/series queries
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh --changed  # mutations for changed source files
```

The requery budget and current reference measurements are in
[`docs/current/performance.md`](../../docs/current/performance.md).

## Rules this crate pins

The current contracts and their reasons are in
[`docs/current/data-path.md`](../../docs/current/data-path.md). The ones most
often tripped:

- `SUM` never double-counts: measures are split by grain at ingest and
  aggregated at their own grain by the compiler.
- View column metadata marks only plain `sum` measures as summable.
  Selection summaries also check per-cell attribution: a value belonging to
  a row does not imply that its column can be totalled across rows.
- A view that `ViewSpec::validate` reports an error for is refused by name, not
  compiled: `query` answers with that view's first error message. The check sits
  after the unknown-view lookup and before the grouping override, so an unknown
  view keeps its own message and a regroup cannot slip past a refusal. `open` and
  `replace_views` share one `validate_views`, and the reload replaces the refusal
  set rather than merging into it, so a view corrected in the configuration serves
  again without a restart. A refused view stays registered so the dialogs can fix
  it.
- The view compiler errors for a required join with an unknown dataset or
  keys no grain carries, and skips such optional joins. A join runs only when
  the query's materialized grouping contains all its keys; at coarser depths
  its selected attributes are absent and deeper grouping keys are NULL.
  Measures use the aggregate from their declared role, with no fallback for
  columns of another role.
- Scope lowering and distinct queries refuse unresolved named-expression
  references with `StoreError::Scope`. The shell resolves names before
  submission so a missing definition cannot silently widen a query.
- Ungrouped primary dimensions use a unanimity aggregate: the common value
  when all contributing rows agree, `mixed` when they disagree (including a
  value alongside NULL), and blank when all values are NULL or no rows match.
  The aggregate reads the coarsest declared grain carrying the column and
  the whole grouping. It joins the tree spine without changing tree rows.
  Views without these dimensions retain the SQL shape checked by
  `testdata/demo_tree_view.sql`.
- Health is keyed by source, never by dataset; deciding and emitting a
  transition are one step under the lock.
- Source-wide conditions use their own load-lane keys from
  `health::condition_key` (`<source>:queue`, `<source>:backlog`) so each is
  reported and cleared alone; health events still carry the source name.
- The runner's document and series queues have no fixed capacity; a source
  past `BACKLOG_DEPTH` (64) queued feed documents and series reports
  `<source>:backlog` (`Degraded "ingest backlog N"`, re-reported at each
  further 64, `Ok` once that source's count falls below 64); `N` is the count
  at the last crossing (65, 129, …), not a live count, and holds while the
  queue drains until the clear. Local writes are not counted.
- Load notes (`LoadNotes`: extra source columns, absent optional ones) ride
  `IngestEvent::Published`; the service warns once per distinct combination,
  up to 256, then once more naming the source and file that reached the cap.
- `apply_schema` is `CREATE TABLE IF NOT EXISTS` and publish moves rows
  positionally, so open compares every existing payload table with its
  declaration (`store::drift`) and refuses a drifted dataset for the run:
  sources `Failed`, writes refused at the runner, reads refused at the
  service, one error diagnostic. Delete the table or fix the dataset, then
  restart.
- Series timestamps are naive UTC, bound and read as epoch micros, so no
  session time zone can shift them.
- A green suite can miss wrong-data behavior when its fixture cannot reach
  the branch. Add a targeted mutation entry for every changed correctness
  contract.
- Generation summaries cover all live/archive pairs, including NULL-book
  partitions. As-of selection and retention break source-time ties by the
  greatest generation ID so corrected republishes win consistently.
- Document provenance reports the selected partition's generation ID, for
  live and historical reads. This distinguishes corrected republishes that
  share a source time. Live views report `dataset_generation`, the maximum
  live-published ID across the dataset; their freshness time instead reports
  the stalest input. Historical views have no scalar generation identity.
  `None` means unknown, not unchanged. See
  [freshness and provenance](../../docs/current/data-path.md#freshness-health-and-delivery).
- `reserve_gen_id` allocates publication IDs from the store sequence, and
  `record` stores the supplied ID. `latest_gen_id` reads the catalog-wide
  maximum for sequence initialization; it is not a read's freshness marker.
- Live/archive retention has a transactional storage API but no production
  scheduler for measure or feed-published document datasets. Local datasets
  are swept on the writer after a local publish takes its document past
  `LOCAL_KEEP_GENERATIONS` (200) archived generations, and the evicted
  generations' provenance is pruned with it. A local save is always published
  live: the writer moves a save stamped at or before live to just past it. Series
  retention runs inside append transactions. See the
  [maintenance contract](../../docs/current/data-path.md#retention-and-maintenance).
- Only a `local = true` dataset can be written or forgotten from the app.
  `forget_document` deletes a document's rows, generation summary and
  provenance in one transaction and runs only on the ingest writer, in the
  same FIFO as document publishes, so a forget queued after a save of the
  same key deletes that save too. A refusal is an error diagnostic plus the
  write's failure outcome; nothing runs.
- Every admitted local publish answers exactly once, `LocalPublished` or
  `LocalPublishFailed`, and every forget `Forgotten` or `ForgetFailed` —
  including one the service refuses before queuing it. They are addressed by
  dataset and document key (there is no requester key), in addition to
  `Published` and the error diagnostics every consumer already reads. The
  app counts its queued saves on that.
- Ingest shutdown runs the queued local writes (`local`-source publishes and
  forgets) in order before the runner stops, each answering as usual, and
  drops every other queued job. A caller that stops waiting for the join can
  still exit with a write running.
- Discovery compares path, size, and source time, not CSV contents. A pattern
  that matches nothing has its literal prefix opened once; a missing,
  non-directory or unreadable prefix, or an invalid pattern, is a `Degraded`
  source. Traversal errors below a readable prefix and CSV metadata errors
  are skipped. Adapter queue admission does not acknowledge storage
  publication. See [source discovery and adapters](../../docs/current/data-path.md#source-discovery-and-adapters).
- Upload channel admission, transport success, and a stored echo are separate
  events. Service-thread validation precedes each target's bounded FIFO worker
  queue; the worker encodes (`kind.write`) and sends. Write errors therefore
  answer after queue admission: a bad document occupies a queue slot until the
  worker reaches it, and one sent to a full or unavailable target answers
  `queue full` or the unavailable reason instead. Refusals, write errors,
  transport returns, and contained encoding or transport panics emit outcomes;
  a panic leaves the worker available for later jobs. Startup failure, blocked
  calls, a worker dying outside its boundaries (queued jobs go unanswered), and
  sink refusal can prevent delivery. Uploads have no keyed cancellation or
  automatic retry.
- Adapter resolution and worker creation request separate egress handles.
  Shutdown closes worker queues and joins after queued jobs run; a stuck
  transport can block shutdown. See
  [egress and uploads](../../docs/current/data-path.md#egress-and-uploads).
