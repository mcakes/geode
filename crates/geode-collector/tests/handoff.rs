//! Cross-process handoff: the real `geode-collector` binary against a temp
//! store, with this test process (or a child of it) as the app.
//!
//! Every test builds a temp world: `HOME` points into a temp directory,
//! `APPDATA` and `LOCALAPPDATA` are removed, `GEODE_DESK_CONFIG` names a temp
//! desk directory and `TMPDIR`, `TMP` and `TEMP` a temp directory (the
//! `--demo` store lives there; Windows' `temp_dir` reads `TMP`/`TEMP`). The
//! user `app.toml` sets `data.db_path` and `[log] collector = "debug"`, so
//! the collector's publish lines reach
//! `<temp home>/.config/geode/logs/<prefix>.*.log` (`collector`, or
//! `collector-demo-<rows>` for a demo store), which the tests read.
//! `GEODE_SERVICE=1` drops the collector's stderr log layer, so a child's
//! stderr (`<root>/stderr.txt`) holds only panics and aborts, shown when a
//! collector exits unexpectedly. The real user configuration is never read
//! or written.
//!
//! Rules that keep these tests bounded and honest:
//!
//! - Every child process is held by a [`Proc`], which kills and reaps it on
//!   drop, so a failed assertion never leaves a collector running.
//! - Every wait is a poll with a deadline of 10 s or less ([`WAIT`]) that
//!   fails on expiry. The one longer bound is the app's own 15 s open
//!   deadline inside `acquire_app`, which returns by itself.
//! - Tests run one at a time ([`serial`]). A spawned child shares this
//!   process's open file descriptions between its fork and its exec, so a
//!   `<db>.app.lock` dropped by one test while another test spawns a child
//!   could stay held for that instant. Within a test, a lease the test
//!   asserts on is taken only after the children it concerns are spawned.
//!
//! Platforms: the tests run on Windows CI too, but have only been run on
//! macOS. Two are Unix only: the lost-race test needs a FIFO, and the pulse
//! test's margin (a 5 ms pulse against the 20 ms confirmation gap) does not
//! survive Windows' ~15.6 ms timer granularity.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use geode_data::STORE_FORMAT;
use geode_data::lease::{self, AppLease, acquire_app, collector_present};
use geode_data::store::stamp::read_format;
use geode_data::store::{Catalog, Store, StoreError};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_geode-collector");
/// The longest any test waits for anything.
const WAIT: Duration = Duration::from_secs(10);
/// The app's open deadline, as the app passes it.
const APP_DEADLINE: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(25);

/// The collector's log text for each event the tests wait on.
const RELEASED: &str = "released the store to the app";
#[cfg(unix)]
const YIELDED: &str = "app took the store first";
/// Logged once per hold, after the store opened and discovery ran: the
/// health of the temp world's one directory source.
const FILES_OK: &str = "geode::collector: files: ok";

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Poll `check` every 25 ms until it answers, failing after `limit`.
fn wait_until<T>(what: &str, limit: Duration, mut check: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            start.elapsed() < limit,
            "timed out after {limit:?} waiting for {what}"
        );
        std::thread::sleep(POLL);
    }
}

/// A child process, killed and reaped on drop.
struct Proc(Child);

