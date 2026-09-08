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
use std::rc::Rc;
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
    /// `None` until the first real `note_health` call — a source that is
    /// merely *configured* (`describe_source`) or has only been *polled*
    /// (`note_polled`) has not yet reported anything, and must not be
    /// counted in [`Diagnostics::summary`] or read as any particular
    /// health (Phase 4b Task 4 fix round 1, CRIT-1: a healthy desk with
    /// no `Health` event ever emitted for a cleanly loading source used
    /// to show a permanent, warning-toned `sources N pending`). Task 5's
    /// sources section reads `None` as "no report yet", not as
    /// `Health::Pending`.
    pub health: Option<Health>,
    pub detail: String,
    pub since: SystemTime,
    pub last_poll: Option<SystemTime>,
    pub next_poll: Option<SystemTime>,
    pub last_ready: usize,
    /// Capped at [`SOURCE_HISTORY_CAP`], oldest first (the newest
    /// transition is always the tail). Only real `note_health` calls
    /// ever push — a source with `health: None` has an empty history.
    pub history: VecDeque<(SystemTime, Health)>,
}

impl Default for SourceState {
    fn default() -> SourceState {
        SourceState {
            spec: None,
            health: None,
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

/// How many batches [`Diagnostics::config_history`] keeps, latest first.
pub const CONFIG_HISTORY_CAP: usize = 16;

/// How many entries [`Diagnostics::data_diagnostics`] keeps, oldest
/// first (a plain append cap, not "batches" — see that field's doc).
pub const DATA_DIAGNOSTICS_CAP: usize = 256;

/// The shell-owned diagnostics gatherer (spec §4.4). See the module doc
/// for the version-bump discipline every mutator here follows.
pub struct Diagnostics {
    pub sources: BTreeMap<String, SourceState>,
    pub datasets: BTreeMap<String, DatasetState>,
    /// The current CONFIG-LOAD diagnostics — the latest load or reload's
    /// batch, whole (Phase 4b Task 4 fix round 1, MAJ-5: was a capped,
    /// ever-appending log; a reload that changed nothing used to
    /// re-append its own unchanged batch, inflating
    /// [`Self::summary`]'s error count every time `:level`'s own persist
    /// triggered a reload). [`Self::note_config`] *replaces* this
    /// wholesale; [`Self::config_history`] is the append-only log now.
    ///
    /// Fed *only* by config load/reload (`ShellView::new`'s startup call
    /// and `hot_reload::apply_reload`, every reload unconditionally) —
    /// **not** by the data layer's own diagnostics, which is
    /// [`Self::data_diagnostics`] (Phase 4b Task 4 fix round 2, NEW-1:
    /// round 1 fed both populations through this one field via
    /// `note_config`'s replace semantics, so a data-layer error and a
    /// later config reload — including the one `:level`'s own persist
    /// write triggers — silently erased each other from the summary,
    /// the same false-signal class CRIT-1 was raised under, just the
    /// opposite direction: a false *negative* instead of a false
    /// positive). [`Self::summary`] counts errors from both fields.
    pub config: Vec<Diagnostic>,
    /// Every batch [`Self::note_config`] has ever installed into
    /// [`Self::config`], latest first, capped at [`CONFIG_HISTORY_CAP`]
    /// — an audit trail for Task 5's config section, distinct from the
    /// "what's true right now" [`Self::config`] the summary counts.
    pub config_history: VecDeque<(SystemTime, Vec<Diagnostic>)>,
    /// The data layer's own diagnostics (Phase 4b Task 4 fix round 2,
    /// NEW-1) — fed by [`Self::note_data_diagnostics`] from the app
    /// bridge's `DataEvent::Diagnostics` arm (schema/dataset/view
    /// validation errors at service open, a failed open, or after a
    /// `Request::ReplaceViews`). A plain append log, oldest first,
    /// capped at [`DATA_DIAGNOSTICS_CAP`] — unlike [`Self::config`],
    /// there is no single "current batch" here: `geode-data` reports
    /// once per condition, not a full snapshot on every event, so
    /// nothing here would be safe to *replace*. A `Diagnostic` already
    /// present (by equality) is not re-appended.
    pub data_diagnostics: VecDeque<(SystemTime, Diagnostic)>,
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
    /// churn PHILOSOPHY.md forbids. `Rc<str>` (Phase 4b Task 4 fix
    /// round 1, MAJ-1 — was `String`): a cache hit used to hand back a
    /// freshly allocated `String` on every single paint (`.clone()` on
    /// a `String` allocates); a hit here clones a refcount instead, the
    /// same shape `Frame::bar_cache` already uses for its `Rc<
    /// ScopeBarModel>`.
    summary_cache: RefCell<(u64, Rc<str>)>,
}

impl Diagnostics {
    pub fn new(levels: LogLevels) -> Diagnostics {
        Diagnostics {
            sources: BTreeMap::new(),
            datasets: BTreeMap::new(),
            config: Vec::new(),
            config_history: VecDeque::new(),
            data_diagnostics: VecDeque::new(),
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
            summary_cache: RefCell::new((u64::MAX, Rc::from(""))),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// A source's static description (once, at bridge `attach`). A
    /// no-op for an identical, already-recorded summary (Phase 4b Task
    /// 4 fix round 1, MIN-2) — brief-sanctioned to bump unconditionally
    /// since `attach` runs once per window, but a second `attach` (or a
    /// future re-describe) must not bump for nothing.
    pub fn describe_source(&mut self, source: &str, summary: SourceSummary) {
        let state = self.sources.entry(source.to_string()).or_default();
        if state.spec.as_ref() == Some(&summary) {
            return;
        }
        state.spec = Some(summary);
        self.version += 1;
    }

    /// Record a source's worst health as of `at`. The *first* real note
    /// for a source is always a transition — guarded on `state.health`
    /// being `None`, not on whether the map entry already exists (Phase
    /// 4b Task 4 fix round 1, MAJ-2: `describe_source`/`note_polled`
    /// both create the entry via `or_default()` before any health ever
    /// arrives, so guarding on entry-existence swallowed the first real
    /// note whenever either had already run — `since` stayed at the
    /// epoch and `history` stayed empty for a perfectly healthy source
    /// for the whole session). After the first real note, reporting the
    /// same `(worst, detail)` again is a no-op.
    pub fn note_health(&mut self, source: &str, worst: Health, detail: String, at: SystemTime) {
        let state = self.sources.entry(source.to_string()).or_default();
        if let Some(current) = &state.health
            && *current == worst
            && state.detail == detail
        {
            return;
        }
        state.health = Some(worst.clone());
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

    /// The current config diagnostics batch, from a load or reload —
    /// *replaces* [`Self::config`] wholesale (Phase 4b Task 4 fix round
    /// 1, MAJ-5: used to append into a capped log unconditionally
    /// except on an empty batch, so an unchanged reload — e.g. the one
    /// `:level`'s own persist write triggers — re-appended the exact
    /// same diagnostics and inflated `summary`'s error count every
    /// time). A no-op when `diags` is byte-identical to the current
    /// batch (this also covers the old "empty batch" guard: an empty
    /// batch equal to an already-empty `self.config` is a no-op, but an
    /// empty batch replacing a *non-empty* one now correctly clears it
    /// — a clean reload after a run of config errors must be able to
    /// zero the count). Every real change is also appended to
    /// [`Self::config_history`], capped at [`CONFIG_HISTORY_CAP`].
    pub fn note_config(&mut self, diags: Vec<Diagnostic>, at: SystemTime) {
        if self.config == diags {
            return;
        }
        self.config = diags.clone();
        self.config_history.push_front((at, diags));
        while self.config_history.len() > CONFIG_HISTORY_CAP {
            self.config_history.pop_back();
        }
        self.version += 1;
    }

    /// The data layer's own diagnostics (Phase 4b Task 4 fix round 2,
    /// NEW-1) — appended, not replaced: unlike a config load, `geode-data`
    /// reports once per condition rather than a full snapshot each time,
    /// so there is no "current batch" to replace here. A `Diagnostic`
    /// already present (by equality, anywhere in the list) is not
    /// re-appended and does not bump — the bridge's own `DataEvent::
    /// Diagnostics` arm can otherwise re-report the same condition (e.g.
    /// a `Request::ReplaceViews` after an unrelated reload). Capped at
    /// [`DATA_DIAGNOSTICS_CAP`], oldest dropped first.
    pub fn note_data_diagnostics(&mut self, diags: Vec<Diagnostic>, at: SystemTime) {
        let mut changed = false;
        for d in diags {
            if self
                .data_diagnostics
                .iter()
                .any(|(_, existing)| existing == &d)
            {
                continue;
            }
            self.data_diagnostics.push_back((at, d));
            changed = true;
        }
        while self.data_diagnostics.len() > DATA_DIAGNOSTICS_CAP {
            self.data_diagnostics.pop_front();
        }
        if changed {
            self.version += 1;
        }
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
    /// snapshot) does not bump. Every dataset's `catalog` is cleared
    /// before the new snapshot's datasets are folded back in (Phase 4b
    /// Task 4 fix round 1, MIN-5): `self.datasets`' *keys* stay
    /// monotonic (a dataset `note_published` has ever named keeps
    /// existing as a map entry), but a dataset absent from a newer
    /// snapshot must not keep showing a stale `DatasetCatalog` forever.
    pub fn set_catalog(&mut self, snapshot: CatalogSnapshot) {
        if self.catalog.as_ref() == Some(&snapshot) {
            return;
        }
        for state in self.datasets.values_mut() {
            state.catalog = None;
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
    /// nothing when no tile could show it) AND only when it actually
    /// changed (Phase 4b Task 4 fix round 1, MAJ-3: the watchers gate
    /// alone still bumped on every ~500ms tick even when nothing was
    /// recorded in between, which — through `ShellView::
    /// on_diagnostics_changed`'s unconditional `cx.notify()` — repainted
    /// the whole shell twice a second forever while a diagnostics tile
    /// sat open and idle, exactly the "bumps unconditionally" failure
    /// this module's own doc warns against). `FrameHistogram` has no
    /// `PartialEq`; `count()` plus `max_micros()` is a cheap, sufficient
    /// proxy — both are monotonically non-decreasing, so equal on both
    /// means nothing new was recorded. `true` (and a bump) exactly when
    /// the copy happened.
    ///
    /// **Known gap (Phase 4b Task 4 fix round 2, NEW-4), accepted as-is:**
    /// a tick whose only change is a fresh `note_discarded_idle` (an
    /// idle gap counted, no frame recorded — `perf.rs`) does not copy,
    /// since that field bumps neither `count()` nor `max_micros()`.
    /// `discarded_idle` is part of the copied `FrameHistogram` and is
    /// surfaced by the profiling-gated `perf::dump`, not the default
    /// overlay, so the practical cost is that Task 5's tile could show a
    /// stale `discarded_idle` value between two ticks that were
    /// otherwise identical. The proxy is sound for everything else: both
    /// fields it does check are monotonic between resets, and a
    /// `reset()` that leaves both at 0 also leaves nothing worth
    /// showing.
    pub fn refresh_frame_hist(&mut self, hist: &FrameHistogram) -> bool {
        if self.watchers == 0 {
            return false;
        }
        if self.frame_hist.count() == hist.count()
            && self.frame_hist.max_micros() == hist.max_micros()
        {
            return false;
        }
        self.frame_hist = hist.clone();
        self.version += 1;
        true
    }

    /// A diagnostics tile became visible: also queues a catalog request
    /// (Task 5's `set_visible(true)` is the one caller — "visibility
    /// triggers the first request through the same drain" the plan's
    /// Step 6 describes, so a freshly opened tile does not sit on
    /// whatever `self.catalog` happened to hold, or nothing, until the
    /// next publish happens to arrive). Does not itself bump `version` —
    /// it changes nothing about the diagnostic *data*, only what future
    /// mutators are allowed to do (spend a catalog request, copy the
    /// frame histogram) and what the bridge's drain will act on once the
    /// caller's own `cx.notify()` runs (same two-step contract every
    /// other entity mutation in this codebase follows: mutate, then
    /// notify at the call site).
    ///
    /// **Returns `true`, always today** (Phase 4b Task 4 fix round 2,
    /// MIN-6): every call queues a request, so the return value carries
    /// no information beyond "a request was queued" — its purpose is to
    /// make that fact visible at the *type* level, not just in this
    /// comment, so a caller reading the signature is reminded a
    /// `cx.notify()` is now owed. **This method does not notify by
    /// itself and cannot** — it has no `Context`. Task 5's `set_visible`
    /// MUST call `cx.notify()` in the same `diagnostics.update(cx, |d,
    /// cx| { d.watch(); cx.notify(); })` block, or the queued request
    /// sits unseen by the bridge's `cx.observe(&diagnostics, ..)` drain
    /// (registered in `geode-app::bridge::attach`) until some *other*
    /// mutation happens to notify later. See
    /// `watch_reaching_the_bridge_drain_requires_the_callers_own_notify`
    /// (`shell/tests/diagnostics.rs`) for the contract exercised end to
    /// end through a real bridge.
    pub fn watch(&mut self) -> bool {
        self.watchers += 1;
        self.pending_catalog_request = true;
        true
    }

    /// The counterpart of [`Self::watch`] — a diagnostics tile went
    /// invisible or was torn down. Saturating: never underflows past 0.
    /// Clears a still-pending catalog request once the *last* watcher
    /// leaves (Phase 4b Task 4 fix round 1, MIN-4): a tile that becomes
    /// visible and immediately invisible again must not cost a database
    /// round trip whose outcome nothing will ever show. A request stays
    /// queued while at least one other tile is still watching.
    pub fn unwatch(&mut self) {
        self.watchers = self.watchers.saturating_sub(1);
        if self.watchers == 0 {
            self.pending_catalog_request = false;
        }
    }

    pub fn watchers(&self) -> u32 {
        self.watchers
    }

    /// Queue a fresh catalog request without changing any diagnostic data
    /// (Phase 4b Task 5 fix round 1, MAJ-7): the resolved-generation
    /// marker the data section paints (spec §4.5) is computed by the data
    /// thread from the `CatalogParams::as_of` the request that produced
    /// the held `CatalogSnapshot` carried — nothing re-requests one when
    /// the frame's as-of changes, so a stale snapshot keeps marking a
    /// generation the engine would no longer resolve to. The diagnostics
    /// tile calls this when its own observed `as_of` version changed
    /// while visible; the bridge's drain (already reading the frame's
    /// *current* as-of at request time) does the rest.
    ///
    /// Never bumps `version` — same reasoning as [`Self::watch`]'s own
    /// "does not itself bump" doc comment: nothing about the diagnostic
    /// *data* changed, only what the bridge's drain is queued to do next.
    /// Callers MUST `cx.notify()` themselves in the same update block —
    /// the bridge's `cx.observe(&diagnostics, ..)` drain does not gate on
    /// `version`, only on the pending flag, but it never runs at all
    /// without a notify to wake it.
    pub fn request_catalog(&mut self) {
        self.pending_catalog_request = true;
    }

    /// `:level <target> <level>` (a later task's module command) or the
    /// reload-driven `[log]` pickup: updates `self.levels` (via
    /// `LogLevels::with`, the same retain-then-push `[log]` parsing
    /// already uses) and queues one persist for
    /// [`Self::take_pending_level`] to drain. A no-op when `target`
    /// already carries `level` (Phase 4b Task 4 fix round 1, MIN-3):
    /// otherwise `:level ingest debug` typed twice queued (and wrote)
    /// two identical persists, and — per the MAJ-5 fix above — could
    /// have tripped a reload each time.
    pub fn request_level(&mut self, target: &str, level: Level) {
        if self
            .levels
            .targets
            .iter()
            .any(|(t, l)| t == target && *l == level)
        {
            return;
        }
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

    /// `"sources 3 ok · 1 degraded · config 2 errors · data 1 error ·
    /// 5 dropped"` — every segment optional, omitted when its count is
    /// zero; `""` (never shown by the status bar — `(!s.is_empty())
    /// .then_some(..)`'s job at the call site) when there is nothing to
    /// report at all. Cached (see [`Self::summary_cache`]'s own doc
    /// comment) keyed on `version`; a hit clones an `Rc<str>` refcount,
    /// never a buffer.
    ///
    /// The `sources` segment counts only sources with a *real* health
    /// note (`SourceState.health.is_some()`) — a configured-but-not-yet-
    /// reported source is not counted at all (Phase 4b Task 4 fix round
    /// 1, CRIT-1; Task 5's sources section shows it as "no report yet"
    /// instead). `config N error(s)` counts `self.config`'s
    /// [`Severity::Error`] entries — the *current* batch only (MAJ-5),
    /// not the history — and `data N error(s)` counts
    /// [`Self::data_diagnostics`]' [`Severity::Error`] entries
    /// separately (Phase 4b Task 4 fix round 2, NEW-1: the two
    /// populations must never share one count, or a config reload and a
    /// data-layer error silently erase each other). No
    /// `restart required: …` segment any more (Phase 4b Task 4 fix
    /// round 1, MAJ-4): the status bar's own `restart_required` segment
    /// (`shell/render.rs`) already shows that message; embedding it here
    /// too duplicated it on screen.
    pub fn summary(&self) -> Rc<str> {
        {
            let cache = self.summary_cache.borrow();
            if cache.0 == self.version {
                return cache.1.clone();
            }
        }
        let built: Rc<str> = Rc::from(self.build_summary());
        *self.summary_cache.borrow_mut() = (self.version, built.clone());
        built
    }

    fn build_summary(&self) -> String {
        const LABELS: [&str; 5] = ["ok", "pending", "pending_too_long", "degraded", "failed"];
        let mut counts = [0usize; LABELS.len()];
        for s in self.sources.values() {
            let Some(health) = &s.health else {
                continue; // no report yet — not counted (CRIT-1)
            };
            if let Some(idx) = LABELS.iter().position(|&l| l == health.label()) {
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

        let plural = |n: usize| if n == 1 { "" } else { "s" };

        let config_errors = self
            .config
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        if config_errors > 0 {
            parts.push(format!(
                "config {config_errors} error{}",
                plural(config_errors)
            ));
        }

        // NEW-1 (Phase 4b Task 4 fix round 2): counted separately from
        // `config_errors` above — the two populations are unrelated
        // (config load vs. the data layer) and must not clobber each
        // other's count, the exact bug this split fixes.
        let data_errors = self
            .data_diagnostics
            .iter()
            .filter(|(_, d)| d.severity == Severity::Error)
            .count();
        if data_errors > 0 {
            parts.push(format!("data {data_errors} error{}", plural(data_errors)));
        }

        if self.dropped_events > 0 {
            parts.push(format!("{} dropped", self.dropped_events));
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

    /// MAJ-2: `describe_source`/`note_polled` both create the entry
    /// before any real health arrives; the first `note_health` call must
    /// still be treated as a transition (`since` set, one history row)
    /// rather than swallowed by comparing against the freshly created
    /// default.
    #[test]
    fn the_first_real_health_note_transitions_even_after_describe_source_and_note_polled() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec![],
                priority: "".into(),
                readiness: "".into(),
            },
        );
        d.note_polled(
            "risk",
            3,
            SystemTime::UNIX_EPOCH,
            SystemTime::UNIX_EPOCH + Duration::from_secs(30),
        );
        assert!(
            d.sources["risk"].health.is_none(),
            "no real health note yet"
        );
        let v0 = d.version();
        d.note_health(
            "risk",
            Health::Ok,
            "".into(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        );
        assert!(d.version() > v0);
        assert_eq!(
            d.sources["risk"].since,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1)
        );
        assert_eq!(
            d.sources["risk"].history.len(),
            1,
            "the first real note is a transition"
        );
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
            d.summary().as_ref(),
            "sources 1 ok · 1 degraded · config 1 error · 5 dropped"
        );
    }

    /// CRIT-1: a source that is configured (`describe_source`) but has
    /// never reported a real health note must not appear in the
    /// summary at all — not as "pending", not as anything.
    #[test]
    fn a_described_but_unreported_source_is_not_counted_in_the_summary() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec!["/data/*.csv".into()],
                priority: "latest_risk".into(),
                readiness: "sentinel".into(),
            },
        );
        assert_eq!(d.summary().as_ref(), "");
    }

    #[test]
    fn a_publish_requests_a_catalog_only_while_watched() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_published("risk");
        assert!(!d.take_pending_catalog_request());
        d.watch();
        d.take_pending_catalog_request(); // drain watch()'s own request first
        d.note_published("risk");
        assert!(d.take_pending_catalog_request());
        assert!(!d.take_pending_catalog_request(), "drained");
    }

    /// `watch()` itself queues a catalog request (Phase 4b §4.5, plan
    /// Step 6: "visibility triggers the first request through the same
    /// drain") — a freshly visible tile gets its first catalog without
    /// waiting for the next publish.
    #[test]
    fn watching_itself_also_requests_the_first_catalog() {
        let mut d = Diagnostics::new(LogLevels::default());
        assert!(!d.take_pending_catalog_request());
        d.watch();
        assert!(
            d.take_pending_catalog_request(),
            "becoming watched requests the first catalog"
        );
        assert!(!d.take_pending_catalog_request(), "drained");
    }

    /// Phase 4b Task 5 fix round 1, MAJ-7: `request_catalog` queues a
    /// request without touching `version` — a caller (the diagnostics
    /// tile, on an as-of change) must still notify itself for the bridge
    /// to see it, but nothing about this call is itself a diagnostic-data
    /// change.
    #[test]
    fn request_catalog_queues_without_bumping_the_version() {
        let mut d = Diagnostics::new(LogLevels::default());
        let v = d.version();
        d.request_catalog();
        assert_eq!(d.version(), v, "no diagnostic data changed");
        assert!(d.take_pending_catalog_request());
        assert!(!d.take_pending_catalog_request(), "drained");
    }

    /// MIN-4: the *last* watcher leaving clears a still-pending request
    /// nobody will see the outcome of.
    #[test]
    fn unwatch_to_zero_clears_a_pending_catalog_request() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.watch();
        d.unwatch();
        assert!(!d.take_pending_catalog_request());
    }

    /// MIN-4's other half: a request stays queued while at least one
    /// other tile is still watching.
    #[test]
    fn unwatch_above_zero_keeps_a_pending_catalog_request() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.watch();
        d.watch();
        d.unwatch();
        assert!(d.take_pending_catalog_request());
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

    /// MAJ-3: while watched, an *unchanged* histogram must not copy or
    /// bump — the reload-poll tick calls this every ~500ms regardless
    /// of whether any new frame was recorded in between.
    #[test]
    fn refresh_frame_hist_is_a_no_op_when_the_histogram_is_unchanged() {
        let mut d = Diagnostics::new(LogLevels::default());
        let mut h = FrameHistogram::new();
        h.record_micros(1000);
        d.watch();
        assert!(d.refresh_frame_hist(&h));
        let v = d.version();
        assert!(
            !d.refresh_frame_hist(&h),
            "identical histogram, no copy, no bump"
        );
        assert_eq!(d.version(), v);
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

    /// MIN-3: the same target+level again must not re-queue a persist.
    #[test]
    fn request_level_is_a_no_op_when_the_target_already_has_that_level() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.request_level("ingest", Level::DEBUG);
        d.take_pending_level();
        let v = d.version();
        d.request_level("ingest", Level::DEBUG);
        assert_eq!(d.version(), v);
        assert_eq!(d.take_pending_level(), None, "no new persist queued");
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

    /// MIN-2: re-describing a source with an identical summary must not
    /// bump — only safe by luck today (`attach` runs once); made real.
    #[test]
    fn describe_source_is_a_no_op_for_an_identical_summary() {
        let mut d = Diagnostics::new(LogLevels::default());
        let summary = SourceSummary {
            paths: vec!["/x".into()],
            priority: "p".into(),
            readiness: "r".into(),
        };
        d.describe_source("risk", summary.clone());
        let v = d.version();
        d.describe_source("risk", summary);
        assert_eq!(d.version(), v);
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

    /// MIN-5: a dataset absent from a newer snapshot must not keep a
    /// stale `DatasetCatalog` forever.
    #[test]
    fn set_catalog_drops_a_dataset_missing_from_a_newer_snapshot() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.set_catalog(CatalogSnapshot {
            datasets: vec![DatasetCatalog {
                name: "risk".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(d.datasets["risk"].catalog.is_some());
        d.set_catalog(CatalogSnapshot {
            datasets: vec![],
            threads: 1,
            ..Default::default()
        });
        assert!(
            d.datasets["risk"].catalog.is_none(),
            "a dataset missing from the newer snapshot is cleared"
        );
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
        assert_eq!(d.summary().as_ref(), "");
    }

    #[test]
    fn unwatch_never_underflows_below_zero() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.unwatch();
        assert_eq!(d.watchers(), 0);
    }

    /// MAJ-4: `restart_required` is the status bar's own segment now
    /// (`render.rs`'s `self.restart_required.as_deref()`); the summary
    /// must not embed it too, or the message paints twice.
    #[test]
    fn set_restart_required_does_not_appear_in_the_summary() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.set_restart_required(Some("sources changed".into()));
        assert_eq!(
            d.summary().as_ref(),
            "",
            "restart_required is the status bar's own segment, not the summary's"
        );
    }

    /// MAJ-1: a cache hit must clone the `Rc<str>` refcount, never
    /// rebuild the string — pinned by pointer identity across two calls
    /// with no mutation between them.
    #[test]
    fn summary_reuses_the_same_allocation_when_the_version_is_unchanged() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_dropped(1);
        let a = d.summary();
        let b = d.summary();
        assert!(
            Rc::ptr_eq(&a, &b),
            "a cache hit must clone a refcount, not rebuild"
        );
    }

    // --- MAJ-5: note_config replaces rather than appends ---------------

    #[test]
    fn note_config_is_a_no_op_for_an_identical_batch() {
        let mut d = Diagnostics::new(LogLevels::default());
        let diags = vec![Diagnostic::error(Layer::User, PathBuf::new(), "bad")];
        d.note_config(diags.clone(), SystemTime::UNIX_EPOCH);
        let v = d.version();
        d.note_config(diags, SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        assert_eq!(d.version(), v, "identical batch, no rebuild, no re-append");
        assert_eq!(d.config_history.len(), 1);
    }

    #[test]
    fn note_config_replaces_the_current_batch_rather_than_appending() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_config(
            vec![Diagnostic::error(Layer::User, PathBuf::new(), "a")],
            SystemTime::UNIX_EPOCH,
        );
        d.note_config(
            vec![Diagnostic::error(Layer::User, PathBuf::new(), "b")],
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        );
        assert_eq!(d.config.len(), 1, "current batch is the latest only");
        assert_eq!(d.config[0].message, "b");
        assert_eq!(d.config_history.len(), 2, "history keeps both batches");
    }

    /// A clean reload (empty batch) after standing errors must actually
    /// clear the count — the old "empty batch is always a no-op" guard
    /// used to make this impossible.
    #[test]
    fn note_config_with_an_empty_batch_clears_a_previously_nonempty_one() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_config(
            vec![Diagnostic::error(Layer::User, PathBuf::new(), "a")],
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(d.summary().as_ref(), "config 1 error");
        d.note_config(Vec::new(), SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        assert_eq!(d.config.len(), 0);
        assert_eq!(d.summary().as_ref(), "");
    }

    #[test]
    fn config_history_is_capped_at_sixteen_batches() {
        let mut d = Diagnostics::new(LogLevels::default());
        for i in 0..20u64 {
            d.note_config(
                vec![Diagnostic::error(
                    Layer::User,
                    PathBuf::new(),
                    format!("e{i}"),
                )],
                SystemTime::UNIX_EPOCH + Duration::from_secs(i),
            );
        }
        assert_eq!(d.config_history.len(), 16);
        assert_eq!(
            d.config_history.front().unwrap().0,
            SystemTime::UNIX_EPOCH + Duration::from_secs(19),
            "latest batch first"
        );
    }

    // --- NEW-1 (fix round 2): note_config and note_data_diagnostics ---
    // ---                       are two producers, neither clobbers the
    // ---                       other.

    #[test]
    fn a_config_reload_does_not_clobber_a_standing_data_diagnostic() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_data_diagnostics(
            vec![Diagnostic::error(
                Layer::Builtin,
                PathBuf::new(),
                "bad schema",
            )],
            SystemTime::UNIX_EPOCH,
        );
        d.note_config(
            vec![Diagnostic::error(Layer::User, PathBuf::new(), "config bad")],
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        );
        assert_eq!(
            d.data_diagnostics.len(),
            1,
            "the config-load producer must not touch data_diagnostics"
        );
        assert_eq!(d.config.len(), 1);
        assert_eq!(
            d.summary().as_ref(),
            "config 1 error · data 1 error",
            "both populations are counted, neither erases the other"
        );
    }

    #[test]
    fn a_data_diagnostic_does_not_clobber_a_standing_config_error() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_config(
            vec![Diagnostic::error(Layer::User, PathBuf::new(), "config bad")],
            SystemTime::UNIX_EPOCH,
        );
        d.note_data_diagnostics(
            vec![Diagnostic::error(
                Layer::Builtin,
                PathBuf::new(),
                "bad schema",
            )],
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        );
        assert_eq!(d.config.len(), 1, "the data producer must not touch config");
        assert_eq!(d.data_diagnostics.len(), 1);
    }

    /// The exact NEW-1 scenario: a data-layer error is present, then an
    /// unrelated config reload runs (e.g. the one `:level`'s own persist
    /// triggers) — the data error must survive it.
    #[test]
    fn a_config_reload_after_a_data_layer_error_keeps_the_error_count() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_data_diagnostics(
            vec![Diagnostic::error(
                Layer::Builtin,
                PathBuf::new(),
                "bad schema",
            )],
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(d.summary().as_ref(), "data 1 error");
        // An unrelated, clean config reload (empty batch — nothing wrong
        // with the config itself).
        d.note_config(Vec::new(), SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        assert_eq!(
            d.summary().as_ref(),
            "data 1 error",
            "the data-layer error must survive an unrelated config reload"
        );
    }

    #[test]
    fn note_data_diagnostics_does_not_reappend_an_identical_entry() {
        let mut d = Diagnostics::new(LogLevels::default());
        let diag = Diagnostic::error(Layer::Builtin, PathBuf::new(), "bad schema");
        d.note_data_diagnostics(vec![diag.clone()], SystemTime::UNIX_EPOCH);
        let v = d.version();
        d.note_data_diagnostics(vec![diag], SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        assert_eq!(d.version(), v, "an identical entry is not re-appended");
        assert_eq!(d.data_diagnostics.len(), 1);
    }

    #[test]
    fn data_diagnostics_is_capped_at_two_hundred_fifty_six() {
        let mut d = Diagnostics::new(LogLevels::default());
        for i in 0..260u64 {
            d.note_data_diagnostics(
                vec![Diagnostic::error(
                    Layer::Builtin,
                    PathBuf::new(),
                    format!("e{i}"),
                )],
                SystemTime::UNIX_EPOCH + Duration::from_secs(i),
            );
        }
        assert_eq!(d.data_diagnostics.len(), 256);
        assert_eq!(
            d.data_diagnostics.back().unwrap().1.message,
            "e259",
            "newest kept at the back"
        );
    }
}
