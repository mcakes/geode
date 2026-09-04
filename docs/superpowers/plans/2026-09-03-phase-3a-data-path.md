# Phase 3a — Data Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give modules a thread-safe door to `DataService` that keys queries
per tile, delivers results and ingest events on one channel, reads
`sources.toml`, and keeps a blotter current through a continuous discovery
scheduler — while the throwaway probe stays alive as the first consumer.

**Architecture:** `geode-core` gains the value types both ends of the query
path share (`AsOf`, `QueryKey`, `QueryOutcome`), index-based `Snapshot`
accessors, a `TreeIndex` built on the query worker, and column presentation
config. `geode-data` re-keys the pool on the tile, delivers through sinks
instead of owning receivers, runs one ingest runner for every dataset with
the `Store` moved onto its thread, adds a discovery scheduler, and wraps the
whole service in a `DataHandle` on its own thread. `geode-app`'s probe is
switched onto the handle so the end-to-end path is exercised before the
blotter exists.

**Tech Stack:** Rust 2024 edition, stable toolchain. `duckdb` 1.10505
(`bundled`, `chrono`). `chrono` 0.4.42 added to `geode-core`. `criterion`
0.8.2. No async runtime in `geode-data`: OS threads, `std::sync::mpsc`,
and caller-supplied sink closures.

**Spec:** `docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md`
— read §2.3–§2.8 and §5 first; this plan argues from them and cites them.
Phase 2's spec (`2026-08-30-geode-phase-2-data-design.md`) is `P2 §n`.

## Global Constraints

- **Layering (CLAUDE.md):** `geode-shell` and `geode-data` never depend on
  each other. `geode-core` names no Arrow type outside `snapshot.rs` and
  `tree.rs`. `geode-data` is the only crate that opens a file.
- **CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo test --workspace`, and `cargo bench --workspace
  --no-run` on both macOS and Windows.** Every task ends green on all four.
- **Every new lib/bin target needs `bench = false`; every `[[bench]]` target
  needs `harness = false`.**
- **Values are bound as parameters, never spliced into SQL text.** Nothing
  in this plan writes SQL, but nothing may undo that either.
- **Nothing in this plan runs on the UI thread.** `DataHandle` methods
  never block: `try_send` only.
- **Commit before you mutate.** `zsh scripts/mutation-check.sh` restores
  files with a backup, but `git checkout` on a mutated file discards
  uncommitted work. Run the harness after every task that says so, and
  finish with an unfiltered run.
- **A green suite proves little here** (`docs/phase-2b-review-handoff.md`).
  Every behaviour this plan adds gets a harness entry whose test asserts
  on a *value*, not a marker.
- **Test the accessor against what DuckDB emits, not against a fixture
  alone** (`docs/phase-3-prerequisites.md`): `row_depth` arrives `Int32`,
  dimensions arrive at three dictionary widths, `sum(BIGINT)` arrives
  `Decimal128`.
- Commit after each task with the repo's trailers:
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1
  ```

## What already exists (do not rebuild)

- `geode_core::snapshot::{Snapshot, ColumnMeta, Provenance, Freshness,
  DictCodes, TestColumn}` — `from_batches(batches, meta, grouping_len,
  provenance)`, by-name accessors `f64_value`, `i64_value`, `text_value`,
  `display_value`, `dict_column`, `depth_of_row`, and `for_tests(columns,
  grouping_len)` under the `test-support` feature.
- `geode_core::config::{Config, MergedDoc, Diagnostic, Severity, Layer,
  LayerDoc, merge_docs}`; `atomic_depth` already lists `sources` and
  `groupings`.
- `geode_core::view::{ViewSpec, ViewColumn, SortKey, JoinSpec}` with
  `from_doc` and `validate`; `geode_core::scope::Scope`;
  `geode_core::dimensions::DerivedDimensions`.
- `geode_data::query::pool::{QueryPool, QueryRequest, QueryResult, QueryId,
  ViewId}` — coalescing keyed on `ViewId`, delivery over a `Receiver`.
- `geode_data::service::{DataService, DataServiceConfig}` —
  `open(config)`, `query(view, scope, as_of, max_depth)`, `cancel(view)`,
  `query_results()`, `freshness`, `as_of_bounds`, `diagnostics`,
  `validate_scope`, `shutdown`.
- `geode_data::ingest::{IngestRunner, IngestHandle, IngestEvent, WorkPlan,
  WorkItem, build_plan, load_file, LoadRequest, LoadOutcome}` — one runner
  per dataset, owning a `Store`.
- `geode_data::source::{SourceSpec, Readiness, Priority, Candidate,
  CandidateState, discover, parse_sentinel}`.
- `geode_data::store::{Store, Catalog, StoreError}` — `Store::{open,
  writer, reader, apply_schema}`; `Catalog::new(&Connection)`.
- `geode_data::health::Health`.
- `geode_app::probe` — `prepare(&Config)`, `start(prepared, shell, cx)`,
  a one-shot ingest then a service on a dedicated thread, readings pushed
  into `ShellView::set_probe`.
- `geode_demo_data::{generate, GeneratorConfig, emit_directory,
  EmitOptions}`; `geode_data::ingest::load::tests_support::{fixture,
  tree_view, batch_of}` (crate-private).

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-core/src/query.rs` (new) | `AsOf`, `QueryKey`, `QueryOutcome` — the value types both ends of the query path share (§2.7, §5.1). |
| `crates/geode-core/src/tree.rs` (new) | `TreeIndex`: parent links and CSR child lists over a snapshot (§5.5). |
| `crates/geode-core/src/snapshot.rs` | Gains index-based accessors, a cached depth column, the grouping names, and the tree. |
| `crates/geode-core/src/view.rs` | Gains `ColumnPresentation` / `ColumnFormat` parsed from `format`, `label`, `width` (§6.2). |
| `crates/geode-core/benches/tree.rs` (new) | `TreeIndex::build` at the three result shapes. |
| `crates/geode-data/src/query/as_of.rs` | `AsOf` becomes a re-export. |
| `crates/geode-data/src/query/pool.rs` | Keyed on `QueryKey`; delivers through a sink; carries `tag` and `submitted`. |
| `crates/geode-data/src/service.rs` | `DataEvent`, `EventSink`, `QueryParams`; `open(config, sink)`, `open_channel`, `replace_views`; owns the runner and scheduler. |
| `crates/geode-data/src/handle.rs` (new) | `DataHandle`, `Request`, the service thread, `for_tests`. |
| `crates/geode-data/src/ingest/runner.rs` | One runner for all datasets; takes `SchemaSpec` and a sink. |
| `crates/geode-data/src/ingest/scheduler.rs` (new) | Discovery on each source's interval; submits plans; health events. |
| `crates/geode-data/src/source/config.rs` (new) | `SourceSpec::from_doc`, `parse_duration`. |
| `crates/geode-app/src/probe.rs` | Rewritten over `DataHandle` + `sources`; still throwaway. |
| `scripts/mutation-check.sh` | One entry per behaviour below. |

---

### Task 1: Move `AsOf` to `geode-core`; add `QueryKey` and `QueryOutcome`

The shell will hold an as-of and route outcomes without naming
`geode-data` (spec §2.7, §5.1).

**Files:**
- Create: `crates/geode-core/src/query.rs`
- Modify: `crates/geode-core/src/lib.rs` (add `pub mod query;` between
  `pub mod dimensions;` and `pub mod schema;`)
- Modify: `crates/geode-core/Cargo.toml` (add `chrono = "0.4.42"` under
  `[dependencies]`)
- Modify: `crates/geode-data/src/query/as_of.rs:13-23` (delete the enum,
  re-export)
- Test: `crates/geode-core/src/query.rs` (inline)

**Interfaces:**
- Produces: `geode_core::query::{AsOf, QueryKey, QueryOutcome}` exactly
  as below. `geode_data::query::AsOf` keeps resolving, as a re-export.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-core/src/query.rs`:

```rust
//! Value types shared by both ends of the query path (spec §5.1, §2.7).
//!
//! The shell holds the frame's as-of and routes query outcomes to tiles;
//! the data layer produces them. Those two crates may never depend on
//! each other (CLAUDE.md), so what they exchange sits below both — the
//! same reason `Scope` lives here.

use crate::snapshot::Snapshot;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Instant;

/// Which point in time a query reads (foundation §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsOf {
    Live,
    At(DateTime<Utc>),
}

impl AsOf {
    pub fn is_live(&self) -> bool {
        matches!(self, AsOf::Live)
    }
}

/// The coalescing key for queries: one in-flight query per key, latest
/// wins (spec §2.4). A tile uses its tile id, so two tiles showing one
/// view never supersede each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueryKey(pub u64);

/// One query's result, addressed to the key that asked.
#[derive(Debug)]
pub struct QueryOutcome {
    pub key: QueryKey,
    /// Echoed from the request. The submitter keeps its own counter and
    /// drops an outcome whose tag is older than its latest submission, so
    /// "a stale result is never rendered" (§7.3) holds at both ends.
    pub tag: u64,
    /// `Err` is the failure text; the tile keeps its last good snapshot.
    pub snapshot: Result<Arc<Snapshot>, String>,
    /// When the submitter asked, for the §7.1 timing readout.
    pub submitted: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_is_live_and_an_instant_is_not() {
        assert!(AsOf::Live.is_live());
        let t = DateTime::parse_from_rfc3339("2026-09-03T14:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!AsOf::At(t).is_live());
    }

    #[test]
    fn keys_compare_and_hash_by_value() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(QueryKey(3));
        set.insert(QueryKey(3));
        set.insert(QueryKey(4));
        assert_eq!(set.len(), 2);
        assert!(QueryKey(3) < QueryKey(4));
    }
}
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo test -p geode-core query:: 2>&1 | head -20`
Expected: error — `chrono` unresolved and `pub mod query` absent.

- [ ] **Step 3: Wire the module and dependency**

In `crates/geode-core/Cargo.toml`, under `[dependencies]`:

```toml
toml = "1.1.4"
arrow = "58.4.0"
chrono = "0.4.42"
```

In `crates/geode-core/src/lib.rs`:

```rust
pub mod attribution;
pub mod config;
pub mod dimensions;
pub mod query;
pub mod schema;
pub mod scope;
pub mod snapshot;
pub mod view;
```

- [ ] **Step 4: Run to verify the core tests pass**

Run: `cargo test -p geode-core query::`
Expected: 2 passed.

- [ ] **Step 5: Make `geode-data` re-export instead of define**

In `crates/geode-data/src/query/as_of.rs`, replace lines 13–23 (the
`AsOf` enum and its `impl`) with:

```rust
/// Re-exported from `geode-core` (spec §2.7): the shell holds the frame's
/// as-of and cannot name this crate.
pub use geode_core::query::AsOf;
```

Keep `use chrono::{DateTime, Utc};` — `ResolvedGeneration` still uses it.

- [ ] **Step 6: Verify the workspace still builds and passes**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all green. Every existing `AsOf` user resolves through the
re-export unchanged.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-core crates/geode-data/src/query/as_of.rs
git commit -m "feat(core): AsOf, QueryKey and QueryOutcome as shared query-path vocabulary

AsOf moves from geode-data so the shell can hold the frame's as-of
without naming the data crate (spec §2.7); QueryKey and QueryOutcome
are what the shell routes to tiles (§5.1).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

---

### Task 2: Key the pool on `QueryKey`; deliver through a sink

Spec §2.4 and §5.1. Two tiles on one view stop cancelling each other, and
results go wherever the caller says rather than into a receiver the
service owns.

**Files:**
- Modify: `crates/geode-data/src/query/pool.rs`
- Modify: `crates/geode-data/src/query/mod.rs` (re-export `ResultSink`)
- Modify: `crates/geode-data/src/service.rs` (compile-only edits, so the
  crate builds; Task 3 does the real work)
- Test: `crates/geode-data/src/query/pool.rs` (inline)

**Interfaces:**
- Consumes: `geode_core::query::QueryKey`.
- Produces:
  - `pub type ResultSink = Arc<dyn Fn(QueryResult) -> bool + Send + Sync>;`
  - `QueryRequest { key: QueryKey, tag: u64, submitted: Instant, view: ViewId, compiled: CompiledQuery, grouping: Vec<String>, provenance: Provenance }`
  - `QueryResult { id: QueryId, key: QueryKey, tag: u64, submitted: Instant, view: ViewId, snapshot: Result<Snapshot, String> }`
  - `QueryPool::spawn_with_sink(store: &Store, workers: usize, sink: ResultSink) -> Result<QueryPool, StoreError>`
  - `QueryPool::spawn(store, workers) -> Result<(QueryPool, Receiver<QueryResult>), StoreError>` (kept, built on the sink)
  - `QueryPool::cancel(&self, key: QueryKey)`

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `pool.rs`, change the `request` helper and add
two tests:

```rust
    fn request(key: u64, view: &str, sql: &str) -> QueryRequest {
        QueryRequest {
            key: QueryKey(key),
            tag: key * 100,
            submitted: std::time::Instant::now(),
            view: ViewId(view.to_string()),
            compiled: query(sql),
            grouping: Vec::new(),
            provenance: Provenance::default(),
        }
    }

    #[test]
    fn two_keys_on_one_view_do_not_coalesce() {
        // Two tiles showing the same view (spec §2.4). Keyed on the view
        // name, the second submit replaced the first and the first tile
        // never got its result.
        let (_d, store) = fixture(1_000);
        let (pool, rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.submit(request(1, "tree", "select sum(v) as v from t"));
        pool.submit(request(2, "tree", "select sum(v) as v from t"));
        let mut keys = Vec::new();
        for _ in 0..2 {
            keys.push(rx.recv_timeout(Duration::from_secs(30)).unwrap().key);
        }
        keys.sort();
        assert_eq!(keys, vec![QueryKey(1), QueryKey(2)]);
        pool.shutdown();
    }

    #[test]
    fn the_tag_and_submission_time_are_echoed() {
        let (_d, store) = fixture(100);
        let (pool, rx) = QueryPool::spawn(&store, 1).unwrap();
        let req = request(7, "v", "select sum(v) as v from t");
        let submitted = req.submitted;
        pool.submit(req);
        let r = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(r.key, QueryKey(7));
        assert_eq!(r.tag, 700);
        assert_eq!(r.submitted, submitted);
        pool.shutdown();
    }

    #[test]
    fn a_sink_receives_what_a_channel_would() {
        let (_d, store) = fixture(100);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink: ResultSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |r: QueryResult| {
                seen.lock().unwrap().push(r.key);
                true
            })
        };
        let pool = QueryPool::spawn_with_sink(&store, 1, sink).unwrap();
        pool.submit(request(9, "v", "select sum(v) as v from t"));
        for _ in 0..3000 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        pool.shutdown();
        assert_eq!(*seen.lock().unwrap(), vec![QueryKey(9)]);
    }
```

Update every existing `request("v1", …)` call to `request(1, "v1", …)`,
`request("v2", …)` to `request(2, "v2", …)`, `request("bad", …)` to
`request(1, "bad", …)`, `request("good", …)` to `request(2, "good", …)`.
Change every `pool.cancel(&ViewId("v1".into()))` to
`pool.cancel(QueryKey(1))`. The `results_for_a_view_never_arrive_out_of_order`
and `leaning_on_a_regroup_key…` tests keep one key, which is the point.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data pool:: 2>&1 | grep -E "^error" | head`
Expected: compile errors on `key`, `tag`, `submitted`, `grouping`,
`spawn_with_sink`, `ResultSink`.

- [ ] **Step 3: Implement**

Replace the top of `pool.rs` through `spawn_with` with:

```rust
//! The read pool (spec §6.7, §7.3). Owns the read connections so the UI
//! thread never holds one, coalesces latest-wins per **key**, tags every
//! request and result so a stale arrival can be dropped, and interrupts a
//! superseded query rather than awaiting it.
//!
//! The key is the caller's (spec §2.4) — a tile id in practice — not the
//! view name. Two tiles showing one view must not supersede each other.
//!
//! Results leave through a sink closure rather than a channel the pool
//! owns (spec §5.1): the service hands the pool a closure onto its one
//! outbound channel, so no forwarding thread sits between a worker and
//! the UI. `spawn` still builds a channel for callers that want one.

use crate::query::compile::CompiledQuery;
use crate::store::Store;
use geode_core::query::QueryKey;
use geode_core::snapshot::{ColumnMeta, Provenance, Snapshot};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(pub String);

pub type QueryId = u64;

/// Where results go. Returns `false` when nothing is listening any more,
/// which stops the worker.
pub type ResultSink = Arc<dyn Fn(QueryResult) -> bool + Send + Sync>;

pub struct QueryRequest {
    pub key: QueryKey,
    /// The submitter's own counter, echoed back untouched.
    pub tag: u64,
    pub submitted: Instant,
    /// The view compiled, for messages. Not a coalescing key.
    pub view: ViewId,
    pub compiled: CompiledQuery,
    /// The grouping columns in order; the snapshot builds its tree from
    /// them (spec §5.5).
    pub grouping: Vec<String>,
    pub provenance: Provenance,
}

pub struct QueryResult {
    pub id: QueryId,
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub view: ViewId,
    /// `Err` carries the failure: a bad query degrades its own key and
    /// leaves the pool running (spec §10.1).
    pub snapshot: Result<Snapshot, String>,
}

#[derive(Default)]
struct Queue {
    /// At most one pending request per key: a newer submit replaces the
    /// pending one outright, which is what makes coalescing latest-wins.
    pending: HashMap<QueryKey, (QueryId, QueryRequest)>,
    /// Interrupt handles for queries currently running.
    running: HashMap<QueryKey, (QueryId, Arc<duckdb::InterruptHandle>)>,
    /// Queries interrupted by `cancel`. The worker drops their result
    /// instead of delivering it: an interrupt the caller asked for comes
    /// back from DuckDB as an `Interrupted` error, and reporting that as a
    /// query failure paints an error on a tile the user simply navigated
    /// away from.
    cancelled: std::collections::HashSet<QueryId>,
    /// The last id handed out. Lives in the queue rather than beside it so
    /// that allocating outside the lock is not expressible: ids and
    /// insertion order cannot disagree if they are produced under the same
    /// guard. It was an `AtomicU64` incremented before the lock was taken,
    /// which let two concurrent submits for one key insert out of id
    /// order and leave the *older* request pending — a race no test can
    /// force reliably (measured: caught on 2 runs in 8), so making it
    /// unrepresentable beats testing for it.
    next_id: QueryId,
    shutdown: bool,
}

