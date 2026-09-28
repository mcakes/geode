//! Operational state shared by the status bar and diagnostics tiles. The app
//! bridge supplies source and catalog events; config load/reload supplies its
//! own diagnostic batches. [`Diagnostics::summary`] caches the status text.
//!
//! This model performs no I/O or clock reads. Callers supply timestamps and
//! notify the GPUI entity after mutations; these methods have no `Context`.
//!
//! Diagnostic data changes advance the combined version and the affected
//! [`DiagVersions`] counters. Repeated equal snapshots are no-ops; loading and
//! publication events always advance their counters. Catalog demand and watcher
//! counts do not change diagnostic data versions, but still require caller
//! notification so the bridge can submit queued work.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::SystemTime;

use geode_core::config::{Diagnostic, Severity};
pub use geode_core::health::Health;
use geode_core::log::{Level, LogLevels};
use geode_core::query::{CatalogSnapshot, DatasetCatalog};
pub use geode_core::source_config::SourceShape;
use gpui::SharedString;

use crate::perf::FrameHistogram;

/// The request loop's thread name as the data layer spawns it. The shell
/// cannot depend on the data crate, so it is repeated here.
const REQUEST_LOOP: &str = "geode-data";

/// A data thread that died despite containment. It stays dead until the app
/// restarts, so nothing clears it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoppedThread {
    /// The spawn name the data layer reported.
    pub thread: String,
    /// What the status bar and the diagnostics tile call it.
    pub label: String,
    pub reason: String,
    pub at: SystemTime,
}

/// The status bar's stopped segment, prepared when a thread stops so paint
/// formats nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct StoppedSegment {
    pub text: SharedString,
    pub detail: SharedString,
}

/// A readable name for a data thread's spawn name.
pub fn thread_label(thread: &str) -> String {
    if let Some(n) = thread.strip_prefix("geode-query-") {
        return format!("query worker {n}");
    }
    if let Some(source) = thread.strip_prefix("geode-fetch-") {
        return format!("fetch {source}");
    }
    if let Some(source) = thread.strip_prefix("geode-subscribe-") {
        return format!("subscription {source}");
    }
    if let Some(target) = thread.strip_prefix("geode-egress-") {
        return format!("egress {target}");
    }
    match thread {
        REQUEST_LOOP => "data service",
        "geode-ingest" => "ingest",
        "geode-discovery" => "discovery",
        "geode-pricing" => "pricing",
        "geode-vol" => "vol model",
        other => other,
    }
    .to_string()
}

/// One segment for every stopped thread. The request loop outranks the
/// rest: once it is gone, nothing else the bar says describes a live
/// service. Two or more other threads collapse to a count.
fn stopped_segment(stopped: &[StoppedThread]) -> Option<StoppedSegment> {
    let first = stopped.first()?;
    if let Some(service) = stopped.iter().find(|t| t.thread == REQUEST_LOOP) {
        let others: Vec<&str> = stopped
            .iter()
            .filter(|t| t.thread != REQUEST_LOOP)
            .map(|t| t.label.as_str())
            .collect();
        let mut detail = service.reason.clone();
        if !others.is_empty() {
            detail.push_str(&format!("; also stopped: {}", others.join(", ")));
        }
        return Some(StoppedSegment {
            text: SharedString::new_static("data service stopped — restart Geode"),
            detail: detail.into(),
        });
    }
    if stopped.len() == 1 {
        return Some(StoppedSegment {
            text: format!("{} stopped", first.label).into(),
            detail: first.reason.clone().into(),
        });
    }
    let detail = stopped
        .iter()
        .map(|t| format!("{}: {}", t.label, t.reason))
        .collect::<Vec<_>>()
        .join("; ");
    Some(StoppedSegment {
        text: format!("{} data threads stopped", stopped.len()).into(),
        detail: detail.into(),
    })
}

/// Why the bridge should read the catalog. Explicit requests (for example an
/// identity picker) remain valid without a visible diagnostics tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogRequest {
    Watched,
    Explicit,
}

