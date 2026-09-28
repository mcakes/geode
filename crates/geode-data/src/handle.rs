//! Cloneable request handle for the service thread. Ordinary submissions use
//! try_send under a short mutex and never wait for queue space: a full channel
//! refuses `Busy` and is counted; a request loop that has stopped, or a closed
//! or disconnected channel, refuses `Stopped`. Acceptance is queue admission,
//! not completion. Cancellation and supersession can suppress query outcomes.
//!
//! View replacements use a latest-value mailbox with a best-effort wakeup.
//! Shutdown and final-handle drop join the service and can block; run those off
//! the UI thread. See `docs/current/request-delivery.md`.

use crate::egress::{UploadOutcome, UploadParams};
use crate::service::{
    DataEvent, DataService, DataServiceConfig, EventSink, FetchParams, LocalForget, QueryParams,
};
use crate::supervise::REQUEST_LOOP;
use geode_core::config::{Diagnostic, Severity};
use geode_core::dimensions::DerivedDimensions;
use geode_core::pricing::{LocalPublish, PriceOutcome, PriceParams};
use geode_core::query::{
    CatalogOutcome, CatalogParams, DistinctOutcome, DistinctParams, DocumentParams, QueryKey,
    QueryOutcome,
};
use geode_core::series::{SeriesOutcome, SeriesParams};
use geode_core::view::ViewSpec;
use geode_core::vol::{VolSliceOutcome, VolSliceParams};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// Maximum waiting requests on the service channel. This bound includes
/// queries, cancellation, and other ordinary requests; it does not bound work
/// already handed to downstream workers.
pub const REQUEST_BOUND: usize = 64;

/// Why a submission was not admitted. `Busy` passes: the request queue was
/// full and a later submission can succeed. `Stopped` does not: the request
/// loop has ended (a panic, a failed open, or shutdown) and nothing will serve
/// a later one, so retrying only repeats the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Busy,
    Stopped,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::Busy => "the data service is busy",
            Refusal::Stopped => "the data service has stopped",
        })
    }
}

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
    /// Vol slice batch, answered by the vol worker with DataEvent::VolSlices.
    VolSlices(VolSliceParams),
    /// App-authored document for a dataset declared local.
    Publish(LocalPublish),
    /// Delete one local document's whole history, answered with
    /// DataEvent::Forgotten or DataEvent::ForgetFailed.
    Forget(LocalForget),
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
    /// Set when the request loop can no longer serve: it failed to open, or
    /// it is unwinding. Read before the channel so a submission racing a
    /// dying loop is refused `Stopped` rather than admitted to a queue that
    /// nothing will read. A deliberate shutdown does not set it.
    stopped: Arc<AtomicBool>,
}

