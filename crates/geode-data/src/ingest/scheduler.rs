//! One discovery thread polls directory sources at their own intervals and
//! submits ready plans to ingest. Polling works on network shares where file
//! watches can miss changes. Every source is initially due immediately.
//!
//! Polls run sequentially: slow filesystem I/O delays other sources and
//! shutdown. Panic containment does not provide an I/O timeout.

use crate::health::{Health, severity_rank};
use crate::ingest::IngestHandle;
use crate::ingest::plan::build_plan;
use crate::source::{CandidateState, SourceSpec, discover};
use crate::store::Catalog;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerEvent {
    /// Poll completion. `ready` counts planned files before runner deduplication,
    /// not newly enqueued jobs. `next_in` is the configured interval used for the
    /// service's next-poll estimate.
    Polled {
        source: String,
        ready: usize,
        next_in: Duration,
    },
    /// The worst thing discovery found. Never modal, never fatal.
    Health {
        source: String,
        worst: Health,
        detail: String,
    },
}

/// Nonblocking discovery event sink. False means delivery was refused, not
/// that polling should stop. Shutdown signals the scheduler's stop condition.
pub type SchedulerSink = Arc<dyn Fn(SchedulerEvent) -> bool + Send + Sync>;

pub struct Scheduler {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Scheduler {
    pub fn spawn(
        sources: Vec<SourceSpec>,
        conn: duckdb::Connection,
        ingest: Arc<IngestHandle>,
        sink: SchedulerSink,
    ) -> Scheduler {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("geode-discovery".into())
            .spawn(move || run(sources, conn, ingest, sink, worker_stop))
            .expect("spawning the discovery thread");
        Scheduler {
            stop,
            thread: Mutex::new(Some(thread)),
        }
    }

    pub fn shutdown(&self) {
        {
            let (lock, cvar) = &*self.stop;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cvar.notify_all();
        }
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Sleep until `until` or until told to stop. `true` means stop.
fn wait_until(stop: &(Mutex<bool>, Condvar), until: Instant) -> bool {
    let (lock, cvar) = stop;
    let mut stopped = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if *stopped {
            return true;
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        let (guard, _) = cvar
            .wait_timeout(stopped, until - now)
            .unwrap_or_else(|e| e.into_inner());
        stopped = guard;
    }
}

/// Record health and poll-result refusals independently so the warning names
/// every undelivered event.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Refused {
    health: bool,
    polled: bool,
}

impl Refused {
    /// A poll that never got as far as its `Polled` — the discovery
    /// error and panic arms, which report health and nothing else.
    fn health(refused: bool) -> Refused {
        Refused {
            health: refused,
            polled: false,
        }
    }