impl Proc {
    fn exited(&mut self) -> Option<ExitStatus> {
        self.0.try_wait().expect("try_wait")
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One test's temp home, desk, inbox and store.
struct World {
    _dir: TempDir,
    home: PathBuf,
    user: PathBuf,
    desk: PathBuf,
    inbox: PathBuf,
    tmp: PathBuf,
    db: PathBuf,
    demo: Option<usize>,
}

const DATASETS: &str = "config_version = 1\n\
[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\nsource_name = \"Book\"\n\
[risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\nsource_name = \"LHU\"\n\
[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"dimension\"\nsource_name = \"PositionRef\"\n\
[risk.columns.counterparty]\ntype = \"utf8\"\nrole = \"dimension\"\nsource_name = \"Counterparty\"\n\
[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\nsource_name = \"NPV\"\n";

const CSV_HEADER: &str = "Book,LHU,PositionRef,Counterparty,NPV";

/// `path` with symlinks resolved on Unix (macOS's temp directory is under
/// the `/var` -> `/private/var` link, and the collector logs the resolved
/// store path). On Windows `canonicalize` gives a `\\?\` verbatim path,
/// which the configuration and DuckDB need not accept, so it is kept as is.
fn real_path(path: &Path) -> PathBuf {
    if cfg!(windows) {
        path.to_path_buf()
    } else {
        path.canonicalize().unwrap()
    }
}

impl World {
    fn dirs() -> (TempDir, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = real_path(dir.path());
        let home = root.join("home");
        let user = home.join(".config").join("geode");
        let desk = root.join("desk");
        let inbox = root.join("in");
        let tmp = root.join("tmp");
        for d in [&user, &desk, &inbox, &tmp] {
            std::fs::create_dir_all(d).unwrap();
        }
        (dir, home, user, desk, inbox, tmp)
    }

    /// A store at `<root>/store/g.duckdb` fed by one directory source,
    /// `files`, polling the inbox every second into dataset `risk`.
    fn files() -> World {
        World::files_with("")
    }

    /// [`World::files`] with `app_extra` appended to the user `app.toml`.
    fn files_with(app_extra: &str) -> World {
        let (dir, home, user, desk, inbox, tmp) = World::dirs();
        let db = real_path(dir.path()).join("store").join("g.duckdb");
        std::fs::write(
            user.join("app.toml"),
            format!(
                "config_version = 1\n[data]\ndb_path = {:?}\n[log]\ncollector = \"debug\"\ningest = \"debug\"\n{app_extra}",
                db.to_str().unwrap()
            ),
        )
        .unwrap();
        std::fs::write(desk.join("datasets.toml"), DATASETS).unwrap();
        std::fs::write(
            desk.join("sources.toml"),
            format!(
                "config_version = 1\n[files]\ndataset = \"risk\"\npaths = [{:?}]\npoll_interval = \"1s\"\n",
                inbox.join("*.csv").to_str().unwrap()
            ),
        )
        .unwrap();
        World {
            _dir: dir,
            home,
            user,
            desk,
            inbox,
            tmp,
            db,
            demo: None,
        }
    }

    /// The `--demo <rows>` store under the temp `TMPDIR`; no desk documents.
    fn demo(rows: usize) -> World {
        let (dir, home, user, desk, inbox, tmp) = World::dirs();
        std::fs::write(
            user.join("app.toml"),
            "config_version = 1\n[log]\ncollector = \"debug\"\n",
        )
        .unwrap();
        let db = tmp
            .join("geode-demo")
            .join(format!("{rows}-42"))
            .join("geode.duckdb");
        World {
            _dir: dir,
            home,
            user,
            desk,
            inbox,
            tmp,
            db,
            demo: Some(rows),
        }
    }

    fn command(&self, first: &str) -> Command {
        self.command_of(Path::new(BIN), first)
    }

    fn command_of(&self, bin: &Path, first: &str) -> Command {
        let mut cmd = Command::new(bin);
        cmd.arg(first);
        if let Some(rows) = self.demo {
            cmd.args(["--demo", &rows.to_string()]);
        }
        cmd.env("HOME", &self.home)
            .env_remove("APPDATA")
            .env_remove("LOCALAPPDATA")
            .env("GEODE_SERVICE", "1")
            .env("GEODE_DESK_CONFIG", &self.desk)
            .env("TMPDIR", &self.tmp)
            .env("TMP", &self.tmp)
            .env("TEMP", &self.tmp)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(self.stderr_file());
        cmd
    }

    /// Children's stderr (panics, an abort's message), appended to
    /// `<root>/stderr.txt` and shown when a collector exits unexpectedly.
    fn stderr_file(&self) -> File {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.stderr_path())
            .unwrap()
    }

    fn stderr_path(&self) -> PathBuf {
        self.home.with_file_name("stderr.txt")
    }

    /// A failure message for a collector that exited: its status, stderr
    /// and log.
    fn exited_message(&self, status: ExitStatus) -> String {
        let stderr = std::fs::read_to_string(self.stderr_path()).unwrap_or_default();
        format!(
            "the collector exited ({status:?})\n--- stderr\n{stderr}\n--- log\n{}",
            self.log()
        )
    }

    fn collector(&self) -> Proc {
        Proc(self.command("run").spawn().expect("spawn geode-collector"))
    }

    /// `geode-collector status` for this world.
    fn status(&self) -> String {
        let out = self
            .command("status")
            .stdout(Stdio::piped())
            .output()
            .expect("geode-collector status");
        assert!(out.status.success(), "status: {:?}", out.status);
        String::from_utf8(out.stdout).unwrap()
    }

    /// Every collector log file for this world's store, oldest first,
    /// concatenated.
    fn log(&self) -> String {
        let Ok(entries) = std::fs::read_dir(self.user.join("logs")) else {
            return String::new();
        };
        let prefix = format!("{}.", geode_collector::install::log_prefix(self.demo));
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect();
        files.sort();
        files
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .collect()
    }

    fn count(&self, needle: &str) -> usize {
        self.log().matches(needle).count()
    }

    /// Drop `<name>.csv` and its sentinel into the inbox: one row per book.
    fn drop_csv(&self, name: &str, books: &[&str]) {
        let mut csv = format!("{CSV_HEADER}\n");
        for (i, book) in books.iter().enumerate() {
            csv.push_str(&format!("{book},L1,P{i},C1,{}.5\n", i + 1));
        }
        write_with_sentinel(&self.inbox, name, &csv, books);
    }

    /// Wait until the collector has opened the store for the `holds`th time
    /// and holds it: alive, `status` running, and the store refusing a
    /// read-only open with DuckDB's lock conflict. The read-only probe runs
    /// only after the open is logged, so it never races the collector's own.
    fn wait_holding(&self, collector: &mut Proc, holds: usize, limit: Duration) {
        wait_until(&format!("hold {holds}"), limit, || {
            if let Some(status) = collector.exited() {
                panic!("{}", self.exited_message(status));
            }
            (self.count(FILES_OK) >= holds).then_some(())
        });
        assert!(
            self.status().starts_with("collector: running; app: absent"),
            "{}",
            self.status()
        );
        match read_format(&self.db) {
            // On Windows DuckDB names the holder (the conflict marker) only
            // when the Restart Manager finds it; a bare sharing violation is
            // still a refused open.
            Err(err) if cfg!(windows) => {
                assert!(matches!(err, StoreError::Open { .. }), "{err}")
            }
            Err(err) => assert!(lease::is_lock_conflict(&err), "{err}"),
            Ok(found) => panic!("the store opened read-only ({found:?}): no one holds it"),
        }
    }

    /// Wait until the log contains `needle`, failing with the log (and the
    /// collector's stderr, if it exited) after `limit`.
    fn wait_log(&self, collector: &mut Proc, needle: &str, limit: Duration) {
        let start = Instant::now();
        loop {
            if self.log().contains(needle) {
                return;
            }
            if let Some(status) = collector.exited() {
                panic!("{}", self.exited_message(status));
            }
            assert!(
                start.elapsed() < limit,
                "timed out after {limit:?} waiting for '{needle}'\n{}",
                self.log()
            );
            std::thread::sleep(POLL);
        }
    }

    /// The generation the collector logged for `dataset`/`batch`, once it
    /// has published it.
    fn wait_published(&self, dataset: &str, batch: &str, limit: Duration) -> i64 {
        wait_until(&format!("{dataset} {batch} published"), limit, || {
            published(&self.log())
                .get(&(dataset.into(), batch.into()))
                .copied()
        })
    }
}

fn write_with_sentinel(dir: &Path, name: &str, csv: &str, books: &[&str]) {
    std::fs::write(dir.join(format!("{name}.csv")), csv).unwrap();
    let books: Vec<String> = books.iter().map(|b| format!("{b:?}")).collect();
    std::fs::write(
        dir.join(format!("{name}.csv.done")),
        format!(
            "{{\"as_of\":\"2026-10-04T07:00:00Z\",\"columns\":[\"Book\",\"LHU\",\"PositionRef\",\"Counterparty\",\"NPV\"],\"books\":[{}]}}",
            books.join(",")
        ),
    )
    .unwrap();
}

/// The greatest generation logged per (dataset, batch): the collector's
/// `published <dataset> <batch> generation <n>` debug lines.
fn published(log: &str) -> BTreeMap<(String, String), i64> {
    let mut out = BTreeMap::new();
    for line in log.lines() {
        let Some(rest) = line.split("geode::collector: published ").nth(1) else {
            continue;
        };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if let [dataset, batch, "generation", generation] = parts[..]
            && let Ok(generation) = generation.parse::<i64>()
        {
            let slot = out
                .entry((dataset.to_string(), batch.to_string()))
                .or_insert(generation);
            *slot = (*slot).max(generation);
        }
    }
    out
}

/// Take the store as the app does: the lease, then `Store::open` retried
/// while the collector hands it over. Returns how long that took and
/// whether the open had to wait.
fn take_store(db: &Path) -> (AppLease, Store, Duration, bool) {
    let mut waited = false;
    let start = Instant::now();
    let (lease, store) = acquire_app(
        db,
        APP_DEADLINE,
        &|| false,
        &mut |_| waited = true,
        &mut || Store::open(db),
    )
    .unwrap_or_else(|e| panic!("acquire_app: {e}"));
    (lease, store, start.elapsed(), waited)
}

fn live(store: &Store, dataset: &str, batch: &str, book: Option<&str>) -> Option<i64> {
    Catalog::new(store.writer())
        .live_generation(dataset, batch, book)
        .unwrap()
}

#[test]
fn a_collector_publishes_and_hands_the_store_to_an_app() {
    let _serial = serial();
    let world = World::files();
    world.drop_csv("r1", &["BK1", "BK2"]);
    let mut collector = world.collector();
    world.wait_holding(&mut collector, 1, WAIT);
    let generation = world.wait_published("risk", "r1", WAIT);

    let (lease, store, took, waited) = take_store(&world.db);
    assert!(took < APP_DEADLINE, "{took:?}");
    assert!(
        waited,
        "the collector held the store, so the first open waits"
    );
    assert_eq!(live(&store, "risk", "r1", Some("BK1")), Some(generation));
    assert_eq!(live(&store, "risk", "r1", Some("BK2")), Some(generation));
    wait_until("the release line", WAIT, || {
        (world.count(RELEASED) == 1).then_some(())
    });
    assert!(collector.exited().is_none(), "the collector stays up");
    drop(store);
    drop(lease);
}

#[test]
fn dropping_the_app_lease_returns_the_store_to_the_collector() {
    let _serial = serial();
    let world = World::files();
    world.drop_csv("r1", &["BK1"]);
    let mut collector = world.collector();
    world.wait_holding(&mut collector, 1, WAIT);
    world.wait_published("risk", "r1", WAIT);

    let (lease, store, _, _) = take_store(&world.db);
    drop(store);
    drop(lease);
    // Review Focus 3, part 1: the collector holds the store again within
    // 2 s of the app letting go.
    world.wait_holding(&mut collector, 2, Duration::from_secs(2));

    world.drop_csv("r2", &["BK1"]);
    let generation = world.wait_published("risk", "r2", WAIT);
    let (lease, store, _, waited) = take_store(&world.db);
    assert!(waited);
    assert_eq!(live(&store, "risk", "r2", Some("BK1")), Some(generation));
    drop(store);
    drop(lease);
}

#[test]
fn a_second_collector_exits_zero() {
    let _serial = serial();
    let world = World::files();
    let mut first = world.collector();
    world.wait_holding(&mut first, 1, WAIT);

    let mut second = world.collector();
    let status = wait_until("the second collector to exit", WAIT, || second.exited());
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(world.log().contains("another collector has this store"));
    assert!(first.exited().is_none(), "the first collector stays up");
    assert!(collector_present(&world.db).unwrap());
}

/// The app side of `a_killed_app_returns_the_store`: takes the store when
/// `GEODE_TEST_APP_DB` names it, prints `HOLDING`, and holds it until stdin
/// closes or the process is killed. Without the variable it does nothing.
#[test]
#[ignore = "a child process of a_killed_app_returns_the_store"]
fn app_holder_child() {
    let Some(db) = std::env::var_os(APP_CHILD_ENV) else {
        return;
    };
    let db = PathBuf::from(db);
    let (lease, store, _, _) = take_store(&db);
    println!("HOLDING");
    std::io::stdout().flush().unwrap();
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
    drop(store);
    drop(lease);
}

const APP_CHILD_ENV: &str = "GEODE_TEST_APP_DB";

/// Review Focus 3: when the app process dies, the OS releases
/// `<db>.app.lock` and the collector takes the store back and publishes
/// within 2 s plus one discovery poll.
#[test]
fn a_killed_app_returns_the_store() {
    let _serial = serial();
    let world = World::files();
    world.drop_csv("r1", &["BK1"]);
    let mut collector = world.collector();
    world.wait_holding(&mut collector, 1, WAIT);
    world.wait_published("risk", "r1", WAIT);

    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--ignored",
            "--exact",
            "app_holder_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(APP_CHILD_ENV, &world.db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut app = Proc(child.spawn().expect("spawn the app child"));
    let stdout = app.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("HOLDING") {
                let _ = tx.send(());
                return;
            }
        }
    });
    rx.recv_timeout(WAIT).expect("the app child took the store");
    assert!(world.status().contains("app: present"));
    world.drop_csv("r2", &["BK1"]);

    app.0.kill().unwrap();
    app.0.wait().unwrap();
    let killed = Instant::now();
    // 2 s plus one discovery poll. Windows releases a dead process's
    // `LockFileEx` locks after a delay that "depends upon available system
    // resources" (the `LockFileEx` documentation), so it gets more room.
    let bound = if cfg!(windows) {
        Duration::from_secs(8)
    } else {
        Duration::from_secs(3)
    };
    world.wait_published("risk", "r2", bound);
    assert!(killed.elapsed() < bound);
    assert!(collector.exited().is_none());
}

