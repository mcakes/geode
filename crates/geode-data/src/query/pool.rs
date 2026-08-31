//! The read pool (spec §6.7, §7.3). Owns the read connections so the UI
//! thread never holds one, coalesces latest-wins per view, tags every
//! request and result so a stale arrival can be dropped, and interrupts a
//! superseded query rather than awaiting it.

use crate::query::compile::CompiledQuery;
use crate::store::Store;
use geode_core::snapshot::{ColumnMeta, Provenance, Snapshot};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(pub String);

pub type QueryId = u64;

pub struct QueryRequest {
    pub view: ViewId,
    pub compiled: CompiledQuery,
    pub grouping_len: usize,
    pub provenance: Provenance,
}

pub struct QueryResult {
    pub id: QueryId,
    pub view: ViewId,
    /// `Err` carries the failure: a bad query degrades its own view and
    /// leaves the pool running (spec §10.1).
    pub snapshot: Result<Snapshot, String>,
}

#[derive(Default)]
struct Queue {
    /// At most one pending request per view: a newer submit replaces the
    /// pending one outright, which is what makes coalescing latest-wins.
    pending: HashMap<ViewId, (QueryId, QueryRequest)>,
    /// Interrupt handles for queries currently running.
    running: HashMap<ViewId, (QueryId, Arc<duckdb::InterruptHandle>)>,
    /// Queries interrupted by `cancel`. The worker drops their result
    /// instead of delivering it: an interrupt the caller asked for comes
    /// back from DuckDB as an `Interrupted` error, and reporting that as a
    /// query failure paints an error on a tile the user simply navigated
    /// away from.
    cancelled: std::collections::HashSet<QueryId>,
    shutdown: bool,
}

/// The work a worker does for one request. A function pointer rather than
/// a direct call so a test can inject one that panics — there is no SQL
/// that makes `run_one` panic, and panic-safety is the property most worth
/// testing here.
type RunFn = fn(&duckdb::Connection, &QueryRequest) -> Result<Snapshot, duckdb::Error>;

pub struct QueryPool {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    next_id: AtomicU64,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl QueryPool {
    /// Each worker gets its own connection, created here and moved in:
    /// duckdb's `Connection` is `Send` but not `Sync`, so the pool cannot
    /// hand out connections from a shared `Store` after spawning.
    pub fn spawn(
        store: &Store,
        workers: usize,
    ) -> Result<(QueryPool, Receiver<QueryResult>), crate::store::StoreError> {
        Self::spawn_with(store, workers, run_one)
    }

