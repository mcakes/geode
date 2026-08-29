//! Config hot reload (Task 1c-1, spec §8): a background poll of the desk
//! and user config directories' `*.toml` mtimes, and the "keep last-good"
//! decision applied when a reload is attempted.
//!
//! This module is deliberately split into pure, `cx`-free pieces —
//! [`scan`]/[`Snapshot::changed_since`] and [`decide`] — so the actual
//! decision logic is unit-testable with tempdirs, with no window or gpui
//! executor involved. `ShellView` (in `shell/mod.rs`) is the only thing that
//! touches gpui: it owns the watched dirs, drives the ~500ms poll via
//! `cx.spawn` + a background timer, and calls [`decide`] after loading a
//! fresh `Config` off the UI thread.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use geode_core::config::{Config, ConfigSources, LayerDoc, Severity};

use crate::defaults::BUILTIN_KEYMAP;

/// Filename excluded from every [`scan`] snapshot. A later task writes
/// `session.toml` into the user config dir from inside the running app;
/// if it were included here, the app's own write would perturb the
/// snapshot and the very next poll would see a "change" and reload itself
/// in a loop. Pre-flight ruling: exclude it now, before anything writes it.
pub const EXCLUDED_FILENAME: &str = "session.toml";

/// A snapshot of every `*.toml` file's mtime across the watched desk and
/// user config directories (`session.toml` excluded — see
/// [`EXCLUDED_FILENAME`]). Pure data: comparing two snapshots
/// ([`changed_since`](Self::changed_since)) is the entire "did anything on
/// disk change" question, with no I/O of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    entries: BTreeMap<PathBuf, SystemTime>,
}

impl Snapshot {
    /// True if `self` differs from `previous` in any way a config reload
    /// should react to: a file added, removed, or modified (mtime changed).
    /// Equivalent to plain inequality since both are captured the same way,
    /// but named for the call site's intent.
    pub fn changed_since(&self, previous: &Snapshot) -> bool {
        self != previous
    }
}

/// Scan every `*.toml` file (non-recursive, `session.toml` excluded) in
/// `desk` and `user` — whichever are `Some` — and capture its mtime. Missing
/// directories and unreadable files are silently skipped, mirroring
/// `geode_core::config::load_layer`'s tolerance for absent layers: a config
/// directory that doesn't exist yet is not an error, just an empty layer.
pub fn scan(desk: Option<&Path>, user: Option<&Path>) -> Snapshot {
    let mut entries = BTreeMap::new();
    for dir in [desk, user].into_iter().flatten() {
        scan_dir(dir, &mut entries);
    }
    Snapshot { entries }
}

fn scan_dir(dir: &Path, entries: &mut BTreeMap<PathBuf, SystemTime>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        let is_toml = path.extension().is_some_and(|ext| ext == "toml");
        let is_excluded = path
            .file_name()
            .is_some_and(|name| name == EXCLUDED_FILENAME);
        if !is_toml || is_excluded {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(mtime) = metadata.modified() else {
            continue;
        };
        entries.insert(path, mtime);
    }
}

/// Load a fresh `Config` from `desk` and `user` the same way `geode-app`'s
/// `main.rs` builds the initial one (builtin keymap doc + the two config
/// directories). Pulled out here so both the app's startup path and the
/// reload watcher build a `Config` the same way — this is plain
/// composition of already-tested pieces (`LayerDoc::builtin`,
/// `Config::load`), not new logic, so it has no tests of its own.
pub fn load_config(desk: Option<PathBuf>, user: Option<PathBuf>) -> Config {
    let builtin_keymap =
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).expect("builtin keymap TOML is well-formed");
    Config::load(&ConfigSources {
        builtin: vec![builtin_keymap],
        desk,
        user,
    })
}

