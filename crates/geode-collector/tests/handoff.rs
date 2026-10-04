//! Cross-process handoff: the real `geode-collector` binary against a temp
//! store, with this test process (or a child of it) as the app.
//!
//! Every test builds a temp world: `HOME` points into a temp directory,
//! `APPDATA` and `LOCALAPPDATA` are removed, `GEODE_DESK_CONFIG` names a temp
//! desk directory and `TMPDIR` a temp directory (the `--demo` store lives
//! there). The user `app.toml` sets `data.db_path` and `[log] collector =
//! "debug"`, so the collector's publish lines reach
//! `<temp home>/.config/geode/logs/collector.*.log`, which the tests read.
//! The real user configuration is never read or written.
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
//! Unix only: the lock and sharing semantics on Windows (DuckDB's sharing
//! violation, Task Scheduler) have not been run on a Windows machine, and
//! the lost-race test needs a FIFO.
#![cfg(unix)]

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
use geode_data::store::{Catalog, Store};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_geode-collector");
/// The longest any test waits for anything.
const WAIT: Duration = Duration::from_secs(10);
/// The app's open deadline, as the app passes it.
const APP_DEADLINE: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(25);

/// The collector's log text for each event the tests wait on.
const RELEASED: &str = "released the store to the app";
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

impl World {
    fn dirs() -> (TempDir, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
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
        let (dir, home, user, desk, inbox, tmp) = World::dirs();
        let db = dir
            .path()
            .canonicalize()
            .unwrap()
            .join("store")
            .join("g.duckdb");
        std::fs::write(
            user.join("app.toml"),
            format!(
                "config_version = 1\n[data]\ndb_path = {:?}\n[log]\ncollector = \"debug\"\ningest = \"debug\"\n",
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
        let mut cmd = Command::new(BIN);
        cmd.arg(first);
        if let Some(rows) = self.demo {
            cmd.args(["--demo", &rows.to_string()]);
        }
        cmd.env("HOME", &self.home)
            .env_remove("APPDATA")
            .env_remove("LOCALAPPDATA")
            .env_remove("GEODE_SERVICE")
            .env("GEODE_DESK_CONFIG", &self.desk)
            .env("TMPDIR", &self.tmp)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
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

    /// Every collector log file, oldest first, concatenated.
    fn log(&self) -> String {
        let Ok(entries) = std::fs::read_dir(self.user.join("logs")) else {
            return String::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("collector."))
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
                panic!("the collector exited ({status:?}):\n{}", self.log());
            }
            (self.count(FILES_OK) >= holds).then_some(())
        });
        assert!(
            self.status().starts_with("collector: running; app: absent"),
            "{}",
            self.status()
        );
        match read_format(&self.db) {
            Err(err) => assert!(lease::is_lock_conflict(&err), "{err}"),
            Ok(found) => panic!("the store opened read-only ({found:?}): no one holds it"),
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
    world.wait_published("risk", "r2", Duration::from_secs(3));
    assert!(killed.elapsed() < Duration::from_secs(3));
    assert!(collector.exited().is_none());
}

/// Review Focus 4, across processes: every document the collector
/// published before or during the handoff is live for the app. The test
/// waits until the startup burst's CVI documents are all logged as
/// published before taking the store: a channel adapter drops messages it
/// has not yet routed when its subscription closes, so taking the store
/// earlier would test the demo bus's timing, not the handoff. The release's
/// own drain publishes whatever the coalescers still hold; those lines are
/// logged before the release line and are checked too.
#[test]
fn a_handoff_keeps_the_documents_the_collector_received() {
    let _serial = serial();
    let world = World::demo(100);
    let mut collector = world.collector();
    let underlyings = geode_demo_data::demo_underlyings();
    wait_until("every CVI document published", WAIT, || {
        if let Some(status) = collector.exited() {
            panic!("the collector exited ({status:?}):\n{}", world.log());
        }
        let done = published(&world.log());
        underlyings
            .iter()
            .all(|u| done.contains_key(&("cvi_params".to_string(), u.clone())))
            .then_some(())
    });

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
    wait_until("the mismatch error", Duration::from_secs(3), || {
        world.log().contains(&mismatch).then_some(())
    });
    // Give a repeat, a publish or an exit time to show.
    std::thread::sleep(Duration::from_secs(1));
    let log = world.log();
    assert_eq!(log.matches(&mismatch).count(), 1, "{log}");
    assert!(log.contains(" ERROR geode::collector:"), "{log}");
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

/// A `status` probe, or anything else, holding `<db>.app.lock` for an
/// instant is not an app: the collector releases only on an app confirmed
/// by a second probe. Five seconds of 10 ms pulses every 50 ms must leave
/// the store with the collector.
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
    while start.elapsed() < Duration::from_secs(5) {
        lock.try_lock().expect("nothing else holds the app lock");
        std::thread::sleep(Duration::from_millis(10));
        lock.unlock().unwrap();
        std::thread::sleep(Duration::from_millis(40));
    }
    drop(lock);

    let log = world.log();
    assert!(!log.contains(RELEASED), "{log}");
    assert!(collector_present(&world.db).unwrap());
    assert!(collector.exited().is_none());
    assert_eq!(world.count(FILES_OK), 1, "one hold, never released");
}

/// `O_NONBLOCK`: an open of a FIFO's write end with it fails (`ENXIO`)
/// while no reader has the FIFO open, rather than blocking.
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(not(target_os = "linux"))]
const O_NONBLOCK: i32 = 0x0004;

/// The FIFO's write end, opened once the collector is blocked opening it
/// for reading. While the test holds it the collector's read stays blocked;
/// dropping it ends the read with an empty document.
fn fifo_writer_when_read(path: &Path) -> File {
    use std::os::unix::fs::OpenOptionsExt;
    wait_until("the collector to read its configuration", WAIT, || {
        OpenOptions::new()
            .write(true)
            .custom_flags(O_NONBLOCK)
            .open(path)
            .ok()
    })
}

/// An app that takes the store between the collector's checks and its open
/// wins the race: the collector's open fails while the app is present, and
/// that is a yield, not a fault. It must neither exit 70 nor give up its
/// lease.
///
/// The race is made deterministic with a FIFO in the desk directory: the
/// collector reads every desk `*.toml` when it loads its configuration, once
/// at start and again at each acquire after the stamp check, and blocks on
/// the FIFO until the test writes it. The test takes the store as the app
/// while the collector is blocked there, then lets it go on to its open.
#[test]
fn a_lost_race_is_a_yield_not_an_exit() {
    let _serial = serial();
    let world = World::files();
    let gate = world.desk.join("gate.toml");
    let made = Command::new("mkfifo").arg(&gate).status().expect("mkfifo");
    assert!(made.success());
    let mut collector = world.collector();
    // The start-up load. The lease line is logged after that read reached
    // the FIFO's end, so the next reader is the acquire's load.
    drop(fifo_writer_when_read(&gate));
    wait_until("the lease", WAIT, || {
        world.log().contains("collecting into").then_some(())
    });
    // The collector found no app and no stamp and is blocked in the
    // acquire's load: the app takes the store now, then lets it go on.
    let held = fifo_writer_when_read(&gate);
    let (lease, store, _, waited) = take_store(&world.db);
    assert!(!waited, "the collector had not opened the store");
    drop(held);
    wait_until("the yield", WAIT, || {
        if let Some(status) = collector.exited() {
            panic!("the collector exited ({status:?}):\n{}", world.log());
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
