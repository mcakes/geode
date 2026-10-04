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
//! collector is still draining), or any open refusal while a collector
//! holds `<db>.collector.lock`, until a deadline, and reports the wait
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
    /// Opening or locking a lock file failed. `path` is the lock file, so
    /// the failure names it (a file system without locking reads as this).
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
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
            LeaseError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for LeaseError {}

impl LeaseError {
    fn io(path: &Path) -> impl FnOnce(std::io::Error) -> LeaseError + '_ {
        move |source| LeaseError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Take `<db>.app.lock`, retrying `try_lock` for up to 1 s; then open the
/// store with `open`, retrying every 100 ms while the error is a lock
/// conflict, or any open error while a collector holds its lease (see
/// `collector_handing_over`), until `deadline`. `on_wait(holder)` runs
/// once, at the first refused open; `holder` is the PID DuckDB named, if
/// any refusal so far named one. At the deadline a refusal with DuckDB's
/// conflict marker is `Held`; one without it is returned as `Open`.
///
/// `deadline` is measured from the start of the open phase, after the lock
/// is taken, so the whole call can last up to `deadline` plus the 1 s lock
/// retry.
///
/// `should_stop` is checked between lock retries and between open retries,
/// and a stop there returns `Cancelled`. It is not checked before an open
/// that then succeeds: a stop arriving just before that open still returns
/// `Ok`, so the caller re-checks its own stop flag after a success.
pub fn acquire_app<T>(
    db: &Path,
    deadline: Duration,
    should_stop: &dyn Fn() -> bool,
    on_wait: &mut dyn FnMut(Option<u32>),
    open: &mut dyn FnMut() -> Result<T, StoreError>,
) -> Result<(AppLease, T), LeaseError> {
    let lease = AppLease {
        _file: take_app_lock(db, should_stop)?,
    };
    let start = Instant::now();
    let mut holder = None;
    let mut reported = false;
    loop {
        let err = match open() {
            Ok(value) => return Ok((lease, value)),
            Err(err) => err,
        };
        let conflict = conflict_text(&err);
        if conflict.is_none() && !collector_handing_over(db, &err) {
            return Err(LeaseError::Open(err));
        }
        if let Some(text) = &conflict {
            holder = holder_pid(text).or(holder);
        }
        if !reported {
            reported = true;
            on_wait(holder);
        }
        let waited = start.elapsed();
        if waited >= deadline {
            // A last refusal without DuckDB's marker is reported as itself:
            // nothing confirmed the collector was still holding the file.
            return Err(match conflict {
                Some(_) => LeaseError::Held { waited, holder },
                None => LeaseError::Open(err),
            });
        }
        std::thread::sleep(OPEN_STEP.min(deadline - waited));
        if should_stop() {
            return Err(LeaseError::Cancelled);
        }
    }
}

/// A refused open that is the collector's handover although DuckDB's text
/// does not say so: any `StoreError::Open` while `<db>.collector.lock` is
/// held. On Windows DuckDB names the holder through the Restart Manager
/// only after `CreateFileW` fails, so a collector that closes the file in
/// that gap leaves a sharing violation with no marker; without this the
/// app would fail at once on a handover that was about to complete. The
/// lock is the protocol's own signal, so no message text is read. A probe
/// that cannot read the lock is no collector, and the refusal fails as
/// itself.
fn collector_handing_over(db: &Path, err: &StoreError) -> bool {
    matches!(err, StoreError::Open { .. }) && collector_present(db).unwrap_or(false)
}

/// `<db>.app.lock`, retried for `APP_LOCK_RETRY`: a collector's probe holds
/// it for an instant, so one refusal does not mean another app.
fn take_app_lock(db: &Path, should_stop: &dyn Fn() -> bool) -> Result<File, LeaseError> {
    let path = app_lock_path(db);
    let file = open_lock(&path).map_err(LeaseError::io(&path))?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(err)) => return Err(LeaseError::io(&path)(err)),
        }
        if start.elapsed() >= APP_LOCK_RETRY {
            return Err(LeaseError::OtherInstance);
        }
        std::thread::sleep(APP_LOCK_STEP);
        if should_stop() {
            return Err(LeaseError::Cancelled);
        }
    }
}

