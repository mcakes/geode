//! The Log section's rows, prepared once per record and kept across
//! rebuilds, and the fuzzy narrowing over them. Pure: no GPUI, no clock
//! reads.
//!
//! A rebuild formats only records it has not seen; a keystroke never
//! reformats the tail. Narrowing a query is the expensive step (a DP per
//! word per row), so the page runs [`Narrowed::run`] off the UI thread and
//! keeps the result: level and target changes reuse it, and records that
//! arrive under an unchanged query are narrowed on their own and appended
//! ([`Narrowed::extend`]).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use geode_core::log::Record;
use geode_shell::listfilter::{ColumnMarks, Narrow, lower};
use gpui::SharedString;

use crate::log::LogFilter;
use crate::prepared::{self, PreparedRow, PreparedTable};

/// The searchable columns, in [`prepared::LOG_COLUMNS`] order: time, level,
/// target, message.
const COLUMNS: usize = 4;

/// One record, formatted and lowered once.
#[derive(Debug)]
pub struct LogEntry {
    pub seq: u64,
    pub level: geode_core::log::Level,
    pub target: &'static str,
    texts: [SharedString; COLUMNS],
    lowered: [Box<[char]>; COLUMNS],
    /// The row as painted without a filter; a narrowed rebuild clones it
    /// and adds the marks.
    row: PreparedRow,
}

fn hms_millis(t: SystemTime, clock: Clock) -> String {
    let dt = DateTime::<Utc>::from(t);
    format!("{}.{:03}", clock.hms(dt), dt.timestamp_subsec_millis())
}

impl LogEntry {
    fn new(r: &Record, clock: Clock) -> LogEntry {
        let hms = SharedString::from(hms_millis(r.at, clock));
        let message = SharedString::from(r.message.clone());
        let texts = [
            hms.clone(),
            SharedString::new_static(r.level.as_str()),
            SharedString::new_static(r.target),
            message.clone(),
        ];
        let lowered = std::array::from_fn(|c| lower(&texts[c]).into_boxed_slice());
        LogEntry {
            seq: r.seq,
            level: r.level,
            target: r.target,
            row: prepared::log_row(r.seq, r.level, r.target, hms, message),
            texts,
            lowered,
        }
    }

    fn narrow(&self, narrow: &mut Narrow) -> Option<ColumnMarks> {
        let columns: [(&str, &[char]); COLUMNS] =
            std::array::from_fn(|c| (self.texts[c].as_ref(), &*self.lowered[c]));
        narrow.row_lowered(&columns)
    }
}

/// The tail's records as [`LogEntry`]s, in sequence order.
#[derive(Debug, Default)]
pub struct LogCache {
    clock: Option<Clock>,
    entries: VecDeque<Arc<LogEntry>>,
}

