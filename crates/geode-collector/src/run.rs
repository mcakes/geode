//! The collector loop: hold the store while no app has it, hand it over
//! when one appears.
//!
//! ```text
//! take <db>.collector.lock (retried 1 s; else another collector: exit 0)
//! loop:
//!     wait until no app holds <db>.app.lock          poll APP_POLL
//!     read the store stamp: a busy store waits again; a mismatch idles
//!         STAMP_RECHECK
//!     load configuration; DataService::spawn_as(Collector); demo bus
//!     while no app and no stopped thread: sleep HOLD_POLL
//!     stopped: an app present is a lost race (wait again); else exit 70
//!     release(HANDOFF_DRAIN); a changed executable exits 0
//! ```
//!
//! The collector spawns no child process: a fork would share the lease's
//! open file description, and the lock would outlive a drop until the
//! child's exec.

use std::collections::HashMap;
use std::fmt::Display;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use geode_core::config::{Diagnostic, Severity};
use geode_core::health::Health;
use geode_core::log::{LevelControl, LogLevels};
use geode_data::lease::{CollectorLease, app_present, try_collector};
use geode_data::store::StoreError;
use geode_data::store::stamp::read_format;
use geode_data::{
    DEFAULT_STORE_DEADLINE, DataEvent, DataService, EventSink, HANDOFF_DRAIN, STORE_FORMAT,
    StoreRole,
};
use tracing::Level;

/// How often the collector looks for the app while it waits for the store.
pub const APP_POLL: Duration = Duration::from_secs(1);
/// How often the collector looks for the app while it holds the store: an
/// app's open waits at most this long before the release starts.
pub const HOLD_POLL: Duration = Duration::from_millis(100);
/// How often a store stamped with another format is read again.
pub const STAMP_RECHECK: Duration = Duration::from_secs(30);
/// How long a refused stamp read with no app present counts as the store
/// being busy before it is reported as unreadable and rechecked every
/// `STAMP_RECHECK`: the app's own open deadline, past which an app that
/// was opening the store has given up.
pub const BUSY_LIMIT: Duration = DEFAULT_STORE_DEADLINE;
/// The gap between the two probes that confirm an app before a release. A
/// `status` probe holds `<db>.app.lock` for an instant only; an app holds
/// it for its whole life.
const CONFIRM_GAP: Duration = Duration::from_millis(20);
/// How long a refused collector lease is retried before another collector
/// is assumed. A dropping lease can stay shared with a child process for
/// the instant between its spawn and its exec; a second collector, or a
/// `status` probe, can hold the lock for an instant too.
const LEASE_RETRY: Duration = Duration::from_secs(1);
const LEASE_STEP: Duration = Duration::from_millis(20);

/// The exit status for a collector that cannot go on: launchd's
/// `KeepAlive { SuccessfulExit = false }` and Task Scheduler's restart on
/// failure start it again, throttled.
pub const EXIT_FAILED: i32 = 70;

const TARGET: &str = "geode::collector";

/// The reason the data service gives when its open fails, before the error.
const OPEN_FAILURE: &str = "data service failed to open: ";

