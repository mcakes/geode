//! Logging setup shared by the app and the background collector.
//!
//! [`install`] sets up process-wide tracing: a reloadable level filter, the
//! stderr and in-memory ring layers, and a daily file under `<user>/logs`
//! named by the caller's prefix (`geode` for the app, `collector` for the
//! background collector). Both processes share the logs directory, so each
//! trims only its own prefix ([`trim_log_files`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use geode_core::log::{LevelControl, LogLevels, Ring, RingLayer};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Registry, fmt, reload};

/// What [`install`] returns: the ring the diagnostics page and panic reports
/// read, the runtime level control, and the file writer's guard.
pub struct Logging {
    pub ring: Arc<Ring>,
    pub control: Arc<dyn LevelControl>,
    /// Keep for the process lifetime and drop before an explicit exit;
    /// dropping it stops the background file writer. `None` when the file
    /// sink could not be set up.
    pub guard: Option<WorkerGuard>,
}

/// Install process-wide tracing with a reloadable level filter, a synchronous
/// stderr layer when `stderr` is true, an in-memory ring layer, and an
/// optional background file writer. Default levels apply until configuration
/// is loaded. The ring retains 4,096 records. File setup failure leaves the
/// stderr (if any) and ring logging available.
///
/// A process whose stderr a service manager captures to an unpruned file
/// (the collector under launchd) passes `false`, so that file holds only
/// panics and failures from before logging started, not every record twice.
///
/// Daily files live under `<user>/logs/<prefix>.YYYY-MM-DD.log`; their date
/// and rotation use UTC, independently of the configured display clock.
/// Startup trims files with this prefix to seven before opening the current
/// log; another prefix's files in the same directory are left alone.
/// Rotation does not prune files during the run.
///
/// Retain the returned [`Logging::guard`] for the process lifetime and drop
/// it before explicit process exits. Dropping it stops the file writer;
/// subsequent file-bound records are lost. Daily logs can lag behind the
/// synchronous ring that supplies panic reports.
pub fn install(prefix: &str, stderr: bool) -> Logging {
    let ring = Arc::new(Ring::new(4096));
    let (filter, reload_handle) = reload::Layer::new(LogLevels::default().to_targets());

    let (_, user) = crate::config_dirs();
    let mut log_guard = None;
    let file_layer = user.as_ref().and_then(|dir| {
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs).ok()?;
        // The seven-file cap, applied once at startup — `tracing_appender`
        // rotates going forward but never prunes files from before this
        // run.
        trim_log_files(&logs, prefix, 7);
        let appender = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(prefix)
            .filename_suffix("log")
            .build(&logs)
            .ok()?;
        // File writes run on a dedicated thread. The panic hook reads the
        // synchronous ring, so its report does not wait for this buffer to flush.
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        log_guard = Some(guard);
        Some(fmt::layer().with_writer(non_blocking).with_ansi(false))
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr.then(|| fmt::layer().with_writer(std::io::stderr)))
        .with(RingLayer::new(ring.clone()))
        .with(file_layer)
        .init();

    struct ReloadControl(reload::Handle<tracing_subscriber::filter::Targets, Registry>);
    impl LevelControl for ReloadControl {
        fn set(&self, levels: &LogLevels) -> Result<(), String> {
            self.0
                .reload(levels.to_targets())
                .map_err(|e| e.to_string())
        }
    }

    Logging {
        ring,
        control: Arc::new(ReloadControl(reload_handle)),
        guard: log_guard,
    }
}

/// Best-effort deletion of matching `<prefix>*<suffix>` paths beyond `keep`.
/// The retained paths are the last `keep` names in lexicographic order; embedded
/// UTC timestamps put later dates after earlier ones without metadata reads.
///
/// Used for daily logs and crash reports. Missing or unreadable directories are
/// ignored, as are unreadable entries and individual deletion failures. Names
/// are matched by prefix and suffix only; their timestamp fields are not parsed.
pub fn prune_files(dir: &Path, prefix: &str, suffix: &str, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            name.starts_with(prefix) && name.ends_with(suffix)
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    for old in &files[..files.len() - keep] {
        let _ = std::fs::remove_file(old);
    }
}

/// Prunes `<prefix>.*.log` files to the last `keep` matching names at startup.
/// Daily rotation creates new files but this startup cap is not reapplied during
/// the run. Missing directories and deletion failures are ignored; see
/// [`prune_files`] for matching and ordering.
pub fn trim_log_files(dir: &Path, prefix: &str, keep: usize) {
    prune_files(dir, &format!("{prefix}."), ".log", keep);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    #[test]
    fn trim_deletes_the_oldest_files_beyond_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=9 {
            touch(dir.path(), &format!("geode.2026-09-{day:02}.log"));
        }
        trim_log_files(dir.path(), "geode", 7);
        let mut remaining: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                "geode.2026-09-03.log",
                "geode.2026-09-04.log",
                "geode.2026-09-05.log",
                "geode.2026-09-06.log",
                "geode.2026-09-07.log",
                "geode.2026-09-08.log",
                "geode.2026-09-09.log",
            ]
        );
    }

    #[test]
    fn trim_is_a_no_op_under_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "geode.2026-09-01.log");
        trim_log_files(dir.path(), "geode", 7);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn trim_ignores_files_that_are_not_named_like_a_geode_log() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=9 {
            touch(dir.path(), &format!("geode.2026-09-{day:02}.log"));
        }
        touch(dir.path(), "other.txt");
        trim_log_files(dir.path(), "geode", 7);
        assert!(dir.path().join("other.txt").exists(), "not ours to delete");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 8); // 7 kept + other.txt
    }

    #[test]
    fn trim_on_a_missing_directory_is_a_no_op_not_a_panic() {
        trim_log_files(Path::new("/does/not/exist"), "geode", 7);
    }

    /// The app and the collector share one logs directory; each trims only
    /// its own prefix, so one process's startup cap never deletes the
    /// other's files.
    #[test]
    fn trim_keeps_to_its_own_prefix_in_a_shared_directory() {
        for (mine, theirs) in [("collector", "geode"), ("geode", "collector")] {
            let dir = tempfile::tempdir().unwrap();
            for day in 1..=9 {
                touch(dir.path(), &format!("{mine}.2026-09-{day:02}.log"));
                touch(dir.path(), &format!("{theirs}.2026-09-{day:02}.log"));
            }
            trim_log_files(dir.path(), mine, 7);
            let count = |prefix: &str| {
                std::fs::read_dir(dir.path())
                    .unwrap()
                    .filter(|e| {
                        e.as_ref()
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(&format!("{prefix}."))
                    })
                    .count()
            };
            assert_eq!(count(mine), 7, "{mine}: trimmed to the cap");
            assert_eq!(count(theirs), 9, "{theirs}: not {mine}'s to delete");
        }
    }
}