    fn spawn_with(
        store: &Store,
        workers: usize,
        run: RunFn,
    ) -> Result<(QueryPool, Receiver<QueryResult>), crate::store::StoreError> {
        let (tx, rx) = channel();
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let mut threads = Vec::new();

        for i in 0..workers.max(1) {
            // A failure part-way through leaves earlier workers running and
            // parked on the condvar. Panicking here would detach them and
            // leak their connections, so unwind deliberately instead: tell
            // the ones already spawned to stop, join them, and report.
            let spawned = store.reader().and_then(|conn| {
                let q = Arc::clone(&queue);
                let tx = tx.clone();
                std::thread::Builder::new()
                    .name(format!("geode-query-{i}"))
                    .spawn(move || worker(conn, q, tx, run))
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

        Ok((
            QueryPool {
                queue,
                next_id: AtomicU64::new(1),
                threads: Mutex::new(threads),
            },
            rx,
        ))
    }

    /// Replace this view's pending request. Returns the id assigned, which
    /// increases monotonically so callers can discard stale arrivals.
    pub fn submit(&self, req: QueryRequest) -> QueryId {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        // Allocated under the lock, so id order and insertion order agree.
        // Allocating first let two concurrent submits for one view insert
        // out of id order, leaving the *older* request pending and the
        // newer one discarded as stale when it returned.
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        // After shutdown nothing will ever run this, so queueing it only
        // strands the request (and its snapshot) in the pool.
        if q.shutdown {
            return id;
        }
        // A superseded query is interrupted, not awaited (spec §7.3).
        if let Some((running_id, handle)) = q.running.get(&req.view)
            && *running_id < id
        {
            handle.interrupt();
        }
        q.pending.insert(req.view.clone(), (id, req));
        cvar.notify_all();
        id
    }

    pub fn cancel(&self, view: &ViewId) {
        let (lock, _) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.pending.remove(view);
        if let Some((id, handle)) = q.running.get(view) {
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
    tx: Sender<QueryResult>,
    run: RunFn,
) {
    let handle = conn.interrupt_handle();

    loop {
        let (id, req) = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                // One in-flight query per view (spec §7.3). Without this,
                // several workers can run the same view concurrently and
                // finish in any order, so a superseded result can land
                // after the one that replaced it.
                let next = q
                    .pending
                    .keys()
                    .find(|v| !q.running.contains_key(*v))
                    .cloned();
                if let Some(view) = next {
                    let (id, req) = q.pending.remove(&view).expect("just observed");
                    q.running.insert(view, (id, Arc::clone(&handle)));
                    break (id, req);
                }
                let (guard, _) = cvar
                    .wait_timeout(q, std::time::Duration::from_millis(20))
                    .unwrap_or_else(|e| e.into_inner());
                q = guard;
            }
        };

        // A panic in the query must not escape this loop. Unwinding out of
        // `worker` would leave this view's entry in `running` forever —
        // no later request for it is ever scheduled, so the tile silently
        // stops updating for the rest of the session — and would take the
        // worker with it, shrinking the pool with nothing reported. §10.1
        // says a bad query degrades its own view; a panicking one is a bad
        // query.
        let outcome =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&conn, &req))) {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(payload) => Err(panic_message(&payload)),
            };

        // The stale check and the send happen under one lock. Releasing it
        // between them let a newer request land in the gap and the older
        // result still be delivered, so a tile briefly painted data it had
        // already superseded. `send` on an unbounded channel does not
        // block, so holding the lock across it cannot deadlock.
        let (lock, _) = &*queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.running.get(&req.view).is_some_and(|(rid, _)| *rid == id) {
            q.running.remove(&req.view);
        }
        // A newer request for this view arrived while we ran: our result is
        // stale, so drop it rather than delivering it out of order (§7.3).
        let stale = q.pending.get(&req.view).is_some_and(|(pid, _)| *pid > id);
        let cancelled = q.cancelled.remove(&id);
        if stale || cancelled {
            continue;
        }
        if tx
            .send(QueryResult {
                id,
                view: req.view.clone(),
                snapshot: outcome,
            })
            .is_err()
        {
            return;
        }
    }
}

/// The message out of a caught panic payload, which is a `&str` for a
/// literal `panic!` and a `String` for a formatted one.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    let what = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string());
    format!("query worker panicked: {what}")
}

