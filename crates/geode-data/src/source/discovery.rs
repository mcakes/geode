//! Poll directory globs, classify readiness, and compare files with the catalog.
//! Polling avoids dependence on filesystem watches on network shares.
//!
//! Discovery reads metadata and sentinels, not CSV content. A pattern that
//! matches nothing has its literal prefix checked once: a missing, non-directory
//! or unreadable prefix, and an invalid pattern, are `PathProblem`s the scheduler
//! reports as `Degraded`. A readable empty directory is healthy. Glob traversal
//! errors below a readable prefix and CSV metadata errors are skipped;
//! catalog errors propagate. See `docs/current/data-path.md`.

use crate::source::sentinel::{Sentinel, parse_sentinel};
use crate::store::Catalog;
use crate::store::StoreError;
use crate::store::catalog::FileGeneration;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// Shared configuration types let the shell validate sources without depending
// on geode-data; discovery and ingest use the same values through this export.
pub use geode_core::source_config::{Priority, Readiness, SourceSpec};

#[derive(Debug, Clone)]
pub enum CandidateState {
    /// Sentinel is ready and catalog identity differs; CSV content is not validated.
    Ready(Sentinel),
    /// Sentinel metadata is unavailable or older than the CSV.
    Pending,
    /// Sentinel metadata is unavailable and CSV age exceeds the timeout.
    PendingTooLong,
    /// Already loaded at this size and source time.
    Unchanged,
    /// Unsupported readiness, or an unreadable/malformed sentinel after metadata
    /// was obtained.
    Orphaned { reason: String },
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub csv_path: PathBuf,
    pub sentinel_path: PathBuf,
    pub batch: String,
    pub size: u64,
    pub mtime: SystemTime,
    pub state: CandidateState,
}

/// A configured pattern this poll could not search: an invalid glob, or one
/// that matched nothing because its literal prefix is missing, not a
/// directory, or unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProblem {
    pub pattern: String,
    pub reason: String,
}

/// One poll's findings: the matched files, and the patterns it could not search.
#[derive(Debug, Default)]
pub struct Discovered {
    pub candidates: Vec<Candidate>,
    pub problems: Vec<PathProblem>,
}

/// The matched files only. Callers that report health use [`discover_all`].
pub fn discover(
    spec: &SourceSpec,
    catalog: &Catalog,
    now: SystemTime,
) -> Result<Vec<Candidate>, StoreError> {
    discover_all(spec, catalog, now).map(|d| d.candidates)
}

pub fn discover_all(
    spec: &SourceSpec,
    catalog: &Catalog,
    now: SystemTime,
) -> Result<Discovered, StoreError> {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for pattern in &spec.paths {
        let paths = match glob::glob(pattern) {
            Ok(paths) => paths,
            Err(e) => {
                problems.push(invalid_pattern(pattern, &e));
                continue;
            }
        };
        let mut matched = false;
        for csv_path in paths.flatten() {
            matched = true;
            let Ok(meta) = std::fs::metadata(&csv_path) else {
                continue;
            };
            let mtime = meta.modified().unwrap_or(now);
            let sentinel_path = sentinel_path_for(&csv_path);
            let batch = spec.batch_of(&csv_path);

            let state = classify(spec, catalog, &csv_path, &sentinel_path, &meta, mtime, now)?;
            out.push(Candidate {
                csv_path,
                sentinel_path,
                batch,
                size: meta.len(),
                mtime,
                state,
            });
        }
        if !matched && let Some(reason) = prefix_problem(pattern) {
            problems.push(PathProblem {
                pattern: pattern.clone(),
                reason,
            });
        }
    }
    out.sort_by(|a, b| a.csv_path.cmp(&b.csv_path));
    Ok(Discovered {
        candidates: out,
        problems,
    })
}

fn invalid_pattern(pattern: &str, e: &glob::PatternError) -> PathProblem {
    PathProblem {
        pattern: pattern.to_string(),
        reason: format!("invalid pattern '{pattern}': {e}"),
    }
}