/// The work a worker does for one request. A function pointer rather than
/// a direct call so a test can inject one that panics — there is no SQL
/// that makes `run_one` panic, and panic-safety is the property most worth
/// testing here.
type RunFn = fn(&duckdb::Connection, &QueryRequest) -> Result<Snapshot, duckdb::Error>;

pub struct QueryPool {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl QueryPool {
    /// Each worker gets its own connection, created here and moved in:
    /// duckdb's `Connection` is `Send` but not `Sync`, so the pool cannot
    /// hand out connections from a shared `Store` after spawning.
    pub fn spawn_with_sink(
        store: &Store,
        workers: usize,
        sink: ResultSink,
    ) -> Result<QueryPool, crate::store::StoreError> {
        Self::spawn_with_run(store, workers, sink, run_one)
    }

    /// A pool delivering into a channel, for callers that block on
    /// results — tests and benches.
    pub fn spawn(
        store: &Store,
        workers: usize,
    ) -> Result<(QueryPool, Receiver<QueryResult>), crate::store::StoreError> {
        let (tx, rx) = channel();
        let sink: ResultSink = Arc::new(move |r| tx.send(r).is_ok());
        Ok((Self::spawn_with_sink(store, workers, sink)?, rx))
    }

    fn spawn_with_run(
        store: &Store,
        workers: usize,
        sink: ResultSink,
        run: RunFn,
    ) -> Result<QueryPool, crate::store::StoreError> {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let mut threads = Vec::new();

        for i in 0..workers.max(1) {
            // A failure part-way through leaves earlier workers running and
            // parked on the condvar. Panicking here would detach them and
            // leak their connections, so unwind deliberately instead: tell
            // the ones already spawned to stop, join them, and report.
            let spawned = store.reader().and_then(|conn| {
                let q = Arc::clone(&queue);
                let sink = Arc::clone(&sink);
                std::thread::Builder::new()
                    .name(format!("geode-query-{i}"))
                    .spawn(move || worker(conn, q, sink, run))
                    .map_err(|source| crate::store::StoreError::SpawnWorker { source })
            });
            match spawned {
                Ok(t) => threads.push(t),
                Err(e) => {
                    {
                        let (lock, cvar) = &*queue;
                        let mut q = lock.lock().unwrap_or_else(|p| p.into_inner());
                        q.shutdown = true;
                        cvar.notify_all();
                    }
                    for t in threads {
                        let _ = t.join();
                    }
                    return Err(e);
                }
            }
        }

        Ok(QueryPool {
            queue,
            threads: Mutex::new(threads),
        })
    }
```

Then, in `submit`, `cancel`, `worker`, and `run_one`:

- `submit`: `q.running.get(&req.key)` and `q.pending.insert(req.key, (id, req))`.
- `cancel(&self, key: QueryKey)`: `q.pending.remove(&key)` and
  `q.running.get(&key)`.
- `worker(conn, queue, sink: ResultSink, run)`: the scheduling loop picks
  `q.pending.keys().find(|k| !q.running.contains_key(*k)).copied()`,
  removes by key, inserts into `running` by key. After the run, the
  stale/cancelled/shutdown checks use `req.key`, and delivery becomes:

```rust
        let delivered = sink(QueryResult {
            id,
            key: req.key,
            tag: req.tag,
            submitted: req.submitted,
            view: req.view.clone(),
            snapshot: outcome,
        });
        if !delivered {
            return;
        }
```

- `run_one`: `Snapshot::from_batches(batches, meta, req.grouping_len, …)`
  stays as-is in this task — `grouping_len` is `req.grouping.len()`:

```rust
    Snapshot::from_batches(batches, meta, req.grouping.len(), req.provenance.clone())
        .map_err(|e| duckdb::Error::InvalidParameterName(e.to_string()))
```

(Task 10 changes `from_batches` to take the names.)

Update the two `spawn_with(&store, 1, boom)` / `gated` test calls to
`spawn_with_run(&store, 1, sink_to(tx), boom)` — add this helper to the
test module:

```rust
    fn channel_pool_with(
        store: &Store,
        run: RunFn,
    ) -> (QueryPool, Receiver<QueryResult>) {
        let (tx, rx) = channel();
        let sink: ResultSink = Arc::new(move |r| tx.send(r).is_ok());
        (QueryPool::spawn_with_run(store, 1, sink, run).unwrap(), rx)
    }
```

and use `let (pool, rx) = channel_pool_with(&store, boom);` in those
three tests.

In `crates/geode-data/src/query/mod.rs`:

```rust
pub use pool::{QueryId, QueryPool, QueryRequest, QueryResult, ResultSink, ViewId};
```

In `crates/geode-data/src/service.rs`, so the crate compiles until Task 3
replaces it: in `query`, build the request with
`key: QueryKey(0), tag: 0, submitted: Instant::now(), grouping:
compiled.grouping.clone()` (drop `grouping_len`), and change `cancel` to
`self.pool.cancel(QueryKey(0))` with `use geode_core::query::QueryKey;
use std::time::Instant;`. This is a placeholder for exactly one task and
is replaced in Task 3.

- [ ] **Step 4: Run the pool tests**

Run: `cargo test -p geode-data pool::`
Expected: all pass, including the three new ones.

- [ ] **Step 5: Full check and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

```bash
git add crates/geode-data
git commit -m "feat(data): key the query pool on the tile, deliver through a sink

Two tiles showing one view no longer supersede each other (spec §2.4).
Results leave through a caller-supplied closure so the service can put
them on its one outbound channel without a forwarding thread (§5.1).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Add the harness entry**

Append to `scripts/mutation-check.sh` before the final unfiltered summary
(a `# ---- query pool (spec §2.4)` section):

```sh
# ---- query pool (spec §2.4, §5.1)

run_mutation "pool: coalescing is keyed on the tile, not the view" \
  crates/geode-data/src/query/pool.rs \
  '        q.pending.insert(req.key, (id, req));' \
  '        let key = QueryKey(0); q.pending.insert(key, (id, req));'

run_mutation "pool: the tag is echoed, not regenerated" \
  crates/geode-data/src/query/pool.rs \
  '            tag: req.tag,' \
  '            tag: 0,'
```

Run: `zsh scripts/mutation-check.sh "pool:"`
Expected: both `caught`. Commit the script.

---

### Task 3: `DataEvent`, `EventSink`, `QueryParams`; the service opens onto a sink

Spec §5.1. The service no longer owns a receiver; everything it produces
goes through one sink. Tests and benches use `open_channel`.

**Files:**
- Modify: `crates/geode-data/src/service.rs`
- Modify: `crates/geode-data/src/lib.rs` (re-exports)
- Modify: `crates/geode-data/benches/query.rs:180-215` (`service()` and
  `requery()`)
- Modify: `crates/geode-app/src/probe.rs:266-305` (`query_once`) — the
  minimum to compile; Task 8 rewrites the file
- Test: `crates/geode-data/src/service.rs` (inline)

**Interfaces:**
- Consumes: Task 2's pool API; `geode_core::query::{QueryKey, QueryOutcome, AsOf}`.
- Produces:
  ```rust
  pub enum DataEvent {
      Query(QueryOutcome),
      Published { dataset: String, batch: String, gen_id: i64, books: Vec<Option<String>> },
      Health { source: String, worst: Health, detail: String },
      Diagnostics(Vec<Diagnostic>),
  }
  pub type EventSink = Arc<dyn Fn(DataEvent) -> bool + Send + Sync>;
  pub struct QueryParams { pub key: QueryKey, pub tag: u64, pub submitted: Instant,
                           pub view: String, pub scope: Scope, pub as_of: AsOf, pub max_depth: usize }
  impl DataService {
      pub fn open(config: DataServiceConfig, sink: EventSink) -> Result<DataService, StoreError>;
      pub fn open_channel(config) -> Result<(DataService, Receiver<DataEvent>), StoreError>;
      pub fn query(&self, params: &QueryParams) -> Result<QueryId, StoreError>;
      pub fn cancel(&self, key: QueryKey);
      pub fn replace_views(&mut self, views: Vec<ViewSpec>, dimensions: DerivedDimensions) -> Vec<Diagnostic>;
  }
  ```
  `query_results()` is removed. `Published` and `Health` are emitted from
  Task 6 on; this task defines them.

- [ ] **Step 1: Write the failing tests**

Replace the `service()` and `next()` helpers in `service.rs`'s test
module, and add three tests:

```rust
    fn service() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let (db, src, store, ds, emitted) = crate::ingest::load::tests_support::fixture();
        for file in emitted.files.iter().filter(|f| f.sentinel_path.is_some()) {
            let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
            let sentinel = crate::source::parse_sentinel(&text).unwrap();
            let batch = crate::ingest::load::tests_support::batch_of(&file.csv_path);
            let _ = crate::ingest::load_file(
                &store,
                &crate::ingest::LoadRequest {
                    dataset: &ds,
                    dataset_name: "risk_snapshot",
                    csv_path: &file.csv_path,
                    sentinel: &sentinel,
                    batch: &batch,
                },
            );
        }
        drop(store);

        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![crate::ingest::load::tests_support::tree_view()],
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: Vec::new(),
        })
        .unwrap();
        (db, src, service, rx)
    }

    /// The next query outcome, skipping any other event.
    fn next(rx: &std::sync::mpsc::Receiver<DataEvent>) -> QueryOutcome {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Query(o) => return o,
                _ => continue,
            }
        }
    }

    fn params(key: u64, view: &str, scope: &Scope, as_of: AsOf, max_depth: usize) -> QueryParams {
        QueryParams {
            key: QueryKey(key),
            tag: key,
            submitted: Instant::now(),
            view: view.to_string(),
            scope: scope.clone(),
            as_of,
            max_depth,
        }
    }

    #[test]
    fn an_outcome_is_addressed_to_the_key_that_asked() {
        let (_db, _src, svc, rx) = service();
        svc.query(&params(42, "tree", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        let o = next(&rx);
        assert_eq!(o.key, QueryKey(42));
        assert_eq!(o.tag, 42);
        assert!(o.snapshot.unwrap().rows() > 0);
        svc.shutdown();
    }

    #[test]
    fn replacing_views_makes_a_new_view_queryable_and_reports_a_bad_one() {
        let (_db, _src, mut svc, rx) = service();
        let mut renamed = crate::ingest::load::tests_support::tree_view();
        renamed.name = "tree2".into();
        let mut broken = crate::ingest::load::tests_support::tree_view();
        broken.name = "broken".into();
        broken.grouping.push("nosuchcolumn".into());

        let diags = svc.replace_views(vec![renamed, broken], DerivedDimensions::default());
        assert!(
            diags.iter().any(|d| d.message.contains("broken")),
            "{diags:?}"
        );
        assert!(
            svc.query(&params(1, "tree", &Scope::default(), AsOf::Live, 1))
                .is_err(),
            "the old view name is gone"
        );
        svc.query(&params(2, "tree2", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        assert!(next(&rx).snapshot.is_ok());
        svc.shutdown();
    }

    #[test]
    fn a_sink_that_reports_nobody_listening_does_not_wedge_the_service() {
        // A closed sink is how the UI goes away. The service must keep
        // accepting requests without panicking; results simply have
        // nowhere to go.
        let (db, _src, svc, rx) = service();
        svc.shutdown();
        drop(rx);
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let sink: EventSink = Arc::new(|_| false);
        let svc = DataService::open(
            DataServiceConfig {
                db_path: db.path().join("geode.duckdb"),
                schema,
                views: vec![crate::ingest::load::tests_support::tree_view()],
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
            },
            sink,
        )
        .unwrap();
        svc.query(&params(1, "tree", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        svc.shutdown();
    }
```

Update the other tests: every `svc.query("tree", &Scope::default(),
AsOf::Live, usize::MAX)` becomes `svc.query(&params(1, "tree",
&Scope::default(), AsOf::Live, usize::MAX))`; every `next(&svc)` becomes
`next(&rx)` with `rx` bound from `service()`; `a_misconfigured_view…`
uses `DataService::open_channel(...)` and reads `.0.diagnostics()`;
`a_historical_result…` keeps `svc._store.writer()` for now (Task 6 moves
the store; that test is adjusted there).

`DataServiceConfig` gains `pub sources: Vec<SourceSpec>` now (empty in
every test until Task 6) so the struct changes once.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data service:: 2>&1 | grep -E "^error" | head`
Expected: compile errors on `DataEvent`, `open_channel`, `QueryParams`,
`sources`, `replace_views`.

- [ ] **Step 3: Implement**

At the top of `service.rs`:

```rust
use crate::health::Health;
use crate::query::as_of::AsOf;
use crate::query::compile::compile_view;
use crate::query::pool::{QueryId, QueryPool, QueryRequest, QueryResult, ResultSink, ViewId};
use crate::source::SourceSpec;
use crate::store::catalog::BookFreshness;
use crate::store::{Catalog, Store, StoreError};
use chrono::{DateTime, Utc};
use geode_core::config::Diagnostic;
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{QueryKey, QueryOutcome};
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::snapshot::{Freshness, Provenance};
use geode_core::view::ViewSpec;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

pub struct DataServiceConfig {
    pub db_path: PathBuf,
    pub schema: SchemaSpec,
    pub views: Vec<ViewSpec>,
    pub dimensions: DerivedDimensions,
    pub query_workers: usize,
    /// Configured sources (spec §5.2). Empty means nothing is ever
    /// ingested — a warm database is queried as it stands.
    pub sources: Vec<SourceSpec>,
}

/// Everything the service produces, on one channel (spec §5.1).
#[derive(Debug)]
pub enum DataEvent {
    Query(QueryOutcome),
    /// A file was published: the frame bumps its data generation and every
    /// visible tile requeries. A burst coalesces there.
    Published {
        dataset: String,
        batch: String,
        gen_id: i64,
        books: Vec<Option<String>>,
    },
    /// The worst state discovery found for a source on its last poll.
    Health {
        source: String,
        worst: Health,
        detail: String,
    },
    /// Config problems found at open or on a view reload (§10.1).
    Diagnostics(Vec<Diagnostic>),
}

/// Where events go. `false` means nobody is listening.
pub type EventSink = Arc<dyn Fn(DataEvent) -> bool + Send + Sync>;

/// One query, as a module asks for it.
#[derive(Debug, Clone)]
pub struct QueryParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub view: String,
    pub scope: Scope,
    pub as_of: AsOf,
    pub max_depth: usize,
}
```

`DataService` fields become:

```rust
pub struct DataService {
    config: DataServiceConfig,
    diagnostics: Vec<Diagnostic>,
    sink: EventSink,
    /// Field order is drop order: the pool joins its workers before the
    /// connections they read through go.
    pool: QueryPool,
    conn: duckdb::Connection,
    _store: Store,
}
```

`open` and the new functions:

```rust
impl DataService {
    pub fn open(config: DataServiceConfig, sink: EventSink) -> Result<DataService, StoreError> {
        let store = Store::open(&config.db_path)?;
        for ds in &config.schema.datasets {
            store.apply_schema(ds)?;
        }
        Catalog::new(store.writer()).ensure_tables()?;
        let conn = store.reader()?;
        let result_sink: ResultSink = {
            let sink = Arc::clone(&sink);
            Arc::new(move |r: QueryResult| {
                sink(DataEvent::Query(QueryOutcome {
                    key: r.key,
                    tag: r.tag,
                    snapshot: r.snapshot.map(Arc::new),
                    submitted: r.submitted,
                }))
            })
        };
        let pool = QueryPool::spawn_with_sink(&store, config.query_workers.max(1), result_sink)?;
        let diagnostics = config
            .views
            .iter()
            .flat_map(|v| v.validate(&config.schema, &config.dimensions))
            .collect();
        Ok(DataService {
            config,
            diagnostics,
            sink,
            _store: store,
            conn,
            pool,
        })
    }

    /// A service delivering into a channel, for callers that block on
    /// events — tests, benches, and the probe.
    pub fn open_channel(
        config: DataServiceConfig,
    ) -> Result<(DataService, Receiver<DataEvent>), StoreError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        Ok((Self::open(config, sink)?, rx))
    }

    /// Swap the view set (a safe hot reload, foundation §8). Returns what
    /// validation found; a broken view is reported and skipped, the rest
    /// take effect.
    pub fn replace_views(
        &mut self,
        views: Vec<ViewSpec>,
        dimensions: DerivedDimensions,
    ) -> Vec<Diagnostic> {
        self.config.dimensions = dimensions;
        let diagnostics: Vec<Diagnostic> = views
            .iter()
            .flat_map(|v| v.validate(&self.config.schema, &self.config.dimensions))
            .collect();
        self.config.views = views;
        self.diagnostics = diagnostics.clone();
        diagnostics
    }
```

(`replace_views` keeps every view, including a broken one, exactly as
`open` does: the broken one fails at query time with a message naming it,
and the diagnostic is what says why. Silently dropping it would make a
`:view` naming it look like a typo.)

`query` takes `&QueryParams`:

```rust
    pub fn query(&self, params: &QueryParams) -> Result<QueryId, StoreError> {
        let view = params.view.as_str();
        let spec = self
            .config
            .views
            .iter()
            .find(|v| v.name == view)
            .ok_or_else(|| StoreError::Sql {
                statement: format!("query view '{view}'"),
                source: duckdb::Error::InvalidParameterName(format!("unknown view '{view}'")),
            })?;

        let compiled = compile_view(
            &self.conn,
            spec,
            &self.config.schema,
            &params.scope,
            &self.config.dimensions,
            &params.as_of,
            params.max_depth,
        )?;
        // … the provenance block is unchanged, reading `params.as_of` …
        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            view: ViewId(view.to_string()),
            grouping: compiled.grouping.clone(),
            compiled,
            provenance,
        }))
    }

    pub fn cancel(&self, key: QueryKey) {
        self.pool.cancel(key);
    }
