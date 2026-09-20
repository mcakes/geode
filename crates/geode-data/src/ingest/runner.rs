//! The ingest runner (spec §5.4–§5.7, Phase 3 §2.5). One thread owning
//! the writer connection for **every** dataset, working three queues —
//! parsed documents, fetched series rows, and a priority-ordered queue
//! of files — never taking the app down.
//!
//! A queued document is taken **ahead of** any file, whatever the file's
//! priority (market-data spec §5.4, amended in Task 11: the rung wording
//! there predates this ruling), and a queued series job is taken
//! **after** documents but **ahead of** any file (timeseries spec §5.4,
//! Task 7): an append was asked for by a trader watching a chart, a file
//! was found by a poll nobody is waiting on. Two reasons documents lead.
//! A document publish is milliseconds — the rows are already parsed and
//! already coalesced to the latest per key upstream (`ingest::coalesce`),
//! so there is nothing to read, split or scan — and so it cannot starve a
//! file load however many arrive: the file it jumps is delayed by the
//! length of one appender pass. And a single rule spares the runner a
//! second priority vocabulary: documents carry no `Priority`, and
//! interleaving them with files by one would mean inventing and
//! maintaining a comparison between two things that are never actually
//! competing for the same time.
//!
//! `Queue::in_flight` belongs to the file arm alone — it is the dedupe
//! key discovery's own polls are checked against, and neither a document
//! nor a series job has a file, a `stat` or a poll, so nothing on either
//! path reads or writes it.
//!
//! One thread, not a pool, and one for all datasets rather than one per
//! dataset: DuckDB is single-writer, so every publish serializes anyway
//! (spec §5.3), and a runner per dataset would make that discipline a
//! convention held by whoever spawned them. The `Store` — and with it the
//! writer — lives here; the service keeps only reader connections cloned
//! before the store moved (Phase 3 §5.3).
//!
//! **Moving staging onto a pool is planned but on hold** — read
//! `docs/ingest-cold-start-handoff.md` first. The 1.87× in `docs/perf.md`
//! measures `read_csv` alone, not staging, and `staging_raw` /
//! `staging_{grain}` are fixed global names that concurrent staging would
//! overwrite. This paragraph becomes wrong the day that lands; rewrite it
//! rather than leaving it to mislead.
//!
//! Preemption granularity is one file: the queue is re-sorted on every
//! submit, so a newly landed current file jumps ahead of remaining backfill
//! without interrupting a load in flight (spec §5.4).

use crate::adapter::SeriesRows;
use crate::health::Health;
use crate::ingest::load::{LoadError, LoadOutcome, LoadRequest, load_file};
use crate::ingest::plan::{WorkItem, WorkPlan};
use crate::source::discovery::is_unchanged;
use crate::source::{CandidateState, Priority};
use crate::store::document::{
    DocumentPublishRequest, DocumentPublished, document_path, publish_document,
};
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
    /// The runner popped a job and is about to load it (spec 2026-09-17
    /// §5.3): `path` is the file's own path, or `document://{source}/
    /// {dataset}` for a document (its batch is not known until the rows
    /// are read, and the strip never needs it); `queued` is how many
    /// items — files and documents — still waited behind it at the
    /// instant it was popped. For a file, emitted only after the pop-time
    /// stale re-check passes (2026-09-19 final review, finding 1) — a
    /// re-queued already-loaded file produces no `Started` at all, rather
    /// than one with nothing to end it. Always followed by exactly one
    /// `Published` or `Failed` for the same job: one runner, one FIFO
    /// queue.
    Started {
        source: String,
        path: String,
        queued: usize,
    },
    Published {
        /// The `[sources.<name>]` this item came from (Phase 4b final
        /// review, MAJ-1) — distinct from `dataset`: `SourceSpec` names
        /// and datasets are two separate fields, and the service must key
        /// `DataEvent::Health` by this, never by `dataset`.
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
    /// A series job appended (timeseries spec §5.4 step 3). `appended`
    /// may be 0 — an overlapping refetch — and the event is still sent,
    /// because coverage was recorded and the asking tile must requery.
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
    /// The queue drained. Not a terminal state — more work may be submitted.
    PlanComplete,
}

