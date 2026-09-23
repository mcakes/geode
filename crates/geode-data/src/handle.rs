//! The module-facing handle. `DataService` owns a DuckDB connection and is
//! not `Sync`, so it lives on one thread; this is the `Clone + Send + Sync`
//! handle to it. Nothing here blocks: every method
//! is a `try_send`, and a request that cannot be queued is refused and
//! counted rather than waited on — backpressure never stalls the UI.

use crate::service::{
    DataEvent, DataService, DataServiceConfig, EventSink, FetchParams, QueryParams,
};
use geode_core::config::{Diagnostic, Severity};
use geode_core::dimensions::DerivedDimensions;
use geode_core::pricing::{LocalPublish, PriceParams};
use geode_core::query::{
    CatalogParams, DistinctOutcome, DistinctParams, DocumentParams, QueryKey, QueryOutcome,
};
use geode_core::series::{SeriesOutcome, SeriesParams};
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
    /// The picker's distinct-values query (spec §3.4).
    Distinct(DistinctParams),
    /// The document request (market-data spec §7): one document by key,
    /// live or as-of — see `DataService::document`.
    Document(DocumentParams),
    /// The timeseries viewer's series query (timeseries spec §6): one
    /// round trip per tile, answered as `DataEvent::Series`.
    Series(SeriesParams),
    /// The diagnostics tile's "what does the database hold" request
    /// (Phase 4b §4.5). Answered synchronously on the service thread,
    /// not through the query pool — see `DataService::catalog`.
    Catalog(CatalogParams),
    /// The line pricer's batch (line-pricer spec §5.3), answered by the
    /// pricing worker as `DataEvent::Price`.
    Price(PriceParams),
    /// A document the app authored, published as a generation of a
    /// `local = true` dataset (spec §5.3, §7.2).
    Publish(LocalPublish),
    /// The timeseries viewer's on-demand fetch (timeseries spec §5.3):
    /// coverage is subtracted on the service thread and only the gaps
    /// reach the source's fetch worker; the outcome is
    /// `DataEvent::SeriesFetched`, keyed by the pair, never by `key`.
    Fetch(FetchParams),
    /// Ask a fetch source for its identities again; they land in the
    /// next `CatalogSnapshot::identities`.
    Identities {
        source: String,
    },
    Cancel {
        key: QueryKey,
    },
    /// Wake the service to apply its latest pending view configuration.
    ReplaceViews,
    Shutdown,
}

#[derive(Debug)]
struct ViewReplacement {
    views: Vec<ViewSpec>,
    dimensions: DerivedDimensions,
}

type PendingViews = Arc<Mutex<Option<ViewReplacement>>>;

struct Inner {
    pending_views: PendingViews,
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
    /// Blocks until the service thread has stopped — see
    /// [`DataHandle::shutdown`] for what that wait can cost (an
    /// in-flight `load_file` or `discover`). This runs on whatever
    /// thread drops the last `DataHandle`, so an app must not let that
    /// be the UI thread.
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

    /// Queue the picker's distinct-values query. `false` means it was not
    /// queued; the result, when it comes, arrives on the sink as
    /// `DataEvent::Distinct`, keyed and tagged as asked.
    pub fn distinct(&self, params: DistinctParams) -> bool {
        self.send(Request::Distinct(params))
    }

    /// Queue the document request (market-data spec §7). `false` means it
    /// was not queued; the result, when it comes, arrives on the sink as
    /// an ordinary `DataEvent::Query`, keyed and tagged as asked.
    pub fn document(&self, params: DocumentParams) -> bool {
        self.send(Request::Document(params))
    }

    /// Queue the timeseries viewer's series query (timeseries spec §6).
    /// `false` means it was not queued; the result, when it comes,
    /// arrives on the sink as `DataEvent::Series`, keyed and tagged as
    /// asked — a cap or compile refusal included, so the asking tile
    /// always hears back.
    pub fn series(&self, params: SeriesParams) -> bool {
        self.send(Request::Series(params))
    }