```

Delete `query_results`. `freshness`, `as_of_bounds`, `diagnostics`,
`validate_scope`, `shutdown` are unchanged. The `sink` field is unused
until Task 6; mark it `#[allow(dead_code)]` with a comment `// Task 6
hands this to the runner and scheduler.` so clippy stays clean.

In `crates/geode-data/src/lib.rs`:

```rust
pub use service::{DataEvent, DataService, DataServiceConfig, EventSink, QueryParams};
```

In `benches/query.rs`, `service()` returns
`(db, src, DataService, Receiver<DataEvent>, usize)` via `open_channel`
with `sources: Vec::new()`, and `requery` becomes:

```rust
fn requery(
    svc: &DataService,
    rx: &std::sync::mpsc::Receiver<geode_data::DataEvent>,
    view: &str,
    scope: &Scope,
    max_depth: usize,
) -> usize {
    svc.query(&QueryParams {
        key: QueryKey(1),
        tag: 0,
        submitted: std::time::Instant::now(),
        view: view.to_string(),
        scope: scope.clone(),
        as_of: AsOf::Live,
        max_depth,
    })
    .unwrap();
    loop {
        match rx.recv_timeout(Duration::from_secs(120)).expect("no result") {
            geode_data::DataEvent::Query(o) => return o.snapshot.expect("query failed").rows(),
            _ => continue,
        }
    }
}
```

with `use geode_core::query::QueryKey; use geode_data::QueryParams;` and
every `requery(&svc, …)` call passing `&rx` after `&svc`.

In `probe.rs`, `query_once` and `run` change minimally: `run` opens with
`DataService::open_channel(...)` binding `(service, rx)`, passes `&rx` to
`query_once`, which submits `QueryParams { key: QueryKey(1), tag: 0,
submitted: Instant::now(), view: setup.view.clone(), scope:
Scope::default(), as_of: AsOf::Live, max_depth: MAX_DEPTH }` and loops
on `rx.recv_timeout` until a `DataEvent::Query`. Add `sources:
Vec::new()` to its `DataServiceConfig`.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-data service:: && cargo bench --workspace --no-run && cargo build -p geode-app`
Expected: all green.

- [ ] **Step 5: Full check and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`

```bash
git add crates/geode-data crates/geode-app/src/probe.rs
git commit -m "feat(data): DataEvent on one sink; QueryParams; replace_views

The service no longer owns a results receiver: everything it produces
leaves through one caller-supplied sink (spec §5.1). Queries are keyed
and tagged by the caller. replace_views is the safe hot-reload path for
views (foundation §8).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Harness entry**

```sh
run_mutation "service: an outcome carries the caller's key" \
  crates/geode-data/src/service.rs \
  '                    key: r.key,' \
  '                    key: QueryKey(0),'

run_mutation "service: replace_views actually replaces" \
  crates/geode-data/src/service.rs \
  '        self.config.views = views;' \
  '        let _ = views;'
```

Run: `zsh scripts/mutation-check.sh "service:"` — both `caught`. Commit.

---

### Task 4: One ingest runner for every dataset, delivering through a sink

Spec §2.5. The runner takes the `Store` and the whole `SchemaSpec`, so
there is exactly one writer thread however many datasets are declared.

**Files:**
- Modify: `crates/geode-data/src/ingest/runner.rs`
- Modify: `crates/geode-data/src/ingest/mod.rs` (re-export `IngestSink`)
- Modify: `crates/geode-app/src/probe.rs:155-200` (`ingest`) — compile
  only; Task 8 rewrites it
- Test: `crates/geode-data/src/ingest/runner.rs` (inline)

**Interfaces:**
- Produces:
  ```rust
  pub type IngestSink = Arc<dyn Fn(IngestEvent) -> bool + Send + Sync>;
  pub enum IngestEvent {
      Published { dataset, batch, gen_id, books: Vec<Option<String>>, rows: usize, health: Health },
      Failed { dataset: String, batch: String, reason: String },
      PlanComplete,
  }
  impl IngestRunner {
      pub fn spawn(store: Store, schema: SchemaSpec, sink: IngestSink) -> IngestHandle;
      pub fn spawn_channel(store: Store, schema: SchemaSpec) -> (IngestHandle, Receiver<IngestEvent>);
  }
  ```

- [ ] **Step 1: Write the failing tests**

In `runner.rs`'s test module, add a helper and one test, and update the
five existing `IngestRunner::spawn(store, ds, "risk_snapshot".into())`
calls to `IngestRunner::spawn_channel(store, schema_of(ds))`:

```rust
    fn schema_of(ds: DatasetSpec) -> geode_core::schema::SchemaSpec {
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(ds);
        schema
    }

    #[test]
    fn an_item_naming_an_undeclared_dataset_fails_by_name_and_the_runner_continues() {
        // One runner serves every dataset (spec §2.5), so an item can name
        // a dataset the schema does not declare — a sources.toml pointing
        // at a dataset that a later datasets.toml edit removed. It must be
        // reported as that item's failure, not a panic and not silence.
        let (_db, _src, store, ds, plan) = harness();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        let mut wrong = plan.items[0].clone();
        wrong.dataset = "nonesuch".into();
        let good = plan.items[1].clone();
        handle.submit(WorkPlan {
            items: vec![wrong, good],
        });
        let events = drain(&rx, 2);
        assert!(
            events.iter().any(|e| matches!(
                e,
                IngestEvent::Failed { dataset, reason, .. }
                    if dataset == "nonesuch" && reason.contains("not declared")
            )),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, IngestEvent::Published { .. })),
            "the good item still loads: {events:?}"
        );
        handle.shutdown();
    }
```

Every existing test that matches `IngestEvent::Failed { batch, reason }`
adds `..`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data runner:: 2>&1 | grep -E "^error" | head`
Expected: `spawn_channel` and `dataset` field missing.

- [ ] **Step 3: Implement**

Replace the module doc's first two paragraphs and the runner API:

```rust
//! The ingest runner (spec §5.4–§5.7, Phase 3 §2.5). One thread owning
//! the writer connection for **every** dataset, working a
//! priority-ordered queue, never taking the app down.
//!
//! One thread, not a pool, and one for all datasets rather than one per
//! dataset: DuckDB is single-writer, so every publish serializes anyway
//! (spec §5.3), and a runner per dataset would make that discipline a
//! convention held by whoever spawned them. The `Store` — and with it the
//! writer — lives here; the service keeps only reader connections cloned
//! before the store moved (Phase 3 §5.3).
```

(Keep the paragraphs about cold-start parallelism and preemption
granularity verbatim.)

```rust
use crate::health::Health;
use crate::ingest::load::{LoadRequest, load_file};
use crate::ingest::plan::{WorkItem, WorkPlan};
use crate::source::{CandidateState, Priority};
use crate::store::Store;
use geode_core::schema::SchemaSpec;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone)]
pub enum IngestEvent {
    Published {
        dataset: String,
        batch: String,
        gen_id: i64,
        /// The partitions written; `None` is the bookless one.
        books: Vec<Option<String>>,
        rows: usize,
        health: Health,
    },
    Failed {
        dataset: String,
        batch: String,
        reason: String,
    },
    /// The queue drained. Not a terminal state — more work may be submitted.
    PlanComplete,
}

/// Where events go. `false` means nobody is listening, which stops the
/// runner.
pub type IngestSink = Arc<dyn Fn(IngestEvent) -> bool + Send + Sync>;

impl IngestRunner {
    pub fn spawn(store: Store, schema: SchemaSpec, sink: IngestSink) -> IngestHandle {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);
        let thread = std::thread::Builder::new()
            .name("geode-ingest".into())
            .spawn(move || run(store, schema, worker_queue, sink))
            .expect("spawning the ingest thread");
        IngestHandle {
            queue,
            thread: Mutex::new(Some(thread)),
        }
    }

    /// A runner delivering into a channel, for callers that block on
    /// events — tests and the cold-start bench.
    pub fn spawn_channel(store: Store, schema: SchemaSpec) -> (IngestHandle, Receiver<IngestEvent>) {
        let (tx, rx) = channel();
        let sink: IngestSink = Arc::new(move |e| tx.send(e).is_ok());
        (Self::spawn(store, schema, sink), rx)
    }
}
```

`run` becomes:

```rust
fn run(
    store: Store,
    schema: SchemaSpec,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    sink: IngestSink,
) {
    let mut announced_idle = false;

    loop {
        let item = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                if !q.items.is_empty() {
                    announced_idle = false;
                    break q.items.remove(0);
                }
                if !announced_idle {
                    announced_idle = true;
                    if !sink(IngestEvent::PlanComplete) {
                        return;
                    }
                }
                let (guard, _) = cvar
                    .wait_timeout(q, std::time::Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner());
                q = guard;
            }
        };

        // The dataset is resolved per item (Phase 3 §2.5). An undeclared
        // one is this item's failure, named, and the runner carries on.
        let Some(dataset) = schema.dataset(&item.dataset) else {
            if !sink(IngestEvent::Failed {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: format!("dataset '{}' is not declared", item.dataset),
            }) {
                return;
            }
            continue;
        };

        // Panic boundary (spec §5.7): a panicking load degrades its file and
        // the runner keeps working. Only a render-thread panic takes the app
        // down.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let CandidateState::Ready(sentinel) = &item.candidate.state else {
                return Err("candidate was not ready".to_string());
            };
            load_file(
                &store,
                &LoadRequest {
                    dataset,
                    dataset_name: &item.dataset,
                    csv_path: &item.candidate.csv_path,
                    sentinel,
                    batch: &item.batch,
                },
            )
            .map_err(|e| e.to_string())
        }));

        let event = match outcome {
            Ok(Ok(loaded)) => IngestEvent::Published {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                gen_id: loaded.gen_id,
                books: loaded.partitions.clone(),
                rows: loaded.rows,
                health: loaded.health,
            },
            Ok(Err(reason)) => IngestEvent::Failed {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason,
            },
            Err(_) => IngestEvent::Failed {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: "ingest task panicked".into(),
            },
        };
        if !sink(event) {
            return; // receiver gone: nothing left to report to
        }
    }
}
```

(Keep the `books:` comment about the sentinel being advisory.) In
`ingest/mod.rs`: `pub use runner::{IngestEvent, IngestHandle, IngestRunner, IngestSink};`.

In `probe.rs`'s `ingest`, replace `IngestRunner::spawn(store, ds,
setup.dataset.clone())` with `IngestRunner::spawn_channel(store,
setup.schema.clone())` and add `..` to its `Failed` match arm.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-data runner:: && cargo build -p geode-app`
Expected: green.

- [ ] **Step 5: Full check and commit**

```bash
git add crates/geode-data crates/geode-app/src/probe.rs
git commit -m "feat(data): one ingest runner for every dataset, owning the store

The runner takes the Store and the SchemaSpec and resolves the dataset
per work item, so there is exactly one writer thread (Phase 3 §2.5).
Events leave through a sink; an undeclared dataset is a named failure.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Harness entry**

```sh
# ---- ingest runner (Phase 3 §2.5)

run_mutation "runner: an undeclared dataset is a named failure, not a skip" \
  crates/geode-data/src/ingest/runner.rs \
  '            if !sink(IngestEvent::Failed {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: format!("dataset '"'"'{}'"'"' is not declared", item.dataset),
            }) {
                return;
            }
            continue;' \
  '            continue;'
