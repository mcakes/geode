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
door: request submission uses `try_send`, and a request that cannot be queued
is refused and counted rather than waited on. Results and health come back as
`DataEvent`s through an `EventSink`; a sink returning `false` means "not
delivered" and no producer stops on it.

View replacements retain the latest configuration even under request-channel
pressure. Shutdown and final-handle drop join workers and must run off the UI
thread. Admission, cancellation, and completion have distinct guarantees; see
[requests and UI delivery](../../docs/current/request-delivery.md).

Read, pricing, ingest, and egress transport paths contain panics at their
operation boundaries; an ingest load panic reports that operation as `Failed`.
Service-thread upload serialization has no equivalent boundary. Containment
does not interrupt blocked adapter or filesystem calls.

The bounded request and adapter channels do not bound the ingest queues.
Documents precede series, which precede files, with no preemption of running
work. Sustained higher-priority traffic can starve lower-priority jobs.
See [queues and shutdown](../../docs/current/data-path.md#queues-and-shutdown)
for capacity, coalescing, and worker shutdown behavior.

## What lives here

| Module | Holds |
|---|---|
| `service` | `DataService`, `DataServiceConfig`, `DataEvent`, and the `HealthTracker` (two lanes per source, `discovery` and `load`; the worse by `severity_rank` wins). |
| `handle` | `DataHandle` and `Request`: queries, distinct values, the catalog, a document by key, a series fetch, identities, an upload. |
| `source` | Directory discovery, sentinel parsing, and readiness classification. Configuration types are shared with `geode-core`; stable-mtime readiness is accepted by configuration but unsupported at runtime. |
| `adapter` | Subscription, upload, and fetch capabilities; a registry, bounded message sink, and topic matching. Includes the in-process `ChannelAdapter`; the app can register additional implementations such as its demo series adapter. |
| `ingest` | The discovery scheduler, the cold-start priority ladder, the per-file load pipeline, the grain split and conflict detector, the ingest runner (one thread, one writer connection, three queues), the subscribed-source receiver, the `Coalescer`, and the fetch worker. |
| `store` | The DuckDB store: DDL generated from the schema, the per-file publish transaction and backfill guard, document publish, the series family's bitemporal append (`append_series`, the one door series rows enter by), retention, and the freshness catalog in source time. |
| `query` | The query path: scope to bound SQL, the grain-aware view compiler, the read pool (latest-wins per key, stale results dropped), as-of routing against the archive, the picker's distinct values, the document request, the catalog request. |
| `documents` | The `DocumentKind` registry the app fills. |
| `egress` | Startup target resolution, service-thread document serialization, and per-target upload workers with eight waiting jobs. Refusals and completed transport calls emit keyed/tagged upload outcomes. |
| `health` | Re-export of `geode_core::health::Health`. |

## Features

- `test-support` exposes `DataHandle::for_tests()` and the other
  service-thread-free fixtures the module crates' tests use.

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
- Live/archive retention has a transactional storage API but no production
  scheduler. Series retention runs inside append transactions. See the
  [maintenance contract](../../docs/current/data-path.md#retention-and-maintenance).
- Discovery compares path, size, and source time, not CSV contents. Glob and
  CSV metadata errors are currently skipped, so an empty poll does not prove
  path accessibility. Adapter queue admission likewise does not acknowledge
  storage publication. See [source discovery and adapters](../../docs/current/data-path.md#source-discovery-and-adapters).
- Upload channel admission, transport success, and a stored echo are separate
  events. Service-thread validation/serialization precedes each target's bounded
  FIFO worker queue. Refusals, transport returns, and contained transport panics
  emit outcomes; a transport panic leaves the worker available for later jobs.
  Startup failure, blocked calls, serialization panics, and sink refusal can
  prevent delivery. Uploads have no keyed cancellation or automatic retry.
- Adapter resolution and worker creation request separate egress handles.
  Shutdown closes worker queues and joins after queued jobs run; a stuck
  transport can block shutdown. See
  [egress and uploads](../../docs/current/data-path.md#egress-and-uploads).
