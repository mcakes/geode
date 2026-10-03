//! One supervised thread per snapshot source: poll at start, then every
//! `interval`, or sooner on `poll_now`. A poll asked for while one runs
//! becomes one follow-up poll, not a queue. Polls only read and validate;
//! publishing belongs to the ingest runner, the writer's only owner.

use crate::adapter::{AdapterError, SnapshotQuery};
use chrono::{DateTime, Utc};
use geode_core::reference::ConformedRows;
use geode_core::schema::DatasetSpec;
use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

/// One snapshot source's polling contract: read `table` into `dataset`
/// every `interval`.
#[derive(Debug, Clone)]
pub struct SnapshotPlan {
    pub source: String,
    pub dataset: DatasetSpec,
    pub table: String,
    pub interval: Duration,
}

#[derive(Debug)]
pub enum SnapshotOutcome {
    /// A conformed table. `received_at` is when the poll started, the
    /// publish's source time. `note` is the once-per-combination warning
    /// for ignored or NULL-read columns.
    Rows {
        rows: ConformedRows,
        received_at: DateTime<Utc>,
        note: Option<String>,
    },
    /// The query failed, panicked, or its rows were refused; nothing to
    /// publish, and the live table stays as it was.
    Failed { reason: String },
}

/// Receives each poll's outcome on the snapshot thread. Keep callbacks
/// short: the next wait and shutdown cannot advance until it returns.
pub type SnapshotSink = Arc<dyn Fn(SnapshotOutcome) + Send + Sync>;

/// Told after every poll: when it finished and when the next is due.
pub type PolledSink = Arc<dyn Fn(SystemTime, SystemTime) + Send + Sync>;

#[derive(Default)]
struct Wake {
    stop: bool,
    poll_now: bool,
}