/// Where events go. `false` means "this event was not delivered" —
/// the caller's bounded channel was full, or its receiver is gone. It is
/// never a shutdown signal: the runner keeps working through one and the
/// event is simply dropped (Phase 4b follow-up, Task 1; the runner's only
/// stop is `IngestHandle::shutdown`). Called from the runner's own thread,
/// so a sink must not block indefinitely — a channel send is fine.
pub type IngestSink = Arc<dyn Fn(IngestEvent) -> bool + Send + Sync>;

/// A parsed document waiting to publish (market-data spec §5.4 step 3).
/// The rows are **owned**: the receiver thread parses into
/// struct-of-arrays and hands the columns over, keeping nothing, so the
/// publish reads no buffer another thread could still be writing and
/// nothing here is cloned per row (PHILOSOPHY §6).
///
/// `source` is the `[sources.<name>]` name and `dataset` the dataset it
/// feeds — two separate fields for the reason `IngestEvent::Published`
/// records (Phase 4b's MAJ-1), not one name used twice.
///
/// `bytes` and `received_at` are what a document has instead of a file's
/// length and mtime: a message carries no `stat`, but provenance still
/// wants both, and `source_time` — the time *the feed* stamped, not the
/// time we saw it — is what the backfill guard and as-of order by.
#[derive(Debug)]
pub struct DocumentJob {
    pub source: String,
    pub dataset: String,
    pub rows: DocumentRows,
    pub source_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub bytes: u64,
}

/// Fetched rows waiting to append (timeseries spec §5.4). Owned, like
/// `DocumentJob`'s rows, for the same reason.
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
    documents: VecDeque<DocumentJob>,
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

/// The work a file load does. A function pointer rather than a direct
/// call to [`load_file`] so a test can inject one that panics — the
/// same shape as `geode_data::query::pool::RunFn`, and for the same
/// reason: there is no CSV that makes `load_file` itself panic, and
/// panic-safety (Phase 4b Task 6: the file and the panic payload land
/// in the reported `Failed.reason`) is the property worth testing here.
type LoadFn = fn(&Store, &LoadRequest) -> Result<LoadOutcome, LoadError>;

/// The work a document publish does, injectable for exactly the reason
/// [`LoadFn`] is: no document makes [`publish_document`] itself panic
/// (`DocumentRows::validate` turns every malformed one into an `Err`
/// before a byte is written), so containment — the property spec §5.7
/// actually asks for on this path — is only testable through an injected
/// publish that does.
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
    /// Add work. Items are merged into the queue and the whole queue is
    /// re-sorted, so a current file preempts pending backfill. Returns how
    /// many of `plan`'s items were newly queued (see `enqueue`), for
    /// tests: the runner's own pop-time re-check (below) independently
    /// guarantees a duplicate is never *loaded* twice regardless of what
    /// this dedupe does, so a test that only observes `IngestEvent`s
    /// cannot isolate this method's own contribution from that backstop.
    ///
    /// See `enqueue` for the dedupe and promotion rule this applies.
    /// Discovery polls on its own clock and re-reports every file the
    /// catalog does not yet reflect — including one this queue already
    /// holds, or one the runner is in the middle of loading, whose catalog
    /// record is written only at the end of the load. Without dedupe, a
    /// poll shorter than a load re-adds a copy of every not-yet-published
    /// file every time it runs: the production symptom was 3051
    /// generations of 17 files that never changed, even though (as the doc
    /// above notes) the pop-time re-check alone would already have kept
    /// every one of those copies from actually reloading — what the
    /// dedupe adds on top is bounding how large the queue, and how many
    /// wasted pop-time catalog lookups, a quiet poll interval can pile up.
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

    /// Hand a parsed document to the runner. Returns nothing, and refuses
    /// nothing: unlike `submit`, there is no dedupe to report on — the
    /// coalescer upstream already keeps at most one pending document per
    /// key, so the deduplication this queue would otherwise need has
    /// already happened where it can also *replace* a pending document
    /// rather than merely drop a duplicate.
    ///
    /// Cheap enough to call from a receiver thread on every message
    /// (spec's "nothing blocks a producer"): it moves the already-parsed
    /// columns into a `VecDeque` under the queue lock and wakes the
    /// runner. It never blocks on the runner itself.
    pub fn submit_document(&self, job: DocumentJob) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.documents.push_back(job);
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

