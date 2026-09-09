//! Source discovery (spec §5.1, §5.2). Polls configured directory globs,
//! decides readiness, and skips what has not changed.
//!
//! Polling rather than filesystem watches: `notify` is unreliable over SMB
//! (spec §11), and polling degrades honestly where a watch fails silently.

use crate::source::sentinel::{Sentinel, parse_sentinel};
use crate::store::Catalog;
use crate::store::StoreError;
use crate::store::catalog::FileGeneration;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// `SourceSpec` (and the `Readiness`/`Priority` types it carries) now lives
// in geode-core so geode-shell can validate a sources doc without
// depending on this crate (Phase 4c §2.2). Discovery and ingest keep using
// it unchanged through this re-export.
pub use geode_core::source_config::{Priority, Readiness, SourceSpec};

#[derive(Debug, Clone)]
pub enum CandidateState {
    /// Complete and not yet loaded.
    Ready(Sentinel),
    /// Waiting on its sentinel. Expected, not broken.
    Pending,
    /// Waiting past the source's timeout.
    PendingTooLong,
    /// Already loaded at this size and source time.
    Unchanged,
    /// Present but unusable — a malformed or unreadable sentinel.
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

pub fn discover(
    spec: &SourceSpec,
    catalog: &Catalog,
    now: SystemTime,
) -> Result<Vec<Candidate>, StoreError> {
    let mut out = Vec::new();
    for pattern in &spec.paths {
        let Ok(paths) = glob::glob(pattern) else {
            continue;
        };
        for csv_path in paths.flatten() {
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
    }
    out.sort_by(|a, b| a.csv_path.cmp(&b.csv_path));
    Ok(out)
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
        // Not implemented: the fallback needs poll history nothing keeps
        // yet. Reporting `Pending` would make such a source ingest nothing,
        // forever, with no diagnostic — silence is the one failure mode
        // spec §5.7 forbids. Surface it instead.
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

    // A sentinel older than its CSV means the file is being rewritten.
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

/// True when `prev` — the catalog's latest recorded generation for a path —
/// already reflects this file's current (size, source time): the same
/// file, not reloaded since. Shared between `classify`'s own change
/// detection and the ingest runner's pop-time re-check
/// (`ingest::runner::run`), so a duplicate queued item is harmless even if
/// one slips past `IngestHandle::submit`'s dedupe — both call sites apply
/// exactly the same rule.
///
/// `Health` is not part of the rule: a generation recorded for a load that
/// then failed (e.g. in `attribute_conflicts`, after the catalog write)
/// still counts as "loaded" here, at both call sites, exactly as it always
/// has for `classify` — this extraction does not change that, only
/// applies it a second place.
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
            name: "risk_files".into(),
            dataset: "risk_snapshot".into(),
            paths: vec![format!("{}/*.csv", root.display())],
            readiness: Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: Duration::from_secs(30),
            pending_timeout: Duration::from_secs(3600),
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
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
            "two business dates must land in the same partition (spec §4.3)"
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
        // The stable-mtime strategy needs poll history nothing keeps yet.
        // Reporting `Pending` would make such a source ingest nothing,
        // forever, with no diagnostic — and a source that silently loads
        // no files is the one failure mode §5.7 forbids, because there is
        // nothing on screen to notice. The branch existed and nothing
        // tested it.
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
}
