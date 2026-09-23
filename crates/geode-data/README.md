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

`DataService` owns the writer connection and is not `Sync`, so it lives
on its own thread. `DataHandle` is the `Clone + Send + Sync` door: every
method is a `try_send`, and a request that cannot be queued is refused
and counted rather than waited on. Results and health come back as
`DataEvent`s through an `EventSink`; a sink returning `false` means "not
delivered" and no producer stops on it.

Every background boundary (an ingest load, a catalog recheck, a discovery
poll, a query worker, a document publish, a message receive) runs under
`geode_core::panic::contained`. A contained panic logs and keeps the app
running; an ingest load panic marks the source `Failed`.

## What lives here

| Module | Holds |
|---|---|
| `service` | `DataService`, `DataServiceConfig`, `DataEvent`, and the `HealthTracker` (two lanes per source, `discovery` and `load`; the worse by `severity_rank` wins). |
| `handle` | `DataHandle` and `Request`: queries, distinct values, the catalog, a document by key, a series fetch, identities. |
| `source` | Configured sources: directory globs, the `.done` sentinel, discovery and readiness. |
| `adapter` | The adapter tier for subscribed and fetch sources: three object-safe traits, a registry, a bounded sink, Solace-style topic matching. The only implementation is the in-process `ChannelAdapter`; a vendor adapter is built elsewhere against this contract. |
| `ingest` | The discovery scheduler, the cold-start priority ladder, the per-file load pipeline, the grain split and conflict detector, the ingest runner (one thread, one writer connection, three queues), the subscribed-source receiver, the `Coalescer`, and the fetch worker. |
| `store` | The DuckDB store: DDL generated from the schema, the per-file publish transaction and backfill guard, document publish, the series family's bitemporal append (`append_series`, the one door series rows enter by), retention, and the freshness catalog in source time. |
| `query` | The query path: scope to bound SQL, the grain-aware view compiler, the read pool (latest-wins per key, stale results dropped), as-of routing against the archive, the picker's distinct values, the document request, the catalog request. |
| `documents` | The `DocumentKind` registry the app fills. |
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