/// A source description prepared by the app bridge from its `SourceSpec`.
/// Priority and readiness are display strings so the shell can describe data
/// sources without depending on `geode-data` types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSummary {
    pub paths: Vec<String>,
    pub priority: String,
    pub readiness: String,
    /// Adapter name, such as `CSV_DIR_ADAPTER` or a broker implementation.
    /// Use `shape` to choose source-specific presentation.
    pub adapter: String,
    /// The topic patterns a subscribed source subscribes to; empty for a
    /// directory or a fetch source. Never how a reader tells the shapes
    /// apart — `shape` below is (a subscribed source is refused at load
    /// without at least one topic, but a fetch source has none by design).
    pub topics: Vec<String>,
    /// Directory, subscribed, or fetch pipeline. The app bridge resolves this
    /// from `SourceSpec` and the dataset family; adapter names and empty topic
    /// lists alone cannot distinguish all three shapes.
    pub shape: SourceShape,
}

/// How many transitions [`SourceState::history`] keeps, newest last.
pub const SOURCE_HISTORY_CAP: usize = 16;

/// One source's live state: its static description (once known), its
/// current health and how long it has held it, its last/next poll, and
/// a bounded transition history.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceState {
    pub spec: Option<SourceSummary>,
    /// Absent until `note_health` supplies a report. Describing or polling a
    /// source does not establish its health. The summary excludes unreported
    /// sources, while the sources section displays "no report yet".
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

/// Current ingest activity, set by `DataEvent::Loading` and cleared by
/// `DataEvent::LoadEnded`. The label is formatted once per event so the status
/// bar can share it on each paint. Files skipped as already loaded emit no
/// start/end pair.
#[derive(Debug, Clone)]
pub struct IngestActivity {
    pub source: String,
    pub path: String,
    pub queued: usize,
    pub since: SystemTime,
    pub label: gpui::SharedString,
}

/// A dataset's catalog snapshot, absent until a catalog outcome includes it
/// or when a newer outcome omits it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatasetState {
    pub catalog: Option<DatasetCatalog>,
}

/// How many batches [`Diagnostics::config_history`] keeps, latest first.
pub const CONFIG_HISTORY_CAP: usize = 16;

/// How many entries [`Diagnostics::data_diagnostics`] keeps, oldest
/// first (a plain append cap, not "batches" — see that field's doc).
pub const DATA_DIAGNOSTICS_CAP: usize = 256;

/// Section-specific change counters alongside [`Diagnostics::version`], which
/// invalidates the shared status summary. A tile compares only the counter for
/// its selected section:
///
/// - `sources`: source descriptions, health, polls, and ingest activity.
/// - `data`: publications and catalog snapshots.
/// - `config`: current config diagnostics, their history, and data diagnostics.
/// - `log_levels`: target-level settings.
/// - `perf`: the copied frame histogram and dropped-event count.
///
/// Frame as-of/config versions and the log ring sequence are separate inputs
/// observed by the tile. Performance rows also read frame requery statistics
/// and catalog resource metrics, but those inputs have no dedicated perf
/// invalidation here; they appear on the next perf-section rebuild. Log-level
/// changes trigger a rebuild, but row substring filtering is tile-local.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagVersions {
    pub sources: u64,
    pub data: u64,
    pub config: u64,
    pub log_levels: u64,
    pub perf: u64,
}