impl Inner {
    fn send(&self, req: Request) -> Result<(), Refusal> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Refusal::Stopped);
        }
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(tx) => match tx.try_send(req) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    Err(Refusal::Busy)
                }
                Err(TrySendError::Disconnected(_)) => Err(Refusal::Stopped),
            },
            None => Err(Refusal::Stopped),
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
    fn send(&self, req: Request) -> Result<(), Refusal> {
        self.inner.send(req)
    }

    /// Queue a query. `Err(Busy)` means the queue was full and a later
    /// submission can succeed; `Err(Stopped)` means the service can no longer
    /// serve and no outcome is owed. Outcomes preserve key/tag; supersession
    /// or cancellation can suppress them.
    pub fn query(&self, params: QueryParams) -> Result<(), Refusal> {
        self.send(Request::Query(params))
    }

    /// Queue cancellation for query-pool and pricing work under this key.
    /// `false` means it was not queued; a stale answer that still arrives is
    /// dropped by its receiver's tag check. There is no acknowledgement; this
    /// does not cancel fetches or ingest jobs, or retract emitted results.
    pub fn cancel(&self, key: QueryKey) -> bool {
        self.send(Request::Cancel { key }).is_ok()
    }

    /// Queue the picker's distinct-values query. `Err(Busy)` means the queue
    /// was full and a later submission can succeed; `Err(Stopped)` means the
    /// service can no longer serve and no outcome is owed. The result, when it
    /// comes, arrives on the sink as `DataEvent::Distinct`, keyed and tagged
    /// as asked.
    pub fn distinct(&self, params: DistinctParams) -> Result<(), Refusal> {
        self.send(Request::Distinct(params))
    }

    /// Queue a document request. `Err(Busy)` means the queue was full and a
    /// later submission can succeed; `Err(Stopped)` means the service can no
    /// longer serve and no outcome is owed. Outcomes share the
    /// DataEvent::Query shape and preserve the request key/tag.
    pub fn document(&self, params: DocumentParams) -> Result<(), Refusal> {
        self.send(Request::Document(params))
    }

    /// Queue an upload. `Err(Busy)` means the queue was full and a later
    /// submission can succeed; `Err(Stopped)` means the service can no longer
    /// serve and no outcome is owed. Either way the caller reports the
    /// refusal. Serviced uploads normally emit one
    /// `DataEvent::Upload`; startup, worker, and event-delivery failures can
    /// prevent that outcome. Admission does not acknowledge transport success.
    pub fn upload(&self, params: UploadParams) -> Result<(), Refusal> {
        self.send(Request::Upload(params))
    }

    /// Queue a series query. `Err(Busy)` means the queue was full and a later
    /// submission can succeed; `Err(Stopped)` means the service can no longer
    /// serve and no outcome is owed. Cap and compile failures
    /// for admitted requests are returned as keyed/tagged DataEvent::Series errors;
    /// superseded or cancelled work can produce no outcome.
    pub fn series(&self, params: SeriesParams) -> Result<(), Refusal> {
        self.send(Request::Series(params))
    }

    /// Queue catalog metadata work. `Err(Busy)` means the queue was full and
    /// a later submission can succeed; `Err(Stopped)` means the service can no
    /// longer serve and no outcome is owed. Outcomes preserve key/tag in
    /// DataEvent::Catalog.
    pub fn catalog(&self, params: CatalogParams) -> Result<(), Refusal> {
        self.send(Request::Catalog(params))
    }

    /// Queue a pricing batch. `Err(Busy)` means the queue was full and a later
    /// submission can succeed; `Err(Stopped)` means the service can no longer
    /// serve and no outcome is owed. A subsequent pricing-worker refusal
    /// instead produces an error for each line.
    pub fn price(&self, params: PriceParams) -> Result<(), Refusal> {
        self.send(Request::Price(params))
    }

    /// Queue a vol slice batch. `Err(Busy)` means the queue was full and a
    /// later submission can succeed; `Err(Stopped)` means the service can no
    /// longer serve and no outcome is owed. A subsequent vol-worker refusal
    /// instead produces an error for each job.
    pub fn vol_slices(&self, params: VolSliceParams) -> Result<(), Refusal> {
        self.send(Request::VolSlices(params))
    }

    /// Queue local publication. `Err(Busy)` means the queue was full and a
    /// later submission can succeed; `Err(Stopped)` means the service can no
    /// longer serve and no outcome is owed. After admission the
    /// service validates local-dataset permission and reports rejection through
    /// Diagnostics and LocalPublishFailed; otherwise the writer answers
    /// LocalPublished or LocalPublishFailed. `Ok` does not mean the document
    /// has been stored.
    pub fn publish(&self, publish: LocalPublish) -> Result<(), Refusal> {
        self.send(Request::Publish(publish))
    }

    /// Queue forgetting one local document. `Err(Busy)` means the queue was
    /// full and a later submission can succeed; `Err(Stopped)` means the
    /// service can no longer serve and no outcome is owed. After
    /// admission the service refuses a dataset that is not local (or a key of
    /// the wrong arity) with an error Diagnostics and ForgetFailed and runs
    /// nothing; otherwise the forget runs on the writer after every publish
    /// queued before it and answers Forgotten or ForgetFailed. `Ok` does not
    /// mean it has run.
    pub fn forget(&self, forget: LocalForget) -> Result<(), Refusal> {
        self.send(Request::Forget(forget))
    }

    /// Queue a fetch. `Err(Busy)` means the queue was full and a later
    /// submission can succeed; `Err(Stopped)` means the service can no longer
    /// serve and no outcome is owed. SeriesFetched identifies the
    /// identity/source pair so all visible tiles watching it can react, including
    /// when completion appended zero rows.
    pub fn fetch(&self, params: FetchParams) -> Result<(), Refusal> {
        self.send(Request::Fetch(params))
    }

    /// Queue an identity refresh. `Err(Busy)` means the queue was full and a
    /// later submission can succeed; `Err(Stopped)` means the service can no
    /// longer serve and no outcome is owed.
    /// Worker refusal is logged; there is no dedicated completion event. Successful
    /// enumeration updates the identities returned by a later catalog request.
    pub fn identities(&self, source: impl Into<String>) -> Result<(), Refusal> {
        self.send(Request::Identities {
            source: source.into(),
        })
    }

    /// Store the latest views and dimensions, replacing any pending replacement.
    /// A full wakeup queue still answers `Ok`: the service checks the mailbox
    /// before every dequeued request, so the only refusal is `Stopped`. `Ok`
    /// acknowledges retained state, not validation or application; validation
    /// diagnostics return through the event sink.
    pub fn replace_views(
        &self,
        views: Vec<ViewSpec>,
        dimensions: DerivedDimensions,
    ) -> Result<(), Refusal> {
        if self.inner.stopped.load(Ordering::Acquire) {
            return Err(Refusal::Stopped);
        }
        let guard = self.inner.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else {
            return Err(Refusal::Stopped);
        };
        let mut pending = self
            .inner
            .pending_views
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *pending = Some(ViewReplacement { views, dimensions });
        match tx.try_send(Request::ReplaceViews) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                pending.take();
                Err(Refusal::Stopped)
            }
        }
    }

    /// Submissions refused `Busy` so far (a full request queue). `Stopped`
    /// refusals are not counted: they describe a service that is gone, not one
    /// that is behind.
    pub fn dropped_requests(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Close request admission and join the service. Idempotent; final-handle
    /// drop does the same. Already admitted requests precede Shutdown, or drain
    /// until sender disconnection if that sentinel could not be queued.
    ///
    /// Joining waits for service open, request dispatch, and downstream worker
    /// shutdown. Fetch calls, discovery, and publication can delay it indefinitely
    /// if their I/O does not return. Ingest shutdown runs the queued local writes
    /// (the app's own publishes and forgets), in order and each answering as usual,
    /// then drops every other queued job. It is not a flush guarantee beyond that:
    /// a caller that stops waiting (gpui's quit hook waits at most 200 ms) can exit
    /// while a write is still running. Call off the UI thread.
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
                    stopped: Arc::default(),
                }),
            },
            rx,
        )
    }

    /// Fill the request queue so the next submission is refused `Busy`, the
    /// refusal a burst produces. The test holding the paired receiver drains
    /// it to admit again. Expects a live, undrained `for_tests` handle: the
    /// loop ends only when `cancel` is refused, which a closed receiver does
    /// at once and a receiver drained concurrently may never do.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fill_for_tests(&self) {
        while self.cancel(QueryKey(u64::MAX)) {}
    }
}

impl DataService {
    /// Spawn the service without waiting for database open/schema setup. An open
    /// error is reported through Diagnostics, then the request receiver closes.
    /// Requests admitted before that failure have no individual outcomes. Failure
    /// to spawn the thread itself panics at the expect below.
    pub fn spawn(config: DataServiceConfig, sink: EventSink) -> DataHandle {
        Self::spawn_with_probe(config, sink, no_probe)
    }

    /// [`DataService::spawn`] with a probe `serve` calls at each
    /// [`ServePoint`]; production passes [`no_probe`].
    fn spawn_with_probe(config: DataServiceConfig, sink: EventSink, probe: Probe) -> DataHandle {
        let (tx, rx) = sync_channel(REQUEST_BOUND);
        let pending_views = PendingViews::default();
        let service_views = Arc::clone(&pending_views);
        let stopped = Arc::new(AtomicBool::new(false));
        let loop_stopped = Arc::clone(&stopped);
        let loop_sink = Arc::clone(&sink);
        let thread =
            crate::supervise::spawn_supervised(REQUEST_LOOP.to_string(), sink, move || {
                serve(config, loop_sink, rx, service_views, loop_stopped, probe)
            })
            .expect("spawning the data service thread");
        DataHandle {
            inner: Arc::new(Inner {
                pending_views,
                tx: Mutex::new(Some(tx)),
                thread: Mutex::new(Some(thread)),
                dropped: AtomicU64::new(0),
                stopped,
            }),
        }
    }
}

