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
    shutdown: bool,
}

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
        let (tx, rx) = channel();
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let mut threads = Vec::new();

        for i in 0..workers.max(1) {
            let conn = store.reader()?;
            let q = Arc::clone(&queue);
            let tx = tx.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(format!("geode-query-{i}"))
                    .spawn(move || worker(conn, q, tx))
                    .expect("spawning a query worker"),
            );
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
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
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
        if let Some((_, handle)) = q.running.get(view) {
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

fn worker(conn: duckdb::Connection, queue: Arc<(Mutex<Queue>, Condvar)>, tx: Sender<QueryResult>) {
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

        let outcome = run_one(&conn, &req).map_err(|e| e.to_string());

        let stale = {
            let (lock, _) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            if q.running.get(&req.view).is_some_and(|(rid, _)| *rid == id) {
                q.running.remove(&req.view);
            }
            // A newer request for this view arrived while we ran: our
            // result is stale, so drop it rather than delivering it
            // out of order (spec §7.3).
            q.pending.get(&req.view).is_some_and(|(pid, _)| *pid > id)
        };

        if stale {
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

    #[test]
    fn shutdown_is_idempotent_and_does_not_hang() {
        let (_d, store) = fixture(100);
        let (pool, _rx) = QueryPool::spawn(&store, 2).unwrap();
        pool.shutdown();
        pool.shutdown();
    }
}