```

Run: `zsh scripts/mutation-check.sh "runner:"` — `caught`. Commit.

---

### Task 5: `sources.toml` → `SourceSpec`

Spec §5.2. One named table per source, atomic by name; durations as
`Ns`/`Nm`/`Nh`; every error a diagnostic.

**Files:**
- Create: `crates/geode-data/src/source/config.rs`
- Modify: `crates/geode-data/src/source/mod.rs` (`pub mod config;` and
  `pub use config::parse_duration;`)
- Test: `crates/geode-data/src/source/config.rs` (inline)

**Interfaces:**
- Consumes: `geode_core::config::{MergedDoc, Diagnostic, Severity}`,
  `geode_core::schema::SchemaSpec`, `SourceSpec`, `Readiness`,
  `Priority`.
- Produces:
  - `pub fn parse_duration(s: &str) -> Option<Duration>`
  - `impl SourceSpec { pub fn from_doc(doc: &MergedDoc, schema: &SchemaSpec) -> (Vec<SourceSpec>, Vec<Diagnostic>) }`

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-data/src/source/config.rs` with the signatures
stubbed as `todo!()` and this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Priority, Readiness};
    use geode_core::config::{LayerDoc, Severity, merge_docs};
    use geode_core::schema::SchemaSpec;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn parse(text: &str) -> (Vec<SourceSpec>, Vec<geode_core::config::Diagnostic>) {
        let doc = merge_docs("sources", &[LayerDoc::builtin("sources", text).unwrap()]);
        SourceSpec::from_doc(&doc, &schema())
    }

    #[test]
    fn durations_are_seconds_minutes_or_hours() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("0s"), Some(Duration::ZERO));
        assert_eq!(parse_duration("30"), None, "a bare number has no unit");
        assert_eq!(parse_duration("1.5h"), None, "integers only");
        assert_eq!(parse_duration("s"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn a_full_declaration_round_trips() {
        let (specs, diags) = parse(
            r#"
[risk_files]
dataset = "risk_snapshot"
paths = ["/mnt/risk/current/*.csv", "//share/risk/**/*.csv"]
readiness = "sentinel"
priority = "latest_other"
poll_interval = "45s"
pending_timeout = "2h"
batch_pattern = '^risk_\d{4}-\d{2}-\d{2}_(?P<batch>.+)$'
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.name, "risk_files");
        assert_eq!(s.dataset, "risk_snapshot");
        assert_eq!(s.paths.len(), 2);
        assert_eq!(s.readiness, Readiness::Sentinel);
        assert_eq!(s.priority, Priority::LatestOther);
        assert_eq!(s.poll_interval, Duration::from_secs(45));
        assert_eq!(s.pending_timeout, Duration::from_secs(7200));
        assert_eq!(
            s.batch_of(std::path::Path::new("/x/risk_2026-09-03_BK000_part1.csv")),
            "BK000_part1"
        );
    }

    #[test]
    fn defaults_fill_what_is_omitted() {
        let (specs, diags) = parse(
            r#"
[risk_files]
dataset = "risk_snapshot"
paths = ["/mnt/risk/*.csv"]
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let s = &specs[0];
        assert_eq!(s.readiness, Readiness::Sentinel);
        assert_eq!(s.priority, Priority::LatestRisk);
        assert_eq!(s.poll_interval, Duration::from_secs(30));
        assert_eq!(s.pending_timeout, Duration::from_secs(600));
        assert_eq!(s.batch_pattern, None);
    }

    #[test]
    fn stable_mtime_readiness_is_a_table() {
        let (specs, _) = parse(
            r#"
[vol]
dataset = "risk_snapshot"
paths = ["/mnt/vol/*.csv"]
readiness = { stable_mtime = 3 }
"#,
        );
        assert_eq!(specs[0].readiness, Readiness::StableMtime { polls: 3 });
    }

    #[test]
    fn a_missing_or_unknown_dataset_is_an_error_and_the_source_is_skipped() {
        let (specs, diags) = parse(
            r#"
[a]
paths = ["/x/*.csv"]
[b]
dataset = "nonesuch"
paths = ["/x/*.csv"]
[c]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
"#,
        );
        assert_eq!(specs.len(), 1, "only c survives: {specs:?}");
        assert_eq!(specs[0].name, "c");
        let errors: Vec<&str> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(errors.len(), 2, "{diags:?}");
        assert!(errors[0].contains("'a'") && errors[0].contains("dataset"));
        assert!(errors[1].contains("'b'") && errors[1].contains("nonesuch"));
    }

    #[test]
    fn missing_or_empty_paths_is_an_error() {
        let (specs, diags) = parse(
            r#"
[a]
dataset = "risk_snapshot"
[b]
dataset = "risk_snapshot"
paths = []
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .count(),
            2,
            "{diags:?}"
        );
    }

    #[test]
    fn a_bad_duration_priority_or_readiness_warns_and_uses_the_default() {
        let (specs, diags) = parse(
            r#"
[a]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
poll_interval = "soon"
pending_timeout = "1.5h"
priority = "urgent"
readiness = "hope"
"#,
        );
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.poll_interval, Duration::from_secs(30));
        assert_eq!(s.pending_timeout, Duration::from_secs(600));
        assert_eq!(s.priority, Priority::LatestRisk);
        assert_eq!(s.readiness, Readiness::Sentinel);
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.severity == Severity::Warning)
                .count(),
            4,
            "{diags:?}"
        );
    }

    #[test]
    fn an_uncompilable_batch_pattern_is_dropped_with_a_warning() {
        let (specs, diags) = parse(
            r#"
[a]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
batch_pattern = "(?P<batch>unclosed"
"#,
        );
        assert_eq!(specs[0].batch_pattern, None);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("batch_pattern")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_pattern_without_a_batch_capture_is_dropped_with_a_warning() {
        // A pattern that compiles but never captures `batch` would make
        // every file's batch its whole stem — silently defeating §4.3.
        let (specs, diags) = parse(
            r#"
[a]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
batch_pattern = "^risk_.*$"
"#,
        );
        assert_eq!(specs[0].batch_pattern, None);
        assert!(
            diags.iter().any(|d| d.message.contains("batch")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_non_table_entry_is_skipped_with_a_warning() {
        let (specs, diags) = parse("config_version = 1\n");
        assert!(specs.is_empty());
        assert!(
            diags.is_empty(),
            "config_version is not a source and not a complaint: {diags:?}"
        );
        let (specs, diags) = parse("stray = 3\n");
        assert!(specs.is_empty());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data source::config:: 2>&1 | tail -5`
Expected: panics on `todo!()`.

- [ ] **Step 3: Implement**

```rust
//! `sources.toml` (Phase 3 spec §5.2): one named table per source,
//! atomic by name like every other named config object (foundation §8).
//! Every problem is a diagnostic; a source that cannot be used is skipped
//! and the rest load.

use crate::source::{Priority, Readiness, SourceSpec};
use geode_core::config::{Diagnostic, MergedDoc, Severity};
use geode_core::schema::SchemaSpec;
use std::time::Duration;

const DEFAULT_POLL: Duration = Duration::from_secs(30);
const DEFAULT_PENDING_TIMEOUT: Duration = Duration::from_secs(600);

/// `30s`, `10m`, `2h` — integers with one of three units. Nothing else:
/// a bare number has no unit and a fraction has no convention.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (digits, unit) = s.split_at(s.len().checked_sub(1)?);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

fn diag(severity: Severity, name: &str, m: impl std::fmt::Display) -> Diagnostic {
    Diagnostic {
        severity,
        layer: None,
        file: None,
        message: format!("source '{name}': {m}"),
    }
}

impl SourceSpec {
    pub fn from_doc(doc: &MergedDoc, schema: &SchemaSpec) -> (Vec<SourceSpec>, Vec<Diagnostic>) {
        let mut out = Vec::new();
        let mut diags = Vec::new();

        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let Some(table) = value.as_table() else {
                diags.push(diag(Severity::Warning, name, "not a table"));
                continue;
            };

            let dataset = match table.get("dataset").and_then(|v| v.as_str()) {
                Some(d) if schema.dataset(d).is_some() => d.to_string(),
                Some(d) => {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        format!("names undeclared dataset '{d}'"),
                    ));
                    continue;
                }
                None => {
                    diags.push(diag(Severity::Error, name, "missing 'dataset'"));
                    continue;
                }
            };

            let paths: Vec<String> = table
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if paths.is_empty() {
                diags.push(diag(Severity::Error, name, "missing or empty 'paths'"));
                continue;
            }

            let readiness = match table.get("readiness") {
                None => Readiness::Sentinel,
                Some(v) if v.as_str() == Some("sentinel") => Readiness::Sentinel,
                Some(v) => match v
                    .as_table()
                    .and_then(|t| t.get("stable_mtime"))
                    .and_then(|p| p.as_integer())
                {
                    Some(polls) if polls > 0 => Readiness::StableMtime {
                        polls: polls as u32,
                    },
                    _ => {
                        diags.push(diag(
                            Severity::Warning,
                            name,
                            format!("unrecognised readiness {v}; using \"sentinel\""),
                        ));
                        Readiness::Sentinel
                    }
                },
            };

            let priority = match table.get("priority").and_then(|v| v.as_str()) {
                None | Some("latest_risk") => Priority::LatestRisk,
                Some("latest_other") => Priority::LatestOther,
                Some("backfill") => Priority::Backfill,
                Some(other) => {
                    diags.push(diag(
                        Severity::Warning,
                        name,
                        format!("unknown priority '{other}'; using \"latest_risk\""),
                    ));
                    Priority::LatestRisk
                }
            };

            let mut duration = |key: &str, default: Duration| -> Duration {
                match table.get(key) {
                    None => default,
                    Some(v) => match v.as_str().and_then(parse_duration) {
                        Some(d) => d,
                        None => {
                            diags.push(diag(
                                Severity::Warning,
                                name,
                                format!(
                                    "'{key}' must be an integer with unit s, m or h \
                                     (got {v}); using {}s",
                                    default.as_secs()
                                ),
                            ));
                            default
                        }
                    },
                }
            };
            let poll_interval = duration("poll_interval", DEFAULT_POLL);
            let pending_timeout = duration("pending_timeout", DEFAULT_PENDING_TIMEOUT);

            let batch_pattern = match table.get("batch_pattern").and_then(|v| v.as_str()) {
                None => None,
                Some(p) => match regex::Regex::new(p) {
                    Ok(re) if re.capture_names().any(|c| c == Some("batch")) => {
                        Some(p.to_string())
                    }
                    Ok(_) => {
                        diags.push(diag(
                            Severity::Warning,
                            name,
                            "'batch_pattern' has no named `batch` capture; ignoring it \
                             (every file's batch would be its whole stem)",
                        ));
                        None
                    }
                    Err(e) => {
                        diags.push(diag(
                            Severity::Warning,
                            name,
                            format!("'batch_pattern' does not compile: {e}; ignoring it"),
                        ));
                        None
                    }
                },
            };

            out.push(SourceSpec {
                name: name.clone(),
                dataset,
                paths,
                readiness,
                priority,
                poll_interval,
                pending_timeout,
                batch_pattern,
            });
        }

        (out, diags)
    }
}
```

(`duration` is a `FnMut` closure borrowing `diags` mutably; call both
before any later `diags.push` — the batch-pattern block comes after, so
the closure's borrow has ended. If the borrow checker objects, make
`duration` a free `fn` taking `&mut Vec<Diagnostic>`.)

- [ ] **Step 4: Run**

Run: `cargo test -p geode-data source::config::`
Expected: 10 passed.

- [ ] **Step 5: Full check and commit**

```bash
git add crates/geode-data/src/source
git commit -m "feat(data): sources.toml is read into SourceSpecs

One named table per source, defaults for what is omitted, and every
problem a diagnostic that names the source (Phase 3 §5.2).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Harness entries**

```sh
# ---- sources config (Phase 3 §5.2)

run_mutation "sources: an undeclared dataset skips the source" \
  crates/geode-data/src/source/config.rs \
  '                Some(d) if schema.dataset(d).is_some() => d.to_string(),' \
  '                Some(d) => d.to_string(),'

run_mutation "sources: a pattern without a batch capture is dropped" \
  crates/geode-data/src/source/config.rs \
  '                    Ok(re) if re.capture_names().any(|c| c == Some("batch")) => {' \
  '                    Ok(re) if re.capture_names().count() > 0 => {'
```

Run: `zsh scripts/mutation-check.sh "sources:"` — both `caught`. Commit.

---

### Task 6: The discovery scheduler inside `DataService`

Spec §5.3. The service owns the runner and a discovery thread; the
`Store` moves onto the runner; publishes become `DataEvent::Published`.

**Files:**
- Create: `crates/geode-data/src/ingest/scheduler.rs`
- Modify: `crates/geode-data/src/ingest/mod.rs` (`pub mod scheduler;`)
- Modify: `crates/geode-data/src/service.rs` (`open` steps 2–5 of §5.3;
  drop `_store`)
- Test: `crates/geode-data/src/ingest/scheduler.rs` and `service.rs`

**Interfaces:**
- Consumes: Task 4's runner; `discover`, `build_plan`; `Catalog`.
- Produces:
  ```rust
  pub enum SchedulerEvent {
      Polled { source: String, ready: usize },
      Health { source: String, worst: Health, detail: String },
  }
  pub type SchedulerSink = Arc<dyn Fn(SchedulerEvent) -> bool + Send + Sync>;
  impl Scheduler {
      pub fn spawn(sources: Vec<SourceSpec>, conn: duckdb::Connection,
                   ingest: Arc<IngestHandle>, sink: SchedulerSink) -> Scheduler;
      pub fn shutdown(&self);
  }
  ```

- [ ] **Step 1: Write the failing scheduler tests**

Create `crates/geode-data/src/ingest/scheduler.rs` with the API stubbed
and:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{IngestEvent, IngestRunner};
    use crate::source::{Priority, Readiness};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    /// A store with the fixture schema, an empty source directory, and a
    /// runner delivering into a channel.
    fn harness(poll: Duration, pending_timeout: Duration) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Arc<IngestHandle>,
        Receiver<IngestEvent>,
        duckdb::Connection,
        SourceSpec,
        geode_core::schema::DatasetSpec,
    ) {
        let (db, src, store, ds, _emitted) = crate::ingest::load::tests_support::fixture();
        // The fixture emitted files; the tests below want an empty
        // directory to start from, so use a fresh one.
        let empty = tempfile::tempdir().unwrap();
        let conn = store.reader().unwrap();
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(ds.clone());
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        let spec = SourceSpec {
            name: "risk".into(),
            dataset: "risk_snapshot".into(),
            paths: vec![format!("{}/*.csv", empty.path().display())],
            readiness: Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: poll,
            pending_timeout,
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
        };
        drop(src);
        (db, empty, Arc::new(handle), rx, conn, spec, ds)
    }

    fn events_sink() -> (SchedulerSink, Receiver<SchedulerEvent>) {
        let (tx, rx) = channel();
        (Arc::new(move |e| tx.send(e).is_ok()), rx)
    }

    #[test]
    fn a_file_that_appears_after_start_is_discovered_and_published() {
        // The whole point of the scheduler (Phase 3 §2.8): the probe
        // discovered once and never again.
        let (_db, dir, ingest, ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(50), Duration::from_secs(3600));
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, Arc::clone(&ingest), sink);

        // First poll: nothing there.
        let first = sched_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(first, SchedulerEvent::Polled { ready: 0, .. }), "{first:?}");

        // Now a file lands.
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 500,
            seed: 7,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(dir.path());
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts).unwrap();

        let mut published = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            match ingest_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(IngestEvent::Published { .. }) => {
                    published += 1;
                    break;
                }
                Ok(IngestEvent::Failed { reason, .. }) => panic!("{reason}"),
                _ => {}
            }
        }
        assert_eq!(published, 1, "the file that landed after start was loaded");
        sched.shutdown();
    }

    #[test]
    fn an_unchanged_directory_submits_nothing_on_later_polls() {
        let (_db, _dir, ingest, ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(20), Duration::from_secs(3600));
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, Arc::clone(&ingest), sink);
        let mut polls = 0;
        while polls < 5 {
            if let Ok(SchedulerEvent::Polled { ready, .. }) =
                sched_rx.recv_timeout(Duration::from_secs(10))
            {
                assert_eq!(ready, 0);
                polls += 1;
            }
        }
        sched.shutdown();
        // The runner announced idle once at most and published nothing.
        while let Ok(e) = ingest_rx.try_recv() {
            assert!(matches!(e, IngestEvent::PlanComplete), "{e:?}");
        }
    }

    #[test]
    fn a_csv_pending_past_its_timeout_is_a_health_event() {
        let (_db, dir, ingest, _ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(50), Duration::ZERO);
        std::fs::write(dir.path().join("risk_2026-09-03_BK000.csv"), "Book\nBK000\n").unwrap();
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, ingest, sink);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = None;
        while std::time::Instant::now() < deadline {
            if let Ok(SchedulerEvent::Health { worst, detail, .. }) =
                sched_rx.recv_timeout(Duration::from_secs(1))
            {
                seen = Some((worst, detail));
                break;
            }
        }
        let (worst, detail) = seen.expect("a health event");
        assert_eq!(worst, Health::PendingTooLong);
        assert!(detail.contains("BK000"), "{detail}");
        sched.shutdown();
    }

    #[test]
    fn no_sources_means_the_thread_exits_and_shutdown_does_not_hang() {
        let (_db, _dir, ingest, _rx, conn, _spec, _ds) =
            harness(Duration::from_secs(1), Duration::from_secs(1));
        let (sink, _sched_rx) = events_sink();
        let sched = Scheduler::spawn(Vec::new(), conn, ingest, sink);
        sched.shutdown();
        sched.shutdown();
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data scheduler:: 2>&1 | tail -3`
Expected: compile error or `todo!()` panics.

- [ ] **Step 3: Implement the scheduler**

```rust
//! Discovery on a schedule (Phase 3 spec §5.3, foundation §5.1). One
//! thread walks every configured source on its own interval, builds a
//! plan from what is ready, and hands it to the ingest runner. Polling,
//! never watching: `notify` is unreliable over SMB (§11).
//!
//! Every source is polled once immediately at start, so cold start is
//! the same code path as the thirtieth poll, and discovery I/O happens
//! here where nothing waits on it.

use crate::health::Health;
use crate::ingest::IngestHandle;
use crate::ingest::plan::build_plan;
use crate::source::{CandidateState, SourceSpec, discover};
use crate::store::Catalog;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerEvent {
    /// One poll finished; `ready` is how many files were handed to the
    /// runner. Tests wait on this; the service ignores it.
    Polled { source: String, ready: usize },
    /// The worst thing discovery found. Never modal, never fatal.
    Health {
        source: String,
        worst: Health,
        detail: String,
    },
}

pub type SchedulerSink = Arc<dyn Fn(SchedulerEvent) -> bool + Send + Sync>;

pub struct Scheduler {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Scheduler {
    pub fn spawn(
        sources: Vec<SourceSpec>,
        conn: duckdb::Connection,
        ingest: Arc<IngestHandle>,
        sink: SchedulerSink,
    ) -> Scheduler {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("geode-discovery".into())
            .spawn(move || run(sources, conn, ingest, sink, worker_stop))
            .expect("spawning the discovery thread");
        Scheduler {
            stop,
            thread: Mutex::new(Some(thread)),
        }
    }

    pub fn shutdown(&self) {
        {
            let (lock, cvar) = &*self.stop;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cvar.notify_all();
        }
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Sleep until `until` or until told to stop. `true` means stop.
fn wait_until(stop: &(Mutex<bool>, Condvar), until: Instant) -> bool {
    let (lock, cvar) = stop;
    let mut stopped = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if *stopped {
            return true;
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        let (guard, _) = cvar
            .wait_timeout(stopped, until - now)
            .unwrap_or_else(|e| e.into_inner());
        stopped = guard;
    }
}

fn run(
    sources: Vec<SourceSpec>,
    conn: duckdb::Connection,
    ingest: Arc<IngestHandle>,
    sink: SchedulerSink,
    stop: Arc<(Mutex<bool>, Condvar)>,
) {
    if sources.is_empty() {
        return;
    }
    // Everything is due now: the first sweep is the cold start.
    let mut due: Vec<(Instant, usize)> = (0..sources.len())
        .map(|i| (Instant::now(), i))
        .collect();

    loop {
        due.sort_by_key(|(t, _)| *t);
        let (when, i) = due[0];
        if wait_until(&stop, when) {
            return;
        }
        let spec = &sources[i];

        // Discovery is a panic boundary too (spec §5.7): a bad glob or a
        // share that hangs must degrade this source, not stop polling
        // every other one.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            discover(spec, &Catalog::new(&conn), SystemTime::now())
        }));

        let delivered = match outcome {
            Ok(Ok(candidates)) => {
                let health = worst_health(&candidates);
                let ok = match health {
                    Some((worst, detail)) => sink(SchedulerEvent::Health {
                        source: spec.name.clone(),
                        worst,
                        detail,
                    }),
                    None => true,
                };
                let plan = build_plan(&[(spec.clone(), candidates)]);
                let ready = plan.items.len();
                if ready > 0 {
                    ingest.submit(plan);
                }
                ok && sink(SchedulerEvent::Polled {
                    source: spec.name.clone(),
                    ready,
                })
            }
            Ok(Err(e)) => sink(SchedulerEvent::Health {
                source: spec.name.clone(),
                worst: Health::Failed {
                    reason: e.to_string(),
                },
                detail: format!("discovery failed: {e}"),
            }),
            Err(_) => sink(SchedulerEvent::Health {
                source: spec.name.clone(),
                worst: Health::Failed {
                    reason: "discovery panicked".into(),
                },
                detail: "discovery panicked".into(),
            }),
        };
        if !delivered {
            return;
        }
        // Re-arm from *now*, not from `when`: a slow share must not make
        // the next poll immediately due and spin.
        due[0] = (Instant::now() + spec.poll_interval, i);
    }
}

/// The worst candidate state and a detail line naming the files in it.
fn worst_health(candidates: &[crate::source::Candidate]) -> Option<(Health, String)> {
    let mut worst: Option<(Health, Vec<String>)> = None;
    for c in candidates {
        let h = match &c.state {
            CandidateState::PendingTooLong => Health::PendingTooLong,
            CandidateState::Orphaned { reason } => Health::Degraded {
                reason: reason.clone(),
            },
            CandidateState::Ready(_) | CandidateState::Pending | CandidateState::Unchanged => {
                continue;
            }
        };
        let name = c
            .csv_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        match &mut worst {
            Some((w, names)) if *w == h => names.push(name),
            Some((w, _)) if *w > h => {}
            _ => worst = Some((h, vec![name])),
        }
    }
    worst.map(|(h, names)| {
        let detail = format!("{}: {}", h.label(), names.join(", "));
        (h, detail)
    })
}
```

`Health` derives `PartialOrd`/`Ord` by severity, so `*w > h` keeps the
worse one. `Health::Degraded { reason }` compares equal only with the
same reason; two orphans with different reasons yield the first — good
enough for a one-line status.

- [ ] **Step 4: Run the scheduler tests**

Run: `cargo test -p geode-data scheduler::`
Expected: 4 passed. (`a_file_that_appears…` takes a few seconds.)

- [ ] **Step 5: Wire the service (`open` steps 2–5 of spec §5.3)**

In `service.rs`, `DataService` becomes:

```rust
pub struct DataService {
    config: DataServiceConfig,
    diagnostics: Vec<Diagnostic>,
    /// Field order is drop order. The pool joins its workers first; the
    /// scheduler stops submitting; the runner drains and drops the
    /// `Store` last, which is what holds the database open for everyone
    /// above it (readers are `try_clone`s and share the handle).
    pool: QueryPool,
    scheduler: Scheduler,
    ingest: Arc<IngestHandle>,
    /// A dedicated read connection for compilation and catalog reads.
    conn: duckdb::Connection,
}
```

and `open`:

```rust
    pub fn open(config: DataServiceConfig, sink: EventSink) -> Result<DataService, StoreError> {
        let store = Store::open(&config.db_path)?;
        for ds in &config.schema.datasets {
            store.apply_schema(ds)?;
        }
        Catalog::new(store.writer()).ensure_tables()?;

        // Every reader the service will ever need is cloned before the
        // store moves onto the ingest thread (Phase 3 §2.5).
        let conn = store.reader()?;
        let discovery_conn = store.reader()?;
        let result_sink: ResultSink = {
            let sink = Arc::clone(&sink);
            Arc::new(move |r: QueryResult| {
                sink(DataEvent::Query(QueryOutcome {
                    key: r.key,
                    tag: r.tag,
                    snapshot: r.snapshot.map(Arc::new),
                    submitted: r.submitted,
                }))
            })
        };
        let pool = QueryPool::spawn_with_sink(&store, config.query_workers.max(1), result_sink)?;

        let ingest_sink: IngestSink = {
            let sink = Arc::clone(&sink);
            Arc::new(move |e: IngestEvent| match e {
                IngestEvent::Published {
                    dataset,
                    batch,
                    gen_id,
                    books,
                    ..
                } => sink(DataEvent::Published {
                    dataset,
                    batch,
                    gen_id,
                    books,
                }),
                IngestEvent::Failed {
                    dataset,
                    batch,
                    reason,
                } => sink(DataEvent::Health {
                    source: dataset,
                    worst: Health::Failed {
                        reason: reason.clone(),
                    },
                    detail: format!("{batch}: {reason}"),
                }),
                IngestEvent::PlanComplete => true,
            })
        };
        let ingest = Arc::new(IngestRunner::spawn(store, config.schema.clone(), ingest_sink));

        let scheduler_sink: SchedulerSink = {
            let sink = Arc::clone(&sink);
            Arc::new(move |e: SchedulerEvent| match e {
                SchedulerEvent::Polled { .. } => true,
                SchedulerEvent::Health {
                    source,
                    worst,
                    detail,
                } => sink(DataEvent::Health {
                    source,
                    worst,
                    detail,
                }),
            })
        };
        let scheduler = Scheduler::spawn(
            config.sources.clone(),
            discovery_conn,
            Arc::clone(&ingest),
            scheduler_sink,
        );

        let diagnostics = config
            .views
            .iter()
            .flat_map(|v| v.validate(&config.schema, &config.dimensions))
            .collect();
        Ok(DataService {
            config,
            diagnostics,
            pool,
            scheduler,
            ingest,
            conn,
        })
    }

    pub fn shutdown(&self) {
        self.pool.shutdown();
        self.scheduler.shutdown();
        self.ingest.shutdown();
    }
```

with `use crate::ingest::scheduler::{Scheduler, SchedulerEvent, SchedulerSink};
use crate::ingest::{IngestEvent, IngestHandle, IngestRunner, IngestSink};`.
Delete the `sink` field and its `#[allow(dead_code)]`.

The test `a_historical_result_is_labelled_with_the_data_it_actually_read`
wrote through `svc._store.writer()`. The store is on the runner now.
Give it a writer of its own: `let writer = svc.conn.try_clone().unwrap();`
and run the same `execute_batch` on it — a `try_clone` of a reader is a
full connection on the same database, which is what the test needs; the
"separate database instance" trap the test's comment warns about is
`Connection::open` on the path, not `try_clone`. Update the comment to
say so.

Add a service-level test that goes end to end through `sources`:

```rust
    #[test]
    fn a_configured_source_is_discovered_loaded_and_announced() {
        // Cold start through the real door: a service opened over an
        // empty database with one source, and nothing else.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 500,
            seed: 3,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(src.path());
        opts.leave_one_pending = false;
        let emitted = geode_demo_data::emit_directory(&batch, &opts).unwrap();

        let (svc, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![crate::ingest::load::tests_support::tree_view()],
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                name: "risk".into(),
                dataset: "risk_snapshot".into(),
                paths: vec![format!("{}/*.csv", src.path().display())],
                readiness: crate::source::Readiness::Sentinel,
                priority: crate::source::Priority::LatestRisk,
                poll_interval: Duration::from_secs(3600),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            }],
        })
        .unwrap();

        let mut published = 0;
        let deadline = Instant::now() + Duration::from_secs(120);
        while published < emitted.files.len() && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(DataEvent::Published { dataset, .. }) => {
                    assert_eq!(dataset, "risk_snapshot");
                    published += 1;
                }
                Ok(DataEvent::Health { detail, .. }) => panic!("{detail}"),
                _ => {}
            }
        }
        assert_eq!(published, emitted.files.len());

        svc.query(&params(1, "tree", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        assert!(next(&rx).snapshot.unwrap().rows() > 1, "data is queryable");
        svc.shutdown();
    }
```

- [ ] **Step 6: Run**

Run: `cargo test -p geode-data && cargo build -p geode-app`
Expected: green; `probe.rs` still compiles because its `ingest()` still
runs *before* the service opens — it now double-ingests harmlessly
(`Unchanged` on the second discovery). Task 8 removes it.

- [ ] **Step 7: Full check and commit**

```bash
git add crates/geode-data
git commit -m "feat(data): discovery scheduler inside DataService

The service owns one ingest runner and a discovery thread that polls
each source on its interval and submits what is ready; the Store moves
onto the runner and the service keeps reader connections. Publishes and
health leave through the event sink (Phase 3 §5.3).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 8: Harness entries**

```sh
# ---- discovery scheduler (Phase 3 §5.3)

run_mutation "scheduler: every source is polled immediately at start" \
  crates/geode-data/src/ingest/scheduler.rs \
  '        .map(|i| (Instant::now(), i))' \
  '        .map(|i| (Instant::now() + Duration::from_secs(3600), i))'

run_mutation "scheduler: the poll re-arms" \
  crates/geode-data/src/ingest/scheduler.rs \
  '        due[0] = (Instant::now() + spec.poll_interval, i);' \
  '        due[0] = (Instant::now() + Duration::from_secs(3600), i);'

run_mutation "scheduler: ready files reach the runner" \
  crates/geode-data/src/ingest/scheduler.rs \
  '                if ready > 0 {
                    ingest.submit(plan);
                }' \
  '                let _ = plan;'