/// Review Focus 4, across processes: every document the collector
/// published before or during the handoff is live for the app, and the
/// release dropped none. The test waits until the startup burst's CVI
/// documents are all logged as published and the collector has published
/// nothing for a second before taking the store: a channel adapter drops
/// messages it has not yet routed when its subscription closes, and a
/// release in the middle of the burst can meet its 2 s deadline (a debug
/// build publishes about 40 documents a second, and the burst is over a
/// hundred), dropping the rest by design;
/// either would test the demo bus's timing, not the handoff. A cadence
/// publish still held by a coalescer at the handoff would be flushed by the
/// release's drain and checked too, but the quiet wait means one rarely is:
/// the flush on release is covered by A4's unit test alone, not here.
#[test]
fn a_handoff_keeps_the_documents_the_collector_received() {
    let _serial = serial();
    let world = World::demo(100);
    let mut collector = world.collector();
    let underlyings = geode_demo_data::demo_underlyings();
    wait_until("every CVI document published", WAIT, || {
        if let Some(status) = collector.exited() {
            panic!("{}", world.exited_message(status));
        }
        let done = published(&world.log());
        underlyings
            .iter()
            .all(|u| done.contains_key(&("cvi_params".to_string(), u.clone())))
            .then_some(())
    });
    wait_quiet(&world, Duration::from_secs(1), WAIT);

    let (lease, store, _, _) = take_store(&world.db);
    wait_until("the release line", WAIT, || {
        (world.count(RELEASED) == 1).then_some(())
    });
    let log = world.log();
    assert!(!log.contains("release: dropped"), "{log}");
    let documents: Vec<_> = published(&log)
        .into_iter()
        .filter(|((dataset, _), _)| {
            ["cvi_params", "dividend_schedule", "option_chain"].contains(&dataset.as_str())
        })
        .collect();
    assert!(documents.len() >= underlyings.len());
    for ((dataset, key), generation) in &documents {
        let found = live(&store, dataset, key, None);
        assert!(
            found.is_some_and(|g| g >= *generation),
            "{dataset} {key}: logged generation {generation}, live {found:?}"
        );
    }
    drop(store);
    drop(lease);
}