/// Open (creating) a lock file without touching its content. The store's
/// directory is created first, best-effort as `Store::open_with` does: the
/// lock is taken before the store is opened, so on a fresh machine nothing
/// else has created it yet. A failure to create it surfaces as the open's
/// own error.
fn open_lock(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    open_lock_file(path)
}

/// Open (creating) a lock file in an existing directory.
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// True while another process (or handle) holds `<db>.app.lock`. A probe is
/// `try_lock` then immediate unlock.
///
/// A missing store directory means no app is present (an app creates it
/// before it locks), so the probe returns `Ok(false)` and creates nothing:
/// a probe never leaves a directory behind.
pub fn app_present(db: &Path) -> std::io::Result<bool> {
    lock_held(&app_lock_path(db))
}

/// True while another process (or handle) holds `<db>.collector.lock`: a
/// collector is running on this store, whether or not it has the store
/// open. The same probe as [`app_present`]; a missing store directory is
/// no collector, and the probe creates nothing.
pub fn collector_present(db: &Path) -> std::io::Result<bool> {
    lock_held(&collector_lock_path(db))
}

/// `try_lock` then immediate unlock on the lock file at `path`.
fn lock_held(path: &Path) -> std::io::Result<bool> {
    let path = path.to_path_buf();
    let file = match open_lock_file(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(naming(&path, err)),
    };
    match file.try_lock() {
        Ok(()) => {
            file.unlock().map_err(|err| naming(&path, err))?;
            Ok(false)
        }
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(err)) => Err(naming(&path, err)),
    }
}