pub struct SnapshotWorker {
    source: String,
    dataset: String,
    wake: Arc<(Mutex<Wake>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl SnapshotWorker {
    pub fn spawn(
        plan: SnapshotPlan,
        query: Box<dyn SnapshotQuery>,
        sink: SnapshotSink,
        polled: PolledSink,
        stop: crate::service::EventSink,
    ) -> Result<SnapshotWorker, AdapterError> {
        let source = plan.source.clone();
        let dataset = plan.dataset.name.clone();
        let wake = Arc::new((Mutex::new(Wake::default()), Condvar::new()));
        let name = format!("geode-snapshot-{source}");
        let thread_wake = wake.clone();
        let thread = crate::supervise::spawn_supervised(name.clone(), stop, move || {
            run(plan, query, sink, polled, thread_wake)
        })
        .map_err(|e| AdapterError {
            message: format!("spawning {name}: {e}"),
        })?;
        Ok(SnapshotWorker {
            source,
            dataset,
            wake,
            thread: Some(thread),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn dataset(&self) -> &str {
        &self.dataset
    }

    /// Ask for a poll now. Never blocks; while a poll runs, any number of
    /// asks collapse into one follow-up poll.
    pub fn poll_now(&self) {
        let (lock, cvar) = &*self.wake;
        lock.lock().unwrap_or_else(|e| e.into_inner()).poll_now = true;
        cvar.notify_one();
    }

    /// Stop and join. A poll in flight finishes (its outcome still reaches
    /// the sink) before the thread sees the stop, as with fetch workers.
    /// Idempotent; also on `Drop`.
    pub fn shutdown(&mut self) {
        let (lock, cvar) = &*self.wake;
        lock.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        cvar.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SnapshotWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Undeclared/missing column combinations already warned about, capped like
/// file loads so a source with churning columns cannot grow it unbounded.
#[derive(Default)]
pub(crate) struct NoteMemory {
    seen: HashSet<(Vec<String>, Vec<String>)>,
    capped: bool,
}

const NOTE_CAP: usize = 256;

impl NoteMemory {
    /// The warning for this combination of ignored (`extra`) and NULL-read
    /// (`missing`) columns, or `None` when both are empty or it was already
    /// reported. Past `NOTE_CAP` one final cap warning, then `None`.
    pub(crate) fn note(
        &mut self,
        source: &str,
        table: &str,
        dataset: &str,
        extra: &[String],
        missing: &[String],
    ) -> Option<String> {
        if extra.is_empty() && missing.is_empty() {
            return None;
        }
        let key = (extra.to_vec(), missing.to_vec());
        if self.capped || self.seen.contains(&key) {
            return None;
        }
        if self.seen.len() >= NOTE_CAP {
            self.capped = true;
            return Some(format!(
                "source '{source}' reached the column-warning cap; later combinations go unreported"
            ));
        }
        self.seen.insert(key);
        let mut parts = Vec::new();
        if !extra.is_empty() {
            parts.push(format!("extra columns [{}] ignored", extra.join(", ")));
        }
        if !missing.is_empty() {
            parts.push(format!(
                "optional [{}] missing, read as NULL",
                missing.join(", ")
            ));
        }
        Some(format!(
            "'{table}' snapshot into '{dataset}' with {}",
            parts.join("; ")
        ))
    }
}

/// One read-and-conform under `contained`, like every background boundary
/// in this crate: a vendor driver that panics fails this poll, logs, and
/// leaves the worker polling.
pub(crate) fn poll_once(
    query: &mut dyn SnapshotQuery,
    plan: &SnapshotPlan,
    notes: &mut NoteMemory,
) -> SnapshotOutcome {
    let received_at = Utc::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| query.query(&plan.table))
    }));
    let rows = match result {
        Ok(Ok(rows)) => rows,
        Ok(Err(e)) => {
            return SnapshotOutcome::Failed {
                reason: format!("query '{}' failed: {}", plan.table, e.message),
            };
        }
        Err(payload) => {
            let message = crate::ingest::runner::panic_payload_message(payload.as_ref());
            tracing::error!(
                target: "geode::ingest",
                "a snapshot of '{}' from '{}' panicked: {message}",
                plan.table,
                plan.source
            );
            return SnapshotOutcome::Failed {
                reason: format!("query '{}' panicked: {message}", plan.table),
            };
        }
    };
    match rows.conform(&plan.dataset) {
        Err(reason) => SnapshotOutcome::Failed {
            reason: format!("snapshot of '{}' refused: {reason}", plan.table),
        },
        Ok(rows) => {
            let note = notes.note(
                &plan.source,
                &plan.table,
                &plan.dataset.name,
                &rows.extra,
                &rows.missing,
            );
            SnapshotOutcome::Rows {
                rows,
                received_at,
                note,
            }
        }
    }
}

/// The worker thread: poll, report the schedule, then wait until the
/// interval elapses, `poll_now` is asked, or `stop` is set. The flags are
/// read under the lock before every wait, so a wake sent mid-poll is never
/// lost. An interval too large to add to the clock (a typo such as
/// `"18446744073709551615s"`) is no deadline at all: the next poll is
/// reported as `at`, and only `poll_now` or `stop` wakes the worker.
fn run(
    plan: SnapshotPlan,
    mut query: Box<dyn SnapshotQuery>,
    sink: SnapshotSink,
    polled: PolledSink,
    wake: Arc<(Mutex<Wake>, Condvar)>,
) {
    let mut notes = NoteMemory::default();
    loop {
        sink(poll_once(query.as_mut(), &plan, &mut notes));
        let at = SystemTime::now();
        polled(at, at.checked_add(plan.interval).unwrap_or(at));
        let deadline = Instant::now().checked_add(plan.interval);
        let (lock, cvar) = &*wake;
        let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if w.stop {
                return;
            }
            if std::mem::take(&mut w.poll_now) {
                break;
            }
            w = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    cvar.wait_timeout(w, deadline - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
                None => cvar.wait(w).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::reference::{RefColumn, TableRows};
    use std::sync::mpsc;

    struct Scripted(Vec<Result<TableRows, AdapterError>>);
    impl SnapshotQuery for Scripted {
        fn query(&mut self, _: &str) -> Result<TableRows, AdapterError> {
            if self.0.is_empty() {
                Ok(good())
            } else {
                self.0.remove(0)
            }
        }
    }
    struct Panics;
    impl SnapshotQuery for Panics {
        fn query(&mut self, _: &str) -> Result<TableRows, AdapterError> {
            panic!("driver exploded")
        }
    }

    fn ds() -> DatasetSpec {
        geode_core::reference::test_support::reference_dataset()
    }
    /// Every declared column, so a good poll carries no note.
    fn good() -> TableRows {
        TableRows {
            columns: vec![
                (
                    "underlying_ref".into(),
                    RefColumn::Utf8(vec![Some("SPX".into())]),
                ),
                ("currency".into(), RefColumn::Utf8(vec![Some("USD".into())])),
                ("multiplier".into(), RefColumn::F64(vec![Some(100.0)])),
            ],
        }
    }
    fn plan() -> SnapshotPlan {
        SnapshotPlan {
            source: "refdb".into(),
            dataset: ds(),
            table: "underlyings".into(),
            interval: Duration::from_secs(3600),
        }
    }

    #[test]
    fn a_good_snapshot_is_conformed() {
        let mut q = Scripted(vec![]);
        match poll_once(&mut q, &plan(), &mut NoteMemory::default()) {
            SnapshotOutcome::Rows { rows, note, .. } => {
                assert_eq!(rows.rows, 1);
                assert_eq!(note, None, "every column declared and present");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_adapter_error_fails_the_poll_with_its_message() {
        let mut q = Scripted(vec![Err(AdapterError {
            message: "login timed out".into(),
        })]);
        match poll_once(&mut q, &plan(), &mut NoteMemory::default()) {
            SnapshotOutcome::Failed { reason } => assert!(reason.contains("login timed out")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_refused_snapshot_fails_the_poll() {
        let mut q = Scripted(vec![Ok(TableRows::default())]);
        assert!(matches!(
            poll_once(&mut q, &plan(), &mut NoteMemory::default()),
            SnapshotOutcome::Failed { .. }
        ));
    }

    #[test]
    fn a_panicking_query_fails_the_poll_instead_of_the_thread() {
        match poll_once(&mut Panics, &plan(), &mut NoteMemory::default()) {
            SnapshotOutcome::Failed { reason } => assert!(reason.contains("driver exploded")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_extra_column_is_noted_once_per_combination() {
        let mut rows = good();
        rows.columns
            .push(("isin".into(), RefColumn::Utf8(vec![Some("x".into())])));
        let mut notes = NoteMemory::default();
        let mut q = Scripted(vec![Ok(rows.clone()), Ok(rows)]);
        let first = poll_once(&mut q, &plan(), &mut notes);
        let second = poll_once(&mut q, &plan(), &mut notes);
        assert!(
            matches!(first, SnapshotOutcome::Rows { note: Some(ref n), .. } if n.contains("isin")),
            "{first:?}"
        );
        assert!(
            matches!(second, SnapshotOutcome::Rows { note: None, .. }),
            "{second:?}"
        );
    }

    #[test]
    fn a_missing_optional_column_is_noted_as_null() {
        let rows = TableRows {
            columns: vec![(
                "underlying_ref".into(),
                RefColumn::Utf8(vec![Some("SPX".into())]),
            )],
        };
        let mut q = Scripted(vec![Ok(rows)]);
        match poll_once(&mut q, &plan(), &mut NoteMemory::default()) {
            SnapshotOutcome::Rows { note: Some(n), .. } => assert_eq!(
                n,
                "'underlyings' snapshot into 'u' with optional [currency, multiplier] \
                 missing, read as NULL"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn notes_past_the_cap_end_with_one_cap_warning() {
        let mut notes = NoteMemory::default();
        for i in 0..NOTE_CAP {
            let n = notes.note("refdb", "underlyings", "u", &[format!("c{i}")], &[]);
            assert!(n.is_some(), "combination {i} is new");
        }
        let cap = notes.note("refdb", "underlyings", "u", &["late".into()], &[]);
        assert_eq!(
            cap.as_deref(),
            Some("source 'refdb' reached the column-warning cap; later combinations go unreported")
        );
        assert_eq!(
            notes.note("refdb", "underlyings", "u", &["later".into()], &[]),
            None
        );
    }

    #[test]
    fn the_worker_polls_at_start_and_on_demand() {
        let (tx, rx) = mpsc::channel();
        let sink: SnapshotSink = Arc::new(move |o| {
            let _ = tx.send(matches!(o, SnapshotOutcome::Rows { .. }));
        });
        let (ptx, prx) = mpsc::channel();
        let polled: PolledSink = Arc::new(move |at, next| {
            let _ = ptx.send(next.duration_since(at).unwrap());
        });
        let mut w = SnapshotWorker::spawn(
            plan(),
            Box::new(Scripted(vec![])),
            sink,
            polled,
            crate::supervise::unwatched(),
        )
        .unwrap();
        assert_eq!(w.source(), "refdb");
        assert_eq!(w.dataset(), "u");
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "first poll at start"
        );
        assert_eq!(
            prx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Duration::from_secs(3600),
            "the next poll is one interval on"
        );
        w.poll_now();
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "poll now");
        w.shutdown();
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a stopped worker polls no more"
        );
    }

    /// Blocks every query until `release` is dropped, announcing each entry
    /// on `entered`, so a test can act while a poll is provably in flight.
    struct Held {
        entered: mpsc::Sender<()>,
        release: Arc<Mutex<mpsc::Receiver<()>>>,
    }
    impl SnapshotQuery for Held {
        fn query(&mut self, _: &str) -> Result<TableRows, AdapterError> {
            let _ = self.entered.send(());
            // Err once the sender is dropped: every later poll runs free.
            let _ = self.release.lock().unwrap().recv();
            Ok(good())
        }
    }

    fn held() -> (Held, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (etx, erx) = mpsc::channel();
        let (rtx, rrx) = mpsc::channel();
        let q = Held {
            entered: etx,
            release: Arc::new(Mutex::new(rrx)),
        };
        (q, erx, rtx)
    }

    fn counting_sink() -> (SnapshotSink, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let sink: SnapshotSink = Arc::new(move |_| {
            let _ = tx.send(());
        });
        (sink, rx)
    }

    #[test]
    fn poll_now_asked_while_a_poll_is_held_runs_exactly_one_follow_up() {
        let (q, entered, release) = held();
        let (sink, outcomes) = counting_sink();
        let mut w = SnapshotWorker::spawn(
            plan(),
            Box::new(q),
            sink,
            Arc::new(|_, _| {}),
            crate::supervise::unwatched(),
        )
        .unwrap();
        entered
            .recv_timeout(Duration::from_secs(5))
            .expect("the first poll is in flight");
        w.poll_now();
        w.poll_now();
        w.poll_now();
        drop(release);
        for n in 0..2 {
            outcomes
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("outcome {n}"));
        }
        assert!(
            outcomes.recv_timeout(Duration::from_millis(300)).is_err(),
            "three asks during one poll are one follow-up poll"
        );
        w.shutdown();
    }

    #[test]
    fn shutdown_during_a_held_poll_returns_once_it_is_released_and_polls_no_more() {
        let (q, entered, release) = held();
        let (sink, outcomes) = counting_sink();
        let mut w = SnapshotWorker::spawn(
            plan(),
            Box::new(q),
            sink,
            Arc::new(|_, _| {}),
            crate::supervise::unwatched(),
        )
        .unwrap();
        entered
            .recv_timeout(Duration::from_secs(5))
            .expect("the first poll is in flight");
        // Asked for before the stop: the stop must still win.
        w.poll_now();
        let (done_tx, done) = mpsc::channel();
        let stopper = std::thread::spawn(move || {
            w.shutdown();
            let _ = done_tx.send(());
        });
        assert!(
            done.recv_timeout(Duration::from_millis(100)).is_err(),
            "shutdown waits for the poll in flight"
        );
        drop(release);
        done.recv_timeout(Duration::from_secs(5))
            .expect("shutdown returns once the poll is released");
        stopper.join().unwrap();
        outcomes
            .recv_timeout(Duration::from_secs(1))
            .expect("the held poll's own outcome still arrives");
        assert!(
            outcomes.recv_timeout(Duration::from_millis(300)).is_err(),
            "no poll after the stop"
        );
        assert!(entered.try_recv().is_err(), "no second query was started");
    }

    #[test]
    fn an_interval_too_large_to_add_waits_for_poll_now_instead_of_panicking() {
        let (stop, stops) = crate::supervise::tests_support::recording();
        let (tx, rx) = mpsc::channel();
        let sink: SnapshotSink = Arc::new(move |o| {
            let _ = tx.send(matches!(o, SnapshotOutcome::Rows { .. }));
        });
        let (ptx, prx) = mpsc::channel();
        let polled: PolledSink = Arc::new(move |at, next| {
            let _ = ptx.send((at, next));
        });
        let mut absurd = plan();
        absurd.interval = Duration::MAX;
        let mut w =
            SnapshotWorker::spawn(absurd, Box::new(Scripted(vec![])), sink, polled, stop).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        let (at, next) = prx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next, at, "an unrepresentable next poll is reported as now");
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "no deadline: the worker waits"
        );
        w.poll_now();
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "the worker is alive and polls on demand"
        );
        w.shutdown();
        assert!(stops.try_recv().is_err(), "the worker never died");
    }

    #[test]
    fn a_snapshot_worker_that_dies_is_declared() {
        let (stop, stops) = crate::supervise::tests_support::recording();
        let sink: SnapshotSink = Arc::new(|_| panic!("the snapshot sink fell over"));
        let polled: PolledSink = Arc::new(|_, _| {});
        let mut w =
            SnapshotWorker::spawn(plan(), Box::new(Scripted(vec![])), sink, polled, stop).unwrap();
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-snapshot-refdb");
        assert!(reason.contains("the snapshot sink fell over"), "{reason}");
        w.shutdown();
    }
}