/// Where `serve` calls its probe. Production passes [`no_probe`]; a test
/// passes one that panics at a chosen point, for the arms and steps that no
/// production input can panic.
#[derive(Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
enum ServePoint<'a> {
    /// Inside the view-replacement boundary, before the replacement runs.
    Views,
    /// Inside a request's boundary, before its arm runs.
    Arm(&'a Request),
    /// Outside every boundary, just after a request is received.
    Loop(&'a Request),
}

type Probe = fn(ServePoint<'_>);

fn no_probe(_: ServePoint<'_>) {}

fn error_diagnostic(message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message,
        path: None,
    }
}

/// Declares the request loop stopped when it is dropped by an unwind.
/// Declared after the service in `serve`, so it drops first: the flag is set
/// before the dying loop joins its workers, which can take as long as the
/// slowest running job, and a submission in that window is refused
/// `Stopped` rather than admitted to a queue nothing will read.
struct StoppedOnUnwind(Arc<AtomicBool>);

impl Drop for StoppedOnUnwind {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(true, Ordering::Release);
        }
    }
}

/// Who a request's answer goes to, taken before its arm runs, so a panicking
/// arm is still answered exactly once through the door that answers its
/// success. A tile never waits on a request the loop swallowed.
enum PanicAnswer {
    Query {
        key: QueryKey,
        tag: u64,
        submitted: Instant,
    },
    Distinct {
        key: QueryKey,
        tag: u64,
        column: String,
    },
    Series {
        key: QueryKey,
        tag: u64,
        submitted: Instant,
    },
    Catalog {
        key: QueryKey,
        tag: u64,
    },
    Price {
        key: QueryKey,
        tag: u64,
        submitted: Instant,
        lines: Vec<(u64, u64)>,
    },
    VolSlices {
        key: QueryKey,
        tag: u64,
        submitted: Instant,
        jobs: usize,
    },
    Upload {
        key: QueryKey,
        tag: u64,
        target: String,
    },
    Fetch {
        source: String,
        identity: String,
    },
    Publish {
        dataset: String,
        batch: String,
    },
    Forget {
        dataset: String,
        batch: String,
    },
    /// Identities, cancel and view replacement have no answer path.
    Unanswered,
}

impl PanicAnswer {
    /// The request's kind, for the message, and its answer.
    fn of(req: &Request) -> (&'static str, PanicAnswer) {
        match req {
            Request::Query(p) => (
                "query",
                PanicAnswer::Query {
                    key: p.key,
                    tag: p.tag,
                    submitted: p.submitted,
                },
            ),
            Request::Document(p) => (
                "document",
                PanicAnswer::Query {
                    key: p.key,
                    tag: p.tag,
                    submitted: p.submitted,
                },
            ),
            Request::Distinct(p) => (
                "distinct",
                PanicAnswer::Distinct {
                    key: p.key,
                    tag: p.tag,
                    column: p.column.clone(),
                },
            ),
            Request::Series(p) => (
                "series",
                PanicAnswer::Series {
                    key: p.key,
                    tag: p.tag,
                    submitted: p.submitted,
                },
            ),
            Request::Catalog(p) => (
                "catalog",
                PanicAnswer::Catalog {
                    key: p.key,
                    tag: p.tag,
                },
            ),
            Request::Price(p) => (
                "price",
                PanicAnswer::Price {
                    key: p.key,
                    tag: p.tag,
                    submitted: p.submitted,
                    lines: p.lines.iter().map(|l| (l.id, l.revision)).collect(),
                },
            ),
            Request::VolSlices(p) => (
                "vol_slices",
                PanicAnswer::VolSlices {
                    key: p.key,
                    tag: p.tag,
                    submitted: p.submitted,
                    jobs: p.jobs.len(),
                },
            ),
            Request::Upload(p) => (
                "upload",
                PanicAnswer::Upload {
                    key: p.key,
                    tag: p.tag,
                    target: p.target.clone(),
                },
            ),
            Request::Fetch(p) => (
                "fetch",
                PanicAnswer::Fetch {
                    source: p.source.clone(),
                    identity: p.identity.clone(),
                },
            ),
            Request::Publish(p) => (
                "publish",
                PanicAnswer::Publish {
                    dataset: p.dataset.clone(),
                    batch: geode_core::document::join_key(&p.rows.key),
                },
            ),
            Request::Forget(f) => (
                "forget",
                PanicAnswer::Forget {
                    dataset: f.dataset.clone(),
                    batch: geode_core::document::join_key(&f.key),
                },
            ),
            Request::Identities { .. } => ("identities", PanicAnswer::Unanswered),
            Request::Cancel { .. } => ("cancel", PanicAnswer::Unanswered),
            Request::ReplaceViews => ("view replacement", PanicAnswer::Unanswered),
            Request::Shutdown => ("shutdown", PanicAnswer::Unanswered),
        }
    }

    fn answer(self, service: &DataService, sink: &EventSink, reason: String) {
        match self {
            PanicAnswer::Query {
                key,
                tag,
                submitted,
            } => {
                let _ = sink(DataEvent::Query(QueryOutcome {
                    key,
                    tag,
                    snapshot: Err(reason),
                    submitted,
                }));
            }
            PanicAnswer::Distinct { key, tag, column } => {
                let _ = sink(DataEvent::Distinct(DistinctOutcome {
                    key,
                    tag,
                    column,
                    values: Err(reason),
                }));
            }
            PanicAnswer::Series {
                key,
                tag,
                submitted,
            } => {
                let _ = sink(DataEvent::Series(SeriesOutcome {
                    key,
                    tag,
                    submitted,
                    result: Err(reason),
                }));
            }
            PanicAnswer::Catalog { key, tag } => {
                let _ = sink(DataEvent::Catalog(CatalogOutcome {
                    key,
                    tag,
                    snapshot: Err(reason),
                }));
            }
            PanicAnswer::Price {
                key,
                tag,
                submitted,
                lines,
            } => {
                let results = lines
                    .into_iter()
                    .map(|(id, revision)| (id, revision, Err(reason.clone())))
                    .collect();
                let _ = sink(DataEvent::Price(PriceOutcome {
                    key,
                    tag,
                    submitted,
                    results,
                }));
            }
            PanicAnswer::VolSlices {
                key,
                tag,
                submitted,
                jobs,
            } => {
                let _ = sink(DataEvent::VolSlices(VolSliceOutcome {
                    key,
                    tag,
                    submitted,
                    results: (0..jobs).map(|_| Err(reason.clone())).collect(),
                }));
            }
            PanicAnswer::Upload { key, tag, target } => {
                let _ = sink(DataEvent::Upload(UploadOutcome {
                    key,
                    tag,
                    target,
                    result: Err(reason),
                }));
            }
            PanicAnswer::Fetch { source, identity } => {
                service.fail_fetch(&source, &identity, reason)
            }
            PanicAnswer::Publish { dataset, batch } => {
                let _ = sink(DataEvent::Diagnostics(vec![error_diagnostic(format!(
                    "local publish of {dataset}/{batch} failed: {reason}"
                ))]));
                let _ = sink(DataEvent::LocalPublishFailed {
                    dataset,
                    batch,
                    reason,
                });
            }
            PanicAnswer::Forget { dataset, batch } => {
                let _ = sink(DataEvent::Diagnostics(vec![error_diagnostic(format!(
                    "forgetting {dataset}/{batch} failed: {reason}"
                ))]));
                let _ = sink(DataEvent::ForgetFailed {
                    dataset,
                    batch,
                    reason,
                });
            }
            PanicAnswer::Unanswered => {
                let _ = sink(DataEvent::Diagnostics(vec![error_diagnostic(reason)]));
            }
        }
    }
}