/// The queue mutation `submit` performs under its lock, pulled out as a
/// free function so a test can drive it synchronously against a bare
/// `Queue` — no runner thread, no timing at all — which is the only way
/// to exercise the `in_flight` half of the dedupe condition directly
/// rather than incidentally through `submit`'s return value.
///
/// An incoming item naming the same `(csv_path, size, source_time)` as
/// one already queued or in flight is not queued again — `size` is part
/// of the key, not just `source_time`, because that is exactly the pair
/// `is_unchanged` compares (`source/discovery.rs`); a key coarser than
/// the change-detection rule it defends would let two different sizes at
/// one source time collapse into "the same file".
///
/// If the match is against an item still sitting in the queue (not one
/// already in flight — that copy is past the point of reprioritising),
/// its priority is promoted to the better of the two
/// (`existing.priority.min(item.priority)`) rather than the new offer
/// being dropped with no effect. `build_plan` decides an item's priority
/// fresh on every poll from whichever candidate is currently the newest
/// *Ready* one for its batch (spec §5.4): once today's file loads and
/// reads `Unchanged`, yesterday's still-queued file can become that
/// newest-Ready candidate and get offered at the source's own priority
/// instead of `Backfill`. Without promotion, a file already queued would
/// stay stuck at whatever priority it first queued under, sitting behind
/// another source's current work it should now jump ahead of.
///
/// Returns how many items were newly queued; a promotion does not count,
/// since nothing new entered the queue.
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
    Document(DocumentJob),
    Series(SeriesJob),
    File(WorkItem),
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

