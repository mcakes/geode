//! The in-process log (spec §4.1–4.3): a fixed ring every subscriber
//! layer feeds, read by the diagnostics tile; `[log]` levels; the
//! control the shell uses to change them at runtime.
use crate::config::{Config, Diagnostic, Severity};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
pub use tracing::Level;
use tracing_subscriber::filter::Targets;

/// The six targets every subscriber layer and `[log]` key name (by
/// suffix, e.g. `ingest` → `geode::ingest`) know about.
pub const TARGETS: [&str; 6] = [
    "geode::ingest",
    "geode::query",
    "geode::config",
    "geode::session",
    "geode::shell",
    "geode::theme",
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
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .len()
    }

    /// Overwrites the oldest slot once full; never blocks a writer for
    /// longer than one copy. `seq` is assigned here, contiguous, so two
    /// threads racing on `push` cannot produce a gap or a duplicate.
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

    /// Records with `seq > since`, oldest first, into `out` (cleared
    /// first). The reader owns the buffer: a tile following the tail
    /// reuses one `Vec` for its life, so a hit allocates nothing beyond
    /// the record clones themselves.
    pub fn drain_since(&self, since: u64, out: &mut Vec<Record>) {
        out.clear();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cap = g.records.len();
        // Oldest slot is `head` once wrapped, else 0.
        for i in 0..cap {
            let idx = (g.head + i) % cap;
            if let Some(r) = &g.records[idx]
                && r.seq > since
            {
                out.push(r.clone());
            }
        }
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
        let Some(table) = config.get("app", "log").and_then(|v| v.as_table()) else {
            return (levels, diags);
        };
        let layer = config.explain("app", "log");
        let warn = |message: String| Diagnostic {
            severity: Severity::Warning,
            layer,
            file: None,
            message,
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
        (levels, diags)
    }

    pub fn to_targets(&self) -> Targets {
        let mut t = Targets::new().with_default(self.default);
        for (suffix, level) in &self.targets {
            t = t.with_target(format!("geode::{suffix}"), *level);
        }
        t
    }

    pub fn with(&self, target: &str, level: Level) -> LogLevels {
        let mut out = self.clone();
        out.targets.retain(|(k, _)| k != target);
        out.targets.push((target.to_string(), level));
        out
    }
}

/// Runtime control over the installed subscriber's level filter — `:level`
/// (a later task) goes through this rather than touching the subscriber
/// directly, so the shell never names `tracing_subscriber::reload` itself.
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
}
