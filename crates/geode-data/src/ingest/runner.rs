//! The ingest runner (spec §5.4–§5.7). Owns the writer connection on its
//! own thread, works a priority-ordered queue, and never takes the app down.
//!
//! One thread, not a pool, deliberately: DuckDB is single-writer so every
//! publish serializes anyway (spec §5.3). Whether parsing in parallel across
//! separate connections buys anything is a benchmark question the spec
//! declines to assume (spec §5.6), and Task 14 measures it.
//!
//! Preemption granularity is one file: the queue is re-sorted on every
//! submit, so a newly landed current file jumps ahead of remaining backfill
//! without interrupting a load in flight (spec §5.4).

use crate::health::Health;
use crate::ingest::load::{LoadRequest, load_file};
use crate::ingest::plan::{WorkItem, WorkPlan};
use crate::source::{CandidateState, Priority};
use crate::store::Store;
use geode_core::schema::DatasetSpec;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone)]
pub enum IngestEvent {
    Published {
        dataset: String,
        batch: String,
        gen_id: i64,
        books: Vec<String>,
        rows: usize,
        health: Health,
    },
    Failed {
        batch: String,
        reason: String,
    },
    /// The queue drained. Not a terminal state — more work may be submitted.
    PlanComplete,
}

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
    pub fn spawn(
        store: Store,
        dataset: DatasetSpec,
        dataset_name: String,
    ) -> (IngestHandle, Receiver<IngestEvent>) {
        let (tx, rx) = channel();
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);

        let thread = std::thread::Builder::new()
            .name("geode-ingest".into())
            .spawn(move || run(store, dataset, dataset_name, worker_queue, tx))
            .expect("spawning the ingest thread");

        (
            IngestHandle {
                queue,
                thread: Mutex::new(Some(thread)),
            },
            rx,
        )
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

fn run(
    store: Store,
    dataset: DatasetSpec,
    dataset_name: String,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    tx: Sender<IngestEvent>,
) {
    loop {
        let item = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                if !q.items.is_empty() {
                    break q.items.remove(0);
                }
                if tx.send(IngestEvent::PlanComplete).is_err() {
                    return;
                }
                let (guard, _) = cvar
                    .wait_timeout(q, std::time::Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner());
                q = guard;
            }
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
                    dataset: &dataset,
                    dataset_name: &dataset_name,
                    csv_path: &item.candidate.csv_path,
                    sentinel,
                    batch: &item.batch,
                },
            )
            .map_err(|e| e.to_string())
        }));

        let event = match outcome {
            Ok(Ok(loaded)) => IngestEvent::Published {
                dataset: dataset_name.clone(),
                batch: item.batch.clone(),
                gen_id: loaded.gen_id,
                books: match &item.candidate.state {
                    CandidateState::Ready(s) => s.books.clone(),
                    _ => Vec::new(),
                },
                rows: loaded.rows,
                health: loaded.health,
            },
            Ok(Err(reason)) => IngestEvent::Failed {
                batch: item.batch.clone(),
                reason,
            },
            Err(_) => IngestEvent::Failed {
                batch: item.batch.clone(),
                reason: "ingest task panicked".into(),
            },
        };
        if tx.send(event).is_err() {
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
    use std::time::Duration;

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

        let (handle, rx) = IngestRunner::spawn(store, ds, "risk_snapshot".into());
        handle.submit(plan);
        let events = drain(&rx, expected);
        handle.shutdown();

        let published = events
            .iter()
            .filter(|e| matches!(e, IngestEvent::Published { .. }))
            .count();
        assert_eq!(published, expected);
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

        let (handle, rx) = IngestRunner::spawn(store, ds, "risk_snapshot".into());
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

        let (handle, rx) = IngestRunner::spawn(store, ds, "risk_snapshot".into());
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
    fn shutdown_is_idempotent_and_does_not_hang() {
        let (_db, _src, store, ds, _plan) = harness();
        let (handle, _rx) = IngestRunner::spawn(store, ds, "risk_snapshot".into());
        handle.shutdown();
        handle.shutdown();
    }
}