    /// Queue the diagnostics tile's catalog request (spec §4.5). `false`
    /// means it was not queued; the result, when it comes, arrives on
    /// the sink as `DataEvent::Catalog`, keyed and tagged as asked.
    pub fn catalog(&self, params: CatalogParams) -> bool {
        self.send(Request::Catalog(params))
    }

    /// Queue a pricing batch. `false` means the request channel refused
    /// it; a batch the worker's own queue refuses is answered with an
    /// error per line, so a tile never waits on a batch that will not
    /// come.
    pub fn price(&self, params: PriceParams) -> bool {
        self.send(Request::Price(params))
    }

    /// Queue a local publish. Refused (an error `Diagnostics` event,
    /// nothing written) unless the dataset is declared `local`.
    pub fn publish(&self, publish: LocalPublish) -> bool {
        self.send(Request::Publish(publish))
    }

    /// Queue an on-demand fetch (timeseries spec §5.3). `false` means it
    /// was not queued; the outcome, when it comes, arrives on the sink as
    /// `DataEvent::SeriesFetched` — keyed by the `(identity, source)`
    /// pair, not by `params.key`, so every tile watching that pair hears
    /// the one answer.
    pub fn fetch(&self, params: FetchParams) -> bool {
        self.send(Request::Fetch(params))
    }

    /// Ask a fetch source for its identities again (timeseries spec
    /// §5.5). `false` means the request was not queued; the answer lands
    /// in the next `CatalogSnapshot::identities`.
    pub fn identities(&self, source: impl Into<String>) -> bool {
        self.send(Request::Identities {
            source: source.into(),
        })
    }