run_mutation "scheduler: pending-too-long surfaces as health" \
  crates/geode-data/src/ingest/scheduler.rs \
  '            CandidateState::PendingTooLong => Health::PendingTooLong,' \
  '            CandidateState::PendingTooLong => continue,'

run_mutation "service: a publish becomes a Published event" \
  crates/geode-data/src/service.rs \
  '                } => sink(DataEvent::Published {
                    dataset,
                    batch,
                    gen_id,
                    books,
                }),' \
  '                } => {
                    let _ = (dataset, batch, gen_id, books);
                    true
                }'
```

Run `zsh scripts/mutation-check.sh "scheduler:"` and `"service:"` — all
`caught`. Commit.

---

### Task 7: `DataHandle` and the service thread

Spec §5.1. The one door modules get: `Clone + Send + Sync`, never blocks,
opens the service on its own thread.

**Files:**
- Create: `crates/geode-data/src/handle.rs`
- Modify: `crates/geode-data/src/lib.rs` (`pub mod handle;` and
  `pub use handle::{DataHandle, Request};`)
- Modify: `crates/geode-data/Cargo.toml` (add `[features] test-support = []`)
- Test: `crates/geode-data/src/handle.rs` (inline)

**Interfaces:**
- Produces:
  ```rust
  pub enum Request {
      Query(QueryParams),
      Cancel { key: QueryKey },
      ReplaceViews { views: Vec<ViewSpec>, dimensions: DerivedDimensions },
      Shutdown,
  }
  #[derive(Clone)] pub struct DataHandle { … }
  impl DataHandle {
      pub fn query(&self, params: QueryParams) -> bool;
      pub fn cancel(&self, key: QueryKey) -> bool;
      pub fn replace_views(&self, views: Vec<ViewSpec>, dimensions: DerivedDimensions) -> bool;
      pub fn dropped_requests(&self) -> u64;
      pub fn shutdown(&self);
      #[cfg(any(test, feature = "test-support"))]
      pub fn for_tests() -> (DataHandle, Receiver<Request>);
  }
  impl DataService { pub fn spawn(config: DataServiceConfig, sink: EventSink) -> DataHandle; }
  ```
  Every method returns `false` when the request could not be queued — the
  channel is full or the service thread is gone — and the caller retries
  on its next trigger; a dropped request is counted.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::scope::Scope;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    fn params(key: u64, view: &str) -> QueryParams {
        QueryParams {
            key: QueryKey(key),
            tag: 1,
            submitted: Instant::now(),
            view: view.to_string(),
            scope: Scope::default(),
            as_of: AsOf::Live,
            max_depth: 1,
        }
    }

    #[test]
    fn a_test_handle_hands_requests_to_the_test() {
        let (h, rx) = DataHandle::for_tests();
        assert!(h.query(params(5, "tree")));
        assert!(h.cancel(QueryKey(5)));
        match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
            Request::Query(p) => assert_eq!(p.key, QueryKey(5)),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Request::Cancel { key: QueryKey(5) }
        ));
    }

    #[test]
    fn a_full_channel_refuses_and_counts_rather_than_blocking() {
        // §7.3: backpressure never stalls the UI. The bound is small on
        // purpose (REQUEST_BOUND); the test fills it without draining.
        let (h, _rx) = DataHandle::for_tests();
        let mut accepted = 0;
        for i in 0..(REQUEST_BOUND as u64 + 5) {
            if h.query(params(i, "tree")) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, REQUEST_BOUND);
        assert_eq!(h.dropped_requests(), 5);
    }

    #[test]
    fn a_gone_service_thread_refuses_every_request() {
        let (h, rx) = DataHandle::for_tests();
        drop(rx);
        assert!(!h.query(params(1, "tree")));
        assert!(!h.cancel(QueryKey(1)));
        assert_eq!(h.dropped_requests(), 2);
    }

    #[test]
    fn the_real_service_answers_through_the_sink_and_reports_open_failures() {
        // A real database on a real thread: a query outcome arrives keyed
        // and tagged; an unknown view arrives as an Err outcome for that
        // key, not a lost request.
        let (db, _src, store, ds, emitted) = crate::ingest::load::tests_support::fixture();
        for file in emitted.files.iter().filter(|f| f.sentinel_path.is_some()) {
            let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
            let sentinel = crate::source::parse_sentinel(&text).unwrap();
            let batch = crate::ingest::load::tests_support::batch_of(&file.csv_path);
            let _ = crate::ingest::load_file(
                &store,
                &crate::ingest::LoadRequest {
                    dataset: &ds,
                    dataset_name: "risk_snapshot",
                    csv_path: &file.csv_path,
                    sentinel: &sentinel,
                    batch: &batch,
                },
            );
        }
        drop(store);
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(ds);

        let (tx, rx) = channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let h = DataService::spawn(
            DataServiceConfig {
                db_path: db.path().join("geode.duckdb"),
                schema,
                views: vec![crate::ingest::load::tests_support::tree_view()],
                dimensions: geode_core::dimensions::DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
            },
            sink,
        );
        assert!(h.query(params(9, "tree")));
        assert!(h.query(params(10, "nonesuch")));
        let mut got = std::collections::BTreeMap::new();
        while got.len() < 2 {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Query(o) => {
                    got.insert(o.key, o.snapshot.is_ok());
                }
                _ => {}
            }
        }
        assert_eq!(got.get(&QueryKey(9)), Some(&true));
        assert_eq!(got.get(&QueryKey(10)), Some(&false), "unknown view is an Err outcome");
        h.shutdown();
        assert!(!h.query(params(11, "tree")), "after shutdown nothing is accepted");
    }

    #[test]
    fn an_unopenable_database_is_a_diagnostic_not_a_panic() {
        let (tx, rx) = channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let h = DataService::spawn(
            DataServiceConfig {
                // A directory, not a file: DuckDB cannot open it.
                db_path: std::env::temp_dir(),
                schema: geode_core::schema::SchemaSpec::default(),
                views: Vec::new(),
                dimensions: geode_core::dimensions::DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
            },
            sink,
        );
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            DataEvent::Diagnostics(d) => {
                assert!(d.iter().any(|d| d.message.contains("open")), "{d:?}")
            }
            other => panic!("{other:?}"),
        }
        // The thread is gone; the handle says so.
        for _ in 0..200 {
            if !h.query(params(1, "tree")) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("requests were still accepted after the service failed to open");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data handle:: 2>&1 | tail -3`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! The door modules get (Phase 3 spec §5.1). `DataService` owns a DuckDB
//! connection and is not `Sync`, so it lives on one thread; this is the
//! `Clone + Send + Sync` handle to it. Nothing here blocks: every method
//! is a `try_send`, and a request that cannot be queued is refused and
//! counted rather than waited on (§7.3 — backpressure never stalls the
//! UI).

use crate::service::{DataEvent, DataService, DataServiceConfig, EventSink, QueryParams};
use geode_core::config::{Diagnostic, Severity};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{QueryKey, QueryOutcome};
use geode_core::view::ViewSpec;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Requests queued before the handle refuses. One in-flight query per
/// tile bounds the steady state at tile count; the headroom is for a
/// burst of frame changes landing before the service thread wakes.
pub const REQUEST_BOUND: usize = 64;

#[derive(Debug)]
pub enum Request {
    Query(QueryParams),
    Cancel { key: QueryKey },
    ReplaceViews {
        views: Vec<ViewSpec>,
        dimensions: DerivedDimensions,
    },
    Shutdown,
}

struct Inner {
    tx: SyncSender<Request>,
    thread: Mutex<Option<JoinHandle<()>>>,
    dropped: AtomicU64,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.tx.try_send(Request::Shutdown);
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

#[derive(Clone)]
pub struct DataHandle {
    inner: Arc<Inner>,
}

impl DataHandle {
    fn send(&self, req: Request) -> bool {
        match self.inner.tx.try_send(req) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Queue a query. `false` means it was not queued — retry on the next
    /// trigger; the result, when it comes, arrives on the sink keyed and
    /// tagged as asked.
    pub fn query(&self, params: QueryParams) -> bool {
        self.send(Request::Query(params))
    }

    pub fn cancel(&self, key: QueryKey) -> bool {
        self.send(Request::Cancel { key })
    }

    /// The safe hot-reload path for views (foundation §8). Diagnostics
    /// come back on the sink.
    pub fn replace_views(&self, views: Vec<ViewSpec>, dimensions: DerivedDimensions) -> bool {
        self.send(Request::ReplaceViews { views, dimensions })
    }

    /// Requests refused so far. A diagnostic, not a UI condition.
    pub fn dropped_requests(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Stop the service thread and wait for it. Idempotent; also runs
    /// when the last handle drops.
    pub fn shutdown(&self) {
        let _ = self.inner.tx.try_send(Request::Shutdown);
        if let Some(t) = self
            .inner
            .thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = t.join();
        }
    }

    /// A handle with no service behind it: the test is the service, and
    /// reads what a module asked for.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests() -> (DataHandle, Receiver<Request>) {
        let (tx, rx) = sync_channel(REQUEST_BOUND);
        (
            DataHandle {
                inner: Arc::new(Inner {
                    tx,
                    thread: Mutex::new(None),
                    dropped: AtomicU64::new(0),
                }),
            },
            rx,
        )
    }
}

impl DataService {
    /// Open the service on its own thread and return the handle at once.
    /// Opening — DuckDB, schema, catalog — happens on that thread, so the
    /// caller (the UI) never waits on it; a failure to open arrives on
    /// the sink as a diagnostic and the handle then refuses everything.
    pub fn spawn(config: DataServiceConfig, sink: EventSink) -> DataHandle {
        let (tx, rx) = sync_channel(REQUEST_BOUND);
        let thread = std::thread::Builder::new()
            .name("geode-data".into())
            .spawn(move || serve(config, sink, rx))
            .expect("spawning the data service thread");
        DataHandle {
            inner: Arc::new(Inner {
                tx,
                thread: Mutex::new(Some(thread)),
                dropped: AtomicU64::new(0),
            }),
        }
    }
}

fn serve(config: DataServiceConfig, sink: EventSink, rx: Receiver<Request>) {
    let mut service = match DataService::open(config, Arc::clone(&sink)) {
        Ok(s) => s,
        Err(e) => {
            sink(DataEvent::Diagnostics(vec![Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("data service failed to open: {e}"),
            }]));
            return;
        }
    };
    if !service.diagnostics().is_empty() {
        sink(DataEvent::Diagnostics(service.diagnostics().to_vec()));
    }

    while let Ok(req) = rx.recv() {
        match req {
            Request::Query(params) => {
                if let Err(e) = service.query(&params) {
                    // A compile-time failure — unknown view, bad scope —
                    // is this key's outcome, not a lost request (§10.1).
                    sink(DataEvent::Query(QueryOutcome {
                        key: params.key,
                        tag: params.tag,
                        snapshot: Err(e.to_string()),
                        submitted: params.submitted,
                    }));
                }
            }
            Request::Cancel { key } => service.cancel(key),
            Request::ReplaceViews { views, dimensions } => {
                let diags = service.replace_views(views, dimensions);
                if !diags.is_empty() {
                    sink(DataEvent::Diagnostics(diags));
                }
            }
            Request::Shutdown => break,
        }
    }
    service.shutdown();
}
```

`crates/geode-data/Cargo.toml`:

```toml
[features]
test-support = []
```

`lib.rs`:

```rust
pub mod handle;
pub mod health;
pub mod ingest;
pub mod query;
pub mod service;
pub mod source;
pub mod store;

pub use handle::{DataHandle, REQUEST_BOUND, Request};
pub use service::{DataEvent, DataService, DataServiceConfig, EventSink, QueryParams};
```

- [ ] **Step 4: Run**

Run: `cargo test -p geode-data handle::`
Expected: 5 passed.

- [ ] **Step 5: Full check and commit**

```bash
git add crates/geode-data
git commit -m "feat(data): DataHandle — the thread-safe door to DataService

Clone + Send + Sync over a bounded request channel to a thread that
owns the service; never blocks, counts what it refuses, reports an
open failure as a diagnostic on the sink (Phase 3 §5.1).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Harness entries**

```sh
# ---- data handle (Phase 3 §5.1)

run_mutation "handle: a refused request is counted" \
  crates/geode-data/src/handle.rs \
  '                self.inner.dropped.fetch_add(1, Ordering::Relaxed);' \
  '                let _ = Ordering::Relaxed;'

run_mutation "handle: a compile failure is delivered as the key's outcome" \
  crates/geode-data/src/handle.rs \
  '                if let Err(e) = service.query(&params) {' \
  '                if let Err(e) = service.query(&params) && false {'
```

Run: `zsh scripts/mutation-check.sh "handle:"` — both `caught`. Commit.

---

### Task 8: The probe on `DataHandle` and `sources`

Spec §9 step 1: the probe is the handle's first consumer and keeps
running. Its one-shot ingest and its own service thread go; `sources`
comes from `sources.toml` when present, else from `GEODE_PROBE_DIR` as
before. This is still throwaway code and is deleted in Plan 3c.

**Files:**
- Modify: `crates/geode-app/src/probe.rs` (rewrite the data half)
- Modify: `crates/geode-shell/src/dataprobe.rs:44-54` (`ProbeState`
  freshness now comes from `Provenance`)
- Test: `crates/geode-shell/src/dataprobe.rs` (existing tests still pass)

**Interfaces:**
- Consumes: `DataService::spawn`, `DataHandle::query`, `DataEvent`,
  `SourceSpec::from_doc`, `QueryParams`.

- [ ] **Step 1: Rewrite `probe.rs`'s module doc paragraphs 3–4 and the
  data half**

Replace the "Why a thread rather than a background task" paragraph with:

```rust
//! **The probe is `DataHandle`'s first consumer** (Phase 3 §9 step 1).
//! The service lives on its own thread behind the handle; this file only
//! submits queries, drains the event sink on the UI side, and pushes
//! readings into the tile. Ingest is the service's scheduler's job now —
//! the probe no longer ingests anything itself.
```