impl LogCache {
    /// Follow the tail: drop entries the tail no longer holds (it pops
    /// from the front and can be cleared) and format only records newer
    /// than the last entry. A clock change reformats everything, since
    /// the time column depends on it.
    pub fn sync<'a>(&mut self, records: impl IntoIterator<Item = &'a Record>, clock: Clock) {
        if self.clock != Some(clock) {
            self.entries.clear();
            self.clock = Some(clock);
        }
        let mut records = records.into_iter().peekable();
        match records.peek() {
            None => self.entries.clear(),
            Some(first) => {
                let first = first.seq;
                while self.entries.front().is_some_and(|e| e.seq < first) {
                    self.entries.pop_front();
                }
            }
        }
        let last = self.entries.back().map(|e| e.seq);
        for r in records {
            if last.is_none_or(|last| r.seq > last) {
                self.entries.push_back(Arc::new(LogEntry::new(r, clock)));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The oldest entry's sequence.
    pub fn first_seq(&self) -> Option<u64> {
        self.entries.front().map(|e| e.seq)
    }

    /// The newest entry's sequence.
    pub fn last_seq(&self) -> Option<u64> {
        self.entries.back().map(|e| e.seq)
    }

    /// Shared handles to the entries newer than `through` (all of them for
    /// `None`), for narrowing off the UI thread.
    pub fn after(&self, through: Option<u64>) -> Vec<Arc<LogEntry>> {
        self.entries
            .iter()
            .filter(|e| through.is_none_or(|t| e.seq > t))
            .cloned()
            .collect()
    }
}

/// One query's narrowing over the entries up to `through`: which of them
/// it keeps and where it marks them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Narrowed {
    query: String,
    through: Option<u64>,
    kept: HashMap<u64, ColumnMarks>,
}

impl Narrowed {
    /// Narrow `entries` (in sequence order) by `query`. Pure and `Send`:
    /// the page runs it on the background executor.
    pub fn run(query: &str, entries: &[Arc<LogEntry>]) -> Narrowed {
        let mut narrow = Narrow::new(query);
        let kept = entries
            .iter()
            .filter_map(|e| Some((e.seq, e.narrow(&mut narrow)?)))
            .collect();
        Narrowed {
            query: query.to_string(),
            through: entries.last().map(|e| e.seq),
            kept,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The newest sequence this narrowing has answered for.
    pub fn through(&self) -> Option<u64> {
        self.through
    }

    /// Take in `more`, the same query's narrowing of the entries after
    /// `self.through`. A narrowing of another query or another stretch is
    /// refused: merging it would answer records it never looked at.
    pub fn extend(&mut self, more: Narrowed) -> bool {
        if more.query != self.query || more.through <= self.through {
            return false;
        }
        self.through = more.through;
        self.kept.extend(more.kept);
        true
    }

    /// Forget entries older than `first`, which the tail has dropped.
    pub fn prune(&mut self, first: u64) {
        self.kept.retain(|seq, _| *seq >= first);
    }

    /// Whether `seq` is answered, and if so its marks when kept.
    fn answer(&self, seq: u64) -> Option<Option<&ColumnMarks>> {
        self.through
            .is_some_and(|t| seq <= t)
            .then(|| self.kept.get(&seq))
    }
}

/// The Log table: the loss notice, then every cached entry the level and
/// target gates pass. Without `narrowed`, every such entry shows unmarked.
/// With it, an entry shows only when the narrowing answered for it and
/// kept it, with its marks; entries newer than the narrowing wait for the
/// next pass rather than showing unfiltered.
pub fn log_table(
    cache: &LogCache,
    filter: &LogFilter,
    narrowed: Option<&Narrowed>,
    lost: u64,
) -> PreparedTable {
    let mut rows = Vec::with_capacity(cache.len() + 1);
    if lost > 0 {
        rows.push(prepared::log_notice(lost));
    }
    for e in &cache.entries {
        if !filter.gates(e.level, e.target) {
            continue;
        }
        let marks = match narrowed.map(|n| n.answer(e.seq)) {
            None => None,
            Some(Some(Some(marks))) => Some(marks),
            Some(_) => continue,
        };
        let mut row = e.row.clone();
        if let Some(marks) = marks {
            for (c, cell) in row.cells.iter_mut().enumerate() {
                cell.marks = marks.get(c).to_vec();
            }
        }
        rows.push(row);
    }
    PreparedTable {
        columns: prepared::LOG_COLUMNS.to_vec(),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::log::Level;
    use std::time::Duration;

    fn record(level: Level, target: &'static str, message: &str, seq: u64) -> Record {
        Record {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(9 * 3600),
            level,
            target,
            message: message.to_string(),
            seq,
        }
    }

    fn fixture() -> Vec<Record> {
        vec![
            record(Level::INFO, "geode::ingest", "partition loaded", 1),
            record(Level::WARN, "geode::shell", "slow paint", 2),
            record(Level::ERROR, "geode::query", "planned 3 tables", 3),
        ]
    }

    fn cache_of(records: &[Record]) -> LogCache {
        let mut cache = LogCache::default();
        cache.sync(records, Clock::utc());
        cache
    }

    fn table(records: &[Record], filter: &LogFilter) -> PreparedTable {
        let cache = cache_of(records);
        let narrowed = (!filter.text.trim().is_empty())
            .then(|| Narrowed::run(&filter.text, &cache.after(None)));
        log_table(&cache, filter, narrowed.as_ref(), 0)
    }

    fn keys(t: &PreparedTable) -> Vec<&str> {
        t.rows.iter().map(|r| r.key.as_str()).collect()
    }

    fn marked(cell: &prepared::Cell) -> Vec<&str> {
        cell.marks.iter().map(|r| &cell.text[r.clone()]).collect()
    }

    /// The Log's text filter narrows fuzzily over every visible column,
    /// time and level included, and marks what it matched; the level
    /// toggles and target select still gate.
    #[test]
    fn the_log_narrows_fuzzily_over_every_column_after_level_and_target() {
        let records = fixture();
        let mut f = LogFilter::all();
        for (query, expected) in [
            ("ptn ld", vec!["1"]),
            ("INGST", vec!["1"]),
            ("warn", vec!["2"]),
            ("09:00", vec!["1", "2", "3"]),
            ("shell paint", vec!["2"]),
            ("ingest paint", vec![]),
        ] {
            f.text = query.into();
            assert_eq!(keys(&table(&records, &f)), expected, "{query:?}");
        }
        f.text = "ingest ptn".into();
        let t = table(&records, &f);
        let cells = &t.rows[0].cells;
        assert!(cells[0].marks.is_empty() && cells[1].marks.is_empty());
        assert_eq!(marked(&cells[2]), ["ingest"]);
        assert_eq!(marked(&cells[3]), ["p", "t", "n"]);
        f.text = "err".into();
        assert_eq!(marked(&table(&records, &f).rows[0].cells[1]), ["ERR"]);
        f.levels[LogFilter::level_index(Level::ERROR)] = false;
        assert!(table(&records, &f).rows.is_empty(), "a level gates");
        f.text.clear();
        f.target = Some("geode::shell".into());
        assert_eq!(keys(&table(&records, &f)), ["2"], "the target gates");
    }

    /// A word never joins the end of one column to the start of the next:
    /// `ll` ends the target "geode::shell" and `sl` starts "slow paint".
    #[test]
    fn a_log_word_is_never_stitched_across_columns() {
        let records = fixture();
        let mut f = LogFilter::all();
        f.text = "llsl".into();
        assert!(table(&records, &f).rows.is_empty());
        f.text = "ll sl".into();
        assert_eq!(keys(&table(&records, &f)), ["2"], "as two words it fits");
    }

    #[test]
    fn the_table_leads_with_a_loss_notice_and_details_each_record() {
        let records = vec![record(Level::ERROR, "geode::shell", "boom", 1)];
        let cache = cache_of(&records);
        let t = log_table(&cache, &LogFilter::all(), None, 7);
        assert!(matches!(t.rows[0].kind, prepared::RowKind::Notice));
        assert!(t.rows[0].cells[0].text.contains("7 records lost"));
        assert_eq!(t.rows[1].tone, crate::model::Tone::Error);
        assert_eq!(
            t.rows[1].detail,
            vec![SharedString::from("09:00:00.000 ERROR geode::shell boom")]
        );
        assert_eq!(log_table(&cache, &LogFilter::all(), None, 0).rows.len(), 1);
    }

    /// The cache formats each record once, follows the tail's front, and
    /// empties with it.
    #[test]
    fn the_cache_formats_only_new_records_and_follows_the_tail() {
        let records = fixture();
        let mut cache = cache_of(&records[..2]);
        let first = Arc::clone(&cache.entries[0]);
        cache.sync(&records[1..], Clock::utc());
        assert_eq!(
            cache.first_seq(),
            Some(2),
            "the front dropped with the tail"
        );
        assert_eq!(cache.last_seq(), Some(3));
        assert!(
            !Arc::ptr_eq(&first, &cache.entries[0]),
            "a dropped entry is gone"
        );
        let kept = Arc::clone(&cache.entries[0]);
        cache.sync(&records[1..], Clock::utc());
        assert!(Arc::ptr_eq(&kept, &cache.entries[0]), "not reformatted");
        cache.sync(&records[1..], Clock::in_zone_named("Europe/London"));
        assert!(
            !Arc::ptr_eq(&kept, &cache.entries[0]),
            "a clock change reformats"
        );
        cache.sync(std::iter::empty(), Clock::utc());
        assert!(cache.is_empty(), "a cleared tail empties the cache");
    }

    /// Records arriving under an unchanged query are narrowed on their own
    /// and appended; until then they do not show, filtered or not. A
    /// narrowing of another query or an old stretch is refused.
    #[test]
    fn new_records_are_narrowed_alone_and_appended() {
        let records = fixture();
        let mut cache = cache_of(&records[..2]);
        let mut f = LogFilter::all();
        f.text = "pa".into();
        let mut narrowed = Narrowed::run("pa", &cache.after(None));
        assert_eq!(narrowed.through(), Some(2));
        cache.sync(&records, Clock::utc());
        assert_eq!(
            keys(&log_table(&cache, &f, Some(&narrowed), 0)),
            ["1", "2"],
            "record 3 waits for its pass"
        );
        let pending = cache.after(narrowed.through());
        assert_eq!(pending.len(), 1);
        assert!(
            !narrowed.extend(Narrowed::run("pb", &pending)),
            "another query"
        );
        assert!(narrowed.extend(Narrowed::run("pa", &pending)));
        assert!(
            !narrowed.extend(Narrowed::run("pa", &pending)),
            "already answered"
        );
        assert_eq!(narrowed.through(), Some(3));
        assert_eq!(
            narrowed,
            Narrowed::run("pa", &cache.after(None)),
            "appending equals narrowing the whole tail"
        );
        narrowed.prune(3);
        assert!(narrowed.kept.keys().all(|seq| *seq >= 3));
    }
}