/// `Ok(None)` when another collector holds the lock.
pub fn try_collector(db: &Path) -> std::io::Result<Option<CollectorLease>> {
    let path = collector_lock_path(db);
    let file = open_lock(&path).map_err(|err| naming(&path, err))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(CollectorLease { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(naming(&path, err)),
    }
}

/// `err` with the lock file's path in front, keeping its kind.
fn naming(path: &Path, err: std::io::Error) -> std::io::Error {
    std::io::Error::new(err.kind(), format!("{}: {err}", path.display()))
}

/// DuckDB's refusal to open a file another process holds. Pinned against a
/// real conflict by a cross-process test; a DuckDB upgrade that rewords it
/// fails that test rather than turning every wait into an open error.
///
/// - Unix: DuckDB takes an fcntl lock and reports `Could not set lock on
///   file "<path>": Conflicting lock is held in <exe> (PID n) …`. The
///   `Conflicting lock is held in` marker is what matches: the same `Could
///   not set lock on file` prefix also carries a file system without locks
///   (`File locks are not supported…`) and a failed `F_GETLK` (strerror's
///   text), and neither clears by waiting, so both fail at once.
/// - Windows: DuckDB opens with share mode 0, so `CreateFileW` itself fails
///   with `Cannot open file "<path>": <OS sharing-violation message>`. When
///   the Restart Manager finds the holder, DuckDB appends `File is already
///   open in` and `<exe> (PID n)`; that marker is what matches here.
///   `Cannot open file` alone is not a conflict: a missing file or a denied
///   permission reads the same.
///
/// On Windows, when the Restart Manager finds no holder, the message
/// carries only the localized OS text and no DuckDB marker, so it is not
/// classified as a conflict. `acquire_app` still waits it out while a
/// collector holds its lease (the handover gap); with no collector it
/// fails at once as `LeaseError::Open`. Known gap: on a Unix other than
/// macOS and Linux, DuckDB names no holder and writes no marker, so a
/// conflict with a process that is not a collector fails at once.
pub fn is_lock_conflict_message(message: &str) -> bool {
    message.contains("Conflicting lock is held in") || message.contains("File is already open in")
}

/// `is_lock_conflict_message` through an open error's source text. Only an
/// open can conflict; any other store error is not retried.
pub fn is_lock_conflict(err: &StoreError) -> bool {
    conflict_text(err).is_some()
}

/// DuckDB's own text for a conflicting open: the `duckdb::Error` display,
/// without the `opening database at {path}:` prefix, so the store path in
/// that prefix is never read as the holder.
fn conflict_text(err: &StoreError) -> Option<String> {
    match err {
        StoreError::Open { source, .. } => {
            let text = source.to_string();
            is_lock_conflict_message(&text).then_some(text)
        }
        _ => None,
    }
}

/// The holder's PID from DuckDB's conflict text: `(PID 123)` gives 123, and
/// so does the bare `PID 123` DuckDB writes when it cannot read the
/// holder's process name. DuckDB names the holder after the file path it
/// quotes, so the last `PID n` that parses wins over one inside the path.
/// `PID` counts only at a word start (after `(`, whitespace, or the text's
/// start). PID 0 is no
/// holder: on macOS, when `F_GETLK` finds the lock already released (a
/// handover racing the open), DuckDB names PID 0, and a caller keeping
/// the last known holder (`holder_pid(..).or(holder)`) must keep it. The
/// zero is filtered after the last match is chosen, so it never falls
/// through to a `(PID n)` inside the quoted path.
pub fn holder_pid(message: &str) -> Option<u32> {
    message
        .rmatch_indices("PID ")
        .find_map(|(at, marker)| {
            let at_word_start = message[..at]
                .chars()
                .next_back()
                .is_none_or(|c| c == '(' || c.is_whitespace());
            if !at_word_start {
                return None;
            }
            let rest = message[at + marker.len()..].trim_start();
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            rest[..end].parse::<u32>().ok()
        })
        .filter(|&pid| pid != 0)
}

/// A child process holding a DuckDB store open, for cross-process tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{BufRead, BufReader};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::{RwLock, RwLockReadGuard, mpsc};
    use std::time::Duration;

    /// Spawning a process briefly shares every open file description with
    /// the child until its exec closes them (close-on-exec), and a flock
    /// belongs to the description, so a lock dropped in that window stays
    /// held until the exec. `spawn_holder` takes this gate exclusively
    /// around the spawn; a test that asserts a lock file is free after a
    /// drop holds `lock_file_gate()` for its whole life.
    static SPAWN_GATE: RwLock<()> = RwLock::new(());

    /// Shared hold on the spawn gate: no child is spawned while it lives.
    pub(crate) fn lock_file_gate() -> RwLockReadGuard<'static, ()> {
        SPAWN_GATE
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
        let gate = SPAWN_GATE
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut child = Command::new(exe)
            .args(["--ignored", "--exact", CHILD_TEST, "--nocapture"])
            .env(HOLD_DB_ENV, db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the lock holder child");
        drop(gate);
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
        open_error("/x/g.duckdb", &message)
    }

    /// An open error at `path` whose DuckDB text is `message`.
    fn open_error(path: &str, message: &str) -> StoreError {
        StoreError::Open {
            path: PathBuf::from(path),
            source: duckdb::Error::DuckDBFailure(
                duckdb::ffi::Error::new(1),
                Some(message.to_string()),
            ),
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
        let _gate = test_support::lock_file_gate();
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
        let _gate = test_support::lock_file_gate();
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
        let _gate = test_support::lock_file_gate();
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
        let _gate = test_support::lock_file_gate();
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
        let _gate = test_support::lock_file_gate();
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
        let _gate = test_support::lock_file_gate();
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
        let _gate = test_support::lock_file_gate();
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

        // An open error that is not DuckDB's conflict is not retried either.
        let mut opens = 0;
        let err = acquire_app(
            &db,
            Duration::from_secs(5),
            &never,
            &mut |_| waited = true,
            &mut || {
                opens += 1;
                Err::<(), _>(open_error(
                    "/x/g.duckdb",
                    "IO Error: Cannot open file \"/x/g.duckdb\": Permission denied",
                ))
            },
        )
        .expect_err("a non-conflict open error is returned");
        assert!(
            matches!(err, LeaseError::Open(StoreError::Open { .. })),
            "{err:?}"
        );
        assert_eq!(opens, 1);
        assert!(!waited);
    }

    #[test]
    fn a_stop_while_another_app_holds_the_lock_cancels_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let _gate = test_support::lock_file_gate();
        let (_lease, ()) = acquire(&db).unwrap();
        let start = Instant::now();
        let stop = || start.elapsed() >= Duration::from_millis(100);
        let mut opened = false;
        let err = acquire_app(
            &db,
            Duration::from_secs(15),
            &stop,
            &mut |_| {},
            &mut || {
                opened = true;
                Ok(())
            },
        )
        .expect_err("a stop during the lock retry cancels");
        assert!(matches!(err, LeaseError::Cancelled), "{err:?}");
        assert!(
            start.elapsed() < Duration::from_millis(600),
            "{:?}",
            start.elapsed()
        );
        assert!(!opened);
    }

    /// The lock is taken before the store opens, so on a fresh machine the
    /// lock's open must create the store's directory itself.
    #[test]
    fn the_app_lease_creates_a_missing_store_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("a").join("b").join("g.duckdb");
        let _gate = test_support::lock_file_gate();
        let (_lease, ()) = acquire(&db).expect("a missing directory is created");
        assert!(app_lock_path(&db).exists());
        assert!(app_present(&db).unwrap());
    }

    #[test]
    fn the_collector_lease_creates_a_missing_store_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("a").join("b").join("g.duckdb");
        let _gate = test_support::lock_file_gate();
        let lease = try_collector(&db).expect("a missing directory is created");
        assert!(lease.is_some());
        assert!(collector_lock_path(&db).exists());
    }

    /// No app can hold a lock in a directory that does not exist, and the
    /// probe leaves nothing behind.
    #[test]
    fn a_probe_of_a_missing_directory_is_no_app_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("a");
        let db = missing.join("b").join("g.duckdb");
        let _gate = test_support::lock_file_gate();
        assert!(!app_present(&db).unwrap());
        assert!(!missing.exists());
    }

    /// A lock file that cannot be opened names itself in the error.
    #[test]
    fn a_lock_file_error_names_the_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let _gate = test_support::lock_file_gate();
        // A directory where the lock file should be: the open fails.
        std::fs::create_dir(app_lock_path(&db)).unwrap();
        let err = acquire(&db).expect_err("a directory is not a lock file");
        let LeaseError::Io { path, .. } = &err else {
            panic!("expected Io, got {err:?}");
        };
        assert_eq!(path, &app_lock_path(&db));
        let shown = err.to_string();
        let prefix = format!("{}: ", app_lock_path(&db).display());
        assert!(shown.starts_with(&prefix), "{shown}");
        std::fs::create_dir(collector_lock_path(&db)).unwrap();
        let err = try_collector(&db).expect_err("a directory is not a lock file");
        let prefix = format!("{}: ", collector_lock_path(&db).display());
        assert!(err.to_string().starts_with(&prefix), "{err}");
    }

    #[test]
    fn a_second_collector_gets_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let _gate = test_support::lock_file_gate();
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
        let _gate = test_support::lock_file_gate();
        let _collector = try_collector(&db).unwrap().unwrap();
        assert!(!app_present(&db).unwrap());
    }

    #[test]
    fn collector_present_reads_the_collector_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let _gate = test_support::lock_file_gate();
        assert!(!collector_present(&db).unwrap());
        let lease = try_collector(&db).unwrap().unwrap();
        assert!(collector_present(&db).unwrap());
        // An app lease is not a collector.
        drop(lease);
        let (_app, ()) = acquire(&db).unwrap();
        assert!(!collector_present(&db).unwrap());
        // A probe leaves the lock free for a collector.
        assert!(try_collector(&db).unwrap().is_some());
    }

    #[test]
    fn a_collector_probe_of_a_missing_directory_is_no_collector_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("a");
        let db = missing.join("b").join("g.duckdb");
        let _gate = test_support::lock_file_gate();
        assert!(!collector_present(&db).unwrap());
        assert!(!missing.exists());
    }

    /// On Windows DuckDB names the holder only after `CreateFileW` fails; a
    /// collector closing the file in that gap leaves a refusal with no
    /// marker. While the collector's lease is held, any open refusal is
    /// its handover and is waited out; the wait is reported once, with no
    /// holder.
    #[test]
    fn a_non_conflict_open_error_is_retried_while_a_collector_holds_its_lease() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let _gate = test_support::lock_file_gate();
        let collector = try_collector(&db).unwrap().unwrap();
        let violation = "IO Error: Cannot open file \"/x/g.duckdb\": The process cannot access \
                         the file because it is being used by another process.";
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
                    Err(open_error("/x/g.duckdb", violation))
                } else {
                    Ok(7)
                }
            },
        )
        .expect("the handover's refusal is waited out");
        assert_eq!(value, 7);
        assert_eq!(refusals, 0);
        assert_eq!(waits, vec![None]);
        drop(collector);
    }

    /// The holder DuckDB named at an earlier refusal is kept through a
    /// later marker-less one, and the wait is still reported once. A
    /// refusal that outlasts the deadline without a marker returns its own
    /// error, not a conflict DuckDB never confirmed.
    #[test]
    fn a_handover_refusal_keeps_the_holder_and_reports_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let _gate = test_support::lock_file_gate();
        let _collector = try_collector(&db).unwrap().unwrap();
        let mut opens = 0;
        let mut waits = Vec::new();
        let start = Instant::now();
        let err = acquire_app(
            &db,
            Duration::from_millis(300),
            &never,
            &mut |holder| waits.push(holder),
            &mut || {
                opens += 1;
                if opens == 1 {
                    Err::<(), _>(conflict(812))
                } else {
                    Err(open_error("/x/g.duckdb", "IO Error: Cannot open file"))
                }
            },
        )
        .expect_err("a refusal that never clears fails at the deadline");
        assert!(start.elapsed() >= Duration::from_millis(300));
        assert_eq!(waits, vec![Some(812)]);
        assert!(opens > 2, "{opens}");
        assert!(
            matches!(err, LeaseError::Open(StoreError::Open { .. })),
            "{err:?}"
        );
    }

    /// Without a collector lease a marker-less refusal is not a handover
    /// and fails at once; a non-open error fails at once even with one.
    #[test]
    fn a_non_conflict_error_fails_at_once_without_a_collector_or_when_not_an_open() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_in(&dir);
        let _gate = test_support::lock_file_gate();
        let attempt = |db: &Path, err: fn() -> StoreError| {
            let mut opens = 0;
            let mut waited = false;
            let start = Instant::now();
            let result = acquire_app(
                db,
                Duration::from_secs(5),
                &never,
                &mut |_| waited = true,
                &mut || {
                    opens += 1;
                    Err::<(), _>(err())
                },
            );
            (result, opens, waited, start.elapsed())
        };
        let refused = || open_error("/x/g.duckdb", "IO Error: Cannot open file");
        let (result, opens, waited, elapsed) = attempt(&db, refused);
        assert!(
            matches!(result, Err(LeaseError::Open(StoreError::Open { .. }))),
            "{result:?}"
        );
        assert_eq!(opens, 1);
        assert!(!waited);
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");

        let _collector = try_collector(&db).unwrap().unwrap();
        let drift = || StoreError::Drift("columns changed".into());
        let (result, opens, waited, _) = attempt(&db, drift);
        assert!(
            matches!(result, Err(LeaseError::Open(StoreError::Drift(_)))),
            "{result:?}"
        );
        assert_eq!(opens, 1);
        assert!(!waited);
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

    /// DuckDB writes a bare `PID n` when it cannot read the holder's
    /// process name (`local_file_system.cpp`).
    #[test]
    fn holder_pid_parses_the_bare_form_without_a_process_name() {
        let bare = "IO Error: Could not set lock on file \"/x/g.duckdb\": Conflicting lock is \
                    held in PID 812 by user mch. See also \
                    https://duckdb.org/docs/stable/connect/concurrency";
        assert!(is_lock_conflict_message(bare));
        assert_eq!(holder_pid(bare), Some(812));
        assert_eq!(holder_pid("Conflicting lock is held in PID 812"), Some(812));
        // The last match wins over one inside the quoted path, in either form.
        let in_path = "IO Error: Could not set lock on file \"/x/PID 5/g.duckdb\": \
                       Conflicting lock is held in PID 812 by user mch.";
        assert_eq!(holder_pid(in_path), Some(812));
        // PID 0 is no holder, and does not fall through to the path's PID.
        let zero = "IO Error: Could not set lock on file \"/x/(PID 5)/g.duckdb\": \
                    Conflicting lock is held in PID 0 by user mch.";
        assert_eq!(holder_pid(zero), None);
        // `PID` inside a word is not the marker.
        assert_eq!(holder_pid("held in XPID 7 by user mch"), None);
    }

    #[test]
    fn a_pid_of_zero_names_no_holder_and_keeps_the_last_known_one() {
        let text = "IO Error: Could not set lock on file \"/x/g.duckdb\": Conflicting lock is \
                    held in  (PID 0) by user mch.";
        assert!(is_lock_conflict_message(text));
        assert_eq!(holder_pid(text), None);
        assert_eq!(holder_pid(text).or(Some(812)), Some(812));
        // A `(PID 5)` inside the quoted path is not the holder either.
        let in_path = "IO Error: Could not set lock on file \"/x/(PID 5)/g.duckdb\": \
                       Conflicting lock is held in  (PID 0) by user mch.";
        assert_eq!(holder_pid(in_path), None);
    }

    /// DuckDB's Windows form (`local_file_system.cpp`): the sharing
    /// violation, with the Restart Manager's holder appended when found.
    #[test]
    fn the_windows_conflict_text_is_a_conflict_only_with_duckdbs_marker() {
        let violation = "Cannot open file \"C:\\x\\g.duckdb\": The process cannot access the \
                         file because it is being used by another process.";
        let with_holder =
            format!("{violation}\nFile is already open in \nC:\\bin\\geode.exe (PID 812)");
        assert!(is_lock_conflict_message(&with_holder));
        assert_eq!(holder_pid(&with_holder), Some(812));
        assert!(is_lock_conflict(&open_error(
            "C:\\x\\g.duckdb",
            &with_holder
        )));
        // Without the holder there is no DuckDB marker: the documented gap.
        assert!(!is_lock_conflict_message(violation));
        // `Cannot open file` alone is a missing file or a denied permission.
        assert!(!is_lock_conflict_message(
            "Cannot open file \"C:\\x\\g.duckdb\": The system cannot find the path specified."
        ));
        assert!(!is_lock_conflict_message(
            "IO Error: Cannot open file \"/x/g.duckdb\": Permission denied"
        ));
    }

    /// DuckDB's Unix lock failures that are not another process's lock:
    /// waiting on them would only spend the deadline before the same error.
    #[test]
    fn a_unix_lock_failure_without_a_holder_is_not_a_conflict() {
        let unsupported = "IO Error: Could not set lock on file \"/x/g.duckdb\": File locks are \
                           not supported for this file system, cannot open the file in read-write \
                           mode. Try opening the file in read-only mode. See also \
                           https://duckdb.org/docs/stable/connect/concurrency";
        assert!(!is_lock_conflict_message(unsupported));
        assert!(!is_lock_conflict(&open_error("/x/g.duckdb", unsupported)));
        // F_GETLK failing as well leaves only strerror's text.
        let getlk = "IO Error: Could not set lock on file \"/x/g.duckdb\": No locks available. \
                     See also https://duckdb.org/docs/stable/connect/concurrency";
        assert!(!is_lock_conflict_message(getlk));
    }

    #[test]
    fn a_pid_in_the_store_path_is_not_the_holder() {
        let path = "/x/(PID 5)/g.duckdb";
        let text = format!(
            "IO Error: Could not set lock on file \"{path}\": Conflicting lock is held in \
             /bin/geode (PID 812) by user mch."
        );
        assert_eq!(holder_pid(&text), Some(812));

        // The holder `on_wait` reports comes from DuckDB's text alone, never
        // from the `opening database at {path}:` prefix `StoreError::Open`
        // displays.
        let reported = |err: StoreError| {
            let dir = tempfile::tempdir().unwrap();
            let db = db_in(&dir);
            let _gate = test_support::lock_file_gate();
            let mut pending = Some(err);
            let mut waits = Vec::new();
            acquire_app(
                &db,
                Duration::from_secs(5),
                &never,
                &mut |holder| waits.push(holder),
                &mut || pending.take().map_or(Ok(()), Err),
            )
            .unwrap();
            waits
        };
        assert_eq!(reported(open_error(path, &text)), vec![Some(812)]);
        let anonymous = "IO Error: Could not set lock on file \"/x/g.duckdb\": Conflicting lock \
                         is held in /bin/geode by user mch.";
        assert_eq!(reported(open_error(path, anonymous)), vec![None]);
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
        let StoreError::Open { source, .. } = &err else {
            panic!("expected an open error, got {err:?}");
        };
        let text = source.to_string();
        eprintln!("duckdb conflict text: {text}");
        // The marker each platform's DuckDB file system writes.
        let marker = if cfg!(windows) {
            "File is already open in"
        } else {
            "Conflicting lock is held in"
        };
        assert!(text.contains(marker), "{text}");
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