/// Shared operational state. Mutators maintain the combined and section
/// versions described in the module documentation.
pub struct Diagnostics {
    pub sources: BTreeMap<String, SourceState>,
    pub datasets: BTreeMap<String, DatasetState>,
    /// What the ingest runner is loading right now, or `None` while
    /// idle — set by [`Self::note_loading`], cleared by
    /// [`Self::note_load_ended`]. The status bar and the sources
    /// section both read this.
    pub ingest: Option<IngestActivity>,
    /// The latest config-load diagnostic batch. `note_config` replaces it,
    /// including clearing it after a clean reload. This population is separate
    /// from `data_diagnostics` so a reload cannot erase a data-layer condition.
    /// The status summary counts errors from both populations independently.
    pub config: Vec<Diagnostic>,
    /// Changed config-load batches, newest first, capped at
    /// [`CONFIG_HISTORY_CAP`]. Includes the current batch; identical reloads
    /// add nothing. History is excluded from the status summary error count.
    pub config_history: VecDeque<(SystemTime, Vec<Diagnostic>)>,
    /// Data-layer conditions from the app bridge, oldest first, capped at
    /// [`DATA_DIAGNOSTICS_CAP`]. Data events report individual conditions, not
    /// complete snapshots, so new entries append and equal retained entries
    /// are skipped. A config reload cannot clear these conditions.
    pub data_diagnostics: VecDeque<(SystemTime, Diagnostic)>,
    pub dropped_events: u64,
    /// Submissions the data handle refused because its queue was full, since
    /// launch (the handle's own counter, read by the bridge each drain).
    pub refused: u64,
    /// Data threads that died, in the order they were reported.
    pub stopped: Vec<StoppedThread>,
    /// The status segment for `stopped`, rebuilt when a thread stops.
    stopped_segment: Option<StoppedSegment>,
    pub restart_required: Option<String>,
    /// A copy of `ShellView::perf`, refreshed by [`Self::refresh_frame_hist`]
    /// on the reload-poll tick — see that method's own doc comment for
    /// why this is a copy rather than the histogram itself.
    pub frame_hist: FrameHistogram,
    /// The latest catalog outcome, including its as-of and resource metrics.
    pub catalog: Option<CatalogSnapshot>,
    pub levels: LogLevels,
    /// Visible diagnostics tile count, maintained by `watch`/`unwatch`.
    /// Watched catalog refreshes and histogram copies require at least one
    /// watcher; explicit catalog consumers are independent.
    watchers: u32,
    version: u64,
    /// Section counters; see [`DiagVersions`].
    versions: DiagVersions,
    pending_level: Option<(String, Level)>,
    pending_overlay_toggle: bool,
    pending_catalog_request: bool,
    pending_explicit_catalog: bool,
    /// Status summary cached by combined version. A cache hit shares the
    /// `Rc<str>` allocation, avoiding formatting and buffer copies during paint.
    summary_cache: RefCell<(u64, Rc<str>)>,
}

impl Diagnostics {
    pub fn new(levels: LogLevels) -> Diagnostics {
        Diagnostics {
            sources: BTreeMap::new(),
            datasets: BTreeMap::new(),
            ingest: None,
            config: Vec::new(),
            config_history: VecDeque::new(),
            data_diagnostics: VecDeque::new(),
            dropped_events: 0,
            refused: 0,
            stopped: Vec::new(),
            stopped_segment: None,
            restart_required: None,
            frame_hist: FrameHistogram::new(),
            catalog: None,
            levels,
            watchers: 0,
            version: 0,
            versions: DiagVersions::default(),
            pending_level: None,
            pending_overlay_toggle: false,
            pending_catalog_request: false,
            pending_explicit_catalog: false,
            summary_cache: RefCell::new((u64::MAX, Rc::from(""))),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Copy the section counters for comparison in an observer.
    pub fn versions(&self) -> DiagVersions {
        self.versions
    }

    /// Record a source description. An identical description leaves versions
    /// unchanged, including when the bridge describes a source again.
    pub fn describe_source(&mut self, source: &str, summary: SourceSummary) {
        let state = self.sources.entry(source.to_string()).or_default();
        if state.spec.as_ref() == Some(&summary) {
            return;
        }
        state.spec = Some(summary);
        self.version += 1;
        self.versions.sources += 1;
    }

    /// Record the source's current worst health and detail. Its first health
    /// report starts the transition history even if description or poll events
    /// already created the map entry. Repeating the same health and detail is
    /// a no-op; a changed detail or recovery records a new transition.
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
        self.versions.sources += 1;
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
        self.versions.sources += 1;
    }

    /// A load began (`DataEvent::Loading`). Always bumps: a new `Started`
    /// is a new record even for the same source (its path or depth moved).
    pub fn note_loading(&mut self, source: &str, path: &str, queued: usize, at: SystemTime) {
        let label: gpui::SharedString = if queued == 0 {
            format!("loading {source}").into()
        } else {
            format!("loading {source} · {queued} queued").into()
        };
        self.ingest = Some(IngestActivity {
            source: source.to_string(),
            path: path.to_string(),
            queued,
            since: at,
            label,
        });
        self.version += 1;
        self.versions.sources += 1;
    }

    /// Clear the active load. The runner sends an end event after load outcomes
    /// and at queue drain, allowing a missed end event to be repaired. Repeated
    /// end events while idle leave versions unchanged.
    pub fn note_load_ended(&mut self) {
        if self.ingest.take().is_some() {
            self.version += 1;
            self.versions.sources += 1;
        }
    }

    /// Record a publication: retain the dataset name and always advance the data
    /// version. Queue a catalog refresh only while diagnostics is watched; a
    /// hidden surface does not need a database read for each publication.
    pub fn note_published(&mut self, dataset: &str) {
        self.datasets.entry(dataset.to_string()).or_default();
        if self.watchers > 0 {
            self.pending_catalog_request = true;
        }
        self.version += 1;
        self.versions.data += 1;
    }

    /// Replace the current config-load batch and retain the changed batch in
    /// `config_history`. An equal batch leaves versions and history unchanged.
    /// An empty batch clears standing config errors after a clean reload.
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
        self.versions.config += 1;
    }