/// The result of one reload attempt, and what the status bar shows for it.
/// `Unchanged` is the starting state before any reload has ever run (and is
/// never produced by [`decide`] — the watcher only calls `decide` after
/// `Snapshot::changed_since` has already said something on disk moved).
#[derive(Debug, Clone, PartialEq)]
pub enum ReloadOutcome {
    /// The freshly loaded config had no error diagnostics and was applied.
    /// `warnings` carries every warning-severity diagnostic message (config
    /// load warnings, keymap build warnings, theme warnings), for the
    /// optional non-noisy status bar marker (brief allows skipping it).
    Applied { warnings: Vec<String> },
    /// The freshly loaded config had at least one error diagnostic; the
    /// entire previous `Config` (and everything built from it) was kept
    /// untouched. `errors` carries every error-severity diagnostic message.
    KeptLastGood { errors: Vec<String> },
    /// No reload has been attempted yet (or the last poll saw no change).
    Unchanged,
}

impl ReloadOutcome {
    /// The status bar's reload indicator text for this outcome: `None` when
    /// config is healthy (brief: "nothing when healthy"; a brief `reloaded`
    /// marker for `Applied` is explicitly optional and skipped here as
    /// noisy), `Some` danger-toned message when the last reload attempt
    /// kept the previous config because the new one had errors.
    pub fn status_message(&self) -> Option<String> {
        match self {
            ReloadOutcome::KeptLastGood { errors } => Some(format!(
                "config: {} error(s) — keeping last good",
                errors.len()
            )),
            ReloadOutcome::Applied { .. } | ReloadOutcome::Unchanged => None,
        }
    }
}