fn run_one(conn: &duckdb::Connection, req: &QueryRequest) -> Result<Snapshot, duckdb::Error> {
    let mut stmt = conn.prepare(&req.compiled.sql)?;
    let batches: Vec<duckdb::arrow::record_batch::RecordBatch> = stmt
        .query_arrow(duckdb::params_from_iter(req.compiled.params.iter()))?
        .collect();
    let meta: Vec<ColumnMeta> = req
        .compiled
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            attribution_by_depth: c.attribution_by_depth.clone(),
            scope_semantics: c.scope_semantics.clone(),
        })
        .collect();
    Snapshot::from_batches(batches, meta, req.grouping_len, req.provenance.clone())
        .map_err(|e| duckdb::Error::InvalidParameterName(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
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
            }]
            .into_iter()
            .map(|m| crate::query::compile::CompiledColumn {
                name: m.name,
                grain: None,
                attribution_by_depth: m.attribution_by_depth,
                scope_semantics: m.scope_semantics,
            })
            .collect(),
            stalest_input: Vec::new(),
            resolved_as_of: Default::default(),
        }
    }

    fn request(view: &str, sql: &str) -> QueryRequest {
        QueryRequest {
            view: ViewId(view.to_string()),
            compiled: query(sql),
            grouping_len: 0,
            provenance: Provenance::default(),
        }
    }

    #[test]
    fn a_submitted_query_returns_a_snapshot() {
        let (_d, store) = fixture(1_000);
        let (pool, rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.submit(request("v1", "select sum(v) as v from t"));
        let result = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(result.view, ViewId("v1".into()));
        assert_eq!(result.snapshot.unwrap().rows(), 1);
        pool.shutdown();
    }

    #[test]
    fn different_views_do_not_coalesce_with_each_other() {
        let (_d, store) = fixture(1_000);
        let (pool, rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.submit(request("v1", "select sum(v) as v from t"));
        pool.submit(request("v2", "select count(*)::double as v from t"));
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
        // rather than delivered after a newer one (spec §7.3).
        let (_d, store) = fixture(200_000);
        let (pool, rx) = QueryPool::spawn(&store, 4).unwrap();
        for _ in 0..6 {
            pool.submit(request("v1", "select sum(v) as v from t"));
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
            pool.submit(request("v1", "select sum(v) as v from t"));
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
        pool.submit(request("bad", "select * from no_such_table"));
        pool.submit(request("good", "select sum(v) as v from t"));
        let mut views = Vec::new();
        for _ in 0..2 {
            if let Ok(r) = rx.recv_timeout(Duration::from_secs(30)) {
                views.push((r.view, r.snapshot.is_ok()));
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
        pool.submit(request("v1", "select sum(v) as v from t"));
        pool.cancel(&ViewId("v1".into()));
        let _ = rx.recv_timeout(Duration::from_secs(30));
        pool.shutdown();
    }

    /// A dataset whose dimension has more than 255 distinct values, read
    /// back through the snapshot boundary.
    ///
    /// DuckDB sizes an ENUM's dictionary key to the vocabulary: UInt8 up
    /// to 255 values, UInt16 above. Every real underlying list is past
    /// that cliff, so this is the first realistic dataset's behaviour, not
    /// an edge case.
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
            }],
            1,
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
    fn a_dimension_below_the_cliff_still_reads() {
        let (_d, store) = enum_fixture(200, "U0199");
        let (snap, arrow_type) = read_one(&store, "select underlying_ref from t");
        assert_eq!(arrow_type, "Dictionary(UInt8, Utf8)");
        assert_eq!(snap.dict_value("underlying_ref", 0), Some("U0199"));
    }

    #[test]
    fn a_panicking_query_degrades_its_view_without_wedging_it() {
        // A panic used to unwind out of the worker, leaving the view's
        // entry in `running` forever. Nothing was ever scheduled for that
        // view again: the tile stopped updating, with no error and no
        // reason shown, for the rest of the session. The pool also lost a
        // worker each time, silently.
        fn boom(_: &duckdb::Connection, _: &QueryRequest) -> Result<Snapshot, duckdb::Error> {
            panic!("injected panic");
        }
        let (_d, store) = fixture(100);
        // One worker, so a lost worker means a dead pool and the second
        // submit below could not be answered by a survivor.
        let (pool, rx) = QueryPool::spawn_with(&store, 1, boom).unwrap();

        pool.submit(request("v1", "select sum(v) as v from t"));
        let first = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(first.view, ViewId("v1".into()));
        let message = first.snapshot.unwrap_err();
        assert!(
            message.contains("panicked") && message.contains("injected panic"),
            "the panic must be reported as this view's failure: {message}"
        );

        // The same view is still schedulable: this is the wedge.
        pool.submit(request("v1", "select sum(v) as v from t"));
        let second = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(second.view, ViewId("v1".into()));
        assert!(second.id > first.id);
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
        fn gated(_: &duckdb::Connection, _: &QueryRequest) -> Result<Snapshot, duckdb::Error> {
            while !CANCEL_GATE.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            // The shape an interrupted query comes back in.
            Err(duckdb::Error::InvalidParameterName("Interrupted".into()))
        }

        CANCEL_GATE.store(false, Ordering::SeqCst);
        let (_d, store) = fixture(100);
        let (pool, rx) = QueryPool::spawn_with(&store, 1, gated).unwrap();
        pool.submit(request("v1", "select sum(v) as v from t"));

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

        pool.cancel(&ViewId("v1".into()));
        CANCEL_GATE.store(true, Ordering::SeqCst);

        assert!(
            rx.recv_timeout(Duration::from_secs(2)).is_err(),
            "a cancelled query must deliver nothing, not an error"
        );
        pool.shutdown();
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
        pool.submit(request("v1", "select sum(v) as v from t"));
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
}
