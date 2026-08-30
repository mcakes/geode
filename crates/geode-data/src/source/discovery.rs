//! Source discovery (spec §5.1, §5.2). Polls configured directory globs,
//! decides readiness, and skips what has not changed.
//!
//! Polling rather than filesystem watches: `notify` is unreliable over SMB
//! (spec §11), and polling degrades honestly where a watch fails silently.

use crate::source::sentinel::{Sentinel, parse_sentinel};
use crate::store::Catalog;
use crate::store::StoreError;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How a source decides a file is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// `<name>.done` exists and is at least as new as the CSV.
    Sentinel,
    /// No sentinel convention: require a stable (size, mtime) across N polls.
    StableMtime { polls: u32 },
}

/// Where a source sits in the cold-start ladder (spec §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Current risk on screen first.
    LatestRisk,
    /// Vol, instrument reference, scenario data.
    LatestOther,
    /// Older files not already in the database.
    Backfill,
}

#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub name: String,
    pub dataset: String,
    /// One or more directory globs (spec §5.1).
    pub paths: Vec<String>,
    pub readiness: Readiness,
    pub priority: Priority,
    pub poll_interval: Duration,
    pub pending_timeout: Duration,
    /// Regex with a named `batch` capture, applied to the file stem, that
    /// strips the date component so business dates share a partition
    /// (spec §4.3). Without one the whole stem is the batch.
    pub batch_pattern: Option<String>,
}

impl SourceSpec {
    pub fn batch_of(&self, csv: &Path) -> String {
        let stem = csv
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let Some(pattern) = &self.batch_pattern else {
            return stem;
        };
        let Ok(re) = regex::Regex::new(pattern) else {
            return stem;
        };
        re.captures(&stem)
            .and_then(|c| c.name("batch"))
            .map(|m| m.as_str().to_string())
            .unwrap_or(stem)
    }
}

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
        && prev.size == meta.len()
        && prev.source_time == sentinel.as_of
    {
        return Ok(CandidateState::Unchanged);
    }
    Ok(CandidateState::Ready(sentinel))
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
            books: vec!["BK000".into()],
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
            books: vec!["BK000".into()],
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
}