Replace `Setup`, `setup`, `ingest`, `source_spec`, `query_once`, `run`,
`start` and `drain` with:

```rust
use geode_data::source::SourceSpec;
use geode_data::{DataEvent, DataService, DataServiceConfig, EventSink, QueryParams};
use geode_core::query::{AsOf, QueryKey};

#[derive(Clone)]
struct Setup {
    schema: SchemaSpec,
    views: Vec<ViewSpec>,
    dimensions: DerivedDimensions,
    sources: Vec<SourceSpec>,
    view: String,
    db_path: PathBuf,
}

fn setup(config: &Config) -> Option<Setup> {
    let source_dir = std::env::var_os("GEODE_PROBE_DIR").map(PathBuf::from);
    let has_sources_doc = config.doc("sources").is_some();
    if source_dir.is_none() && !has_sources_doc {
        return None;
    }
    let missing: Vec<&str> = ["datasets", "views"]
        .into_iter()
        .filter(|doc| config.doc(doc).is_none())
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "[probe] the probe is enabled but the config has no {}; it will stay idle",
            missing.join(" or ")
        );
        return None;
    }
    let (schema, schema_warnings) = SchemaSpec::from_doc(config.doc("datasets")?);
    let (views, view_warnings) = ViewSpec::from_doc(config.doc("views")?);
    let (dimensions, dimension_warnings) = match config.doc("dimensions") {
        Some(doc) => DerivedDimensions::from_doc(doc),
        None => (DerivedDimensions::default(), Vec::new()),
    };
    for warning in schema_warnings
        .iter()
        .chain(view_warnings.iter())
        .chain(dimension_warnings.iter())
    {
        eprintln!("[probe] warning: {warning}");
    }
    let Some(view) = views.first() else {
        eprintln!("[probe] views.toml declares no views, so there is nothing to query");
        return None;
    };
    let dataset = view.dataset.clone();
    if schema.dataset(&dataset).is_none() {
        eprintln!("[probe] view '{}' names undeclared dataset '{dataset}'", view.name);
        return None;
    }

    // sources.toml when present; otherwise the environment variable
    // builds one source over the directory, as it always did.
    let sources = match config.doc("sources") {
        Some(doc) => {
            let (sources, diags) = SourceSpec::from_doc(doc, &schema);
            for d in &diags {
                eprintln!("[probe] sources: {d}");
            }
            sources
        }
        None => vec![SourceSpec {
            name: "probe".into(),
            dataset: dataset.clone(),
            paths: vec![format!("{}/*.csv", source_dir?.display())],
            readiness: Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: REQUERY_INTERVAL,
            pending_timeout: Duration::from_secs(60),
            batch_pattern: None,
        }],
    };

    Some(Setup {
        schema,
        views,
        dimensions,
        sources,
        view: view.name.clone(),
        db_path: std::env::temp_dir().join("geode-probe.duckdb"),
    })
}

/// The probe's resolved inputs. Opaque: the binary only carries one from
/// [`prepare`] to [`start`].
pub struct Prepared(Setup);

pub fn prepare(config: &Config) -> Option<Prepared> {
    setup(config).map(Prepared)
}

/// Start the probe against an open shell. Returns immediately: the
/// service opens on its own thread behind the handle.
pub fn start(prepared: Prepared, shell: Entity<ShellView>, cx: &mut App) {
    let setup = prepared.0;
    let (tx, rx) = channel::<DataEvent>();
    let sink: EventSink = std::sync::Arc::new(move |e| tx.send(e).is_ok());
    let handle = DataService::spawn(
        DataServiceConfig {
            db_path: setup.db_path.clone(),
            schema: setup.schema.clone(),
            views: setup.views.clone(),
            dimensions: setup.dimensions.clone(),
            query_workers: 2,
            sources: setup.sources.clone(),
        },
        sink,
    );
    drain(rx, handle, setup.view, shell, cx);
}

/// Move events onto the UI thread and requery on a timer and on every
/// publish. Nothing here blocks; `cx.notify()` only when something
/// arrived. Still the 250 ms poll the probe always had — the blotter's
/// bridge wakes on the channel instead (Phase 3 §5.1) and this file is
/// deleted with the probe.
fn drain(
    rx: Receiver<DataEvent>,
    handle: geode_data::DataHandle,
    view: String,
    shell: Entity<ShellView>,
    cx: &mut App,
) {
    let shell = shell.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut tag = 0u64;
        let mut last_query = Instant::now() - REQUERY_INTERVAL;
        let mut submitted_at: Option<Instant> = None;
        loop {
            let mut requery = last_query.elapsed() >= REQUERY_INTERVAL;
            let mut reading: Option<Reading> = None;
            loop {
                match rx.try_recv() {
                    Ok(DataEvent::Query(o)) => {
                        if o.tag != tag {
                            continue; // stale
                        }
                        let query_micros = submitted_at
                            .map(|t| t.elapsed().as_micros() as u64)
                            .unwrap_or(0);
                        reading = Some(match o.snapshot {
                            Ok(snapshot) => Reading {
                                freshness: snapshot
                                    .provenance()
                                    .datasets
                                    .iter()
                                    .map(|f| {
                                        (
                                            f.dataset.clone(),
                                            f.as_of.clone().unwrap_or_else(|| "—".into()),
                                            f.generation,
                                        )
                                    })
                                    .collect(),
                                snapshot: Some(snapshot),
                                query_micros,
                                error: None,
                            },
                            Err(e) => Reading {
                                snapshot: None,
                                freshness: Vec::new(),
                                query_micros,
                                error: Some(e),
                            },
                        });
                    }
                    Ok(DataEvent::Published { dataset, batch, gen_id, .. }) => {
                        eprintln!("[probe] published {dataset}/{batch} gen {gen_id}");
                        requery = true;
                    }
                    Ok(DataEvent::Health { source, worst, detail }) => {
                        eprintln!("[probe] health {source}: {} — {detail}", worst.label());
                    }
                    Ok(DataEvent::Diagnostics(diags)) => {
                        for d in diags {
                            eprintln!("[probe] {d}");
                        }
                    }
                    Err(TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => break,
                }
            }
            if requery {
                tag += 1;
                last_query = Instant::now();
                submitted_at = Some(last_query);
                if !handle.query(QueryParams {
                    key: QueryKey(1),
                    tag,
                    submitted: last_query,
                    view: view.clone(),
                    scope: Scope::default(),
                    as_of: AsOf::Live,
                    max_depth: MAX_DEPTH,
                }) {
                    eprintln!("[probe] query refused ({} dropped so far)", handle.dropped_requests());
                }
            }
            if let Some(reading) = reading {
                match (&reading.snapshot, &reading.error) {
                    (_, Some(error)) => eprintln!("[probe] {view}: {error}"),
                    (Some(snapshot), None) => eprintln!(
                        "[probe] {view}: {} rows in {:.1} ms",
                        snapshot.rows(),
                        reading.query_micros as f64 / 1000.0
                    ),
                    (None, None) => {}
                }
                let pushed = shell.update(cx, |shell, cx| {
                    shell.set_probe(
                        ProbeState {
                            snapshot: reading.snapshot,
                            freshness: reading.freshness,
                            query_micros: reading.query_micros,
                            error: reading.error,
                        },
                        cx,
                    );
                });
                if pushed.is_err() {
                    return; // the window is gone
                }
            }
            cx.background_executor().timer(DRAIN_INTERVAL).await;
        }
    })
    .detach();
}
```

`Reading.snapshot` becomes `Option<Arc<Snapshot>>` (the outcome already
carries an `Arc`); `ProbeState.snapshot` is already `Option<Arc<Snapshot>>`.
Remove the now-unused imports (`IngestEvent`, `IngestRunner`,
`build_plan`, `discover`, `Store`, `Catalog`, `SystemTime`, `Instant`
stays) and the `ingest`/`source_spec`/`query_once`/`run` functions.

The `[probe]` freshness line previously printed per-book; it now prints
per-dataset from `Provenance`, and its `generation` is the dataset's
latest, which is what `Freshness` already carries.

- [ ] **Step 2: Run the probe end to end**

```sh
cargo run -p geode-demo-data --example emit -- /tmp/geode-probe 100000
rm -f "$TMPDIR/geode-probe.duckdb"
GEODE_PROBE_DIR=/tmp/geode-probe GEODE_DESK_CONFIG=examples/probe-config cargo run -p geode-app
```

Expected on stderr: `[probe] published risk_snapshot/… gen N` lines as
the scheduler loads the directory, then `[probe] tree: N rows in X ms`.
Toggle `mod+shift+d`: the tile paints rows. Then, with the app still
running:

```sh
cargo run -p geode-demo-data --example emit -- /tmp/geode-probe-2 20000
cp /tmp/geode-probe-2/*BK005* /tmp/geode-probe/
```

Expected: within 5 s a new `published` line and the tile's row count
changes without any keypress — spec §2.8's done state, met.

- [ ] **Step 3: Full check and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`

```bash
git add crates/geode-app/src/probe.rs crates/geode-shell/src/dataprobe.rs
git commit -m "feat(app): the probe rides DataHandle and the scheduler

The probe no longer ingests or owns a service thread: it submits keyed
queries through the handle, drains the event sink, and requeries on
every publish. sources.toml is honoured when present; GEODE_PROBE_DIR
still builds one source. A file landing after start now updates the
tile (Phase 3 §2.8).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

---

### Task 9: Index-based `Snapshot` accessors and a checked column order

Spec §5.5. The blotter resolves every column once per snapshot and never
searches by name in a cell; the probe's five `index_of` searches per cell
are exactly what this removes.

**Files:**
- Modify: `crates/geode-core/src/snapshot.rs`
- Test: `crates/geode-core/src/snapshot.rs` (inline)

**Interfaces:**
- Produces, on `Snapshot`:
  - `pub fn column_index(&self, name: &str) -> Option<usize>`
  - `pub fn columns(&self) -> usize`
  - `pub fn meta_at(&self, idx: usize) -> Option<&ColumnMeta>`
  - `pub fn f64_at(&self, idx: usize, row: usize) -> Option<f64>`
  - `pub fn i64_at(&self, idx: usize, row: usize) -> Option<i64>`
  - `pub fn text_at(&self, idx: usize, row: usize) -> Option<&str>`
  - `pub fn display_at(&self, idx: usize, row: usize) -> Option<String>`
  - `pub fn dict_codes_at(&self, idx: usize) -> Option<(DictCodes<'_>, &StringArray)>`
  - `from_batches` returns `Err(ArrowError::SchemaError)` when `meta`'s
    names do not equal the batch's column names in order.
  - `depth_of_row` reads a depth column index cached at construction.

- [ ] **Step 1: Write the failing tests**

Add to `snapshot.rs`'s test module:

```rust
    #[test]
    fn index_accessors_agree_with_their_by_name_twins_under_every_type() {
        // The blotter resolves a column once and reads by index for the
        // rest of the snapshot's life (§5.5). Every typed accessor here
        // must answer exactly what its by-name twin answers, including
        // NULL, past-the-end, and the narrow-integer and dictionary
        // shapes DuckDB actually emits.
        let s = Snapshot::for_tests(
            vec![
                (dim("book"), TestColumn::Dict(vec![Some("BK000".into()), None])),
                (dim("lhu"), TestColumn::Str(vec![Some("L1"), None])),
                (dim("row_depth"), TestColumn::I32(vec![1, 0])),
                (dim("delta01"), TestColumn::F64(vec![Some(1.5), None])),
            ],
            2,
        );
        assert_eq!(s.columns(), 4);
        assert_eq!(s.column_index("book"), Some(0));
        assert_eq!(s.column_index("delta01"), Some(3));
        assert_eq!(s.column_index("nonesuch"), None);
        assert_eq!(s.meta_at(3).map(|m| m.name.as_str()), Some("delta01"));
        assert!(s.meta_at(4).is_none());

        for row in 0..3 {
            assert_eq!(s.text_at(0, row), s.text_value("book", row), "book row {row}");
            assert_eq!(s.text_at(1, row), s.text_value("lhu", row), "lhu row {row}");
            assert_eq!(s.i64_at(2, row), s.i64_value("row_depth", row), "depth row {row}");
            assert_eq!(s.f64_at(3, row), s.f64_value("delta01", row), "delta row {row}");
            assert_eq!(
                s.display_at(0, row),
                s.display_value("book", row),
                "display row {row}"
            );
        }
        assert_eq!(s.f64_at(3, 1), None, "NULL is still NULL by index");
        assert_eq!(s.f64_at(9, 0), None, "an index past the end is None, not a panic");
        assert!(s.dict_codes_at(0).is_some());
        assert!(s.dict_codes_at(1).is_none(), "a plain string column has no codes");
    }

    #[test]
    fn a_meta_list_that_disagrees_with_the_batch_is_refused() {
        // Index accessors assume meta[i] describes batch column i. The
        // compiler keeps them aligned; a fixture or a future refactor
        // that does not must fail here, loudly, not read the wrong
        // attribution for every cell.
        let mut wrong = meta();
        wrong.swap(0, 2);
        let err = Snapshot::from_batches(batches(), wrong, vec!["book".into()], Provenance::default());
        assert!(err.is_err(), "misaligned meta must not build a snapshot");
    }

    #[test]
    fn depth_is_read_through_the_cached_column() {
        let s = snapshot();
        assert_eq!(s.depth_of_row(0), Some(1));
        assert_eq!(s.depth_of_row(1), Some(0));
        let flat = Snapshot::for_tests(
            vec![(dim("delta01"), TestColumn::F64(vec![Some(1.0)]))],
            0,
        );
        assert_eq!(flat.depth_of_row(0), None, "no depth column, no depth");
    }
```

Note: `from_batches` now takes `grouping: Vec<String>` (this task changes
the signature ahead of Task 10 so the change lands once). Update every
`Snapshot::from_batches(batches(), meta(), 1, …)` in this test module to
`Snapshot::from_batches(batches(), meta(), vec!["book".into()], …)`, and
the `dim("row_depth"), dim("wide")` test to `vec!["wide".into(), "x".into()]`
(two grouping names, matching its former `2`). `pool.rs`'s `run_one`
passes `req.grouping.clone()`; its `read_one` and `a_summed_bigint…`
tests pass `vec!["underlying_ref".into()]` / `vec!["qty".into()]`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core snapshot:: 2>&1 | grep -E "^error" | head`
Expected: missing methods.

- [ ] **Step 3: Implement**

`Snapshot` gains fields and the constructor checks alignment:

```rust
#[derive(Debug)]
pub struct Snapshot {
    batch: Option<RecordBatch>,
    meta: Vec<ColumnMeta>,
    /// The grouping columns in order; each prefix is one tree level.
    grouping: Vec<String>,
    /// Index of `row_depth`, resolved once. `None` for a flat result.
    depth_col: Option<usize>,
    provenance: Provenance,
}

impl Snapshot {
    pub fn from_batches(
        batches: Vec<RecordBatch>,
        meta: Vec<ColumnMeta>,
        grouping: Vec<String>,
        provenance: Provenance,
    ) -> Result<Snapshot, arrow::error::ArrowError> {
        let batch = match batches.first() {
            None => None,
            Some(_) => Some(concat_preserving_dictionaries(&batches)?),
        };
        // `meta[i]` must describe batch column `i`: the index accessors
        // rely on it, and the by-name ones are implemented over them.
        if let Some(b) = &batch {
            let names: Vec<&str> = b.schema().fields().iter().map(|f| f.name().as_str()).collect();
            let described: Vec<&str> = meta.iter().map(|m| m.name.as_str()).collect();
            if names != described {
                return Err(arrow::error::ArrowError::SchemaError(format!(
                    "snapshot meta {described:?} does not match batch columns {names:?}"
                )));
            }
        }
        let depth_col = batch
            .as_ref()
            .and_then(|b| b.schema().index_of("row_depth").ok());
        Ok(Snapshot {
            batch,
            meta,
            grouping,
            depth_col,
            provenance,
        })
    }

    pub fn grouping(&self) -> &[String] {
        &self.grouping
    }

    pub fn grouping_len(&self) -> usize {
        self.grouping.len()
    }

    pub fn columns(&self) -> usize {
        self.meta.len()
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.meta.iter().position(|m| m.name == name)
    }

    pub fn meta_at(&self, idx: usize) -> Option<&ColumnMeta> {
        self.meta.get(idx)
    }

    fn column_at(&self, idx: usize) -> Option<&dyn Array> {
        let batch = self.batch.as_ref()?;
        (idx < batch.num_columns()).then(|| batch.column(idx).as_ref())
    }

    fn column(&self, name: &str) -> Option<&dyn Array> {
        self.column_at(self.column_index(name)?)
    }
```

Then split each typed accessor into a private `*_in(arr: &dyn Array,
row)` helper used by both the `_at` and the by-name form:

```rust
    pub fn f64_value(&self, name: &str, row: usize) -> Option<f64> {
        f64_in(self.column(name)?, row)
    }