    /// The safe hot-reload path for views (foundation §8). Diagnostics
    /// come back on the sink. Latest configuration wins even when the request
    /// queue is full; false means the service is gone, never temporary pressure.
    pub fn replace_views(&self, views: Vec<ViewSpec>, dimensions: DerivedDimensions) -> bool {
        let guard = self.inner.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let mut pending = self
            .inner
            .pending_views
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *pending = Some(ViewReplacement { views, dimensions });
        match tx.try_send(Request::ReplaceViews) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => {
                pending.take();
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Requests refused so far. A diagnostic, not a UI condition.
    pub fn dropped_requests(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Stop the service thread and wait for it. Idempotent; also runs
    /// when the last handle drops. Disconnecting the channel — not the
    /// queued `Shutdown` sentinel — is what guarantees the thread ends:
    /// see `Inner::stop`.
    ///
    /// **This blocks the calling thread until the service thread has
    /// actually stopped.** Once `serve`'s loop sees `Request::Shutdown`
    /// it calls `DataService::shutdown`, which joins the scheduler and
    /// ingest threads in turn — so this call waits out whatever either
    /// of them is doing right now: an in-flight `load_file` (seconds,
    /// for a large CSV) or an in-flight `discover` (unbounded, if the
    /// share it is scanning is hung). Call this — and let the last
    /// `DataHandle` drop — off the UI thread.
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
                    pending_views: Arc::default(),
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
        let pending_views = PendingViews::default();
        let service_views = Arc::clone(&pending_views);
        let thread = std::thread::Builder::new()
            .name("geode-data".into())
            .spawn(move || serve(config, sink, rx, service_views))
            .expect("spawning the data service thread");
        DataHandle {
            inner: Arc::new(Inner {
                pending_views,
                tx: Mutex::new(Some(tx)),
                thread: Mutex::new(Some(thread)),
                dropped: AtomicU64::new(0),
            }),
        }
    }
}

fn serve(
    config: DataServiceConfig,
    sink: EventSink,
    rx: Receiver<Request>,
    pending_views: PendingViews,
) {
    let mut service = match DataService::open(config, Arc::clone(&sink)) {
        Ok(s) => s,
        Err(e) => {
            sink(DataEvent::Diagnostics(vec![Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("data service failed to open: {e}"),
                path: None,
            }]));
            return;
        }
    };
    if !service.diagnostics().is_empty() {
        sink(DataEvent::Diagnostics(service.diagnostics().to_vec()));
    }

    while let Ok(req) = rx.recv() {
        // Check before every request: a full request queue is already a wakeup.
        // The latest configuration therefore cannot be lost behind a burst.
        let replacement = pending_views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(ViewReplacement { views, dimensions }) = replacement {
            let diags = service.replace_views(views, dimensions);
            if !diags.is_empty() {
                sink(DataEvent::Diagnostics(diags));
            }
        }
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
            Request::Distinct(params) => {
                if let Err(e) = service.distinct(&params) {
                    // Same rule as `Query`: a compile-time failure is
                    // this key's outcome, not a lost request (§10.1).
                    sink(DataEvent::Distinct(DistinctOutcome {
                        key: params.key,
                        tag: params.tag,
                        column: params.column,
                        values: Err(e.to_string()),
                    }));
                }
            }
            Request::Document(params) => {
                if let Err(e) = service.document(&params) {
                    // Same rule as `Query`: a compile-time failure is
                    // this key's outcome, not a lost request (§10.1).
                    // `Document` shares `Query`'s outcome shape (there is
                    // no `DataEvent::Document`), so a failed compile goes
                    // out as `DataEvent::Query` exactly as `Request::
                    // Query`'s own error arm does.
                    sink(DataEvent::Query(QueryOutcome {
                        key: params.key,
                        tag: params.tag,
                        snapshot: Err(e.to_string()),
                        submitted: params.submitted,
                    }));
                }
            }
            Request::Series(params) => {
                if let Err(e) = service.series(&params) {
                    // Same rule as `Query`: a cap or compile failure is
                    // this key's outcome, not a lost request.
                    sink(DataEvent::Series(SeriesOutcome {
                        key: params.key,
                        tag: params.tag,
                        submitted: params.submitted,
                        result: Err(e.to_string()),
                    }));
                }
            }
            Request::Catalog(params) => {
                sink(DataEvent::Catalog(service.catalog(&params)));
            }
            Request::Price(params) => service.price(params),
            Request::Publish(publish) => service.publish(publish),
            Request::Fetch(params) => service.fetch(&params),
            Request::Identities { source } => {
                if !service.identities(&source) {
                    tracing::warn!(target: "geode::ingest", "identities request for '{source}' refused");
                }
            }
            Request::Cancel { key } => service.cancel(key),
            Request::ReplaceViews => {}
            Request::Shutdown => break,
        }
    }
    service.shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::PricerConfig;
    use crate::query::as_of::AsOf;
    use crate::store::ddl::tests_support::{cvi_dataset, cvi_doc, local_dataset, sheet_rows, ts};
    use geode_core::scope::Scope;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    #[test]
    fn view_reload_survives_a_full_request_queue_and_keeps_the_latest() {
        let (handle, requests) = DataHandle::for_tests();
        for _ in 0..REQUEST_BOUND - 1 {
            assert!(handle.send(Request::Cancel { key: QueryKey(1) }));
        }
        assert!(handle.query(params(2, "reloaded")));
        assert!(handle.replace_views(Vec::new(), DerivedDimensions::default()));
        let mut view = crate::ingest::load::tests_support::tree_view();
        view.name = "reloaded".into();
        assert!(handle.replace_views(vec![view], DerivedDimensions::default()));
        assert_eq!(handle.dropped_requests(), 0);

        let (db, _src, store, ds, _emitted) = crate::ingest::load::tests_support::fixture();
        drop(store);
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(ds);
        let config = DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![crate::ingest::load::tests_support::tree_view()],
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: crate::adapter::AdapterRegistry::default(),
            documents: crate::documents::DocumentRegistry::default(),
            pricer: PricerConfig::default(),
        };
        let (tx, outcomes) = channel();
        let sink: EventSink = Arc::new(move |event| tx.send(event).is_ok());
        let pending = Arc::clone(&handle.inner.pending_views);
        // The service starts only after the queue filled and both reloads arrived.
        let service = std::thread::spawn(move || serve(config, sink, requests, pending));
        loop {
            if let DataEvent::Query(outcome) =
                outcomes.recv_timeout(Duration::from_secs(60)).unwrap()
            {
                assert_eq!(outcome.key, QueryKey(2));
                assert!(outcome.snapshot.is_ok(), "{:?}", outcome.snapshot);
                break;
            }
        }
        handle.shutdown();
        service.join().unwrap();
        assert!(!handle.replace_views(Vec::new(), DerivedDimensions::default()));
    }

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
    fn document_requests_are_forwarded_with_their_key() {
        let (handle, rx) = DataHandle::for_tests();
        assert!(handle.document(DocumentParams {
            key: QueryKey(5),
            tag: 1,
            submitted: Instant::now(),
            dataset: "cvi_params".into(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        }));
        match rx.recv().unwrap() {
            Request::Document(p) => assert_eq!(
                (p.key, p.document_key.as_slice()),
                (QueryKey(5), &["SPX.Z".to_string()][..])
            ),
            other => panic!("{other:?}"),
        }
    }

    fn distinct_params(key: u64, column: &str) -> DistinctParams {
        DistinctParams {
            key: QueryKey(key),
            tag: 1,
            column: column.to_string(),
            scope: Scope::default(),
            as_of: AsOf::Live,
        }
    }

    #[test]
    fn a_test_handle_hands_a_distinct_request_to_the_test() {
        let (h, rx) = DataHandle::for_tests();
        assert!(h.distinct(distinct_params(7, "book")));
        match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
            Request::Distinct(p) => {
                assert_eq!(p.key, QueryKey(7));
                assert_eq!(p.column, "book");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fetch_and_identities_are_queued_as_requests() {
        let (handle, rx) = DataHandle::for_tests();
        assert!(handle.fetch(FetchParams {
            key: QueryKey(3),
            source: "k".into(),
            identity: "SPX".into(),
            from: chrono::Utc::now(),
            to: chrono::Utc::now(),
        }));
        assert!(handle.identities("k"));
        assert!(matches!(rx.recv().unwrap(), Request::Fetch(p) if p.identity == "SPX"));
        assert!(matches!(rx.recv().unwrap(), Request::Identities { source } if source == "k"));
    }

    /// Timeseries spec §6: the series query rides the same queue as
    /// every other request, carrying its params untouched.
    #[test]
    fn a_series_request_is_queued_as_a_request() {
        use geode_core::series::{BucketRule, Frequency, SeriesParams, SeriesSpec, SlotKind};
        let (handle, rx) = DataHandle::for_tests();
        assert!(handle.series(SeriesParams {
            key: QueryKey(3),
            tag: 5,
            submitted: Instant::now(),
            dataset: "series".into(),
            range: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            window: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            as_of: AsOf::Live,
            frequency: Frequency::D1,
            series: vec![SeriesSpec {
                slot: 1,
                kind: SlotKind::Source {
                    source: "k".into(),
                    identity: "SPX".into(),
                    rule: BucketRule::Last,
                },
            }],
            percentiles: Vec::new(),
            bins: None,
        }));
        match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
            Request::Series(p) => {
                assert_eq!(p.key, QueryKey(3));
                assert_eq!(p.tag, 5);
                assert_eq!(p.dataset, "series");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_test_handle_hands_a_catalog_request_to_the_test() {
        let (h, rx) = DataHandle::for_tests();
        assert!(h.catalog(CatalogParams {
            key: QueryKey(9),
            tag: 3,
            as_of: AsOf::Live,
        }));
        match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
            Request::Catalog(p) => {
                assert_eq!(p.key, QueryKey(9));
                assert_eq!(p.tag, 3);
            }
            other => panic!("{other:?}"),
        }
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
                adapters: crate::adapter::AdapterRegistry::default(),
                documents: crate::documents::DocumentRegistry::default(),
                pricer: crate::pricing::PricerConfig::default(),
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
        assert_eq!(
            h.dropped_requests(),
            1,
            "the refused post-shutdown request is counted"
        );
    }

    #[test]
    fn a_document_compile_error_is_that_keys_outcome_on_the_real_service() {
        // Mirrors `the_real_service_answers_through_the_sink_and_reports_
        // open_failures`'s shape for `Request::Query`, for `Request::
        // Document`: a real service thread, a good request and a
        // compile-time failure, both addressed to the key that asked —
        // the failure arriving as `DataEvent::Query` since there is no
        // `DataEvent::Document`.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::catalog::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        crate::store::document::publish_document(
            &store,
            &crate::store::document::DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.]),
                source_time: ts("2026-09-12T14:00:00Z"),
                received_at: ts("2026-09-12T14:00:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        drop(store);
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(ds);

        let (tx, rx) = channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let h = DataService::spawn(
            DataServiceConfig {
                db_path: dir.path().join("geode.duckdb"),
                schema,
                views: Vec::new(),
                dimensions: geode_core::dimensions::DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
                adapters: crate::adapter::AdapterRegistry::default(),
                documents: crate::documents::DocumentRegistry::default(),
                pricer: crate::pricing::PricerConfig::default(),
            },
            sink,
        );
        assert!(h.document(DocumentParams {
            key: QueryKey(9),
            tag: 1,
            submitted: Instant::now(),
            dataset: "cvi_params".into(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        }));
        assert!(h.document(DocumentParams {
            key: QueryKey(10),
            tag: 1,
            submitted: Instant::now(),
            dataset: "nonesuch".into(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        }));
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
            "unknown dataset is an Err outcome"
        );
        h.shutdown();
    }

    #[test]
    fn a_catalog_request_reaches_the_sink_as_a_catalog_event() {
        // The full path: `DataHandle::catalog` -> `Request::Catalog` ->
        // `serve`'s request loop -> `DataService::catalog` -> the sink,
        // as `DataEvent::Catalog`, tag echoed.
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
                adapters: crate::adapter::AdapterRegistry::default(),
                documents: crate::documents::DocumentRegistry::default(),
                pricer: crate::pricing::PricerConfig::default(),
            },
            sink,
        );
        assert!(h.catalog(CatalogParams {
            key: QueryKey(21),
            tag: 21,
            as_of: AsOf::Live,
        }));
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Catalog(o) => {
                    assert_eq!(o.key, QueryKey(21));
                    assert_eq!(o.tag, 21);
                    let snap = o.snapshot.expect("catalog request failed");
                    assert_eq!(snap.datasets.len(), 1);
                    break;
                }
                _ => continue,
            }
        }
        h.shutdown();
    }

    #[test]
    fn price_and_publish_requests_reach_the_real_service() {
        let dir = tempfile::tempdir().unwrap();
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(local_dataset());
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig {
                db_path: dir.path().join("geode.duckdb"),
                schema,
                views: Vec::new(),
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
                adapters: Default::default(),
                documents: Default::default(),
                pricer: PricerConfig::missing("vendor"),
            },
            sink,
        );
        assert!(handle.price(crate::pricing::worker::tests::params(2, 9, &["SPX"])));
        assert!(handle.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: sheet_rows("a", &[7]),
        }));
        let mut priced = false;
        let mut published = false;
        while !(priced && published) {
            match rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap() {
                DataEvent::Price(o) => {
                    assert_eq!((o.key, o.tag), (QueryKey(2), 9));
                    assert_eq!(
                        o.results[0].2.as_ref().unwrap_err(),
                        "pricer \"vendor\" is not built into this binary"
                    );
                    priced = true;
                }
                DataEvent::Published { dataset, .. } if dataset == "sheets" => published = true,
                _ => {}
            }
        }
        handle.shutdown();
        assert!(
            !handle.price(crate::pricing::worker::tests::params(2, 10, &["SPX"])),
            "refused after shutdown"
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
                adapters: crate::adapter::AdapterRegistry::default(),
                documents: crate::documents::DocumentRegistry::default(),
                pricer: crate::pricing::PricerConfig::default(),
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
                adapters: crate::adapter::AdapterRegistry::default(),
                documents: crate::documents::DocumentRegistry::default(),
                pricer: crate::pricing::PricerConfig::default(),
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
