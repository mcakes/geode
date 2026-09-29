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

Ordinary requests use a bounded channel. `DataHandle` returns `Err(Refusal)`
when submission was refused — `Busy` for a full queue, `Stopped` once the
request loop has ended — and callers must handle that rather than wait for a
reply.
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
| `DataHandle` request channel | Bounded; `try_send` refuses `Busy` without waiting when full, and `Stopped` when the request loop has ended or admission is closed. |
| Adapter message sink | Bounded; refused messages are counted and dropped. |
| Subscription coalescer | One pending document per key; newer documents replace it without moving its release deadline. Already submitted jobs are unaffected. |
| Fetch worker | Up to 64 waiting requests per source; a refused fetch is reported as an outcome. |
| Egress worker | Up to 8 waiting uploads per target, behind the one in flight; queue refusal emits an upload error naming the target. |
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
stops polling; the ingest runner finishes its current operation, then runs
the queued local writes (`local`-source publishes and forgets) in queue order,
each answering its writer as usual, and exits. Every other queued job — feed
documents, series, files — is dropped; its source resends it after a restart.
The local writes are the user's last edits (the pricer saves every unsaved
sheet at quit, before the data service is told to stop), which nothing would
resend. Submission to the runner itself has no shutdown refusal, so producer
ordering is required. Egress workers close their queue
first (refusing further submissions), then join; jobs already queued still
run and answer, so shutdown can wait on a slow or stuck transport — see
[egress and uploads](#egress-and-uploads) below. Shutdown is not a flush
guarantee: the app's quit hook runs the shutdown on the background executor,
and gpui waits for quit hooks only up to its `SHUTDOWN_TIMEOUT` (200 ms). A
local write still running or queued when the process exits is lost; DuckDB's
write-ahead log keeps the database consistent, at the previous generation. Blocking adapter, parser, or filesystem calls can delay joins;
panic containment does not cancel them. See
[`runner.rs`](../../crates/geode-data/src/ingest/runner.rs),
[`subscribe.rs`](../../crates/geode-data/src/ingest/subscribe.rs), and
[`fetch.rs`](../../crates/geode-data/src/ingest/fetch.rs).

## Containment and liveness

No contained panic in the data layer ends as only a log line: a panicking
request is answered, a dying thread is declared, and a refused submission says
whether a retry can succeed.

### Supervised threads

Every long-lived data thread is spawned through
[`supervise::spawn_supervised`](../../crates/geode-data/src/supervise.rs): the
request loop (`geode-data`), the ingest writer (`geode-ingest`), discovery
(`geode-discovery`), each read-pool worker (`geode-query-N`), pricing
(`geode-pricing`), the vol worker (`geode-vol`), and one thread per fetch
source (`geode-fetch-<source>`),
subscribed source (`geode-subscribe-<source>`), and egress target
(`geode-egress-<target>`). A body that unwinds past every containment boundary
emits one `DataEvent::ThreadStopped { thread, reason }` carrying the panic
payload, logs an error, and ends. Nothing restarts it, because a panic that
repeats on every request would otherwise crash-loop. The body runs outside the
`contained` marker, so the app's panic hook still writes a crash file for it. A
body that returns — a deliberate shutdown — declares nothing.

A request loop that fails to open emits `ThreadStopped` for `geode-data`
(reason `data service failed to open: <error>`) beside its error diagnostic,
although nothing unwound. The app mailbox keys `ThreadStopped` by thread, so
two threads stopping before one UI drain are both delivered. The status bar
shows every stopped thread until the app restarts; see
[stopped threads and refusals](shell.md#stopped-threads-and-refusals).

### The request loop

The loop contains each request's arm, and the view-replacement step, in its
own boundary. A panicking request is answered exactly once, with the error
`<kind> request panicked: <payload>`, through the route that answers its
success, and the loop goes on to the next request:

| Request | Answer to a panic |
|---|---|
| Query, document | `Query` error for its key and tag |
| Distinct values | `Distinct` error for its key, tag, and column |
| Series | `Series` error for its key and tag |
| Catalog | `Catalog` error for its key and tag |
| Pricing | `Price` outcome with the error on every submitted line |
| Upload | `Upload` error for its key, tag, and target |
| History fetch | The pair's load lane reports `Failed`, then `SeriesFetched` carries the error |
| Local publish | Error diagnostic and `LocalPublishFailed` |
| Local forget | Error diagnostic and `ForgetFailed` |
| Identity refresh, cancellation | One error diagnostic |

A panicking view replacement is one error diagnostic (`view replacement
panicked: …; the previous views stay in force`). The service validates new
views before assigning any of them, so the previous views, dimensions, and
refusals stay in force together.

The answer is sent after the arm's boundary. An event sink that itself panics
while delivering that answer unwinds the loop, which is then declared stopped
like any other thread death.

### Refusals

Submissions return `Result<(), Refusal>`. `Refusal::Busy` (`the data service
is busy`) means the request queue was full; it is counted in
`DataHandle::dropped_requests`, and a later submission can succeed.
`Refusal::Stopped` (`the data service has stopped`) means the request loop has
ended — it panicked, it failed to open, or it was shut down — and no retry can
succeed; it is not counted. The handle's stopped flag is set when open fails
and when the loop starts to unwind, before the dying loop joins its workers.
That join can take as long as the slowest running job; a submission made in
that window is refused `Stopped` instead of being admitted to a queue nothing
will read. A clean shutdown never sets the flag and never emits
`ThreadStopped`; submissions after it are refused `Stopped` because admission
is closed.

### Panics with no requester

Five paths run work nobody is waiting on. Each reports its panic where a
trader can see it:

| Path | Report |
|---|---|
| A fetch source's identity listing | Error diagnostic `identity listing for <source> panicked: <payload>` |
| The pop-time stale check before a file load | Error diagnostic naming the file (`the stale check for <file> could not read the catalog (…); loading it anyway`); counts in the status bar's `data N errors` |
| The local-dataset sweep after a save | Error diagnostic `local sweep panicked: <payload>`; the save it follows is already stored |
| Discovery | The source's health goes `Failed` with reason `discovery panicked: <payload>` |
| Building a read-pool result's event | Built inside its own boundary; a panic answers that key with `result delivery panicked: <payload>` in the result's own kind |

The stale check fails open: a lookup that errors or panics loads the file
anyway, so no rows are lost and the worst cost is a redundant reload of the
same rows as a new generation. A catalog row the lookup cannot read is still
corruption, so the report is an error rather than a warning. A distinct answer missing
its `value` or `n` column is that key's error rather than a panic.

### Limits

- Nothing restarts a stopped thread. The work it served stays undone until the
  app restarts, and the status bar says so for that whole time.
- A request already queued to, or claimed by, a data thread when it dies is
  never answered. The asking tile's loading or in-flight state (a market-data
  upload "in flight", for example) stays until restart; the status bar's
  stopped segment is the signal that it will not resolve.
- The channel adapter's dispatcher (`geode-channel-<name>`) and the demo bus
  thread are not supervised. They are transport-tier threads that stand in for
  a vendor client's own threads, which Geode will not own either, and they are
  created without an event sink. An unwind there leaves the crash file and the
  log, not a status segment.
- Containment does not interrupt a blocked call. A thread stuck in adapter,
  filesystem, or DuckDB I/O is neither stopped nor declared.

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

## Document validation and storage

Document kinds expose a column vocabulary and parse/write functions through
[`DocumentKind`](../../crates/geode-core/src/document.rs). Source startup
checks that kind and dataset have the same column names and types in both
directions. That check does not compare order or validate payload values.
The [document kinds](../../crates/geode-documents/README.md) apply their own
wire-format rules, including required fields and finite numeric values.

Before staging, `DocumentRows::validate` checks key arity and the reserved key
separator, nonempty rows, axis order and types, unique axis tuples, required
values and attributes, and equal column lengths. Values and attributes match
by name. This shared validator does not reject duplicate value or attribute
names or enforce each kind's numeric rules. A parser or caller remains
responsible for producing an unambiguous document. Empty documents are
refused: replacing a live document with no payload rows would leave its new
generation indistinguishable from a missing document.

[`publish_document`](../../crates/geode-data/src/store/document.rs) stages in
the dataset's column order, repeating keys and document-level attributes on
each row. Key parts join with a reserved separator to form the batch; book is
NULL. The single ingest writer owns the shared staging table. Publication
commits rows, categorical dictionaries, and provenance together, using the
same backfill and source-time rules as file publication. Explicit appender
flush errors abort publication rather than silently storing a shorter document.

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

An upload encodes a whole document and sends it through a configured
adapter. Targets resolve at startup from
[`egress.toml`](configuration.md#egress-configuration). Each usable target has
one worker thread and its own `Egress` handle. Adapter resolution probes
`Adapter::egress()` once, then worker creation obtains another handle;
adapters must support repeated capability requests. A worker-start failure
leaves the target unavailable and later requests receive a named refusal.

There are two admission boundaries. `DataHandle::upload` uses the bounded
service channel: an `Err(Refusal)` means nothing was admitted and no outcome
is owed. Once dispatched, the service validates the target and accepted
document name, looks up its `DocumentKind`, and queues the rows, the kind, and
the expanded address on the target worker, without encoding them. The worker
queue holds up to eight waiting jobs behind the running one; a full or stopped
queue is refused at once. The worker takes one job at a time, in submission
order, and runs the encoder (`kind.write`) and then the transport, each inside
its own panic boundary, so neither a slow encoder nor a slow transport holds
up the request loop.

Because encoding happens after queue admission, a document that cannot be
encoded still takes a queue slot until the worker reaches it, and a bad
document sent to a full or unavailable target answers `queue full` or the
unavailable reason rather than its write error.

Refusal paths and completed jobs each emit one `DataEvent::Upload`, echoing
the requester's key, tag, and target. Errors name the target, including
unknown targets, unsupported documents, missing document kinds, unavailable
workers, queue refusal, write errors, and transport errors. Validation and queue
refusals answer from the service thread; write errors, transport results, and
panics answer from the worker. An encoding panic becomes `egress '<target>':
encoding panicked: …` and a transport panic `egress '<target>': transport
panicked: …`; either way the worker continues with the next queued job using
the same transport handle. A panic in the service's own upload step is
answered by the request loop (see [the request loop](#the-request-loop)), and
a worker that dies outside both boundaries is declared stopped
(`geode-egress-<target>`): jobs still queued behind it are never answered,
and later uploads to that target answer `egress '<target>': stopped`.

Completion still has limits: service startup can fail after channel admission,
and encoder or transport calls can block indefinitely. Event-sink refusal has
no retry. Uploads have no timeout, automatic retry, or keyed cancellation.

A successful outcome means the adapter's `upload` call returned successfully;
the adapter defines what that acknowledges. It does not establish that a
subscriber received, parsed, or stored the document. `ChannelAdapter`, for
example, acknowledges admission to its bus queue; downstream subscription
queues can still refuse delivery. Market-data panels compare later document
generations separately to confirm a sent draft. The app mailbox retains upload
outcomes by `(tile key, upload tag)`, so different uploads do not supersede
one another before UI delivery.

`EgressSpec::address` substitutes key parts joined by `/` for every `{key}` in
the configured address. It performs literal replacement, without escaping key
parts. A template with no `{key}` sends all keys of that document to one address.

Shutdown closes every target queue and joins its worker, allowing already
queued jobs to finish if the transport returns normally. Egress stops before
subscription workers, but this does not guarantee that an echoed document
reaches storage: subscriptions and ingest do not flush all pending work.
Joining can wait indefinitely on transport I/O and belongs off the UI thread.
See [`egress.rs`](../../crates/geode-data/src/egress.rs).

## Queries and time travel

A view query compiles scope predicates and grouping into one statement for
all tree depths. Each measure is aggregated at its own grain, then joined at
the grouping cardinality. The result is an immutable columnar `Snapshot`:
expanding a tree node works on the prepared result rather than issuing another
database query. User supplied scope values are bound as parameters. A computed
dataset has no relation: the compiler refuses a view or join over it, and
distinct-value requests skip it.

A scope reaching compilation still carrying a named-expression reference
(`Scope.named` nonempty) is refused outright, with `StoreError::Scope("scope
carries unresolved named expressions")`, rather than compiled with that
reference silently dropped. Resolving a name against `expressions.toml` is
the shell's job, before a query is ever submitted (see
[the shared frame](shell.md#the-shared-frame)); this refusal is the safety net
behind that call site, not a path meant to be exercised in normal use.

The line pricer's sheet never reaches DuckDB, so the pricer evaluates the
frame's scope in process with `geode_core::scope::eval` (`Scope::matches`
over one row at a time). The SQL lowering in `scope_sql` is the authority:
the evaluator copies what DuckDB does with the predicate that lowering emits,
and a parity fixture in `geode-data` (`query/eval_parity.rs`) runs every
operator, NULL case, cross-type cast and constant fold through both paths
over one in-memory table and asserts equal row sets, or that both refuse.
Where the two could disagree, the fixture decides and the evaluator follows
the SQL. What it pins that a reader would not guess:

- `like` is DuckDB's `ilike` with no ESCAPE clause: case-insensitive, `%`
  any run, `_` one character, and a backslash an ordinary character. It is
  refused on anything but a text column and a text pattern.
- The text filter is a case-insensitive substring OR-ed over the dataset's
  textual columns only; a needle found only in a number or a non-textual
  column matches nothing, and a dataset with no textual column makes the
  filter match nothing.
- `=` and ordering on text compare bytes, so case matters.
- A number against a text column casts the *column* to DOUBLE, one row at a
  time (a row that is not a number fails the query); ordering between text
  and a number is refused. Text against an i64 column casts the *literal* to
  BIGINT, rounding half away from zero (`qty = '2.5'` is `qty = 3`); a number
  against an i64 column compares as DOUBLE (`qty = 2.5` matches nothing).
- NaN equals itself and orders above every number.
- A derived dimension compares by membership of its source value. A derived
  value no source maps to makes `=` and `in` the constant false and `!=` the
  constant true (keeping a NULL source), and DuckDB folds that constant before
  it converts the literal beside it, so `region = 'APAC' and strike = 'abc'`
  keeps nothing rather than failing.
- NULL is three-valued: a comparison reaching NULL is UNKNOWN, `not UNKNOWN`
  is UNKNOWN, and only TRUE keeps a row. `not (currency = 'USD')` does not
  keep a row whose currency is NULL.

An evaluator error on any row refuses the whole scope, because the same
statement fails whole in DuckDB; keeping the rows that did evaluate would
narrow the result in a way no query does. The evaluator is stricter in one
place: it reports a row that fails a cast even where DuckDB's filter order
might discard that row first, a case where DuckDB's own result is not stable.
It can therefore refuse where DuckDB succeeds, but never keeps a row DuckDB
would drop. Not pinned by the fixture: which text spellings cast to BOOL,
date literals other than ISO `YYYY-MM-DD`, and DOUBLE spellings such as `inf`
or `+5`. Timestamp and bool columns, and a derived dimension over a non-text
source, are refused in process.

The [typed-document reference](typed-documents.md) describes schema and view
validation, grain meaning, scope composition, and checks deferred to query
compilation. A typed reader returning a value does not prove every requested
query can be served by the dataset's actual storage grains.

Every view is validated when the service opens and again on every reload,
through one helper both paths call, so open and a reload cannot disagree about
which views can be honoured. A view carrying an error diagnostic is **refused by
name** when queried: `query` answers with that view's first error message
instead of compiling it. The refusal is decided before a grouping override is
considered, so regrouping a refused view is not a way in, and a reload replaces
the refusal set rather than merging into it, so a view the author has just
corrected serves again without a restart. A refused view stays registered and
its siblings still serve; one unhonourable view does not take the desk down or
disappear from the dialogs that would fix it.

A refused view reports an error instead of returning an absent column that
would look like a genuine NULL. Measure aggregation follows the schema's
explicit role; a grain-bearing attribute cannot fall through to a default sum
and produce a misleading total.

`required = false` downgrades unusable joins, measure-role mismatches, and
unreachable dimension columns to warnings, allowing those declarations to be
dropped. Unknown columns and invalid derived-dimension sources remain errors.
A join keyed outside the grouping that supplies no selected column only warns,
regardless of `required`, because no selected value depends on it.

The same checks run against a **per-query grouping override**, which validates a
copy of the view with the override's grouping in place. Regrouping away from a
`dimension` column that the view's own grouping supplied makes it an ungrouped
dimension (below): it is shown by the unanimity rule when a grain carries it
alongside the new grouping, and otherwise the query is refused rather than
painting the column blank for as long as the override lasts. The remedy is in
the message: group by the column again, or declare it `required = false`.
An override cannot rescue a view already refused during load validation.

The compiler also checks joins against available datasets and grains. An
unusable required join produces an error naming the dataset; an optional join
is omitted. These checks protect callers that bypass service-level validation.
Derived SQL remains subject to compilation errors.

The read pool coalesces by the **caller's key**, usually a tile, rather than
by view name. Two tiles showing one view therefore do not supersede each
other. A newer request interrupts an older one for the same key; request and
result tags let the receiver discard a stale arrival. A failed or refused
delivery does not stop a worker. An interrupt can land on a read transaction's
own `BEGIN`, `COMMIT` or `ROLLBACK` and leave the connection inside an aborted
transaction, so a worker issues `ROLLBACK` after every run, retrying while it
is interrupted, and starts each request outside any transaction. See [`compile.rs`](../../crates/geode-data/src/query/compile.rs)
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

A document request names its document by key. A key with fewer parts than the
dataset declares reads **every document under it**: `["SPX"]` on
`option_chain` (keyed `underlying, expiry`) returns every SPX expiry in one
snapshot, ordered by the key parts left open and then the axes, so each
document's rows stay together. A key with more parts than declared, or with
none, is refused, as is a key part containing the key separator (it would
join to another key's partition). Matching is by key part, never by string prefix, so `SPX`
never reads `SPXW`'s documents (`geode_core::document::is_key_prefix`). An
as-of prefix read resolves each matched document's generation independently
and pins that set, so a document first published after the instant is
absent and a republished one reads its older generation. Provenance follows
the historical view rule: the oldest matched source time, and a generation ID
only when exactly one document matched, since no single ID names several. See
[`document.rs`](../../crates/geode-data/src/query/document.rs).

Attribution says whether a value belongs to its row; it does not say whether
a column adds up. The compiler records that separately as
`ColumnMeta::summable`, true only for a plain measure whose schema aggregate
is `sum`. Min, max, and any measures, derived expressions, joined columns,
and grouping columns are not summable. Document-query and catalog snapshot
builders mark their columns false; custom builders must supply the flag
explicitly. Selection totals require this flag and additive contributing
values. Attribution alone cannot justify a total of maxima or ratios.

### Ungrouped dimension columns

A view may show a `dimension` column it does not group by — `strike` or
`expiry` beside an `lhu → underlying_ref → position_ref` tree. When the column
is declared with the dimension role in the primary (measure-family) dataset, is
not a derived dimension, and no join supplies it, the compiler computes it by
the **unanimity rule**, at every depth including the grand total:

- the value, when every stored row under the tree row has that one non-NULL
  value;
- **mixed**, when they disagree — including a NULL beside a value, since
  showing the value would claim it for rows that have none;
- blank (NULL), when no row under it has a value.

Unanimity prevents an arbitrary leg's strike from appearing as the strike
for a position containing several instruments.

The column is read from one table: the coarsest declared grain of the dataset
that carries it and every column of the view's grouping (`unanimity_grain` in
`geode-core`'s `view` module). The whole grouping, not a query's bounded prefix,
so one grain serves every depth and a view that validates compiles at every
`max_depth`. Per column the aggregate is `case when count(c) = count(*) and
min(c) = max(c) then min(c) end` plus a flag `count(c) > 0 and (count(c) <
count(*) or min(c) <> max(c))`, over the same grouping sets and level marker as
a measure aggregate at that grain, joined to the spine the same way, with the
grain's scope predicate and era. When a measure aggregate already reads that
grain the two aggregates ride its scan; otherwise one `dim_<table>` CTE per grain
holds them. That CTE is joined to the spine but never feeds it, so adding a
display column never adds or removes tree rows. A view adds this aggregation
only for the columns it shows ungrouped and for the context columns below.

The result carries the value in the column's own type, so a numeric dimension
sorts as a number and paints its shortest exact form (`4250`, `4250.5`), and a
boolean companion column named `<column>#mixed` (false where the grain has no
rows under the spine row). `ColumnMeta::mixed_flag` links the value to its
companion by index and `Snapshot::from_batches` refuses a flag that is not a
boolean column of the batch; `Snapshot::is_mixed_at` reads it. The column is not
summable and is `Additive` at every depth, because the rule is already exact,
and takes the chosen grain's scope semantics. A consumer reading the snapshot
without the flag sees NULL, not a value.

Known limitations: the unanimity is over the chosen grain table's rows, so an
instrument with no row in that table (a cash instrument absent from the
underlying table when the grouping forces the underlying grain) does not take
part; choosing the coarsest carrying grain minimises this. A derived
column over the dimension sees only the value column, so where the input is
mixed the derived cell is blank, not marked.

**Context columns.** A query can also carry columns the view does not show,
so the shell can read one row's value of each (the dimension context behind
`g m`). `ViewSpec::context` holds them. It is runtime-only, never read from
config, and validation ignores it. The data service fills it on a copy of the
view for every query, from the list set with `DataHandle::set_context_columns`;
`geode-app` publishes `ModuleRoster::context_columns` (the union of every
factory's `accepts`) there at startup. `ViewSpec::context_dimensions` keeps a
listed column only when the primary dataset declares it as a key or a
dimension, the dataset is not a document or series dataset, and the view
neither groups by it, nor shows it, nor derives it, nor takes it off a join;
each column appears once. A column the dataset lacks is dropped silently,
since the list spans datasets, and so is one no declared grain carries
alongside the grouping: context columns are optional, never a compile error.

Each kept column is computed by the unanimity rule above, so every view query
carries a value and `<column>#mixed` pair for it. The pair is hidden only in
the sense that the blotter plan builds from `view.columns` and never paints
it; it is in the snapshot. The cost is per query: when a measure aggregate
already reads the column's grain the pair's two aggregates ride that scan;
otherwise the grain gets its own `dim_<table>` CTE: one more scan of that
table and one more join to the spine, shared by every column read at that
grain.

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
feed-published documents; the API is called only by tests for those. Feed
archives therefore grow with every publish: each republish archives the
outgoing generation, for the demo's `opra_sim` option chains (twelve documents
per underlying, one per expiry) as for its CVI documents (one per underlying).
Local documents (`local = true`) are the exception: they keep 200 archived
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
diagnostic; a forget the service refused answers `ForgetFailed` too, so
every forget a caller submitted answers exactly once. A forget has no
progress or health lane.

Series retention is separate and runs for the affected `(source, identity)`
pair inside each append transaction. See
[`retention.rs`](../../crates/geode-data/src/store/retention.rs) and
[`series.rs`](../../crates/geode-data/src/store/series.rs).

## Freshness, health, and delivery

Freshness is measured in source time. A book is as fresh as its stalest
contributing file, and a view with multiple inputs is as fresh as its stalest
input. That avoids labeling a partial or joined answer with the newest
contributor's timestamp.

Provenance reports source time and generation separately. Corrected republishes
can share a source time while taking different generation IDs. Planning,
provenance lookup, and row execution share one reader transaction, so these
values describe the same database snapshot.

| Read | Generation reported |
|---|---|
| Live document | Newest live-published generation of the requested key's partition; for a key prefix, the greatest across every partition under it (a change marker; the reported source time is the stalest matched document's). |
| Historical document | Generation selected for that key at the requested instant; for a key prefix, `None` unless exactly one document matched. |
| Live view | Greatest live-published generation ID across each input dataset, regardless of query scope. |
| Historical view | `None`; each partition resolves independently. |

A live view's dataset-wide value is a publication change marker; it does not
name every partition's generation. Archive-only arrivals do not advance live
markers. Document reads report no generation when none matches, including a
historical request before the document's first retained generation.

An absent generation means unknown, not unchanged. The document panel compares
known generation IDs as well as source times. Its source-time fallback cannot
distinguish corrected republishes at the same source time.

A live prefix read takes its freshness from the bookless partitions whose
batch is the key or lies under it. `Catalog::live_source_time_under` takes
each matched document's newest live source time, then the oldest of those:
like a view labelled by its oldest book, a set of documents is as fresh as
its stalest member, so one freshly republished expiry cannot hide a stale
one. It follows that a document nothing republishes (for example an option
expiry that has passed) holds a live prefix read's freshness back until the
document is removed. A historical prefix read reports the oldest matched source time for the
same reason. `live_generation_under` reports the greatest generation ID
instead, because it is a change marker: generation IDs come from one store
sequence, so it changes whenever any matched document republishes. The SQL
matches `batch = key or starts_with(batch, key‖separator)`, the SQL form of
`is_key_prefix`; it takes no wildcard, so an `_` or `%` in a key cannot widen
the match, and the two must be kept in agreement.

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
the error diagnostic. A publish the service refuses before queuing it (the
dataset is not `local`, or not declared) answers `LocalPublishFailed` too, so
every admitted local publish answers exactly once — the pricer counts its
queued saves on that. Load entries have no eviction policy, and unresolved
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

Health reaches a tile through the shell's `Diagnostics` entity, never the
data handle. The app bridge describes each source with its dataset at attach;
a tile maps what it reads to sources through that link (the blotter: its
snapshot's provenance datasets; market-data: its panel's dataset; the pricer:
`pricer_sheets`; timeseries: its series' sources by name) and re-asks only
when `DiagVersions.sources` moves. **Known limitation:** the pricer's prices
come from the pricer behind the pricing door, which reads no dataset today, so
its chip covers the sheet store only. A pricer that reads market-data datasets
must declare them, and the pricer tile adds them to its question.

Non-local publication events advance matching dataset/document watches; a
document watch on a key prefix advances for every document under it, at the
same key-part boundary a prefix read uses. Local publications update
diagnostics without advancing frame revisions. Query
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
  can detect particular wrong-data behaviors. `--anchors-only` validates the
  table without running Cargo or editing source: it rejects a stale or
  ambiguous anchor, a filter matching no test or several without an exact
  name, an entry with no filter, a replacement equal to its anchor, and a
  mutation repeated under the same test. The checker,
  [`mutation_anchors.py`](../../scripts/mutation_anchors.py), has its own
  unittest suite in `scripts/test_mutation_anchors.py`. A mutation that
  does not compile is reported as `BUILD`, not caught, and fails the run,
  as does a stale entry; the exit status reports only such harness errors,
  while `SURVIVED`, `caught` and `FILTER` are verdicts read from the output.
  `--build-check` compiles each selected mutation without running tests,
  on the same target and test profile a mutation run builds, to find
  replacements left stale by signature changes that the static check
  cannot see. A mutation run or build check that selects no entry exits
  nonzero, unless `--changed` skipped every candidate because no anchored
  file changed.

The code linked above is the implementation authority. If this guide and the
code disagree, correct the guide and assess whether the behavior is an
unintended regression.
