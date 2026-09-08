//! The shell-owned `Diagnostics` entity (Phase 4b Task 4, spec §4.4):
//! source health, dataset generations, config diagnostics and dropped
//! events gathered in one place beside [`crate::frame::Frame`], fed by
//! the app bridge (`geode-app`, the only crate allowed to touch
//! `geode-data`) and by config load/reload. The status bar reads a
//! cached [`Diagnostics::summary`]; the diagnostics module (Task 5)
//! observes the whole entity and rebuilds only on a version change.
//!
//! Pure: no gpui, no I/O, no clock reads — every method that needs "now"
//! takes it as a parameter, same discipline as `Frame`. `ShellView` owns
//! this in a gpui entity and notifies on write; a module reads it
//! through that entity.
//!
//! **Version discipline (load-bearing):** every `note_*`/`set_*` method
//! that actually changes state bumps `version`; one that changes nothing
//! (the same health reported again, the same catalog snapshot, an empty
//! config diagnostic batch) does not. A diagnostics tile compares
//! versions to decide whether to rebuild — a method that bumps
//! unconditionally would rebuild it on every no-op poll.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::time::SystemTime;

use geode_core::config::{Diagnostic, Severity};
pub use geode_core::health::Health;
use geode_core::log::{Level, LogLevels};
use geode_core::query::{CatalogSnapshot, DatasetCatalog};

use crate::perf::FrameHistogram;

/// One source's static description (Phase 4b §4.4's "sources" section),
/// filled once by the app bridge at `attach` from its `SourceSpec` —
/// plain strings, not `geode_data::source::{Priority, Readiness}`
/// themselves, so this module never names `geode-data` (CLAUDE.md: shell
/// and data never depend on each other). The bridge renders `priority`/
/// `readiness` with their own `Debug`/label forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSummary {
    pub paths: Vec<String>,
    pub priority: String,
    pub readiness: String,
}

/// How many transitions [`SourceState::history`] keeps, newest last.
pub const SOURCE_HISTORY_CAP: usize = 16;

/// One source's live state: its static description (once known), its
/// current health and how long it has held it, its last/next poll, and
/// a bounded transition history.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceState {
    pub spec: Option<SourceSummary>,
    pub health: Health,
    pub detail: String,
    pub since: SystemTime,
    pub last_poll: Option<SystemTime>,
    pub next_poll: Option<SystemTime>,
    pub last_ready: usize,
    /// Capped at [`SOURCE_HISTORY_CAP`], oldest first (the newest
    /// transition is always the tail).
    pub history: VecDeque<(SystemTime, Health)>,
}

impl Default for SourceState {
    /// `Health::Pending` — a source with no health note yet is honestly
    /// "hasn't reported", the same label a CSV whose sentinel hasn't
    /// landed carries.
    fn default() -> SourceState {
        SourceState {
            spec: None,
            health: Health::Pending,
            detail: String::new(),
            since: SystemTime::UNIX_EPOCH,
            last_poll: None,
            next_poll: None,
            last_ready: 0,
            history: VecDeque::new(),
        }
    }
}

/// One dataset's catalog state (Phase 4b §4.5) — `None` until the first
/// `Request::Catalog` outcome names it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatasetState {
    pub catalog: Option<DatasetCatalog>,
}

/// The shell-owned diagnostics gatherer (spec §4.4). See the module doc
/// for the version-bump discipline every mutator here follows.
pub struct Diagnostics {
    pub sources: BTreeMap<String, SourceState>,
    pub datasets: BTreeMap<String, DatasetState>,
    /// Every config diagnostic seen this session (load, then every
    /// reload), latest batch first, capped at [`CONFIG_HISTORY_CAP`].
    pub config: VecDeque<(SystemTime, Diagnostic)>,
    pub dropped_events: u64,
    pub restart_required: Option<String>,
    /// A copy of `ShellView::perf`, refreshed by [`Self::refresh_frame_hist`]
    /// on the reload-poll tick — see that method's own doc comment for
    /// why this is a copy rather than the histogram itself.
    pub frame_hist: FrameHistogram,
    /// The last `Request::Catalog` outcome, whole (Phase 4b §4.5).
    pub catalog: Option<CatalogSnapshot>,
    pub levels: LogLevels,
    /// How many diagnostics tiles currently have this entity visible
    /// (Phase 4b open question 2's ruling) — [`Self::watch`]/
    /// [`Self::unwatch`] bracket a tile's `set_visible`, so
    /// `refresh_frame_hist`/`note_published`'s catalog request only do
    /// real work while at least one tile could show it.
    watchers: u32,
    version: u64,
    pending_level: Option<(String, Level)>,
    pending_overlay_toggle: bool,
    pending_catalog_request: bool,
    /// [`Self::summary`]'s cache, keyed on `version` (same `RefCell`
    /// pattern as `Frame::bar_cache`) — `summary` is called from the
    /// status bar's render path every frame, and rebuilding the
    /// formatted string (iterating both maps) on every one of those
    /// calls when nothing changed would be exactly the per-frame heap
    /// churn PHILOSOPHY.md forbids.
    summary_cache: RefCell<(u64, String)>,
}

