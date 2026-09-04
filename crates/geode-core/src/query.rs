//! Value types shared by both ends of the query path (spec §5.1, §2.7).
//!
//! The shell holds the frame's as-of and routes query outcomes to tiles;
//! the data layer produces them. Those two crates may never depend on
//! each other (CLAUDE.md), so what they exchange sits below both — the
//! same reason `Scope` lives here.

use crate::snapshot::Snapshot;
use chrono::{DateTime, Utc};
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
}
