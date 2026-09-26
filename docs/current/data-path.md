# The data path

This guide describes the current data path: how data enters Geode, becomes
queryable, and reaches a tile. It covers the contracts shared by file,
document, and series sources. The [geode-data crate README](../../crates/geode-data/README.md)
maps these contracts to modules.

## Boundary and flow

`DataService` is the application's door to stored data. Modules submit
requests through the cloneable `DataHandle`; they do not own DuckDB
connections or source transports. This keeps I/O and query work off the UI
thread and gives the app one place to enforce backpressure, health, and
delivery semantics.

```text
file / subscribed document / fetched series
                │
                ▼
     discovery or adapter worker
                │
                ▼
       one ingest writer ─────► persistent DuckDB
                                      │
tile ─► DataHandle ─► DataService ─► read pool
  ▲                                   │
  └────────── DataEvent ◄─────────────┘
```

Geode serializes publication on one writer connection. Independent read
connections serve queries. The ingest runner gives parsed documents priority
over fetched series, and both priority over queued files; it finishes a file
already in flight before taking another job. This puts trader-requested work
ahead of background discovery without attempting concurrent writes.

Ordinary requests use a bounded channel. `DataHandle` returns `false` when
submission was refused; callers must handle that rather than wait for a reply.
Acceptance is not completion: startup failure, cancellation, and supersession
can leave an admitted request without an individual outcome. View replacements
use a separate latest-value mailbox. The app's event sink must not wait for
the UI or reenter the service because some producers hold a queue lock. See
[requests and UI delivery](request-delivery.md) for admission, cancellation,
reload, shutdown, and event-coalescing contracts.

## Queues and shutdown

The ingestion boundaries have different capacity and replacement rules:

| Boundary | Accepted work and refusal |
|---|---|
| `DataHandle` request channel | Bounded; `try_send` refuses without waiting when full or disconnected. |
| Adapter message sink | Bounded; refused messages are counted and dropped. |
| Subscription coalescer | One pending document per key; newer documents replace it without moving its release deadline. Already submitted jobs are unaffected. |
| Fetch worker | Up to 64 waiting requests per source; a refused fetch is reported as an outcome. |
| Egress worker | Up to 8 waiting uploads per target, behind the one in flight; a refused upload answers `Err("queue full")` at once. |
| Ingest runner | No fixed capacity. Documents and series are FIFO within their queues; files deduplicate by path, size, and source time. |

For queued files, resubmission can promote priority without adding another
job. A catalog check immediately before loading skips work that has already
been published, without starting progress. Within a file priority, newer
source times run first. The runner finishes each operation before selecting
another; sustained document traffic can starve series and files, and sustained
series traffic can starve files. Coalescing reduces repeated documents but
does not bound the writer backlog or the number of distinct document keys.
Fixed staging-table names also require serialized file loads on a store.

Subscription release deadlines drive the receiver's wait, capped at 250 ms
while idle. They are not a hard latency guarantee: parsing and thread
scheduling can delay release. The coalescer retains release timestamps for
observed keys without eviction. Unknown-element warning deduplication stops
growing at 256 paths per source; further unremembered paths can warn repeatedly.

