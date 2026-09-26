//! One ingest thread owns the writer and serializes all publication.
//!
//! Each dequeue prefers documents, then fetched series, then files. Documents
//! (publishes and local forgets, in one queue) and series are FIFO; files follow planned priority and descending source
//! time. Running work finishes before priorities are reconsidered. Sustained
//! higher-priority traffic can starve lower-priority work.
//!
//! File submissions deduplicate queued/in-flight path, size, and source time;
//! a queued file can be promoted. Document and series queues have no dedupe,
//! refusal, or fixed capacity. Upstream coalescing does not bound these queues.
//!
//! Loads use fixed staging-table names, so concurrent file loads on the same
//! store are unsafe. Shutdown joins running work but does not drain queued
//! jobs. See `docs/current/data-path.md` for delivery and health contracts.

use crate::adapter::SeriesRows;
use crate::health::Health;
use crate::ingest::load::{LoadError, LoadOutcome, LoadRequest, load_file};
use crate::ingest::plan::{WorkItem, WorkPlan};
use crate::source::discovery::is_unchanged;
use crate::source::{CandidateState, Priority};
use crate::store::document::{
    DocumentPublishRequest, DocumentPublished, document_generation_count, document_path,
    forget_document, prune_orphan_provenance, publish_document,
};
use crate::store::retention::{RetentionPolicy, sweep};
use crate::store::series::{SeriesAppendRequest, SeriesAppended, Span, append_series};
use crate::store::{Catalog, Store, StoreError};
use chrono::{DateTime, Utc};
use geode_core::document::{DocumentRows, join_key};
use geode_core::pricing::LOCAL_SOURCE;
use geode_core::schema::SchemaSpec;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone)]
pub enum IngestEvent {
    /// One job is about to run; `queued` counts waiting jobs across all three
    /// queues. Files/documents finish with Published or Failed; series finish
    /// with SeriesAppended or SeriesFailed. Local documents omit this event.
    Started {
        source: String,
        path: String,
        queued: usize,
    },
    Published {
        /// Configured source name, distinct from dataset. Source health must use
        /// this name even when several sources feed the same dataset.
        source: String,
        dataset: String,
        batch: String,
        gen_id: i64,
        /// The partitions written; `None` is the bookless one.
        books: Vec<Option<String>>,
        rows: usize,
        health: Health,
    },
    Failed {
        /// See `Published::source`'s doc — same reasoning, same field.
        source: String,
        dataset: String,
        batch: String,
        reason: String,
    },
    /// Series append completion, emitted even when `appended` is zero because
    /// coverage has been recorded and watchers must requery.
    SeriesAppended {
        source: String,
        dataset: String,
        identity: String,
        appended: usize,
        swept: usize,
    },
    /// A series job that could not be appended: refused rows, an
    /// undeclared dataset, or a panic inside the append. Load lane,
    /// keyed by the pair.
    SeriesFailed {
        source: String,
        dataset: String,
        identity: String,
        reason: String,
    },
    /// A forget deleted every generation of one document (`rows` payload
    /// rows, live plus archive; zero when nothing held the key).
    Forgotten {
        dataset: String,
        batch: String,
        rows: usize,
    },
    /// A forget that deleted nothing: the dataset is undeclared or not
    /// local, the delete failed and rolled back, or it panicked.
    ForgetFailed {
        dataset: String,
        batch: String,
        reason: String,
    },
    /// The queue drained. Not a terminal state — more work may be submitted.
    PlanComplete,
}

/// Archive generations kept per local document after each local publish:
/// with the live one, a sheet's history holds at most this many plus one.
/// Local datasets have no configured retention and are written by autosave,
/// so without a bound every edit burst would grow the archive forever.
pub const LOCAL_KEEP_GENERATIONS: usize = 200;

/// Nonblocking event delivery. `false` means refused; the runner continues
/// without retrying the event. The callback can run under the queue lock and
/// must not reenter the runner.
pub type IngestSink = Arc<dyn Fn(IngestEvent) -> bool + Send + Sync>;

/// Owned parsed columns waiting for publication. The receiver transfers them
/// without per-row copies or shared mutable buffers. `source` names the
/// health owner; `dataset` names storage. `bytes` and `received_at` record
/// message provenance; `source_time` orders generations and historical reads.
#[derive(Debug)]
pub struct DocumentJob {
    pub source: String,
    pub dataset: String,
    pub rows: DocumentRows,
    pub source_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub bytes: u64,
}

/// Delete one local document's whole history (every generation, its summary
/// and provenance rows). Queued in the documents FIFO, so it runs after every
/// publish submitted before it — including a save of the same key.
#[derive(Debug, Clone)]
pub struct ForgetJob {
    pub dataset: String,
    /// The document's key, joined (`join_key`).
    pub batch: String,
}

/// One entry in the documents FIFO. Publishes and forgets share one queue so
/// their relative order is exactly their submission order.
#[derive(Debug)]
enum DocumentWork {
    Publish(DocumentJob),
    Forget(ForgetJob),
}

impl From<DocumentJob> for DocumentWork {
    fn from(job: DocumentJob) -> Self {
        DocumentWork::Publish(job)
    }
}

/// Owned fetched rows waiting for the serialized append operation.
#[derive(Debug)]
pub struct SeriesJob {
    pub source: String,
    pub dataset: String,
    pub identity: String,
    pub rows: SeriesRows,
    pub span: Span,
    pub received_at: DateTime<Utc>,
}

#[derive(Default)]
struct Queue {
    /// Parsed documents, taken ahead of `items` (see the module doc for
    /// why). A `VecDeque`, and popped from the front, so two documents
    /// for *different* keys publish in the order they arrived: the
    /// coalescer upstream has already collapsed repeats of the same key,
    /// so everything still in here is distinct work, and a LIFO would
    /// reorder unrelated keys for no gain.
    documents: VecDeque<DocumentWork>,
    /// Fetched series, taken after documents and ahead of files: a
    /// fetch was asked for by a trader watching a chart, a file was
    /// found by a poll.
    series: VecDeque<SeriesJob>,
    items: Vec<WorkItem>,
    shutdown: bool,
    /// The file the runner has popped and is loading (or is about to skip
    /// as stale) right now, if any — keyed the same way `enqueue`'s dedupe
    /// is, `(csv_path, size, source_time)`, so the two agree on what
    /// counts as "the same file". Set when an item is popped, cleared once
    /// that item's outcome — `Published`, `Failed`, or a silent
    /// stale-skip — has been reported. Kept for the whole load, not just
    /// before it starts, so `enqueue`'s dedupe still sees it as spoken for
    /// the entire time a poll could otherwise re-add it.
    in_flight: Option<(PathBuf, u64, DateTime<Utc>)>,
}

pub struct IngestHandle {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

pub struct IngestRunner;

/// File-load operation, injectable to exercise panic containment independently
/// of ordinary malformed-input errors.
type LoadFn = fn(&Store, &LoadRequest) -> Result<LoadOutcome, LoadError>;

/// Document-publish operation, injectable to verify panic containment.
/// Malformed document validation normally returns an error before writing.
type PublishFn = fn(&Store, &DocumentPublishRequest) -> Result<DocumentPublished, StoreError>;

impl IngestRunner {
    pub fn spawn(store: Store, schema: SchemaSpec, sink: IngestSink) -> IngestHandle {
        Self::spawn_with(store, schema, sink, load_file, publish_document)
    }

    fn spawn_with(
        store: Store,
        schema: SchemaSpec,
        sink: IngestSink,
        load: LoadFn,
        publish: PublishFn,
    ) -> IngestHandle {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);
        let thread = std::thread::Builder::new()
            .name("geode-ingest".into())
            .spawn(move || run(store, schema, worker_queue, sink, load, publish))
            .expect("spawning the ingest thread");
        IngestHandle {
            queue,
            thread: Mutex::new(Some(thread)),
        }
    }

    /// A runner delivering into a channel, for callers that block on
    /// events — tests and the cold-start bench.
    pub fn spawn_channel(
        store: Store,
        schema: SchemaSpec,
    ) -> (IngestHandle, Receiver<IngestEvent>) {
        let (tx, rx) = channel();
        let sink: IngestSink = Arc::new(move |e| tx.send(e).is_ok());
        (Self::spawn(store, schema, sink), rx)
    }
}