    /// What to name in the log, or `None` when everything landed. A pure
    /// function so the message is testable without a scheduler thread.
    fn what(self) -> Option<&'static str> {
        match (self.health, self.polled) {
            (true, true) => Some("the health report and the poll result"),
            (true, false) => Some("the health report"),
            (false, true) => Some("the poll result"),
            (false, false) => None,
        }
    }
}

fn run(
    sources: Vec<SourceSpec>,
    conn: duckdb::Connection,
    ingest: Arc<IngestHandle>,
    sink: SchedulerSink,
    stop: Arc<(Mutex<bool>, Condvar)>,
) {
    if sources.is_empty() {
        return;
    }
    // Warn once per scheduler about refused delivery to preserve log capacity.
    // Delivery counters retain the full refusal count.
    let refusal_logged = AtomicBool::new(false);
    // Everything is due now: the first sweep is the cold start.
    let mut due: Vec<(Instant, usize)> = (0..sources.len()).map(|i| (Instant::now(), i)).collect();

    loop {
        due.sort_by_key(|(t, _)| *t);
        let (when, i) = due[0];
        if wait_until(&stop, when) {
            return;
        }
        let spec = &sources[i];

        // Contain the whole poll, including planning, submission, and normal sink
        // calls. Report errors/panics as source failure, then rearm. This boundary
        // does not interrupt a blocked filesystem call; failure reporting also
        // requires the sink itself to return normally.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<Refused, crate::store::StoreError> {
                geode_core::panic::contained(|| {
                    let candidates = discover(spec, &Catalog::new(&conn), SystemTime::now())?;
                    let health = worst_health(&candidates);
                    // Report every poll, including clean state. The service's shared tracker
                    // combines discovery with load health and deduplicates transitions.
                    let health_delivered = match health {
                        Some((worst, detail)) => sink(SchedulerEvent::Health {
                            source: spec.name.clone(),
                            worst,
                            detail,
                        }),
                        None => sink(SchedulerEvent::Health {
                            source: spec.name.clone(),
                            worst: Health::Ok,
                            detail: String::new(),
                        }),
                    };
                    let plan = build_plan(&[(spec.clone(), candidates)]);
                    let ready = plan.items.len();
                    if ready > 0 {
                        ingest.submit(plan);
                    }
                    // Attempt Polled independently of the health delivery verdict.
                    let polled_delivered = sink(SchedulerEvent::Polled {
                        source: spec.name.clone(),
                        ready,
                        next_in: spec.poll_interval,
                    });
                    Ok(Refused {
                        health: !health_delivered,
                        polled: !polled_delivered,
                    })
                })
            },
        ));

        let refused = match outcome {
            Ok(Ok(refused)) => refused,
            Ok(Err(e)) => Refused::health(!sink(SchedulerEvent::Health {
                source: spec.name.clone(),
                worst: Health::Failed {
                    reason: e.to_string(),
                },
                detail: format!("discovery failed: {e}"),
            })),
            Err(_) => Refused::health(!sink(SchedulerEvent::Health {
                source: spec.name.clone(),
                worst: Health::Failed {
                    reason: "discovery panicked".into(),
                },
                detail: "discovery panicked".into(),
            })),
        };
        if let Some(what) = refused.what() {
            log_refused_discovery(&refusal_logged, what, &spec.name);
        }
        // Re-arm from *now*, not from `when`: a slow share must not make
        // the next poll immediately due and spin.
        due[0] = (Instant::now() + spec.poll_interval, i);
    }
}

/// Warn once about refused diagnostics and continue polling. The scheduler
/// does not retry individual events; the service can reoffer an unacknowledged
/// health transition on the next report.
fn log_refused_discovery(latched: &AtomicBool, what: &str, source: &str) {
    if latched.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::warn!(
        target: "geode::ingest",
        "event channel refused {what} for source '{source}': dropped, polling \
         continues (further refusals are counted, not logged)",
    );
}

/// Select by severity rank and include every file at the worst rank. Equal
/// severity must not compare reason strings alphabetically. The returned
/// Health carries the first candidate's reason; detail includes individual
/// reasons when they differ and a shared label when they agree.
fn worst_health(candidates: &[crate::source::Candidate]) -> Option<(Health, String)> {
    let mut worst: Vec<(Health, String)> = Vec::new();
    for c in candidates {
        let h = match &c.state {
            CandidateState::PendingTooLong => Health::PendingTooLong,
            CandidateState::Orphaned { reason } => Health::Degraded {
                reason: reason.clone(),
            },
            CandidateState::Ready(_) | CandidateState::Pending | CandidateState::Unchanged => {
                continue;
            }
        };
        let name = c
            .csv_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let incumbent_rank = worst.first().map(|(w, _)| severity_rank(w));
        match incumbent_rank {
            Some(r) if r == severity_rank(&h) => worst.push((h, name)),
            Some(r) if r > severity_rank(&h) => {}
            _ => worst = vec![(h, name)],
        }
    }
    let first = worst.first()?.0.clone();
    let first_reason = reason(&first);
    let reasons_differ = worst.iter().any(|(h, _)| reason(h) != first_reason);
    let names = worst.iter().map(|(h, name)| {
        if reasons_differ && let Some(r) = reason(h) {
            return format!("{name} ({r})");
        }
        name.clone()
    });
    let detail = format!(
        "{}: {}",
        first.label(),
        names.collect::<Vec<_>>().join(", ")
    );
    Some((first, detail))
}