/// The directory a pattern's matches must live under: the text before its
/// first glob character (`*`, `?`, `[`), cut back to the last separator. A
/// pattern with no glob character names one file, so its prefix is that
/// file's directory. No separator means the working directory, as glob
/// resolves a relative pattern; a cut at the root keeps the root. `~` is
/// not expanded.
pub(crate) fn literal_prefix(pattern: &str) -> PathBuf {
    let head = match pattern.find(['*', '?', '[']) {
        Some(i) => &pattern[..i],
        None => pattern,
    };
    match head.rfind(std::path::is_separator) {
        Some(0) => PathBuf::from(&head[..1]),
        Some(i) => PathBuf::from(&head[..i]),
        None => PathBuf::from("."),
    }
}

/// Why a pattern that matched nothing could never have matched: its literal
/// prefix is missing, not a directory, or unreadable. `None` for a readable
/// directory, because an empty drop directory is normal. Opens the directory
/// without listing it; `fs::metadata` alone succeeds on one the process
/// cannot read.
fn prefix_problem(pattern: &str) -> Option<String> {
    let prefix = literal_prefix(pattern);
    match std::fs::read_dir(&prefix) {
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Some(format!("path '{}' not found", prefix.display()))
        }
        Err(e) => Some(format!("path '{}' unreadable: {e}", prefix.display())),
    }
}

fn sentinel_path_for(csv: &Path) -> PathBuf {
    let mut name = csv
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".done");
    csv.with_file_name(name)
}

fn classify(
    spec: &SourceSpec,
    catalog: &Catalog,
    csv_path: &Path,
    sentinel_path: &Path,
    meta: &std::fs::Metadata,
    mtime: SystemTime,
    now: SystemTime,
) -> Result<CandidateState, StoreError> {
    if let Readiness::StableMtime { polls } = spec.readiness {
        // Stable-mtime needs history across polls, which discovery does not retain.
        // Report the unsupported strategy for this candidate instead of leaving it
        // pending indefinitely.
        return Ok(CandidateState::Orphaned {
            reason: format!(
                "source '{}' uses the stable-mtime readiness strategy \
                 ({polls} polls), which is not implemented; configure a \
                 sentinel convention or the source will never load",
                spec.name
            ),
        });
    }

    let Ok(sentinel_meta) = std::fs::metadata(sentinel_path) else {
        let waited = now.duration_since(mtime).unwrap_or_default();
        return Ok(if waited > spec.pending_timeout {
            CandidateState::PendingTooLong
        } else {
            CandidateState::Pending
        });
    };

    // An older sentinel does not establish completion of the current CSV.
    // This branch remains Pending regardless of pending_timeout.
    let sentinel_mtime = sentinel_meta.modified().unwrap_or(now);
    if sentinel_mtime < mtime {
        return Ok(CandidateState::Pending);
    }

    let text = match std::fs::read_to_string(sentinel_path) {
        Ok(t) => t,
        Err(e) => {
            return Ok(CandidateState::Orphaned {
                reason: e.to_string(),
            });
        }
    };
    let sentinel = match parse_sentinel(&text) {
        Ok(s) => s,
        Err(e) => {
            return Ok(CandidateState::Orphaned {
                reason: e.to_string(),
            });
        }
    };

    // Change detection: (size, source time) against what we last loaded.
    if let Some(prev) = catalog.lookup_by_path(csv_path)?
        && is_unchanged(&prev, meta.len(), sentinel.as_of)
    {
        return Ok(CandidateState::Unchanged);
    }
    Ok(CandidateState::Ready(sentinel))
}

