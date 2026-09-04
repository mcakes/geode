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
use crate::source::{CandidateState, Priority};
use crate::store::Store;
use geode_core::schema::SchemaSpec;
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
    /// re-sorted, so a current file preempts pending backfill.
    pub fn submit(&self, plan: WorkPlan) {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.items.extend(plan.items);
        q.items.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(b.source_time.cmp(&a.source_time))
        });
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
                    break q.items.remove(0);
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

        // The dataset is resolved per item (Phase 3 §2.5). An undeclared
        // one is this item's failure, named, and the runner carries on.
        let Some(dataset) = schema.dataset(&item.dataset) else {
            if !sink(IngestEvent::Failed {
                dataset: item.dataset.clone(),
                batch: item.batch.clone(),
                reason: format!("dataset '{}' is not declared", item.dataset),
            }) {
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
        if !sink(event) {
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

        // Force everything to Backfill, then submit one current item.
        for item in &mut plan.items {
            item.priority = Priority::Backfill;
        }
        let current = {
            let mut c = plan.items[plan.items.len() - 1].clone();
            c.priority = Priority::LatestRisk;
            c
        };
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
}