    /// Append data-layer conditions, deduplicating against retained entries.
    /// Each event reports individual conditions rather than a replacement batch.
    /// Drop the oldest entries above `DATA_DIAGNOSTICS_CAP`; bump versions only
    /// when a new condition is appended.
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
            // `config`, not `data` — despite this field's name, it is
            // `sections::config_rows` that renders `data_diagnostics`
            // (see `DiagVersions`'s own doc for the full mapping).
            self.versions.config += 1;
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
        // `sections::perf_rows` is the section that renders
        // `dropped_events`.
        self.versions.perf += 1;
    }

    /// Record a data thread's death. A thread already recorded is a no-op:
    /// each dies once, and a redelivered event must not duplicate it.
    pub fn note_thread_stopped(&mut self, thread: &str, reason: String, at: SystemTime) {
        if self.stopped.iter().any(|t| t.thread == thread) {
            return;
        }
        self.stopped.push(StoppedThread {
            thread: thread.to_string(),
            label: thread_label(thread),
            reason,
            at,
        });
        self.stopped_segment = stopped_segment(&self.stopped);
        self.version += 1;
        // `sections::sources_rows` renders the stopped threads.
        self.versions.sources += 1;
    }

    /// The data handle's running total of `Busy` refusals. The same total
    /// again does not bump.
    pub fn note_refused(&mut self, total: u64) {
        if self.refused == total {
            return;
        }
        self.refused = total;
        self.version += 1;
    }

    /// The prepared stopped segment, `None` while every data thread lives.
    pub fn stopped_segment(&self) -> Option<&StoppedSegment> {
        self.stopped_segment.as_ref()
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

    /// Store a changed catalog snapshot and refresh the per-dataset slices.
    /// Dataset names remain in the map, but a dataset omitted from the new
    /// snapshot has its catalog cleared so stale generations cannot linger.
    /// An equal snapshot leaves versions unchanged.
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
        self.versions.data += 1;
    }

    /// Copy the frame histogram only while watched and when its count or maximum
    /// changes. Return `true` and advance the perf version only after copying;
    /// an idle poll must not notify merely because it checked the histogram.
    ///
    /// This is a change proxy, not a full histogram comparison. Changes solely
    /// to discarded-idle counts are not copied, and a reset followed by new
    /// samples with the same count and maximum can also go undetected.
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
        self.versions.perf += 1;
        true
    }

    /// Register a visible tile and queue its initial catalog refresh, returning
    /// `true`. This changes demand, not diagnostic data, so versions stay put.
    /// The caller must `cx.notify()` in the same entity update; otherwise the
    /// bridge cannot observe the queued request until another mutation notifies.
    /// Each visibility transition must have a matching `unwatch`.
    pub fn watch(&mut self) -> bool {
        self.watchers += 1;
        self.pending_catalog_request = true;
        true
    }

    /// Remove a visible tile's watch, saturating at zero. The last watcher
    /// clears pending watched demand, while explicit consumers retain their
    /// requests. The caller must notify observers after the visibility change.
    pub fn unwatch(&mut self) {
        self.watchers = self.watchers.saturating_sub(1);
        if self.watchers == 0 {
            self.pending_catalog_request = false;
        }
    }

    pub fn watchers(&self) -> u32 {
        self.watchers
    }

    /// Queue an explicit catalog read, even without visible diagnostics (for
    /// example, the timeseries identity picker). It survives diagnostics hiding.
    /// The bridge reads the current frame as-of when it submits the request.
    /// This does not bump `version`; callers must notify in the same update to
    /// wake the bridge. Diagnostics itself uses `request_catalog_refresh`.
    pub fn request_catalog(&mut self) {
        self.pending_explicit_catalog = true;
    }

    /// Queue a refresh for visible diagnostics, cancelled by the last unwatch.
    /// As with `request_catalog`, the caller must notify to wake the bridge.
    pub fn request_catalog_refresh(&mut self) {
        self.pending_catalog_request |= self.watchers > 0;
    }

    /// Apply one target-level setting and queue its persistence for
    /// [`Self::take_pending_level`]. Repeating the same target and level is a
    /// no-op. The palette uses this path; disk reload uses `set_levels` instead
    /// so reading a saved setting does not write it again.
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
        self.versions.log_levels += 1;
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
        self.versions.log_levels += 1;
        true
    }

    /// Queue a toggle for the shell to drain. Command-locality tests observe
    /// this seam to detect forbidden tile-wide effects. Production overlay
    /// actions toggle `ShellView::perf_overlay` directly.
    pub fn request_overlay_toggle(&mut self) {
        self.pending_overlay_toggle = true;
        self.version += 1;
    }

    pub fn take_pending_overlay_toggle(&mut self) -> bool {
        std::mem::take(&mut self.pending_overlay_toggle)
    }

    /// Consume queued demand, preserving explicit consumers when diagnostics hides.
    pub fn take_catalog_request(&mut self) -> Option<CatalogRequest> {
        let watched = std::mem::take(&mut self.pending_catalog_request);
        let explicit = std::mem::take(&mut self.pending_explicit_catalog);
        if explicit {
            Some(CatalogRequest::Explicit)
        } else if watched {
            Some(CatalogRequest::Watched)
        } else {
            None
        }
    }

    /// Consume queued demand without retaining its retry/visibility policy.
    /// The bridge uses `take_catalog_request`; this is the test drain seam.
    #[cfg(any(test, feature = "test-support"))]
    pub fn take_pending_catalog_request(&mut self) -> bool {
        self.take_catalog_request().is_some()
    }

    /// Whether either a watched refresh or an explicit request remains queued.
    pub fn pending_catalog_request(&self) -> bool {
        self.pending_catalog_request || self.pending_explicit_catalog
    }

    /// Status summary, with optional source-health, current config-error,
    /// retained data-error, dropped-event, and refused-submission segments.
    /// Stopped data threads have their own segment ([`Self::stopped_segment`]). Zero counts are omitted;
    /// an empty string means there is nothing to show. A cache hit shares the
    /// existing `Rc<str>` buffer.
    ///
    /// Only sources with a health report are counted. Config history contributes
    /// no errors; data conditions are counted separately so config reloads
    /// cannot hide them. Restart-required text has its own status-bar segment.
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
                continue; // Unreported sources have no health to count.
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

        // Count retained data errors independently of the current config-load batch.
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
        if self.refused > 0 {
            parts.push(format!("{} refused", self.refused));
        }

        parts.join(" · ")
    }
}

