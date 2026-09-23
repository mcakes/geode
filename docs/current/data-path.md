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

The writer is serialized because DuckDB admits one writer. Independent read
connections serve queries. The ingest runner gives parsed documents priority
over fetched series, and both priority over queued files; it finishes a file
already in flight before taking another job. This puts trader-requested work
ahead of background discovery without attempting concurrent writes.

The request channel is bounded. `DataHandle` uses `try_send`: `false` means
the request was **not queued**, so a caller must handle refusal rather than
wait for an answer that cannot arrive. Accepted requests return outcomes
through `DataEvent`. The app's event sink must remain nonblocking because
some producers call it while holding a queue lock. See
[`handle.rs`](../../crates/geode-data/src/handle.rs) and
[`service.rs`](../../crates/geode-data/src/service.rs).

## Ingestion and publication

Directory sources are polled, not watched, because file watches can fail
silently on network shares. Discovery checks readiness through the configured
strategy, including the `.done` sentinel, and skips unchanged generations.
A file that is pending, malformed, or too old to become current is reported
according to its state; the system does not silently present it as fresh.

The unit of replacement is a **partition** identified by dataset, batch, and
book, including a possible NULL book. A file can publish several partitions
and grains. For each partition, live data holds one generation. Publishing a
newer generation moves the outgoing rows to archive and installs the incoming
rows in one transaction. An older arrival goes to archive without replacing
live data. This keeps live queries bounded by current data while preserving
history for time travel. The source timestamp from the sentinel orders
generations; filesystem modification time does not.

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

## Queries and time travel

A view query compiles scope predicates and grouping into one statement for
all tree depths. Each measure is aggregated at its own grain, then joined at
the grouping cardinality. The result is an immutable columnar `Snapshot`:
expanding a tree node works on the prepared result rather than issuing another
database query. User supplied scope values are bound as parameters.

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

Retention is per partition, so a frequently updated book cannot evict a
quiet book's history merely by publishing more often. A sweep reconciles the
generation summary against every table pair in the dataset. See
[`retention.rs`](../../crates/geode-data/src/store/retention.rs).

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

Publication events invalidate affected views. Query results are addressed to
the requesting key. Series fetch completion is addressed by `(identity,
source)` so every tile watching the same pair can requery, including when a
fetch appended zero rows because the span was already covered. These routing
rules prevent an accepted operation from leaving a tile waiting indefinitely.

## Limits and verification

- The demo database is not automatically migrated after schema changes.
- Historical as-of depends on retained generations; retention bounds how
  far back it can answer.
- The exact performance measurements and their conditions are in
  [perf.md](../perf.md), not inferred from the architecture diagram.
- Ordinary tests verify outcomes. Targeted mutations in
  [`mutation-check.sh`](../../scripts/mutation-check.sh) check whether tests
  can detect particular wrong-data behaviors; `--anchors-only` validates
  their source anchors without running Cargo.

The code linked above is the implementation authority. If this guide and the
code disagree, correct the guide and assess whether the behavior is an
unintended regression.
