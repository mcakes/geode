//! Read workers own their connections, keeping database work off the UI
//! thread. Pending requests coalesce to the latest request per caller key;
//! superseded queries are interrupted and stale results are discarded.
//!
//! The key belongs to the caller, usually a tile. Two tiles showing the
//! same view use different keys and cannot supersede each other.
//!
//! Workers deliver directly through a sink closure. The service routes those
//! results into its event sink without a forwarding thread. `spawn` supplies
//! a channel for callers that need to receive results synchronously.

use crate::query::compile::CompiledQuery;
use crate::query::series::{SeriesPlan, run_series};
use crate::store::Store;
use geode_core::query::QueryKey;
use geode_core::series::SeriesResult;
use geode_core::snapshot::{ColumnMeta, Provenance, Snapshot};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(pub String);

pub type QueryId = u64;

/// Nonblocking result delivery. `false` means the sink refused the result;
/// the worker continues and does not retry it. Shutdown stops the workers.
///
/// Called while the queue lock is held, so a sink must not wait for a consumer
/// or call back into this pool: `submit` and `cancel` take the same lock.
/// Any locks acquired by the sink must preserve that order and must not
/// lead back to a pool operation. The service reads series load-lane health
/// under the tracker lock, releases it, then invokes its event sink.
pub type ResultSink = Arc<dyn Fn(QueryResult) -> bool + Send + Sync>;

/// What a worker produced: a view or document
/// query's `Snapshot`, or a series query's struct-of-arrays result. Two
/// kinds rather than a series `Snapshot` because the chart wants arrays
/// and a series has no tree, grouping or attribution to put in one.
///
/// Not boxed (`clippy::large_enum_variant`): the big variant is the
/// common one — every view and document query answers with a `Snapshot`
/// — so boxing it would add an allocation per query to shrink a value
/// moved once, from the worker into the sink.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Payload {
    Snapshot(Snapshot),
    Series(SeriesResult),
}

/// Work yielding a snapshot or a series result. Snapshot work can be a
/// compiled statement or a request whose database-dependent compilation runs
/// inside the worker's read transaction. Coalescing, interruption and panic
/// containment apply to every variant.
#[derive(Debug)]
pub enum Work {
    Query(CompiledQuery),
    Series(Box<SeriesPlan>),
    /// Database-dependent compilation and execution share one read transaction.
    Read(Box<super::read::ReadQuery>),
}

/// What kind of request this is, carried through to the result so the
/// service's sink can route it to the right `DataEvent` variant without
/// re-deriving it from the compiled SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKind {
    Query,
    Distinct {
        column: String,
    },
    Series {
        /// The `(slot, source, identity)` of every source slot, carried
        /// through so the service's sink can attach each pair's
        /// load-lane health without re-reading the plan.
        pairs: Vec<(u8, String, String)>,
    },
}

pub struct QueryRequest {
    pub key: QueryKey,
    /// The submitter's own counter, echoed back untouched.
    pub tag: u64,
    pub submitted: Instant,
    /// The view compiled, for messages. Not a coalescing key.
    pub view: ViewId,
    pub work: Work,
    /// The grouping columns in order; the snapshot builds its tree from
    /// them.
    pub grouping: Vec<String>,
    pub provenance: Provenance,
    pub kind: RequestKind,
}

pub struct QueryResult {
    pub id: QueryId,
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub view: ViewId,
    /// `Err` carries the failure: a bad query degrades its own key and
    /// leaves the pool running.
    pub payload: Result<Payload, String>,
    pub kind: RequestKind,
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
    /// The last request ID handed out. Allocation and insertion both hold the
    /// queue lock, so concurrent submissions cannot insert an older request
    /// after its replacement and leave the wrong request pending.
    next_id: QueryId,
    shutdown: bool,
}

