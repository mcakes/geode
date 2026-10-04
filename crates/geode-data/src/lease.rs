//! Who may open the store: the app or the background collector.
//!
//! DuckDB admits one writing process per file, so the app and the headless
//! collector hand the store between them through two advisory lock files
//! beside it, `<db>.app.lock` and `<db>.collector.lock`. The app holds its
//! lock for its whole life; the collector polls `app_present` and releases
//! the store when an app appears. The lock files' content is not the
//! protocol: nothing is written into them, and the OS drops the locks when a
//! process dies, so a crashed holder never strands the store.
//!
//! The app's open waits: once it holds `<db>.app.lock` it retries the
//! DuckDB open while the refusal is DuckDB's own lock conflict (the
//! collector is still draining), until a deadline, and reports the wait
//! once. A stop request ends the wait at the next retry.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::store::StoreError;

/// How long the app retries `<db>.app.lock` before deciding another window
/// holds it. Long enough to absorb a collector's momentary probe.
const APP_LOCK_RETRY: Duration = Duration::from_secs(1);
/// Sleep between attempts on `<db>.app.lock`.
const APP_LOCK_STEP: Duration = Duration::from_millis(20);
/// Sleep between refused store opens; also the stop-check interval.
const OPEN_STEP: Duration = Duration::from_millis(100);

/// `<db>.app.lock`: the database file name plus `.app.lock`.
pub fn app_lock_path(db: &Path) -> PathBuf {
    lock_path(db, ".app.lock")
}

/// `<db>.collector.lock`: the database file name plus `.collector.lock`.
pub fn collector_lock_path(db: &Path) -> PathBuf {
    lock_path(db, ".collector.lock")
}

fn lock_path(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    db.with_file_name(name)
}

/// Held for the app's life; dropping it releases `<db>.app.lock`.
#[derive(Debug)]
pub struct AppLease {
    _file: File,
}

/// Held for the collector's life; dropping it releases
/// `<db>.collector.lock`.
#[derive(Debug)]
pub struct CollectorLease {
    _file: File,
}

#[derive(Debug)]
pub enum LeaseError {
    /// The app lock stayed held for the full second: another Geode window
    /// has this store.
    OtherInstance,
    /// The store stayed locked until the deadline: the collector did not
    /// release it.
    Held {
        waited: Duration,
        holder: Option<u32>,
    },
    /// A stop was requested while waiting.
    Cancelled,
    Open(StoreError),
    Io(std::io::Error),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseError::OtherInstance => write!(f, "another Geode window has this store open"),
            LeaseError::Held { waited, holder } => {
                let secs = waited.as_secs_f64().round() as u64;
                write!(
                    f,
                    "the background collector did not release the store within {secs} s"
                )?;
                if let Some(pid) = holder {
                    write!(f, " (PID {pid})")?;
                }
                Ok(())
            }
            LeaseError::Cancelled => write!(f, "stopped while waiting for the store"),
            LeaseError::Open(err) => write!(f, "{err}"),
            LeaseError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for LeaseError {}

/// Take `<db>.app.lock`, retrying `try_lock` for up to 1 s; then open the
/// store with `open`, retrying every 100 ms while the error is a lock
/// conflict, until `deadline`. `on_wait(holder)` runs once, at the first
/// refused open. `should_stop` is checked between retries.
pub fn acquire_app<T>(
    db: &Path,
    deadline: Duration,
    should_stop: &dyn Fn() -> bool,
    on_wait: &mut dyn FnMut(Option<u32>),
    open: &mut dyn FnMut() -> Result<T, StoreError>,
) -> Result<(AppLease, T), LeaseError> {
    let lease = AppLease {
        _file: take_app_lock(db)?,
    };
    let start = Instant::now();
    let mut holder = None;
    let mut reported = false;
    loop {
        let err = match open() {
            Ok(value) => return Ok((lease, value)),
            Err(err) if is_lock_conflict(&err) => err,
            Err(err) => return Err(LeaseError::Open(err)),
        };
        holder = holder_pid(&err.to_string()).or(holder);
        if !reported {
            reported = true;
            on_wait(holder);
        }
        let waited = start.elapsed();
        if waited >= deadline {
            return Err(LeaseError::Held { waited, holder });
        }
        std::thread::sleep(OPEN_STEP.min(deadline - waited));
        if should_stop() {
            return Err(LeaseError::Cancelled);
        }
    }
}

/// `<db>.app.lock`, retried for `APP_LOCK_RETRY`: a collector's probe holds
/// it for an instant, so one refusal does not mean another app.
fn take_app_lock(db: &Path) -> Result<File, LeaseError> {
    let file = open_lock(&app_lock_path(db)).map_err(LeaseError::Io)?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(err)) => return Err(LeaseError::Io(err)),
        }
        if start.elapsed() >= APP_LOCK_RETRY {
            return Err(LeaseError::OtherInstance);
        }
        std::thread::sleep(APP_LOCK_STEP);
    }
}

