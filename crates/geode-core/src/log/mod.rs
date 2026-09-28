//! Bounded in-process log storage, tracing integration, and runtime level control.
//! The diagnostics page reads the ring; `[log]` configures Geode target levels.
use crate::config::{Config, Diagnostic, Severity};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
pub use tracing::Level;
use tracing_subscriber::filter::Targets;

/// Geode tracing targets configurable by their suffix in `[log]`, such as
/// `ingest` for `geode::ingest`.
pub const TARGETS: [&str; 7] = [
    "geode::ingest",
    "geode::query",
    "geode::config",
    "geode::session",
    "geode::shell",
    "geode::theme",
    "geode::pricing",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub at: SystemTime,
    pub level: Level,
    pub target: &'static str,
    pub message: String,
    pub seq: u64,
}

struct RingInner {
    records: Box<[Option<Record>]>,
    head: usize,
    seq: u64,
}

pub struct Ring {
    inner: Mutex<RingInner>,
    /// Test-only count of slots examined by `drain_since`. Checks scan cost
    /// separately from returned values: a full-ring scan could produce the same
    /// records while wasting work on every tail read.
    #[cfg(test)]
    drain_visits: std::sync::atomic::AtomicU64,
}

impl Ring {
    pub fn new(capacity: usize) -> Ring {
        let capacity = capacity.max(1);
        Ring {
            inner: Mutex::new(RingInner {
                records: (0..capacity).map(|_| None).collect(),
                head: 0,
                seq: 0,
            }),
            #[cfg(test)]
            drain_visits: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .len()
    }

    /// Insert a record, overwriting the oldest slot when full. Assign contiguous
    /// sequence numbers under the mutex so concurrent writers cannot duplicate or
    /// skip a number. Readers also hold this mutex while cloning records.
    pub fn push(&self, mut r: Record) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.seq += 1;
        r.seq = g.seq;
        let cap = g.records.len();
        let head = g.head;
        g.records[head] = Some(r);
        g.head = (head + 1) % cap;
    }