/// The last 32 dispatched action IDs as FNV-1a hashes, for crash reporting.
/// Fixed storage and hashing avoid allocating an action string per dispatch.
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

    /// A recovery report replaces degraded health with `Ok`. This model stores
    /// current health, not the worst state ever seen.
    #[test]
    fn a_degraded_source_that_recovers_shows_ok_in_the_entity() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::UNIX_EPOCH;
        d.note_health("risk", Health::PendingTooLong, "BK000".into(), t);
        assert_eq!(d.sources["risk"].health, Some(Health::PendingTooLong));
        d.note_health("risk", Health::Ok, "".into(), t + Duration::from_secs(120));
        assert_eq!(
            d.sources["risk"].health,
            Some(Health::Ok),
            "a recovered source must read Ok, not stay latched as degraded"
        );
        assert_eq!(d.summary().as_ref(), "sources 1 ok");
    }

    /// A source described or polled before its first health event still needs
    /// that event to set `since` and begin the transition history.
    #[test]
    fn the_first_real_health_note_transitions_even_after_describe_source_and_note_polled() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec![],
                priority: "".into(),
                readiness: "".into(),
                adapter: "csv_dir".into(),
                topics: Vec::new(),
                shape: SourceShape::Directory,
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

    /// A configured source without a health report is excluded from the summary.
    #[test]
    fn a_described_but_unreported_source_is_not_counted_in_the_summary() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec!["/data/*.csv".into()],
                priority: "latest_risk".into(),
                readiness: "sentinel".into(),
                adapter: "csv_dir".into(),
                topics: Vec::new(),
                shape: SourceShape::Directory,
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

    /// Becoming visible requests a catalog without waiting for a publication.
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

    /// An explicit catalog request changes demand without changing diagnostic
    /// data versions. Its caller still must notify the bridge.
    #[test]
    fn request_catalog_queues_without_bumping_the_version() {
        let mut d = Diagnostics::new(LogLevels::default());
        let v = d.version();
        d.request_catalog();
        assert_eq!(d.version(), v, "no diagnostic data changed");
        assert!(d.take_pending_catalog_request());
        assert!(!d.take_pending_catalog_request(), "drained");
    }

    /// The last watcher leaving clears pending watched demand.
    #[test]
    fn unwatch_to_zero_clears_a_pending_catalog_request() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.watch();
        d.unwatch();
        assert!(!d.take_pending_catalog_request());
    }

    /// Watched demand stays queued while another tile is still watching.
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

    /// An unchanged histogram must not copy or bump during the periodic poll.
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

    /// An identical target-level request must not queue another write.
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

    // --- Log, status, and diagnostic lifecycle coverage ----------------

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
                adapter: "csv_dir".into(),
                topics: Vec::new(),
                shape: SourceShape::Directory,
            },
        );
        assert!(d.version() > v0);
        assert_eq!(
            d.sources["risk"].spec.as_ref().unwrap().priority,
            "latest_risk"
        );
    }

    /// An identical source description must not advance versions.
    #[test]
    fn describe_source_is_a_no_op_for_an_identical_summary() {
        let mut d = Diagnostics::new(LogLevels::default());
        let summary = SourceSummary {
            paths: vec!["/x".into()],
            priority: "p".into(),
            readiness: "r".into(),
            adapter: "csv_dir".into(),
            topics: Vec::new(),
            shape: SourceShape::Directory,
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

    /// A dataset omitted from a newer snapshot must lose its stale catalog.
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

    /// Restart-required text belongs to its own status segment and must not
    /// also appear in the diagnostics summary.
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

    /// A summary cache hit preserves allocation identity across calls without
    /// a mutation; equal text alone would not prove that no copy was made.
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

    // Config batch replacement and history.

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

    /// An empty config batch clears errors from the preceding load.
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

    // Config-load snapshots and data-layer conditions remain independent.

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

    /// An unrelated config reload must preserve a retained data-layer error.
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

    #[test]
    fn note_loading_records_the_activity_and_bumps_the_sources_version() {
        let mut d = Diagnostics::new(LogLevels::default());
        let v = d.versions().sources;
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        d.note_loading("risk", "/data/risk/EOD.csv", 3, at);
        let a = d.ingest.as_ref().expect("recorded");
        assert_eq!(a.source, "risk");
        assert_eq!(a.path, "/data/risk/EOD.csv");
        assert_eq!(a.queued, 3);
        assert_eq!(a.since, at);
        assert_eq!(&*a.label, "loading risk · 3 queued");
        assert_eq!(d.versions().sources, v + 1);
        d.note_loading("cvi", "document://cvi/cvi_params", 0, at);
        assert_eq!(&*d.ingest.as_ref().unwrap().label, "loading cvi");
    }

    #[test]
    fn note_load_ended_clears_and_is_a_no_op_when_idle() {
        let mut d = Diagnostics::new(LogLevels::default());
        let v0 = d.versions().sources;
        d.note_load_ended();
        assert!(d.ingest.is_none());
        assert_eq!(d.versions().sources, v0, "nothing to clear, nothing bumps");
        d.note_loading("risk", "/x.csv", 0, SystemTime::UNIX_EPOCH);
        let v1 = d.versions().sources;
        d.note_load_ended();
        assert!(d.ingest.is_none());
        assert_eq!(d.versions().sources, v1 + 1);
    }

    #[test]
    fn one_stopped_thread_is_named_with_its_reason_in_the_tooltip() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::UNIX_EPOCH);
        let seg = d.stopped_segment().expect("a segment");
        assert_eq!(seg.text.as_ref(), "ingest stopped");
        assert_eq!(seg.detail.as_ref(), "boom");
        assert_eq!(d.stopped[0].label, "ingest");
    }

    #[test]
    fn two_stopped_threads_collapse_to_a_count_listing_each() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_thread_stopped("geode-discovery", "a".into(), SystemTime::UNIX_EPOCH);
        d.note_thread_stopped("geode-query-2", "b".into(), SystemTime::UNIX_EPOCH);
        let seg = d.stopped_segment().unwrap();
        assert_eq!(seg.text.as_ref(), "2 data threads stopped");
        assert_eq!(seg.detail.as_ref(), "discovery: a; query worker 2: b");
    }

    #[test]
    fn a_stopped_request_loop_outranks_the_other_threads() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_thread_stopped("geode-ingest", "a".into(), SystemTime::UNIX_EPOCH);
        d.note_thread_stopped("geode-data", "the loop died".into(), SystemTime::UNIX_EPOCH);
        let seg = d.stopped_segment().unwrap();
        assert_eq!(seg.text.as_ref(), "data service stopped — restart Geode");
        assert_eq!(seg.detail.as_ref(), "the loop died; also stopped: ingest");
    }

    #[test]
    fn a_thread_reported_twice_is_recorded_once() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::UNIX_EPOCH);
        let version = d.version();
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::UNIX_EPOCH);
        assert_eq!(d.stopped.len(), 1);
        assert_eq!(d.version(), version, "a repeat changes nothing");
    }

    #[test]
    fn refused_submissions_show_in_the_summary_and_are_omitted_at_zero() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_refused(0);
        assert_eq!(d.summary().as_ref(), "");
        d.note_refused(3);
        d.note_dropped(2);
        assert_eq!(d.summary().as_ref(), "2 dropped · 3 refused");
    }

    #[test]
    fn thread_labels_are_readable() {
        for (thread, label) in [
            ("geode-data", "data service"),
            ("geode-ingest", "ingest"),
            ("geode-discovery", "discovery"),
            ("geode-pricing", "pricing"),
            ("geode-vol", "vol model"),
            ("geode-query-0", "query worker 0"),
            ("geode-fetch-kdb", "fetch kdb"),
            ("geode-subscribe-cvi", "subscription cvi"),
            ("geode-egress-sophis", "egress sophis"),
            ("something-else", "something-else"),
        ] {
            assert_eq!(thread_label(thread), label);
        }
    }
}
