//! Value types shared by both ends of the query path (spec §5.1, §2.7).
//!
//! The shell holds the frame's as-of and routes query outcomes to tiles;
//! the data layer produces them. Those two crates may never depend on
//! each other (CLAUDE.md), so what they exchange sits below both — the
//! same reason `Scope` lives here.

use crate::scope::Scope;
use crate::snapshot::Snapshot;
use chrono::{DateTime, NaiveTime, Utc};
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

/// `HH:MM` or `HH:MM:SS` means today at that time (UTC, the data's
/// clock); anything else must be RFC 3339.
pub fn parse_as_of(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M") {
        return Ok(now.date_naive().and_time(t).and_utc());
    }
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M:%S") {
        return Ok(now.date_naive().and_time(t).and_utc());
    }
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| format!("'{text}' is not HH:MM or an RFC 3339 time"))
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

    #[test]
    fn as_of_accepts_a_clock_time_today_or_rfc3339() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 16, 0, 0).unwrap();
        assert_eq!(
            parse_as_of("14:05", now),
            Ok(Utc.with_ymd_and_hms(2026, 9, 3, 14, 5, 0).unwrap())
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
            Ok(Utc.with_ymd_and_hms(2026, 9, 3, 14, 5, 30).unwrap())
        );
    }
}
