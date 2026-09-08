//! Value types shared by both ends of the query path (spec §5.1, §2.7).
//!
//! The shell holds the frame's as-of and routes query outcomes to tiles;
//! the data layer produces them. Those two crates may never depend on
//! each other (CLAUDE.md), so what they exchange sits below both — the
//! same reason `Scope` lives here.

use crate::scope::Scope;
use crate::snapshot::Snapshot;
use chrono::{DateTime, Local, NaiveTime, TimeZone, Utc};
use std::sync::Arc;
use std::time::Instant;

/// Which point in time a query reads (foundation §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsOf {
    Live,
    At(DateTime<Utc>),
}

impl AsOf {
    pub fn is_live(&self) -> bool {
        matches!(self, AsOf::Live)
    }
}

/// The coalescing key for queries: one in-flight query per key, latest
/// wins (spec §2.4). A tile uses its tile id, so two tiles showing one
/// view never supersede each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueryKey(pub u64);

/// One query's result, addressed to the key that asked.
#[derive(Debug)]
pub struct QueryOutcome {
    pub key: QueryKey,
    /// Echoed from the request. The submitter keeps its own counter and
    /// drops an outcome whose tag is older than its latest submission, so
    /// "a stale result is never rendered" (§7.3) holds at both ends.
    pub tag: u64,
    /// `Err` is the failure text; the tile keeps its last good snapshot.
    pub snapshot: Result<Arc<Snapshot>, String>,
    /// When the submitter asked, for the §7.1 timing readout.
    pub submitted: Instant,
}

/// The picker's distinct-values request (spec §3.4): per value, how many
/// rows the frame's scope — with this column's own selection removed by
/// the caller — would leave, across every dataset that carries the
/// column.
///
/// `PartialEq` (not `Eq` — `Scope` itself stops at `PartialEq`, since a
/// scope's expression can carry a float literal) so `ShellEvent::
/// DistinctRequested(DistinctParams)` (`geode_shell::shell`) can still be
/// compared in a test's recorded-events `Vec` the way every other
/// `ShellEvent` variant already is.
#[derive(Debug, Clone, PartialEq)]
pub struct DistinctParams {
    pub key: QueryKey,
    pub tag: u64,
    pub column: String,
    /// The frame's scope with this column's own selection removed —
    /// the caller does the removal (spec §3.4).
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

/// The diagnostics tile's request: what the database holds (Phase 4b
/// §4.5). Built on the data service thread, from the `generations`
/// summary table, `file_generations`, and DuckDB's own introspection
/// functions — never a data-table scan (`geode_data::query::catalog::
/// build_catalog`'s doc comment says why).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogParams {
    pub key: QueryKey,
    pub tag: u64,
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
    pub datasets: Vec<DatasetCatalog>,
    /// `block_size * total_blocks` from `pragma_database_size()`.
    pub database_bytes: u64,
    /// `used_blocks` from the same.
    pub used_blocks: u64,
    /// `sum(memory_usage_bytes)` from `duckdb_memory()`.
    pub memory_bytes: u64,
    /// `current_setting('threads')`.
    pub threads: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DatasetCatalog {
    pub name: String,
    pub partitions: Vec<PartitionCatalog>,
    /// Summed `estimated_size` (spec: a row *estimate*, labelled
    /// "rows (est.)") over the dataset's live tables at every grain.
    pub live_rows: u64,
    /// The same, over the archive tables.
    pub archive_rows: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PartitionCatalog {
    pub batch: String,
    /// `None` is the bookless partition (spec §4.4) — a real partition,
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

/// `HH:MM` or `HH:MM:SS` means today at that time on the trader's LOCAL
/// clock (the modal's presets, its preview, the scope bar and the status
/// segment all display local time — one clock throughout, spec §3.6);
/// anything else must be RFC 3339. `now` stays UTC so callers and tests
/// keep their shape; it is converted to the local date internally. A
/// local time that does not exist or is ambiguous (a DST gap or overlap)
/// is an `Err` naming the time.
pub fn parse_as_of(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let today_local = now.with_timezone(&Local).date_naive();
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M") {
        return resolve_local(today_local, t, text);
    }
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M:%S") {
        return resolve_local(today_local, t, text);
    }
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| format!("'{text}' is not HH:MM or an RFC 3339 time"))
}

/// Resolve `date` + `time` as a LOCAL instant and map it to UTC. `text`
/// is only for the error message when the local time does not exist or
/// is ambiguous (a DST gap or overlap).
fn resolve_local(
    date: chrono::NaiveDate,
    time: NaiveTime,
    text: &str,
) -> Result<DateTime<Utc>, String> {
    Local
        .from_local_datetime(&date.and_time(time))
        .single()
        .map(|local| local.to_utc())
        .ok_or_else(|| format!("'{text}' does not name a valid local time (a DST gap or overlap)"))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The same instant [`parse_as_of`] should produce for `time` on
    /// `now`'s LOCAL date, computed independently of the parser under
    /// test so the assertion holds on any machine's zone.
    fn expect_local(now: DateTime<Utc>, time: NaiveTime) -> DateTime<Utc> {
        let today_local = now.with_timezone(&Local).date_naive();
        Local
            .from_local_datetime(&today_local.and_time(time))
            .unwrap()
            .to_utc()
    }

    #[test]
    fn as_of_accepts_a_clock_time_today_or_rfc3339() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 16, 0, 0).unwrap();
        assert_eq!(
            parse_as_of("14:05", now),
            Ok(expect_local(
                now,
                NaiveTime::from_hms_opt(14, 5, 0).unwrap()
            ))
        );
        assert_eq!(
            parse_as_of("2026-09-01T07:00:00Z", now),
            Ok(Utc.with_ymd_and_hms(2026, 9, 1, 7, 0, 0).unwrap())
        );
        assert!(parse_as_of("25:00", now).is_err());
        assert!(parse_as_of("yesterday", now).unwrap_err().contains("HH:MM"));
    }

    #[test]
    fn as_of_accepts_hh_mm_ss() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 16, 0, 0).unwrap();
        assert_eq!(
            parse_as_of("14:05:30", now),
            Ok(expect_local(
                now,
                NaiveTime::from_hms_opt(14, 5, 30).unwrap()
            ))
        );
    }

    /// Pins the behaviour F1 fixed: `HH:MM`/`HH:MM:SS` resolve on the
    /// LOCAL date, not UTC's — computed independently of the machine's
    /// zone so this test catches a regression to `.and_utc()` (the old
    /// UTC-resolving behaviour) on any machine, not just ones offset
    /// from UTC. Harness: mutate `resolve_local`'s callers back to
    /// `now.date_naive().and_time(t).and_utc()`.
    #[test]
    fn as_of_resolves_on_the_local_date_not_utcs() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 16, 0, 0).unwrap();
        let t = NaiveTime::from_hms_opt(14, 5, 0).unwrap();
        let today_local = now.with_timezone(&Local).date_naive();
        let expected = Local
            .from_local_datetime(&today_local.and_time(t))
            .unwrap()
            .to_utc();
        assert_eq!(parse_as_of("14:05", now), Ok(expected));
    }
}
