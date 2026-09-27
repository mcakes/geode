# geode-data

`DataService`, the only door to data in Geode. It discovers sources,
ingests them into one persistent DuckDB database, keeps freshness and
health, and answers queries as immutable columnar `Snapshot`s over a
channel. No other crate opens a file or a socket; modules hold a
`DataHandle` and ask.

This crate depends on `geode-core` alone. It never depends on the shell,
on a module or on a parser crate: document kinds arrive as
`geode_core::document::DocumentKind` trait objects that `geode-app`
registers at startup.

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

View replacements retain the latest configuration even under request-channel
pressure. Shutdown and final-handle drop join workers and must run off the UI
thread. Admission, cancellation, and completion have distinct guarantees; see
[requests and UI delivery](../../docs/current/request-delivery.md).

The request loop contains each request's arm and the view-replacement step:
a panicking request is answered once, with `<kind> request panicked:
<payload>`, through the route that answers its success, and the loop serves
on. Read, pricing, ingest, and egress paths contain panics at their operation
boundaries; an ingest load panic reports that operation as `Failed`, and egress
contains encoding and transport separately on the target's worker. Identity
listings, the stale check, the local sweep, discovery, and result-event
building report their panics as error diagnostics, health, or the key's
error. Containment does not interrupt blocked adapter or filesystem calls.

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
| `service` | `DataService`, `DataServiceConfig`, `DataEvent`, and the `HealthTracker` (two lanes per source, `discovery` and `load`; the worse by `severity_rank` wins). |
| `handle` | `DataHandle`, `Refusal`, and `Request`: queries, distinct values, the catalog, a document by key, a series fetch, identities, an upload, a local publish, and a local forget. |
| `source` | Directory discovery, sentinel parsing, and readiness classification. Configuration types are shared with `geode-core`; stable-mtime readiness is accepted by configuration but unsupported at runtime. |
| `adapter` | Subscription, upload, and fetch capabilities; a registry, bounded message sink, and topic matching. Includes the in-process `ChannelAdapter`; the app can register additional implementations such as its demo series adapter. |
| `ingest` | The discovery scheduler, the cold-start priority ladder, the per-file load pipeline, the grain split and conflict detector, the ingest runner (one thread, one writer connection, three queues), the subscribed-source receiver, the `Coalescer`, and the fetch worker. |
| `store` | The DuckDB store: DDL generated from the schema, the per-file publish transaction and backfill guard, document publish, the series family's bitemporal append (`append_series`, the one door series rows enter by), retention, and the freshness catalog in source time. |
| `query` | The query path: scope to bound SQL, the grain-aware view compiler, the read pool (latest-wins per key, stale results dropped), as-of routing against the archive, the picker's distinct values, the document request, the catalog request. A scope still carrying a named-expression reference (`Scope.named` nonempty) is refused with `StoreError::Scope` rather than compiled — resolving a name is the shell's job, before a query ever reaches here. |
| `documents` | The `DocumentKind` registry the app fills. |
| `egress` | Startup target resolution and per-target workers that encode and send, with eight waiting jobs. Refusals answer from the service thread; encoding and transport results, including contained panics, answer from the worker as keyed/tagged upload outcomes. |
| `health` | Re-export of `geode_core::health::Health`. |

## Features

- `test-support` exposes `DataHandle::for_tests()`,
  `DataHandle::fill_for_tests()` (the next submission is refused `Busy`), and
  the other service-thread-free fixtures the module crates' tests use.

## Commands

```sh
cargo test -p geode-data
cargo bench -p geode-data      # ingest, query, publish_document, append_series
zsh scripts/mutation-check.sh  # run after touching the compiler, scope lowering,
                               # as-of routing, publish, retention or discovery
```

The requery budget and current reference measurements are in
[`docs/current/performance.md`](../../docs/current/performance.md).

## Rules this crate pins

The current contracts and their reasons are in
[`docs/current/data-path.md`](../../docs/current/data-path.md). The ones most
often tripped:

- `SUM` never double-counts: measures are split by grain at ingest and
  aggregated at their own grain by the compiler.
- A view that `ViewSpec::validate` reports an error for is refused by name, not
  compiled: `query` answers with that view's first error message. The check sits
  after the unknown-view lookup and before the grouping override, so an unknown
  view keeps its own message and a regroup cannot slip past a refusal. `open` and
  `replace_views` share one `validate_views`, and the reload replaces the refusal
  set rather than merging into it, so a view corrected in the configuration serves
  again without a restart. A refused view stays registered so the dialogs can fix
  it.
- The view compiler errors rather than dropping a join it cannot honour: an
  unknown join dataset, or keys no grain of the joined dataset carries. The one
  remaining `continue` in that loop is a depth fact, not a configuration error —
  a join runs only at depths whose spine materializes its keys, and at a coarser
  depth the joined columns are left out of the statement entirely while the
  grouping key itself is NULL from a separate path. A measure's aggregate comes
  from its declared role with no fallback, so a non-measure column cannot reach
  one.
- An ungrouped dimension column is computed by the unanimity rule (value, mixed,
  or blank; never `any_value`) from the grain `ViewSpec::ungrouped_dimensions`
  names, joined like a measure aggregate but never feeding the spine. A view
  declaring none compiles to its old statement byte for byte, pinned by
  `testdata/demo_tree_view.sql`.
- Health is keyed by source, never by dataset; deciding and emitting a
  transition are one step under the lock.
- `apply_schema` is `CREATE TABLE IF NOT EXISTS` and publish moves rows
  positionally. A column change in `datasets.toml` against an existing
  database is not migrated; delete the database first.
- Series timestamps are naive UTC, bound and read as epoch micros, so no
  session time zone can shift them.
- A green suite can miss wrong-data behavior when its fixture cannot reach
  the branch. Add a targeted mutation entry for every changed correctness
  contract.
- Generation summaries cover all live/archive pairs, including NULL-book
  partitions. As-of selection and retention break source-time ties by the
  greatest generation ID so corrected republishes win consistently.
- Provenance names the generation a read actually used, because source time
  alone cannot see a corrected republish. The catalog answers the live cases:
  `live_generation` for one partition's newest and `dataset_generation` for a
  whole dataset's, the maximum where `dataset_as_of` takes the minimum, since
  this field reports whether the data changed rather than how stale it is. A
  historical document read reports the `gen_id` it pinned and a historical view
  read reports none, its era having resolved one generation per partition.
  `latest_gen_id` stays an internal sequence helper: `ensure_tables` reads it
  once to start the generation ID sequence above recorded history, and nothing
  else calls it. A load allocates from that sequence through `reserve_gen_id`
  and `record` stores the ID it was given. `latest_gen_id` aggregates the whole
  catalog, so it names no partition and is not a freshness answer.
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
- Discovery compares path, size, and source time, not CSV contents. Glob and
  CSV metadata errors are currently skipped, so an empty poll does not prove
  path accessibility. Adapter queue admission likewise does not acknowledge
  storage publication. See [source discovery and adapters](../../docs/current/data-path.md#source-discovery-and-adapters).
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