Shutdown stops producers before the ingest writer. Fetch workers drain their
accepted requests and join. Subscription workers unsubscribe, set a stop flag,
and join without flushing documents still held by their coalescers. Discovery
stops polling; the ingest runner finishes its current operation and exits
without draining queued jobs. Submission to the runner itself has no shutdown
refusal, so producer ordering is required. Egress workers close their queue
first (refusing further submissions), then join; jobs already queued still
run and answer, so shutdown can wait on a slow or stuck transport — see
[egress and uploads](#egress-and-uploads) below. Shutdown is not a flush
guarantee. Blocking adapter, parser, or filesystem calls can delay joins;
panic containment does not cancel them. See
[`runner.rs`](../../crates/geode-data/src/ingest/runner.rs),
[`subscribe.rs`](../../crates/geode-data/src/ingest/subscribe.rs), and
[`fetch.rs`](../../crates/geode-data/src/ingest/fetch.rs).

## Ingestion and publication

Directory sources are polled, not watched, because file watches can fail
silently on network shares. Discovery checks readiness through the configured
strategy, including the `.done` sentinel, and skips unchanged generations.
Readiness and discovery gaps are described under
[source discovery and adapters](#source-discovery-and-adapters).

The unit of replacement is a **partition** identified by dataset, batch, and
book, including a possible NULL book. A file can publish several partitions
and grains. For each partition, live data holds one generation. Publishing a
newer generation moves the outgoing rows to archive and installs the incoming
rows in one transaction. An older arrival goes to archive without replacing
live data. This keeps live queries bounded by current data while preserving
history for time travel. The source timestamp from the sentinel orders
generations; filesystem modification time does not. Equal source times allow
a corrected republish to replace live data, with the greater generation ID
winning historical ties. Outgoing rows retain their original ID and source
time when archived.

The file loader publishes all grains, dictionary updates, and catalog metadata
in one transaction after staging. A failed publication rolls back the complete
generation. The lower-level table-pair publisher requires its owner's
transaction when participating in a file or document load. IDs are reserved
from sequences before use; a failed load can consume an ID without reusing it.

Measures are split by declared grain during ingest. The query compiler
aggregates each measure at its own grain before joining grouped results. This
is the central protection against multiplying a coarser measure by the number
of finer rows. Document datasets use their own live/archive table pair;
fetched series use bitemporal append and coverage records instead of replacing
whole file partitions. See [`publish.rs`](../../crates/geode-data/src/store/publish.rs),
[`split.rs`](../../crates/geode-data/src/ingest/split.rs), and
[`series.rs`](../../crates/geode-data/src/store/series.rs).

**Schema limitation:** `apply_schema` creates missing tables but does not
migrate existing payload columns. Publication moves rows positionally, so a
column change against an old database can fail or, for same-typed reorders,
misfile values silently. Rebuild an affected demo database after changing its
schema; production migration needs an explicit procedure. See
[`store/mod.rs`](../../crates/geode-data/src/store/mod.rs) and
[`geode-data README`](../../crates/geode-data/README.md).

## Source discovery and adapters

Directory discovery reads metadata and the JSON sentinel at
`<csv filename>.done`. The sentinel requires `as_of` (RFC 3339, normalized to
UTC) and `columns` (a string array); unknown fields are ignored, while known
fields must have the expected JSON types. The parser does not verify CSV
content or reconcile optional metadata with it. Sentinel books are advisory;
publication derives partitions from staged rows.

| Condition | Discovery result |
|---|---|
| Sentinel metadata unavailable | `Pending`, or `PendingTooLong` when CSV age strictly exceeds the timeout. Metadata errors use the same path as a missing sentinel. |
| Sentinel older than CSV | `Pending` regardless of age; this branch does not apply the timeout. |
| Sentinel metadata available, but text read or parsing fails | `Orphaned`, with the reason. |
| Stable-mtime readiness configured | `Orphaned` for each matched candidate: poll history is not implemented. |
| Latest catalog entry for the path has equal size and source time | `Unchanged`. |
| Otherwise, with a valid current sentinel | `Ready`; the CSV has not yet been parsed or validated. |

CSV modification time affects readiness and pending age; it does not order
generations or participate in unchanged-file detection. Neither does a content
hash. A same-size correction with the same source time is therefore skipped,
even if its bytes changed. Discovery and the runner's pre-load check share
this rule. A committed degraded generation counts as loaded; a rolled-back
load has no new catalog entry.

Invalid glob syntax, glob traversal errors, and CSV metadata errors are
currently skipped. Catalog lookup errors propagate to the scheduler. An
empty discovery result therefore cannot establish that every path was
accessible. See [`discovery.rs`](../../crates/geode-data/src/source/discovery.rs)
and [`sentinel.rs`](../../crates/geode-data/src/source/sentinel.rs).

Adapters expose independent subscription, upload, and fetch capabilities.
The app registers them by name; a duplicate registration warns and replaces
the earlier adapter. A message sink's successful push acknowledges queue
admission only. Full or disconnected queues refuse, drop, and count the
message. Connection health is separate from message delivery and stored-data
health. Fetch implementations must supply their own timeouts for both history
and identity enumeration; the worker provides no cancellation or deadline.

`ChannelAdapter` is the in-process bus used by demos and integration tests.
Its bounded inbound queue and each bounded subscription queue can refuse
independently. The dispatcher starts on the first subscription and delivers
once per matching registration. Message and health fan-out copy their target
handles under the registration lock, then release it before delivery.
Unsubscribe removes future interest but cannot retract a delivery snapshot
already taken.

Feeds and outstanding upload handles keep the channel open. Dropping the last
sender lets the dispatcher drain and exit; its thread is not joined. The bus
cannot reopen, and closing it does not emit `Lost` or remove subscriptions.
`ChannelFeed::set_state` synchronously notifies current subscribers without
retaining state or changing message flow; new subscriptions report `Connected`.
Concurrent state notifications have no ordering guarantee, and health
callbacks must return promptly without panicking. See
[`adapter/mod.rs`](../../crates/geode-data/src/adapter/mod.rs) and
[`channel.rs`](../../crates/geode-data/src/adapter/channel.rs).

## Egress and uploads

An upload writes a document back out through a configured target's adapter —
the mirror of a subscribed source's inbound path. `egress.toml` (see
[configuration](configuration.md#egress-configuration)) resolves at startup
against the adapter registry; `DataService::open` then spawns one worker
thread per surviving target (`geode-egress-<name>`), each owning that
adapter's own `Egress` handle. `Adapter::egress()` returns a fresh handle on
every call: `egress::resolve` calls it once just to probe availability at
startup, and spawning the worker calls it again to build the handle the
worker keeps, so an adapter whose transport is not safely shared across
owners (`ChannelAdapter` upgrades its own weak reference into a fresh handle
holding a strong sender clone) never has to serve two callers from one
instance.

A worker drains its target's queue strictly in submission order, one upload
at a time: a slow or stuck transport blocks only that target's own uploads,
never the request loop or another target's worker. The queue holds up to
`EGRESS_QUEUE_BOUND` (8) jobs waiting behind the one in flight; past that a
submission answers `Err("egress '<target>': queue full")` at once rather
than waiting.

Every submitted upload answers exactly one `DataEvent::Upload(UploadOutcome)`,
echoing the requester's key and tag. A refusal decided on the service thread
— an unknown target, a target whose `documents` does not accept the
requested document, an unregistered document kind, a `DocumentKind::write`
failure, or a full or stopped queue — answers synchronously, before anything
reaches a worker thread. An accepted job answers from its target's worker
once the transport call returns. Every `Err` is prefixed `egress
'<target>': ` and names the specific reason, so the requesting tile can
report a failure without knowing the target's configuration.

The document's key selects the write address: `EgressSpec::address`
substitutes the document key's parts, joined by `/`, for `{key}` in the
target's configured template; a template with no `{key}` is one fixed
address for every key of that document.

Shutdown closes every target's queue, refusing further submissions, then
joins every worker thread — jobs already queued still run and answer before
their worker exits, so shutdown can wait on a slow or stuck transport; run
it off the UI thread, as every other `DataService` shutdown. See
[`egress.rs`](../../crates/geode-data/src/egress.rs).

## Queries and time travel

A view query compiles scope predicates and grouping into one statement for
all tree depths. Each measure is aggregated at its own grain, then joined at
the grouping cardinality. The result is an immutable columnar `Snapshot`:
expanding a tree node works on the prepared result rather than issuing another
database query. User supplied scope values are bound as parameters.

The [typed-document reference](typed-documents.md) describes schema and view
validation, grain meaning, scope composition, and checks deferred to query
compilation. A typed reader returning a value does not prove every requested
query can be served by the dataset's actual storage grains.

The read pool coalesces by the **caller's key**, usually a tile, rather than
by view name. Two tiles showing one view therefore do not supersede each
other. A newer request interrupts an older one for the same key; request and
result tags let the receiver discard a stale arrival. A failed or refused
delivery does not stop a worker. See [`compile.rs`](../../crates/geode-data/src/query/compile.rs)
and [`pool.rs`](../../crates/geode-data/src/query/pool.rs).

Live queries read live tables directly. An as-of query resolves, for **each
partition**, the newest generation whose source time is at or before the
requested instant, then reads the appropriate live or archive rows. A
corrected generation at the same source time wins by generation ID. The
`generations` summary table makes resolution cheap; publication and
retention maintain it transactionally. The displayed freshness must reflect
the generation actually selected, not merely the time requested. See
[`as_of.rs`](../../crates/geode-data/src/query/as_of.rs) and
[`catalog.rs`](../../crates/geode-data/src/store/catalog.rs).

The summary covers every live and archive table owned by the dataset. A
partition may appear at only one grain, and its newest generation may exist
only in live. Historical reads filter both sides by the resolved identities.
The predicate combines a generation-ID list for scan pruning with an exact
`(batch, book, gen_id, source_time)` tuple match. NULL books remain matchable;
source time disambiguates stored history containing reused legacy IDs.
Resolution errors propagate instead of silently dropping partitions.

Each joined dataset resolves independently and records its oldest selected
source time. Reporting the requested instant for both sides would conceal a
stale input. Grain attribution likewise follows actual inputs: derived
dimensions resolve to their source columns, and derived measures inherit
their inputs' attribution. Non-attributable results must return NULL, with
validity preserved through `Snapshot`, as well as carry the attribution marker.

## Retention and maintenance

The live/archive retention API works per table pair and partition, so a busy
book cannot evict a quiet book's history. If both count and source-age limits
are supplied, a generation must satisfy both to survive. Source-time ties
use the same descending generation-ID order as historical resolution.

Each sweep owns one transaction covering archive eviction and summary
reconciliation. Reconciliation checks every declared live/archive pair even
when the caller evicts from only one pair: a generation surviving at another
grain must remain resolvable. NULL-safe comparisons prevent bookless rows from
disabling eviction or losing their summary entries. A sweep failure rolls
back earlier pair deletions.

The connection must be outside a transaction and all declared pairs must
exist. Sweeping on the ingest writer delays publication for the duration of
the call. Checkpointing is a separate operation that can also stall writes.
`SweepReport::oldest_remaining` covers only the swept archive tables; it does
not promise complete history across all grains and partitions.

The application does not schedule live/archive sweeps for measure datasets or
feed-published documents; the API is called only by tests for those. Local
documents (`local = true`) are the exception: they keep 200 archived
generations per document (`LOCAL_KEEP_GENERATIONS`; with the live one, at most
201), with no age limit. After each successful local publish the ingest writer
counts the saved document's generation summary rows; only when that document
has crossed the bound does it sweep the whole dataset, then delete, in a
separate transaction, the dataset's `file_books`/`file_generations` rows whose
generation the summary no longer holds. An ordinary autosave therefore costs
one summary count. The sweep runs after the publish committed and its outcome
was sent, so a failure is logged and never turns a stored save into a failed
one; that document's next save retries, since it is still past the bound.
Sweeps of other datasets (tests only) leave evicted generations' provenance
rows in place.

A local save is always published live. Local saves are stamped with the wall
clock, which can step back; the writer moves a save stamped at or before the
document's live source time to one microsecond past it, so the backfill guard
never archives the app's latest save while still answering `LocalPublished`.

**Forgetting a local document.** `DataHandle::forget(LocalForget)` deletes one
document's whole history: its live and archived rows, its generation summary
rows, and its `file_generations`/`file_books` provenance, in one writer
transaction, then rebuilds the dataset's categorical dictionaries. The service
refuses a dataset that is not `local` or a key of the wrong arity with an error
diagnostic and queues nothing; the runner refuses a non-local dataset again,
since `ForgetJob` is a public door onto the writer. An accepted forget joins
the documents FIFO, so it runs after every publish queued before it,
including a save of the same key. It answers `DataEvent::Forgotten`, also
for a key that held nothing, or `DataEvent::ForgetFailed` beside an error
diagnostic. A forget has no progress or health lane.

Series retention is separate and runs for the affected `(source, identity)`
pair inside each append transaction. See
[`retention.rs`](../../crates/geode-data/src/store/retention.rs) and
[`series.rs`](../../crates/geode-data/src/store/series.rs).

## Freshness, health, and delivery

Freshness is measured in source time. A book is as fresh as its stalest
contributing file, and a view with multiple inputs is as fresh as its stalest
input. That avoids labeling a partial or joined answer with the newest
contributor's timestamp.

Health is keyed by **source**. Discovery and load outcomes occupy separate
lanes because a clean, content-blind poll cannot prove that the last publish
was clean. The reported state is the worse lane, with its own explanation;
load health is tracked per batch so a clean batch cannot clear another
batch's degradation. Only a corrected outcome for that batch can do so.
See [`service.rs`](../../crates/geode-data/src/service.rs).

Equal severities choose the most recently changed health/detail pair.
Repeating the same report preserves its change stamp, preventing steady
polling from alternating the displayed reason. Combining and emitting state
happen under the tracker lock; callbacks must not reenter the service or
query pool. A transition is acknowledged only if its health event was
delivered. Refusal leaves it eligible for the next source report, with no
independent retry timer. Publication and health delivery are attempted
independently. The scheduler reports every poll, leaving transition
deduplication to this shared tracker.

Adapter connection state uses the discovery lane. Content failures use the
load lane: document keys for parsed documents, raw topics for parse failures,
and `identity@source` for fetches. A message that parses, validates, and stamps
successfully clears an earlier failure under its raw topic; publication then
reports under the document key. Local document writes have no configured
source-health lane. Instead, a local publish also answers its writer by
dataset and document key: `DataEvent::LocalPublished` with the generation ID
beside `Published`, or `DataEvent::LocalPublishFailed` with the reason beside
the error diagnostic. Load entries have no eviction policy, and unresolved
raw-topic failures have no fixed cap; memory can grow with distinct names.

At startup, persisted unhealthy file generations seed the load lane. An
unchanged file need not reload, so its retained degradation must survive a
restart. Live-health resolution excludes archive-only arrivals, uses the
greatest generation ID for source-time ties, and reports each batch's worst
book. It has no host-clock cutoff because live queries also serve
future-stamped source data. Unknown health labels and generations without a
file-catalog record do not establish a health report.

The catalog does not record which configured source owns a file generation.
Sources sharing a dataset therefore receive the same persisted degradation
at startup, which can conservatively over-report a source's load health.
Seeds use publication's batch key so a corrected load can clear them.

Non-local publication events advance matching dataset/document watches; local
publications update diagnostics without advancing frame revisions. Query
results are addressed to the requesting key. Series fetch completion is
broadcast to visible occupants by `(identity, source)` so modules watching
that pair can react, including when a fetch appended zero rows. See
[window routing](request-delivery.md#routing-into-the-window) for delivery
filters and catalog refresh behavior.

Ingest and discovery workers continue after event refusal and do not retry
individual events. The app mailbox retains terminal outcomes and publication
invalidations until drained; an alternate sink must provide its own delivery
policy. Ingest progress starts only for work that will run and ends after an
operation or queue drain. Local document writes omit the start event, so
autosave does not activate progress. A poll's `ready` count is the plan size
before runner deduplication; its next-poll time is an estimate, falling back
to the report time if adding the interval overflows.

## Limits and verification

- The demo database is not automatically migrated after schema changes.
- Historical as-of depends on retained generations. Measure and feed-published
  document archives currently have no automatic retention sweep; local
  documents keep 200 archived generations each.
- Maintained budgets and known gaps are in
  [performance.md](performance.md); raw conditions and runs are in the
  measurement log.
- Ordinary tests verify outcomes. Targeted mutations in
  [`mutation-check.sh`](../../scripts/mutation-check.sh) check whether tests
  can detect particular wrong-data behaviors; `--anchors-only` validates
  their source anchors without running Cargo.

The code linked above is the implementation authority. If this guide and the
code disagree, correct the guide and assess whether the behavior is an
unintended regression.