    pub fn latest_seq(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).seq
    }

    /// The oldest retained sequence number, or `None` before the first write.
    /// A reader whose last consumed sequence is `since` has lost
    /// `oldest.saturating_sub(since.saturating_add(1))` unread records.
    ///
    /// After wrapping, `head` points to the oldest retained slot; before wrapping,
    /// the first slot holds the oldest record.
    pub fn oldest_seq(&self) -> Option<u64> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &g.records[g.head] {
            Some(r) => Some(r.seq),
            None => g.records[0].as_ref().map(|r| r.seq),
        }
    }

    /// Copy retained records with `seq > since` into `out`, oldest first, clearing
    /// its previous contents. Reusing `out` avoids repeated buffer allocation;
    /// cloning each record's message can still allocate while the mutex is held.
    /// Formatting happens before insertion, outside this lock.
    ///
    /// Walk backward from the newest record and stop at the first empty slot or
    /// sequence at or before `since`, then reverse the output. A reader following
    /// the tail examines only new records plus the stopping slot, up to ring capacity.
    pub fn drain_since(&self, since: u64, out: &mut Vec<Record>) {
        out.clear();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cap = g.records.len();
        for i in 0..cap {
            let idx = (g.head + cap - 1 - i) % cap;
            #[cfg(test)]
            self.drain_visits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match &g.records[idx] {
                Some(r) if r.seq > since => out.push(r.clone()),
                // Either an empty slot (the ring hasn't wrapped yet, and
                // we've walked past its oldest write) or a record at or
                // before `since` — descending order means nothing
                // further back can be newer than `since` either.
                _ => break,
            }
        }
        out.reverse(); // collected newest-first; the contract is oldest-first
    }

    #[cfg(test)]
    fn drain_visits(&self) -> u64 {
        self.drain_visits.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The subscriber layer that fills the ring. Formats the message on
/// the emitting thread, outside the ring's lock.
pub struct RingLayer {
    ring: Arc<Ring>,
}

impl RingLayer {
    pub fn new(ring: Arc<Ring>) -> RingLayer {
        RingLayer { ring }
    }
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            let _ = write!(self.0, "{}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write;
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            let _ = write!(self.0, "{}={value}", field.name());
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RingLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut v = MessageVisitor(String::new());
        event.record(&mut v);
        let meta = event.metadata();
        self.ring.push(Record {
            at: SystemTime::now(),
            level: *meta.level(),
            target: meta.target(),
            message: v.0,
            seq: 0,
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLevels {
    pub default: Level,
    pub targets: Vec<(String, Level)>,
}

fn parse_level(s: &str) -> Option<Level> {
    match s.to_ascii_lowercase().as_str() {
        "error" => Some(Level::ERROR),
        "warn" | "warning" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
}

impl Default for LogLevels {
    fn default() -> Self {
        LogLevels {
            default: Level::INFO,
            targets: Vec::new(),
        }
    }
}

impl LogLevels {
    /// `[log]` in the `app` doc: `default = "info"`, then one key per
    /// target suffix (`ingest = "debug"`). An unknown level is a
    /// warning and the key keeps the default; an unknown key is a
    /// warning too (a typo must not silence a target).
    pub fn from_doc(config: &Config) -> (LogLevels, Vec<Diagnostic>) {
        let mut levels = LogLevels::default();
        let mut diags = Vec::new();
        let layer = config.explain("app", "log");
        let warn = |message: String| Diagnostic {
            severity: Severity::Warning,
            layer,
            file: None,
            message,
            path: None,
        };
        let Some(value) = config.get("app", "log") else {
            return (levels, diags);
        };
        // A scalar `log = "debug"` is not a `[log]` table. Diagnose it instead
        // of silently leaving default levels in effect.
        let Some(table) = value.as_table() else {
            diags.push(warn(
                "[log]: expected a table, e.g. [log]\\ndefault = \"info\"".to_string(),
            ));
            return (levels, diags);
        };
        for (key, value) in table {
            let Some(s) = value.as_str() else {
                diags.push(warn(format!("[log] {key}: expected a level string")));
                continue;
            };
            let Some(level) = parse_level(s) else {
                diags.push(warn(format!(
                    "[log] {key} = {s:?}: not a level (error, warn, info, debug, trace)"
                )));
                continue;
            };
            if key == "default" {
                levels.default = level;
                continue;
            }
            if !TARGETS
                .iter()
                .any(|t| t.strip_prefix("geode::") == Some(key.as_str()))
            {
                diags.push(warn(format!(
                    "[log] {key}: not a known target ({})",
                    TARGETS
                        .iter()
                        .map(|t| &t[7..])
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
                continue;
            }
            levels.targets.retain(|(k, _)| k != key);
            levels.targets.push((key.clone(), level));
        }
        // Canonical target order makes derived equality independent of TOML key
        // order, so an unchanged configuration does not reapply the runtime filter.
        levels.targets.sort();
        (levels, diags)
    }

    /// Build a global filter: targets outside `geode` are capped at `warn`,
    /// `geode` follows `self.default`, and configured target overrides take
    /// precedence through longest-prefix matching. Verbose Geode logging therefore
    /// does not enable verbose third-party records that could crowd out the ring.
    pub fn to_targets(&self) -> Targets {
        let mut t = Targets::new()
            .with_default(Level::WARN)
            .with_target("geode", self.default);
        for (suffix, level) in &self.targets {
            t = t.with_target(format!("geode::{suffix}"), *level);
        }
        t
    }

    pub fn with(&self, target: &str, level: Level) -> LogLevels {
        let mut out = self.clone();
        out.targets.retain(|(k, _)| k != target);
        out.targets.push((target.to_string(), level));
        out.targets.sort(); // Match the canonical order produced by `from_doc`.
        out
    }
}

/// Runtime control over the installed subscriber's level filter. Shell actions
/// use this interface without depending on `tracing_subscriber::reload`.
pub trait LevelControl: Send + Sync {
    fn set(&self, levels: &LogLevels) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(seq: u64, msg: &str) -> Record {
        Record {
            at: std::time::SystemTime::UNIX_EPOCH,
            level: Level::INFO,
            target: "geode::shell",
            message: msg.into(),
            seq,
        }
    }

    #[test]
    fn drain_since_returns_only_newer_records_oldest_first() {
        let ring = Ring::new(4);
        for i in 1..=3 {
            ring.push(rec(i, &format!("m{i}")));
        }
        let mut out = vec![rec(0, "stale")];
        ring.drain_since(1, &mut out);
        assert_eq!(
            out.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![2, 3],
            "cleared first, then seq > since"
        );
        assert_eq!(ring.latest_seq(), 3);
    }

    #[test]
    fn wrapping_overwrites_the_oldest_and_keeps_order() {
        let ring = Ring::new(3);
        for i in 1..=5 {
            ring.push(rec(i, "m"));
        }
        let mut out = Vec::new();
        ring.drain_since(0, &mut out);
        assert_eq!(out.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
    }

    #[test]
    fn a_hit_allocates_nothing_in_the_reader() {
        // The reader's buffer is reused: after one drain that returned
        // n records, a second drain returning n records must not grow
        // the Vec's capacity.
        let ring = Ring::new(8);
        for i in 1..=4 {
            ring.push(rec(i, "m"));
        }
        let mut out = Vec::new();
        ring.drain_since(0, &mut out);
        let cap = out.capacity();
        for i in 5..=8 {
            ring.push(rec(i, "m"));
        }
        ring.drain_since(4, &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(out.capacity(), cap);
    }

    #[test]
    fn two_writers_never_lose_a_sequence_number() {
        let ring = std::sync::Arc::new(Ring::new(1024));
        let a = {
            let r = ring.clone();
            std::thread::spawn(move || {
                for _ in 0..500 {
                    r.push(rec(0, "a"));
                }
            })
        };
        let b = {
            let r = ring.clone();
            std::thread::spawn(move || {
                for _ in 0..500 {
                    r.push(rec(0, "b"));
                }
            })
        };
        a.join().unwrap();
        b.join().unwrap();
        let mut out = Vec::new();
        ring.drain_since(0, &mut out);
        assert_eq!(out.len(), 1000);
        assert!(
            out.windows(2).all(|w| w[1].seq == w[0].seq + 1),
            "seq is assigned by the ring, contiguous"
        );
    }

    #[test]
    fn levels_read_the_log_table_and_report_a_bad_level() {
        let cfg = crate::config::test_support::config_from(
            "app",
            "config_version = 1\n[log]\ndefault = \"info\"\ningest = \"debug\"\nquery = \"loud\"\n",
        );
        let (levels, diags) = LogLevels::from_doc(&cfg);
        assert_eq!(levels.default, Level::INFO);
        assert_eq!(levels.targets, vec![("ingest".to_string(), Level::DEBUG)]);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("loud"));
    }

    #[test]
    fn to_targets_maps_suffixes_onto_geode_targets() {
        let levels = LogLevels {
            default: Level::WARN,
            targets: vec![("ingest".into(), Level::TRACE)],
        };
        let t = levels.to_targets();
        assert!(t.would_enable("geode::ingest", &Level::TRACE));
        assert!(!t.would_enable("geode::query", &Level::INFO));
        assert!(t.would_enable("geode::query", &Level::WARN));
    }

    #[test]
    fn an_unknown_target_key_is_a_warning() {
        let cfg = crate::config::test_support::config_from(
            "app",
            "config_version = 1\n[log]\nbogus = \"debug\"\n",
        );
        let (levels, diags) = LogLevels::from_doc(&cfg);
        assert!(
            levels.targets.is_empty(),
            "an unknown key is dropped, not kept"
        );
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("bogus"));
    }

    /// Equivalent target settings compare equal regardless of file order.
    /// Both parsing and `with` must canonicalize the vector so consumers can
    /// skip unchanged level updates.
    #[test]
    fn targets_in_a_different_file_order_compare_equal() {
        let a = crate::config::test_support::config_from(
            "app",
            "config_version = 1\n[log]\ningest = \"debug\"\nquery = \"trace\"\n",
        );
        let b = crate::config::test_support::config_from(
            "app",
            "config_version = 1\n[log]\nquery = \"trace\"\ningest = \"debug\"\n",
        );
        let (levels_a, _) = LogLevels::from_doc(&a);
        let (levels_b, _) = LogLevels::from_doc(&b);
        assert_eq!(levels_a, levels_b);

        // `with` must preserve the same canonical order too.
        let via_with = LogLevels::default()
            .with("query", Level::TRACE)
            .with("ingest", Level::DEBUG);
        assert_eq!(via_with, levels_a);
    }

    // ---- Configuration validation and filtering -----------------------

    /// A scalar `log = "debug"` warns and leaves defaults in effect;
    /// configuration requires a `[log]` table.
    #[test]
    fn log_present_but_not_a_table_is_a_warning() {
        let cfg = crate::config::test_support::config_from(
            "app",
            "config_version = 1\nlog = \"debug\"\n",
        );
        let (levels, diags) = LogLevels::from_doc(&cfg);
        assert_eq!(levels, LogLevels::default(), "no effect, same as today");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("table"));
    }

    /// Targets outside `geode` stay capped at `warn` when Geode's default is
    /// more verbose. Geode targets follow the configured default.
    #[test]
    fn to_targets_caps_non_geode_targets_at_warn_regardless_of_default() {
        let levels = LogLevels {
            default: Level::TRACE,
            targets: Vec::new(),
        };
        let t = levels.to_targets();
        assert!(
            t.would_enable("geode::shell", &Level::TRACE),
            "geode follows [log] default"
        );
        assert!(
            !t.would_enable("some_dependency", &Level::INFO),
            "a third party is capped below info at the default trace"
        );
        assert!(
            t.would_enable("some_dependency", &Level::WARN),
            "warn itself is still the third-party ceiling"
        );
    }

    // ---- Tracing subscriber and structured message fields --------------

    use tracing_subscriber::layer::SubscriberExt;

    /// Runs `f` under a subscriber that feeds only a fresh [`Ring`],
    /// scoped (`tracing::subscriber::with_default`) rather than global —
    /// this never touches whatever subscriber a real process installed,
    /// so it's safe next to every other test in the suite.
    fn logged(capacity: usize, f: impl FnOnce()) -> Vec<Record> {
        let ring = Arc::new(Ring::new(capacity));
        let sub = tracing_subscriber::registry().with(RingLayer::new(ring.clone()));
        tracing::subscriber::with_default(sub, f);
        let mut out = Vec::new();
        ring.drain_since(0, &mut out);
        out
    }

    #[test]
    fn ring_layer_records_target_level_and_the_formatted_message() {
        let records = logged(8, || {
            tracing::warn!(target: "geode::shell", "hello {}", 1);
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].target, "geode::shell");
        assert_eq!(records[0].level, Level::WARN);
        assert_eq!(records[0].message, "hello 1");
    }

    /// The visitor's `record_str`/`record_debug` split: the message
    /// field always renders unquoted (whichever visit method carries
    /// it — `Debug for fmt::Arguments` forwards to `Display`, so
    /// `record_debug`'s `{value:?}` on the message is still unquoted); a
    /// non-message `&str` field goes through `record_str` and also
    /// renders unquoted; a non-message field forced through `Debug`
    /// (`?field`) renders however that type's `Debug` does — quoted, for
    /// a `&str`.
    #[test]
    fn message_visitor_treats_the_message_field_specially_and_separates_others_with_spaces() {
        let records = logged(8, || {
            tracing::info!(target: "geode::query", plain = "x", debug = ?"y", "structured");
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "structured plain=x debug=\"y\"");
    }

    /// A structured string field remains in the recorded message with its name
    /// and value, independently of the primary message text.
    #[test]
    fn a_non_message_str_field_is_not_dropped() {
        let records = logged(8, || {
            tracing::info!(target: "geode::query", book = "EU_TECH", "requery");
        });
        assert_eq!(records.len(), 1);
        assert!(
            records[0].message.contains("book=EU_TECH"),
            "{:?}",
            records[0].message
        );
    }

    // ---- Bounded tail reads and overwritten-record detection -----------

    #[test]
    fn drain_since_stops_scanning_once_it_reaches_records_at_or_before_since() {
        let ring = Ring::new(1024);
        for i in 1..=1000 {
            ring.push(rec(i, "m"));
        }
        let mut out = Vec::new();
        // Only the last three are new; a bounded scan from the tail
        // should visit a handful of slots, not all 1000+ written ones.
        ring.drain_since(997, &mut out);
        assert_eq!(
            out.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![998, 999, 1000]
        );
        assert!(
            ring.drain_visits() < 10,
            "expected an early break near the tail, visited {}",
            ring.drain_visits()
        );
    }

    #[test]
    fn oldest_seq_is_none_when_empty_then_tracks_the_surviving_floor_through_a_wrap() {
        let ring = Ring::new(3);
        assert_eq!(ring.oldest_seq(), None);
        ring.push(rec(0, "a"));
        assert_eq!(
            ring.oldest_seq(),
            Some(1),
            "not wrapped: oldest is the first write"
        );
        for _ in 0..4 {
            ring.push(rec(0, "b"));
        }
        // 5 pushes into a 3-slot ring: seq 1 and 2 were overwritten:
        // seq 3 is the oldest survivor.
        assert_eq!(ring.oldest_seq(), Some(3), "wrapped: oldest is at head");
    }

    #[test]
    fn pricing_is_a_known_log_target() {
        assert_eq!(TARGETS.len(), 7);
        assert!(TARGETS.contains(&"geode::pricing"));
        let config =
            crate::config::test_support::config_from("app", "[log]\npricing = \"debug\"\n");
        let (levels, diags) = LogLevels::from_doc(&config);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            levels
                .targets
                .iter()
                .any(|(t, l)| t == "pricing" && *l == tracing::Level::DEBUG)
        );
    }
}