/// How many config diagnostics [`Diagnostics::config`] keeps.
pub const CONFIG_HISTORY_CAP: usize = 256;

impl Diagnostics {
    pub fn new(levels: LogLevels) -> Diagnostics {
        Diagnostics {
            sources: BTreeMap::new(),
            datasets: BTreeMap::new(),
            config: VecDeque::new(),
            dropped_events: 0,
            restart_required: None,
            frame_hist: FrameHistogram::new(),
            catalog: None,
            levels,
            watchers: 0,
            version: 0,
            pending_level: None,
            pending_overlay_toggle: false,
            pending_catalog_request: false,
            summary_cache: RefCell::new((u64::MAX, String::new())),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// A source's static description (once, at bridge `attach`). Always
    /// bumps — this only ever runs once per source at startup, so there
    /// is no steady-state no-op case to guard against.
    pub fn describe_source(&mut self, source: &str, summary: SourceSummary) {
        let state = self.sources.entry(source.to_string()).or_default();
        state.spec = Some(summary);
        self.version += 1;
    }

    /// Record a source's worst health as of `at`. The *first* note for a
    /// source is always a transition (nothing to compare against yet);
    /// after that, reporting the same `(worst, detail)` again is a
    /// no-op — see the module doc's version discipline.
    pub fn note_health(&mut self, source: &str, worst: Health, detail: String, at: SystemTime) {
        let is_new = !self.sources.contains_key(source);
        let state = self.sources.entry(source.to_string()).or_default();
        if !is_new && state.health == worst && state.detail == detail {
            return;
        }
        state.health = worst.clone();
        state.detail = detail;
        state.since = at;
        state.history.push_back((at, worst));
        while state.history.len() > SOURCE_HISTORY_CAP {
            state.history.pop_front();
        }
        self.version += 1;
    }

    /// Record a source's last/next poll and how many files were ready.
    /// A no-op (same three values again) does not bump.
    pub fn note_polled(&mut self, source: &str, ready: usize, at: SystemTime, next: SystemTime) {
        let state = self.sources.entry(source.to_string()).or_default();
        if state.last_poll == Some(at) && state.next_poll == Some(next) && state.last_ready == ready
        {
            return;
        }
        state.last_poll = Some(at);
        state.next_poll = Some(next);
        state.last_ready = ready;
        self.version += 1;
    }

    /// A file was published for `dataset` (§3.12-style feed, mirrored
    /// here for the diagnostics view): always bumps, and — while at
    /// least one diagnostics tile is watching — sets
    /// [`Self::pending_catalog_request`] so the bridge's drain requests
    /// a fresh `Request::Catalog` (spec §4.5). Unwatched, a publish is
    /// still worth recording (the dataset now exists in `self.datasets`
    /// even before its first real catalog outcome), it just doesn't
    /// spend a database round trip nobody would see.
    pub fn note_published(&mut self, dataset: &str) {
        self.datasets.entry(dataset.to_string()).or_default();
        if self.watchers > 0 {
            self.pending_catalog_request = true;
        }
        self.version += 1;
    }

    /// A batch of config diagnostics from a load or reload — dropped
    /// straight through if empty (nothing changed). Pushed in original
    /// order ahead of whatever was already there, so the batch's own
    /// first diagnostic ends up frontmost (latest-first across batches).
    pub fn note_config(&mut self, diags: Vec<Diagnostic>, at: SystemTime) {
        if diags.is_empty() {
            return;
        }
        for d in diags.into_iter().rev() {
            self.config.push_front((at, d));
        }
        while self.config.len() > CONFIG_HISTORY_CAP {
            self.config.pop_back();
        }
        self.version += 1;
    }

    /// The app bridge's running total of events its bounded channel
    /// refused. A no-op (the same total again) does not bump.
    pub fn note_dropped(&mut self, total: u64) {
        if self.dropped_events == total {
            return;
        }
        self.dropped_events = total;
        self.version += 1;
    }

    /// `None` clears it. A no-op (the same message, or already `None`)
    /// does not bump.
    pub fn set_restart_required(&mut self, message: Option<String>) {
        if self.restart_required == message {
            return;
        }
        self.restart_required = message;
        self.version += 1;
    }

    /// A fresh `Request::Catalog` outcome (spec §4.5): stored whole and
    /// folded per-dataset into `self.datasets`. A no-op (byte-identical
    /// snapshot) does not bump.
    pub fn set_catalog(&mut self, snapshot: CatalogSnapshot) {
        if self.catalog.as_ref() == Some(&snapshot) {
            return;
        }
        for ds in &snapshot.datasets {
            self.datasets.entry(ds.name.clone()).or_default().catalog = Some(ds.clone());
        }
        self.catalog = Some(snapshot);
        self.version += 1;
    }

    /// Copy `hist` into [`Self::frame_hist`] — only while at least one
    /// diagnostics tile is watching (open question 2's ruling: this is
    /// the one place a `FrameHistogram` is cloned, and it must cost
    /// nothing when no tile could show it). `true` (and a bump) exactly
    /// when the copy happened.
    pub fn refresh_frame_hist(&mut self, hist: &FrameHistogram) -> bool {
        if self.watchers == 0 {
            return false;
        }
        self.frame_hist = hist.clone();
        self.version += 1;
        true
    }

    /// A diagnostics tile became visible. Does not itself bump — it
    /// changes nothing about the diagnostic data, only what future
    /// mutators are allowed to do (spend a catalog request, copy the
    /// frame histogram).
    pub fn watch(&mut self) {
        self.watchers += 1;
    }

    /// The counterpart of [`Self::watch`] — a diagnostics tile went
    /// invisible or was torn down. Saturating: never underflows past 0.
    pub fn unwatch(&mut self) {
        self.watchers = self.watchers.saturating_sub(1);
    }

    pub fn watchers(&self) -> u32 {
        self.watchers
    }

    /// `:level <target> <level>` (a later task's module command) or the
    /// reload-driven `[log]` pickup: updates `self.levels` (via
    /// `LogLevels::with`, the same retain-then-push `[log]` parsing
    /// already uses) and queues one persist for
    /// [`Self::take_pending_level`] to drain.
    pub fn request_level(&mut self, target: &str, level: Level) {
        self.levels = self.levels.with(target, level);
        self.pending_level = Some((target.to_string(), level));
        self.version += 1;
    }

    pub fn take_pending_level(&mut self) -> Option<(String, Level)> {
        self.pending_level.take()
    }

    /// A reloaded `[log]` table replacing the whole `LogLevels` at once
    /// (unlike [`Self::request_level`], which changes one target and
    /// queues a persist — a reload picked this up from disk already, so
    /// nothing here re-persists it). A no-op (identical levels) does not
    /// bump.
    pub fn set_levels(&mut self, levels: LogLevels) -> bool {
        if self.levels == levels {
            return false;
        }
        self.levels = levels;
        self.version += 1;
        true
    }

    /// `:overlay` (a later task's module command): queues a toggle for
    /// [`Self::take_pending_overlay_toggle`] to drain. Modules never
    /// reach `ShellView` directly (spec ruling); this is the door.
    pub fn request_overlay_toggle(&mut self) {
        self.pending_overlay_toggle = true;
        self.version += 1;
    }

    pub fn take_pending_overlay_toggle(&mut self) -> bool {
        std::mem::take(&mut self.pending_overlay_toggle)
    }

    pub fn take_pending_catalog_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_catalog_request)
    }