    pub fn f64_at(&self, idx: usize, row: usize) -> Option<f64> {
        f64_in(self.column_at(idx)?, row)
    }
```

with `fn f64_in(arr: &dyn Array, row: usize) -> Option<f64>` holding the
existing body (Float64 → Decimal128 → integer widths → Float32),
`fn i64_in`, `fn text_in` (dictionary at three widths, then `StringArray`),
and `fn display_in` (text, then Boolean/Date32/Date64/Timestamp). Keep
every comment those bodies carry. `dict_codes_at(idx)` is `dict_column`
over `column_at`. `depth_of_row`:

```rust
    pub fn depth_of_row(&self, row: usize) -> Option<usize> {
        let depth = self.i64_at(self.depth_col?, row)?;
        usize::try_from(depth)
            .ok()
            .filter(|d| *d <= self.grouping.len())
    }
```

`for_tests(columns, grouping_len)` derives the names: `let grouping =
columns.iter().take(grouping_len).map(|(m, _)| m.name.clone()).collect();`
with a doc line: *"The first `grouping_len` columns are the grouping
columns, which is the order the compiler emits."*

- [ ] **Step 4: Run**

Run: `cargo test -p geode-core && cargo test -p geode-data pool:: && cargo test -p geode-shell dataprobe::`
Expected: green.

- [ ] **Step 5: Full check and commit**

```bash
git add crates/geode-core crates/geode-data/src/query/pool.rs
git commit -m "feat(core): index-based Snapshot accessors; meta must match the batch

column_index resolves once; f64_at/i64_at/text_at/display_at read by
index with the by-name accessors implemented over them. from_batches
refuses meta that does not describe the batch's columns in order, and
takes the grouping names it will build the tree from (Phase 3 §5.5).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Harness entries** (package `geode-core`)

```sh
# ---- snapshot index accessors (Phase 3 §5.5)

run_mutation "snapshot: f64_at honours the null bitmap" \
  crates/geode-core/src/snapshot.rs \
  '        return (row < values.len() && !values.is_null(row)).then(|| values.value(row));' \
  '        return (row < values.len()).then(|| values.value(row));' \
  geode-core

run_mutation "snapshot: misaligned meta is refused" \
  crates/geode-core/src/snapshot.rs \
  '            if names != described {' \
  '            if false && names != described {' \
  geode-core
```

Run: `zsh scripts/mutation-check.sh "snapshot:"` — both `caught`. Commit.

---

### Task 10: `TreeIndex`, built on the query worker

Spec §5.5. Parent links and CSR child lists, hash-attached on the
grouping prefix, no dependence on sibling contiguity or ENUM collation.

**Files:**
- Create: `crates/geode-core/src/tree.rs`
- Create: `crates/geode-core/benches/tree.rs`
- Modify: `crates/geode-core/src/lib.rs` (`pub mod tree;`)
- Modify: `crates/geode-core/src/snapshot.rs` (`tree` field, built in
  `from_batches`; `tree()` accessor)
- Modify: `crates/geode-core/Cargo.toml` (criterion dev-dep, self
  dev-dep for `test-support`, `[[bench]]`)
- Test: `crates/geode-core/src/tree.rs` (inline)

**Interfaces:**
- Produces:
  ```rust
  pub const NO_PARENT: u32 = u32::MAX;
  impl TreeIndex {
      pub fn build(snapshot: &Snapshot) -> TreeIndex;
      pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool;
      pub fn depth(&self, row: usize) -> usize;
      pub fn parent(&self, row: usize) -> Option<usize>;
      pub fn children(&self, row: usize) -> &[u32];
      pub fn has_children(&self, row: usize) -> bool;
      pub fn roots(&self) -> &[u32];
      pub fn unplaced(&self) -> usize;
  }
  impl Snapshot { pub fn tree(&self) -> &TreeIndex; }
  ```

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-core/src/tree.rs` with the API stubbed (`todo!()`)
and:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{Attribution, ScopeSemantics};
    use crate::snapshot::{ColumnMeta, Snapshot, TestColumn};

    fn dim(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 4],
            scope_semantics: ScopeSemantics::Direct,
        }
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// lhu > underlying > position, three levels, rows sorted by depth
    /// only. Siblings are deliberately interleaved within a depth: L1's
    /// children are rows 3 and 5, L2's are 4 and 6. This is what a
    /// declared `sort` produces (Phase 3 §5.5).
    fn interleaved(dict: bool) -> Snapshot {
        let lhu = vec![None, s("L1"), s("L2"), s("L1"), s("L2"), s("L1"), s("L2"), s("L1")];
        let und = vec![None, None, None, s("SPX"), s("SPX"), s("NDX"), s("NDX"), s("SPX")];
        let pos = vec![None, None, None, None, None, None, None, s("P1")];
        let col = |v: Vec<Option<String>>| {
            if dict {
                TestColumn::Dict(v)
            } else {
                TestColumn::Str(v.iter().map(|x| x.as_deref().map(|s| -> &'static str {
                    Box::leak(s.to_string().into_boxed_str())
                })).collect())
            }
        };
        Snapshot::for_tests(
            vec![
                (dim("lhu"), col(lhu)),
                (dim("underlying_ref"), col(und)),
                (dim("position_ref"), col(pos)),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2, 2, 2, 2, 3])),
                (
                    dim("delta01"),
                    TestColumn::F64((0..8).map(|i| Some(i as f64)).collect()),
                ),
            ],
            3,
        )
    }

    #[test]
    fn children_are_found_by_prefix_not_by_contiguity() {
        for dict in [true, false] {
            let snap = interleaved(dict);
            let t = snap.tree();
            assert_eq!(t.len(), 8);
            assert_eq!(t.roots(), &[0], "dict={dict}");
            assert_eq!(t.children(0), &[1, 2], "dict={dict}");
            assert_eq!(t.children(1), &[3, 5], "L1's underlyings, dict={dict}");
            assert_eq!(t.children(2), &[4, 6], "L2's underlyings, dict={dict}");
            assert_eq!(t.children(3), &[7], "L1/SPX's position, dict={dict}");
            assert!(t.children(4).is_empty());
            assert_eq!(t.parent(7), Some(3));
            assert_eq!(t.parent(0), None);
            assert_eq!(t.depth(7), 3);
            assert!(t.has_children(1) && !t.has_children(7));
            assert_eq!(t.unplaced(), 0);
        }
    }

    #[test]
    fn children_keep_row_order_so_a_declared_sort_is_the_default_sibling_order() {
        // Rows 5 (NDX) precedes nothing here, but if the compiler's sort
        // put NDX before SPX the CSR would list 5 before 3. Row order in,
        // row order out — the index imposes none of its own.
        let snap = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Str(vec![None, Some("L1"), Some("L1"), Some("L1")])),
                (
                    dim("underlying_ref"),
                    TestColumn::Str(vec![None, None, Some("SPX"), Some("NDX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 2, 2])),
            ],
            2,
        );
        assert_eq!(snap.tree().children(1), &[2, 3]);
    }

    #[test]
    fn a_row_whose_parent_is_missing_attaches_to_the_root_and_is_counted() {
        // A stale ENUM blanks a value the row carries at a finer level
        // (P2 §3.6): the child's prefix names an lhu no depth-1 row has.
        // Silently dropping it would hide a real position; hiding the
        // count would hide that anything went wrong.
        let snap = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Str(vec![None, Some("L1"), Some("GHOST")])),
                (
                    dim("underlying_ref"),
                    TestColumn::Str(vec![None, None, Some("SPX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 2])),
            ],
            2,
        );
        let t = snap.tree();
        assert_eq!(t.parent(2), Some(0), "attached to the grand total");
        assert_eq!(t.children(0), &[1, 2]);
        assert_eq!(t.unplaced(), 1);
    }

    #[test]
    fn null_is_its_own_token_distinct_from_an_empty_string() {
        // A depth-1 row with a NULL lhu (the blanked-ENUM case) and one
        // with "" are different parents, and a child with NULL lhu finds
        // the NULL one.
        let snap = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Str(vec![None, None, Some(""), None])),
                (
                    dim("underlying_ref"),
                    TestColumn::Str(vec![None, None, None, Some("SPX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2])),
            ],
            2,
        );
        let t = snap.tree();
        assert_eq!(t.parent(3), Some(1));
        assert!(t.children(2).is_empty());
        assert_eq!(t.unplaced(), 0);
    }

    #[test]
    fn a_result_without_a_depth_column_is_flat() {
        let snap = Snapshot::for_tests(
            vec![(dim("delta01"), TestColumn::F64(vec![Some(1.0), Some(2.0)]))],
            0,
        );
        let t = snap.tree();
        assert_eq!(t.roots(), &[0, 1]);
        assert!(t.children(0).is_empty());
        assert_eq!(t.unplaced(), 0);
    }

    #[test]
    fn an_empty_result_has_an_empty_tree() {
        let snap = Snapshot::for_tests(
            vec![(dim("lhu"), TestColumn::Str(vec![])), (dim("row_depth"), TestColumn::I32(vec![]))],
            1,
        );
        assert!(snap.tree().is_empty());
        assert!(snap.tree().roots().is_empty());
        assert!(snap.tree().children(0).is_empty(), "out of range is empty, not a panic");
        assert_eq!(snap.tree().parent(0), None);
    }

    #[test]
    fn a_grouping_column_absent_from_the_batch_still_builds() {
        // Below the depth bound the compiler emits a NULL constant for
        // every unmaterialised grouping column, so it is present. But a
        // fixture, or a future compiler, might omit it; the index treats
        // an absent column as NULL for every row rather than panicking.
        let snap = Snapshot::from_batches(
            {
                use arrow::array::{Int32Array, StringArray};
                use arrow::datatypes::{DataType, Field, Schema};
                use arrow::record_batch::RecordBatch;
                use std::sync::Arc;
                let schema = Arc::new(Schema::new(vec![
                    Field::new("lhu", DataType::Utf8, true),
                    Field::new("row_depth", DataType::Int32, true),
                ]));
                vec![RecordBatch::try_new(
                    schema,
                    vec![
                        Arc::new(StringArray::from(vec![None, Some("L1")])),
                        Arc::new(Int32Array::from(vec![0, 1])),
                    ],
                )
                .unwrap()]
            },
            vec![dim("lhu"), dim("row_depth")],
            vec!["lhu".into(), "underlying_ref".into()],
            crate::snapshot::Provenance::default(),
        )
        .unwrap();
        assert_eq!(snap.tree().children(0), &[1]);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core tree:: 2>&1 | tail -3`
Expected: `todo!()` panics or missing `tree()`.

- [ ] **Step 3: Implement**

```rust
//! The parent/child structure of a rollup result (Phase 3 spec §5.5).
//!
//! Built once, on the query worker, inside `Snapshot::from_batches`:
//! for a 729k-row result the build costs tens of milliseconds, which is
//! the whole §7.1 budget if it ran on the render thread. Immutable and
//! `Arc`-shared with the snapshot it describes.
//!
//! The compiler orders by `row_depth` first, so a parent always precedes
//! its children. Within a depth the order is the view's declared sort,
//! then the grouping columns — so siblings are **not** contiguous when a
//! sort is declared, and nothing here assumes they are. Each row's
//! grouping prefix is hashed into a per-depth table and its parent looked
//! up in the table for the depth above; children are then laid out in CSR
//! form in row order, which makes a declared sort the default sibling
//! order for free. Equality is on the cell text under either encoding,
//! with NULL as its own token, so a blanked ENUM value (P2 §3.6) is a
//! distinct key rather than a collision — and nothing depends on how
//! DuckDB collates an ENUM.

use crate::snapshot::Snapshot;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

pub const NO_PARENT: u32 = u32::MAX;

#[derive(Debug, Clone, Default)]
pub struct TreeIndex {
    parent: Vec<u32>,
    depth: Vec<u8>,
    /// CSR offsets, length `rows + 1`.
    child_start: Vec<u32>,
    children: Vec<u32>,
    roots: Vec<u32>,
    unplaced: u32,
}

impl TreeIndex {
    pub fn build(snapshot: &Snapshot) -> TreeIndex {
        let n = snapshot.rows();
        let grouping = snapshot.grouping();
        let cols: Vec<Option<usize>> = grouping
            .iter()
            .map(|g| snapshot.column_index(g))
            .collect();
        let max_depth = grouping.len();

        let mut depth = vec![0u8; n];
        let mut parent = vec![NO_PARENT; n];
        let mut roots = Vec::new();
        let mut unplaced = 0u32;

        let has_depth = n > 0 && snapshot.depth_of_row(0).is_some() || (0..n).any(|r| snapshot.depth_of_row(r).is_some());
        if !has_depth {
            roots.extend(0..n as u32);
            return Self::finish(parent, depth, roots, unplaced);
        }

        // Rows bucketed by depth so every parent is indexed before any
        // child looks for it, whatever the row order.
        let mut by_depth: Vec<Vec<u32>> = vec![Vec::new(); max_depth + 1];
        for r in 0..n {
            let d = snapshot.depth_of_row(r).unwrap_or(0).min(max_depth);
            depth[r] = d as u8;
            by_depth[d].push(r as u32);
        }

        // One hash table per depth, chained through `next` on collision
        // so a lookup verifies the actual prefix rather than trusting a
        // 64-bit hash. Each row sits in exactly one table.
        let mut tables: Vec<HashMap<u64, u32>> = (0..=max_depth).map(|_| HashMap::new()).collect();
        let mut next = vec![NO_PARENT; n];

        for d in 0..=max_depth {
            for &r in &by_depth[d] {
                let row = r as usize;
                if d == 0 {
                    roots.push(r);
                } else {
                    let h = prefix_hash(snapshot, &cols, row, d - 1);
                    let mut candidate = tables[d - 1].get(&h).copied();
                    let mut found = None;
                    while let Some(c) = candidate {
                        if prefix_eq(snapshot, &cols, row, c as usize, d - 1) {
                            found = Some(c);
                            break;
                        }
                        candidate = (next[c as usize] != NO_PARENT).then(|| next[c as usize]);
                    }
                    match found {
                        Some(p) => parent[row] = p,
                        None => {
                            unplaced += 1;
                            parent[row] = roots.first().copied().unwrap_or(NO_PARENT);
                        }
                    }
                }
                let h = prefix_hash(snapshot, &cols, row, d);
                match tables[d].entry(h) {
                    Entry::Occupied(mut e) => {
                        next[row] = *e.get();
                        *e.get_mut() = r;
                    }
                    Entry::Vacant(v) => {
                        v.insert(r);
                    }
                }
            }
        }
        Self::finish(parent, depth, roots, unplaced)
    }

    fn finish(parent: Vec<u32>, depth: Vec<u8>, roots: Vec<u32>, unplaced: u32) -> TreeIndex {
        let n = parent.len();
        let mut counts = vec![0u32; n];
        for &p in &parent {
            if p != NO_PARENT {
                counts[p as usize] += 1;
            }
        }
        let mut child_start = vec![0u32; n + 1];
        for i in 0..n {
            child_start[i + 1] = child_start[i] + counts[i];
        }
        let mut fill = child_start.clone();
        let mut children = vec![0u32; child_start[n] as usize];
        for (r, &p) in parent.iter().enumerate() {
            if p != NO_PARENT {
                let slot = fill[p as usize];
                children[slot as usize] = r as u32;
                fill[p as usize] += 1;
            }
        }
        TreeIndex {
            parent,
            depth,
            child_start,
            children,
            roots,
            unplaced,
        }
    }

    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    pub fn depth(&self, row: usize) -> usize {
        self.depth.get(row).copied().unwrap_or(0) as usize
    }

    pub fn parent(&self, row: usize) -> Option<usize> {
        match self.parent.get(row) {
            Some(&p) if p != NO_PARENT => Some(p as usize),
            _ => None,
        }
    }

    pub fn children(&self, row: usize) -> &[u32] {
        if row + 1 >= self.child_start.len() {
            return &[];
        }
        let (a, b) = (self.child_start[row] as usize, self.child_start[row + 1] as usize);
        &self.children[a..b]
    }

    pub fn has_children(&self, row: usize) -> bool {
        !self.children(row).is_empty()
    }

    /// Depth-0 rows: the grand total, normally exactly one. A flat
    /// result lists every row.
    pub fn roots(&self) -> &[u32] {
        &self.roots
    }

    /// Rows whose parent was not in the result, attached to the first
    /// root instead. Shown in the blotter's footer, never hidden.
    pub fn unplaced(&self) -> usize {
        self.unplaced as usize
    }
}

/// FNV-1a over the first `k` grouping cells of `row`. A NULL cell hashes
/// a token no string can produce; an absent column is NULL everywhere.
fn prefix_hash(snapshot: &Snapshot, cols: &[Option<usize>], row: usize, k: usize) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut feed = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    };
    for col in cols.iter().take(k) {
        match col.and_then(|c| snapshot.text_at(c, row)) {
            Some(s) => {
                feed(0x01);
                for b in s.bytes() {
                    feed(b);
                }
            }
            None => feed(0x00),
        }
        feed(0xff);
    }
    h
}

fn prefix_eq(snapshot: &Snapshot, cols: &[Option<usize>], a: usize, b: usize, k: usize) -> bool {
    cols.iter().take(k).all(|col| match col {
        Some(c) => snapshot.text_at(*c, a) == snapshot.text_at(*c, b),
        None => true,
    })
}
```

Replace the awkward `has_depth` line with the honest one:

```rust
        let has_depth = snapshot.has_depth_column();
```

and add to `Snapshot`: `pub fn has_depth_column(&self) -> bool { self.depth_col.is_some() }`.

In `snapshot.rs`, add the field `tree: TreeIndex` and build it at the end
of `from_batches`:

```rust
        let mut snapshot = Snapshot {
            batch,
            meta,
            grouping,
            depth_col,
            provenance,
            tree: TreeIndex::default(),
        };
        snapshot.tree = TreeIndex::build(&snapshot);
        Ok(snapshot)
```

with `use crate::tree::TreeIndex;` and

```rust
    /// The parent/child structure, built once here (Phase 3 §5.5).
    pub fn tree(&self) -> &TreeIndex {
        &self.tree
    }
```

`lib.rs` adds `pub mod tree;` after `pub mod snapshot;`.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-core`
Expected: green; the tree tests pass under both encodings.

- [ ] **Step 5: The bench**

`crates/geode-core/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = "3.27.0"
criterion = "0.8.2"
# Enables `test-support` for this crate's own benches (cargo permits a
# self dev-dependency for exactly this), so `Snapshot::for_tests` is
# available to `benches/tree.rs` under the plain `cargo bench --workspace
# --no-run` CI runs.
geode-core = { path = ".", features = ["test-support"] }

[[bench]]
name = "tree"
harness = false
```

Create `crates/geode-core/benches/tree.rs`:

```rust
//! `TreeIndex::build` at the three result shapes `docs/perf.md` records
//! for the blotter's tree view: 133 rows (bounded to depth 2), 136,868
//! (scoped to three books, all depths) and 729,466 (unscoped). The build
//! runs on the query worker, so this is what §7.1's handoff pays.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
use geode_core::tree::TreeIndex;
use std::hint::black_box;

fn dim(name: &str) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![Attribution::Additive; 4],
        scope_semantics: ScopeSemantics::Direct,
    }
}