/// Review Focus 5 for the binary: a store stamped by another format is not
/// opened for writing. One error names both formats, no payload table is
/// created, and the collector stays up, idling.
#[test]
fn a_mismatched_stamp_idles_the_collector() {
    let _serial = serial();
    let world = World::files();
    let other = STORE_FORMAT + 1;
    {
        let store = Store::open(&world.db).unwrap();
        store
            .writer()
            .execute_batch(&format!(
                "CREATE TABLE geode_meta (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);
                 INSERT INTO geode_meta VALUES ('store_format', '{other}');"
            ))
            .unwrap();
    }
    world.drop_csv("r1", &["BK1"]);
    let mut collector = world.collector();
    let mismatch =
        format!("is stamped store format {other}; this collector writes format {STORE_FORMAT}");
    // Timed from the collector's first line: the first launch of a freshly
    // linked binary can take seconds on macOS before `main` runs.
    world.wait_log(&mut collector, "collecting into", WAIT);
    world.wait_log(&mut collector, &mismatch, Duration::from_secs(3));
    // Give a repeat, a publish or an exit time to show.
    std::thread::sleep(Duration::from_secs(1));
    let log = world.log();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains(&mismatch)).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains(" ERROR geode::collector:"), "{log}");
    assert!(collector.exited().is_none());
    assert!(world.status().starts_with("collector: running"));

    let store = Store::open(&world.db).unwrap();
    let tables: Vec<String> = {
        let mut stmt = store
            .writer()
            .prepare("select table_name from duckdb_tables() order by 1")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(tables, vec!["geode_meta".to_string()]);
}