    /// `"sources 3 ok · 1 degraded · config 2 errors · 5 dropped ·
    /// restart required: …"` — every segment optional, omitted when its
    /// count is zero / its value is `None`; `""` (never shown by the
    /// status bar — `Some(str).filter(|s| !s.is_empty())`'s job at the
    /// call site) when there is nothing to report at all. Cached (see
    /// [`Self::summary_cache`]'s own doc comment) keyed on `version`.
    ///
    /// The `config N error(s)` count is `self.config`'s
    /// [`Severity::Error`] entries — everything currently held in the
    /// capped history, not "currently unresolved" (nothing here models
    /// resolution; a stale error from three reloads ago still counts
    /// until it ages out of the 256-entry cap).
    pub fn summary(&self) -> String {
        {
            let cache = self.summary_cache.borrow();
            if cache.0 == self.version {
                return cache.1.clone();
            }
        }
        let built = self.build_summary();
        *self.summary_cache.borrow_mut() = (self.version, built.clone());
        built
    }

    fn build_summary(&self) -> String {
        const LABELS: [&str; 5] = ["ok", "pending", "pending_too_long", "degraded", "failed"];
        let mut counts = [0usize; LABELS.len()];
        for s in self.sources.values() {
            if let Some(idx) = LABELS.iter().position(|&l| l == s.health.label()) {
                counts[idx] += 1;
            }
        }
        let mut parts: Vec<String> = Vec::new();
        let source_parts: Vec<String> = LABELS
            .iter()
            .zip(counts.iter())
            .filter(|&(_, &n)| n > 0)
            .map(|(label, n)| format!("{n} {label}"))
            .collect();
        if !source_parts.is_empty() {
            parts.push(format!("sources {}", source_parts.join(" · ")));
        }

        let error_count = self
            .config
            .iter()
            .filter(|(_, d)| d.severity == Severity::Error)
            .count();
        if error_count > 0 {
            let plural = if error_count == 1 { "" } else { "s" };
            parts.push(format!("config {error_count} error{plural}"));
        }

        if self.dropped_events > 0 {
            parts.push(format!("{} dropped", self.dropped_events));
        }

        if let Some(message) = &self.restart_required {
            parts.push(format!("restart required: {message}"));
        }

        parts.join(" · ")
    }
}

