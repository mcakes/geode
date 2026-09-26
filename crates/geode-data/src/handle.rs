//! Cloneable request handle for the service thread. Ordinary submissions use
//! try_send under a short mutex: full or disconnected channels refuse and count
//! the request without waiting for queue space. Acceptance is queue admission,
//! not completion. Cancellation and supersession can suppress query outcomes.
//!
//! View replacements use a latest-value mailbox with a best-effort wakeup.
//! Shutdown and final-handle drop join the service and can block; run those off
//! the UI thread. See `docs/current/request-delivery.md`.

use crate::egress::UploadParams;
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

/// Maximum waiting requests on the service channel. This bound includes
/// queries, cancellation, and other ordinary requests; it does not bound work
/// already handed to downstream workers.
pub const REQUEST_BOUND: usize = 64;

#[derive(Debug)]
pub enum Request {
    Query(QueryParams),
    /// Picker distinct-values request, answered with DataEvent::Distinct.
    Distinct(DistinctParams),
    /// One document by key, live or as-of, answered with DataEvent::Query.
    Document(DocumentParams),
    /// Series query, answered with DataEvent::Series.
    Series(SeriesParams),
    /// Catalog metadata request, answered synchronously on the service reader
    /// rather than through the query pool.
    Catalog(CatalogParams),
    /// Pricing batch, answered by the pricing worker with DataEvent::Price.
    Price(PriceParams),
    /// App-authored document for a dataset declared local.
    Publish(LocalPublish),
    /// Document upload to an egress target, answered with DataEvent::Upload
    /// from the service thread (a refusal) or the target's worker.
    Upload(UploadParams),
    /// History fetch. The service subtracts committed coverage and submits gaps
    /// to the source's fetch worker. Completion uses the identity/source pair,
    /// not the requester's key.
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

    /// Close admission before joining. Offer Shutdown best-effort, then drop the
    /// sender so even a full queue eventually disconnects after queued requests
    /// are dispatched. Downstream shutdown can still wait on running I/O.
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

    /// Queue a query. False means no request was admitted and no reply is owed.
    /// Outcomes preserve key/tag; supersession or cancellation can suppress them.
    pub fn query(&self, params: QueryParams) -> bool {
        self.send(Request::Query(params))
    }

    /// Queue cancellation for query-pool and pricing work under this key.
    /// False means cancellation was not queued. There is no acknowledgement;
    /// this does not cancel fetches or ingest jobs, or retract emitted results.
    pub fn cancel(&self, key: QueryKey) -> bool {
        self.send(Request::Cancel { key })
    }

    /// Queue the picker's distinct-values query. `false` means it was not
    /// queued; the result, when it comes, arrives on the sink as
    /// `DataEvent::Distinct`, keyed and tagged as asked.
    pub fn distinct(&self, params: DistinctParams) -> bool {
        self.send(Request::Distinct(params))
    }

    /// Queue a document request. False means not queued. Outcomes share the
    /// DataEvent::Query shape and preserve the request key/tag.
    pub fn document(&self, params: DocumentParams) -> bool {
        self.send(Request::Document(params))
    }

    /// Queue an upload. False means no request was admitted and no outcome is
    /// owed; the caller reports the refusal. Serviced uploads normally emit one
    /// `DataEvent::Upload`; startup, worker, and event-delivery failures can
    /// prevent that outcome. Admission does not acknowledge transport success.
    pub fn upload(&self, params: UploadParams) -> bool {
        self.send(Request::Upload(params))
    }

    /// Queue a series query. False means not queued. Cap and compile failures
    /// for admitted requests are returned as keyed/tagged DataEvent::Series errors;
    /// superseded or cancelled work can produce no outcome.
    pub fn series(&self, params: SeriesParams) -> bool {
        self.send(Request::Series(params))
    }

    /// Queue catalog metadata work. False means not queued; outcomes preserve
    /// key/tag in DataEvent::Catalog.
    pub fn catalog(&self, params: CatalogParams) -> bool {
        self.send(Request::Catalog(params))
    }

    /// Queue a pricing batch. False means the service channel refused it. A
    /// subsequent pricing-worker refusal instead produces an error for each line.
    pub fn price(&self, params: PriceParams) -> bool {
        self.send(Request::Price(params))
    }

    /// Queue local publication. False means no admission. After admission the
    /// service validates local-dataset permission and reports rejection through
    /// Diagnostics; true does not mean the document has been stored.
    pub fn publish(&self, publish: LocalPublish) -> bool {
        self.send(Request::Publish(publish))
    }

    /// Queue a fetch. False means not queued. SeriesFetched identifies the
    /// identity/source pair so all visible tiles watching it can react, including
    /// when completion appended zero rows.
    pub fn fetch(&self, params: FetchParams) -> bool {
        self.send(Request::Fetch(params))
    }

    /// Queue an identity refresh. False means no admission to the service queue.
    /// Worker refusal is logged; there is no dedicated completion event. Successful
    /// enumeration updates the identities returned by a later catalog request.
    pub fn identities(&self, source: impl Into<String>) -> bool {
        self.send(Request::Identities {
            source: source.into(),
        })
    }

    /// Store the latest views and dimensions, replacing any pending replacement.
    /// A full wakeup queue still returns true: the service checks the mailbox
    /// before every dequeued request. False means closed admission or a disconnected
    /// service. True acknowledges retained state, not validation or application;
    /// validation diagnostics return through the event sink.
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