/// The work a worker does for one request. A function pointer rather than
/// a direct call so a test can inject one that panics — there is no SQL
/// that makes `run_one` panic, and panic-safety is the property most worth
/// testing here.
type RunFn = fn(&duckdb::Connection, &QueryRequest) -> Result<Payload, duckdb::Error>;

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

    /// Replace this key's pending request. Returns the id assigned, which
    /// increases monotonically so callers can discard stale arrivals.
    pub fn submit(&self, req: QueryRequest) -> QueryId {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.next_id += 1;
        let id = q.next_id;
        // After shutdown nothing will ever run this, so queueing it only
        // strands the request (and its snapshot) in the pool.
        if q.shutdown {
            return id;
        }
        // Interrupt the superseded query so its replacement can run promptly.
        if let Some((running_id, handle)) = q.running.get(&req.key)
            && *running_id < id
        {
            handle.interrupt();
        }
        q.pending.insert(req.key, (id, req));
        cvar.notify_all();
        id
    }

    pub fn cancel(&self, key: QueryKey) {
        let (lock, _) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.pending.remove(&key);
        if let Some((id, handle)) = q.running.get(&key) {
            // Record before interrupting: the resulting `Interrupted` is
            // this cancel's own doing, not a query failure, and must not
            // reach the UI as one.
            let (id, handle) = (*id, Arc::clone(handle));
            q.cancelled.insert(id);
            handle.interrupt();
        }
    }

    pub fn shutdown(&self) {
        {
            let (lock, cvar) = &*self.queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.shutdown = true;
            for (_, handle) in q.running.values() {
                handle.interrupt();
            }
            cvar.notify_all();
        }
        for t in self
            .threads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            let _ = t.join();
        }
    }
}

impl Drop for QueryPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker(
    conn: duckdb::Connection,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    sink: ResultSink,
    run: RunFn,
) {
    let handle = conn.interrupt_handle();
    // Log at most one refused result per worker to avoid repeated warnings
    // while a receiver is unavailable. The caller owns the dropped-result count.
    let refusal_logged = AtomicBool::new(false);

    loop {
        let (id, req) = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                // One in-flight query per key. Without this,
                // several workers can run the same key concurrently and
                // finish in any order, so a superseded result can land
                // after the one that replaced it.
                let next = q
                    .pending
                    .keys()
                    .find(|k| !q.running.contains_key(*k))
                    .copied();
                if let Some(key) = next {
                    let (id, req) = q.pending.remove(&key).expect("just observed");
                    q.running.insert(key, (id, Arc::clone(&handle)));
                    break (id, req);
                }
                let (guard, _) = cvar
                    .wait_timeout(q, std::time::Duration::from_millis(20))
                    .unwrap_or_else(|e| e.into_inner());
                q = guard;
            }
        };

        // Contain a query panic so the key leaves `running` and the worker can
        // serve later requests. Report the panic as this request's failure.
        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| run(&conn, &req))
        })) {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(payload) => Err(panic_message(&*payload)),
        };
        release_transaction(&conn);

        // Check staleness and deliver under the same lock, preventing a newer
        // submission from landing between those steps. The sink must not wait
        // for a consumer or reenter the pool. The app sink records pending state
        // and signals its receiver without waiting for the UI to process it.
        let (lock, _) = &*queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.running.get(&req.key).is_some_and(|(rid, _)| *rid == id) {
            q.running.remove(&req.key);
        }
        // A newer request for this key arrived while we ran: our result is
        // stale, so drop it rather than delivering it out of order.
        let stale = q.pending.get(&req.key).is_some_and(|(pid, _)| *pid > id);
        let cancelled = q.cancelled.remove(&id);
        // Shutdown and cancellation deliberately interrupt running queries.
        // Discard their results instead of reporting these interrupts as failures.
        if stale || cancelled || q.shutdown {
            continue;
        }
        let delivered = sink(QueryResult {
            id,
            key: req.key,
            tag: req.tag,
            submitted: req.submitted,
            view: req.view.clone(),
            payload: outcome,
            kind: req.kind.clone(),
        });
        // The queue lock is released before logging: formatting a warning
        // under it would block `submit`/`cancel` on the UI thread.
        drop(q);
        if !delivered {
            log_refused_result(&refusal_logged, &req.view.0);
        }
    }
}

/// Maximum rollback attempts before reporting a connection cleanup failure.
/// Each new statement clears a pending interrupt, so retries can recover from
/// supersession interrupting the preceding rollback. Cleanup runs after every
/// request; a later cleanup may recover a connection that remains aborted.
const ROLLBACK_ATTEMPTS: usize = 3;