/// A fixed-capacity ring of the last 32 dispatched actions' FNV-1a
/// hashes (Phase 4b Task 6 fills its consumer, the crash file — this
/// type lives here since it's shell-owned state, recorded on every
/// dispatch). Hashes, not `ActionId`s: cloning a `String` per dispatch
/// would be the exact per-frame heap churn PHILOSOPHY.md forbids: see
/// the plan's ruling ("the action tail stores FNV-1a hashes, not ids").
#[derive(Debug, Clone)]
pub struct ActionTail {
    hashes: [u64; 32],
    next: usize,
    len: usize,
}

impl Default for ActionTail {
    fn default() -> ActionTail {
        ActionTail::new()
    }
}

impl ActionTail {
    pub const fn new() -> ActionTail {
        ActionTail {
            hashes: [0; 32],
            next: 0,
            len: 0,
        }
    }

    /// Hash `id` and record it, overwriting the oldest entry once full.
    /// No allocation: `[u64; 32]` written in place.
    pub fn record(&mut self, id: &str) {
        self.hashes[self.next] = fnv1a(id);
        self.next = (self.next + 1) % self.hashes.len();
        if self.len < self.hashes.len() {
            self.len += 1;
        }
    }

    /// The recorded hashes, oldest first, at most 32. When full, `next`
    /// names the oldest surviving slot (the one about to be
    /// overwritten) — same wrap convention as `geode_core::log::Ring`.
    pub fn recent(&self) -> impl Iterator<Item = u64> + '_ {
        let start = if self.len < self.hashes.len() {
            0
        } else {
            self.next
        };
        let cap = self.hashes.len();
        (0..self.len).map(move |i| self.hashes[(start + i) % cap])
    }
}