/// `h`'s reason, without `to_parts()`'s always-allocated label —
/// `worst_health` calls this once per candidate at the worst rank, on
/// every scheduler poll.
fn reason(h: &Health) -> Option<&str> {
    match h {
        Health::Degraded { reason } | Health::Failed { reason } => Some(reason.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{IngestEvent, IngestRunner};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    /// A store with the fixture schema, an empty source directory, and a
    /// runner delivering into a channel.
    fn harness(
        poll: Duration,
        pending_timeout: Duration,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Arc<IngestHandle>,
        Receiver<IngestEvent>,
        duckdb::Connection,
        SourceSpec,
        geode_core::schema::DatasetSpec,
    ) {
        let (db, src, store, ds, _emitted) = crate::ingest::load::tests_support::fixture();
        // The fixture emitted files; the tests below want an empty
        // directory to start from, so use a fresh one.
        let empty = tempfile::tempdir().unwrap();
        let conn = store.reader().unwrap();
        let mut schema = geode_core::schema::SchemaSpec::default();
        schema.datasets.push(ds.clone());
        let (handle, rx) = IngestRunner::spawn_channel(store, schema);
        let spec = SourceSpec {
            poll_interval: poll,
            pending_timeout,
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            ..SourceSpec::directory(
                "risk",
                "risk_snapshot",
                vec![format!("{}/*.csv", empty.path().display())],
            )
        };
        drop(src);
        (db, empty, Arc::new(handle), rx, conn, spec, ds)
    }

    fn events_sink() -> (SchedulerSink, Receiver<SchedulerEvent>) {
        let (tx, rx) = channel();
        (Arc::new(move |e| tx.send(e).is_ok()), rx)
    }

    /// Refuse the first matching event and count it. Tests wait for the refusal
    /// before verifying discovery continues.
    fn refusing_events_sink(
        refuse: fn(&SchedulerEvent) -> bool,
    ) -> (
        SchedulerSink,
        Receiver<SchedulerEvent>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let (tx, rx) = channel();
        let refusals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&refusals);
        let sink: SchedulerSink = Arc::new(move |e: SchedulerEvent| {
            // Claimed atomically: load-then-add
            // could refuse twice if two threads ever shared this sink.
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

    /// A candidate at `name`'s path in the given state; every other
    /// field is filler `worst_health` never reads.
    fn candidate(name: &str, state: CandidateState) -> crate::source::Candidate {
        crate::source::Candidate {
            csv_path: std::path::PathBuf::from(name),
            sentinel_path: std::path::PathBuf::from(format!("{name}.done")),
            batch: "b".into(),
            size: 0,
            mtime: SystemTime::now(),
            state,
        }
    }

    #[test]
    fn two_orphaned_candidates_with_different_reasons_are_both_named() {
        // Equal-severity candidates with different reasons must both appear in
        // the detail. Using derived `Health::Ord` would compare reason text and
        // select only one; different-variant candidates would not expose that error.
        let candidates = vec![
            candidate(
                "b.csv",
                CandidateState::Orphaned {
                    reason: "no header".into(),
                },
            ),
            candidate(
                "a.csv",
                CandidateState::Orphaned {
                    reason: "expected value at line 1".into(),
                },
            ),
        ];
        let (health, detail) = worst_health(&candidates).expect("both candidates are unhealthy");
        assert_eq!(
            health,
            Health::Degraded {
                reason: "no header".into(),
            },
            "the health carries the FIRST candidate's reason, in candidate order"
        );
        assert_eq!(
            detail,
            "degraded: b.csv (no header), a.csv (expected value at line 1)"
        );
    }

    #[test]
    fn a_higher_rank_candidate_replaces_the_names_accumulated_at_a_lower_rank() {
        // Two PendingTooLong candidates accumulate details at the same rank.
        // A later Orphaned candidate has higher severity and must replace both
        // names, keeping only candidates at the current worst rank.
        let candidates = vec![
            candidate("a.csv", CandidateState::PendingTooLong),
            candidate("b.csv", CandidateState::PendingTooLong),
            candidate(
                "c.csv",
                CandidateState::Orphaned {
                    reason: "no header".into(),
                },
            ),
        ];
        let (health, detail) =
            worst_health(&candidates).expect("all three candidates are unhealthy");
        assert_eq!(
            health,
            Health::Degraded {
                reason: "no header".into(),
            },
        );
        assert_eq!(
            detail, "degraded: c.csv",
            "the two lower-rank names must be replaced, not kept alongside the winner"
        );
    }

    #[test]
    fn a_refused_event_does_not_stop_the_scheduler() {
        // Refused event delivery must not stop subsequent discovery polls.
        let poll = Duration::from_millis(20);
        let (_db, _dir, ingest, _ingest_rx, conn, spec, _ds) =
            harness(poll, Duration::from_secs(3600));
        let (sink, sched_rx, refusals) =
            refusing_events_sink(|e| matches!(e, SchedulerEvent::Polled { .. }));
        let sched = Scheduler::spawn(vec![spec], conn, Arc::clone(&ingest), sink);

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline
            && refusals.load(std::sync::atomic::Ordering::SeqCst) == 0
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            refusals.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the first Polled event must have been refused"
        );

        // A later poll must still arrive: the refusal cost one event, not
        // the whole thread.
        let mut later = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            match sched_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(e @ SchedulerEvent::Polled { .. }) => {
                    later = Some(e);
                    break;
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        sched.shutdown();
        assert!(
            later.is_some(),
            "the source must be polled again after a refused event"
        );
    }

    /// Records logged while `f` runs, on this thread only — the same
    /// scoped-subscriber pattern `runner.rs`'s own test module uses.
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
    fn a_refusal_is_logged_once_per_scheduler_not_once_per_poll() {
        // Repeated delivery refusal must produce only one warning per scheduler.
        let latch = AtomicBool::new(false);
        let records = logged(|| {
            log_refused_discovery(&latch, "the health report", "risk");
            log_refused_discovery(&latch, "the poll result", "risk");
        });
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].level, tracing::Level::WARN);
        assert_eq!(records[0].target, "geode::ingest");
        assert!(records[0].message.contains("risk"), "{records:?}");
    }

    #[test]
    fn the_warning_names_which_of_a_polls_events_was_refused() {
        assert_eq!(Refused::default().what(), None);
        assert_eq!(
            Refused {
                health: true,
                polled: false
            }
            .what(),
            Some("the health report")
        );
        assert_eq!(
            Refused {
                health: false,
                polled: true
            }
            .what(),
            Some("the poll result")
        );
        assert_eq!(
            Refused {
                health: true,
                polled: true
            }
            .what(),
            Some("the health report and the poll result")
        );
    }

    #[test]
    fn a_refused_health_does_not_swallow_that_polls_result() {
        // Refuse Health and verify the same poll still attempts Polled. The two
        // delivery verdicts must not short-circuit each other.
        let (_db, _dir, ingest, _ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(20), Duration::from_secs(3600));
        let (sink, sched_rx, refusals) =
            refusing_events_sink(|e| matches!(e, SchedulerEvent::Health { .. }));
        let sched = Scheduler::spawn(vec![spec], conn, Arc::clone(&ingest), sink);

        let first = sched_rx.recv_timeout(Duration::from_secs(30));
        sched.shutdown();
        let first = first.expect("a poll whose Health was refused must still report its result");
        assert!(
            matches!(first, SchedulerEvent::Polled { .. }),
            "expected this poll's own Polled, not the next poll's Health: {first:?}"
        );
        assert_eq!(
            refusals.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one Health was refused"
        );
    }

    #[test]
    fn a_file_that_appears_after_start_is_discovered_and_published() {
        // A file arriving after startup must be found by a later poll.
        let (_db, dir, ingest, ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(50), Duration::from_secs(3600));
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, Arc::clone(&ingest), sink);

        // Skip the first clean health report while waiting for poll completion.
        let mut first = sched_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        if matches!(
            first,
            SchedulerEvent::Health {
                worst: Health::Ok,
                ..
            }
        ) {
            first = sched_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        assert!(
            matches!(first, SchedulerEvent::Polled { ready: 0, .. }),
            "{first:?}"
        );

        // Now a file lands.
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 500,
            seed: 7,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(dir.path());
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts).unwrap();

        let mut published = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            match ingest_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(IngestEvent::Published { .. }) => {
                    published += 1;
                    break;
                }
                Ok(IngestEvent::Failed { reason, .. }) => panic!("{reason}"),
                _ => {}
            }
        }
        assert_eq!(published, 1, "the file that landed after start was loaded");
        sched.shutdown();
    }

    #[test]
    fn an_unchanged_directory_submits_nothing_on_later_polls() {
        let poll = Duration::from_millis(20);
        let (_db, _dir, ingest, ingest_rx, conn, spec, _ds) =
            harness(poll, Duration::from_secs(3600));
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, Arc::clone(&ingest), sink);
        let mut polls = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while polls < 5 && std::time::Instant::now() < deadline {
            if let Ok(SchedulerEvent::Polled { ready, next_in, .. }) =
                sched_rx.recv_timeout(Duration::from_secs(10))
            {
                assert_eq!(ready, 0);
                assert_eq!(next_in, poll, "next_in is this source's poll_interval");
                polls += 1;
            }
        }
        assert_eq!(polls, 5, "five polls within 30s");
        sched.shutdown();
        // The runner announced idle once at most and published nothing.
        while let Ok(e) = ingest_rx.try_recv() {
            assert!(matches!(e, IngestEvent::PlanComplete), "{e:?}");
        }
    }

    #[test]
    fn a_csv_pending_past_its_timeout_is_a_health_event() {
        let (_db, dir, ingest, _ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(50), Duration::ZERO);
        std::fs::write(
            dir.path().join("risk_2026-09-03_BK000.csv"),
            "Book\nBK000\n",
        )
        .unwrap();
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, ingest, sink);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = None;
        while std::time::Instant::now() < deadline {
            if let Ok(SchedulerEvent::Health { worst, detail, .. }) =
                sched_rx.recv_timeout(Duration::from_secs(1))
            {
                seen = Some((worst, detail));
                break;
            }
        }
        let (worst, detail) = seen.expect("a health event");
        assert_eq!(worst, Health::PendingTooLong);
        assert!(detail.contains("BK000"), "{detail}");
        sched.shutdown();
    }

    /// A recovered source must report Ok after PendingTooLong.
    #[test]
    fn a_degraded_source_that_recovers_emits_an_ok_health_event() {
        let (_db, dir, ingest, _ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(50), Duration::ZERO);
        std::fs::write(
            dir.path().join("risk_2026-09-03_BK000.csv"),
            "Book\nBK000\n",
        )
        .unwrap();
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, ingest, sink);

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut degraded = false;
        while std::time::Instant::now() < deadline {
            if let Ok(SchedulerEvent::Health {
                worst: Health::PendingTooLong,
                ..
            }) = sched_rx.recv_timeout(Duration::from_secs(1))
            {
                degraded = true;
                break;
            }
        }
        assert!(degraded, "setup: the source must degrade first");

        // The sentinel lands: the file becomes Ready, so the next poll's
        // `worst_health` returns None.
        std::fs::write(
            dir.path().join("risk_2026-09-03_BK000.csv.done"),
            r#"{"as_of":"2026-09-03T07:00:00Z","columns":["Book"],"books":["BK000"]}"#,
        )
        .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut recovered = false;
        while std::time::Instant::now() < deadline {
            if let Ok(SchedulerEvent::Health {
                worst: Health::Ok, ..
            }) = sched_rx.recv_timeout(Duration::from_secs(1))
            {
                recovered = true;
                break;
            }
        }
        assert!(recovered, "a recovered source must report Health::Ok");
        sched.shutdown();
    }

    /// The scheduler reports Ok on every clean poll. Transition deduplication
    /// belongs to the service, where discovery and load outcomes are combined.
    #[test]
    fn a_steadily_healthy_source_reports_ok_on_every_poll() {
        let poll = Duration::from_millis(20);
        let (_db, _dir, ingest, _ingest_rx, conn, spec, _ds) =
            harness(poll, Duration::from_secs(3600));
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, ingest, sink);
        let mut polls = 0;
        let mut ok_count = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while polls < 8 && std::time::Instant::now() < deadline {
            match sched_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(SchedulerEvent::Polled { .. }) => polls += 1,
                Ok(SchedulerEvent::Health {
                    worst: Health::Ok, ..
                }) => ok_count += 1,
                _ => {}
            }
        }
        assert_eq!(polls, 8, "eight polls within 30s");
        assert_eq!(
            ok_count, 8,
            "the scheduler emits Ok on every clean poll now — dedup lives \
             in DataService's shared HealthTracker"
        );
        sched.shutdown();
    }

    #[test]
    fn no_sources_means_the_thread_exits_and_shutdown_does_not_hang() {
        let (_db, _dir, ingest, _rx, conn, _spec, _ds) =
            harness(Duration::from_secs(1), Duration::from_secs(1));
        let (sink, _sched_rx) = events_sink();
        let sched = Scheduler::spawn(Vec::new(), conn, ingest, sink);
        sched.shutdown();
        sched.shutdown();
    }

    /// Poll faster than files load so discovery repeatedly sees unpublished work.
    /// Runner deduplication must prevent duplicate generations and allow the
    /// queue to drain once each unchanged file is loaded.
    #[test]
    fn a_poll_shorter_than_a_load_does_not_reload_files_already_queued() {
        let (_db, dir, ingest, ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(20), Duration::from_secs(3600));

        // The shared `harness()` above drops its own fixture's source
        // directory before returning (its tests want to start empty), so a
        // second, independent fixture supplies the ready files this test
        // copies into `dir`.
        let (_db2, _src2, _store2, _ds2, emitted) = crate::ingest::load::tests_support::fixture();
        let ready_files: Vec<&geode_demo_data::EmittedFile> = emitted
            .files
            .iter()
            .filter(|f| f.sentinel_path.is_some())
            .collect();
        assert!(
            ready_files.len() >= 3,
            "need several ready files to observe the ladder: {}",
            ready_files.len()
        );
        for f in &emitted.files {
            std::fs::copy(
                &f.csv_path,
                dir.path().join(f.csv_path.file_name().unwrap()),
            )
            .unwrap();
            if let Some(sp) = &f.sentinel_path {
                std::fs::copy(sp, dir.path().join(sp.file_name().unwrap())).unwrap();
            }
        }

        // A second reader on the same database, cloned before `conn` moves
        // into the scheduler, for the honest check below.
        let query_conn = conn.try_clone().unwrap();
        let (sink, _sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, Arc::clone(&ingest), sink);

        // Allow time for real CSV loads and publication transactions under parallel
        // test load. Duplicate publication is checked separately before this timeout.
        let mut published = 0;
        let mut idle = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        while std::time::Instant::now() < deadline {
            match ingest_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(IngestEvent::Published { .. }) => {
                    published += 1;
                    // Fail immediately if publications exceed the number of files: duplicate
                    // work may prevent the queue from ever becoming idle.
                    assert!(
                        published <= ready_files.len(),
                        "{published} publishes for {} files — duplicates",
                        ready_files.len()
                    );
                }
                Ok(IngestEvent::PlanComplete) if published >= 1 => {
                    idle = true;
                    break;
                }
                Ok(IngestEvent::Failed { reason, .. }) => panic!("{reason}"),
                _ => {}
            }
        }
        sched.shutdown();
        ingest.shutdown();
        assert!(
            idle,
            "the runner never went idle within 45s ({published} published so far)"
        );

        assert_eq!(
            published,
            ready_files.len(),
            "every ready file must publish exactly once, not once per \
             intervening poll"
        );

        // The honest check: a `Published` count can match by accident if a
        // file were both reloaded and something else under-counted. What
        // must actually be true is that the catalog holds exactly one
        // generation per path.
        let mut stmt = query_conn
            .prepare("select path, count(*) from file_generations group by path")
            .unwrap();
        let counts: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(counts.len(), ready_files.len(), "{counts:?}");
        for (path, c) in &counts {
            assert_eq!(*c, 1, "{path} has {c} generations, expected exactly one");
        }
    }
}
