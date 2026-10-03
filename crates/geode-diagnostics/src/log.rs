//! The retained log tail and its page-side filter. Pure: no GPUI, no clock.

use std::collections::VecDeque;
use std::sync::Arc;

use geode_core::log::{Level, Record, Ring};

pub const LOG_CAP: usize = 4_096;

/// The levels in [`LogFilter::levels`] order, most severe first.
pub const LEVELS: [Level; 5] = [
    Level::ERROR,
    Level::WARN,
    Level::INFO,
    Level::DEBUG,
    Level::TRACE,
];

/// A bounded copy of the ring from the sequence at creation. `drain` reports
/// the gap the ring overwrote since the last drain, never a lifetime total.
pub struct LogTail {
    ring: Arc<Ring>,
    since: u64,
    lost: u64,
    buf: Vec<Record>,
    records: VecDeque<Record>,
}

impl LogTail {
    pub fn new(ring: Arc<Ring>) -> LogTail {
        let since = ring.latest_seq();
        LogTail {
            ring,
            since,
            lost: 0,
            buf: Vec::new(),
            records: VecDeque::new(),
        }
    }

    /// Pull new records. Returns whether anything arrived.
    pub fn drain(&mut self) -> bool {
        let latest = self.ring.latest_seq();
        if latest <= self.since {
            return false;
        }
        self.lost = self
            .ring
            .oldest_seq()
            .map(|oldest| oldest.saturating_sub(self.since + 1))
            .unwrap_or(0);
        self.ring.drain_since(self.since, &mut self.buf);
        self.since = latest;
        self.records.extend(self.buf.drain(..));
        while self.records.len() > LOG_CAP {
            self.records.pop_front();
        }
        true
    }

    pub fn has_new(&self) -> bool {
        self.ring.latest_seq() > self.since
    }

    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.iter()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Records overwritten before the last drain reached them.
    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// Forget retained records; the next drain continues from where it was.
    pub fn clear(&mut self) {
        self.records.clear();
        self.lost = 0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    /// Indexed by [`LogFilter::level_index`]; all true by default.
    pub levels: [bool; 5],
    pub target: Option<String>,
    /// The fuzzy text filter. The page narrows the cached rows by it
    /// ([`crate::log_cache::Narrowed`]); [`LogFilter::accepts`] never sees it.
    pub text: String,
}

impl LogFilter {
    pub fn all() -> LogFilter {
        LogFilter {
            levels: [true; 5],
            target: None,
            text: String::new(),
        }
    }

    pub fn level_index(level: Level) -> usize {
        LEVELS.iter().position(|l| *l == level).unwrap_or(2)
    }

    /// Whether the level toggles and the exact target select pass `r`.
    /// The text filter is applied after, where the time is formatted.
    pub fn accepts(&self, r: &Record) -> bool {
        self.gates(r.level, r.target)
    }

    /// [`LogFilter::accepts`] over a record's level and target alone.
    pub fn gates(&self, level: Level, target: &str) -> bool {
        self.levels[Self::level_index(level)] && self.target.as_deref().is_none_or(|t| t == target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn rec(level: Level, target: &'static str, message: &str) -> Record {
        Record {
            at: SystemTime::now(),
            level,
            target,
            message: message.to_string(),
            seq: 0,
        }
    }

    #[test]
    fn a_tail_starts_at_the_current_sequence_and_caps_at_log_cap() {
        let ring = Arc::new(Ring::new(8_192));
        ring.push(rec(Level::INFO, "geode::shell", "before"));
        let mut tail = LogTail::new(ring.clone());
        assert!(!tail.drain(), "nothing since creation");
        for i in 0..(LOG_CAP + 10) {
            ring.push(rec(Level::INFO, "geode::shell", &format!("m{i}")));
        }
        assert!(tail.drain());
        assert_eq!(tail.len(), LOG_CAP);
        assert_eq!(tail.records().next().unwrap().message, "m10");
        assert_eq!(tail.lost(), 0);
    }

    #[test]
    fn a_wrap_between_drains_reports_the_gap_measured_at_that_drain() {
        let ring = Arc::new(Ring::new(4));
        let mut tail = LogTail::new(ring.clone());
        for i in 0..10 {
            ring.push(rec(Level::WARN, "geode::ingest", &format!("w{i}")));
        }
        tail.drain();
        // seq 1..=10 pushed, capacity 4 keeps 7..=10: 6 lost since `since` (0).
        assert_eq!(tail.lost(), 6);
        ring.push(rec(Level::WARN, "geode::ingest", "w10"));
        tail.drain();
        assert_eq!(tail.lost(), 0, "not cumulative");
    }

    #[test]
    fn the_filter_gates_on_level_and_target() {
        let mut f = LogFilter::all();
        let r = rec(Level::DEBUG, "geode::query", "planned 3 tables");
        assert!(f.accepts(&r));
        f.levels[LogFilter::level_index(Level::DEBUG)] = false;
        assert!(!f.accepts(&r));
        f.levels[LogFilter::level_index(Level::DEBUG)] = true;
        f.target = Some("geode::shell".into());
        assert!(!f.accepts(&r));
        f.target = Some("geode::query".into());
        assert!(f.accepts(&r));
        f.text = "absent".into();
        assert!(f.accepts(&r), "text narrows later, over the cached rows");
    }
}
