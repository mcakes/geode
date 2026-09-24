//! Filesystem snapshots and the decision to apply a configuration reload.
//!
//! [`scan`] reads directory entries and modification times; snapshot comparison
//! and [`decide`] are I/O-free. The shell watcher scans and loads on the
//! background executor, then validates and applies the candidate on the UI
//! thread.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use geode_core::config::{Config, ConfigSources, LayerDoc, Severity};

/// Exclude session saves from change detection: writing layout or transient
/// state must not trigger a layered configuration reload.
pub const EXCLUDED_FILENAME: &str = "session.toml";

/// Paths and modification times for watched TOML files, excluding
/// [`EXCLUDED_FILENAME`]. Equality detects additions, removals, and changed
/// mtimes; content changes with an unchanged mtime are invisible.
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

/// Capture mtimes of `*.toml` files directly inside `desk` and `user`, excluding
/// `session.toml`. Unreadable directories, directory entries, metadata, and
/// modification times are silently skipped. A skipped file can therefore appear
/// removed relative to the previous snapshot.
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

/// Merge the process's original builtin documents with freshly read desk and
/// user files. The caller supplies the complete builtin layer, including any
/// generated demo documents, because it cannot be recovered from disk.
/// The shell watcher runs this filesystem work on the background executor.
pub fn load_config(builtin: Vec<LayerDoc>, desk: Option<PathBuf>, user: Option<PathBuf>) -> Config {
    Config::load(&ConfigSources {
        builtin,
        desk,
        user,
    })
}

/// Decision for a reload attempt. `Unchanged` is the initial state and is never
/// returned by [`decide`]; polls without a change retain the previous outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum ReloadOutcome {
    /// The candidate has no error diagnostics at the decision boundary.
    /// `warnings` contains warning messages present when [`decide`] ran;
    /// diagnostics from later runtime readers are not included.
    Applied { warnings: Vec<String> },
    /// The candidate has errors, so retain the active config and runtime
    /// settings. The shell still updates reload status and diagnostics.
    KeptLastGood { errors: Vec<String> },
    /// No reload has been attempted yet.
    Unchanged,
}

impl ReloadOutcome {
    /// Show an error count for a rejected attempt; otherwise show no marker.
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

/// Reject a candidate if `new_config.diagnostics` contains any errors;
/// otherwise return `Applied` with its warning messages. This function neither
/// validates typed documents nor mutates runtime state. Callers must append all
/// diagnostics that should block application before calling it.
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
    // A builtin keymap fixture; production reload receives the app's full layer.
    use crate::defaults::BUILTIN_KEYMAP;
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
            "writing session.toml must not trigger a layered config reload"
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

    // --- load_config ----------------------------------------------------

    /// Reload retains all supplied builtin documents, including generated demo
    /// views, datasets, and sources that have no backing files.
    #[test]
    fn a_reload_keeps_every_builtin_doc_not_just_the_keymap() {
        let user = tempfile::tempdir().unwrap();
        let builtin = vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("views", "[risk]\ncolumns = [\"delta\"]\n").unwrap(),
        ];

        let config = load_config(builtin, None, Some(user.path().to_path_buf()));

        assert!(
            config.doc("views").is_some(),
            "a reload dropped the app's builtin `views` doc — the builtin \
             layer must be reused, not rebuilt"
        );
        assert!(
            config.doc("keymap").is_some(),
            "the builtin keymap must survive the reload too"
        );
    }

    /// Reload merges user files over the retained builtin layer.
    #[test]
    fn a_user_doc_still_overrides_the_builtin_layer_after_a_reload() {
        let user = tempfile::tempdir().unwrap();
        write(
            user.path(),
            "app.toml",
            "config_version = 1\n[theme]\nname = \"user-choice\"\n",
        );
        let builtin = vec![
            LayerDoc::builtin("app", "[theme]\nname = \"builtin-choice\"\n").unwrap(),
            LayerDoc::builtin("views", "[risk]\ncolumns = [\"delta\"]\n").unwrap(),
        ];

        let config = load_config(builtin, None, Some(user.path().to_path_buf()));

        assert_eq!(
            config.get("app", "theme.name").and_then(|v| v.as_str()),
            Some("user-choice"),
            "the user layer must still win over the builtin one"
        );
        assert!(
            config.doc("views").is_some(),
            "the builtin-only doc must survive alongside the overridden one"
        );
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
