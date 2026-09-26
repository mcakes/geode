//! Requests and outcomes shared by the shell, feature modules, and data layer.
//! The shell owns frame context and routes answers; the data layer executes
//! requests. Shared types keep those crates independent of each other.

use crate::scope::Scope;
use crate::snapshot::Snapshot;
use chrono::{DateTime, NaiveTime, Utc};
use std::sync::Arc;
use std::time::Instant;

/// Which point in time a query reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AsOf {
    #[default]
    Live,
    At(DateTime<Utc>),
}

impl AsOf {
    pub fn is_live(&self) -> bool {
        matches!(self, AsOf::Live)
    }
}

/// Identity used to coalesce pending queries and route outcomes. New requests
/// supersede older pending requests for the same key; running work may still
/// answer, so callers also check tags. Tiles use their IDs to avoid superseding
/// another tile's request for the same view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueryKey(pub u64);

/// One query's result, addressed to the key that asked.
#[derive(Debug)]
pub struct QueryOutcome {
    pub key: QueryKey,
    /// Echoed from the request. The submitter compares it with its current tag
    /// to reject results from superseded submissions.
    pub tag: u64,
    /// `Err` is the failure text; the tile keeps its last good snapshot.
    pub snapshot: Result<Arc<Snapshot>, String>,
    /// Submission time used to measure request latency.
    pub submitted: Instant,
}

/// Request value counts across datasets carrying `column`, under the frame's
/// scope with that column's selection removed by the caller. Expressions and
/// other selections still constrain the counts.
///
/// `PartialEq` supports request comparisons; `Scope` contains floating-point
/// expression literals and therefore does not implement `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub struct DistinctParams {
    pub key: QueryKey,
    pub tag: u64,
    pub column: String,
    /// The frame's scope with this column's own selection removed —
    /// the caller does the removal.
    pub scope: Scope,
    pub as_of: AsOf,
}

/// The picker's distinct-values result, addressed to the key that asked.
#[derive(Debug)]
pub struct DistinctOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub column: String,
    /// Sorted by value. `Err` is the failure text.
    pub values: Result<Vec<(String, u64)>, String>,
}

/// Request stored datasets, generations, and resource metrics for diagnostics.
/// The data service reads summary tables and database introspection functions,
/// avoiding scans of the stored payload tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogParams {
    pub key: QueryKey,
    pub tag: u64,
    pub as_of: AsOf,
}

/// Request one document by its full key, live or at a historical time.
/// Document reads have no view, grouping, or scope. The data layer's document
/// compiler selects the row set identified by the dataset and key tuple.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub dataset: String,
    /// In the dataset's declared `key` order (`DatasetSpec::key`) — the
    /// same order `geode_core::document::join_key` expects, since a
    /// document's storage identity (`batch`) is exactly that join.
    pub document_key: Vec<String>,
    pub as_of: AsOf,
}

/// The catalog request's result, addressed to the key that asked.
#[derive(Debug)]
pub struct CatalogOutcome {
    pub key: QueryKey,
    pub tag: u64,
    /// `Err` is the failure text.
    pub snapshot: Result<CatalogSnapshot, String>,
}

/// What the database holds, as of the moment it was read.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CatalogSnapshot {
    /// The time context used to resolve `resolved_gen`. Consumers must compare
    /// it with the current frame before showing historical-generation markers:
    /// a fresh frame context can precede its matching catalog answer.
    pub as_of: AsOf,
    pub datasets: Vec<DatasetCatalog>,
    /// `sum(block_size * total_blocks)` from `pragma_database_size()`.
    /// Reports checkpointed database blocks, excluding the current write-ahead log.
    /// Zero can mean an empty database or writes not yet checkpointed.
    pub database_bytes: u64,
    /// `sum(used_blocks)` from the same.
    pub used_blocks: u64,
    /// `max(block_size)` from the same — the unit `used_blocks` (and
    /// `total_blocks`, folded into `database_bytes` already) are
    /// counted in. Exposed separately so a caller can render
    /// `used_blocks * block_size` without repeating the query, and so
    /// `total_blocks == 0` (derivable as `database_bytes == 0` while
    /// `block_size > 0`) is available as an explicit "not yet
    /// checkpointed" signal distinct from "the type is unknown".
    pub block_size: u64,
    /// `sum(memory_usage_bytes)` from `duckdb_memory()`.
    pub memory_bytes: u64,
    /// `current_setting('threads')`.
    pub threads: u64,
    /// Each fetch source that answered a catalogue, with its identities
    /// sorted, for the picker's typeahead. Empty
    /// for a source that cannot enumerate, and for a build with no fetch
    /// source at all.
    pub identities: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DatasetCatalog {
    pub name: String,
    pub partitions: Vec<PartitionCatalog>,
    /// Estimated row count summed over the dataset's live tables at every grain.
    /// This is an estimate from database introspection, not an exact count.
    pub live_rows: u64,
    /// The same, over the archive tables.
    pub archive_rows: u64,
    /// Series family only: one row per `(identity, source)` pair from the coverage
    /// table, without scanning series values. Empty for other dataset families.
    pub series: Vec<SeriesCatalog>,
}