/// A collector idling on a mismatched stamp never reaches a release, so it
/// checks its executable while it idles: a rebuilt binary exits 75 for the
/// service manager to start the new build. The test runs a copy of the
/// binary and replaces the copy (a new file renamed over it, as a build
/// does), so the shared binary is never touched. The collector also logs
/// its desk and user configuration directories at start.
///
/// macOS only: Linux resolves the running executable to the replaced
/// inode, and Windows refuses to replace a running executable.
#[cfg(target_os = "macos")]
#[test]
fn a_rebuilt_binary_restarts_an_idle_collector() {
    let _serial = serial();
    let world = World::files();
    let other = STORE_FORMAT + 1;
    {
        let store = Store::open(&world.db).unwrap();
        store
            .writer()
            .execute_batch(&format!(
                "CREATE TABLE geode_meta (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);
                 INSERT INTO geode_meta VALUES ('store_format', '{other}');"
            ))
            .unwrap();
    }
    let bin_dir = world.tmp.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let exe = bin_dir.join("geode-collector");
    std::fs::copy(BIN, &exe).unwrap();
    let mut collector = Proc(
        world
            .command_of(&exe, "run")
            .spawn()
            .expect("spawn the copied geode-collector"),
    );
    world.wait_log(&mut collector, "collecting into", WAIT);
    world.wait_log(
        &mut collector,
        &format!(
            "configuration: desk {}, user {}",
            world.desk.display(),
            world.user.display()
        ),
        Duration::from_secs(3),
    );
    world.wait_log(
        &mut collector,
        "is stamped store format",
        Duration::from_secs(3),
    );
    assert!(collector.exited().is_none());

    // A rebuild: a new file with another size renamed over the old one.
    let next = bin_dir.join("geode-collector.next");
    std::fs::copy(BIN, &next).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&next)
        .unwrap()
        .write_all(b"rebuilt")
        .unwrap();
    std::fs::rename(&next, &exe).unwrap();
    let status = wait_until("the exit after the rebuild", WAIT, || collector.exited());
    assert_eq!(status.code(), Some(75), "{}", world.log());
    assert!(world.log().contains("the collector executable changed"));
}