/// Leave the connection outside any transaction before the worker's next
/// request. A read transaction ends in the `ROLLBACK` duckdb-rs issues when
/// it is dropped, and that error is discarded. A supersession interrupt that
/// lands on the `ROLLBACK` leaves the connection inside an aborted
/// transaction, where every later statement fails with "Current transaction
/// is aborted" — every view this worker serves, for the rest of the session.
///
/// duckdb-rs 1.10505's `Connection::is_autocommit` always answers `true`, so
/// the transaction state is read from the `ROLLBACK` itself: DuckDB refuses
/// it with [`NO_TRANSACTION`] when the connection is already outside one,
/// which is the ordinary case after a clean read.
fn release_transaction(conn: &duckdb::Connection) {
    for _ in 0..ROLLBACK_ATTEMPTS {
        match conn.execute_batch("ROLLBACK") {
            Ok(()) => return,
            Err(e) if e.to_string().contains(NO_TRANSACTION) => return,
            Err(_) => {}
        }
    }
    tracing::error!(
        target: "geode::query",
        "a query worker could not leave its transaction after {ROLLBACK_ATTEMPTS} \
         rollbacks; its next queries will fail",
    );
}

/// DuckDB's refusal of a `ROLLBACK` outside any transaction. Pinned by
/// `a_rollback_outside_a_transaction_names_no_transaction`, so a DuckDB bump
/// that rewords it fails a test instead of logging on every query.
const NO_TRANSACTION: &str = "no transaction is active";

/// Log a refusal once per worker. Results are not retried; each sink owns
/// its recovery policy. The app mailbox retains results while the UI is busy.
fn log_refused_result(latched: &AtomicBool, view: &str) {
    if latched.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::warn!(
        target: "geode::query",
        "event channel refused the result for view '{view}': dropped, the worker \
         keeps working (further refusals are counted, not logged)",
    );
}

/// The message out of a caught panic payload, which is a `&str` for a
/// literal `panic!` and a `String` for a formatted one.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    let what = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string());
    format!("query worker panicked: {what}")
}

// `pub(crate)`: `query::distinct`'s test fixtures run a compiled
// `DistinctParams` query the same way the pool itself does, rather than
// re-deriving the arrow-to-Snapshot plumbing.
pub(crate) fn run_one(
    conn: &duckdb::Connection,
    req: &QueryRequest,
) -> Result<Payload, duckdb::Error> {
    match &req.work {
        Work::Series(plan) => run_series(conn, plan).map(Payload::Series),
        Work::Read(query) => query
            .run(conn)
            .map(Payload::Snapshot)
            .map_err(|e| duckdb::Error::InvalidParameterName(e.to_string())),
        Work::Query(compiled) => {
            run_snapshot(conn, compiled, &req.grouping, req.provenance.clone())
                .map(Payload::Snapshot)
        }
    }
}