impl IngestHandle {
    /// Merge file work, deduplicating queued and in-flight identities and
    /// promoting already queued work when its priority improves. The pop-time
    /// catalog check separately skips files that have already been loaded.
    pub fn submit(&self, plan: WorkPlan) -> usize {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        let enqueued = enqueue(&mut q, plan.items);
        q.items.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(b.source_time.cmp(&a.source_time))
        });
        cvar.notify_all();
        enqueued
    }

    /// Append a document under a short queue lock. No capacity limit, refusal,
    /// or deduplication applies here; upstream coalescing only replaces documents
    /// that have not yet been submitted to this runner.
    pub fn submit_document(&self, job: DocumentJob) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.documents.push_back(DocumentWork::Publish(job));
        cvar.notify_all();
    }

    /// Queue a forget behind every document already queued. No refusal
    /// here: the runner itself refuses a dataset that is not local, with
    /// `ForgetFailed`.
    pub fn submit_forget(&self, job: ForgetJob) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.documents.push_back(DocumentWork::Forget(job));
        cvar.notify_all();
    }

    /// Hand fetched rows to the runner. No dedupe and no refusal, as
    /// `submit_document`: the service subtracted coverage before the
    /// fetch, and `append_series` drops unchanged rows regardless.
    pub fn submit_series(&self, job: SeriesJob) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.series.push_back(job);
        cvar.notify_all();
    }

    /// Stop the runner and join it. Queued local writes (app publishes and
    /// forgets) still run first; every other queued item is dropped.
    pub fn shutdown(&self) {
        {
            let (lock, cvar) = &*self.queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.shutdown = true;
            cvar.notify_all();
        }
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

impl Drop for IngestHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Deduplicate by path, size, and source time. A matching queued entry can
/// move to a higher priority. Return only the number of new entries, excluding
/// promotions and duplicates.
fn enqueue(q: &mut Queue, items: Vec<WorkItem>) -> usize {
    let mut enqueued = 0;
    for item in items {
        let key = (
            item.candidate.csv_path.clone(),
            item.candidate.size,
            item.source_time,
        );
        if q.in_flight.as_ref() == Some(&key) {
            continue;
        }
        if let Some(existing) = q.items.iter_mut().find(|existing| {
            existing.candidate.csv_path == key.0
                && existing.candidate.size == key.1
                && existing.source_time == key.2
        }) {
            existing.priority = existing.priority.min(item.priority);
            continue;
        }
        q.items.push(item);
        enqueued += 1;
    }
    enqueued
}

/// Clears `Queue::in_flight`, unconditionally. Called once an item's
/// outcome — published, failed, or silently skipped as stale — has been
/// decided, never before: `submit`'s dedupe must see this file as spoken
/// for the whole time it could still be re-added by a poll, which is the
/// entire load, not just the moment before it starts.
fn clear_in_flight(queue: &(Mutex<Queue>, Condvar)) {
    let (lock, _cvar) = queue;
    let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
    q.in_flight = None;
}

/// One unit of work the runner popped.
#[derive(Debug)]
enum Work {
    Document(DocumentWork),
    Series(SeriesJob),
    File(WorkItem),
}

/// The queued local writes — `LOCAL_SOURCE` publishes and every forget
/// (forgets are local-only) — in queue order, leaving nothing queued.
fn take_local_writes(q: &mut Queue) -> Vec<DocumentWork> {
    q.documents
        .drain(..)
        .filter(|work| match work {
            DocumentWork::Publish(job) => job.source == LOCAL_SOURCE,
            DocumentWork::Forget(_) => true,
        })
        .collect()
}

/// Takes the next unit of work, **documents first, then series** (module
/// doc). A free function for the reason `enqueue` is one: the ordering
/// rule is the whole point and a test can only state it without a race by
/// driving a bare `Queue` synchronously — through the runner thread,
/// whether a document beat a file is a matter of when the submit landed.
///
/// Only the file arm sets `in_flight`: it is the file dedupe's key, and
/// neither a document nor a series job is a file. `None` means *every*
/// queue is empty, which is what makes it the `PlanComplete` condition.
fn take_work(q: &mut Queue) -> Option<Work> {
    if let Some(job) = q.documents.pop_front() {
        return Some(Work::Document(job));
    }
    if let Some(job) = q.series.pop_front() {
        return Some(Work::Series(job));
    }
    if q.items.is_empty() {
        return None;
    }
    let it = q.items.remove(0);
    q.in_flight = Some((
        it.candidate.csv_path.clone(),
        it.candidate.size,
        it.source_time,
    ));
    Some(Work::File(it))
}

/// Publish one owned document and report its outcome through the same
/// generation events as files. The rows are consumed by this operation.
fn publish_one_document(
    store: &Store,
    schema: &SchemaSpec,
    sink: &IngestSink,
    publish: PublishFn,
    refusal_logged: &AtomicBool,
    job: DocumentJob,
) {
    // The batch is the document's key, joined — computed here, before the
    // dataset is even resolved, because every failure below must name it:
    // a feed's broken key is the one thing a diagnostics row for this
    // failure is filed under (`HealthTracker`'s load lane is keyed by
    // batch), and an undeclared dataset would otherwise report a failure
    // against no key at all.
    let batch = join_key(&job.rows.key);
    // Resolved per document, exactly as the file arm resolves per item: a
    // `[sources.<name>]` can name a dataset a later `datasets.toml` edit
    // removed, and that is this document's failure, named, not a panic.
    let Some(dataset) = schema.dataset(&job.dataset) else {
        let failed = sink(IngestEvent::Failed {
            source: job.source.clone(),
            dataset: job.dataset.clone(),
            batch: batch.clone(),
            reason: format!("dataset '{}' is not declared", job.dataset),
        });
        if !failed {
            log_refused_event(
                refusal_logged,
                &format!(
                    "the undeclared-dataset failure for document {}/{batch}",
                    job.dataset
                ),
            );
        }
        return;
    };

    let source_time = if job.source == LOCAL_SOURCE {
        local_source_time(store, dataset, &batch, job.source_time)
    } else {
        job.source_time
    };

    // Contain publication panics as this document's failure and continue. The
    // contained marker tells the process panic hook to log without a crash file.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| {
            publish(
                store,
                &DocumentPublishRequest {
                    dataset,
                    source: &job.source,
                    rows: &job.rows,
                    source_time,
                    received_at: job.received_at,
                    bytes: job.bytes,
                },
            )
            .map_err(|e| e.to_string())
        })
    }));

    let event = match outcome {
        Ok(Ok(published)) => IngestEvent::Published {
            source: job.source.clone(),
            dataset: job.dataset.clone(),
            // The publish's own batch, not the one computed above: they
            // agree by construction, and reading it back from the publish
            // keeps the event describing what was actually written.
            batch: published.batch,
            gen_id: published.gen_id,
            // A document has no book column: the one partition written is
            // the bookless one, spelled `None`. Not an empty list, which
            // is how a load that wrote *nothing* reads.
            books: vec![None],
            rows: published.rows,
            health: Health::Ok,
        },
        Ok(Err(reason)) => IngestEvent::Failed {
            source: job.source.clone(),
            dataset: job.dataset.clone(),
            batch: batch.clone(),
            reason,
        },
        Err(payload) => {
            let message = panic_payload_message(payload.as_ref());
            // There is no file to name, so the synthetic
            // `document://source/dataset/batch` path stands in — the same
            // string `file_generations.path` records for this publish, so
            // the log line and the catalog row can be matched up.
            let path = document_path(&job.source, &job.dataset, &batch);
            log_ingest_panic(&path, &message);
            IngestEvent::Failed {
                source: job.source.clone(),
                dataset: job.dataset.clone(),
                batch: batch.clone(),
                reason: format!("document publish panicked at {}: {message}", path.display()),
            }
        }
    };
    let published = matches!(event, IngestEvent::Published { .. });
    if !sink(event) {
        log_refused_event(
            refusal_logged,
            &format!("the document publish outcome for {}/{batch}", job.dataset),
        );
    }
    if published && dataset.local {
        sweep_local(store, dataset, &batch);
    }
}

/// The source time a local save is published at: its own stamp, or just past
/// the live generation's when the wall clock stepped back. The app's latest
/// save is by definition its newest, and the writer is serialized, so it must
/// become live — an archived-only save would still answer `LocalPublished`,
/// telling the sheet it was stored while a reload shows the older content.
/// A lookup failure keeps the job's stamp (the publish's own backfill guard
/// then decides, as before) and is logged.
fn local_source_time(
    store: &Store,
    dataset: &geode_core::schema::DatasetSpec,
    batch: &str,
    stamped: DateTime<Utc>,
) -> DateTime<Utc> {
    match Catalog::new(store.writer()).live_source_time(&dataset.name, batch, None) {
        Ok(Some(live)) => stamped.max(live + chrono::Duration::microseconds(1)),
        Ok(None) => stamped,
        Err(e) => {
            tracing::warn!(
                target: "geode::ingest",
                "reading the live source time of {}/{batch} failed; the save keeps its own stamp: {e}",
                dataset.name,
            );
            stamped
        }
    }
}

/// Whether the document just saved holds more generations than a local
/// document keeps (the archive bound plus the live one). A document only
/// crosses the bound on its own publish, so checking the saved document alone
/// is enough to know a sweep has work.
fn local_needs_sweep(
    store: &Store,
    dataset: &geode_core::schema::DatasetSpec,
    batch: &str,
) -> Result<bool, StoreError> {
    Ok(document_generation_count(store, dataset, batch)? > LOCAL_KEEP_GENERATIONS + 1)
}

/// Bound a local dataset's archive after a publish (`LOCAL_KEEP_GENERATIONS`
/// per document), then prune the evicted generations' provenance. Gated on
/// the saved document crossing the bound, so an ordinary autosave costs one
/// summary count rather than a sweep of every sheet; once gated, the sweep
/// covers the whole dataset. Runs on the writer, after the publish committed
/// and its outcome was sent, so a failure never turns a stored save into a
/// failed one: it is logged, and that document's next save retries (it is
/// still past the bound). Returns whether a sweep ran and succeeded.
fn sweep_local(store: &Store, dataset: &geode_core::schema::DatasetSpec, batch: &str) -> bool {
    let policy = RetentionPolicy {
        keep_generations: Some(LOCAL_KEEP_GENERATIONS),
        keep_age: None,
    };
    let pairs = crate::store::ddl::table_pairs(dataset);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| {
            if !local_needs_sweep(store, dataset, batch).map_err(|e| e.to_string())? {
                return Ok(false);
            }
            sweep(store.writer(), dataset, &pairs, &policy, Utc::now())
                .map_err(|e| e.to_string())?;
            prune_orphan_provenance(store, dataset).map_err(|e| e.to_string())?;
            Ok(true)
        })
    }));
    let reason = match outcome {
        Ok(Ok(swept)) => return swept,
        Ok(Err(reason)) => reason,
        Err(payload) => format!("panicked: {}", panic_payload_message(payload.as_ref())),
    };
    tracing::warn!(
        target: "geode::ingest",
        "retention sweep of local dataset '{}' failed (history is kept until the next publish sweeps): {reason}",
        dataset.name,
    );
    false
}