/// FNV-1a, 64-bit. Used for [`ActionTail`]'s hashes — deterministic,
/// allocation-free, good enough to tell dispatched actions apart in a
/// crash file (not a security hash).
pub fn fnv1a(s: &str) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Diagnostic, Layer};
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn a_repeated_identical_health_does_not_bump_the_version() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::UNIX_EPOCH;
        d.note_health("risk", Health::Ok, "".into(), t);
        let v = d.version();
        d.note_health("risk", Health::Ok, "".into(), t + Duration::from_secs(1));
        assert_eq!(d.version(), v, "no change, no rebuild");
        d.note_health(
            "risk",
            Health::Degraded { reason: "x".into() },
            "x".into(),
            t + Duration::from_secs(2),
        );
        assert!(d.version() > v);
        assert_eq!(
            d.sources["risk"].history.len(),
            2,
            "each transition recorded"
        );
        assert_eq!(d.sources["risk"].since, t + Duration::from_secs(2));
    }

    #[test]
    fn history_is_capped_at_sixteen_transitions() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::UNIX_EPOCH;
        for i in 0..20u64 {
            let health = if i % 2 == 0 {
                Health::Ok
            } else {
                Health::Degraded {
                    reason: format!("r{i}"),
                }
            };
            d.note_health("risk", health, format!("d{i}"), t + Duration::from_secs(i));
        }
        assert_eq!(d.sources["risk"].history.len(), 16);
        // Oldest four (i = 0..=3) dropped; the tail is the newest.
        assert_eq!(
            d.sources["risk"].history.back().unwrap().0,
            t + Duration::from_secs(19)
        );
    }

    #[test]
    fn the_summary_counts_sources_by_health_and_config_errors() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_health("a", Health::Ok, "".into(), SystemTime::UNIX_EPOCH);
        d.note_health(
            "b",
            Health::Degraded { reason: "r".into() },
            "r".into(),
            SystemTime::UNIX_EPOCH,
        );
        d.note_config(
            vec![Diagnostic::error(Layer::User, PathBuf::new(), "bad")],
            SystemTime::UNIX_EPOCH,
        );
        d.note_dropped(5);
        assert_eq!(
            d.summary(),
            "sources 1 ok · 1 degraded · config 1 error · 5 dropped"
        );
    }

    #[test]
    fn a_publish_requests_a_catalog_only_while_watched() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_published("risk");
        assert!(!d.take_pending_catalog_request());
        d.watch();
        d.note_published("risk");
        assert!(d.take_pending_catalog_request());
        assert!(!d.take_pending_catalog_request(), "drained");
    }

    #[test]
    fn the_frame_histogram_is_copied_only_while_watched() {
        let mut d = Diagnostics::new(LogLevels::default());
        let mut h = FrameHistogram::new();
        h.record_micros(1000);
        assert!(!d.refresh_frame_hist(&h));
        d.watch();
        assert!(d.refresh_frame_hist(&h));
        assert_eq!(d.frame_hist.count(), 1);
    }

    #[test]
    fn request_level_updates_levels_and_queues_one_persist() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.request_level("ingest", Level::DEBUG);
        assert_eq!(d.levels.targets, vec![("ingest".to_string(), Level::DEBUG)]);
        assert_eq!(
            d.take_pending_level(),
            Some(("ingest".to_string(), Level::DEBUG))
        );
        assert_eq!(d.take_pending_level(), None);
    }

    #[test]
    fn the_action_tail_keeps_the_last_thirty_two_without_allocating() {
        let mut t = ActionTail::new();
        for i in 0..40 {
            t.record(&format!("a{i}"));
        }
        let recent: Vec<u64> = t.recent().collect();
        assert_eq!(recent.len(), 32);
        assert_eq!(recent[0], fnv1a("a8"), "oldest kept is the 9th");
        assert_eq!(*recent.last().unwrap(), fnv1a("a39"));
    }

    // --- Additional coverage beyond the brief's Step 1 list -----------

    #[test]
    fn describe_source_always_bumps_and_fills_the_spec() {
        let mut d = Diagnostics::new(LogLevels::default());
        let v0 = d.version();
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec!["/data/*.csv".into()],
                priority: "latest_risk".into(),
                readiness: "sentinel".into(),
            },
        );
        assert!(d.version() > v0);
        assert_eq!(
            d.sources["risk"].spec.as_ref().unwrap().priority,
            "latest_risk"
        );
    }

    #[test]
    fn note_dropped_is_a_no_op_when_the_total_is_unchanged() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_dropped(3);
        let v = d.version();
        d.note_dropped(3);
        assert_eq!(d.version(), v);
        d.note_dropped(4);
        assert!(d.version() > v);
    }

    #[test]
    fn set_restart_required_is_a_no_op_when_unchanged() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.set_restart_required(Some("sources changed".into()));
        let v = d.version();
        d.set_restart_required(Some("sources changed".into()));
        assert_eq!(d.version(), v);
        d.set_restart_required(None);
        assert!(d.version() > v);
    }

    #[test]
    fn set_catalog_is_a_no_op_for_a_byte_identical_snapshot() {
        let mut d = Diagnostics::new(LogLevels::default());
        let snap = CatalogSnapshot::default();
        d.set_catalog(snap.clone());
        let v = d.version();
        d.set_catalog(snap);
        assert_eq!(d.version(), v, "identical snapshot, no rebuild");
    }

    #[test]
    fn set_levels_is_a_no_op_when_unchanged() {
        let mut d = Diagnostics::new(LogLevels::default());
        let changed = d.levels.with("ingest", Level::DEBUG);
        assert!(d.set_levels(changed.clone()));
        let v = d.version();
        assert!(!d.set_levels(changed));
        assert_eq!(d.version(), v);
    }

    #[test]
    fn overlay_toggle_is_queued_and_drains_once() {
        let mut d = Diagnostics::new(LogLevels::default());
        assert!(!d.take_pending_overlay_toggle());
        d.request_overlay_toggle();
        assert!(d.take_pending_overlay_toggle());
        assert!(!d.take_pending_overlay_toggle(), "drained");
    }

    #[test]
    fn an_empty_diagnostics_summary_is_empty() {
        let d = Diagnostics::new(LogLevels::default());
        assert_eq!(d.summary(), "");
    }

    #[test]
    fn unwatch_never_underflows_below_zero() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.unwatch();
        assert_eq!(d.watchers(), 0);
    }
}