/// A `status` probe, or anything else, holding `<db>.app.lock` for an
/// instant is not an app: the collector releases only on an app confirmed
/// by a second probe. Five seconds of 5 ms pulses with 40 ms between must
/// leave the store with the collector: an overshot sleep keeps a pulse under
/// the 20 ms confirmation gap, and the 45 ms period never puts a pulse 20 ms
/// after another. 5 ms rather than shorter keeps the single-probe mutation
/// caught: it must see a pulse on some poll within the five seconds.
///
/// Unix only: Windows' timer granularity (about 15.6 ms) turns a 5 ms sleep
/// into one that eats most of the margin against the 20 ms gap.
#[cfg(unix)]
#[test]
fn a_momentary_app_lock_does_not_release_the_store() {
    let _serial = serial();
    let world = World::files();
    let mut collector = world.collector();
    world.wait_holding(&mut collector, 1, WAIT);

    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lease::app_lock_path(&world.db))
        .unwrap();
    let start = Instant::now();
    // The longest pulse: one past the collector's 20 ms confirmation gap
    // would be an app by its rule, and a release then would be right.
    let mut longest = Duration::ZERO;
    while start.elapsed() < Duration::from_secs(5) {
        // The collector's own probe holds the lock for an instant.
        wait_until("the app lock between probes", WAIT, || lock.try_lock().ok());
        let held = Instant::now();
        std::thread::sleep(Duration::from_millis(5));
        lock.unlock().unwrap();
        longest = longest.max(held.elapsed());
        std::thread::sleep(Duration::from_millis(40));
    }
    drop(lock);

    let log = world.log();
    assert!(!log.contains(RELEASED), "longest pulse {longest:?}\n{log}");
    assert!(collector_present(&world.db).unwrap());
    assert!(collector.exited().is_none());
    assert_eq!(world.count(FILES_OK), 1, "one hold, never released");
}

/// `O_NONBLOCK`: an open of a FIFO's write end with it fails (`ENXIO`)
/// while no reader has the FIFO open, rather than blocking.
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(all(unix, not(target_os = "linux")))]
const O_NONBLOCK: i32 = 0x0004;

/// The FIFO's write end, if a reader has the FIFO open (or is blocked
/// opening it); `None` while no reader does.
#[cfg(unix)]
fn fifo_writer(path: &Path) -> Option<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .custom_flags(O_NONBLOCK)
        .open(path)
        .ok()
}

/// An app that takes the store between the collector's checks and its open
/// wins the race: the collector's open fails while the app is present, and
/// that is a yield, not a fault. It must neither exit 70 nor give up its
/// lease.
///
/// The race is made deterministic with a FIFO in the desk directory: the
/// collector reads every desk `*.toml` when it loads its configuration at
/// each acquire, after the app probe and the stamp check, and blocks
/// opening the FIFO until a writer opens it. The test takes the store as
/// the app while the collector is blocked there, then lets it go on to its
/// open.
///
/// The FIFO appears only after the start-up load: until then a raw lock on
/// `<db>.app.lock`, taken before the spawn and released long after it,
/// keeps the collector waiting for the app. Unix only: it needs a FIFO.
#[cfg(unix)]
#[test]
fn a_lost_race_is_a_yield_not_an_exit() {
    let _serial = serial();
    let world = World::files();
    let gate = world.desk.join("gate.toml");
    std::fs::create_dir_all(world.db.parent().unwrap()).unwrap();
    let app_lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lease::app_lock_path(&world.db))
        .unwrap();
    app_lock.try_lock().unwrap();
    let mut collector = world.collector();
    wait_until("the lease", WAIT, || {
        world.log().contains("collecting into").then_some(())
    });
    let made = Command::new("mkfifo").arg(&gate).status().expect("mkfifo");
    assert!(made.success());
    app_lock.unlock().unwrap();
    drop(app_lock);

    // Within a second the collector sees no app, finds no stamp and blocks
    // in the acquire's load: the app takes the store now.
    let held = wait_until("the collector to load its configuration", WAIT, || {
        fifo_writer(&gate)
    });
    let (lease, store, _, waited) = take_store(&world.db);
    assert!(!waited, "the collector had not opened the store");
    drop(held);
    // A writer that closes before the blocked reader has woken can leave
    // it asleep (macOS), so open and close the write end again until the
    // collector moves on. No other read of the FIFO can begin before the
    // yield: the next load waits for the app to go.
    wait_until("the collector to acquire", WAIT, || {
        if world.log().contains("acquiring the store") {
            return Some(());
        }
        drop(fifo_writer(&gate));
        None
    });
    wait_until("the yield", WAIT, || {
        if let Some(status) = collector.exited() {
            panic!("{}", world.exited_message(status));
        }
        world.log().contains(YIELDED).then_some(())
    });
    std::thread::sleep(Duration::from_millis(500));
    assert!(collector.exited().is_none(), "{}", world.log());
    assert!(collector_present(&world.db).unwrap());
    assert!(!world.log().contains("exiting with status"));
    drop(store);
    drop(lease);
}