/// Forget one local document under panic containment and report the outcome.
/// A dataset that is undeclared or not `local` is refused here, not only at
/// the service: deleting a feed's history is never a forget.
fn forget_one_document(
    store: &Store,
    schema: &SchemaSpec,
    sink: &IngestSink,
    refusal_logged: &AtomicBool,
    job: ForgetJob,
) {
    let failed = |reason: String| IngestEvent::ForgetFailed {
        dataset: job.dataset.clone(),
        batch: job.batch.clone(),
        reason,
    };
    let event = match schema.dataset(&job.dataset) {
        None => failed(format!("dataset '{}' is not declared", job.dataset)),
        Some(ds) if !ds.local => failed(format!(
            "dataset '{}' is not a local dataset; only a local document can be forgotten",
            job.dataset
        )),
        Some(ds) => {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                geode_core::panic::contained(|| {
                    forget_document(store, ds, &job.batch).map_err(|e| e.to_string())
                })
            }));
            match outcome {
                Ok(Ok(rows)) => IngestEvent::Forgotten {
                    dataset: job.dataset.clone(),
                    batch: job.batch.clone(),
                    rows,
                },
                Ok(Err(reason)) => failed(reason),
                Err(payload) => {
                    let message = panic_payload_message(payload.as_ref());
                    let path = document_path(LOCAL_SOURCE, &job.dataset, &job.batch);
                    log_ingest_panic(&path, &message);
                    failed(format!("forget panicked at {}: {message}", path.display()))
                }
            }
        }
    };
    if !sink(event) {
        log_refused_event(
            refusal_logged,
            &format!("the forget outcome for {}/{}", job.dataset, job.batch),
        );
    }
}

/// Resolve the series dataset, append under panic containment, and report
/// completion or failure. Refused delivery does not stop the runner.
fn append_one_series(
    store: &Store,
    schema: &SchemaSpec,
    sink: &IngestSink,
    refusal_logged: &AtomicBool,
    job: SeriesJob,
) {
    let pair = format!("{}@{}", job.identity, job.source);
    let failed = |reason: String| IngestEvent::SeriesFailed {
        source: job.source.clone(),
        dataset: job.dataset.clone(),
        identity: job.identity.clone(),
        reason,
    };
    let Some(dataset) = schema.dataset(&job.dataset) else {
        let event = failed(format!("dataset '{}' is not declared", job.dataset));
        if !sink(event) {
            log_refused_event(
                refusal_logged,
                &format!("the undeclared-dataset failure for series {pair}"),
            );
        }
        return;
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| {
            append_series(
                store,
                &SeriesAppendRequest {
                    dataset,
                    source: &job.source,
                    identity: &job.identity,
                    rows: &job.rows,
                    span: job.span,
                    received_at: job.received_at,
                },
            )
            .map_err(|e| e.to_string())
        })
    }));
    let event = match outcome {
        Ok(Ok(SeriesAppended { appended, swept })) => IngestEvent::SeriesAppended {
            source: job.source.clone(),
            dataset: job.dataset.clone(),
            identity: job.identity.clone(),
            appended,
            swept,
        },
        Ok(Err(reason)) => failed(reason),
        Err(payload) => {
            let message = panic_payload_message(payload.as_ref());
            let path =
                std::path::PathBuf::from(format!("series://{}/{}", job.source, job.identity));
            log_ingest_panic(&path, &message);
            failed(format!(
                "series append panicked at {}: {message}",
                path.display()
            ))
        }
    };
    if !sink(event) {
        log_refused_event(
            refusal_logged,
            &format!("the series append outcome for {pair}"),
        );
    }
}

fn run(
    store: Store,
    schema: SchemaSpec,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    sink: IngestSink,
    load: LoadFn,
    publish: PublishFn,
) {
    // Announce PlanComplete once at startup and on each transition to idle.
    // Repeated idle wakeups must not produce redundant progress events.
    let mut announced_idle = false;
    // Warn once per runner about refused delivery so a disconnected receiver
    // cannot fill the log ring. Delivery counters remain the full count.
    let refusal_logged = AtomicBool::new(false);

    loop {
        let work = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    let local = take_local_writes(&mut q);
                    drop(q);
                    // The app's own writes still queued are its user's last
                    // edits (a save flushed at quit, a `:rm` just confirmed):
                    // run them, in order, each answering as usual, before
                    // stopping. Feed documents, series and files are dropped;
                    // their sources resend them after a restart.
                    for work in local {
                        match work {
                            DocumentWork::Forget(job) => {
                                forget_one_document(&store, &schema, &sink, &refusal_logged, job)
                            }
                            DocumentWork::Publish(job) => publish_one_document(
                                &store,
                                &schema,
                                &sink,
                                publish,
                                &refusal_logged,
                                job,
                            ),
                        }
                    }
                    return;
                }
                // Documents first, then series, then files; `None` means
                // all three queues are empty, which is the only state
                // that announces a drain.
                if let Some(work) = take_work(&mut q) {
                    announced_idle = false;
                    break (work, q.items.len() + q.documents.len() + q.series.len());
                }
                if !announced_idle {
                    announced_idle = true;
                    if !sink(IngestEvent::PlanComplete) {
                        // Release the queue lock before logging so formatting cannot block submit.
                        // Reacquire and recheck shutdown/work before waiting again.
                        drop(q);
                        log_refused_event(&refusal_logged, "the queue-drained announcement");
                        q = lock.lock().unwrap_or_else(|e| e.into_inner());
                        continue;
                    }
                }
                let (guard, _) = cvar
                    .wait_timeout(q, std::time::Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner());
                q = guard;
            }
        };

        let (work, queued) = work;

        // Documents bypass file readiness and stale checks. Local writes omit
        // Started so autosave does not activate ingest progress; publication still
        // produces an outcome and the service's LoadEnded.
        let item = match work {
            // A forget is a local write like a local publish: no Started.
            Work::Document(DocumentWork::Forget(job)) => {
                forget_one_document(&store, &schema, &sink, &refusal_logged, job);
                continue;
            }
            Work::Document(DocumentWork::Publish(job)) => {
                if job.source == LOCAL_SOURCE {
                    // no Started/Loading for a local publish — see the
                    // doc comment above.
                } else if !sink(IngestEvent::Started {
                    source: job.source.clone(),
                    path: format!("document://{}/{}", job.source, job.dataset),
                    queued,
                }) {
                    log_refused_event(&refusal_logged, "a load-started announcement");
                }
                publish_one_document(&store, &schema, &sink, publish, &refusal_logged, job);
                continue;
            }
            Work::Series(job) => {
                if !sink(IngestEvent::Started {
                    source: job.source.clone(),
                    path: format!("series://{}/{}", job.source, job.identity),
                    queued,
                }) {
                    log_refused_event(&refusal_logged, "a load-started announcement");
                }
                append_one_series(&store, &schema, &sink, &refusal_logged, job);
                continue;
            }
            Work::File(item) => item,
        };

        // Recheck the catalog before loading: work may have become redundant while
        // queued. A stale skip emits no event. Lookup errors and contained panics
        // fail open so the load is attempted rather than silently discarded.
        let stale = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| {
                Catalog::new(store.writer()).lookup_by_path(&item.candidate.csv_path)
            })
        }))
        .map(|result| match result {
            Ok(Some(prev)) => is_unchanged(&prev, item.candidate.size, item.source_time),
            Ok(None) => false,
            Err(_store_error) => false,
        })
        .unwrap_or(false);
        if stale {
            clear_in_flight(&queue);
            continue;
        }

        // Announce Started only after the stale check, so every announcement has a
        // terminal operation outcome.
        if !sink(IngestEvent::Started {
            source: item.source.clone(),
            path: item.candidate.csv_path.to_string_lossy().into_owned(),
            queued,
        }) {
            log_refused_event(&refusal_logged, "a load-started announcement");
        }

        // Resolve the dataset per item. An undeclared dataset fails only this job.
        let Some(dataset) = schema.dataset(&item.dataset) else {
            let failed = sink(IngestEvent::Failed {
                source: item.source.clone(),
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: format!("dataset '{}' is not declared", item.dataset),
            });
            clear_in_flight(&queue);
            if !failed {
                log_refused_event(
                    &refusal_logged,
                    &format!(
                        "the undeclared-dataset failure for {}/{}",
                        item.dataset, item.batch
                    ),
                );
            }
            continue;
        };

        // Contain load panics, report the file as Failed, and continue with the next
        // job. Ordinary load errors use the same failure event.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| {
                let CandidateState::Ready(sentinel) = &item.candidate.state else {
                    return Err("candidate was not ready".to_string());
                };
                load(
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
            })
        }));

        let event = match outcome {
            Ok(Ok(loaded)) => IngestEvent::Published {
                source: item.source.clone(),
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                gen_id: loaded.gen_id,
                // What the load actually wrote, not what the sentinel
                // advertised. The sentinel's list is advisory — a row
                // whose book it omits is still published, and the
                // bookless partition appears in no sentinel — so
                // reporting it made a subscriber's view of a load differ
                // from the load.
                books: loaded.partitions.clone(),
                rows: loaded.rows,
                health: loaded.health,
            },
            Ok(Err(reason)) => IngestEvent::Failed {
                source: item.source.clone(),
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason,
            },
            Err(payload) => {
                // Preserve string panic payloads in Failed.reason; custom payloads receive
                // a named placeholder so the failure event is still delivered.
                let message = panic_payload_message(payload.as_ref());
                let path = item.candidate.csv_path.display();
                log_ingest_panic(&item.candidate.csv_path, &message);
                IngestEvent::Failed {
                    source: item.source.clone(),
                    dataset: item.dataset.clone(),
                    batch: item.batch.clone(),
                    reason: format!("ingest task panicked at {path}: {message}"),
                }
            }
        };
        let delivered = sink(event);
        // Cleared only now: while the load ran, `in_flight` kept a
        // duplicate submitted mid-load harmless.
        clear_in_flight(&queue);
        if !delivered {
            log_refused_event(
                &refusal_logged,
                &format!("the load outcome for {}/{}", item.dataset, item.batch),
            );
        }
    }
}

/// Log refused delivery once; the runner does not retry individual events.
fn log_refused_event(latched: &AtomicBool, what: &str) {
    if latched.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::warn!(
        target: "geode::ingest",
        "event channel refused {what}: dropped, the runner keeps working \
         (further refusals are counted, not logged)",
    );
}

/// Backfill items yield between files so current work is not starved. The
/// queue re-sort in `submit` is what actually reorders; this exists so the
/// intent is testable and named.
pub fn is_preemptible(item: &WorkItem) -> bool {
    item.priority == Priority::Backfill
}

/// Render a panic payload for Failed.reason. Strings are preserved and custom
/// payloads receive a placeholder. Shared by the runner and receiver so their
/// diagnostics describe the same payload consistently.
pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}