fn serve(
    config: DataServiceConfig,
    sink: EventSink,
    rx: Receiver<Request>,
    pending_views: PendingViews,
    stopped: Arc<AtomicBool>,
    probe: Probe,
) {
    let mut service = match DataService::open(config, Arc::clone(&sink)) {
        Ok(s) => s,
        Err(e) => {
            // The service never became available: refuse later submissions
            // `Stopped` from now, and declare the loop gone although nothing
            // unwound, so the status bar says so.
            stopped.store(true, Ordering::Release);
            let reason = format!("data service failed to open: {e}");
            let _ = sink(DataEvent::Diagnostics(vec![error_diagnostic(
                reason.clone(),
            )]));
            let _ = sink(DataEvent::ThreadStopped {
                thread: REQUEST_LOOP.to_string(),
                reason,
            });
            return;
        }
    };
    if !service.diagnostics().is_empty() {
        let _ = sink(DataEvent::Diagnostics(service.diagnostics().to_vec()));
    }

    let _declare = StoppedOnUnwind(Arc::clone(&stopped));
    while let Ok(req) = rx.recv() {
        probe(ServePoint::Loop(&req));
        // Check before every request: a full request queue is already a wakeup.
        // The latest configuration therefore cannot be lost behind a burst.
        let replacement = pending_views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(ViewReplacement { views, dimensions }) = replacement {
            // Contained like a request: a panic keeps the previous views (the
            // replacement validates before it assigns) and the loop serves on.
            let replaced = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                geode_core::panic::contained(|| {
                    probe(ServePoint::Views);
                    service.replace_views(views, dimensions)
                })
            }));
            let diags = replaced.unwrap_or_else(|payload| {
                vec![error_diagnostic(format!(
                    "view replacement panicked: {}; the previous views stay in force",
                    crate::ingest::runner::panic_payload_message(payload.as_ref())
                ))]
            });
            if !diags.is_empty() {
                let _ = sink(DataEvent::Diagnostics(diags));
            }
        }
        if matches!(req, Request::Shutdown) {
            break;
        }
        // One request's panic is that request's error, answered once through
        // its own door; the next request is still served. The answer is taken
        // before the arm runs, so every arm must make its own answer (or its
        // handoff to a worker) its last step: a panic after an arm has already
        // answered would answer the same key a second time.
        let (kind, answer) = PanicAnswer::of(&req);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| {
                probe(ServePoint::Arm(&req));
                dispatch(&service, &sink, req);
            })
        }));
        if let Err(payload) = outcome {
            let payload = crate::ingest::runner::panic_payload_message(payload.as_ref());
            tracing::error!(target: "geode::ingest", "a {kind} request panicked on the request loop: {payload}");
            answer.answer(
                &service,
                &sink,
                format!("{kind} request panicked: {payload}"),
            );
        }
    }
    service.shutdown();
}

