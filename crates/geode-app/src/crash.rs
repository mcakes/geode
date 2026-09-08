//! Crash and log-file housekeeping (Phase 4b). Task 2 adds only
//! [`trim_log_files`], the pure startup trim for the daily rolling log
//! directory (`main.rs::install_logging`); the panic hook and crash-file
//! writer that give this module its name are a later task.

use std::path::{Path, PathBuf};

/// Deletes the oldest `geode.*.log` files in `dir` beyond `keep`, applied
/// once at startup (`tracing_appender::rolling::daily` itself never
/// prunes past files it didn't create this run). Oldest-first by file
/// name — `RollingFileAppender`'s daily names sort lexicographically by
/// date (`geode.YYYY-MM-DD.log`), so a plain sort is a correct age order
/// with no filesystem metadata read. A pure function of a directory
/// listing: never called with a `dir` that doesn't exist, but a missing
/// or unreadable directory is simply a no-op, not a panic (this runs
/// ahead of the subscriber being fully up).
pub fn trim_log_files(dir: &Path, keep: usize) {
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
            name.starts_with("geode.") && name.ends_with(".log")
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
        trim_log_files(dir.path(), 7);
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
        trim_log_files(dir.path(), 7);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn trim_ignores_files_that_are_not_named_like_a_geode_log() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=9 {
            touch(dir.path(), &format!("geode.2026-09-{day:02}.log"));
        }
        touch(dir.path(), "other.txt");
        trim_log_files(dir.path(), 7);
        assert!(dir.path().join("other.txt").exists(), "not ours to delete");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 8); // 7 kept + other.txt
    }

    #[test]
    fn trim_on_a_missing_directory_is_a_no_op_not_a_panic() {
        trim_log_files(Path::new("/does/not/exist"), 7);
    }
}