    /// Close request admission and join the service. Idempotent; final-handle
    /// drop does the same. Already admitted requests precede Shutdown, or drain
    /// until sender disconnection if that sentinel could not be queued.
    ///
    /// Joining waits for service open, request dispatch, and downstream worker
    /// shutdown. Fetch calls, discovery, and publication can delay it indefinitely
    /// if their I/O does not return. This is not a storage flush guarantee; ingest
    /// shutdown does not drain its queued jobs. Call off the UI thread.
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
    /// Spawn the service without waiting for database open/schema setup. An open
    /// error is reported through Diagnostics, then the request receiver closes.
    /// Requests admitted before that failure have no individual outcomes. Failure
    /// to spawn the thread itself panics at the expect below.
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
                    // Return compile/validation failure with the original request key and tag.
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
                    // Return distinct-query compilation failure to the original requester.
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
                    // Document failures use DataEvent::Query, just like successful document
                    // results, with the original request key/tag.
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
            Request::Upload(params) => service.upload(params),
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
            egress: Vec::new(),
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

    fn upload_params(tag: u64) -> UploadParams {
        UploadParams {
            key: QueryKey(9),
            tag,
            target: "sophis".into(),
            document: "dividend_schedule".into(),
            rows: geode_core::document::DocumentRows {
                key: vec!["XYZ".into()],
                attributes: Vec::new(),
                axes: Vec::new(),
                values: Vec::new(),
            },
        }
    }

    #[test]
    fn upload_is_refused_when_the_request_channel_is_full() {
        let (handle, rx) = DataHandle::for_tests();
        for _ in 0..REQUEST_BOUND {
            assert!(handle.cancel(QueryKey(1)));
        }
        assert!(!handle.upload(upload_params(1)));
        assert_eq!(handle.dropped_requests(), 1);
        // The admitted requests are the cancels; the refused upload left nothing.
        let queued: Vec<Request> = rx.try_iter().collect();
        assert_eq!(queued.len(), REQUEST_BOUND);
        assert!(queued.iter().all(|r| matches!(r, Request::Cancel { .. })));
    }

    /// The production route: `DataHandle::upload` through the service
    /// thread's `serve` loop to the target's worker and back as one
    /// `DataEvent::Upload`, with the bytes on the bus.
    #[test]
    fn an_upload_request_is_served_to_its_target_and_answered() {
        struct KeyKind;
        impl geode_core::document::DocumentKind for KeyKind {
            fn name(&self) -> &'static str {
                "dividend_schedule"
            }
            fn columns(&self) -> &[(&'static str, geode_core::schema::ColumnType)] {
                &[]
            }
            fn parse(
                &self,
                _bytes: &[u8],
            ) -> Result<geode_core::document::ParsedDocument, geode_core::document::ParseError>
            {
                unreachable!("uploads never parse")
            }
            fn write(
                &self,
                rows: &geode_core::document::DocumentRows,
            ) -> Result<Vec<u8>, geode_core::document::WriteError> {
                Ok(rows.key.join("/").into_bytes())
            }
        }
        use crate::adapter::{Adapter, MessageSink};
        let (adapter, _feed) = crate::adapter::ChannelAdapter::new("demo_bus");
        let (bus_sink, bus_rx) = MessageSink::bounded(8);
        let mut sub = adapter.subscription().unwrap();
        sub.subscribe(
            &["marketdata/dividend/>".into()],
            bus_sink,
            Arc::new(|_| {}),
        )
        .unwrap();
        let mut adapters = crate::adapter::AdapterRegistry::default();
        adapters.register(adapter.clone());
        let mut documents = crate::documents::DocumentRegistry::default();
        documents.register(Arc::new(KeyKind));
        let db = tempfile::tempdir().unwrap();
        let (tx, events) = channel();
        let sink: EventSink = Arc::new(move |event| tx.send(event).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig {
                db_path: db.path().join("geode.duckdb"),
                schema: geode_core::schema::SchemaSpec::default(),
                views: Vec::new(),
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
                adapters,
                documents,
                egress: vec![geode_core::egress_config::EgressSpec {
                    name: "sophis".into(),
                    adapter: "demo_bus".into(),
                    documents: vec![(
                        "dividend_schedule".into(),
                        "marketdata/dividend/{key}".into(),
                    )],
                }],
                pricer: PricerConfig::default(),
            },
            sink,
        );

        assert!(handle.upload(upload_params(4)));

        let outcome = loop {
            match events.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Upload(outcome) => break outcome,
                DataEvent::Diagnostics(d) => panic!("{d:?}"),
                _ => {}
            }
        };
        assert_eq!(
            outcome,
            crate::egress::UploadOutcome {
                key: QueryKey(9),
                tag: 4,
                target: "sophis".into(),
                result: Ok(()),
            }
        );
        let m = bus_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            (m.topic.as_str(), m.bytes.as_slice()),
            ("marketdata/dividend/XYZ", &b"XYZ"[..])
        );
        handle.shutdown();
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

    /// Series parameters must pass unchanged through the common request queue.
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
        // Fill the bounded channel without draining it to verify refusal and its
        // counter without relying on service-thread timing.
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
                egress: Vec::new(),
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
                egress: Vec::new(),
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
                egress: Vec::new(),
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
                egress: Vec::new(),
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
        // Shutdown must drop the sender even when the queue refuses its sentinel.
        // Otherwise the service can drain the queue and then wait forever in recv.
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
                egress: Vec::new(),
                pricer: crate::pricing::PricerConfig::default(),
            },
            sink,
        );

        // Submit a burst during startup to exercise shutdown under request
        // pressure. How many offers are refused depends on thread scheduling.
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
                egress: Vec::new(),
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