/// Open (creating) a lock file without touching its content.
fn open_lock(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// True while another process (or handle) holds `<db>.app.lock`. A probe is
/// `try_lock` then immediate unlock.
pub fn app_present(db: &Path) -> std::io::Result<bool> {
    let file = open_lock(&app_lock_path(db))?;
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// `Ok(None)` when another collector holds the lock.
pub fn try_collector(db: &Path) -> std::io::Result<Option<CollectorLease>> {
    let file = open_lock(&collector_lock_path(db))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(CollectorLease { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// DuckDB's refusal to open a file another process holds. Pinned against a
/// real conflict by a cross-process test; a DuckDB upgrade that rewords it
/// fails that test rather than turning every wait into an open error.
pub fn is_lock_conflict_message(message: &str) -> bool {
    message.contains("Could not set lock on file") || message.contains("Conflicting lock is held")
}

/// `is_lock_conflict_message` through an open error's source text. Only an
/// open can conflict; any other store error is not retried.
pub fn is_lock_conflict(err: &StoreError) -> bool {
    match err {
        StoreError::Open { source, .. } => is_lock_conflict_message(&source.to_string()),
        _ => false,
    }
}

/// The holder's PID from DuckDB's conflict text: `(PID 123)` gives 123.
pub fn holder_pid(message: &str) -> Option<u32> {
    let (_, rest) = message.split_once("(PID ")?;
    let (digits, _) = rest.split_once(')')?;
    digits.trim().parse().ok()
}

/// A child process holding a DuckDB store open, for cross-process tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{BufRead, BufReader};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    /// The env var naming the store `lock_holder_child` opens.
    pub(crate) const HOLD_DB_ENV: &str = "GEODE_TEST_HOLD_DB";
    /// libtest's exact name for the helper test.
    const CHILD_TEST: &str = "lease::tests::lock_holder_child";
    /// How long the child may take to open the store.
    const READY_DEADLINE: Duration = Duration::from_secs(10);

    /// A running child holding the store. Dropping it kills the child.
    pub(crate) struct Holder {
        child: Child,
    }

    impl Holder {
        pub(crate) fn pid(&self) -> u32 {
            self.child.id()
        }
    }

    impl Drop for Holder {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Re-run this test binary as `lock_holder_child` holding `db`, and
    /// return once it reports `HOLDING`. Panics if it does not within 10 s.
    pub(crate) fn spawn_holder(db: &Path) -> Holder {
        let exe = std::env::current_exe().expect("test binary path");
        let mut child = Command::new(exe)
            .args(["--ignored", "--exact", CHILD_TEST, "--nocapture"])
            .env(HOLD_DB_ENV, db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the lock holder child");
        let stdout = child.stdout.take().expect("child stdout");
        // The guard exists before the wait, so a panic below kills the child.
        let holder = Holder { child };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.contains("HOLDING") {
                    let _ = tx.send(());
                }
            }
        });
        match rx.recv_timeout(READY_DEADLINE) {
            Ok(()) => holder,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("lock holder child did not report HOLDING within {READY_DEADLINE:?}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("lock holder child exited before reporting HOLDING")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::store::Store;

    fn conflict(pid: u32) -> StoreError {
        let message = format!(
            "IO Error: Could not set lock on file \"/x/g.duckdb\": Conflicting lock is held in \
             /bin/geode (PID {pid}) by user mch. See also \
             https://duckdb.org/docs/stable/connect/concurrency"
        );
        StoreError::Open {
            path: PathBuf::from("/x/g.duckdb"),
            source: duckdb::Error::DuckDBFailure(duckdb::ffi::Error::new(1), Some(message)),
        }
    }

    fn db_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("g.duckdb")
    }

    fn never() -> bool {
        false
    }

    fn acquire(db: &Path) -> Result<(AppLease, ()), LeaseError> {
        acquire_app(db, Duration::from_secs(1), &never, &mut |_| {}, &mut || {
            Ok(())
        })
    }

    #[test]
    fn lock_paths_append_to_the_file_name() {
        let db = Path::new("/x/g.duckdb");
        assert_eq!(app_lock_path(db), Path::new("/x/g.duckdb.app.lock"));
        assert_eq!(
            collector_lock_path(db),
            Path::new("/x/g.duckdb.collector.lock")
        );
    }

    #[test]
    fn a_second_app_lease_in_one_process_is_other_instance_after_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let (_lease, ()) = acquire(&db).expect("first lease");
        let start = Instant::now();
        let err = acquire(&db).expect_err("second lease must be refused");
        let elapsed = start.elapsed();
        assert!(matches!(err, LeaseError::OtherInstance), "{err:?}");
        assert_eq!(err.to_string(), "another Geode window has this store open");
        assert!(
            elapsed >= Duration::from_millis(900) && elapsed <= Duration::from_secs(3),
            "{elapsed:?}"
        );
    }

    #[test]
    fn a_probe_does_not_refuse_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        // A probe that is descheduled while it holds the lock: the app's
        // first attempts collide with it and the retry must absorb them.
        let slow_probe = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(app_lock_path(&db))
            .unwrap();
        slow_probe.try_lock().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            slow_probe.unlock().unwrap();
        });
        let (lease, ()) = acquire(&db).expect("a held probe must not refuse the app");
        drop(lease);
        release.join().unwrap();

        // Fast probes in a loop, while the app takes the lease repeatedly.
        let probing = Arc::new(AtomicBool::new(true));
        let prober = {
            let (db, probing) = (db.clone(), probing.clone());
            std::thread::spawn(move || {
                while probing.load(Ordering::SeqCst) {
                    let _ = app_present(&db).unwrap();
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        let start = Instant::now();
        let mut taken = 0;
        while start.elapsed() < Duration::from_millis(300) {
            let (lease, ()) = acquire(&db).expect("a probe must not refuse the app");
            drop(lease);
            taken += 1;
        }
        probing.store(false, Ordering::SeqCst);
        prober.join().unwrap();
        assert!(taken > 0);
    }

    #[test]
    fn a_dropped_lease_frees_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let (lease, ()) = acquire(&db).unwrap();
        assert!(app_present(&db).unwrap());
        drop(lease);
        assert!(!app_present(&db).unwrap());
        // A probe leaves the lock free.
        assert!(!app_present(&db).unwrap());
    }

    #[test]
    fn a_conflicting_open_waits_reports_once_and_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let mut refusals = 3;
        let mut waits = Vec::new();
        let (_lease, value) = acquire_app(
            &db,
            Duration::from_secs(5),
            &never,
            &mut |holder| waits.push(holder),
            &mut || {
                if refusals > 0 {
                    refusals -= 1;
                    Err(conflict(4242))
                } else {
                    Ok(7)
                }
            },
        )
        .expect("the open succeeds once the conflict clears");
        assert_eq!(value, 7);
        assert_eq!(refusals, 0);
        assert_eq!(waits, vec![Some(4242)]);
    }

    #[test]
    fn an_open_that_never_frees_is_held_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let start = Instant::now();
        let err = acquire_app(
            &db,
            Duration::from_millis(300),
            &never,
            &mut |_| {},
            &mut || Err::<(), _>(conflict(812)),
        )
        .expect_err("a store that never frees is held");
        assert!(start.elapsed() < Duration::from_secs(3));
        match &err {
            LeaseError::Held { waited, holder } => {
                assert!(*waited >= Duration::from_millis(300), "{waited:?}");
                assert_eq!(*holder, Some(812));
            }
            other => panic!("expected Held, got {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            "the background collector did not release the store within 0 s (PID 812)"
        );
        // The lease is released with the refusal.
        assert!(!app_present(&db).unwrap());
    }

    #[test]
    fn held_names_the_deadline_in_whole_seconds_and_the_pid_only_when_known() {
        let held = |ms, holder| LeaseError::Held {
            waited: Duration::from_millis(ms),
            holder,
        };
        assert_eq!(
            held(15_040, Some(9)).to_string(),
            "the background collector did not release the store within 15 s (PID 9)"
        );
        assert_eq!(
            held(15_040, None).to_string(),
            "the background collector did not release the store within 15 s"
        );
        assert_eq!(
            LeaseError::Cancelled.to_string(),
            "stopped while waiting for the store"
        );
    }

    #[test]
    fn a_stop_during_the_wait_cancels_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let start = Instant::now();
        let stop = || start.elapsed() >= Duration::from_millis(100);
        let err = acquire_app(
            &db,
            Duration::from_secs(15),
            &stop,
            &mut |_| {},
            &mut || Err::<(), _>(conflict(812)),
        )
        .expect_err("a stop cancels the wait");
        assert!(matches!(err, LeaseError::Cancelled), "{err:?}");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_non_lock_open_error_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let mut opens = 0;
        let mut waited = false;
        let err = acquire_app(
            &db,
            Duration::from_secs(5),
            &never,
            &mut |_| waited = true,
            &mut || {
                opens += 1;
                Err::<(), _>(StoreError::Drift("columns changed".into()))
            },
        )
        .expect_err("a non-conflict error is returned");
        assert!(
            matches!(err, LeaseError::Open(StoreError::Drift(_))),
            "{err:?}"
        );
        assert_eq!(opens, 1);
        assert!(!waited);
    }

    #[test]
    fn a_second_collector_gets_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let first = try_collector(&db).unwrap();
        assert!(first.is_some());
        assert!(try_collector(&db).unwrap().is_none());
        drop(first);
        assert!(try_collector(&db).unwrap().is_some());
    }

    #[test]
    fn the_collector_lock_does_not_mark_an_app_present() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let _collector = try_collector(&db).unwrap().unwrap();
        assert!(!app_present(&db).unwrap());
    }

    #[test]
    fn holder_pid_parses_the_duckdb_text() {
        let text = "IO Error: Could not set lock on file \"/x/g.duckdb\": Conflicting lock is \
                    held in /bin/geode (PID 812) by user mch. See also \
                    https://duckdb.org/docs/stable/connect/concurrency";
        assert_eq!(holder_pid(text), Some(812));
        assert!(is_lock_conflict_message(text));
        assert_eq!(holder_pid("IO Error: Could not set lock on file"), None);
        assert_eq!(holder_pid("(PID )"), None);
        assert!(!is_lock_conflict_message(
            "Catalog Error: table x does not exist"
        ));
        assert!(is_lock_conflict(&conflict(1)));
        assert!(!is_lock_conflict(&StoreError::Drift(
            "Could not set lock on file".into()
        )));
    }

    #[test]
    fn the_classifier_matches_a_real_duckdb_conflict_from_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let holder = test_support::spawn_holder(&db);
        let err = match Store::open(&db) {
            Ok(_) => panic!("the store must be held by the child"),
            Err(err) => err,
        };
        let text = err.to_string();
        eprintln!("duckdb conflict text: {text}");
        assert!(is_lock_conflict(&err), "{text}");
        assert_eq!(holder_pid(&text), Some(holder.pid()), "{text}");
        drop(holder);
        // The child's death frees the store.
        Store::open(&db).expect("the store opens once the holder is gone");
    }

    /// Run only by `test_support::spawn_holder`, which sets the env var:
    /// open the store, report `HOLDING`, and hold it until stdin closes.
    #[test]
    #[ignore = "a child process for cross-process lock tests"]
    fn lock_holder_child() {
        let Some(db) = std::env::var_os(test_support::HOLD_DB_ENV) else {
            return;
        };
        let _store = Store::open(PathBuf::from(db)).expect("child opens the store");
        let mut out = std::io::stdout();
        writeln!(out, "HOLDING").unwrap();
        out.flush().unwrap();
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
    }
}