/// Compare size and source time with the latest catalog generation for this
/// path. Shared by discovery and the runner's pre-load stale check.
///
/// CSV mtime, health, and content are not part of the comparison. A same-size
/// rewrite with unchanged source time is invisible to both checks. A failed
/// transaction rolls back its catalog row and cannot establish a new identity;
/// a committed degraded generation is still considered loaded.
pub(crate) fn is_unchanged(prev: &FileGeneration, size: u64, source_time: DateTime<Utc>) -> bool {
    prev.size == size && prev.source_time == source_time
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::time::{Duration, SystemTime};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn spec(root: &std::path::Path) -> SourceSpec {
        SourceSpec {
            pending_timeout: Duration::from_secs(3600),
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            ..SourceSpec::directory(
                "risk_files",
                "risk_snapshot",
                vec![format!("{}/*.csv", root.display())],
            )
        }
    }

    fn write(root: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = root.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    const SENTINEL: &str =
        r#"{"as_of":"2026-08-30T07:00:00Z","columns":["Book"],"books":["BK000"]}"#;

    fn store() -> (tempfile::TempDir, crate::store::Store) {
        let d = tempfile::tempdir().unwrap();
        let s = crate::store::Store::open(d.path().join("g.duckdb")).unwrap();
        crate::store::Catalog::new(s.writer())
            .ensure_tables()
            .unwrap();
        (d, s)
    }

    #[test]
    fn batch_strips_the_date_so_business_dates_share_a_partition() {
        let d = tempfile::tempdir().unwrap();
        let s = spec(d.path());
        assert_eq!(
            s.batch_of(std::path::Path::new("/x/risk_2026-08-30_BK000_part1.csv")),
            "BK000_part1"
        );
        assert_eq!(
            s.batch_of(std::path::Path::new("/x/risk_2026-08-29_BK000_part1.csv")),
            "BK000_part1",
            "two business dates must land in the same partition"
        );
    }

    #[test]
    fn batch_falls_back_to_the_whole_stem_without_a_pattern() {
        let d = tempfile::tempdir().unwrap();
        let mut s = spec(d.path());
        s.batch_pattern = None;
        assert_eq!(
            s.batch_of(std::path::Path::new("/x/anything.csv")),
            "anything"
        );
    }

    #[test]
    fn a_csv_with_its_sentinel_is_ready_and_carries_the_parsed_sentinel() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0].state, CandidateState::Ready(_)));
        assert_eq!(found[0].batch, "BK000");
    }

    #[test]
    fn a_csv_without_a_sentinel_is_pending_not_broken() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        assert!(matches!(found[0].state, CandidateState::Pending));
    }

    #[test]
    fn pending_past_the_timeout_becomes_pending_too_long() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let later = SystemTime::now() + Duration::from_secs(7200);
        let found = discover(&spec(d.path()), &cat, later).unwrap();
        assert!(matches!(found[0].state, CandidateState::PendingTooLong));
    }

    #[test]
    fn a_sentinel_older_than_its_csv_means_the_file_is_being_rewritten() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        std::thread::sleep(Duration::from_millis(20));
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        assert!(
            matches!(found[0].state, CandidateState::Pending),
            "{:?}",
            found[0].state
        );
    }

    #[test]
    fn an_unimplemented_readiness_strategy_says_so_instead_of_going_quiet() {
        // A stable-mtime candidate must report its unsupported strategy and source
        // name rather than remain pending without a diagnostic.
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);

        let mut s = spec(d.path());
        s.readiness = Readiness::StableMtime { polls: 3 };
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover(&s, &cat, SystemTime::now()).unwrap();

        assert_eq!(found.len(), 1);
        match &found[0].state {
            CandidateState::Orphaned { reason } => {
                assert!(
                    reason.contains("not implemented") && reason.contains("risk_files"),
                    "the diagnostic must name the source and say why: {reason}"
                );
            }
            other => panic!("a source that can never load must say so, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_sentinel_is_orphaned_with_the_reason() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", "{ not json");
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        match &found[0].state {
            CandidateState::Orphaned { reason } => assert!(reason.contains("JSON"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_already_loaded_unchanged_file_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let csv = write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let meta = std::fs::metadata(&csv).unwrap();
        cat.record(&crate::store::FileGeneration {
            file_id: 0,
            dataset: "risk_snapshot".into(),
            batch: "BK000".into(),
            path: csv.clone(),
            size: meta.len(),
            mtime: Utc::now(),
            source_time: ts("2026-08-30T07:00:00Z"),
            gen_id: 1,
            loaded_at: Utc::now(),
            row_count: 1,
            books: vec![Some("BK000".to_string())],
            archived_only: false,
            health: crate::health::Health::Ok,
        })
        .unwrap();

        let found = discover(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        assert!(matches!(found[0].state, CandidateState::Unchanged));
    }

    #[test]
    fn a_changed_file_is_ready_again() {
        let d = tempfile::tempdir().unwrap();
        let csv = write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        cat.record(&crate::store::FileGeneration {
            file_id: 0,
            dataset: "risk_snapshot".into(),
            batch: "BK000".into(),
            path: csv.clone(),
            size: 999_999, // different size => changed
            mtime: Utc::now(),
            source_time: ts("2026-08-30T07:00:00Z"),
            gen_id: 1,
            loaded_at: Utc::now(),
            row_count: 1,
            books: vec![Some("BK000".to_string())],
            archived_only: false,
            health: crate::health::Health::Ok,
        })
        .unwrap();
        let found = discover(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        assert!(matches!(found[0].state, CandidateState::Ready(_)));
    }

    #[test]
    fn multiple_globs_are_all_searched() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for d in [&a, &b] {
            write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
            write(d.path(), "risk_2026-08-30_BK000.csv.done", SENTINEL);
        }
        let mut s = spec(a.path());
        s.paths.push(format!("{}/*.csv", b.path().display()));
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        assert_eq!(discover(&s, &cat, SystemTime::now()).unwrap().len(), 2);
    }

    fn generation_at(size: u64, source_time: DateTime<Utc>) -> FileGeneration {
        FileGeneration {
            file_id: 0,
            dataset: "risk_snapshot".into(),
            batch: "BK000".into(),
            path: "/src/risk_2026-08-30_BK000.csv".into(),
            size,
            mtime: Utc::now(),
            source_time,
            gen_id: 1,
            loaded_at: Utc::now(),
            row_count: 1,
            books: vec![Some("BK000".to_string())],
            archived_only: false,
            health: crate::health::Health::Ok,
        }
    }

    #[test]
    fn is_unchanged_true_only_when_size_and_source_time_both_match() {
        // Shared between `classify`'s change detection and the runner's
        // pop-time re-check (both must apply exactly the same rule, or a
        // duplicate that slips past one sees a different answer from the
        // other).
        let prev = generation_at(10, ts("2026-08-30T07:00:00Z"));
        assert!(is_unchanged(&prev, 10, ts("2026-08-30T07:00:00Z")));
        assert!(
            !is_unchanged(&prev, 11, ts("2026-08-30T07:00:00Z")),
            "a different size is a change"
        );
        assert!(
            !is_unchanged(&prev, 10, ts("2026-08-30T08:00:00Z")),
            "a different source time is a change"
        );
    }

    #[test]
    fn literal_prefix_cuts_at_the_first_glob_character_and_back_to_a_separator() {
        use std::path::PathBuf;
        assert_eq!(
            literal_prefix("/mnt/risk/*.csv"),
            PathBuf::from("/mnt/risk")
        );
        assert_eq!(
            literal_prefix("/mnt/risk/2026-*/x.csv"),
            PathBuf::from("/mnt/risk")
        );
        assert_eq!(literal_prefix("/mnt/ri?k/x.csv"), PathBuf::from("/mnt"));
        assert_eq!(literal_prefix("/mnt/[ab]/x.csv"), PathBuf::from("/mnt"));
        assert_eq!(
            literal_prefix("//share/risk/**/*.csv"),
            PathBuf::from("//share/risk")
        );
        // No glob character: the pattern names one file, so its directory decides.
        assert_eq!(
            literal_prefix("/mnt/risk/eod.csv"),
            PathBuf::from("/mnt/risk")
        );
        // Relative patterns resolve against the working directory, as glob does.
        assert_eq!(literal_prefix("drops/*.csv"), PathBuf::from("drops"));
        assert_eq!(literal_prefix("*.csv"), PathBuf::from("."));
        assert_eq!(literal_prefix("/*.csv"), PathBuf::from("/"));
        // glob does not expand `~`; the prefix keeps it so the reason names it.
        assert_eq!(literal_prefix("~/risk/*.csv"), PathBuf::from("~/risk"));
    }

    fn one_pattern(pattern: String) -> SourceSpec {
        SourceSpec::directory("risk_files", "risk_snapshot", vec![pattern])
    }

    #[test]
    fn a_pattern_under_a_missing_directory_is_a_path_problem() {
        let d = tempfile::tempdir().unwrap();
        let gone = d.path().join("not-mounted");
        let s = one_pattern(format!("{}/*.csv", gone.display()));
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover_all(&s, &cat, SystemTime::now()).unwrap();
        assert!(found.candidates.is_empty());
        assert_eq!(
            found.problems,
            vec![PathProblem {
                pattern: s.paths[0].clone(),
                reason: format!("path '{}' not found", gone.display()),
            }]
        );
    }

    #[test]
    fn an_existing_empty_directory_is_healthy() {
        let d = tempfile::tempdir().unwrap();
        let s = one_pattern(format!("{}/*.csv", d.path().display()));
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover_all(&s, &cat, SystemTime::now()).unwrap();
        assert!(found.candidates.is_empty());
        assert!(found.problems.is_empty(), "{:?}", found.problems);
    }

    #[test]
    fn an_invalid_pattern_is_a_path_problem() {
        let d = tempfile::tempdir().unwrap();
        let s = one_pattern(format!("{}/[.csv", d.path().display()));
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover_all(&s, &cat, SystemTime::now()).unwrap();
        assert_eq!(found.problems.len(), 1);
        assert!(
            found.problems[0]
                .reason
                .starts_with(&format!("invalid pattern '{}': ", s.paths[0])),
            "{:?}",
            found.problems
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_is_a_path_problem() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // A privileged runner reads anything; there is nothing to prove there.
        let privileged = std::fs::read_dir(&locked).is_ok();
        let s = one_pattern(format!("{}/*.csv", locked.display()));
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover_all(&s, &cat, SystemTime::now()).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if privileged {
            return;
        }
        assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
        assert!(
            found.problems[0]
                .reason
                .starts_with(&format!("path '{}' unreadable: ", locked.display())),
            "{:?}",
            found.problems
        );
    }

    #[test]
    fn a_pattern_that_matches_is_not_checked_for_its_prefix() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover_all(&spec(d.path()), &cat, SystemTime::now()).unwrap();
        assert_eq!(found.candidates.len(), 1);
        assert!(found.problems.is_empty());
    }
    /// A pattern with no glob character matches through `fs::metadata`
    /// alone, so its file is found under a directory the process may search
    /// but not list (mode 0o311) — the one prefix a check would call
    /// unreadable. Matching wins: the check runs only when nothing matched.
    #[cfg(unix)]
    #[test]
    fn a_match_under_an_unlistable_prefix_is_not_a_path_problem() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let csv = write(&locked, "risk_2026-08-30_BK000.csv", "Book\nBK000\n");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o311)).unwrap();
        // A privileged runner lists anything; there is nothing to prove there.
        let privileged = std::fs::read_dir(&locked).is_ok();
        let s = one_pattern(csv.display().to_string());
        let (_sd, st) = store();
        let cat = crate::store::Catalog::new(st.writer());
        let found = discover_all(&s, &cat, SystemTime::now()).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if privileged {
            return;
        }
        assert_eq!(found.candidates.len(), 1, "fixture: the file matched");
        assert!(found.problems.is_empty(), "{:?}", found.problems);
    }
}