/// A three-level tree with `l1` first-level nodes, `l2` under each, and
/// `l3` under each of those, rows emitted depth-first by level (the
/// compiler's order) with siblings interleaved as a declared sort would.
fn shape(l1: usize, l2: usize, l3: usize) -> Snapshot {
    let mut lhu: Vec<Option<String>> = vec![None];
    let mut und: Vec<Option<String>> = vec![None];
    let mut pos: Vec<Option<String>> = vec![None];
    let mut depth: Vec<i32> = vec![0];
    for a in 0..l1 {
        lhu.push(Some(format!("L{a}")));
        und.push(None);
        pos.push(None);
        depth.push(1);
    }
    for b in 0..l2 {
        for a in 0..l1 {
            lhu.push(Some(format!("L{a}")));
            und.push(Some(format!("U{b}")));
            pos.push(None);
            depth.push(2);
        }
    }
    for c in 0..l3 {
        for b in 0..l2 {
            for a in 0..l1 {
                lhu.push(Some(format!("L{a}")));
                und.push(Some(format!("U{b}")));
                pos.push(Some(format!("P{a}_{b}_{c}")));
                depth.push(3);
            }
        }
    }
    Snapshot::for_tests(
        vec![
            (dim("lhu"), TestColumn::Dict(lhu)),
            (dim("underlying_ref"), TestColumn::Dict(und)),
            (dim("position_ref"), TestColumn::Dict(pos)),
            (dim("row_depth"), TestColumn::I32(depth)),
        ],
        3,
    )
}

fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_index_build");
    group.sample_size(10);
    for (name, l1, l2, l3) in [
        ("133_rows", 12, 10, 0),
        ("137k_rows", 12, 10, 1_130),
        ("729k_rows", 80, 10, 900),
    ] {
        let snap = shape(l1, l2, l3);
        eprintln!("[{name}] {} rows", snap.rows());
        group.bench_function(name, |b| b.iter(|| black_box(TreeIndex::build(&snap))));
    }
    group.finish();
}

criterion_group!(benches, bench_build);
criterion_main!(benches);
```

Run: `cargo bench -p geode-core -- tree_index_build` and record the three
medians in `docs/perf.md` under a new heading **"Phase 3a: tree index
(`cargo bench -p geode-core`)"**, with one sentence saying whether the
729k build fits the §7.1 handoff comfortably (expected: tens of ms, on
the worker, off the render thread).

- [ ] **Step 6: Full check and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`

```bash
git add crates/geode-core docs/perf.md
git commit -m "feat(core): TreeIndex built on the query worker

Parent links and CSR child lists over a snapshot, attached by hashing
the grouping prefix per depth — no dependence on sibling contiguity or
ENUM collation, so a declared sort is the default sibling order. Rows
whose parent is absent attach to the root and are counted (Phase 3
§5.5). Benchmarked at the three result shapes.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 7: Harness entries** (package `geode-core`)

```sh
# ---- tree index (Phase 3 §5.5)

run_mutation "tree: a parent is found by prefix, not by position" \
  crates/geode-core/src/tree.rs \
  '                        if prefix_eq(snapshot, &cols, row, c as usize, d - 1) {' \
  '                        if true {' \
  geode-core

run_mutation "tree: an unplaced row is counted" \
  crates/geode-core/src/tree.rs \
  '                            unplaced += 1;' \
  '                            unplaced += 0;' \
  geode-core

run_mutation "tree: NULL is not the empty string" \
  crates/geode-core/src/tree.rs \
  '            None => feed(0x00),' \
  '            None => feed(0x01),' \
  geode-core

run_mutation "tree: children keep row order" \
  crates/geode-core/src/tree.rs \
  '        for (r, &p) in parent.iter().enumerate() {' \
  '        for (r, &p) in parent.iter().enumerate().rev() {' \
  geode-core
```

Run: `zsh scripts/mutation-check.sh "tree:"` — all `caught`. (The NULL
entry is caught by `null_is_its_own_token…`: with NULL hashing like a
one-byte string the chain still verifies with `prefix_eq`, so make sure
that test asserts the *parent*, not just the count — it does.) If the
NULL entry survives, the hash collision is being rescued by `prefix_eq`,
which is correct behaviour; replace the entry with one that breaks
`prefix_eq`'s `None => true` arm instead:

```sh
run_mutation "tree: an absent grouping column is NULL everywhere" \
  crates/geode-core/src/tree.rs \
  '        None => true,' \
  '        None => false,' \
  geode-core
```

Commit.

---

### Task 11: Column presentation in view config

Spec §6.2: `format`, `label`, `width` on a view column, keyed by name so
the compiler's `ViewColumn` matching is untouched.

**Files:**
- Modify: `crates/geode-core/src/view.rs`
- Test: `crates/geode-core/src/view.rs` (inline)

**Interfaces:**
- Produces:
  ```rust
  pub enum Negative { Minus, Parens }
  pub enum Colour { None, Sign }
  pub enum Scale { None, Thousands, Millions }
  impl Scale { pub fn divisor(self) -> f64; pub fn suffix(self) -> &'static str }
  pub struct ColumnFormat { pub precision: u8, pub thousands: bool, pub negative: Negative, pub colour: Colour, pub scale: Scale }
  impl ColumnFormat { pub const MEASURE: ColumnFormat; pub const TEXT: ColumnFormat;
                      pub fn with(self, p: &ColumnPresentation) -> ColumnFormat }
  #[derive(Default)] pub struct ColumnPresentation { pub precision: Option<u8>, pub thousands: Option<bool>,
      pub negative: Option<Negative>, pub colour: Option<Colour>, pub scale: Option<Scale>,
      pub label: Option<String>, pub width: Option<f32> }
  impl ViewSpec { pub presentation: BTreeMap<String, ColumnPresentation>;
                  pub fn presentation_of(&self, column: &str) -> ColumnPresentation }
  ```

- [ ] **Step 1: Write the failing tests**

Add to `view.rs`'s test module (it already has a `doc` helper building a
`MergedDoc` from text; use it):

```rust
    #[test]
    fn presentation_is_parsed_per_column_and_defaults_are_per_kind() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { precision = 0, thousands = true, negative = "parens", colour = "sign", scale = "k" }
label = "NPV"
width = 110
[[tree.columns]]
name = "delta01"
format = { precision = 4 }
[[tree.columns]]
name = "lhu"
kind = "dimension"
"#));
        assert!(diags.is_empty(), "{diags:?}");
        let v = &views[0];
        let npv = v.presentation_of("npv");
        assert_eq!(npv.label.as_deref(), Some("NPV"));
        assert_eq!(npv.width, Some(110.0));
        let f = ColumnFormat::MEASURE.with(&npv);
        assert_eq!(f.precision, 0);
        assert!(f.thousands);
        assert_eq!(f.negative, Negative::Parens);
        assert_eq!(f.colour, Colour::Sign);
        assert_eq!(f.scale, Scale::Thousands);
        assert_eq!(Scale::Thousands.divisor(), 1_000.0);
        assert_eq!(Scale::Millions.divisor(), 1_000_000.0);
        assert_eq!(Scale::None.divisor(), 1.0);
        assert_eq!(Scale::Thousands.suffix(), "k");
        assert_eq!(Scale::Millions.suffix(), "M");
        assert_eq!(Scale::None.suffix(), "");

        let d = ColumnFormat::MEASURE.with(&v.presentation_of("delta01"));
        assert_eq!(d.precision, 4, "one field overrides, the rest default");
        assert!(d.thousands);
        assert_eq!(d.negative, Negative::Minus);
        assert_eq!(d.scale, Scale::None, "unscaled by default");

        let l = ColumnFormat::TEXT.with(&v.presentation_of("lhu"));
        assert_eq!(l.colour, Colour::None);
        assert_eq!(v.presentation_of("nonesuch"), ColumnPresentation::default());
    }

    #[test]
    fn bad_presentation_values_warn_and_are_ignored() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { precision = 40, negative = "red", colour = "loud", thousands = "yes", scale = "bn" }
width = -5
"#));
        let p = views[0].presentation_of("npv");
        assert_eq!(p, ColumnPresentation::default(), "{p:?}");
        assert_eq!(diags.len(), 6, "{diags:?}");
        assert!(diags.iter().all(|d| d.message.contains("npv")));
    }

    #[test]
    fn color_is_accepted_as_a_spelling_of_colour() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { color = "none" }
"#));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(views[0].presentation_of("npv").colour, Some(Colour::None));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core view:: 2>&1 | grep -E "^error" | head -3`

- [ ] **Step 3: Implement**

Add after `SortKey`:

```rust
/// How a negative number is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Negative {
    Minus,
    Parens,
}

/// Whether a number's sign colours the cell (`chart_bullish` /
/// `chart_bearish` in the theme).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    None,
    Sign,
}

/// Divide before display: `k` by a thousand, `M` by a million.
/// `precision` applies to the divided number (Phase 3 §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    None,
    Thousands,
    Millions,
}

impl Scale {
    pub fn divisor(self) -> f64 {
        match self {
            Scale::None => 1.0,
            Scale::Thousands => 1_000.0,
            Scale::Millions => 1_000_000.0,
        }
    }

    /// What the header shows after the label.
    pub fn suffix(self) -> &'static str {
        match self {
            Scale::None => "",
            Scale::Thousands => "k",
            Scale::Millions => "M",
        }
    }
}

/// A resolved format: every field decided (Phase 3 §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnFormat {
    pub precision: u8,
    pub thousands: bool,
    pub negative: Negative,
    pub colour: Colour,
    pub scale: Scale,
}

impl ColumnFormat {
    /// The default for a measure or derived number.
    pub const MEASURE: ColumnFormat = ColumnFormat {
        precision: 2,
        thousands: true,
        negative: Negative::Minus,
        colour: Colour::Sign,
        scale: Scale::None,
    };
    /// The default for a dimension or attribute.
    pub const TEXT: ColumnFormat = ColumnFormat {
        precision: 0,
        thousands: false,
        negative: Negative::Minus,
        colour: Colour::None,
        scale: Scale::None,
    };

    /// This default with the presentation's overrides applied.
    pub fn with(self, p: &ColumnPresentation) -> ColumnFormat {
        ColumnFormat {
            precision: p.precision.unwrap_or(self.precision),
            thousands: p.thousands.unwrap_or(self.thousands),
            negative: p.negative.unwrap_or(self.negative),
            colour: p.colour.unwrap_or(self.colour),
            scale: p.scale.unwrap_or(self.scale),
        }
    }
}

/// What a view says about how a column looks — each field optional, so
/// a per-kind default fills the rest at plan time. Keyed by column name
/// on the view rather than carried on `ViewColumn`, so the compiler's
/// matching on that enum is untouched.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ColumnPresentation {
    pub precision: Option<u8>,
    pub thousands: Option<bool>,
    pub negative: Option<Negative>,
    pub colour: Option<Colour>,
    pub scale: Option<Scale>,
    pub label: Option<String>,
    pub width: Option<f32>,
}
```

`ViewSpec` gains `pub presentation: BTreeMap<String, ColumnPresentation>,`
(`use std::collections::BTreeMap;`) and:

```rust
    /// The presentation declared for a column, or the empty one.
    pub fn presentation_of(&self, column: &str) -> ColumnPresentation {
        self.presentation.get(column).cloned().unwrap_or_default()
    }
```

In `from_doc`, inside the columns loop after `view.columns.push(column);`:

```rust
                    let mut p = ColumnPresentation::default();
                    let warn = |m: String| bad(format!("column '{col_name}': {m}"));
                    if let Some(f) = c.get("format") {
                        match f.as_table() {
                            None => diags.push(warn("'format' is not a table".into())),
                            Some(f) => {
                                match f.get("precision") {
                                    None => {}
                                    Some(v) => match v.as_integer() {
                                        Some(n) if (0..=12).contains(&n) => p.precision = Some(n as u8),
                                        _ => diags.push(warn(format!(
                                            "'precision' must be an integer 0–12 (got {v})"
                                        ))),
                                    },
                                }
                                match f.get("thousands") {
                                    None => {}
                                    Some(v) => match v.as_bool() {
                                        Some(b) => p.thousands = Some(b),
                                        None => diags.push(warn(format!(
                                            "'thousands' must be true or false (got {v})"
                                        ))),
                                    },
                                }
                                match f.get("negative").and_then(|v| v.as_str()) {
                                    None if f.get("negative").is_none() => {}
                                    Some("minus") => p.negative = Some(Negative::Minus),
                                    Some("parens") => p.negative = Some(Negative::Parens),
                                    other => diags.push(warn(format!(
                                        "'negative' must be \"minus\" or \"parens\" (got {other:?})"
                                    ))),
                                }
                                let colour = f.get("colour").or_else(|| f.get("color"));
                                match colour.and_then(|v| v.as_str()) {
                                    None if colour.is_none() => {}
                                    Some("none") => p.colour = Some(Colour::None),
                                    Some("sign") => p.colour = Some(Colour::Sign),
                                    other => diags.push(warn(format!(
                                        "'colour' must be \"none\" or \"sign\" (got {other:?})"
                                    ))),
                                }
                                match f.get("scale").and_then(|v| v.as_str()) {
                                    None if f.get("scale").is_none() => {}
                                    Some("none") => p.scale = Some(Scale::None),
                                    Some("k") => p.scale = Some(Scale::Thousands),
                                    Some("M") => p.scale = Some(Scale::Millions),
                                    other => diags.push(warn(format!(
                                        "'scale' must be \"none\", \"k\" or \"M\" (got {other:?})"
                                    ))),
                                }
                            }
                        }
                    }
                    if let Some(l) = c.get("label") {
                        match l.as_str() {
                            Some(s) => p.label = Some(s.to_string()),
                            None => diags.push(warn("'label' must be a string".into())),
                        }
                    }
                    if let Some(w) = c.get("width") {
                        match w.as_float().or_else(|| w.as_integer().map(|i| i as f64)) {
                            Some(x) if x > 0.0 => p.width = Some(x as f32),
                            _ => diags.push(warn(format!("'width' must be a positive number (got {w})"))),
                        }
                    }
                    if p != ColumnPresentation::default() {
                        view.presentation.insert(col_name.to_string(), p);
                    }
```

- [ ] **Step 4: Run**

Run: `cargo test -p geode-core view::`
Expected: green, including the existing view tests.

- [ ] **Step 5: Full check and commit**

```bash
git add crates/geode-core/src/view.rs
git commit -m "feat(core): column presentation on views — format, label, width

Keyed by column name so the compiler's ViewColumn matching is untouched;
per-kind defaults fill what a view omits; every bad value warns and is
ignored (Phase 3 §6.2).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

- [ ] **Step 6: Harness entry** (package `geode-core`)

```sh
# ---- column presentation (Phase 3 §6.2)

run_mutation "view: a format override applies over the kind default" \
  crates/geode-core/src/view.rs \
  '            precision: p.precision.unwrap_or(self.precision),' \
  '            precision: self.precision,' \
  geode-core
```

Run: `zsh scripts/mutation-check.sh "view:"` — `caught`. Commit.

---

### Task 12: Unfiltered harness run, docs, and handoff

**Files:**
- Modify: `scripts/mutation-check.sh` (header count), `CLAUDE.md`
  (harness entry count in the commands block; the Phase 2/3 status
  paragraph), `docs/perf.md` (done in Task 10)

- [ ] **Step 1: Run the whole harness**

Run: `zsh scripts/mutation-check.sh`
Expected: every line `caught`; no `SURVIVED`, no `ANCHOR`. A `SURVIVED`
is a missing test — add it before going on, in the task that owns the
behaviour. An `ANCHOR` is a stale entry — fix the anchor.

- [ ] **Step 2: Update the counts and the status paragraph**

In `CLAUDE.md`, the commands block says `# mutation harness (89
entries)`; make it the new total (`grep -c '^run_mutation' scripts/mutation-check.sh`).
In the "Phase 3 (blotter) is next" paragraph, add one sentence after the
sequencing constraint:

> **Phase 3a is done:** `[sources]` is read (`sources.toml`,
> `SourceSpec::from_doc`), `DataService` owns a discovery scheduler and
> one ingest runner, and modules reach it through `DataHandle`
> (`geode_data::handle`). The probe now rides the handle; it is deleted
> in Phase 3c.

- [ ] **Step 3: Final check and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`

```bash
git add CLAUDE.md scripts/mutation-check.sh
git commit -m "docs: Phase 3a landed — sources, scheduler, DataHandle, tree index

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01H67rCSzZZZdiknfaMNBhx1"
```

---

## Self-Review

**Spec coverage (Phase 3 spec → task):**

| Spec | Task |
|---|---|
| §2.4 coalescing per tile | 2 |
| §2.5 one runner, store on its thread | 4, 6 |
| §2.6 snapshot carries its tree | 10 |
| §2.7 `AsOf` in core | 1 |
| §2.8 done state (file after start updates the tile) | 6, 8 |
| §5.1 `DataHandle`, `DataEvent`, `QueryOutcome`, bounded channel, drop count, `for_tests` | 1, 3, 7 |
| §5.2 `sources.toml` | 5 |
| §5.3 scheduler; immediate first poll; health never fatal | 6 |
| §5.4 database path | **not here** — `[app] data.db_path` and `--demo` are `geode-app` config, Plan 3c |
| §5.5 index accessors, `TreeIndex`, `ColumnFormat`, `AsOf` | 9, 10, 11, 1 |
| §7.2 data tests listed | 5 (from_doc errors), 6 (scheduler), 2 (per-key), 7 (round trip, full channel) |
| §7.3 tree bench | 10 |
| §9 step 1 (probe alive on the handle) | 8 |
| §9 step 2 (core additions) | 9–11 |

Spec §5.1 says the UI drains the outbound channel in a foreground task
that *wakes on arrival*; that task is the app bridge, Plan 3c. This plan
delivers the sink and the probe keeps its 250 ms poll, as Task 8 says.

**Placeholder scan:** no TBD/TODO. Every step has code.

**Type consistency:**
- `QueryParams` (Task 3) is what `DataHandle::query` (Task 7) and the
  probe (Task 8) build; fields match.
- `IngestEvent::Failed { dataset, batch, reason }` (Task 4) is what the
  service maps (Task 6).
- `SchedulerEvent`/`SchedulerSink` (Task 6) are consumed only inside
  `service.rs`.
- `from_batches(batches, meta, grouping: Vec<String>, provenance)` is
  changed in Task 9 and consumed by Task 10; Task 2's `run_one` passes
  `req.grouping.len()` until Task 9 switches it to `req.grouping.clone()`.
- `Health` derives `PartialOrd`/`Ord`; Task 6's `worst_health` relies on
  it.

## Execution Handoff

Plan complete. Next plans: **3b** (shell hosting, frame, counts, command
line — spec §3, §4) and **3c** (the blotter, `--demo`, probe deletion —
spec §6, §7, §9 step 5). Each is green on CI on its own and 3b does not
depend on 3a beyond `geode_core::query`.