/// Fetch coverage for one source/identity pair: the hull of recorded spans,
/// their count, and their newest receive time. The hull can contain gaps;
/// it does not promise a stored point at every timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeriesCatalog {
    pub source: String,
    pub identity: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub fetches: u64,
    pub latest_received_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PartitionCatalog {
    pub batch: String,
    /// `None` is the bookless partition — a real partition,
    /// not a missing one.
    pub book: Option<String>,
    pub generations: Vec<GenerationInfo>,
    /// The generation `resolve_generations` names for this partition
    /// under the request's `as_of`. `None` under `AsOf::Live` — nothing
    /// is resolved, live is live.
    pub resolved_gen: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationInfo {
    pub gen_id: i64,
    pub source_time: DateTime<Utc>,
    /// From `file_generations`. `None` when the generation predates that
    /// table's row (an older database) or was reconstructed rather than
    /// loaded through the ordinary path.
    pub loaded_at: Option<DateTime<Utc>>,
    pub file_rows: Option<u64>,
    /// The newest generation of this partition, per the same
    /// `(source_time, gen_id)` tie-break `resolve_generations` uses.
    pub live: bool,
}

/// The last microsecond of a day — what a bare `YYYY-MM-DD` as-of means:
/// "the newest generation of that day". The last microsecond, not the
/// last whole second: a subscribed document's `source_time` is
/// `Utc::now()` at receive (sub-second), and as-of compares
/// `source_time <= ?` at microsecond precision, so `23:59:59` would
/// exclude a document published at `23:59:59.4` — every *displayed*
/// time in the app truncates to whole seconds, so nothing visible
/// changes by carrying the extra precision here.
pub const END_OF_DAY: NaiveTime = match NaiveTime::from_hms_micro_opt(23, 59, 59, 999_999) {
    Some(t) => t,
    None => unreachable!(),
};

/// `HH:MM` or `HH:MM:SS` means today at that time on the trader's
/// configured clock (`clock`). `YYYY-MM-DD` means the end of that day
/// ([`END_OF_DAY`]);
/// `YYYY-MM-DD HH:MM` or `YYYY-MM-DD HH:MM:SS` means that local instant;
/// anything else must be RFC 3339. `now` stays UTC; the date-carrying
/// forms never consult it. A local time that does not exist or is
/// ambiguous (a DST gap or overlap) is an `Err` naming the time.
pub fn parse_as_of(
    text: &str,
    now: DateTime<Utc>,
    clock: &crate::clock::Clock,
) -> Result<DateTime<Utc>, String> {
    let today = clock.today(now);
    let resolve = |date: chrono::NaiveDate, time: NaiveTime| {
        clock.resolve_local(date, time).map_err(|e| e.to_string())
    };
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M") {
        return resolve(today, t);
    }
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M:%S") {
        return resolve(today, t);
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return resolve(d, END_OF_DAY);
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M") {
        return resolve(dt.date(), dt.time());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S") {
        return resolve(dt.date(), dt.time());
    }
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| format!("'{text}' is not HH:MM, YYYY-MM-DD[ HH:MM[:SS]] or an RFC 3339 time"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn live_is_live_and_an_instant_is_not() {
        assert!(AsOf::Live.is_live());
        let t = DateTime::parse_from_rfc3339("2026-09-03T14:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!AsOf::At(t).is_live());
    }

    #[test]
    fn keys_compare_and_hash_by_value() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(QueryKey(3));
        set.insert(QueryKey(3));
        set.insert(QueryKey(4));
        assert_eq!(set.len(), 2);
        assert!(QueryKey(3) < QueryKey(4));
    }

    fn ny() -> crate::clock::Clock {
        crate::clock::Clock::in_zone(chrono_tz::America::New_York)
    }

    /// The instant `parse_as_of` should produce for `date` + `time` in
    /// New York, computed independently of the parser.
    fn expect_ny(date: chrono::NaiveDate, time: NaiveTime) -> DateTime<Utc> {
        chrono_tz::America::New_York
            .from_local_datetime(&date.and_time(time))
            .single()
            .expect("test picks a time that exists")
            .to_utc()
    }

    #[test]
    fn a_bare_date_resolves_to_the_end_of_that_local_day() {
        let now = DateTime::parse_from_rfc3339("2026-09-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        assert_eq!(
            parse_as_of("2026-09-08", now, &ny()).unwrap(),
            expect_ny(date, END_OF_DAY)
        );
        assert_eq!(
            END_OF_DAY,
            NaiveTime::from_hms_micro_opt(23, 59, 59, 999_999).unwrap()
        );
    }

    #[test]
    fn a_date_with_a_time_resolves_to_that_local_instant() {
        let now = DateTime::parse_from_rfc3339("2026-09-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        assert_eq!(
            parse_as_of("2026-09-08 14:05", now, &ny()).unwrap(),
            expect_ny(date, NaiveTime::from_hms_opt(14, 5, 0).unwrap())
        );
        assert_eq!(
            parse_as_of("2026-09-08 14:05:30", now, &ny()).unwrap(),
            expect_ny(date, NaiveTime::from_hms_opt(14, 5, 30).unwrap())
        );
    }

    #[test]
    fn a_date_form_ignores_today_entirely() {
        // `now` is on a different day; the parsed instant must not depend on it.
        let a = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let b = DateTime::parse_from_rfc3339("2026-12-31T23:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            parse_as_of("2026-09-08 09:30", a, &ny()),
            parse_as_of("2026-09-08 09:30", b, &ny())
        );
    }

    #[test]
    fn an_impossible_date_or_a_slashed_one_is_an_error_naming_the_forms() {
        let now = Utc::now();
        let err = parse_as_of("2026-02-30", now, &ny()).unwrap_err();
        assert!(err.contains("YYYY-MM-DD"), "{err}");
        let err = parse_as_of("2026/09/08", now, &ny()).unwrap_err();
        assert!(err.contains("YYYY-MM-DD"), "{err}");
        assert!(err.contains("HH:MM"), "{err}");
    }

    #[test]
    fn the_existing_forms_still_parse() {
        let now = DateTime::parse_from_rfc3339("2026-09-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(parse_as_of("14:05", now, &ny()).is_ok());
        assert!(parse_as_of("14:05:30", now, &ny()).is_ok());
        assert!(parse_as_of("2026-09-08T14:05:00Z", now, &ny()).is_ok());
        assert!(parse_as_of("2026-09-08T14:05:00+01:00", now, &ny()).is_ok());
    }

    #[test]
    fn as_of_accepts_a_clock_time_today_or_rfc3339() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 16, 0, 0).unwrap();
        assert_eq!(
            parse_as_of("14:05", now, &ny()),
            Ok(expect_ny(
                ny().today(now),
                NaiveTime::from_hms_opt(14, 5, 0).unwrap()
            ))
        );
        assert_eq!(
            parse_as_of("2026-09-01T07:00:00Z", now, &ny()),
            Ok(Utc.with_ymd_and_hms(2026, 9, 1, 7, 0, 0).unwrap())
        );
        assert!(parse_as_of("25:00", now, &ny()).is_err());
        assert!(
            parse_as_of("yesterday", now, &ny())
                .unwrap_err()
                .contains("HH:MM")
        );
    }

    #[test]
    fn as_of_accepts_hh_mm_ss() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 16, 0, 0).unwrap();
        assert_eq!(
            parse_as_of("14:05:30", now, &ny()),
            Ok(expect_ny(
                ny().today(now),
                NaiveTime::from_hms_opt(14, 5, 30).unwrap()
            ))
        );
    }

    /// Time-only input resolves on the configured clock's date. The expected
    /// instant is computed independently at a time when UTC and New York dates differ.
    #[test]
    fn as_of_resolves_on_the_clocks_date_not_utcs() {
        use chrono::TimeZone;
        // 01:00 UTC on the 4th is 21:00 on the 3rd in New York: "14:05"
        // must mean the 3rd, not the 4th.
        let now = Utc.with_ymd_and_hms(2026, 9, 4, 1, 0, 0).unwrap();
        let t = NaiveTime::from_hms_opt(14, 5, 0).unwrap();
        assert_eq!(
            parse_as_of("14:05", now, &ny()),
            Ok(expect_ny(
                chrono::NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(),
                t
            ))
        );
    }
}
