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
    Cancel {
        key: QueryKey,
    },
    ReplaceViews {
        views: Vec<ViewSpec>,
        dimensions: DerivedDimensions,
    },
    Shutdown,
}

struct Inner {
    tx: Mutex<Option<SyncSender<Request>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    dropped: AtomicU64,
}

impl Inner {
    fn send(&self, req: Request) -> bool {
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(tx) => match tx.try_send(req) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    false
                }
            },
            None => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Stop accepting requests and end the service thread. A queued
    /// `Shutdown` sentinel is not enough on its own: if the channel is
    /// full the sentinel is refused, and `serve`'s blocking `rx.recv()`
    /// would then wait forever for a request that never lands. What
    /// actually guarantees the thread ends is dropping the sender —
    /// `recv()` returns `Err` once the channel is empty and
    /// disconnected, regardless of how full it was a moment before —
    /// so the sender is taken out of the `Option` first (every later
    /// `send` sees `None` and refuses), a `Shutdown` is offered
    /// best-effort so a healthy loop can exit its `match` promptly
    /// rather than via a wasted `recv` error path, and only then is the
    /// sender dropped and the thread joined.
    fn stop(&self) {
        let taken = self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(tx) = taken {
            let _ = tx.try_send(Request::Shutdown);
            drop(tx);
        }
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone)]
pub struct DataHandle {
    inner: Arc<Inner>,
}

impl DataHandle {
    fn send(&self, req: Request) -> bool {
        self.inner.send(req)
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
    /// when the last handle drops. Disconnecting the channel — not the
    /// queued `Shutdown` sentinel — is what guarantees the thread ends:
    /// see `Inner::stop`.
    pub fn shutdown(&self) {
        self.inner.stop();
    }

    /// A handle with no service behind it: the test is the service, and
    /// reads what a module asked for.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests() -> (DataHandle, Receiver<Request>) {
        let (tx, rx) = sync_channel(REQUEST_BOUND);
        (
            DataHandle {
                inner: Arc::new(Inner {
                    tx: Mutex::new(Some(tx)),
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
                tx: Mutex::new(Some(tx)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::as_of::AsOf;
    use geode_core::scope::Scope;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    fn params(key: u64, view: &str) -> QueryParams {
        QueryParams {
            key: QueryKey(key),
            tag: 1,
            submitted: Instant::now(),
            view: view.to_string(),
            grouping: None,
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
            if let DataEvent::Query(o) = rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                got.insert(o.key, o.snapshot.is_ok());
            }
        }
        assert_eq!(got.get(&QueryKey(9)), Some(&true));
        assert_eq!(
            got.get(&QueryKey(10)),
            Some(&false),
            "unknown view is an Err outcome"
        );
        h.shutdown();
        assert!(
            !h.query(params(11, "tree")),
            "after shutdown nothing is accepted"
        );
    }

    #[test]
    fn shutdown_completes_even_when_the_request_queue_is_full() {
        // Reproduces: shutdown must disconnect the channel, not merely
        // enqueue a Shutdown sentinel — a full queue drops that sentinel
        // and `serve`'s `rx.recv()` then blocks forever, hanging `join`.
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

        let (tx, _rx) = channel();
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

        // Fill the queue immediately, before the service can have
        // finished opening — some of these will be refused, which is
        // fine; the point is the queue is full.
        for _ in 0..(REQUEST_BOUND + 8) {
            h.cancel(QueryKey(1));
        }

        let (done_tx, done_rx) = channel();
        let h2 = h.clone();
        std::thread::spawn(move || {
            h2.shutdown();
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("shutdown must not hang on a full queue");
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
