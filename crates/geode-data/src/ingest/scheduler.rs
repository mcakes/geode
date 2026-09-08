//! Discovery on a schedule (Phase 3 spec §5.3, foundation §5.1). One
//! thread walks every configured source on its own interval, builds a
//! plan from what is ready, and hands it to the ingest runner. Polling,
//! never watching: `notify` is unreliable over SMB (§11).
//!
//! Every source is polled once immediately at start, so cold start is
//! the same code path as the thirtieth poll, and discovery I/O happens
//! here where nothing waits on it.

use crate::health::Health;
use crate::ingest::IngestHandle;
use crate::ingest::plan::build_plan;
use crate::source::{CandidateState, SourceSpec, discover};
use crate::store::Catalog;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerEvent {
    /// One poll finished; `ready` is how many files were handed to the
    /// runner. Tests wait on this; the service maps it to
    /// `DataEvent::Polled` (Phase 4b §4.4's last/next-poll diagnostic).
    /// `next_in` is this source's `poll_interval` at the moment of this
    /// poll — the service adds it to "now" to get the next poll's
    /// estimated time.
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
    // Everything is due now: the first sweep is the cold start.
    let mut due: Vec<(Instant, usize)> = (0..sources.len()).map(|i| (Instant::now(), i)).collect();

    loop {
        due.sort_by_key(|(t, _)| *t);
        let (when, i) = due[0];
        if wait_until(&stop, when) {
            return;
        }
        let spec = &sources[i];

        // The whole poll is the panic boundary (spec §5.7), not just
        // `discover`: building the plan, submitting it, and the sink
        // calls all run inside `catch_unwind` too. A panic anywhere in
        // here — a bad glob, a share that hangs, a sink that panics —
        // must degrade this source and let the thread carry on to the
        // next one. Left partly outside, a panic in `build_plan` or
        // `ingest.submit` would unwind straight out of this thread; the
        // `JoinHandle` from `spawn` is never inspected for `Err`, so the
        // thread would die silently and every other source would stop
        // being polled with nothing on the sink to say so — exactly the
        // silence spec §5.7 forbids.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<bool, crate::store::StoreError> {
                geode_core::panic::contained(|| {
                    let candidates = discover(spec, &Catalog::new(&conn), SystemTime::now())?;
                    let health = worst_health(&candidates);
                    let ok = match health {
                        Some((worst, detail)) => sink(SchedulerEvent::Health {
                            source: spec.name.clone(),
                            worst,
                            detail,
                        }),
                        None => true,
                    };
                    let plan = build_plan(&[(spec.clone(), candidates)]);
                    let ready = plan.items.len();
                    if ready > 0 {
                        ingest.submit(plan);
                    }
                    Ok(ok
                        && sink(SchedulerEvent::Polled {
                            source: spec.name.clone(),
                            ready,
                            next_in: spec.poll_interval,
                        }))
                })
            },
        ));

        let delivered = match outcome {
            Ok(Ok(delivered)) => delivered,
            Ok(Err(e)) => sink(SchedulerEvent::Health {
                source: spec.name.clone(),
                worst: Health::Failed {
                    reason: e.to_string(),
                },
                detail: format!("discovery failed: {e}"),
            }),
            Err(_) => sink(SchedulerEvent::Health {
                source: spec.name.clone(),
                worst: Health::Failed {
                    reason: "discovery panicked".into(),
                },
                detail: "discovery panicked".into(),
            }),
        };
        if !delivered {
            return;
        }
        // Re-arm from *now*, not from `when`: a slow share must not make
        // the next poll immediately due and spin.
        due[0] = (Instant::now() + spec.poll_interval, i);
    }
}

/// The worst candidate state and a detail line naming the files in it.
fn worst_health(candidates: &[crate::source::Candidate]) -> Option<(Health, String)> {
    let mut worst: Option<(Health, Vec<String>)> = None;
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
        match &mut worst {
            Some((w, names)) if *w == h => names.push(name),
            Some((w, _)) if *w > h => {}
            _ => worst = Some((h, vec![name])),
        }
    }
    worst.map(|(h, names)| {
        let detail = format!("{}: {}", h.label(), names.join(", "));
        (h, detail)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{IngestEvent, IngestRunner};
    use crate::source::{Priority, Readiness};
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
            name: "risk".into(),
            dataset: "risk_snapshot".into(),
            paths: vec![format!("{}/*.csv", empty.path().display())],
            readiness: Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: poll,
            pending_timeout,
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
        };
        drop(src);
        (db, empty, Arc::new(handle), rx, conn, spec, ds)
    }

    fn events_sink() -> (SchedulerSink, Receiver<SchedulerEvent>) {
        let (tx, rx) = channel();
        (Arc::new(move |e| tx.send(e).is_ok()), rx)
    }

    #[test]
    fn a_file_that_appears_after_start_is_discovered_and_published() {
        // The whole point of the scheduler (Phase 3 §2.8): the probe
        // discovered once and never again.
        let (_db, dir, ingest, ingest_rx, conn, spec, _ds) =
            harness(Duration::from_millis(50), Duration::from_secs(3600));
        let (sink, sched_rx) = events_sink();
        let sched = Scheduler::spawn(vec![spec], conn, Arc::clone(&ingest), sink);

        // First poll: nothing there.
        let first = sched_rx.recv_timeout(Duration::from_secs(10)).unwrap();
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

    #[test]
    fn no_sources_means_the_thread_exits_and_shutdown_does_not_hang() {
        let (_db, _dir, ingest, _rx, conn, _spec, _ds) =
            harness(Duration::from_secs(1), Duration::from_secs(1));
        let (sink, _sched_rx) = events_sink();
        let sched = Scheduler::spawn(Vec::new(), conn, ingest, sink);
        sched.shutdown();
        sched.shutdown();
    }

    /// The production symptom (2026-09-07 display): 3051 generations of 17
    /// files that never changed, loaded over an hour of 2s polls. `submit`
    /// never deduplicates, and `discover` only knows the catalog — not what
    /// is already queued or being loaded right now — so every poll shorter
    /// than a load re-adds a copy of every file not yet published. This
    /// reproduces the ladder at a scale a unit test can afford: a 20ms
    /// poll against files that each take longer than that to load.
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

        // Bounded generously rather than tightly: this fixture has 15 real
        // files to load (real DuckDB I/O, one publish transaction each),
        // and under `cargo test --workspace`'s full parallel load that
        // legitimately took longer than a tighter 10s bound allowed,
        // failing a run that was simply still working, not stuck. A
        // regression reproduces at a wholly different scale — the RED run
        // against the unfixed code still hadn't gone idle after 10s with
        // 45 *duplicate* publishes and climbing (the display's own case
        // took an hour) — so this bound stays tight enough to fail fast on
        // an actual defect while giving legitimate contention real room.
        let mut published = 0;
        let mut idle = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        while std::time::Instant::now() < deadline {
            match ingest_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(IngestEvent::Published { .. }) => {
                    published += 1;
                    // Fail on the defect itself, immediately, rather than
                    // waiting out the full 45s bound: a regression means
                    // published keeps climbing past the file count (the
                    // RED run against `a18d0a9` reached 45 duplicate
                    // publishes in the first 10s alone, still climbing),
                    // and the two assertions below — the honest ones —
                    // are otherwise unreachable in that case, since `idle`
                    // never becomes true first. This makes the 45s bound
                    // something only a genuinely slow-but-correct run
                    // ever pays in full.
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