// ---- the handoff measurement (spec §7): not a test

/// The handoff time recorded in `docs/perf.md`: from `acquire_app` to an
/// open store, against the collector running idle, right after it opened
/// (the demo bus's startup burst in flight), and loading large files.
/// Inert unless `GEODE_MEASURE_HANDOFF` is set:
///
/// ```text
/// GEODE_MEASURE_HANDOFF=1 cargo test --release -p geode-collector \
///     --test handoff -- --ignored measure_handoff --nocapture
/// ```
///
/// `GEODE_MEASURE_RUNS` (default 10) sets the runs per case;
/// `GEODE_MEASURE_SKIP_DEMO` skips the demo cases. Emitting the
/// 100,000-row demo store and loading the large files take minutes, so its
/// waits are longer than the tests'; each is still bounded.
///
/// The large-file cases are 1,000,000-row files under the default
/// `[collector] memory_limit` (unset: DuckDB's own) and 2,000,000-row files
/// under 1GB. The 1,000,000-row figures in `docs/perf.md` were recorded at
/// 512MB, the default then; at 512MB DuckDB aborted the collector on large
/// loads (`temporary_memory_manager.cpp` assertion), which is why the
/// default is now unset.
#[test]
#[ignore = "a measurement: set GEODE_MEASURE_HANDOFF=1 and build with --release"]
fn measure_handoff() {
    if std::env::var_os("GEODE_MEASURE_HANDOFF").is_none() {
        return;
    }
    let _serial = serial();
    let runs = std::env::var("GEODE_MEASURE_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    if std::env::var_os("GEODE_MEASURE_SKIP_DEMO").is_none() {
        measure_demo(runs);
    }
    measure_large_files(runs, 1_000_000, None);
    measure_large_files(runs, 2_000_000, Some("1GB"));
}

const PUBLISHED: &str = "geode::collector: published ";
const LONG: Duration = Duration::from_secs(600);

/// One handoff's figures.
struct Sample {
    took: Duration,
    waited: bool,
    /// Publishes logged from the call to `acquire_app` to the release line:
    /// what the release drained, plus any publish in the 100 ms before the
    /// collector saw the app.
    drained: usize,
    /// The collector's own `released the store to the app in N ms`.
    release_ms: Option<u64>,
}