/// Run one request's arm. Every failure an arm returns is answered here on
/// the request's own key; a panic is answered by `serve`.
fn dispatch(service: &DataService, sink: &EventSink, req: Request) {
    match req {
        Request::Query(params) => {
            if let Err(e) = service.query(&params) {
                // Return compile/validation failure with the original request key and tag.
                let _ = sink(DataEvent::Query(QueryOutcome {
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
                let _ = sink(DataEvent::Distinct(DistinctOutcome {
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
                let _ = sink(DataEvent::Query(QueryOutcome {
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
                let _ = sink(DataEvent::Series(SeriesOutcome {
                    key: params.key,
                    tag: params.tag,
                    submitted: params.submitted,
                    result: Err(e.to_string()),
                }));
            }
        }
        Request::Catalog(params) => {
            let _ = sink(DataEvent::Catalog(service.catalog(&params)));
        }
        Request::Price(params) => service.price(params),
        Request::VolSlices(params) => service.vol_slices(params),
        Request::Publish(publish) => service.publish(publish),
        Request::Forget(forget) => service.forget(forget),
        Request::Upload(params) => service.upload(params),
        Request::Fetch(params) => service.fetch(&params),
        Request::Identities { source } => {
            if !service.identities(&source) {
                tracing::warn!(target: "geode::ingest", "identities request for '{source}' refused");
            }
        }
        Request::Cancel { key } => service.cancel(key),
        // Both are handled in `serve` before dispatch.
        Request::ReplaceViews | Request::Shutdown => {}
    }
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
            assert!(handle.send(Request::Cancel { key: QueryKey(1) }).is_ok());
        }
        assert!(handle.query(params(2, "reloaded")).is_ok());
        assert!(
            handle
                .replace_views(Vec::new(), DerivedDimensions::default())
                .is_ok()
        );
        let mut view = crate::ingest::load::tests_support::tree_view();
        view.name = "reloaded".into();
        assert!(
            handle
                .replace_views(vec![view], DerivedDimensions::default())
                .is_ok()
        );
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
            vol: crate::vol::VolConfig::default(),
        };
        let (tx, outcomes) = channel();
        let sink: EventSink = Arc::new(move |event| tx.send(event).is_ok());
        let pending = Arc::clone(&handle.inner.pending_views);
        // The service starts only after the queue filled and both reloads arrived.
        let service = std::thread::spawn(move || {
            serve(config, sink, requests, pending, Arc::default(), no_probe)
        });
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
        assert_eq!(
            handle.replace_views(Vec::new(), DerivedDimensions::default()),
            Err(Refusal::Stopped)
        );
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
        assert!(h.query(params(5, "tree")).is_ok());
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
        assert!(
            handle
                .document(DocumentParams {
                    key: QueryKey(5),
                    tag: 1,
                    submitted: Instant::now(),
                    dataset: "cvi_params".into(),
                    document_key: vec!["SPX.Z".into()],
                    as_of: AsOf::Live,
                })
                .is_ok()
        );
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
        assert_eq!(handle.upload(upload_params(1)), Err(Refusal::Busy));
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
                vol: crate::vol::VolConfig::default(),
            },
            sink,
        );

        assert!(handle.upload(upload_params(4)).is_ok());

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
        assert!(h.distinct(distinct_params(7, "book")).is_ok());
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
        assert!(
            handle
                .fetch(FetchParams {
                    key: QueryKey(3),
                    source: "k".into(),
                    identity: "SPX".into(),
                    from: chrono::Utc::now(),
                    to: chrono::Utc::now(),
                })
                .is_ok()
        );
        assert!(handle.identities("k").is_ok());
        assert!(matches!(rx.recv().unwrap(), Request::Fetch(p) if p.identity == "SPX"));
        assert!(matches!(rx.recv().unwrap(), Request::Identities { source } if source == "k"));
    }

    fn series_params(key: u64) -> SeriesParams {
        use geode_core::series::{BucketRule, Frequency, SeriesSpec, SlotKind};
        SeriesParams {
            key: QueryKey(key),
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
        }
    }

    /// Series parameters must pass unchanged through the common request queue.
    #[test]
    fn a_series_request_is_queued_as_a_request() {
        let (handle, rx) = DataHandle::for_tests();
        assert!(handle.series(series_params(3)).is_ok());
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
        assert!(
            h.catalog(CatalogParams {
                key: QueryKey(9),
                tag: 3,
                as_of: AsOf::Live,
            })
            .is_ok()
        );
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
            if h.query(params(i, "tree")).is_ok() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, REQUEST_BOUND);
        assert_eq!(h.dropped_requests(), 5);
        assert_eq!(
            h.query(params(99, "tree")),
            Err(Refusal::Busy),
            "a full queue passes: a later submission can succeed"
        );
    }

    #[test]
    fn a_gone_service_thread_refuses_every_request() {
        let (h, rx) = DataHandle::for_tests();
        drop(rx);
        assert_eq!(h.query(params(1, "tree")), Err(Refusal::Stopped));
        assert!(!h.cancel(QueryKey(1)));
        assert_eq!(
            h.dropped_requests(),
            0,
            "a stopped service is not a busy one; only Busy counts"
        );
    }

    #[test]
    fn fill_for_tests_makes_the_next_submission_busy() {
        let (h, _rx) = DataHandle::for_tests();
        h.fill_for_tests();
        assert_eq!(h.series(series_params(3)), Err(Refusal::Busy));
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
                vol: crate::vol::VolConfig::default(),
            },
            sink,
        );
        assert!(h.query(params(9, "tree")).is_ok());
        assert!(h.query(params(10, "nonesuch")).is_ok());
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
        assert_eq!(
            h.query(params(11, "tree")),
            Err(Refusal::Stopped),
            "after shutdown nothing is accepted, and nothing will be"
        );
        assert_eq!(
            h.dropped_requests(),
            0,
            "a refusal after shutdown is not a busy one"
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
                vol: crate::vol::VolConfig::default(),
            },
            sink,
        );
        assert!(
            h.document(DocumentParams {
                key: QueryKey(9),
                tag: 1,
                submitted: Instant::now(),
                dataset: "cvi_params".into(),
                document_key: vec!["SPX.Z".into()],
                as_of: AsOf::Live,
            })
            .is_ok()
        );
        assert!(
            h.document(DocumentParams {
                key: QueryKey(10),
                tag: 1,
                submitted: Instant::now(),
                dataset: "nonesuch".into(),
                document_key: vec!["SPX.Z".into()],
                as_of: AsOf::Live,
            })
            .is_ok()
        );
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
                vol: crate::vol::VolConfig::default(),
            },
            sink,
        );
        assert!(
            h.catalog(CatalogParams {
                key: QueryKey(21),
                tag: 21,
                as_of: AsOf::Live,
            })
            .is_ok()
        );
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
                vol: crate::vol::VolConfig::default(),
            },
            sink,
        );
        assert!(
            handle
                .price(crate::pricing::worker::tests::params(2, 9, &["SPX"]))
                .is_ok()
        );
        assert!(
            handle
                .publish(LocalPublish {
                    dataset: "sheets".into(),
                    rows: sheet_rows("a", &[7]),
                })
                .is_ok()
        );
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
        assert_eq!(
            handle.price(crate::pricing::worker::tests::params(2, 10, &["SPX"])),
            Err(Refusal::Stopped),
            "refused after shutdown"
        );
    }

    #[test]
    fn a_missing_vol_model_names_itself_through_the_handle() {
        let dir = tempfile::tempdir().unwrap();
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(local_dataset());
        let (tx, outcomes) = std::sync::mpsc::channel();
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
                pricer: PricerConfig::default(),
                vol: crate::vol::VolConfig::missing("vendor"),
            },
            sink,
        );
        handle
            .vol_slices(crate::vol::worker::tests::params(1, 1, &["2026-10-16"]))
            .unwrap();
        let outcome = loop {
            match outcomes
                .recv_timeout(Duration::from_secs(10))
                .expect("an event")
            {
                DataEvent::VolSlices(o) => break o,
                _ => continue,
            }
        };
        assert_eq!(
            outcome.results[0].as_ref().unwrap_err(),
            "vol model \"vendor\" is not built into this binary"
        );
        handle.shutdown();
        assert_eq!(
            handle.vol_slices(crate::vol::worker::tests::params(1, 2, &["2026-10-16"])),
            Err(Refusal::Stopped),
            "refused after shutdown"
        );
    }

    /// A real service over the local `sheets` dataset and the non-local CVI
    /// dataset, for the forget tests.
    fn local_handle() -> (
        tempfile::TempDir,
        DataHandle,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (handle, rx) = local_handle_at(dir.path());
        (dir, handle, rx)
    }

    /// [`local_handle`] over an existing directory: a restart.
    fn local_handle_at(
        dir: &std::path::Path,
    ) -> (DataHandle, std::sync::mpsc::Receiver<DataEvent>) {
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(local_dataset());
        schema.datasets.push(cvi_dataset());
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig {
                db_path: dir.join("geode.duckdb"),
                schema,
                views: Vec::new(),
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
                adapters: Default::default(),
                documents: Default::default(),
                egress: Vec::new(),
                pricer: PricerConfig::default(),
                vol: crate::vol::VolConfig::default(),
            },
            sink,
        );
        (handle, rx)
    }

    /// Local writes still queued when the app quits are the user's last
    /// edits: shutting the service down runs them before it stops, so a
    /// restart over the same database holds every one of them (and none
    /// of a forgotten one).
    #[test]
    fn local_writes_queued_at_shutdown_are_stored_before_the_service_stops() {
        let (dir, handle, _rx) = local_handle();
        let sheets: Vec<String> = (0..30).map(|i| format!("s{i}")).collect();
        for sheet in &sheets {
            assert!(
                handle
                    .publish(LocalPublish {
                        dataset: "sheets".into(),
                        rows: sheet_rows(sheet, &[1, 2]),
                    })
                    .is_ok()
            );
        }
        assert!(
            handle
                .forget(crate::service::LocalForget {
                    dataset: "sheets".into(),
                    key: vec!["s0".into()],
                })
                .is_ok()
        );
        handle.shutdown();

        let (handle, rx) = local_handle_at(dir.path());
        for (tag, sheet) in sheets.iter().enumerate() {
            assert!(
                handle
                    .document(DocumentParams {
                        key: QueryKey(9),
                        tag: tag as u64,
                        submitted: Instant::now(),
                        dataset: "sheets".into(),
                        document_key: vec![sheet.clone()],
                        as_of: AsOf::Live,
                    })
                    .is_ok()
            );
            let rows = loop {
                if let DataEvent::Query(o) = rx.recv_timeout(Duration::from_secs(30)).unwrap()
                    && o.tag == tag as u64
                {
                    break o.snapshot.unwrap().rows();
                }
            };
            let expected = if sheet == "s0" { 0 } else { 2 };
            assert_eq!(rows, expected, "{sheet} after the restart");
        }
        handle.shutdown();
    }

    #[test]
    fn a_forget_through_the_handle_deletes_the_document_and_reports_forgotten() {
        let (_dir, handle, rx) = local_handle();
        assert!(
            handle
                .publish(LocalPublish {
                    dataset: "sheets".into(),
                    rows: sheet_rows("a", &[7, 8]),
                })
                .is_ok()
        );
        assert!(
            handle
                .forget(crate::service::LocalForget {
                    dataset: "sheets".into(),
                    key: vec!["a".into()],
                })
                .is_ok()
        );
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                DataEvent::Forgotten { dataset, batch } => {
                    assert_eq!((dataset.as_str(), batch.as_str()), ("sheets", "a"));
                    break;
                }
                DataEvent::ForgetFailed { reason, .. } => panic!("{reason}"),
                _ => {}
            }
        }
        assert!(
            handle
                .document(DocumentParams {
                    key: QueryKey(4),
                    tag: 1,
                    submitted: Instant::now(),
                    dataset: "sheets".into(),
                    document_key: vec!["a".into()],
                    as_of: AsOf::Live,
                })
                .is_ok()
        );
        loop {
            if let DataEvent::Query(o) = rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                assert_eq!(o.snapshot.unwrap().rows(), 0, "no document is left");
                break;
            }
        }
        handle.shutdown();
    }

    /// A forget to a dataset that is not local, or with a key of the wrong
    /// arity, is refused at the service with an error diagnostic and one
    /// `ForgetFailed` (its asker may be waiting on an outcome), and never
    /// reaches the writer. The proof it never ran: the documents lane is
    /// FIFO, so a forget that had been queued would answer a second time
    /// (`Forgotten` or the writer's own `ForgetFailed`) before the publish
    /// submitted after it.
    #[test]
    fn a_forget_to_a_non_local_dataset_is_a_diagnostic_and_runs_nothing() {
        let (_dir, handle, rx) = local_handle();
        let refusals = [
            (
                crate::service::LocalForget {
                    dataset: "cvi_params".into(),
                    key: vec!["SPX.Z".into()],
                },
                "not a local dataset",
            ),
            (
                crate::service::LocalForget {
                    dataset: "sheets".into(),
                    key: Vec::new(),
                },
                "key has 0 part(s)",
            ),
        ];
        for (forget, expected) in refusals {
            let (dataset, batch) = (
                forget.dataset.clone(),
                geode_core::document::join_key(&forget.key),
            );
            assert!(
                handle.forget(forget).is_ok(),
                "admitted; the service refuses it"
            );
            let mut diagnosed = false;
            let mut answered = false;
            while !(diagnosed && answered) {
                match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                    DataEvent::Diagnostics(d) => {
                        assert!(
                            d.iter()
                                .any(|d| d.severity == Severity::Error
                                    && d.message.contains(expected)),
                            "{d:?}"
                        );
                        diagnosed = true;
                    }
                    DataEvent::ForgetFailed {
                        dataset: d,
                        batch: b,
                        reason,
                    } if !answered => {
                        assert_eq!((d.as_str(), b.as_str()), (dataset.as_str(), batch.as_str()));
                        assert!(reason.contains(expected), "{reason}");
                        answered = true;
                    }
                    DataEvent::Forgotten { .. } | DataEvent::ForgetFailed { .. } => {
                        panic!("the refused forget reached the writer")
                    }
                    _ => {}
                }
            }
        }
        assert!(
            handle
                .publish(LocalPublish {
                    dataset: "sheets".into(),
                    rows: sheet_rows("proof", &[1]),
                })
                .is_ok()
        );
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                DataEvent::Published { dataset, .. } if dataset == "sheets" => break,
                DataEvent::Forgotten { .. } | DataEvent::ForgetFailed { .. } => {
                    panic!("the refused forget reached the writer")
                }
                _ => {}
            }
        }
        handle.shutdown();
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
                vol: crate::vol::VolConfig::default(),
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
    fn an_unopenable_database_is_a_diagnostic_and_a_stopped_request_loop() {
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
                vol: crate::vol::VolConfig::default(),
            },
            sink,
        );
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            DataEvent::Diagnostics(d) => {
                assert!(d.iter().any(|d| d.message.contains("open")), "{d:?}")
            }
            other => panic!("{other:?}"),
        }
        // Set before the diagnostic was sent: the next submission already
        // says the service is gone, with no retry worth making.
        assert_eq!(h.query(params(1, "tree")), Err(Refusal::Stopped));
        let (thread, reason) = crate::supervise::tests_support::next_stop(&rx);
        assert_eq!(thread, crate::supervise::REQUEST_LOOP);
        assert!(reason.contains("failed to open"), "{reason}");
    }

    const ARM_PANIC: &str = "injected arm panic";
    const MARKED: QueryKey = QueryKey(666);

    fn is_marked(req: &Request) -> bool {
        match req {
            Request::Query(p) => p.key == MARKED,
            Request::Distinct(p) => p.key == MARKED,
            Request::Document(p) => p.key == MARKED,
            Request::Series(p) => p.key == MARKED,
            Request::Catalog(p) => p.key == MARKED,
            Request::Price(p) => p.key == MARKED,
            Request::VolSlices(p) => p.key == MARKED,
            Request::Upload(p) => p.key == MARKED,
            Request::Fetch(p) => p.key == MARKED,
            Request::Publish(p) => p.dataset == "marked",
            Request::Forget(f) => f.dataset == "marked",
            Request::Identities { source } => source == "marked",
            Request::Cancel { key } => *key == MARKED,
            Request::ReplaceViews | Request::Shutdown => false,
        }
    }

    /// Panics inside a marked request's boundary: the arms no production
    /// input can panic.
    fn panic_marked_arms(point: ServePoint<'_>) {
        if let ServePoint::Arm(req) = point
            && is_marked(req)
        {
            panic!("{ARM_PANIC}");
        }
    }

    /// Panics outside every boundary on a marked cancel: loop death.
    fn panic_marked_cancel_outside(point: ServePoint<'_>) {
        if let ServePoint::Loop(Request::Cancel { key }) = point
            && *key == MARKED
        {
            panic!("injected loop panic");
        }
    }

    fn empty_config(dir: &std::path::Path) -> DataServiceConfig {
        DataServiceConfig {
            db_path: dir.join("geode.duckdb"),
            schema: geode_core::schema::SchemaSpec::default(),
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
        }
    }

    fn probed(probe: Probe) -> (tempfile::TempDir, DataHandle, Receiver<DataEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn_with_probe(empty_config(dir.path()), sink, probe);
        (dir, handle, rx)
    }

    /// Everything the service emits before it answers a fresh catalog
    /// request: proof the loop still serves. A `ThreadStopped` fails.
    fn serves_on(handle: &DataHandle, rx: &Receiver<DataEvent>) -> Vec<DataEvent> {
        handle
            .catalog(CatalogParams {
                key: QueryKey(1),
                tag: 4242,
                as_of: AsOf::Live,
            })
            .unwrap();
        let mut seen = Vec::new();
        loop {
            match rx
                .recv_timeout(Duration::from_secs(30))
                .expect("the loop still answers")
            {
                DataEvent::Catalog(o) if o.tag == 4242 => {
                    assert!(o.snapshot.is_ok(), "{:?}", o.snapshot);
                    return seen;
                }
                DataEvent::ThreadStopped { thread, reason } => {
                    panic!("{thread} stopped: {reason}")
                }
                e => seen.push(e),
            }
        }
    }

    fn panicked(reason: &str, kind: &str) -> bool {
        reason.contains(&format!("{kind} request panicked")) && reason.contains(ARM_PANIC)
    }

    fn error_names(events: &[DataEvent], kind: &str) -> bool {
        events.iter().any(|e| {
            matches!(e, DataEvent::Diagnostics(d)
                if d.iter().any(|d| d.severity == Severity::Error && panicked(&d.message, kind)))
        })
    }

    #[test]
    fn a_panicking_query_is_answered_on_its_key_and_the_loop_serves_on() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.query(params(MARKED.0, "tree")).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(|e| matches!(e, DataEvent::Query(o)
            if o.key == MARKED && o.snapshot.as_ref().is_err_and(|r| panicked(r, "query")))),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_document_request_is_answered_on_its_key() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.document(DocumentParams {
            key: MARKED,
            tag: 1,
            submitted: Instant::now(),
            dataset: "cvi_params".into(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        })
        .unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(|e| matches!(e, DataEvent::Query(o)
            if o.key == MARKED && o.snapshot.as_ref().is_err_and(|r| panicked(r, "document")))),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_distinct_request_is_answered_on_its_key_and_column() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.distinct(distinct_params(MARKED.0, "book")).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(|e| matches!(e, DataEvent::Distinct(o)
            if o.key == MARKED && o.column == "book"
                && o.values.as_ref().is_err_and(|r| panicked(r, "distinct")))),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_series_request_is_answered_on_its_key() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.series(series_params(MARKED.0)).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(|e| matches!(e, DataEvent::Series(o)
            if o.key == MARKED && o.result.as_ref().is_err_and(|r| panicked(r, "series")))),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_catalog_request_is_answered_on_its_key() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.catalog(CatalogParams {
            key: MARKED,
            tag: 7,
            as_of: AsOf::Live,
        })
        .unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(|e| matches!(e, DataEvent::Catalog(o)
            if o.key == MARKED && o.tag == 7
                && o.snapshot.as_ref().is_err_and(|r| panicked(r, "catalog")))),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_price_request_answers_every_line() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.price(crate::pricing::worker::tests::params(
            MARKED.0,
            3,
            &["SPX", "NDX"],
        ))
        .unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(|e| matches!(e, DataEvent::Price(o)
            if o.key == MARKED && o.tag == 3 && o.results.len() == 2
                && o.results.iter().all(|(_, _, r)| r.as_ref().is_err_and(|r| panicked(r, "price"))))),
            "{seen:?}"
        );
    }

    #[test]
    fn a_request_loop_panic_on_a_vol_batch_answers_every_job_with_the_reason() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.vol_slices(crate::vol::worker::tests::params(
            MARKED.0,
            3,
            &["2026-10-16", "2026-11-20"],
        ))
        .unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(|e| matches!(e, DataEvent::VolSlices(o)
            if o.key == MARKED && o.tag == 3 && o.results.len() == 2
                && o.results.iter().all(|r| r.as_ref().is_err_and(|r| panicked(r, "vol_slices"))))),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_upload_is_answered_on_its_key_and_target() {
        let (_d, h, rx) = probed(panic_marked_arms);
        let mut upload = upload_params(5);
        upload.key = MARKED;
        h.upload(upload).unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(|e| matches!(e, DataEvent::Upload(o)
            if o.key == MARKED && o.tag == 5 && o.target == "sophis"
                && o.result.as_ref().is_err_and(|r| panicked(r, "upload")))),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_publish_is_a_diagnostic_and_its_writers_failure() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.publish(LocalPublish {
            dataset: "marked".into(),
            rows: sheet_rows("s", &[1]),
        })
        .unwrap();
        let seen = serves_on(&h, &rx);
        assert!(error_names(&seen, "publish"), "{seen:?}");
        assert!(
            seen.iter().any(
                |e| matches!(e, DataEvent::LocalPublishFailed { dataset, batch, reason }
            if dataset == "marked" && batch == "s" && panicked(reason, "publish"))
            ),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_forget_is_a_diagnostic_and_its_askers_failure() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.forget(crate::service::LocalForget {
            dataset: "marked".into(),
            key: vec!["s".into()],
        })
        .unwrap();
        let seen = serves_on(&h, &rx);
        assert!(error_names(&seen, "forget"), "{seen:?}");
        assert!(
            seen.iter().any(
                |e| matches!(e, DataEvent::ForgetFailed { dataset, batch, reason }
            if dataset == "marked" && batch == "s" && panicked(reason, "forget"))
            ),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panicking_identities_request_is_one_error_diagnostic() {
        let (_d, h, rx) = probed(panic_marked_arms);
        h.identities("marked").unwrap();
        let seen = serves_on(&h, &rx);
        assert!(error_names(&seen, "identities"), "{seen:?}");
    }

    #[test]
    fn a_panicking_cancel_is_one_error_diagnostic() {
        let (_d, h, rx) = probed(panic_marked_arms);
        assert!(h.cancel(MARKED));
        let seen = serves_on(&h, &rx);
        assert!(error_names(&seen, "cancel"), "{seen:?}");
    }

    #[test]
    fn a_panicking_view_replacement_is_one_diagnostic_and_serves_on() {
        fn panic_views(point: ServePoint<'_>) {
            if let ServePoint::Views = point {
                panic!("injected view panic");
            }
        }
        let (_d, h, rx) = probed(panic_views);
        h.replace_views(Vec::new(), DerivedDimensions::default())
            .unwrap();
        let seen = serves_on(&h, &rx);
        assert!(
            seen.iter().any(
                |e| matches!(e, DataEvent::Diagnostics(d) if d.iter().any(|d|
            d.severity == Severity::Error
                && d.message.contains("view replacement panicked")
                && d.message.contains("injected view panic")
                && d.message.contains("previous views stay in force")))
            ),
            "{seen:?}"
        );
    }

    #[test]
    fn a_panic_outside_every_arm_declares_the_request_loop_stopped() {
        let (_d, h, rx) = probed(panic_marked_cancel_outside);
        assert!(h.cancel(MARKED));
        let (thread, reason) = crate::supervise::tests_support::next_stop(&rx);
        assert_eq!(thread, crate::supervise::REQUEST_LOOP);
        assert!(reason.contains("injected loop panic"), "{reason}");
        assert_eq!(h.query(params(1, "tree")), Err(Refusal::Stopped));
        assert_eq!(h.dropped_requests(), 0, "a stopped loop is not a busy one");
    }

    #[test]
    fn a_clean_shutdown_declares_nothing() {
        let (_d, h, rx) = probed(no_probe);
        let _ = serves_on(&h, &rx);
        h.shutdown();
        assert!(
            rx.try_iter()
                .all(|e| !matches!(e, DataEvent::ThreadStopped { .. })),
            "a quit is not a failure"
        );
        assert!(!h.inner.stopped.load(Ordering::Acquire));
    }

    /// A pricer whose `price` announces it has started, then blocks until
    /// the test releases it: it holds a dying loop in its worker join for
    /// exactly as long as the test wants, with no clock involved.
    struct HeldPricer {
        started: Mutex<std::sync::mpsc::Sender<()>>,
        release: Mutex<Receiver<()>>,
    }

    impl geode_core::pricing::Pricer for HeldPricer {
        fn name(&self) -> &str {
            "held"
        }
        fn set_overrides(
            &self,
            _: &geode_core::pricing::MarketOverrides,
        ) -> Result<(), geode_core::pricing::PricingError> {
            Ok(())
        }
        fn price(
            &self,
            _: &geode_core::pricing::PriceRequest,
        ) -> Result<geode_core::pricing::PriceResult, geode_core::pricing::PricingError> {
            let _ = self.started.lock().unwrap().send(());
            let _ = self.release.lock().unwrap().recv();
            Err(geode_core::pricing::PricingError("released".into()))
        }
    }

    /// The dying loop joins its workers before its receiver drops; a
    /// submission in that window must be refused `Stopped`, not admitted to
    /// a queue nothing will read. A pricer line the test holds keeps the
    /// window open until the test releases it.
    #[test]
    fn a_submission_while_the_dying_loop_joins_its_workers_is_refused_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let (started_tx, started) = channel();
        let (release, release_rx) = channel();
        let mut config = empty_config(dir.path());
        config.pricer = PricerConfig::with(Arc::new(HeldPricer {
            started: Mutex::new(started_tx),
            release: Mutex::new(release_rx),
        }));
        let (tx, rx) = channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let h = DataService::spawn_with_probe(config, sink, panic_marked_cancel_outside);
        // Rebound after `h` so a failing assertion drops it first: the held
        // pricer is released before `h`'s drop joins the dying loop, and a
        // failure is reported instead of hanging.
        let release = release;
        h.price(crate::pricing::worker::tests::params(5, 1, &["SPX"]))
            .unwrap();
        started
            .recv_timeout(Duration::from_secs(30))
            .expect("the pricer started its line");
        assert!(h.cancel(MARKED));
        // The loop is now unwinding and blocked joining the held pricer.
        // The deadline only guards against a hang; it is not the contract.
        let deadline = Instant::now() + Duration::from_secs(30);
        while h.query(params(1, "tree")) != Err(Refusal::Stopped) {
            assert!(
                Instant::now() < deadline,
                "the dying loop admitted submissions while it joined its workers"
            );
            std::thread::yield_now();
        }
        assert!(
            rx.try_iter()
                .all(|e| !matches!(e, DataEvent::ThreadStopped { .. })),
            "the loop cannot be declared while it still joins the held pricer"
        );
        release.send(()).unwrap();
        h.shutdown();
        let stops: Vec<String> = rx
            .try_iter()
            .filter_map(|e| match e {
                DataEvent::ThreadStopped { thread, reason } => {
                    assert!(reason.contains("injected loop panic"), "{reason}");
                    Some(thread)
                }
                _ => None,
            })
            .collect();
        assert_eq!(stops, vec![crate::supervise::REQUEST_LOOP.to_string()]);
    }

    /// `DataService::open` must hand the production sink to each supervised
    /// worker as its stop sink, or a worker's death is announced to no one.
    /// A sink that panics delivering the marked price outcome kills the
    /// pricing worker outside its per-line boundaries.
    #[test]
    fn a_supervised_worker_death_reaches_the_services_own_sink() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| {
            if let DataEvent::Price(o) = &e
                && o.key == MARKED
            {
                panic!("injected delivery panic");
            }
            tx.lock().unwrap().send(e).is_ok()
        });
        let h = DataService::spawn(empty_config(dir.path()), sink);
        h.price(crate::pricing::worker::tests::params(MARKED.0, 1, &["SPX"]))
            .unwrap();
        let (thread, reason) = crate::supervise::tests_support::next_stop(&rx);
        assert_eq!(thread, "geode-pricing");
        assert!(reason.contains("injected delivery panic"), "{reason}");
        h.shutdown();
    }
}
