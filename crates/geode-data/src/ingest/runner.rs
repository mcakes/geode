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

use crate::health::Health;
use crate::ingest::load::{LoadRequest, load_file};
use crate::ingest::plan::{WorkItem, WorkPlan};
use crate::source::discovery::is_unchanged;
use crate::source::{CandidateState, Priority};
use crate::store::{Catalog, Store};
use chrono::{DateTime, Utc};
use geode_core::schema::SchemaSpec;
use std::path::PathBuf;
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
/// runner. Called from the runner's own thread with no lock held, so a
/// sink must not block indefinitely — a channel send is fine.
pub type IngestSink = Arc<dyn Fn(IngestEvent) -> bool + Send + Sync>;

#[derive(Default)]
struct Queue {
    items: Vec<WorkItem>,
    shutdown: bool,
    /// The file the runner has popped and is loading (or is about to skip
    /// as stale) right now, if any. Set when an item is popped, cleared
    /// once that item's outcome — `Published`, `Failed`, or a silent
    /// stale-skip — has been reported. Kept for the whole load, not just
    /// before it starts, so `submit`'s dedupe still sees it as spoken for
    /// the entire time a poll could otherwise re-add it.
    in_flight: Option<(PathBuf, DateTime<Utc>)>,
}

pub struct IngestHandle {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

pub struct IngestRunner;

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
    /// many of `plan`'s items were actually enqueued, for tests: the
    /// runner's own pop-time re-check (below) independently guarantees a
    /// duplicate is never *loaded* twice regardless of what this dedupe
    /// does, so a test that only observes `IngestEvent`s cannot isolate
    /// this method's own contribution from that backstop.
    ///
    /// An incoming item is dropped if an item naming the same
    /// `(csv_path, source_time)` is already queued or is the item the
    /// runner is loading right now (`Queue::in_flight`). Discovery polls
    /// on its own clock and re-reports every file the catalog does not yet
    /// reflect — including one this queue already holds, or one the runner
    /// is in the middle of loading, whose catalog record is written only
    /// at the end of the load. Without this, a poll shorter than a load
    /// re-adds a copy of every not-yet-published file every time it runs:
    /// the production symptom was 3051 generations of 17 files that never
    /// changed, even though (as the doc above notes) the pop-time re-check
    /// alone would already have kept every one of those copies from
    /// actually reloading — what this dedupe adds on top is bounding how
    /// large the queue, and how many wasted pop-time catalog lookups, a
    /// quiet poll interval can pile up.
    pub fn submit(&self, plan: WorkPlan) -> usize {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut enqueued = 0;
        for item in plan.items {
            let key = (item.candidate.csv_path.clone(), item.source_time);
            let already_queued = q.in_flight.as_ref() == Some(&key)
                || q.items.iter().any(|existing| {
                    (&existing.candidate.csv_path, existing.source_time) == (&key.0, key.1)
                });
            if already_queued {
                continue;
            }
            q.items.push(item);
            enqueued += 1;
        }
        q.items.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(b.source_time.cmp(&a.source_time))
        });
        cvar.notify_all();
        enqueued
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

fn run(store: Store, schema: SchemaSpec, queue: Arc<(Mutex<Queue>, Condvar)>, sink: IngestSink) {
    // PlanComplete is announced once per drain, on the transition from
    // working to idle — not on every wakeup. An idle runner would otherwise
    // push an event every poll interval, forever, into an unbounded channel.
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
                    let it = q.items.remove(0);
                    q.in_flight = Some((it.candidate.csv_path.clone(), it.source_time));
                    break it;
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

        // Pop-time re-check, the other half of the dedupe: the queue can
        // still hold a copy of a file that finished loading — under a
        // different, no-longer-in-flight copy — while this one waited.
        // `submit`'s dedupe (above) catches the common case; this catches
        // what slips past it (spec §5.7: not a failure, so no event).
        let stale = match Catalog::new(store.writer()).lookup_by_path(&item.candidate.csv_path) {
            Ok(Some(prev)) => is_unchanged(&prev, item.candidate.size, item.source_time),
            _ => false,
        };
        if stale {
            clear_in_flight(&queue);
            continue;
        }

        // The dataset is resolved per item (Phase 3 §2.5). An undeclared
        // one is this item's failure, named, and the runner carries on.
        let Some(dataset) = schema.dataset(&item.dataset) else {
            let failed = sink(IngestEvent::Failed {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: format!("dataset '{}' is not declared", item.dataset),
            });
            clear_in_flight(&queue);
            if !failed {
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
        let delivered = sink(event);
        // Cleared only now: while the load ran, `in_flight` kept a
        // duplicate submitted mid-load harmless.
        clear_in_flight(&queue);
        if !delivered {
            return; // receiver gone: nothing left to report to
        }
    }
}

/// Backfill items yield between files so current work is not starved. The
/// queue re-sort in `submit` is what actually reorders; this exists so the
/// intent is testable and named.
pub fn is_preemptible(item: &WorkItem) -> bool {
    item.priority == Priority::Backfill
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Priority;
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
            name: "risk".into(),
            dataset: "risk_snapshot".into(),
            paths: vec![format!("{}/*.csv", src_dir.path().display())],
            readiness: crate::source::Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: Duration::from_secs(30),
            pending_timeout: Duration::from_secs(3600),
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
        };
        let cat = crate::store::Catalog::new(store.writer());
        let found = crate::source::discover(&spec, &cat, std::time::SystemTime::now()).unwrap();
        let plan = crate::ingest::build_plan(&[(spec, found)]);
        (db_dir, src_dir, store, ds, plan)
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

        assert!(
            events.iter().any(
                |e| matches!(e, IngestEvent::Published { batch, .. } if *batch == fresh.batch)
            ),
            "the fresh file must still load: {events:?}"
        );
        assert!(
            events.iter().all(|e| !matches!(
                e,
                IngestEvent::Published { batch, .. } | IngestEvent::Failed { batch, .. }
                    if *batch == stale.batch
            )),
            "a file already loaded before this item was popped must not \
             reload: {events:?}"
        );
    }
}