/// The executable's size and modification time, recorded at start and
/// compared after each release: a rebuilt or upgraded binary exits so the
/// service manager starts the new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExeStamp {
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl ExeStamp {
    /// `None` when the file cannot be read (it may be mid-replacement).
    pub fn of(path: &Path) -> Option<ExeStamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(ExeStamp {
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    pub fn current() -> Option<ExeStamp> {
        ExeStamp::of(&std::env::current_exe().ok()?)
    }
}

/// True when the executable differs from the one recorded at start: its
/// size or modification time changed, or it could be read then and cannot
/// now (a replacement in progress). Unreadable both times is unchanged.
pub fn exe_changed(before: Option<ExeStamp>, after: Option<ExeStamp>) -> bool {
    before != after
}

/// The collector loop. Returns the process exit status: 0 when another
/// collector has the store or the executable changed, [`EXIT_FAILED`] when
/// a data thread stopped with no app to explain it, or setup failed.
/// `clock` times the holds and releases it logs.
pub fn run(demo_rows: Option<usize>, clock: &dyn Fn() -> Instant) -> i32 {
    run_with_levels(demo_rows, clock, None)
}

/// [`run`] applying `[log]` from `app.toml` through `levels` at start and
/// at every acquire, as the configuration is reloaded then.
pub fn run_with_levels(
    demo_rows: Option<usize>,
    clock: &dyn Fn() -> Instant,
    levels: Option<&dyn LevelControl>,
) -> i32 {
    let demo_root = crate::demo_root(demo_rows);
    if let (Some(rows), Some(root)) = (demo_rows, &demo_root)
        && let Err(e) = geode_compose::demo::ensure_emitted(root, rows)
    {
        tracing::error!(target: TARGET, "failed to emit sample data into {root:?}: {e}");
        return EXIT_FAILED;
    }
    let demo_root = demo_root.as_deref();
    let config = geode_compose::load_config(demo_root, geode_compose::config_dirs());
    apply_levels(&config, levels);
    let db = geode_compose::store_path(&config, demo_root);
    let _lease = match take_lease(&db, LEASE_RETRY) {
        Ok(Some(lease)) => lease,
        Ok(None) => {
            tracing::info!(target: TARGET, "another collector has this store ({}); exiting", db.display());
            return 0;
        }
        Err(e) => {
            tracing::error!(target: TARGET, "cannot take the collector lease: {e}");
            return EXIT_FAILED;
        }
    };
    tracing::info!(target: TARGET, "collecting into {}", db.display());
    let exe = ExeStamp::current();
    let mut stamp_reported: Option<StampCheck> = None;
    let mut busy_timer = BusyTimer::default();
    loop {
        wait_for_no_app(&db);
        let mut check = stamp_check(read_format(&db));
        let busy = matches!(check, StampCheck::Busy(_));
        let app = busy && app_present(&db).unwrap_or(true);
        if busy_timer.escalate(busy && !app, clock())
            && let StampCheck::Busy(reason) = check
        {
            check = StampCheck::Unreadable(reason);
        }
        match &check {
            StampCheck::Ready => stamp_reported = None,
            StampCheck::Busy(reason) => {
                // Usually an app that appeared since the poll. Otherwise a
                // store this process cannot open yet: said once per reason,
                // and an error once it outlasts BUSY_LIMIT.
                if app {
                    tracing::debug!(target: TARGET, "store busy: {reason}");
                } else if stamp_reported.as_ref() != Some(&check) {
                    tracing::warn!(
                        target: TARGET,
                        "cannot read the store's stamp yet, waiting up to {} s: {reason}",
                        BUSY_LIMIT.as_secs()
                    );
                    stamp_reported = Some(check.clone());
                }
                std::thread::sleep(APP_POLL);
                continue;
            }
            StampCheck::Mismatch(found) => {
                if stamp_reported.as_ref() != Some(&check) {
                    tracing::error!(
                        target: TARGET,
                        "{} is stamped store format {found}; this collector writes format \
                         {STORE_FORMAT}; idling until they match (rechecked every {} s)",
                        db.display(),
                        STAMP_RECHECK.as_secs()
                    );
                    stamp_reported = Some(check.clone());
                }
                std::thread::sleep(STAMP_RECHECK);
                continue;
            }
            StampCheck::Unreadable(reason) => {
                if stamp_reported.as_ref() != Some(&check) {
                    tracing::error!(
                        target: TARGET,
                        "cannot read the stamp of {}: {reason}; idling (rechecked every {} s)",
                        db.display(),
                        STAMP_RECHECK.as_secs()
                    );
                    stamp_reported = Some(check.clone());
                }
                std::thread::sleep(STAMP_RECHECK);
                continue;
            }
        }
        match hold(&db, demo_root, clock, levels) {
            Held::Released => {
                if exe_changed(exe, ExeStamp::current()) {
                    tracing::info!(target: TARGET, "the collector executable changed; exiting so the new build runs");
                    return 0;
                }
            }
            Held::Yielded => {}
            Held::Stopped => return EXIT_FAILED,
        }
    }
}

/// `try_collector`, retried every 20 ms for `retry` while another holder
/// has the lock. `Ok(None)` when it is still held at the end.
pub(crate) fn take_lease(db: &Path, retry: Duration) -> std::io::Result<Option<CollectorLease>> {
    let start = Instant::now();
    loop {
        if let Some(lease) = try_collector(db)? {
            return Ok(Some(lease));
        }
        if start.elapsed() >= retry {
            return Ok(None);
        }
        std::thread::sleep(LEASE_STEP);
    }
}

/// Return once no app holds `<db>.app.lock`. A probe error counts as an app
/// present (opening the store while unsure could stall an app's open) and
/// is warned once per distinct error.
fn wait_for_no_app(db: &Path) {
    let mut reported: Option<String> = None;
    loop {
        match app_present(db) {
            Ok(false) => return,
            Ok(true) => {}
            Err(e) => {
                let text = e.to_string();
                if reported.as_deref() != Some(&text) {
                    tracing::warn!(target: TARGET, "cannot probe for the app: {text}; waiting");
                    reported = Some(text);
                }
            }
        }
        std::thread::sleep(APP_POLL);
    }
}

/// What the stamp read before an acquire decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StampCheck {
    /// No store, no stamp yet, or this build's format: acquire.
    Ready,
    /// The read-only open was refused, typically because an app has the
    /// store: wait for the app again, never idle or exit.
    Busy(String),
    /// Stamped by a build with another layout: idle and recheck.
    Mismatch(u32),
    /// The store opened but its stamp could not be read: idle and recheck.
    Unreadable(String),
}

pub(crate) fn stamp_check(read: Result<Option<u32>, StoreError>) -> StampCheck {
    match read {
        Ok(Some(found)) if found != STORE_FORMAT => StampCheck::Mismatch(found),
        Ok(_) => StampCheck::Ready,
        Err(err @ StoreError::Open { .. }) => StampCheck::Busy(err.to_string()),
        Err(err) => StampCheck::Unreadable(err.to_string()),
    }
}

/// How long stamp reads have been refused with no app present.
#[derive(Debug, Default)]
pub(crate) struct BusyTimer {
    since: Option<Instant>,
}

impl BusyTimer {
    /// Record one stamp check at `now`; `refused_alone` is a refused read
    /// with no app present. True once refused reads alone have lasted
    /// `BUSY_LIMIT` without a break: no app is opening the store, so the
    /// refusal is the store's own fault. Any other check restarts the count.
    pub(crate) fn escalate(&mut self, refused_alone: bool, now: Instant) -> bool {
        if !refused_alone {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) >= BUSY_LIMIT
    }
}

/// True when `probe` sees an app twice, `gap` apart. One sighting can be a
/// `status` probe holding `<db>.app.lock` for an instant; releasing on it
/// would hand the store to nobody. A first `false` returns at once.
pub(crate) fn confirmed(probe: &mut dyn FnMut() -> bool, gap: Duration) -> bool {
    if !probe() {
        return false;
    }
    std::thread::sleep(gap);
    probe()
}

/// How a hold ended.
enum Held {
    /// The store went to the app.
    Released,
    /// The service stopped while an app was present: the app won the race
    /// for the store. Wait for it again.
    Yielded,
    /// A data thread stopped with no app present.
    Stopped,
}

/// One acquire: fresh configuration, the service in the collector role and
/// the demo bus, held until an app appears or a data thread stops.
fn hold(
    db: &Path,
    demo_root: Option<&Path>,
    clock: &dyn Fn() -> Instant,
    levels: Option<&dyn LevelControl>,
) -> Held {
    let config = geode_compose::load_config(demo_root, geode_compose::config_dirs());
    apply_levels(&config, levels);
    let configured = geode_compose::store_path(&config, demo_root);
    if configured != db {
        tracing::warn!(
            target: TARGET,
            "data.db_path now names {}; this collector keeps {} until it restarts",
            configured.display(),
            db.display()
        );
    }
    let (settings, diagnostics) = geode_compose::collector_settings(&config);
    log_config_diagnostics(&diagnostics);
    let (adapters, feed) = geode_compose::adapters(demo_root);
    let setup = geode_compose::engine_setup(&config, db.to_path_buf(), adapters);
    log_config_diagnostics(&setup.diagnostics);
    let today = geode_core::clock::Clock::from_config(&config)
        .0
        .today(chrono::Utc::now());

    let events = Arc::new(Events::new());
    let started = clock();
    tracing::info!(
        target: TARGET,
        "acquiring the store (memory_limit {})",
        settings.memory_limit
    );
    let handle = DataService::spawn_as(
        setup.config,
        Events::sink(&events),
        StoreRole::Collector {
            memory_limit: Some(settings.memory_limit),
        },
    );
    let mut bus = feed.map(|feed| geode_compose::demo_bus::spawn_default(feed, today));
    // A probe error counts as an app present: release rather than hold a
    // store an app may be waiting for. An app is confirmed by a second
    // probe, so a `status` probe's instant on the lock is not an app.
    let mut probe = || app_present(db).unwrap_or(true);
    while !events.stopped() && !confirmed(&mut probe, CONFIRM_GAP) {
        std::thread::sleep(HOLD_POLL);
    }

    if events.stopped() {
        handle.shutdown();
        if let Some(bus) = bus.as_mut() {
            bus.stop();
        }
        // The one place a stop is judged: the sink held back both the stop
        // and any open failure, so each is logged once, at this level.
        let app = confirmed(&mut || app_present(db).unwrap_or(false), CONFIRM_GAP);
        let (level, lines) = stop_report(&events.take_open_failures(), &events.take_stops(), app);
        if app {
            for line in &lines {
                log_at(level, &format_args!("app took the store first: {line}"));
            }
            return Held::Yielded;
        }
        for line in &lines {
            log_at(level, line);
        }
        tracing::error!(target: TARGET, "a data thread stopped with no app present; exiting with status {EXIT_FAILED}");
        return Held::Stopped;
    }

    let releasing = clock();
    handle.release(HANDOFF_DRAIN);
    if let Some(bus) = bus.as_mut() {
        bus.stop();
    }
    let now = clock();
    tracing::info!(
        target: TARGET,
        "released the store to the app in {} ms, after holding it {} s",
        now.duration_since(releasing).as_millis(),
        releasing.duration_since(started).as_secs()
    );
    Held::Released
}

/// What the loop logs when a hold ends with a stopped thread: each open
/// failure the sink held back, then each stop whose reason is not one of
/// them (a failed open reports the same text as both). With an app present
/// the stop is the app winning the race for the store, an expected yield:
/// info. With none it is a fault: error.
pub(crate) fn stop_report(
    open_failures: &[Diagnostic],
    stops: &[(String, String)],
    app_present: bool,
) -> (Level, Vec<String>) {
    let level = if app_present {
        Level::INFO
    } else {
        Level::ERROR
    };
    let mut lines: Vec<String> = open_failures.iter().map(|d| d.to_string()).collect();
    for (thread, reason) in stops {
        if !open_failures.iter().any(|d| &d.message == reason) {
            lines.push(format!("{thread} stopped: {reason}"));
        }
    }
    (level, lines)
}

fn apply_levels(config: &geode_core::config::Config, levels: Option<&dyn LevelControl>) {
    let Some(control) = levels else {
        return;
    };
    let (wanted, diagnostics) = LogLevels::from_doc(config);
    log_config_diagnostics(&diagnostics);
    if let Err(e) = control.set(&wanted) {
        tracing::warn!(target: TARGET, "cannot apply [log] levels: {e}");
    }
}

fn log_config_diagnostics(diagnostics: &[Diagnostic]) {
    for d in diagnostics {
        log_at(severity_level(d.severity), d);
    }
}

fn severity_level(severity: Severity) -> Level {
    match severity {
        Severity::Warning => Level::WARN,
        Severity::Error => Level::ERROR,
    }
}

fn log_at(level: Level, message: &dyn Display) {
    match level {
        Level::ERROR => tracing::error!(target: TARGET, "{message}"),
        Level::WARN => tracing::warn!(target: TARGET, "{message}"),
        Level::INFO => tracing::info!(target: TARGET, "{message}"),
        Level::DEBUG => tracing::debug!(target: TARGET, "{message}"),
        _ => tracing::trace!(target: TARGET, "{message}"),
    }
}

/// The data service's own open failure, which the sink holds back for the
/// loop to judge: with an app present it is the app winning the race.
fn is_open_failure(d: &Diagnostic) -> bool {
    d.message.starts_with(OPEN_FAILURE)
}

/// The service's event sink for one hold. Every arm logs and returns at
/// once; the locks are held only to update a map or a list.
pub(crate) struct Events {
    health: Mutex<HashMap<String, Health>>,
    stopped: AtomicBool,
    stops: Mutex<Vec<(String, String)>>,
    open_failures: Mutex<Vec<Diagnostic>>,
}

impl Events {
    pub(crate) fn new() -> Events {
        Events {
            health: Mutex::new(HashMap::new()),
            stopped: AtomicBool::new(false),
            stops: Mutex::new(Vec::new()),
            open_failures: Mutex::new(Vec::new()),
        }
    }

    fn take_open_failures(&self) -> Vec<Diagnostic> {
        std::mem::take(&mut *self.open_failures.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub(crate) fn sink(events: &Arc<Events>) -> EventSink {
        let events = Arc::clone(events);
        Arc::new(move |event| {
            events.handle(event);
            true
        })
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn take_stops(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.stops.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// True when `worst` differs from the last health reported for `source`
    /// (the first report is a change).
    pub(crate) fn health_changed(&self, source: &str, worst: &Health) -> bool {
        let mut health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        if health.get(source) == Some(worst) {
            return false;
        }
        health.insert(source.to_string(), worst.clone());
        true
    }

    pub(crate) fn handle(&self, event: DataEvent) {
        match event {
            DataEvent::Health {
                source,
                worst,
                detail,
            } => {
                if self.health_changed(&source, &worst) {
                    let label = worst.label();
                    if detail.is_empty() {
                        tracing::info!(target: TARGET, "{source}: {label}");
                    } else {
                        tracing::info!(target: TARGET, "{source}: {label} — {detail}");
                    }
                }
            }
            DataEvent::Published {
                dataset,
                batch,
                gen_id,
                ..
            } => {
                tracing::debug!(target: TARGET, "published {dataset} {batch} generation {gen_id}");
            }
            DataEvent::Diagnostics(diagnostics) => {
                for d in diagnostics {
                    if is_open_failure(&d) {
                        // Judged and logged by the loop with the stop.
                        self.open_failures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(d);
                    } else {
                        log_at(severity_level(d.severity), &d);
                    }
                }
            }
            DataEvent::SeriesFetched {
                source,
                identity,
                result: Err(reason),
            } => {
                tracing::warn!(target: TARGET, "{source} {identity}: fetch failed: {reason}");
            }
            // Logged by the loop once it knows whether an app explains it.
            DataEvent::ThreadStopped { thread, reason } => {
                self.stops
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((thread, reason));
                self.stopped.store(true, Ordering::Release);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn diagnostic(severity: Severity, message: &str) -> Diagnostic {
        Diagnostic {
            severity,
            layer: None,
            file: None,
            message: message.to_string(),
            path: None,
        }
    }

    fn stamp(size: u64, secs: u64) -> Option<ExeStamp> {
        Some(ExeStamp {
            size,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
        })
    }

    #[test]
    fn a_changed_size_or_mtime_is_a_changed_executable() {
        assert!(!exe_changed(stamp(10, 5), stamp(10, 5)));
        assert!(exe_changed(stamp(10, 5), stamp(11, 5)));
        assert!(exe_changed(stamp(10, 5), stamp(10, 6)));
        // Unreadable now (mid-replacement) is a change; unreadable both
        // times is not.
        assert!(exe_changed(stamp(10, 5), None));
        assert!(!exe_changed(None, None));
    }

    #[test]
    fn the_running_executable_has_a_stamp() {
        let exe = std::env::current_exe().unwrap();
        let first = ExeStamp::of(&exe);
        assert!(first.is_some());
        assert!(!exe_changed(first, ExeStamp::of(&exe)));
        assert_eq!(ExeStamp::of(Path::new("/does/not/exist")), None);
    }

    /// An open refused while an app holds the store means the app is
    /// opening it: the collector waits for the app again rather than idling
    /// or exiting. Only a parsed mismatch idles.
    #[test]
    fn a_refused_stamp_read_is_busy_and_only_a_mismatch_idles() {
        // A real refused read-only open: the file is not a DuckDB store.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.duckdb");
        std::fs::write(&db, b"not a duckdb file").unwrap();
        let refused = read_format(&db);
        assert!(
            matches!(refused, Err(StoreError::Open { .. })),
            "{refused:?}"
        );
        assert!(matches!(stamp_check(refused), StampCheck::Busy(_)));
        assert_eq!(stamp_check(Ok(None)), StampCheck::Ready);
        assert_eq!(stamp_check(Ok(Some(STORE_FORMAT))), StampCheck::Ready);
        assert_eq!(
            stamp_check(Ok(Some(STORE_FORMAT + 1))),
            StampCheck::Mismatch(STORE_FORMAT + 1)
        );
        assert!(matches!(
            stamp_check(Err(StoreError::Drift("x".into()))),
            StampCheck::Unreadable(_)
        ));
    }

    /// A refused read with no app present is busy until `BUSY_LIMIT` has
    /// passed without a break; then it escalates. A check that is not a
    /// lone refusal (an app present, a readable stamp) restarts the count.
    #[test]
    fn a_lone_refusal_escalates_after_the_busy_limit() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let limit = BUSY_LIMIT.as_millis() as u64;
        let mut timer = BusyTimer::default();
        assert!(!timer.escalate(true, at(0)));
        assert!(!timer.escalate(true, at(limit - 1)));
        assert!(timer.escalate(true, at(limit)));
        assert!(timer.escalate(true, at(limit + 30_000)));
        // An app appearing (or a clean read) restarts the count.
        assert!(!timer.escalate(false, at(limit + 31_000)));
        assert!(!timer.escalate(true, at(limit + 32_000)));
        assert!(!timer.escalate(true, at(2 * limit + 31_999)));
        assert!(timer.escalate(true, at(2 * limit + 32_000)));
        assert_eq!(BUSY_LIMIT, Duration::from_secs(15));
    }

    /// One sighting of the app lock is not an app: a `status` probe holds
    /// it for an instant. Two sightings are.
    #[test]
    fn an_app_is_confirmed_by_a_second_probe() {
        let run = |answers: &[bool]| {
            let mut answers = answers.iter().copied();
            let mut calls = 0;
            let seen = confirmed(
                &mut || {
                    calls += 1;
                    answers.next().unwrap()
                },
                Duration::from_millis(1),
            );
            (seen, calls)
        };
        assert_eq!(run(&[false]), (false, 1));
        assert_eq!(run(&[true, false]), (false, 2));
        assert_eq!(run(&[true, true]), (true, 2));
    }

    /// The loop alone judges a stop: info while an app explains it, error
    /// otherwise. A failed open reports one line, not two.
    #[test]
    fn a_stop_is_info_while_an_app_explains_it_and_an_error_otherwise() {
        let failure = diagnostic(
            Severity::Error,
            "data service failed to open: IO Error: Could not set lock on file",
        );
        let stops = vec![("geode-data".to_string(), failure.message.clone())];
        let (level, lines) = stop_report(std::slice::from_ref(&failure), &stops, true);
        assert_eq!(level, Level::INFO);
        assert_eq!(lines, vec![failure.to_string()]);
        let (level, lines) = stop_report(std::slice::from_ref(&failure), &stops, false);
        assert_eq!(level, Level::ERROR);
        assert_eq!(lines.len(), 1);
        // A stop with no open failure behind it is named by its thread.
        let panicked = vec![("geode-ingest".to_string(), "boom".to_string())];
        let (level, lines) = stop_report(&[], &panicked, false);
        assert_eq!(level, Level::ERROR);
        assert_eq!(lines, vec!["geode-ingest stopped: boom".to_string()]);
    }

    /// The service's real open-failure diagnostic is held back by the sink
    /// for the loop to judge, with the stop; other diagnostics are not.
    #[test]
    fn the_sink_holds_back_the_real_open_failure_and_the_stop() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the store's directory should be: the open fails.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let db = blocker.join("g.duckdb");
        let config = geode_compose::load_config(None, (None, None));
        let setup = geode_compose::engine_setup(
            &config,
            db,
            geode_data::adapter::AdapterRegistry::default(),
        );
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let handle = DataService::spawn_as(
            setup.config,
            sink,
            StoreRole::Collector {
                memory_limit: Some("64MB".into()),
            },
        );
        let events = Events::new();
        let mut stopped = false;
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(10)) {
            let stop = matches!(event, DataEvent::ThreadStopped { .. });
            events.handle(event);
            if stop {
                stopped = true;
                break;
            }
        }
        handle.shutdown();
        assert!(stopped);
        assert!(events.stopped());
        let failures = events.take_open_failures();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(is_open_failure(&failures[0]), "{}", failures[0].message);
        let stops = events.take_stops();
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].1, failures[0].message);

        events.handle(DataEvent::Diagnostics(vec![diagnostic(
            Severity::Error,
            "a configuration error",
        )]));
        assert!(events.take_open_failures().is_empty());
    }

    #[test]
    fn health_is_logged_on_a_change_only() {
        let events = Events::new();
        assert!(events.health_changed("risk", &Health::Ok));
        assert!(!events.health_changed("risk", &Health::Ok));
        assert!(events.health_changed("other", &Health::Ok));
        let failed = Health::Failed {
            reason: "bad".into(),
        };
        assert!(events.health_changed("risk", &failed));
        assert!(!events.health_changed("risk", &failed));
        assert!(events.health_changed("risk", &Health::Ok));
    }

    /// A lease still held for an instant (a dropping lease shared with a
    /// spawning child, a status probe) is waited out; one held throughout
    /// is another collector.
    #[test]
    fn the_lease_is_retried_before_another_collector_is_assumed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.duckdb");
        let held = try_collector(&db).unwrap().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(held);
        });
        let lease = take_lease(&db, LEASE_RETRY).unwrap();
        release.join().unwrap();
        assert!(lease.is_some(), "a briefly held lease is taken");

        let start = Instant::now();
        assert!(
            take_lease(&db, Duration::from_millis(300))
                .unwrap()
                .is_none()
        );
        assert!(start.elapsed() >= Duration::from_millis(300));
    }
}