/// Publishes one queued document, reporting the outcome as the same
/// `IngestEvent`s a file load reports — a document *is* a file to
/// everything downstream (generations, as-of, retention, the freshness
/// catalog), so a second event vocabulary would only make the service
/// and the diagnostics tile handle the same publish twice.
///
/// A free function rather than an arm of `run`'s `match`, so the file
/// body below keeps the one indentation level it has always had: the
/// alternative re-indents every line of it, which is both a diff nobody
/// can review and — concretely — a break of the six mutation anchors
/// sitting inside it. It takes `job` by value because the rows die with
/// the publish.
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

    // The same boundary the file arm uses (spec §5.7): a panicking publish
    // degrades its own document and the runner keeps working. `contained`
    // is what tells the process-wide panic hook this one is handled, so it
    // logs at `error` instead of writing a crash file.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| {
            publish(
                store,
                &DocumentPublishRequest {
                    dataset,
                    source: &job.source,
                    rows: &job.rows,
                    source_time: job.source_time,
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
    if !sink(event) {
        log_refused_event(
            refusal_logged,
            &format!("the document publish outcome for {}/{batch}", job.dataset),
        );
    }
}

/// The series arm of the runner (timeseries spec §5.4 step 3), the
/// document arm's shape exactly: resolve the dataset by name, append
/// under `contained`, announce the outcome, count a refused announcement.
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
    // PlanComplete is announced once per drain, on the transition from
    // working to idle — not on every wakeup. An idle runner would otherwise
    // push an event every poll interval, forever, into an unbounded channel.
    let mut announced_idle = false;
    // One line per runner, not one per refused event (final review,
    // MIN-3): before Task 1 a gone receiver ended this thread after a
    // single refusal, so it could never repeat. Now the thread lives
    // on, and an unlatched warning would fill the 4,096-entry log ring
    // — the one in-process log a diagnostics tile reads — with copies
    // of itself. The caller's `dropped` counter stays the
    // authoritative count.
    let refusal_logged = AtomicBool::new(false);

    loop {
        let work = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
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
                        // Logged with the lock released, the same rule
                        // `query::pool`'s delivery site follows (fix
                        // round 1, MIN-5): formatting a warning under
                        // the queue mutex blocks `submit`. Re-acquired
                        // and re-checked from the top, so a shutdown or
                        // an item that landed meanwhile is seen at once
                        // rather than after the 50 ms wait.
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

        // A document is published here and the loop starts over: none of
        // the file machinery below applies to it — no pop-time change
        // detection (there is no file to re-`stat`), no `in_flight` to
        // clear (it was never set), no sentinel. Its `Started` is emitted
        // right here, before the publish, since a document has no stale
        // check to emit it after — except for a local publish
        // (`LOCAL_SOURCE`, line-pricer §5.5, final-review finding 3): a
        // sheet autosave must never blink the ingest progress strip, so
        // its `Started`/`Loading` is skipped entirely. The unconditional
        // `LoadEnded` the service sends after every publish is untouched
        // — the strip tolerates a `LoadEnded` with no matching `Started`.
        let item = match work {
            Work::Document(job) => {
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

        // Pop-time re-check, the other half of the dedupe: the queue can
        // still hold a copy of a file that finished loading — under a
        // different, no-longer-in-flight copy — while this one waited.
        // `submit`'s dedupe (above) catches the common case; this catches
        // what slips past it (spec §5.7: not a failure, so no event).
        //
        // Wrapped in its own panic boundary, same reasoning as the load's
        // below: `lookup_by_path` unwraps every column it reads
        // (`store/catalog.rs`), so a `file_generations` row an older build
        // wrote in a shape this build does not expect can panic here
        // rather than return `Err`. A panic and a `StoreError` both fail
        // open — "not stale", so the load proceeds rather than an item
        // being silently dropped by a check that itself broke — but the
        // two are matched as distinct arms so a future diagnostic can
        // tell "the read broke" from "nothing was found" apart, even
        // though both currently do the same thing.
        //
        // Fix round 1, MIN-10: unlike the load boundary twenty lines
        // down, a panic caught here produces no `IngestEvent` — it is
        // swallowed into "not stale" by `.unwrap_or(false)` below. It is
        // not silent: the boundary runs under `geode_core::panic::
        // contained`, so the process-wide hook logs it at `error` as a
        // contained panic (message and location) without a crash file.
        // What it lacks is the load arm's *named* event with the file
        // path; recorded as a known asymmetry rather than fixed here.
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

        // Emitted only now — after the stale check passed — so a
        // re-queued already-loaded file (finding 1, 2026-09-19 final
        // review) never starts the strip: a `Started` with no
        // `Published`/`Failed` to follow it would leave the status bar
        // stuck.
        if !sink(IngestEvent::Started {
            source: item.source.clone(),
            path: item.candidate.csv_path.to_string_lossy().into_owned(),
            queued,
        }) {
            log_refused_event(&refusal_logged, "a load-started announcement");
        }

        // The dataset is resolved per item (Phase 3 §2.5). An undeclared
        // one is this item's failure, named, and the runner carries on.
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

        // Panic boundary (spec §5.7): a panicking load degrades its file and
        // the runner keeps working. Only a render-thread panic takes the app
        // down.
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
                // The panic's payload (Phase 4b Task 6, spec §4.7's
                // as-built deviation: a panicking load is `Failed`, not
                // `Degraded` — the plan's ruling): `&str`/`String` cover
                // every `panic!`/`unwrap`/`expect` in practice; anything
                // else (a custom payload type) falls back to a named
                // placeholder rather than losing the event.
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

/// A refused event was not delivered — the caller's bounded channel was
/// momentarily full, or its receiver is gone. Either way the runner keeps
/// going (Phase 4b follow-up, Task 1: exit-on-false was never the shutdown
/// path — `IngestHandle::shutdown` + `Drop` is, and one cold-start burst
/// filling a 256-slot channel used to end ingest for the session). Logged
/// once per RUNNER, never retried: the health transitions that matter are
/// re-offered by `HealthTracker` on the next report, and `latched` keeps a
/// permanently gone receiver from filling the log ring with copies of this
/// line (final review, MIN-3). A free function for the same reason
/// `log_ingest_panic` is one: it fires on the runner's own thread, so a
/// test can only reach it directly.
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

/// A `catch_unwind` payload as text, for the `Failed.reason` a panicking
/// load produces (Phase 4b Task 6). `panic!`/`unwrap`/`expect` payloads
/// are always `&'static str` or `String`; anything else (a custom
/// `panic_any` payload) falls back to a named placeholder rather than
/// losing the event.
///
/// `pub(crate)` for the receiver thread's own boundary
/// (`ingest::subscribe`), which reports a panicking parse the same way
/// this one reports a panicking load: one spelling of a payload, so two
/// `Failed.reason`s a trader reads side by side in the diagnostics tile
/// cannot describe the same panic differently.
pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}

/// Logs a panicking load at `geode::ingest` `error`, with the file and
/// payload (Phase 4b Task 6 fix round 1, MAJ-2). Pulled out as a free
/// function — the crate's established pattern for a log line a test
/// needs to drive directly, `service.rs`'s `log_health_event`/
/// `log_ingest_failure` — for the same reason those exist: this event
/// fires on the spawned `geode-ingest` thread, and
/// `tracing::subscriber::with_default`'s scope is thread-local, so a
/// test running on the *test* thread could never observe it if it
/// stayed inline in `run`.
///
/// Fix round 1, MIN-9: a panicking load is logged at `error` twice —
/// this line (which has the file) and `service.rs`'s own
/// `log_ingest_failure` a moment later on the same thread (which has
/// the dataset/batch, reading `Failed.reason` — the string this event's
/// `message` also feeds). Both are correct per their own task's brief;
/// recorded here so two `geode::ingest` errors for one failure reads as
/// a decision, not a surprise, to whoever next looks at the ring or the
/// diagnostics tile for this source.
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

    /// A sink that forwards into a channel but REFUSES the first event
    /// matching `refuse` — returning `false` without sending it. `false`
    /// means "this event was not delivered", never "stop producing"
    /// (Phase 4b follow-up, Task 1): every test below asserts the runner
    /// kept working through one. The returned counter is how many
    /// refusals actually happened, so a test can wait for the refusal
    /// rather than racing it.
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
        // A momentarily full event channel is not a shutdown signal: the
        // runner's own `shutdown` is (Phase 4b follow-up, Task 1).
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
        // A DIFFERENT file: sharing `plan.items[0]`'s `(csv_path, size,
        // source_time)` races `clear_in_flight`, which runs after the
        // sink call this test waits on, so the resubmission could be
        // dropped as an in-flight duplicate (fix round 1, MIN-1).
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

        // Force everything to Backfill, then pull one item *out* to
        // resubmit separately at LatestRisk priority — a poll finding this
        // file newly current. It must be removed from `plan` first: this
        // test used to clone it in place instead, submitting the same
        // (csv_path, source_time) twice at different priorities and
        // expecting *two* Published events for it. That is the ladder
        // defect A fixed (submit's own dedupe now drops the second copy),
        // so `expected` would then wait forever on a publish that no
        // longer comes. Removing it first keeps this test about
        // preemption, not a second covert case of the dedupe.
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
        // Unlike the test above (a brand-new file jumping the queue), this
        // is the *same* file resubmitted at a better priority — a poll
        // finding that yesterday's still-queued file is now the newest
        // Ready candidate for its batch, once whatever used to eclipse it
        // has already loaded (spec §5.4, `build_plan`). The resubmission
        // must promote the queued entry rather than being dropped with no
        // effect, or the file would be stuck behind other Backfill work it
        // should now jump ahead of.
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
        assert_eq!(
            published, good,
            "one bad file must not stop the run (spec §5.7)"
        );
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

    /// `service.rs`'s own `logged(...)` pattern (Phase 4b Task 2 fix
    /// round 1, MAJ-1), reproduced here rather than shared: `log_ingest_
    /// panic` fires on the spawned `geode-ingest` thread, so — unlike
    /// `a_panicking_load_names_the_file_and_the_panic_payload` above,
    /// which only sees the `Failed.reason` string that crosses the
    /// channel — this drives the free function directly, on the test
    /// thread, against a scoped ring subscriber (fix round 1, MAJ-2:
    /// exactly why it was pulled out of `run` as a free function).
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
        // A gone receiver refuses every event for the rest of the
        // session; unlatched, this line alone would evict the 4,096-entry
        // ring a diagnostician came to read (final review, MIN-3).
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

    #[test]
    fn submit_drops_an_item_already_queued_for_the_same_file_and_source_time() {
        // `submit` used to `extend` the queue unconditionally: two plans
        // naming the same (path, source_time) queued the file twice. This
        // is the common case the production ladder came from — a poll
        // re-adding a file that is already waiting to load.
        //
        // The assertion has to be on `submit`'s own return value, not on
        // the eventual `IngestEvent` stream: the runner's pop-time
        // re-check (a separate defence) independently guarantees a
        // duplicate is never *loaded* twice regardless of what `submit`
        // does, so "exactly one Published" is true even with this
        // dedupe disabled — it would not isolate this method's own
        // contribution from that backstop.
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
        // The other half of the fix: even a duplicate that slips past
        // `submit`'s dedupe (e.g. it was already popped and in flight when
        // the duplicate arrived) must be harmless once the file it names
        // has actually finished loading. Load one file directly, bypassing
        // the runner, then submit it alongside a second, genuinely unloaded
        // file — the runner must publish nothing for the first and still
        // load the second.
        //
        // The second file is what makes this deterministic rather than
        // racing `PlanComplete` (which fires once before the stale item is
        // even popped, and gives no event at all for a silent stale skip):
        // giving the stale item strictly higher priority guarantees the
        // single-threaded runner pops and dispenses with it *before* the
        // fresh one, so waiting for the fresh item's own Published event
        // is proof enough that the stale item was already handled.
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

        // Finding 1 (2026-09-19 final review): a stale-skipped file must
        // never even announce a `Started` — one with no `Published`/
        // `Failed` to follow it would start the status bar's strip and
        // leave it stuck.
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
        // The pop-time re-check calls `lookup_by_path`, which unwraps
        // every column it reads (`store/catalog.rs`). A `file_generations`
        // row this build cannot read the shape of — here, `mtime` is
        // NULL, which the read side never expects — panics there instead
        // of returning an `Err`. That panic must degrade only the item
        // being popped (fail open: "not stale", so its own load proceeds
        // normally) and must not take the ingest thread down with it
        // (spec §5.7): a second, distinct item queued behind it must
        // still publish.
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

    // ---- documents (market-data spec §5.4 step 3) ----------------------

    /// A store that can hold the spec's own document dataset: the schema
    /// applied and the catalog tables created, exactly as
    /// `store::document`'s own fixture builds one. Separate from
    /// `harness()` because a document needs no source directory and no
    /// generated CSV at all.
    fn document_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&cvi_dataset()).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    /// The next outcome event, skipping the idle announcements a runner
    /// legitimately emits before and between work — `PlanComplete` fires
    /// once when the runner starts on an empty queue, which races any
    /// submit, so no test may treat it as positional — and `Started`,
    /// which now always precedes the outcome these tests are watching
    /// for (Task 1).
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

    /// line-pricer §5.5, final-review finding 3: a local publish
    /// (`LOCAL_SOURCE`) must never blink the ingest progress strip — no
    /// `IngestEvent::Started` for it, though `Published` still fires.
    /// Reads the raw stream (not `next_event`, which skips `Started` on
    /// purpose) so an unwanted `Started` cannot hide from the assertion.
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
        q.documents.push_back(job("cvi_params", spx()));
        match take_work(&mut q) {
            Some(Work::Document(d)) => assert_eq!(d.rows.key, vec!["SPX.Z".to_string()]),
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

    // ---- series (timeseries spec §5.4 step 3) ---------------------------

    /// A store that can hold the timeseries spec's own series dataset —
    /// schema applied, catalog tables created — the way `document_store`
    /// builds one for the document family.
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
        // Every lane populated at once, in the reverse of pop order, so a
        // swap of any two `pop_front`s in `take_work` (documents/series or
        // series/files) is caught here rather than passing every other
        // test in the crate — the earlier version of this test queued no
        // document at all and so could not see that first ordering break.
        let mut q = Queue::default();
        q.documents.push_back(job("cvi_params", spx()));
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
}