/// Decide whether `new_config` should be applied or discarded in favor of
/// the last-good config, purely from its own diagnostics (plan constraint:
/// "any error diagnostic ⇒ keep last-good entire Config"). Any
/// error-severity diagnostic — regardless of how many, or whether warnings
/// are also present — means [`ReloadOutcome::KeptLastGood`]; zero errors
/// (warnings allowed) means [`ReloadOutcome::Applied`].
///
/// Callers that also need to fold in diagnostics from a later step (e.g.
/// keymap building, which needs the config's own layered docs and so can
/// only run after `Config::load`) should extend `new_config.diagnostics`
/// with those before calling `decide`, so a keymap error is treated exactly
/// like a config error — one "did the reload attempt produce any error"
/// question, not two.
pub fn decide(new_config: &Config) -> ReloadOutcome {
    let errors: Vec<String> = new_config
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.clone())
        .collect();
    if !errors.is_empty() {
        return ReloadOutcome::KeptLastGood { errors };
    }
    let warnings: Vec<String> = new_config
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| d.message.clone())
        .collect();
    ReloadOutcome::Applied { warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, Diagnostic, Layer};
    use std::time::Duration;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    // --- Snapshot / scan / changed_since -----------------------------

    #[test]
    fn two_scans_of_an_untouched_directory_are_equal() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.toml", "config_version = 1\n");
        let a = scan(Some(dir.path()), None);
        let b = scan(Some(dir.path()), None);
        assert!(!b.changed_since(&a));
    }

    #[test]
    fn adding_a_file_is_a_change() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.toml", "config_version = 1\n");
        let before = scan(Some(dir.path()), None);
        write(dir.path(), "keymap.toml", "config_version = 1\n");
        let after = scan(Some(dir.path()), None);
        assert!(after.changed_since(&before));
    }

    #[test]
    fn removing_a_file_is_a_change() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.toml", "config_version = 1\n");
        write(dir.path(), "keymap.toml", "config_version = 1\n");
        let before = scan(Some(dir.path()), None);
        std::fs::remove_file(dir.path().join("keymap.toml")).unwrap();
        let after = scan(Some(dir.path()), None);
        assert!(after.changed_since(&before));
    }

    #[test]
    fn modifying_a_files_mtime_is_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        write(dir.path(), "app.toml", "config_version = 1\n");
        let before = scan(Some(dir.path()), None);

        // Bump the mtime forward explicitly rather than relying on real
        // wall-clock time to have advanced enough between two fs writes in
        // the same test (some filesystems have coarse mtime resolution).
        let bumped = std::fs::metadata(&path).unwrap().modified().unwrap() + Duration::from_secs(5);
        let file = std::fs::File::open(&path).unwrap();
        file.set_modified(bumped).unwrap();

        let after = scan(Some(dir.path()), None);
        assert!(after.changed_since(&before));
    }

    #[test]
    fn session_toml_is_excluded_from_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let before = scan(Some(dir.path()), None);
        write(dir.path(), "session.toml", "workspace = 1\n");
        let after = scan(Some(dir.path()), None);
        assert!(
            !after.changed_since(&before),
            "writing session.toml must not look like a config change — a later \
             task writes this file from inside the app, and it must never \
             trigger a self-reload"
        );
    }

    #[test]
    fn non_toml_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let before = scan(Some(dir.path()), None);
        write(dir.path(), "notes.txt", "hello\n");
        let after = scan(Some(dir.path()), None);
        assert!(!after.changed_since(&before));
    }

    #[test]
    fn both_desk_and_user_dirs_are_scanned() {
        let desk = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write(desk.path(), "app.toml", "config_version = 1\n");
        let before = scan(Some(desk.path()), Some(user.path()));
        write(user.path(), "keymap.toml", "config_version = 1\n");
        let after = scan(Some(desk.path()), Some(user.path()));
        assert!(after.changed_since(&before));
    }

    #[test]
    fn missing_directories_and_absent_dirs_produce_an_empty_unchanging_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let a = scan(Some(&missing), None);
        let b = scan(None, None);
        assert!(!a.changed_since(&b));
    }

    // --- decide ---------------------------------------------------------

    fn config_with(diagnostics: Vec<Diagnostic>) -> Config {
        let mut config = Config::load(&ConfigSources::default());
        config.diagnostics = diagnostics;
        config
    }

    #[test]
    fn no_diagnostics_applies_with_no_warnings() {
        let config = config_with(vec![]);
        assert_eq!(decide(&config), ReloadOutcome::Applied { warnings: vec![] });
    }

    #[test]
    fn warnings_only_still_applies_and_carries_the_warnings() {
        let config = config_with(vec![Diagnostic::warning(
            Layer::User,
            PathBuf::from("app.toml"),
            "missing config_version",
        )]);
        assert_eq!(
            decide(&config),
            ReloadOutcome::Applied {
                warnings: vec!["missing config_version".to_string()]
            }
        );
    }

    #[test]
    fn any_error_keeps_last_good_even_alongside_warnings() {
        let config = config_with(vec![
            Diagnostic::warning(Layer::User, PathBuf::from("app.toml"), "a warning"),
            Diagnostic::error(Layer::Desk, PathBuf::from("keymap.toml"), "parse error"),
        ]);
        assert_eq!(
            decide(&config),
            ReloadOutcome::KeptLastGood {
                errors: vec!["parse error".to_string()]
            }
        );
    }

    // --- ReloadOutcome::status_message -----------------------------

    #[test]
    fn applied_and_unchanged_have_no_status_message() {
        assert_eq!(
            ReloadOutcome::Applied { warnings: vec![] }.status_message(),
            None
        );
        assert_eq!(
            ReloadOutcome::Applied {
                warnings: vec!["a warning".to_string()]
            }
            .status_message(),
            None
        );
        assert_eq!(ReloadOutcome::Unchanged.status_message(), None);
    }

    #[test]
    fn kept_last_good_reports_the_error_count() {
        let outcome = ReloadOutcome::KeptLastGood {
            errors: vec!["bad a".to_string(), "bad b".to_string()],
        };
        assert_eq!(
            outcome.status_message(),
            Some("config: 2 error(s) — keeping last good".to_string())
        );
    }

    #[test]
    fn multiple_errors_are_all_collected() {
        let config = config_with(vec![
            Diagnostic::error(Layer::Desk, PathBuf::from("a.toml"), "bad a"),
            Diagnostic::error(Layer::User, PathBuf::from("b.toml"), "bad b"),
        ]);
        assert_eq!(
            decide(&config),
            ReloadOutcome::KeptLastGood {
                errors: vec!["bad a".to_string(), "bad b".to_string()]
            }
        );
    }
}