/// Take the store, read the release's figures from the log, give the store
/// back, and wait until the collector has opened it again (`marker`, a
/// health line logged once per hold, appears once more).
fn handoff_once(world: &World, collector: &mut Proc, marker: &str) -> Sample {
    let published_before = world.count(PUBLISHED);
    let released_before = world.count(RELEASED);
    let (lease, store, took, waited) = take_store(&world.db);
    let log = wait_until("the release line", WAIT, || {
        if let Some(status) = collector.exited() {
            panic!("{}", world.exited_message(status));
        }
        let log = world.log();
        (log.matches(RELEASED).count() > released_before).then_some(log)
    });
    let at = log.rfind(RELEASED).unwrap();
    let drained = log[..at].matches(PUBLISHED).count() - published_before;
    let release_ms = log[at + RELEASED.len()..]
        .trim_start()
        .strip_prefix("in ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    let holds = world.count(marker);
    drop(store);
    drop(lease);
    wait_until("the collector to hold again", WAIT, || {
        (world.count(marker) > holds).then_some(())
    });
    Sample {
        took,
        waited,
        drained,
        release_ms,
    }
}

/// Wait until no publish has been logged for `quiet`.
fn wait_quiet(world: &World, quiet: Duration, limit: Duration) {
    let mut last = world.count(PUBLISHED);
    let mut since = Instant::now();
    wait_until("a quiet collector", limit, || {
        let now = world.count(PUBLISHED);
        if now != last {
            last = now;
            since = Instant::now();
        }
        (since.elapsed() >= quiet).then_some(())
    });
}

fn report(label: &str, samples: &[Sample]) {
    let mut ms: Vec<f64> = samples
        .iter()
        .map(|s| s.took.as_secs_f64() * 1000.0)
        .collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = ms.len();
    let median = if n.is_multiple_of(2) {
        (ms[n / 2 - 1] + ms[n / 2]) / 2.0
    } else {
        ms[n / 2]
    };
    println!(
        "{label}: n {n}, median {median:.0} ms, min {:.0} ms, max {:.0} ms",
        ms[0],
        ms[n - 1]
    );
    for s in samples {
        println!(
            "  {:>6.0} ms  waited {}  drained {:>3}  collector release {:?} ms",
            s.took.as_secs_f64() * 1000.0,
            s.waited,
            s.drained,
            s.release_ms
        );
    }
}

/// The populated demo store (`--demo 100000`): the collector idle, then
/// right after each reopen.
fn measure_demo(runs: usize) {
    let world = World::demo(100_000);
    let mut collector = world.collector();
    let marker = "geode::collector: cvi: ok";
    wait_until("the demo store emitted and opened", LONG, || {
        if let Some(status) = collector.exited() {
            panic!("{}", world.exited_message(status));
        }
        (world.count(marker) >= 1).then_some(())
    });
    wait_quiet(&world, Duration::from_secs(3), LONG);

    let mut idle = Vec::new();
    for _ in 0..runs {
        idle.push(handoff_once(&world, &mut collector, marker));
        // The reopened collector's bus bursts again; let it settle.
        wait_quiet(&world, Duration::from_secs(2), LONG);
    }
    report("demo 100000, collector idle", &idle);

    // Right after each reopen: the bus's startup burst is in flight.
    let mut burst = Vec::new();
    handoff_once(&world, &mut collector, marker);
    for _ in 0..runs {
        burst.push(handoff_once(&world, &mut collector, marker));
    }
    report("demo 100000, right after the collector opened", &burst);
    assert!(collector.exited().is_none());
}

/// Three large CSVs dropped into the directory source; the store is taken
/// once discovery has queued them and the first load is under way. The
/// release lets that load finish and leaves the others for the next hold.
fn measure_large_files(runs: usize, rows: usize, memory_limit: Option<&str>) {
    const FILES: usize = 3;
    let world = World::files_with(
        &memory_limit
            .map(|limit| format!("[collector]\nmemory_limit = {limit:?}\n"))
            .unwrap_or_default(),
    );
    let mut collector = world.collector();
    world.wait_holding(&mut collector, 1, WAIT);
    let books: Vec<String> = (0..20).map(|b| format!("BK{b:03}")).collect();
    let book_refs: Vec<&str> = books.iter().map(String::as_str).collect();
    let mut body = String::with_capacity(rows * 32);
    body.push_str(CSV_HEADER);
    body.push('\n');
    for i in 0..rows {
        body.push_str(&format!(
            "{},L{},P{i},C{},{}.25\n",
            books[i % books.len()],
            i % 7,
            i % 13,
            i % 1000
        ));
    }
    let ready = format!("polled files: {FILES} ready");

    let mut samples = Vec::new();
    for run in 0..runs {
        let polled = world.count(&ready);
        let names: Vec<String> = (0..FILES).map(|k| format!("big{run}_{k}")).collect();
        for name in &names {
            write_with_sentinel(&world.inbox, name, &body, &book_refs);
        }
        wait_until("discovery to queue the files", WAIT, || {
            (world.count(&ready) > polled).then_some(())
        });
        std::thread::sleep(Duration::from_millis(300));
        samples.push(handoff_once(&world, &mut collector, FILES_OK));
        wait_until("the files loaded after the handoff", LONG, || {
            if let Some(status) = collector.exited() {
                panic!("{}", world.exited_message(status));
            }
            let done = published(&world.log());
            names
                .iter()
                .all(|n| done.contains_key(&("risk".to_string(), n.clone())))
                .then_some(())
        });
        for name in &names {
            let _ = std::fs::remove_file(world.inbox.join(format!("{name}.csv")));
            let _ = std::fs::remove_file(world.inbox.join(format!("{name}.csv.done")));
        }
    }
    report(
        &format!(
            "{FILES} files of {rows} rows, the first loading (memory_limit {})",
            memory_limit.unwrap_or("default")
        ),
        &samples,
    );
}