/// Log a contained ingest panic with its file and payload. Kept callable
/// independently so tests can capture it with a thread-local subscriber.
fn log_ingest_panic(path: &std::path::Path, message: &str) {
    tracing::error!(target: "geode::ingest", file = %path.display(), "ingest task panicked: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Priority;
    use crate::store::ddl::tests_support::{cvi_dataset, cvi_doc, series_dataset, series_rows};
    use geode_core::schema::DatasetSpec;
    use std::time::Duration;

    fn schema_of(ds: DatasetSpec) -> geode_core::schema::SchemaSpec {
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(ds);
        schema
    }

    fn drain(rx: &Receiver<IngestEvent>, want_published: usize) -> Vec<IngestEvent> {
        let mut out = Vec::new();
        let mut published = 0;
        while published < want_published {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(e) => {
                    if matches!(
                        e,
                        IngestEvent::Published { .. } | IngestEvent::Failed { .. }
                    ) {
                        published += 1;
                    }
                    out.push(e);
                }
                Err(_) => break,
            }
        }
        out
    }

    /// Builds a store, schema, and a source directory; returns a plan over it.
    fn harness() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Store,
        DatasetSpec,
        WorkPlan,
    ) {
        let (db_dir, src_dir, store, ds, _emitted) = crate::ingest::load::tests_support::fixture();
        let spec = crate::source::SourceSpec {
            pending_timeout: Duration::from_secs(3600),
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            ..crate::source::SourceSpec::directory(
                "risk",
                "risk_snapshot",
                vec![format!("{}/*.csv", src_dir.path().display())],
            )
        };
        let cat = crate::store::Catalog::new(store.writer());
        let found = crate::source::discover(&spec, &cat, std::time::SystemTime::now()).unwrap();
        let plan = crate::ingest::build_plan(&[(spec, found)]);
        (db_dir, src_dir, store, ds, plan)
    }

    #[test]
    fn started_precedes_each_publish_and_counts_what_is_still_queued() {
        // Two files submitted back to back: the first pops with one item
        // still behind it, the second with none. Every Started precedes
        // its own Published, and the path is the file's own.
        let (_db, _src, store, ds, plan) = harness();
        assert!(plan.items.len() >= 2, "need at least two files");
        let a = plan.items[0].clone();
        let b = plan.items[1].clone();
        let a_path = a.candidate.csv_path.to_string_lossy().to_string();
        let b_path = b.candidate.csv_path.to_string_lossy().to_string();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.submit(WorkPlan { items: vec![a, b] });
        let events = drain(&rx, 2);
        let started: Vec<(String, usize)> = events
            .iter()
            .filter_map(|e| match e {
                IngestEvent::Started { path, queued, .. } => Some((path.clone(), *queued)),
                _ => None,
            })
            .collect();
        assert_eq!(started, vec![(a_path, 1), (b_path, 0)]);
        // Ordering: Started(a) < Published(a) < Started(b) < Published(b).
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                IngestEvent::Started { .. } => "started",
                IngestEvent::Published { .. } => "published",
                IngestEvent::Failed { .. } => "failed",
                IngestEvent::SeriesAppended { .. } => "series_appended",
                IngestEvent::SeriesFailed { .. } => "series_failed",
                IngestEvent::Forgotten { .. } => "forgotten",
                IngestEvent::ForgetFailed { .. } => "forget_failed",
                IngestEvent::PlanComplete => "drained",
            })
            .filter(|k| *k != "drained")
            .collect();
        assert_eq!(kinds, ["started", "published", "started", "published"]);
        handle.shutdown();
    }

    #[test]
    fn a_document_job_starts_with_its_synthetic_path() {
        let (_dir, store) = document_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(cvi_dataset()));
        handle.submit_document(job("cvi_params", spx()));
        let events = drain(&rx, 1);
        let started = events.iter().find_map(|e| match e {
            IngestEvent::Started {
                source,
                path,
                queued,
            } => Some((source.clone(), path.clone(), *queued)),
            _ => None,
        });
        assert_eq!(
            started,
            Some((
                "cvi".to_string(),
                "document://cvi/cvi_params".to_string(),
                0
            ))
        );
        handle.shutdown();
    }

    /// Forward events except the first matching refusal. Count refusals so tests
    /// can observe the boundary before asserting the runner remains usable.
    fn refusing_sink(
        refuse: fn(&IngestEvent) -> bool,
    ) -> (
        IngestSink,
        Receiver<IngestEvent>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let (tx, rx) = channel();
        let refusals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&refusals);
        let sink: IngestSink = Arc::new(move |e: IngestEvent| {
            // One `compare_exchange` rather than load-then-add: the
            // refusal is claimed atomically, so "the first match only"
            // holds however many threads call this.
            if refuse(&e)
                && counter
                    .compare_exchange(
                        0,
                        1,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok()
            {
                return false;
            }
            tx.send(e).is_ok()
        });
        (sink, rx, refusals)
    }

    /// Block until `counter` reaches `n`, or fail. The refusal happens on
    /// the runner's own thread; a test that submitted more work without
    /// waiting would not know whether the refusal came first.
    fn wait_for_count(counter: &std::sync::atomic::AtomicUsize, n: usize, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if counter.load(std::sync::atomic::Ordering::SeqCst) >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {n} {what}");
    }

    #[test]
    fn a_refused_load_outcome_does_not_stop_the_runner() {
        // Refused event delivery must not stop the runner.
        let (_db, _src, store, ds, plan) = harness();
        assert!(plan.items.len() >= 2, "need at least two files");
        let first = plan.items[0].clone();
        let second = plan.items[1].clone();

        let (sink, rx, refusals) = refusing_sink(|e| {
            matches!(
                e,
                IngestEvent::Published { .. } | IngestEvent::Failed { .. }
            )
        });
        let handle = IngestRunner::spawn(store, schema_of(ds), sink);
        handle.submit(WorkPlan { items: vec![first] });
        wait_for_count(&refusals, 1, "refused terminal events");

        handle.submit(WorkPlan {
            items: vec![second.clone()],
        });
        let events = drain(&rx, 1);
        handle.shutdown();

        assert!(
            events.iter().any(|e| matches!(
                e,
                IngestEvent::Published { batch, .. } if *batch == second.batch
            )),
            "the item after a refused event must still load and report: {events:?}"
        );
    }

    #[test]
    fn a_refused_plan_complete_does_not_stop_the_runner() {
        let (_db, _src, store, ds, plan) = harness();
        let (sink, rx, refusals) = refusing_sink(|e| matches!(e, IngestEvent::PlanComplete));
        let handle = IngestRunner::spawn(store, schema_of(ds), sink);
        // The runner idles immediately with an empty queue, announces
        // once, and that announcement is refused.
        wait_for_count(&refusals, 1, "refused PlanComplete events");

        handle.submit(plan);
        let events = drain(&rx, 1);
        handle.shutdown();

        assert!(
            events
                .iter()
                .any(|e| matches!(e, IngestEvent::Published { .. })),
            "work submitted after a refused idle announcement must still load: {events:?}"
        );
    }

    #[test]
    fn a_refused_undeclared_dataset_failure_does_not_stop_the_runner() {
        let (_db, _src, store, ds, plan) = harness();
        assert!(plan.items.len() >= 2, "need at least two files");
        let mut undeclared = plan.items[0].clone();
        undeclared.dataset = "not_declared_anywhere".into();
        // Use a different file identity: the previous event can arrive before
        // clear_in_flight, so immediate resubmission could be deduplicated.
        let good = plan.items[1].clone();

        let (sink, rx, refusals) = refusing_sink(|e| matches!(e, IngestEvent::Failed { .. }));
        let handle = IngestRunner::spawn(store, schema_of(ds), sink);
        handle.submit(WorkPlan {
            items: vec![undeclared],
        });
        wait_for_count(&refusals, 1, "refused Failed events");

        handle.submit(WorkPlan {
            items: vec![good.clone()],
        });
        let events = drain(&rx, 1);
        handle.shutdown();

        assert!(
            events.iter().any(|e| matches!(
                e,
                IngestEvent::Published { batch, .. } if *batch == good.batch
            )),
            "an undeclared dataset's refused failure must not end the runner: {events:?}"
        );
    }

    /// Terse RFC 3339 literal, matching the convention used by
    /// `discovery.rs` and `as_of.rs`'s own test modules.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// A minimal `WorkItem` naming a file at `path` with the given `size`
    /// and `source_time` — enough to drive `enqueue` directly, with no
    /// store, no runner thread, and no real file on disk. `Candidate`'s
    /// other fields do not matter to `enqueue`, which only ever reads
    /// `candidate.csv_path` and `candidate.size`.
    fn work_item(
        path: &str,
        size: u64,
        source_time: DateTime<Utc>,
        priority: Priority,
    ) -> WorkItem {
        WorkItem {
            source: "test".into(),
            dataset: "risk_snapshot".into(),
            batch: "BK000".into(),
            candidate: crate::source::Candidate {
                csv_path: PathBuf::from(path),
                sentinel_path: PathBuf::from(format!("{path}.done")),
                batch: "BK000".into(),
                size,
                mtime: std::time::SystemTime::now(),
                state: CandidateState::Pending,
            },
            priority,
            source_time,
        }
    }

    #[test]
    fn enqueue_drops_an_item_already_queued_for_the_same_file() {
        // The `q.items` half of the dedupe condition, exercised directly
        // and synchronously.
        let mut q = Queue::default();
        let first = work_item(
            "/src/a.csv",
            10,
            ts("2026-08-30T07:00:00Z"),
            Priority::Backfill,
        );
        assert_eq!(enqueue(&mut q, vec![first]), 1);

        let dup = work_item(
            "/src/a.csv",
            10,
            ts("2026-08-30T07:00:00Z"),
            Priority::Backfill,
        );
        let n = enqueue(&mut q, vec![dup]);
        assert_eq!(n, 0, "the real queue's own length must not grow: {n}");
        assert_eq!(
            q.items.len(),
            1,
            "asserting on the queue itself, not a counter the code being tested computes for itself"
        );
    }

    #[test]
    fn enqueue_drops_an_item_matching_the_in_flight_key() {
        // The `q.in_flight` half of the dedupe condition — the half that,
        // through the public `submit`/runner API, only ever gets exercised
        // incidentally by timing. Driven directly here: no runner thread,
        // no race, `q.in_flight` just pre-set as if the runner had already
        // popped this exact file and were loading it right now.
        let mut q = Queue {
            in_flight: Some((PathBuf::from("/src/a.csv"), 10, ts("2026-08-30T07:00:00Z"))),
            ..Queue::default()
        };
        let item = work_item(
            "/src/a.csv",
            10,
            ts("2026-08-30T07:00:00Z"),
            Priority::LatestRisk,
        );
        let n = enqueue(&mut q, vec![item]);
        assert_eq!(
            n, 0,
            "an item matching the in-flight key must not be queued"
        );
        assert!(
            q.items.is_empty(),
            "and nothing landed in the real queue: {:?}",
            q.items
        );
    }

    #[test]
    fn enqueue_treats_a_different_size_at_the_same_source_time_as_a_different_file() {
        // The dedupe key must be exactly as fine as `is_unchanged`'s own
        // change-detection rule — `(size, source_time)` — or it can
        // collapse two genuinely different generations into one.
        let mut q = Queue::default();
        let a = work_item(
            "/src/a.csv",
            10,
            ts("2026-08-30T07:00:00Z"),
            Priority::Backfill,
        );
        assert_eq!(enqueue(&mut q, vec![a]), 1);

        let b = work_item(
            "/src/a.csv",
            11,
            ts("2026-08-30T07:00:00Z"),
            Priority::Backfill,
        );
        let n = enqueue(&mut q, vec![b]);
        assert_eq!(
            n, 1,
            "a different size is a different generation, not a duplicate"
        );
        assert_eq!(q.items.len(), 2);
    }

    #[test]
    fn enqueue_promotes_a_queued_items_priority_to_the_better_of_the_two() {
        let mut q = Queue::default();
        let low = work_item(
            "/src/a.csv",
            10,
            ts("2026-08-30T07:00:00Z"),
            Priority::Backfill,
        );
        assert_eq!(enqueue(&mut q, vec![low]), 1);

        let promote = work_item(
            "/src/a.csv",
            10,
            ts("2026-08-30T07:00:00Z"),
            Priority::LatestRisk,
        );
        let n = enqueue(&mut q, vec![promote]);
        assert_eq!(n, 0, "the file is already queued; nothing new is enqueued");
        assert_eq!(q.items.len(), 1);
        assert_eq!(
            q.items[0].priority,
            Priority::LatestRisk,
            "the queued entry is promoted, not left stuck at Backfill"
        );
    }

    #[test]
    fn works_a_plan_and_reports_every_publish() {
        let (_db, _src, store, ds, plan) = harness();
        let expected = plan.items.len();
        assert!(expected > 0, "fixture must produce work");

        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.submit(plan);
        let events = drain(&rx, expected);
        handle.shutdown();

        let published: Vec<&IngestEvent> = events
            .iter()
            .filter(|e| matches!(e, IngestEvent::Published { .. }))
            .collect();
        assert_eq!(published.len(), expected);

        // The event names the partitions the load actually wrote, not the
        // sentinel's advisory list. The two differ whenever a row carries
        // a book the sentinel omits, and always for the bookless
        // partition, which appears in no sentinel at all — so a subscriber
        // reading this saw a different load than the one that ran.
        for e in &published {
            let IngestEvent::Published { books, rows, .. } = e else {
                unreachable!()
            };
            assert!(
                !books.is_empty(),
                "a publish of {rows} rows wrote at least one partition: {e:?}"
            );
        }
    }

    #[test]
    fn a_newly_submitted_current_file_preempts_remaining_backfill() {
        let (_db, _src, store, ds, mut plan) = harness();
        assert!(plan.items.len() >= 3, "need several items to observe order");

        // Remove one backfill item and resubmit it at LatestRisk to isolate priority
        // preemption. Leaving its original copy would also exercise deduplication.
        for item in &mut plan.items {
            item.priority = Priority::Backfill;
        }
        let mut current = plan.items.remove(plan.items.len() - 1);
        current.priority = Priority::LatestRisk;
        let expected = plan.items.len() + 1;

        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.submit(plan);
        handle.submit(WorkPlan {
            items: vec![current.clone()],
        });

        let events = drain(&rx, expected);
        handle.shutdown();

        let positions: Vec<usize> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    IngestEvent::Published { .. } | IngestEvent::Failed { .. }
                )
            })
            .enumerate()
            .filter_map(|(i, e)| match e {
                IngestEvent::Published { batch, .. } if *batch == current.batch => Some(i),
                _ => None,
            })
            .collect();
        assert!(!positions.is_empty(), "preempting item never ran");
        assert!(
            positions[0] < expected - 1,
            "a current file must not wait behind all remaining backfill: {positions:?}"
        );
    }

    #[test]
    fn a_dedupe_hit_promotes_the_queued_items_priority_to_the_better_of_the_two() {
        // Resubmitting the same queued file at a better priority must promote it.
        // This exercises promotion rather than insertion of a new current file.
        let (_db, _src, store, ds, mut plan) = harness();
        assert!(plan.items.len() >= 2, "need at least two files");
        for item in &mut plan.items {
            item.priority = Priority::Backfill;
        }
        let expected = plan.items.len();
        let promoted = plan.items[0].clone();

        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.submit(plan);
        let mut re_offer = promoted.clone();
        re_offer.priority = Priority::LatestRisk;
        let enqueued = handle.submit(WorkPlan {
            items: vec![re_offer],
        });
        assert_eq!(
            enqueued, 0,
            "the file is already queued; the resubmission is a dedupe hit, not a new item"
        );

        let events = drain(&rx, expected);
        handle.shutdown();

        let positions: Vec<usize> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    IngestEvent::Published { .. } | IngestEvent::Failed { .. }
                )
            })
            .enumerate()
            .filter_map(|(i, e)| match e {
                IngestEvent::Published { batch, .. } if *batch == promoted.batch => Some(i),
                _ => None,
            })
            .collect();
        assert!(!positions.is_empty(), "the promoted item never ran");
        assert!(
            positions[0] < expected - 1,
            "a promoted item must not wait behind all remaining backfill: {positions:?}"
        );
    }

    #[test]
    fn a_failing_item_degrades_and_the_runner_keeps_going() {
        let (_db, _src, store, ds, mut plan) = harness();
        let good = plan.items.len();
        // Point one item at a nonexistent CSV.
        let mut broken = plan.items[0].clone();
        broken.candidate.csv_path = broken.candidate.csv_path.with_file_name("gone.csv");
        plan.items.insert(0, broken);

        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.submit(plan);
        let events = drain(&rx, good + 1);
        handle.shutdown();

        assert!(
            events
                .iter()
                .any(|e| matches!(e, IngestEvent::Failed { .. })),
            "the broken file must be reported"
        );
        let published = events
            .iter()
            .filter(|e| matches!(e, IngestEvent::Published { .. }))
            .count();
        assert_eq!(published, good, "one bad file must not stop the run");
    }

    /// A runner delivering into a channel, built on an injected `load` —
    /// mirrors `query/pool.rs`'s `channel_pool_with`, for a test that
    /// needs to control what a load does (panic) rather than run a real
    /// one.
    fn spawn_channel_with_load(
        store: Store,
        schema: geode_core::schema::SchemaSpec,
        load: LoadFn,
    ) -> (IngestHandle, Receiver<IngestEvent>) {
        let (tx, rx) = channel();
        let sink: IngestSink = Arc::new(move |e| tx.send(e).is_ok());
        (
            IngestRunner::spawn_with(store, schema, sink, load, publish_document),
            rx,
        )
    }

    fn boom(_store: &Store, _req: &LoadRequest) -> Result<LoadOutcome, LoadError> {
        panic!("injected panic payload");
    }

    #[test]
    fn a_panicking_load_names_the_file_and_the_panic_payload() {
        let (_db, _src, store, ds, plan) = harness();
        let item = plan.items[0].clone();
        let path = item.candidate.csv_path.clone();

        let (handle, rx) = spawn_channel_with_load(store, schema_of(ds), boom);
        handle.submit(WorkPlan { items: vec![item] });
        let events = drain(&rx, 1);
        handle.shutdown();

        let reason = events
            .iter()
            .find_map(|e| match e {
                IngestEvent::Failed { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no Failed event: {events:?}"));
        assert!(
            reason.contains(&path.display().to_string()),
            "reason must name the file: {reason}"
        );
        assert!(
            reason.contains("injected panic payload"),
            "reason must carry the panic payload: {reason}"
        );
    }

    /// Capture a direct log call with a scoped subscriber. The subscriber is
    /// thread-local, so it cannot observe calls on the spawned ingest thread.
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
    fn a_refusal_is_logged_once_per_runner_not_once_per_event() {
        // Repeated refusal from a gone receiver must produce one warning, preserving
        // space in the diagnostics ring for other events.
        let latch = AtomicBool::new(false);
        let records = logged(|| {
            log_refused_event(&latch, "the first thing");
            log_refused_event(&latch, "the second thing");
            log_refused_event(&latch, "the third thing");
        });
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].level, tracing::Level::WARN);
        assert_eq!(records[0].target, "geode::ingest");
        assert!(
            records[0].message.contains("the first thing"),
            "{records:?}"
        );
    }

    #[test]
    fn log_ingest_panic_logs_the_file_and_payload_at_error() {
        let records = logged(|| {
            log_ingest_panic(std::path::Path::new("/tmp/risk_snapshot.csv"), "boom");
        });
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].level, tracing::Level::ERROR);
        assert_eq!(records[0].target, "geode::ingest");
        assert!(
            records[0].message.contains("/tmp/risk_snapshot.csv"),
            "{records:?}"
        );
        assert!(records[0].message.contains("boom"), "{records:?}");
    }

    #[test]
    fn an_idle_runner_announces_completion_once_not_per_wakeup() {
        let (_db, _src, store, ds, _plan) = harness();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));

        // No work submitted: the runner idles. Give it many poll intervals.
        std::thread::sleep(Duration::from_millis(600));
        handle.shutdown();

        let completes = rx
            .try_iter()
            .filter(|e| matches!(e, IngestEvent::PlanComplete))
            .count();
        assert_eq!(
            completes, 1,
            "an idle runner must not flood an unbounded channel"
        );
    }

    #[test]
    fn shutdown_is_idempotent_and_does_not_hang() {
        let (_db, _src, store, ds, _plan) = harness();
        let (handle, _rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.shutdown();
        handle.shutdown();
    }

    #[test]
    fn an_item_naming_an_undeclared_dataset_fails_by_name_and_the_runner_continues() {
        // An item can name a removed dataset. Report its failure and keep the runner
        // available for other datasets.
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

    #[test]
    fn submit_drops_an_item_already_queued_for_the_same_file_and_source_time() {
        // Assert submit's queued count directly. Counting publications alone cannot
        // prove submission deduplication because the pop-time stale guard also
        // prevents duplicate loads.
        let (_db, _src, store, ds, plan) = harness();
        let item = plan.items[0].clone();

        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        let first = handle.submit(WorkPlan {
            items: vec![item.clone()],
        });
        let second = handle.submit(WorkPlan { items: vec![item] });
        assert_eq!(
            (first, second),
            (1, 0),
            "the second submit names a (path, source_time) the first already \
             queued and must enqueue nothing"
        );

        // `drain` waits for exactly the one terminal event this single
        // enqueued item can produce — no dependence on `PlanComplete`
        // timing, which can legitimately fire once before either `submit`
        // call above even runs (the runner thread finding an empty queue
        // at the moment it starts), and once for real after; a helper that
        // stopped on the first one would race that window.
        let events = drain(&rx, 1);
        handle.shutdown();

        let published = events
            .iter()
            .filter(|e| matches!(e, IngestEvent::Published { .. }))
            .count();
        assert_eq!(
            published, 1,
            "and, end to end, the file loads exactly once: {events:?}"
        );
    }

    #[test]
    fn a_queued_item_whose_file_was_loaded_meanwhile_is_skipped_at_pop_time() {
        // Load one file outside the runner, then queue it ahead of a fresh file.
        // The fresh file's publication proves the stale file has already been
        // processed; a queue-drained announcement could race submission.
        let (_db, _src, store, ds, plan) = harness();
        assert!(plan.items.len() >= 2, "need two distinct files");
        let mut stale = plan.items[0].clone();
        let mut fresh = plan.items[1].clone();
        assert_ne!(
            stale.batch, fresh.batch,
            "fixture must offer distinct files"
        );
        stale.priority = Priority::LatestRisk;
        fresh.priority = Priority::Backfill;

        let CandidateState::Ready(sentinel) = &stale.candidate.state else {
            panic!("fixture item must be ready: {stale:?}");
        };
        load_file(
            &store,
            &LoadRequest {
                dataset: &ds,
                dataset_name: &stale.dataset,
                csv_path: &stale.candidate.csv_path,
                sentinel,
                batch: &stale.batch,
            },
        )
        .unwrap();

        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.submit(WorkPlan {
            items: vec![stale.clone(), fresh.clone()],
        });

        let events = drain(&rx, 1);
        handle.shutdown();

        // Order matters here: `stale` (LatestRisk) is popped before
        // `fresh` (Backfill), and `drain(&rx, 1)` returns after the
        // *first* terminal event. Under the mutation this test exists to
        // catch (the pop-time guard disabled), that first event is
        // `stale` reloading — so this assertion, not the one below, is
        // the one that actually fails and names the real defect. Keeping
        // it first means the failure message points at what broke.
        assert!(
            events.iter().all(|e| !matches!(
                e,
                IngestEvent::Published { batch, .. } | IngestEvent::Failed { batch, .. }
                    if *batch == stale.batch
            )),
            "a file already loaded before this item was popped must not \
             reload: {events:?}"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, IngestEvent::Published { batch, .. } if *batch == fresh.batch)
            ),
            "the fresh file must still load: {events:?}"
        );

        // A stale skip must emit no Started: without a matching operation outcome,
        // the progress strip could remain active.
        let stale_path = stale.candidate.csv_path.to_string_lossy().into_owned();
        let started_paths: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                IngestEvent::Started { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            !started_paths.contains(&stale_path.as_str()),
            "a stale skip must not start the strip: {started_paths:?}"
        );
    }

    #[test]
    fn a_malformed_catalog_row_panics_the_pop_time_lookup_without_killing_the_runner() {
        // A NULL mtime in the catalog triggers the lookup's unwrap panic. The stale
        // check must fail open, allow the file load, and leave the runner able to
        // publish a second distinct file.
        let (_db, _src, store, ds, plan) = harness();
        assert!(plan.items.len() >= 2, "need two distinct files");
        let poisoned = plan.items[0].clone();
        let good = plan.items[1].clone();
        assert_ne!(
            poisoned.batch, good.batch,
            "fixture must offer distinct files"
        );

        store
            .writer()
            .execute_batch(&format!(
                "insert into file_generations
                     (file_id, dataset, batch, path, size, mtime, source_time,
                      gen_id, loaded_at, row_count, health, health_reason,
                      archived_only)
                 values
                     (-1, '{}', '{}', '{}', {}, NULL, '{}'::timestamptz, -1,
                      now(), 1, 'ok', NULL, false);",
                poisoned.dataset,
                poisoned.batch,
                poisoned.candidate.csv_path.display(),
                poisoned.candidate.size,
                poisoned.source_time.to_rfc3339(),
            ))
            .unwrap();

        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(ds));
        handle.submit(WorkPlan {
            items: vec![poisoned.clone(), good.clone()],
        });

        let events = drain(&rx, 2);
        handle.shutdown();

        assert!(
            events
                .iter()
                .any(|e| matches!(e, IngestEvent::Published { batch, .. } if *batch == good.batch)),
            "a poisoned catalog row must not take the ingest thread down — \
             the next item still publishes: {events:?}"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, IngestEvent::Published { batch, .. } if *batch == poisoned.batch)
            ),
            "fail-open means \"not stale\": the poisoned item's own load \
             still proceeds rather than being silently skipped: {events:?}"
        );
    }

    // Document publication.

    /// Create a document store with schema and catalog tables. Documents need
    /// neither a source directory nor generated CSV fixtures.
    fn document_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&cvi_dataset()).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    /// Read the next outcome, skipping Started and idle announcements.
    /// PlanComplete may occur before submission and between jobs.
    fn next_event(rx: &Receiver<IngestEvent>) -> IngestEvent {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(IngestEvent::PlanComplete) | Ok(IngestEvent::Started { .. }) => continue,
                Ok(e) => return e,
                Err(e) => panic!("no outcome event: {e}"),
            }
        }
    }

    fn job(dataset: &str, rows: geode_core::document::DocumentRows) -> DocumentJob {
        DocumentJob {
            source: "cvi".into(),
            dataset: dataset.into(),
            rows,
            source_time: ts("2026-09-12T14:00:00Z"),
            received_at: ts("2026-09-12T14:00:00Z"),
            bytes: 10,
        }
    }

    /// The fixture document every test below publishes, spelled once.
    fn spx() -> geode_core::document::DocumentRows {
        cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.])
    }

    #[test]
    fn a_submitted_document_publishes_and_reports_its_batch() {
        let (dir, store) = document_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(cvi_dataset()));
        handle.submit_document(job("cvi_params", spx()));
        match next_event(&rx) {
            IngestEvent::Published {
                source,
                dataset,
                batch,
                books,
                rows,
                health,
                ..
            } => {
                assert_eq!(
                    (source.as_str(), dataset.as_str(), batch.as_str()),
                    ("cvi", "cvi_params", "SPX.Z")
                );
                // The bookless partition, spelled as the one `None` the
                // publish actually wrote: a document has no book column,
                // so an empty list would tell a subscriber nothing was
                // written at all.
                assert_eq!((books, rows, health), (vec![None], 6, Health::Ok));
            }
            other => panic!("{other:?}"),
        }
        handle.shutdown();
        let _ = dir;
    }

    /// Local publication must omit Started while still emitting Published.
    /// Read the raw stream so the outcome helper cannot hide an unwanted start.
    #[test]
    fn a_local_publish_emits_no_started() {
        let (dir, store) = document_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(cvi_dataset()));
        let mut local_job = job("cvi_params", spx());
        local_job.source = geode_core::pricing::LOCAL_SOURCE.into();
        handle.submit_document(local_job);
        let mut saw_published = false;
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(10)) {
            match e {
                IngestEvent::Started { ref source, .. } if source == "local" => {
                    panic!("a local publish must not emit Started, got {e:?}")
                }
                IngestEvent::Published { .. } => {
                    saw_published = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_published, "the local publish still reports Published");
        handle.shutdown();
        let _ = dir;
    }

    #[test]
    fn an_invalid_document_fails_by_batch_and_the_runner_lives() {
        let (dir, store) = document_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(cvi_dataset()));

        // Zero rows: refused by `DocumentRows::validate` before anything is
        // written (publishing one is silently destructive — see its doc).
        let mut empty = spx();
        empty.axes = vec![
            ("term".into(), geode_core::document::Column::Date(vec![])),
            ("node".into(), geode_core::document::Column::F64(vec![])),
        ];
        empty.values = vec![("param".into(), geode_core::document::Column::F64(vec![]))];
        handle.submit_document(job("cvi_params", empty));

        match next_event(&rx) {
            IngestEvent::Failed {
                source,
                dataset,
                batch,
                reason,
            } => {
                // The batch is the document's own key, joined: a failure
                // with no batch could not be filed against the key whose
                // feed is broken.
                assert_eq!(
                    (source.as_str(), dataset.as_str(), batch.as_str()),
                    ("cvi", "cvi_params", "SPX.Z")
                );
                assert!(reason.contains("document has no rows"), "{reason}");
            }
            other => panic!("{other:?}"),
        }

        // And the runner is still working.
        handle.submit_document(job("cvi_params", spx()));
        assert!(
            matches!(next_event(&rx), IngestEvent::Published { batch, .. } if batch == "SPX.Z"),
            "a rejected document must not end the runner"
        );
        handle.shutdown();
        let _ = dir;
    }

    #[test]
    fn a_document_for_an_undeclared_dataset_fails_naming_it() {
        let (dir, store) = document_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema_of(cvi_dataset()));
        handle.submit_document(job("nonesuch", spx()));
        match next_event(&rx) {
            IngestEvent::Failed {
                dataset,
                batch,
                reason,
                ..
            } => {
                assert_eq!((dataset.as_str(), batch.as_str()), ("nonesuch", "SPX.Z"));
                assert!(reason.contains("not declared"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        handle.shutdown();
        let _ = dir;
    }

    #[test]
    fn take_work_prefers_a_document_over_a_queued_file() {
        // The ordering rule, driven synchronously against a bare `Queue` —
        // no runner thread and no timing, the same way the `enqueue` tests
        // reach the dedupe. The end-to-end test below observes the same
        // rule through events, but only this one can state it without a
        // race.
        let mut q = Queue::default();
        q.items.push(work_item(
            "/src/a.csv",
            10,
            ts("2026-08-30T07:00:00Z"),
            Priority::LatestRisk,
        ));
        q.documents.push_back(job("cvi_params", spx()).into());
        match take_work(&mut q) {
            Some(Work::Document(DocumentWork::Publish(d))) => {
                assert_eq!(d.rows.key, vec!["SPX.Z".to_string()])
            }
            other => panic!("a document outranks a file, even a LatestRisk one: {other:?}"),
        }
        // The file is still queued — a document takes no file's turn away,
        // it only goes first.
        assert_eq!(q.items.len(), 1);
        assert!(
            q.in_flight.is_none(),
            "`in_flight` is the file dedupe's key; a document must not touch it"
        );
        assert!(matches!(take_work(&mut q), Some(Work::File(_))));
    }

    #[test]
    fn a_document_is_popped_ahead_of_a_queued_file() {
        // End to end: with several files queued, a document submitted
        // afterwards must not wait for them all to drain. Asserted the way
        // `a_newly_submitted_current_file_preempts_remaining_backfill`
        // asserts preemption — by position, not by "the first event" —
        // because the runner may already have popped file one by the time
        // the document is submitted.
        let (_db, _src, store, ds, plan) = harness();
        assert!(plan.items.len() >= 3, "need several files to observe order");
        let files = plan.items.len();
        store.apply_schema(&cvi_dataset()).unwrap();
        let mut schema = schema_of(ds);
        schema.datasets.push(cvi_dataset());

        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        handle.submit(plan);
        handle.submit_document(job("cvi_params", spx()));

        let events = drain(&rx, files + 1);
        handle.shutdown();

        let outcomes: Vec<&IngestEvent> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    IngestEvent::Published { .. } | IngestEvent::Failed { .. }
                )
            })
            .collect();
        let position = outcomes
            .iter()
            .position(
                |e| matches!(e, IngestEvent::Published { dataset, .. } if dataset == "cvi_params"),
            )
            .unwrap_or_else(|| panic!("the document never published: {events:?}"));
        assert!(
            position < files,
            "a document must not wait behind every queued file: {position} of {files}"
        );
    }

    /// A runner delivering into a channel with an injected *publish* —
    /// `spawn_channel_with_load`'s sibling, for the same reason: no
    /// document makes `publish_document` itself panic, and containment is
    /// the property worth testing.
    fn spawn_channel_with_publish(
        store: Store,
        schema: geode_core::schema::SchemaSpec,
        publish: PublishFn,
    ) -> (IngestHandle, Receiver<IngestEvent>) {
        let (tx, rx) = channel();
        let sink: IngestSink = Arc::new(move |e| tx.send(e).is_ok());
        (
            IngestRunner::spawn_with(store, schema, sink, load_file, publish),
            rx,
        )
    }

    /// A publish slow enough that a shutdown lands while it runs.
    fn slow_publish(
        store: &Store,
        req: &DocumentPublishRequest,
    ) -> Result<DocumentPublished, crate::store::StoreError> {
        std::thread::sleep(std::time::Duration::from_millis(200));
        publish_document(store, req)
    }

    /// A shutdown runs the local writes still queued (saves and forgets,
    /// in order, each answering as usual) before the runner stops; a feed's
    /// queued document is still dropped, as files and series are.
    #[test]
    fn shutdown_runs_queued_local_writes_and_drops_the_rest() {
        let (_dir, path, store, schema) = local_store();
        let (handle, rx) = spawn_channel_with_publish(store, schema, slow_publish);
        let at = ts("2026-09-12T14:00:00Z");
        handle.submit_document(local_job("a", &[1], at));
        handle.submit_document(job("cvi_params", spx()));
        handle.submit_document(local_job("b", &[1, 2], at));
        handle.submit_document(local_job("c", &[1, 2, 3], at));
        handle.submit_forget(ForgetJob {
            dataset: "sheets".into(),
            batch: "b".into(),
        });
        // Lands while `a` is publishing.
        handle.shutdown();
        let answered: Vec<String> = rx
            .try_iter()
            .filter_map(|e| match e {
                IngestEvent::Published { batch, .. } => Some(format!("published {batch}")),
                IngestEvent::Forgotten { batch, .. } => Some(format!("forgot {batch}")),
                IngestEvent::Failed { batch, reason, .. } => panic!("{batch}: {reason}"),
                _ => None,
            })
            .collect();
        assert_eq!(
            answered,
            ["published a", "published b", "published c", "forgot b"],
            "the feed's document is dropped, the local writes all run"
        );
        assert_eq!(
            count_in(&path, "select count(*) from sheets_document_live"),
            1 + 3
        );
    }

    fn boom_publish(
        _store: &Store,
        _req: &DocumentPublishRequest,
    ) -> Result<DocumentPublished, crate::store::StoreError> {
        panic!("injected publish panic");
    }

    #[test]
    fn a_panicking_publish_is_contained_and_reported() {
        let (dir, store) = document_store();
        let (handle, rx) =
            spawn_channel_with_publish(store, schema_of(cvi_dataset()), boom_publish);
        handle.submit_document(job("cvi_params", spx()));
        match next_event(&rx) {
            IngestEvent::Failed { batch, reason, .. } => {
                assert_eq!(batch, "SPX.Z");
                assert!(reason.contains("panicked"), "{reason}");
                assert!(reason.contains("injected publish panic"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        // The thread survived its own panic: the next document still runs.
        handle.submit_document(job("cvi_params", spx()));
        assert!(
            matches!(next_event(&rx), IngestEvent::Failed { reason, .. } if reason.contains("panicked")),
            "a contained panic must not end the ingest thread"
        );
        handle.shutdown();
        let _ = dir;
    }

    // Series append.

    /// Create a series store with schema and catalog tables.
    fn series_store() -> (tempfile::TempDir, Store, geode_core::schema::SchemaSpec) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = series_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(ds);
        (dir, store, schema)
    }

    fn series_job(identity: &str, rows: SeriesRows) -> SeriesJob {
        SeriesJob {
            source: "demo_kdb".into(),
            dataset: "series".into(),
            identity: identity.into(),
            rows,
            span: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            received_at: ts("2026-01-06T09:00:00Z"),
        }
    }

    /// The next event, skipping only `PlanComplete` — it fires once on an
    /// idle runner and races every submit (see `next_event`'s own doc
    /// above), so a test asserting `Started` itself, not just the outcome
    /// behind it, must tolerate one arriving first.
    fn next_skipping_idle(rx: &Receiver<IngestEvent>) -> IngestEvent {
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                IngestEvent::PlanComplete => continue,
                e => return e,
            }
        }
    }

    #[test]
    fn a_series_job_is_appended_and_announced() {
        let (_d, store, schema) = series_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        handle.submit_series(series_job(
            "SPX.close",
            series_rows("2026-01-05T14:30:00Z", 3, 100.0),
        ));
        let started = next_skipping_idle(&rx);
        assert!(
            matches!(&started, IngestEvent::Started { source, path, .. }
                if source == "demo_kdb" && path == "series://demo_kdb/SPX.close"),
            "{started:?}"
        );
        let done = next_skipping_idle(&rx);
        assert!(
            matches!(&done, IngestEvent::SeriesAppended { source, dataset, identity, appended: 3, swept: 0 }
                if source == "demo_kdb" && dataset == "series" && identity == "SPX.close"),
            "{done:?}"
        );
        handle.shutdown();
    }

    #[test]
    fn a_series_job_for_an_undeclared_dataset_fails_by_name() {
        let (_d, store, schema) = series_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        let mut job = series_job("SPX.close", SeriesRows::default());
        job.dataset = "nope".into();
        handle.submit_series(job);
        let done = loop {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                IngestEvent::SeriesFailed {
                    reason, identity, ..
                } => break (reason, identity),
                _ => continue,
            }
        };
        assert_eq!(done.1, "SPX.close");
        assert!(
            done.0.contains("dataset 'nope' is not declared"),
            "{}",
            done.0
        );
        handle.shutdown();
    }

    #[test]
    fn invalid_series_rows_fail_the_job_and_leave_the_runner_working() {
        let (_d, store, schema) = series_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        let bad = SeriesRows {
            ts: vec![ts("2026-01-05T14:30:00Z")],
            value: vec![1.0, 2.0],
        };
        handle.submit_series(series_job("SPX.close", bad));
        handle.submit_series(series_job(
            "VIX",
            series_rows("2026-01-05T14:30:00Z", 1, 20.0),
        ));
        let mut failed = None;
        let mut appended = None;
        for _ in 0..6 {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                IngestEvent::SeriesFailed {
                    identity, reason, ..
                } => failed = Some((identity, reason)),
                IngestEvent::SeriesAppended {
                    identity,
                    appended: n,
                    ..
                } => appended = Some((identity, n)),
                _ => {}
            }
            if failed.is_some() && appended.is_some() {
                break;
            }
        }
        let (id, reason) = failed.unwrap();
        assert_eq!(id, "SPX.close");
        assert!(reason.contains("2 values for 1 timestamps"), "{reason}");
        assert_eq!(appended.unwrap(), ("VIX".to_string(), 1));
        handle.shutdown();
    }

    #[test]
    fn take_work_pops_documents_then_series_then_files() {
        // Populate all three queues in reverse priority order. This detects either
        // a document/series or a series/file ordering swap.
        let mut q = Queue::default();
        q.documents.push_back(job("cvi_params", spx()).into());
        q.series
            .push_back(series_job("SPX.close", SeriesRows::default()));
        q.items.push(work_item(
            "a.csv",
            10,
            ts("2026-08-30T07:00:00Z"),
            Priority::Backfill,
        ));
        let first = take_work(&mut q).unwrap();
        assert!(matches!(first, Work::Document(_)));
        let second = take_work(&mut q).unwrap();
        assert!(matches!(second, Work::Series(_)));
        let third = take_work(&mut q).unwrap();
        assert!(matches!(third, Work::File(_)));
        assert!(take_work(&mut q).is_none());
    }

    // Local documents: forget and retention.

    /// A store holding both the local `sheets` dataset and the non-local
    /// CVI dataset, with the path kept so a test can reopen it after the
    /// runner (which owns the store) has shut down.
    fn local_store() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        Store,
        geode_core::schema::SchemaSpec,
    ) {
        use crate::store::ddl::tests_support::local_dataset;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geode.duckdb");
        let store = Store::open(&path).unwrap();
        let mut schema = geode_core::schema::SchemaSpec::default();
        for ds in [local_dataset(), cvi_dataset()] {
            store.apply_schema(&ds).unwrap();
            schema.datasets.push(ds);
        }
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, path, store, schema)
    }

    fn local_job(sheet: &str, qty: &[i64], at: DateTime<Utc>) -> DocumentJob {
        DocumentJob {
            source: LOCAL_SOURCE.into(),
            dataset: "sheets".into(),
            rows: crate::store::ddl::tests_support::sheet_rows(sheet, qty),
            source_time: at,
            received_at: at,
            bytes: 0,
        }
    }

    fn count_in(path: &std::path::Path, sql: &str) -> i64 {
        let store = Store::open(path).unwrap();
        store.writer().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// The documents lane is one FIFO: a forget queued behind a save of the
    /// same key runs after it, so the save's generation is deleted too —
    /// what `:rm` after an unconfirmed autosave must mean.
    #[test]
    fn a_forget_queued_after_a_save_of_the_same_key_leaves_no_document() {
        let (_dir, path, store, schema) = local_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        handle.submit_document(local_job("s", &[1, 2], ts("2026-09-12T14:00:00Z")));
        handle.submit_forget(ForgetJob {
            dataset: "sheets".into(),
            batch: "s".into(),
        });
        match next_event(&rx) {
            IngestEvent::Published { batch, .. } => assert_eq!(batch, "s"),
            other => panic!("the save runs first: {other:?}"),
        }
        match next_event(&rx) {
            IngestEvent::Forgotten {
                dataset,
                batch,
                rows,
            } => assert_eq!((dataset.as_str(), batch.as_str(), rows), ("sheets", "s", 2)),
            other => panic!("{other:?}"),
        }
        handle.shutdown();
        drop(rx);
        for sql in [
            "select count(*) from sheets_document_live",
            "select count(*) from sheets_document_archive",
            "select count(*) from generations where dataset = 'sheets'",
            "select count(*) from file_generations where dataset = 'sheets'",
        ] {
            assert_eq!(count_in(&path, sql), 0, "{sql}");
        }
    }

    /// The runner refuses a forget that is not for a declared local dataset
    /// itself, not only behind the service's gate: `ForgetJob` is a public
    /// door onto the writer, and deleting a feed's history is not a thing
    /// any caller of it may do.
    #[test]
    fn a_forget_of_a_non_local_or_undeclared_dataset_fails_and_deletes_nothing() {
        let (_dir, path, store, schema) = local_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        handle.submit_document(job("cvi_params", spx()));
        assert!(matches!(next_event(&rx), IngestEvent::Published { .. }));
        for dataset in ["cvi_params", "nonesuch"] {
            handle.submit_forget(ForgetJob {
                dataset: dataset.into(),
                batch: "SPX.Z".into(),
            });
            match next_event(&rx) {
                IngestEvent::ForgetFailed {
                    dataset: d,
                    batch,
                    reason,
                } => {
                    assert_eq!((d.as_str(), batch.as_str()), (dataset, "SPX.Z"));
                    assert!(reason.contains(dataset), "{reason}");
                }
                other => panic!("{other:?}"),
            }
        }
        handle.shutdown();
        drop(rx);
        assert_eq!(
            count_in(&path, "select count(*) from cvi_params_document_live"),
            6
        );
    }

    /// The app's latest save is by definition its newest: a local save
    /// stamped at or before what is live (a wall-clock step-back) is moved
    /// just past live on the writer instead of being archived, or the sheet
    /// would be told "saved" while a reload shows the older content.
    #[test]
    fn a_local_save_older_than_live_is_still_published_live() {
        let (_dir, path, store, schema) = local_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        let future = ts("2026-09-12T15:00:00Z");
        handle.submit_document(local_job("s", &[1], future));
        handle.submit_document(local_job("s", &[2], ts("2026-09-12T14:00:00Z")));
        for _ in 0..2 {
            assert!(matches!(next_event(&rx), IngestEvent::Published { .. }));
        }
        handle.shutdown();
        drop(rx);
        assert_eq!(
            count_in(&path, "select min(qty) from sheets_document_live"),
            2,
            "the latest save is live"
        );
        assert_eq!(
            count_in(
                &path,
                "select count(*) from file_generations \
                 where dataset = 'sheets' and archived_only"
            ),
            0
        );
        let store = Store::open(&path).unwrap();
        let live: DateTime<Utc> = store
            .writer()
            .query_row(
                "select max(source_time) from sheets_document_live",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, future + chrono::Duration::microseconds(1));
    }

    /// The sweep is gated on the saved document crossing the bound, so a
    /// save under it costs one small count, not a sweep of every sheet; the
    /// sweep that does run also prunes the evicted generations' provenance.
    #[test]
    fn a_local_sweep_runs_only_past_the_bound_and_prunes_provenance() {
        use crate::store::ddl::tests_support::local_dataset;
        let (_dir, path, store, _schema) = local_store();
        let ds = local_dataset();
        let start = ts("2026-09-12T14:00:00Z");
        let save = |i: usize| {
            let at = start + chrono::Duration::seconds(i as i64);
            publish_document(
                &store,
                &DocumentPublishRequest {
                    dataset: &ds,
                    source: LOCAL_SOURCE,
                    rows: &crate::store::ddl::tests_support::sheet_rows("s", &[i as i64]),
                    source_time: at,
                    received_at: at,
                    bytes: 0,
                },
            )
            .unwrap();
        };
        for i in 0..=LOCAL_KEEP_GENERATIONS {
            save(i);
        }
        assert!(
            !local_needs_sweep(&store, &ds, "s").unwrap(),
            "at the bound"
        );
        assert!(!sweep_local(&store, &ds, "s"), "no sweep under the bound");
        save(LOCAL_KEEP_GENERATIONS + 1);
        assert!(
            local_needs_sweep(&store, &ds, "s").unwrap(),
            "past the bound"
        );
        assert!(sweep_local(&store, &ds, "s"));
        drop(store);
        let bound = LOCAL_KEEP_GENERATIONS as i64 + 1;
        for sql in [
            "select count(*) from generations where dataset = 'sheets'",
            "select count(*) from file_generations where dataset = 'sheets'",
            "select count(*) from file_books",
        ] {
            assert_eq!(count_in(&path, sql), bound, "{sql}");
        }
        assert_eq!(
            count_in(
                &path,
                "select count(*) from file_generations fg where dataset = 'sheets' \
                 and not exists (select 1 from generations g \
                 where g.dataset = fg.dataset and g.gen_id = fg.gen_id)"
            ),
            0,
            "only evicted generations' provenance went"
        );
    }

    /// Local documents are swept to the retention bound after each local
    /// publish, per key; a feed's documents are never swept by that path.
    #[test]
    fn local_publishes_are_swept_to_the_retention_bound_and_feeds_are_not() {
        let (_dir, path, store, schema) = local_store();
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        let n = LOCAL_KEEP_GENERATIONS + 5;
        let start = ts("2026-09-12T14:00:00Z");
        for i in 0..n {
            let at = start + chrono::Duration::seconds(i as i64);
            handle.submit_document(local_job("s", &[i as i64], at));
            let mut feed = job("cvi_params", spx());
            feed.source_time = at;
            feed.received_at = at;
            handle.submit_document(feed);
        }
        for _ in 0..2 * n {
            match next_event(&rx) {
                IngestEvent::Published { .. } => {}
                other => panic!("{other:?}"),
            }
        }
        handle.shutdown();
        drop(rx);
        assert_eq!(
            count_in(
                &path,
                "select count(distinct gen_id) from sheets_document_archive"
            ),
            LOCAL_KEEP_GENERATIONS as i64,
            "the archive keeps the bound"
        );
        assert_eq!(
            count_in(
                &path,
                "select count(*) from generations where dataset = 'sheets'"
            ),
            LOCAL_KEEP_GENERATIONS as i64 + 1,
            "live plus the kept archive"
        );
        assert_eq!(
            count_in(&path, "select min(qty) from sheets_document_archive"),
            4,
            "the oldest four generations went, not arbitrary ones"
        );
        assert_eq!(
            count_in(
                &path,
                "select count(distinct gen_id) from cvi_params_document_archive"
            ),
            n as i64 - 1,
            "a feed's history is not swept by a local publish"
        );
        assert_eq!(
            count_in(
                &path,
                "select count(*) from file_generations where dataset = 'sheets'"
            ),
            LOCAL_KEEP_GENERATIONS as i64 + 1,
            "provenance is pruned with the sweep"
        );
    }
}