pub(crate) fn run_snapshot(
    conn: &duckdb::Connection,
    compiled: &CompiledQuery,
    grouping: &[String],
    provenance: Provenance,
) -> Result<Snapshot, duckdb::Error> {
    let mut stmt = conn.prepare(&compiled.sql)?;
    let batches: Vec<duckdb::arrow::record_batch::RecordBatch> = stmt
        .query_arrow(duckdb::params_from_iter(compiled.params.iter()))?
        .collect();
    let meta: Vec<ColumnMeta> = compiled
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            attribution_by_depth: c.attribution_by_depth.clone(),
            scope_semantics: c.scope_semantics.clone(),
            summable: c.summable,
            mixed_flag: c.mixed_flag,
        })
        .collect();
    Snapshot::from_batches(batches, meta, grouping.to_vec(), provenance)
        .map_err(|e| duckdb::Error::InvalidParameterName(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    /// A store with one table holding `rows` rows.
    fn fixture(rows: usize) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(&format!(
                "create table t as select i as k, i::double as v
                 from range(0, {rows}) t(i);"
            ))
            .unwrap();
        (dir, store)
    }

    fn query(sql: &str) -> CompiledQuery {
        CompiledQuery {
            sql: sql.to_string(),
            params: Vec::new(),
            grouping: Vec::new(),
            columns: vec![geode_core::snapshot::ColumnMeta {
                name: "v".into(),
                attribution_by_depth: vec![geode_core::attribution::Attribution::Additive],
                scope_semantics: geode_core::attribution::ScopeSemantics::Direct,
                summable: false,
                mixed_flag: None,
            }]
            .into_iter()
            .map(|m| crate::query::compile::CompiledColumn {
                name: m.name,
                grain: None,
                attribution_by_depth: m.attribution_by_depth,
                scope_semantics: m.scope_semantics,
                summable: false,
                mixed_flag: None,
            })
            .collect(),
            stalest_input: Vec::new(),
            resolved_as_of: Default::default(),
            resolved_generation: None,
        }
    }

    fn request(key: u64, view: &str, sql: &str) -> QueryRequest {
        QueryRequest {
            key: QueryKey(key),
            tag: key * 100,
            submitted: std::time::Instant::now(),
            view: ViewId(view.to_string()),
            work: Work::Query(query(sql)),
            grouping: Vec::new(),
            provenance: Provenance::default(),
            kind: RequestKind::Query,
        }
    }

    /// Extract a snapshot payload for view-query assertions.
    fn snapshot(r: QueryResult) -> Result<Snapshot, String> {
        r.payload.map(|p| match p {
            Payload::Snapshot(s) => s,
            Payload::Series(_) => panic!("a view query answered with a series"),
        })
    }

    /// A pool delivering into a channel, built on an injected `run` — for
    /// tests that need to control what a query does (panic, block on a
    /// gate) rather than run real SQL.
    fn channel_pool_with(store: &Store, run: RunFn) -> (QueryPool, Receiver<QueryResult>) {
        let (tx, rx) = channel();
        let sink: ResultSink = Arc::new(move |r| tx.send(r).is_ok());
        (QueryPool::spawn_with_run(store, 1, sink, run).unwrap(), rx)
    }

    #[test]
    fn a_submitted_query_returns_a_snapshot() {
        let (_d, store) = fixture(1_000);
        let (pool, rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        let result = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(result.view, ViewId("v1".into()));
        assert_eq!(snapshot(result).unwrap().rows(), 1);
        pool.shutdown();
    }

    /// Records logged while `f` runs, on this thread only — the same
    /// scoped-subscriber pattern `ingest/runner.rs`'s test module uses.
    fn logged(f: impl FnOnce()) -> Vec<geode_core::log::Record> {
        use tracing_subscriber::layer::SubscriberExt;
        let ring = Arc::new(geode_core::log::Ring::new(8));
        let sub =
            tracing_subscriber::registry().with(geode_core::log::RingLayer::new(ring.clone()));
        tracing::subscriber::with_default(sub, f);
        let mut out = Vec::new();
        ring.drain_since(0, &mut out);
        out
    }

    #[test]
    fn a_refusal_is_logged_once_per_worker_not_once_per_result() {
        // Repeated refusals produce one warning for this worker.
        let latch = AtomicBool::new(false);
        let records = logged(|| {
            log_refused_result(&latch, "positions");
            log_refused_result(&latch, "positions");
        });
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].level, tracing::Level::WARN);
        assert_eq!(records[0].target, "geode::query");
        assert!(records[0].message.contains("positions"), "{records:?}");
    }

    #[test]
    fn a_refused_result_does_not_stop_the_worker() {
        // A refused delivery must leave the worker available for the next query,
        // even with only one worker. Shutdown is the only stop signal.
        let (_d, store) = fixture(100);
        let (tx, rx) = channel();
        let refusals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&refusals);
        let sink: ResultSink = Arc::new(move |r: QueryResult| {
            // Claim exactly one refusal across concurrent worker calls.
            if counter
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return false;
            }
            tx.send(r).is_ok()
        });
        let pool = QueryPool::spawn_with_sink(&store, 1, sink).unwrap();

        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline && refusals.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            refusals.load(Ordering::SeqCst),
            1,
            "the first result must have been refused"
        );

        pool.submit(request(2, "v2", "select sum(v) as v from t"));
        let r = rx.recv_timeout(Duration::from_secs(30));
        pool.shutdown();
        let r = r.expect("the single worker must still deliver the next result");
        assert_eq!(r.key, QueryKey(2));
    }

    #[test]
    fn two_keys_on_one_view_do_not_coalesce() {
        // Two tiles showing the same view have independent request keys.
        // Each must receive its result without superseding the other.
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

    #[test]
    fn different_views_do_not_coalesce_with_each_other() {
        let (_d, store) = fixture(1_000);
        let (pool, rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        pool.submit(request(2, "v2", "select count(*)::double as v from t"));
        let mut seen = Vec::new();
        for _ in 0..2 {
            seen.push(rx.recv_timeout(Duration::from_secs(30)).unwrap().view);
        }
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(seen, vec![ViewId("v1".into()), ViewId("v2".into())]);
        pool.shutdown();
    }

    #[test]
    fn results_for_a_view_never_arrive_out_of_order() {
        // Generation-tagged: a result superseded while it ran is dropped
        // rather than delivered after a newer one.
        let (_d, store) = fixture(200_000);
        let (pool, rx) = QueryPool::spawn(&store, 4).unwrap();
        for _ in 0..6 {
            pool.submit(request(1, "v1", "select sum(v) as v from t"));
        }
        let mut delivered = Vec::new();
        while let Ok(r) = rx.recv_timeout(Duration::from_secs(10)) {
            delivered.push(r.id);
        }
        pool.shutdown();
        for w in delivered.windows(2) {
            assert!(w[0] < w[1], "out of order: {delivered:?}");
        }
    }

    #[test]
    fn leaning_on_a_regroup_key_delivers_far_fewer_than_it_receives() {
        // Latest-wins coalescing: a pending request is replaced outright,
        // so most of a burst never runs at all.
        let (_d, store) = fixture(200_000);
        let (pool, rx) = QueryPool::spawn(&store, 1).unwrap();
        for _ in 0..20 {
            pool.submit(request(1, "v1", "select sum(v) as v from t"));
        }
        let mut delivered = 0;
        while rx.recv_timeout(Duration::from_secs(10)).is_ok() {
            delivered += 1;
        }
        pool.shutdown();
        assert!(
            delivered < 20,
            "expected coalescing, delivered all {delivered}"
        );
    }

    #[test]
    fn a_failing_query_reports_rather_than_killing_the_pool() {
        let (_d, store) = fixture(100);
        let (pool, rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.submit(request(1, "bad", "select * from no_such_table"));
        pool.submit(request(2, "good", "select sum(v) as v from t"));
        let mut views = Vec::new();
        for _ in 0..2 {
            if let Ok(r) = rx.recv_timeout(Duration::from_secs(30)) {
                views.push((r.view.clone(), snapshot(r).is_ok()));
            }
        }
        pool.shutdown();
        assert!(
            views
                .iter()
                .any(|(v, ok)| *v == ViewId("good".into()) && *ok),
            "one bad query must not stop the pool: {views:?}"
        );
        assert!(
            views
                .iter()
                .any(|(v, ok)| *v == ViewId("bad".into()) && !*ok),
            "the failure must be reported, not swallowed: {views:?}"
        );
    }

    #[test]
    fn cancelling_a_view_does_not_hang() {
        let (_d, store) = fixture(500_000);
        let (pool, rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        pool.cancel(QueryKey(1));
        let _ = rx.recv_timeout(Duration::from_secs(30));
        pool.shutdown();
    }

    /// Read a dimension with more than 255 distinct values through the
    /// snapshot boundary. DuckDB promotes the ENUM dictionary key from UInt8
    /// to UInt16 at this size; the snapshot must support both widths.
    fn enum_fixture(distinct: usize, pick: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        let list = (0..distinct)
            .map(|i| format!("'U{i:04}'"))
            .collect::<Vec<_>>()
            .join(",");
        store
            .writer()
            .execute_batch(&format!(
                "create type underlying_enum as enum ({list});
                 create table t as select '{pick}'::underlying_enum as underlying_ref;"
            ))
            .unwrap();
        (dir, store)
    }

    /// Returns the snapshot alongside the Arrow type DuckDB actually
    /// produced, so the key-width promotion is asserted rather than
    /// assumed. This is the one place the promotion is observable —
    /// `Snapshot` exists to keep Arrow out of everything above it.
    fn read_one(store: &Store, sql: &str) -> (Snapshot, String) {
        let conn = store.reader().unwrap();
        let mut stmt = conn.prepare(sql).unwrap();
        let batches: Vec<_> = stmt.query_arrow(duckdb::params![]).unwrap().collect();
        let arrow_type = format!("{:?}", batches[0].schema().field(0).data_type());
        let snap = Snapshot::from_batches(
            batches,
            vec![ColumnMeta {
                name: "underlying_ref".into(),
                attribution_by_depth: vec![geode_core::attribution::Attribution::Additive],
                scope_semantics: geode_core::attribution::ScopeSemantics::Direct,
                summable: false,
                mixed_flag: None,
            }],
            vec!["underlying_ref".into()],
            Provenance::default(),
        )
        .unwrap();
        (snap, arrow_type)
    }

    #[test]
    fn a_dimension_past_the_255_value_cliff_reads_back() {
        let (_d, store) = enum_fixture(300, "U0299");
        let (snap, arrow_type) = read_one(&store, "select underlying_ref from t");
        assert_eq!(
            arrow_type, "Dictionary(UInt16, Utf8)",
            "the cliff this test exists for; if DuckDB stops promoting, \
             the test is no longer covering anything"
        );
        assert_eq!(
            snap.dict_value("underlying_ref", 0),
            Some("U0299"),
            "a 300-value ENUM promotes to UInt16 keys and must still read"
        );
    }

    #[test]
    fn a_summed_bigint_comes_back_as_a_decimal() {
        // Pins the type DuckDB actually produces for a declared `i64`
        // measure, which is why `f64_value` needs a Decimal128 arm at all:
        // `sum(BIGINT)` is HUGEINT, exported as `Decimal128(38, 0)`. If
        // this ever changes, the accessor should be revisited rather than
        // silently reading blank.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .writer()
            .execute_batch("create table t as select 5::bigint as qty union all select 7::bigint;")
            .unwrap();
        let conn = store.reader().unwrap();
        let mut stmt = conn.prepare("select sum(qty) as qty from t").unwrap();
        let batches: Vec<_> = stmt.query_arrow(duckdb::params![]).unwrap().collect();
        assert_eq!(
            format!("{:?}", batches[0].schema().field(0).data_type()),
            "Decimal128(38, 0)"
        );

        let snap = Snapshot::from_batches(
            batches,
            vec![ColumnMeta {
                name: "qty".into(),
                attribution_by_depth: vec![geode_core::attribution::Attribution::Additive],
                scope_semantics: geode_core::attribution::ScopeSemantics::Direct,
                summable: false,
                mixed_flag: None,
            }],
            vec!["qty".into()],
            Provenance::default(),
        )
        .unwrap();
        assert_eq!(
            snap.f64_value("qty", 0),
            Some(12.0),
            "a legal i64 measure must be readable, not blank"
        );
    }

    #[test]
    fn a_dimension_below_the_cliff_still_reads() {
        let (_d, store) = enum_fixture(200, "U0199");
        let (snap, arrow_type) = read_one(&store, "select underlying_ref from t");
        assert_eq!(arrow_type, "Dictionary(UInt8, Utf8)");
        assert_eq!(snap.dict_value("underlying_ref", 0), Some("U0199"));
    }

    #[test]
    fn a_panicking_query_degrades_its_view_without_wedging_it() {
        // A panic must report a failure, clear this key's running entry, and
        // leave the worker available to execute later requests.
        fn boom(_: &duckdb::Connection, _: &QueryRequest) -> Result<Payload, duckdb::Error> {
            panic!("injected panic");
        }
        let (_d, store) = fixture(100);
        // One worker, so a lost worker means a dead pool and the second
        // submit below could not be answered by a survivor.
        let (pool, rx) = channel_pool_with(&store, boom);

        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        let first = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(first.view, ViewId("v1".into()));
        let first_id = first.id;
        let message = snapshot(first).unwrap_err();
        assert!(
            message.contains("panicked") && message.contains("injected panic"),
            "the panic must be reported as this view's failure: {message}"
        );

        // The key remains schedulable after its preceding query panics.
        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        let second = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(second.view, ViewId("v1".into()));
        assert!(second.id > first_id);
        pool.shutdown();
    }

    #[test]
    fn interrupts_on_the_transaction_statements_do_not_wedge_the_connection() {
        // Interrupts on transaction statements can leave the connection aborted.
        // Repeated interrupts exercise cleanup around BEGIN, COMMIT and ROLLBACK;
        // a later request must remain executable after the interrupting thread stops.
        use crate::query::series::{SeriesPlan, Statement, run_series};
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch("create table t as select 1::bigint i, 1.0::double v")
            .unwrap();
        let statement = |sql: &str| Statement {
            sql: sql.into(),
            params: vec![],
        };
        let plan = SeriesPlan {
            slots: vec![1],
            points: statement("select i, v from t"),
            fractions: vec![0.5],
            percentiles: vec![(1, statement("select quantile_cont(v, 0.5) from t"))],
            bin_count: 1,
            bins: vec![(1, statement("select v, v+1, 1::bigint, 1::bigint from t"))],
            coverage: vec![(1, statement("select i, i, i from t"))],
        };
        let handle = conn.interrupt_handle();
        let stop = Arc::new(AtomicBool::new(false));
        let spamming = Arc::clone(&stop);
        let spammer = std::thread::spawn(move || {
            while !spamming.load(Ordering::Relaxed) {
                handle.interrupt();
                std::hint::spin_loop();
            }
        });
        for _ in 0..2000 {
            let _ = run_series(&conn, &plan);
            release_transaction(&conn);
        }
        stop.store(true, Ordering::Relaxed);
        spammer.join().unwrap();
        release_transaction(&conn);
        assert!(
            run_series(&conn, &plan).is_ok(),
            "the connection must come out of the interrupts usable"
        );
    }

    #[test]
    fn a_rollback_outside_a_transaction_names_no_transaction() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let e = conn.execute_batch("ROLLBACK").unwrap_err().to_string();
        assert!(e.contains(NO_TRANSACTION), "{e}");
    }

    /// Set by the first run of `abort_then_run`, so only that run leaves its
    /// transaction behind.
    static LEFT_ABORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    #[test]
    fn a_transaction_left_aborted_does_not_wedge_the_worker() {
        // duckdb-rs discards rollback errors when dropping a transaction. An
        // interrupt during rollback can leave the connection aborted, so inject
        // that state directly and verify cleanup before the next request.
        fn abort_then_run(
            conn: &duckdb::Connection,
            req: &QueryRequest,
        ) -> Result<Payload, duckdb::Error> {
            if !LEFT_ABORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                conn.execute_batch("BEGIN TRANSACTION")?;
                conn.execute_batch("select error('injected')")?;
            }
            run_one(conn, req)
        }
        let (_d, store) = fixture(100);
        let (pool, rx) = channel_pool_with(&store, abort_then_run);

        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        let first = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(
            snapshot(first).is_err(),
            "fixture check: the first run fails"
        );

        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        let second = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(
            snapshot(second).is_ok(),
            "the worker's next query must not inherit the aborted transaction"
        );
        pool.shutdown();
    }

    /// Held false until the test has cancelled, so `cancel` is guaranteed
    /// to land while the query is running rather than racing a query
    /// chosen to be slow.
    static CANCEL_GATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    #[test]
    fn cancelling_a_running_query_delivers_no_failure() {
        // `cancel` interrupts the running query, and DuckDB reports that
        // as an error. It is this cancel's own doing, so delivering it
        // paints "query failed" on a tile the user merely navigated away
        // from.
        fn gated(_: &duckdb::Connection, _: &QueryRequest) -> Result<Payload, duckdb::Error> {
            while !CANCEL_GATE.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            // The shape an interrupted query comes back in.
            Err(duckdb::Error::InvalidParameterName("Interrupted".into()))
        }

        CANCEL_GATE.store(false, Ordering::SeqCst);
        let (_d, store) = fixture(100);
        let (pool, rx) = channel_pool_with(&store, gated);
        pool.submit(request(1, "v1", "select sum(v) as v from t"));

        // Cancel only records an id it finds in `running`, so wait until
        // the worker has actually picked the request up.
        let mut running = false;
        for _ in 0..2000 {
            {
                let (lock, _) = &*pool.queue;
                let q = lock.lock().unwrap_or_else(|e| e.into_inner());
                running = !q.running.is_empty();
            }
            if running {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(running, "the query never started, so nothing was cancelled");

        pool.cancel(QueryKey(1));
        CANCEL_GATE.store(true, Ordering::SeqCst);

        assert!(
            rx.recv_timeout(Duration::from_secs(2)).is_err(),
            "a cancelled query must deliver nothing, not an error"
        );
        pool.shutdown();
    }

    /// Held false until the test has called `shutdown`, so the interrupt
    /// lands while the query is running.
    static SHUTDOWN_GATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    #[test]
    fn shutdown_does_not_deliver_its_own_interrupt_as_a_failure() {
        // Shutdown interrupts must not appear as query failures to a receiver
        // draining results during teardown.
        fn gated(_: &duckdb::Connection, _: &QueryRequest) -> Result<Payload, duckdb::Error> {
            while !SHUTDOWN_GATE.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(duckdb::Error::InvalidParameterName("Interrupted".into()))
        }

        SHUTDOWN_GATE.store(false, Ordering::SeqCst);
        let (_d, store) = fixture(100);
        let (pool, rx) = channel_pool_with(&store, gated);
        pool.submit(request(1, "v1", "select sum(v) as v from t"));

        let mut running = false;
        for _ in 0..2000 {
            {
                let (lock, _) = &*pool.queue;
                let q = lock.lock().unwrap_or_else(|e| e.into_inner());
                running = !q.running.is_empty();
            }
            if running {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(running, "the query never started");

        SHUTDOWN_GATE.store(true, Ordering::SeqCst);
        pool.shutdown();

        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "shutting down must deliver nothing, not a failure it caused"
        );
    }

    #[test]
    fn a_post_shutdown_submit_is_not_queued() {
        // Asserted on the queue rather than on the channel: after shutdown
        // the workers are already joined, so nothing is delivered whether
        // the request was queued or not, and a test that only watched the
        // channel would pass either way. What changes is that the request
        // — and the compiled SQL and provenance it carries — is stranded
        // in the pool for the life of the process.
        let (_d, store) = fixture(100);
        let (pool, rx) = QueryPool::spawn(&store, 1).unwrap();
        pool.shutdown();
        pool.submit(request(1, "v1", "select sum(v) as v from t"));
        {
            let (lock, _) = &*pool.queue;
            let q = lock.lock().unwrap_or_else(|e| e.into_inner());
            assert!(
                q.pending.is_empty(),
                "a post-shutdown submit must not be queued"
            );
        }
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "and nothing runs after shutdown"
        );
    }

    #[test]
    fn shutdown_is_idempotent_and_does_not_hang() {
        let (_d, store) = fixture(100);
        let (pool, _rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.shutdown();
        pool.shutdown();
    }

    #[test]
    fn a_series_request_rides_the_pool_and_delivers_a_series_payload() {
        use crate::adapter::SeriesRows;
        use crate::query::series::compile_series;
        use crate::store::ddl::tests_support::{series_dataset, ts};
        use crate::store::series::{SeriesAppendRequest, append_series};
        use geode_core::query::AsOf;
        use geode_core::schema::SchemaSpec;
        use geode_core::series::{BucketRule, Frequency, SeriesParams, SeriesSpec, SlotKind};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = series_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        let start = ts("2026-01-05T14:30:00Z");
        append_series(
            &store,
            &SeriesAppendRequest {
                dataset: &ds,
                source: "demo_kdb",
                identity: "A",
                rows: &SeriesRows {
                    ts: vec![start, start + chrono::Duration::minutes(1)],
                    value: vec![1.0, 2.0],
                },
                span: (start, start + chrono::Duration::days(1)),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let params = SeriesParams {
            key: QueryKey(3),
            tag: 9,
            submitted: Instant::now(),
            dataset: "series".into(),
            range: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            window: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            as_of: AsOf::Live,
            frequency: Frequency::D1,
            series: vec![SeriesSpec {
                slot: 1,
                kind: SlotKind::Source {
                    source: "demo_kdb".into(),
                    identity: "A".into(),
                    rule: BucketRule::Last,
                },
            }],
            percentiles: Vec::new(),
            bins: None,
        };
        let plan = compile_series(&schema, &params).unwrap();
        let (pool, rx) = QueryPool::spawn(&store, 1).unwrap();
        pool.submit(QueryRequest {
            key: QueryKey(3),
            tag: 9,
            submitted: Instant::now(),
            view: ViewId("series:series".into()),
            work: Work::Series(Box::new(plan)),
            grouping: Vec::new(),
            provenance: Provenance::default(),
            kind: RequestKind::Series {
                pairs: vec![(1, "demo_kdb".into(), "A".into())],
            },
        });
        let r = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        assert_eq!((r.key, r.tag), (QueryKey(3), 9));
        assert!(matches!(r.kind, RequestKind::Series { .. }));
        match r.payload.unwrap() {
            Payload::Series(res) => {
                assert_eq!(res.slots[0].values, vec![2.0]);
                assert_eq!(res.buckets.len(), 1);
            }
            Payload::Snapshot(_) => panic!("a series request answered with a snapshot"),
        }
        pool.shutdown();
    }
}
