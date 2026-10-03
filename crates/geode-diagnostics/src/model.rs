//! Typed row models per section, built from the shell's `Diagnostics`
//! entity, the loaded `Config`, the log tail, and the frame's requery stats.
//! Pure: explicit `now` and clock inputs, no GPUI, no I/O.

use std::time::SystemTime;

use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use geode_core::config::{Config, Diagnostic, Severity};
use geode_core::log::{Level, Record};
use geode_core::query::AsOf;
use geode_shell::diagnostics::{Diagnostics, Health, SourceShape, SourceState};
use geode_shell::perf::{BUCKET_UPPER_BOUNDS_MICROS, FrameHistogram, RequeryStats, format_ms};

use crate::log::LogFilter;

/// A row's visual weight; the page maps it to theme paint. `Marked`
/// highlights the generation resolved under a historical as-of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Muted,
    Warn,
    Error,
    Marked,
}

pub fn health_tone(health: Option<&Health>) -> Tone {
    match health {
        None => Tone::Muted,
        Some(Health::Ok) => Tone::Normal,
        Some(Health::Pending | Health::PendingTooLong) => Tone::Muted,
        Some(Health::Degraded { .. }) => Tone::Warn,
        Some(Health::Failed { .. }) => Tone::Error,
    }
}

/// The health column's spelling: a title-case word, unlike the snake-case
/// `Health::label` the status summary and the session file use. The
/// history line in the detail strip spells its entries the same way.
pub fn health_title(health: &Health) -> &'static str {
    match health {
        Health::Ok => "Ok",
        Health::Pending => "Pending",
        Health::PendingTooLong => "Pending too long",
        Health::Degraded { .. } => "Degraded",
        Health::Failed { .. } => "Failed",
    }
}

/// The worst reported health by [`Health::severity`]. Among equals the first
/// by source name wins, so the returned reason is deterministic.
fn worst_health(d: &Diagnostics) -> Option<&Health> {
    d.sources
        .values()
        .filter_map(|s| s.health.as_ref())
        .fold(None, |worst: Option<&Health>, h| match worst {
            Some(w) if w.severity() >= h.severity() => Some(w),
            _ => Some(h),
        })
}

/// Format `HH:MM:SS` using the supplied display clock, shared with the
/// frame's time readouts. The page supplies the configured `AppClock`.
fn local_hms(t: SystemTime, clock: Clock) -> String {
    clock.hms(DateTime::<Utc>::from(t))
}

fn local_hms_utc(t: DateTime<Utc>, clock: Clock) -> String {
    clock.hms(t)
}

fn local_hms_millis(t: SystemTime, clock: Clock) -> String {
    let dt = DateTime::<Utc>::from(t);
    format!("{}.{:03}", clock.hms(dt), dt.timestamp_subsec_millis())
}

// ------------------------------------------------------------ Sources

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub name: String,
    pub tone: Tone,
    /// "Ok", "Degraded — reason", "no report yet".
    pub health: String,
    pub since: Option<SystemTime>,
    pub since_hms: String,
    pub shape: String,
    pub last_poll: String,
    pub next_poll: String,
    pub ready: String,
    pub loading: String,
    /// Detail-strip lines: the spec detail by shape.
    pub detail: Vec<String>,
    /// Oldest first, as the entity keeps it.
    pub history: Vec<(String, Health)>,
}

/// Worst health first by variant rank, then name; unreported sources last
/// by name. Not `Health`'s derived `Ord`: that falls through to reason text
/// before the name, so two failed sources would order by their reasons.
///
/// A source the ingest runner is loading but nothing has described or
/// reported yet still gets a row: the load is the first thing known about
/// it, and the row is where that shows.
pub fn source_rows(d: &Diagnostics, clock: Clock) -> Vec<SourceRow> {
    let mut reported: Vec<(&str, &SourceState)> = d
        .sources
        .iter()
        .filter(|(_, s)| s.health.is_some())
        .map(|(name, s)| (name.as_str(), s))
        .collect();
    let rank = |s: &SourceState| s.health.as_ref().map(Health::severity).unwrap_or(0);
    reported.sort_by(|a, b| rank(b.1).cmp(&rank(a.1)).then_with(|| a.0.cmp(b.0)));
    let mut unreported: Vec<(&str, &SourceState)> = d
        .sources
        .iter()
        .filter(|(_, s)| s.health.is_none())
        .map(|(name, s)| (name.as_str(), s))
        .collect();
    let unknown = SourceState::default();
    if let Some(a) = &d.ingest
        && !d.sources.contains_key(&a.source)
    {
        unreported.push((a.source.as_str(), &unknown));
    }
    unreported.sort_by(|a, b| a.0.cmp(b.0));

    reported
        .into_iter()
        .chain(unreported)
        .map(|(name, state)| source_row(d, name, state, clock))
        .collect()
}

fn source_row(d: &Diagnostics, name: &str, state: &SourceState, clock: Clock) -> SourceRow {
    // An ingest `Started` can land before the source's first health
    // note, so the loading text is built for reported and
    // unreported sources alike.
    let loading = d
        .ingest
        .as_ref()
        .filter(|a| a.source == name)
        .map(|a| format!("{} since {}", a.path, local_hms(a.since, clock)))
        .unwrap_or_default();
    let (health, since, since_hms) = match &state.health {
        None => ("no report yet".to_string(), None, String::new()),
        Some(h) => {
            let label = health_title(h);
            let text = match h {
                Health::Degraded { reason } | Health::Failed { reason } if !reason.is_empty() => {
                    format!("{label} — {reason}")
                }
                _ => label.to_string(),
            };
            (text, Some(state.since), local_hms(state.since, clock))
        }
    };
    // The bridge resolves `SourceSummary::shape` from the dataset
    // family: neither the adapter name nor an empty topic list can
    // tell the shapes apart, so a fetch source is its adapter plus
    // `fetch`, never an empty `topics:`.
    let (shape, detail) = match &state.spec {
        None => (String::new(), Vec::new()),
        Some(spec) => match spec.shape {
            SourceShape::Directory => (
                "directory".to_string(),
                vec![
                    format!("path: {}", spec.paths.join(", ")),
                    format!(
                        "adapter: {} · priority: {} · readiness: {}",
                        spec.adapter, spec.priority, spec.readiness
                    ),
                ],
            ),
            SourceShape::Fetch => (
                "fetch".to_string(),
                vec![format!("adapter: {}", spec.adapter), "fetch".to_string()],
            ),
            SourceShape::Snapshot => (
                "snapshot".to_string(),
                vec![
                    format!("adapter: {}", spec.adapter),
                    format!("priority: {}", spec.priority),
                ],
            ),
            SourceShape::Subscribed => (
                format!("subscribed · {} topics", spec.topics.len()),
                vec![
                    format!("adapter: {}", spec.adapter),
                    format!("topics: {}", spec.topics.join(", ")),
                ],
            ),
        },
    };
    let polled = state.last_poll.is_some() || state.next_poll.is_some();
    SourceRow {
        name: name.to_string(),
        tone: health_tone(state.health.as_ref()),
        health,
        since,
        since_hms,
        shape,
        last_poll: state
            .last_poll
            .map(|t| local_hms(t, clock))
            .unwrap_or_default(),
        next_poll: state
            .next_poll
            .map(|t| local_hms(t, clock))
            .unwrap_or_default(),
        ready: if polled {
            state.last_ready.to_string()
        } else {
            String::new()
        },
        loading,
        detail,
        history: state
            .history
            .iter()
            .map(|(at, h)| (local_hms(*at, clock), h.clone()))
            .collect(),
    }
}

/// "4 m", "12 s", "1 h 3 m"; empty when unknown.
pub fn age_text(since: Option<SystemTime>, now: SystemTime) -> String {
    let Some(since) = since else {
        return String::new();
    };
    let secs = now.duration_since(since).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3_600 => format!("{} m", s / 60),
        s => format!("{} h {} m", s / 3_600, (s % 3_600) / 60),
    }
}

// ------------------------------------------------------------ Data

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionRow {
    /// "2026-09-27 · EU_TECH" or "2026-09-27 · (bookless)".
    pub label: String,
    pub gen_id: String,
    pub source_time: String,
    pub loaded: String,
    pub rows: String,
    pub kind: &'static str,
    /// The generation `resolve_generations` chose for the frame's as-of,
    /// shown only when the catalog was answered for that as-of.
    pub marked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetRow {
    pub name: String,
    pub has_catalog: bool,
    pub live_rows: String,
    pub archive_rows: String,
    pub partitions: usize,
    pub latest_gen: String,
    pub published: String,
    pub resolved: String,
    pub children: Vec<PartitionRow>,
}

/// Per-dataset catalog slices share the snapshot's as-of. After a frame
/// as-of change, the resolved marker hides until a matching catalog arrives
/// so the displayed generation cannot imply a different historical instant.
pub fn catalog_matches_frame(d: &Diagnostics, as_of: &AsOf) -> bool {
    d.catalog.as_ref().is_some_and(|c| &c.as_of == as_of)
}

pub fn dataset_rows(d: &Diagnostics, as_of: &AsOf, clock: Clock) -> Vec<DatasetRow> {
    let matches = catalog_matches_frame(d, as_of);
    d.datasets
        .iter()
        .map(|(name, state)| {
            let Some(catalog) = &state.catalog else {
                return DatasetRow {
                    name: name.clone(),
                    has_catalog: false,
                    live_rows: String::new(),
                    archive_rows: String::new(),
                    partitions: 0,
                    latest_gen: String::new(),
                    published: String::new(),
                    resolved: String::new(),
                    children: Vec::new(),
                };
            };
            let mut children = Vec::new();
            let mut latest: Option<(i64, DateTime<Utc>)> = None;
            let mut resolved = Vec::new();
            for part in &catalog.partitions {
                let book = part.book.as_deref().unwrap_or("(bookless)");
                for g in &part.generations {
                    let marked = !as_of.is_live() && matches && part.resolved_gen == Some(g.gen_id);
                    if marked {
                        resolved.push(g.gen_id.to_string());
                    }
                    if g.live && latest.is_none_or(|(_, t)| g.source_time > t) {
                        latest = Some((g.gen_id, g.source_time));
                    }
                    children.push(PartitionRow {
                        label: format!("{} · {book}", part.batch),
                        gen_id: g.gen_id.to_string(),
                        source_time: local_hms_utc(g.source_time, clock),
                        loaded: g
                            .loaded_at
                            .map(|t| local_hms_utc(t, clock))
                            .unwrap_or_else(|| "?".into()),
                        rows: g
                            .file_rows
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".into()),
                        kind: if g.live { "live" } else { "archive" },
                        marked,
                    });
                }
            }
            DatasetRow {
                name: name.clone(),
                has_catalog: true,
                live_rows: catalog.live_rows.to_string(),
                archive_rows: catalog.archive_rows.to_string(),
                partitions: catalog.partitions.len(),
                latest_gen: latest.map(|(g, _)| g.to_string()).unwrap_or_default(),
                published: latest
                    .map(|(_, t)| local_hms_utc(t, clock))
                    .unwrap_or_default(),
                resolved: resolved.join(", "),
                children,
            }
        })
        .collect()
}

// ------------------------------------------------------------ Config

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Config,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRow {
    pub severity: Severity,
    pub lane: Lane,
    /// History rows carry their batch time; current rows none.
    pub batch: Option<String>,
    /// "file › path" when the diagnostic carries them; else its layer or "".
    pub location: String,
    pub message: String,
    /// `Diagnostic`'s own Display, for the detail strip.
    pub full: String,
}

fn diagnostic_row(diag: &Diagnostic, lane: Lane, batch: Option<String>) -> DiagnosticRow {
    let file = diag
        .file
        .as_ref()
        .and_then(|f| f.file_name())
        .map(|f| f.to_string_lossy().to_string());
    let location = match (file, &diag.path) {
        (Some(f), Some(p)) => format!("{f} › {p}"),
        (Some(f), None) => f,
        (None, Some(p)) => p.clone(),
        (None, None) => diag.layer.map(|l| l.name().to_string()).unwrap_or_default(),
    };
    DiagnosticRow {
        severity: diag.severity,
        lane,
        batch,
        location,
        message: diag.message.clone(),
        full: diag.to_string(),
    }
}

/// The current config batch, then retained data conditions.
pub fn current_diagnostics(d: &Diagnostics) -> Vec<DiagnosticRow> {
    d.config
        .iter()
        .map(|x| diagnostic_row(x, Lane::Config, None))
        .chain(
            d.data_diagnostics
                .iter()
                .map(|(_, x)| diagnostic_row(x, Lane::Data, None)),
        )
        .collect()
}

/// Prior batches, newest first, each row tagged with its batch time. The
/// current batch (index 0) is excluded.
pub fn history_diagnostics(d: &Diagnostics, clock: Clock) -> Vec<DiagnosticRow> {
    d.config_history
        .iter()
        .skip(1)
        .flat_map(|(at, diags)| {
            let batch = local_hms(*at, clock);
            diags
                .iter()
                .map(move |x| diagnostic_row(x, Lane::Config, Some(batch.clone())))
        })
        .collect()
}

/// Maximum leaves kept per document, with the rest counted in
/// `ConfigDoc::omitted`. This bounds the prepared list size; leaf discovery
/// still walks the whole document.
pub const MAX_LEAVES_PER_DOC: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLeaf {
    pub key: String,
    pub value: String,
    pub layer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDoc {
    pub name: String,
    pub leaves: Vec<ConfigLeaf>,
    /// Leaves past the cap, after filtering.
    pub omitted: usize,
}

/// Every loaded document with its leaves (`a.b.0.c = value [layer]`),
/// filtered by substring over `doc.key` and value, capped per document.
pub fn config_docs(config: &Config, filter: &str) -> Vec<ConfigDoc> {
    let filter = filter.to_lowercase();
    config
        .doc_names()
        .filter_map(|doc_name| {
            let doc = config.doc(doc_name)?;
            let mut leaves = Vec::new();
            walk_leaves(&doc.value, "", &mut leaves);
            let mut kept: Vec<ConfigLeaf> = leaves
                .into_iter()
                .filter(|(path, value)| {
                    filter.is_empty()
                        || format!("{doc_name}.{path}")
                            .to_lowercase()
                            .contains(&filter)
                        || value.to_lowercase().contains(&filter)
                })
                .map(|(path, value)| ConfigLeaf {
                    layer: config
                        .explain(doc_name, &path)
                        .map(|l| l.name().to_string())
                        .unwrap_or_else(|| "?".into()),
                    key: path,
                    value,
                })
                .collect();
            if !filter.is_empty() && kept.is_empty() {
                return None;
            }
            let omitted = kept.len().saturating_sub(MAX_LEAVES_PER_DOC);
            kept.truncate(MAX_LEAVES_PER_DOC);
            Some(ConfigDoc {
                name: doc_name.to_string(),
                leaves: kept,
                omitted,
            })
        })
        .collect()
}

/// Walk nested tables and arrays, giving each leaf its own indexed path
/// (e.g. `bindings.0.keys.j`). Stringifying an entire array of tables
/// would create one long value with unnecessary text-shaping work.
fn walk_leaves(table: &toml::Table, prefix: &str, out: &mut Vec<(String, String)>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        walk_value(value, &path, out);
    }
}

fn walk_value(value: &toml::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        toml::Value::Table(t) => walk_leaves(t, path, out),
        toml::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                walk_value(v, &format!("{path}.{i}"), out);
            }
        }
        other => out.push((path.to_string(), other.to_string())),
    }
}

// ------------------------------------------------------------ Log

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub hms_millis: String,
    pub level: Level,
    pub target: &'static str,
    pub message: String,
    pub seq: u64,
}

pub fn log_rows<'a>(
    records: impl Iterator<Item = &'a Record>,
    filter: &LogFilter,
    clock: Clock,
) -> Vec<LogRow> {
    records
        .filter(|r| filter.accepts(r))
        .map(|r| LogRow {
            hms_millis: local_hms_millis(r.at, clock),
            level: r.level,
            target: r.target,
            message: r.message.clone(),
            seq: r.seq,
        })
        .collect()
}

/// Distinct targets in the tail, sorted, for the target select.
pub fn log_targets<'a>(records: impl Iterator<Item = &'a Record>) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = records.map(|r| r.target).collect();
    v.sort_unstable();
    v.dedup();
    v
}

// ------------------------------------------------------------ Perf

pub const FRAME_BUDGET_MICROS: u64 = 8_000;
pub const REQUERY_BUDGET_MICROS: u64 = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Percentiles {
    pub p50: String,
    pub p95: String,
    pub max: String,
    pub samples: u64,
}

fn percentiles(h: &FrameHistogram) -> Option<Percentiles> {
    (h.count() > 0).then(|| Percentiles {
        p50: h.percentile_micros(50.0).map(format_ms).unwrap_or_default(),
        p95: h.percentile_micros(95.0).map(format_ms).unwrap_or_default(),
        max: format_ms(h.max_micros()),
        samples: h.count(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerfModel {
    pub frame: Option<Percentiles>,
    pub frame_count: u64,
    pub submit: Option<Percentiles>,
    pub paint: Option<Percentiles>,
    pub dropped: u64,
    /// Data requests the handle refused `Busy` (the status bar's "N refused").
    pub refused: u64,
    /// `(upper bound µs, count)` per bucket, then the overflow bucket as
    /// `(u64::MAX, overflow)`.
    pub buckets: Vec<(u64, u32)>,
    /// Frames above the last bucket bound; the tail of `buckets` repeats it.
    pub overflow: u32,
    /// Catalog resource metrics; empty until the first catalog outcome.
    pub database: String,
    pub used: String,
    pub block_size: String,
    pub memory: String,
    pub threads: String,
    /// DuckDB's memory limit, or [`UNLIMITED`]; empty without a catalog or
    /// when unknown.
    pub memory_limit: String,
    /// DuckDB's spilled temporary files; empty without a catalog.
    pub temp: String,
    /// The largest DuckDB memory tags as `(tag, size)`, largest first.
    pub memory_top: Vec<(String, String)>,
    /// The sampled process memory; `None` before the first copy or where
    /// the platform cannot be sampled.
    pub process: Option<ProcessMemoryModel>,
    pub overlay: bool,
}

/// The process memory readout, formatted for the Performance page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMemoryModel {
    pub current: String,
    pub peak: String,
    /// When the peak was first seen, in the app clock's zone.
    pub peak_at: String,
}

/// A DuckDB memory limit at or above this (1 PiB) is read as unlimited:
/// `memory_limit = '-1'` reads back as 16383.9 PiB, not a real budget.
pub const UNLIMITED_LIMIT_BYTES: u64 = 1 << 50;

/// `PerfModel::memory_limit` for a limit at or above [`UNLIMITED_LIMIT_BYTES`].
pub const UNLIMITED: &str = "unlimited";

pub fn perf_model(d: &Diagnostics, requery: &RequeryStats, clock: Clock) -> PerfModel {
    let h = &d.frame_hist;
    let mut buckets: Vec<(u64, u32)> = BUCKET_UPPER_BOUNDS_MICROS
        .iter()
        .copied()
        .zip(h.buckets().iter().copied())
        .collect();
    buckets.push((u64::MAX, h.overflow()));
    let (database, used, block_size, memory, threads) = match &d.catalog {
        Some(c) => (
            format_bytes(c.database_bytes),
            format_bytes(c.used_blocks.saturating_mul(c.block_size)),
            format_bytes(c.block_size),
            format_bytes(c.memory_bytes),
            c.threads.to_string(),
        ),
        None => (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ),
    };
    let (memory_limit, temp, memory_top) = match &d.catalog {
        Some(c) => (
            if c.memory_limit_bytes == 0 {
                String::new()
            } else if c.memory_limit_bytes >= UNLIMITED_LIMIT_BYTES {
                UNLIMITED.to_string()
            } else {
                format_bytes(c.memory_limit_bytes)
            },
            format_bytes(c.temp_bytes),
            c.memory_top
                .iter()
                .map(|(tag, bytes)| (tag.clone(), format_bytes(*bytes)))
                .collect(),
        ),
        None => (String::new(), String::new(), Vec::new()),
    };
    let process = d.memory.as_ref().map(|m| ProcessMemoryModel {
        current: format_bytes(m.current_bytes),
        peak: format_bytes(m.peak_bytes),
        peak_at: local_hms(m.peak_at, clock),
    });
    PerfModel {
        frame: percentiles(h),
        frame_count: h.count(),
        submit: percentiles(requery.submit_to_snapshot()),
        paint: percentiles(requery.snapshot_to_paint()),
        dropped: d.dropped_events,
        refused: d.refused,
        buckets,
        overflow: h.overflow(),
        database,
        used,
        block_size,
        memory,
        threads,
        memory_limit,
        temp,
        memory_top,
        process,
        overlay: d.overlay_visible(),
    }
}

/// The shell's byte formatter, shared so the Performance page shows the
/// peak at exactly the text `Diagnostics::refresh_memory` compares.
pub use geode_shell::memory::format_bytes;

// ------------------------------------------------------------ Badges and header

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badges {
    /// Worst reported health and the source count.
    pub sources: (Option<Health>, usize),
    pub datasets: usize,
    /// (errors, warnings) in the current config batch plus data conditions.
    pub config: (usize, usize),
    pub log_errors: usize,
    pub perf_p95: String,
}

pub fn badges(d: &Diagnostics, log_errors: usize) -> Badges {
    let worst = worst_health(d).cloned();
    let current = current_diagnostics(d);
    let errors = current
        .iter()
        .filter(|r| r.severity == Severity::Error)
        .count();
    let warnings = current
        .iter()
        .filter(|r| r.severity == Severity::Warning)
        .count();
    Badges {
        sources: (worst, d.sources.len()),
        datasets: d.datasets.len(),
        config: (errors, warnings),
        log_errors,
        perf_p95: d
            .frame_hist
            .percentile_micros(95.0)
            .map(format_ms)
            .unwrap_or_default(),
    }
}

/// The header chips, worst first: source health, config errors, data
/// errors, catalog time or pending.
pub fn header_chips(d: &Diagnostics, clock: Clock) -> Vec<(String, Tone)> {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut out = Vec::new();
    if let Some(worst) = worst_health(d) {
        let n = d
            .sources
            .values()
            .filter(|s| s.health.as_ref().map(Health::label) == Some(worst.label()))
            .count();
        out.push((format!("{n} {}", worst.label()), health_tone(Some(worst))));
    }
    let errors = d
        .config
        .iter()
        .filter(|x| x.severity == Severity::Error)
        .count();
    if errors > 0 {
        out.push((
            format!("config {errors} error{}", plural(errors)),
            Tone::Error,
        ));
    }
    let data_errors = d
        .data_diagnostics
        .iter()
        .filter(|(_, x)| x.severity == Severity::Error)
        .count();
    if data_errors > 0 {
        out.push((
            format!("data {data_errors} error{}", plural(data_errors)),
            Tone::Error,
        ));
    }
    match (d.catalog.as_ref(), d.catalog_at) {
        (Some(_), Some(at)) => {
            out.push((format!("catalog {}", local_hms(at, clock)), Tone::Normal))
        }
        _ => out.push(("catalog pending".to_string(), Tone::Muted)),
    }
    out
}

/// `pub(crate)` so the page's own tests can reuse `dataset_catalog`.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use geode_core::log::LogLevels;
    use geode_core::query::{CatalogSnapshot, DatasetCatalog, GenerationInfo, PartitionCatalog};
    use std::time::{Duration, SystemTime};

    /// The page's worst health is the core severity, not label or reason
    /// order: `degraded` outranks `pending_too_long` although it sorts
    /// before it as text.
    #[test]
    fn the_page_ranks_health_by_the_core_severity() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = std::time::SystemTime::UNIX_EPOCH;
        d.note_health("a", Health::Degraded { reason: "r".into() }, "r".into(), t);
        d.note_health("b", Health::PendingTooLong, "f.csv".into(), t);
        assert_eq!(worst_health(&d).map(Health::label), Some("degraded"));
    }

    fn clock() -> Clock {
        Clock::utc()
    }

    #[test]
    fn sources_order_worst_first_then_name_and_unreported_last() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::now();
        d.note_health("b_ok", Health::Ok, String::new(), t);
        d.note_health(
            "a_degraded",
            Health::Degraded {
                reason: "stale".into(),
            },
            String::new(),
            t,
        );
        d.note_polled("zz_unreported", 0, t, t + Duration::from_secs(60));
        // Same variant, reasons in the opposite order to the names: the
        // derived `Ord` would put `c_degraded` ("zzz") before `a_degraded`.
        d.note_health(
            "c_degraded",
            Health::Degraded {
                reason: "zzz".into(),
            },
            String::new(),
            t,
        );
        let rows = source_rows(&d, clock());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a_degraded", "c_degraded", "b_ok", "zz_unreported"],
            "rank then name, never reason text"
        );
        assert_eq!(rows[0].health, "Degraded — stale");
        assert_eq!(rows[0].tone, Tone::Warn);
        assert_eq!(rows[3].health, "no report yet");
        assert_eq!(rows[3].ready, "0", "polled: ready shown");
        assert_eq!(rows[0].ready, "", "never polled: blank");
    }

    #[test]
    fn a_loading_row_shows_for_an_unreported_source_too() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::now();
        d.note_loading("cold", "/x/a.csv", 2, t);
        let rows = source_rows(&d, clock());
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].loading.starts_with("/x/a.csv since "),
            "{}",
            rows[0].loading
        );
    }

    /// The shape column and the detail lines follow `SourceSummary::shape`,
    /// never the adapter name or an empty topic list: a fetch source is its
    /// adapter plus `fetch`, and a subscribed source names its topics.
    #[test]
    fn a_source_row_describes_its_shape_from_the_summary() {
        use geode_shell::diagnostics::SourceSummary;
        let mut d = Diagnostics::new(LogLevels::default());
        let summary = |shape: SourceShape, topics: Vec<String>| SourceSummary {
            dataset: String::new(),
            paths: vec!["/x".into()],
            priority: "1".into(),
            readiness: "ready".into(),
            adapter: "ADAPTER".into(),
            topics,
            shape,
        };
        d.describe_source("dir", summary(SourceShape::Directory, Vec::new()));
        d.describe_source("fetch", summary(SourceShape::Fetch, Vec::new()));
        d.describe_source(
            "sub",
            summary(SourceShape::Subscribed, vec!["a/>".into(), "b".into()]),
        );
        let rows = source_rows(&d, clock());
        let row = |name: &str| rows.iter().find(|r| r.name == name).unwrap();
        assert_eq!(row("dir").shape, "directory");
        assert_eq!(row("dir").detail[0], "path: /x");
        assert_eq!(row("fetch").shape, "fetch");
        assert_eq!(
            row("fetch").detail,
            vec!["adapter: ADAPTER".to_string(), "fetch".to_string()]
        );
        assert_eq!(row("sub").shape, "subscribed · 2 topics");
        assert_eq!(row("sub").detail[1], "topics: a/>, b");
    }

    #[test]
    fn age_text_scales() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        assert_eq!(age_text(Some(now - Duration::from_secs(5)), now), "5 s");
        assert_eq!(age_text(Some(now - Duration::from_secs(240)), now), "4 m");
        assert_eq!(
            age_text(Some(now - Duration::from_secs(3_780)), now),
            "1 h 3 m"
        );
        assert_eq!(age_text(None, now), "");
    }

    pub(crate) fn dataset_catalog() -> DatasetCatalog {
        DatasetCatalog {
            name: "risk".into(),
            partitions: vec![PartitionCatalog {
                batch: "2026-09-08".into(),
                book: Some("EU_TECH".into()),
                generations: vec![
                    GenerationInfo {
                        gen_id: 1,
                        source_time: chrono::DateTime::UNIX_EPOCH,
                        loaded_at: Some(chrono::DateTime::UNIX_EPOCH),
                        file_rows: Some(100),
                        live: false,
                    },
                    GenerationInfo {
                        gen_id: 2,
                        source_time: chrono::DateTime::UNIX_EPOCH,
                        loaded_at: Some(chrono::DateTime::UNIX_EPOCH),
                        file_rows: Some(120),
                        live: true,
                    },
                ],
                resolved_gen: Some(1),
            }],
            live_rows: 120,
            archive_rows: 100,
            series: Vec::new(),
        }
    }

    #[test]
    fn resolved_markers_need_a_matching_catalog_and_a_historical_frame() {
        let mut d = Diagnostics::new(LogLevels::default());
        let at = AsOf::At(chrono::Utc::now());
        d.set_catalog(
            CatalogSnapshot {
                as_of: at.clone(),
                datasets: vec![dataset_catalog()],
                database_bytes: 0,
                used_blocks: 0,
                block_size: 0,
                memory_bytes: 0,
                memory_limit_bytes: 0,
                temp_bytes: 0,
                memory_top: Vec::new(),
                threads: 1,
                identities: Vec::new(),
            },
            SystemTime::now(),
        );
        let rows = dataset_rows(&d, &at, clock());
        assert_eq!(rows[0].children.iter().filter(|c| c.marked).count(), 1);
        assert_eq!(rows[0].resolved, "1");
        assert_eq!(rows[0].latest_gen, "2");
        let live = dataset_rows(&d, &AsOf::Live, clock());
        assert!(
            live[0].children.iter().all(|c| !c.marked),
            "live: nothing resolved"
        );
        let other = AsOf::At(chrono::Utc::now() + chrono::Duration::hours(1));
        let stale = dataset_rows(&d, &other, clock());
        assert!(
            stale[0].children.iter().all(|c| !c.marked),
            "catalog for another as-of: hidden"
        );
        assert!(!catalog_matches_frame(&d, &other));
    }

    #[test]
    fn config_docs_carry_provenance_recurse_into_arrays_and_filter() {
        use geode_core::config::{ConfigSources, LayerDoc};
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "config_version = 1\n[theme]\nname = \"Solarized\"\n")
                    .unwrap(),
                LayerDoc::builtin(
                    "keymap",
                    "config_version = 1\n[[bindings]]\n[bindings.keys]\n\"j\" = \"x::down\"\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let docs = config_docs(&config, "");
        let app = docs.iter().find(|d| d.name == "app").unwrap();
        let theme = app.leaves.iter().find(|l| l.key == "theme.name").unwrap();
        assert_eq!(theme.value, "\"Solarized\"");
        assert_eq!(theme.layer, "builtin");
        let keymap = docs.iter().find(|d| d.name == "keymap").unwrap();
        assert!(
            keymap.leaves.iter().any(|l| l.key == "bindings.0.keys.j"),
            "indexed array path"
        );
        assert!(config_docs(&config, "no-such-key").is_empty());
        assert_eq!(config_docs(&config, "SOLARIZED").len(), 1);
        let filtered = config_docs(&config, "theme");
        assert_eq!(filtered.len(), 1, "unmatched documents are hidden");
        assert!(filtered.iter().all(|d| {
            d.leaves
                .iter()
                .all(|l| format!("{}.{}", d.name, l.key).contains("theme"))
        }));
        assert!(
            docs.iter()
                .all(|d| d.leaves.len() <= MAX_LEAVES_PER_DOC && d.omitted == 0)
        );
    }

    #[test]
    fn badges_count_errors_and_warnings_and_worst_health() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::now();
        d.note_health("a", Health::Ok, String::new(), t);
        d.note_health("b", Health::Failed { reason: "x".into() }, String::new(), t);
        d.note_config(
            vec![
                Diagnostic {
                    severity: Severity::Error,
                    layer: None,
                    file: None,
                    message: "e".into(),
                    path: None,
                },
                Diagnostic {
                    severity: Severity::Warning,
                    layer: None,
                    file: None,
                    message: "w".into(),
                    path: None,
                },
            ],
            t,
        );
        let b = badges(&d, 3);
        assert_eq!(b.sources, (Some(Health::Failed { reason: "x".into() }), 2));
        assert_eq!(b.config, (1, 1));
        assert_eq!(b.log_errors, 3);
        let chips = header_chips(&d, clock());
        assert_eq!(chips[0].0, "1 failed");
        assert_eq!(chips[1], ("config 1 error".to_string(), Tone::Error));
        assert_eq!(
            chips.last().unwrap(),
            &("catalog pending".to_string(), Tone::Muted)
        );
    }

    /// `Health`'s derived `Eq`/`Ord` include the reason, so a full-value
    /// compare reads two failed sources as "1 failed" and picks the worst by
    /// reason text; the status summary counts by label and must agree.
    #[test]
    fn the_health_chip_counts_by_label_not_by_reason() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::now();
        d.note_health(
            "a",
            Health::Failed {
                reason: "zzz".into(),
            },
            String::new(),
            t,
        );
        d.note_health(
            "b",
            Health::Failed {
                reason: "aaa".into(),
            },
            String::new(),
            t,
        );
        d.note_health(
            "c",
            Health::Degraded {
                reason: "~~~ sorts above Failed's reasons".into(),
            },
            String::new(),
            t,
        );
        let chips = header_chips(&d, clock());
        assert_eq!(chips[0], ("2 failed".to_string(), Tone::Error));
        let b = badges(&d, 0);
        assert!(
            matches!(b.sources.0, Some(Health::Failed { .. })),
            "worst by variant, not by reason text: {:?}",
            b.sources
        );
        assert_eq!(d.summary().as_ref(), "sources 1 degraded · 2 failed");
    }

    #[test]
    fn perf_model_carries_the_refused_request_total() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_refused(7);
        assert_eq!(perf_model(&d, &RequeryStats::new(), clock()).refused, 7);
    }

    #[test]
    fn perf_model_carries_buckets_overflow_and_the_overlay_mirror() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.watch();
        let mut hist = FrameHistogram::new();
        hist.record_micros(1_000);
        hist.record_micros(200_000);
        d.refresh_frame_hist(&hist);
        d.set_overlay_visible(true);
        let m = perf_model(&d, &RequeryStats::new(), clock());
        assert_eq!(m.frame_count, 2);
        assert_eq!(m.buckets.len(), BUCKET_UPPER_BOUNDS_MICROS.len() + 1);
        assert_eq!(m.buckets.iter().map(|(_, n)| *n as u64).sum::<u64>(), 2);
        assert_eq!(m.buckets.last(), Some(&(u64::MAX, 1)));
        assert!(m.overlay);
        // The database tiles are empty until a catalog arrives, then read
        // its resource figures: used space is blocks times block size.
        assert_eq!(
            (&m.database, &m.used, &m.block_size, &m.memory, &m.threads),
            (
                &String::new(),
                &String::new(),
                &String::new(),
                &String::new(),
                &String::new()
            )
        );
        d.set_catalog(
            CatalogSnapshot {
                as_of: AsOf::Live,
                datasets: Vec::new(),
                database_bytes: 3 * 1024 * 1024 * 1024,
                used_blocks: 4,
                block_size: 256 * 1024,
                memory_bytes: 512,
                memory_limit_bytes: 0,
                temp_bytes: 0,
                memory_top: Vec::new(),
                threads: 8,
                identities: Vec::new(),
            },
            SystemTime::now(),
        );
        let m = perf_model(&d, &RequeryStats::new(), clock());
        assert_eq!(m.database, "3.0GB");
        assert_eq!(m.used, "1.0MB");
        assert_eq!(m.block_size, "256.0KB");
        assert_eq!(m.memory, "512B");
        assert_eq!(m.threads, "8");
        assert_eq!(m.memory_limit, "", "an unreadable limit stays empty");
    }

    #[test]
    fn perf_model_formats_process_memory_and_duckdb_memory_detail() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let mut d = Diagnostics::new(LogLevels::default());
        let m = perf_model(&d, &RequeryStats::new(), clock());
        assert_eq!(m.process, None, "no reading yet");
        assert_eq!((m.memory_limit.as_str(), m.temp.as_str()), ("", ""));
        assert!(m.memory_top.is_empty());

        d.watch();
        d.refresh_memory(&geode_shell::memory::MemoryReading {
            current_bytes: 3 * GIB,
            peak_bytes: 7 * GIB + GIB / 10,
            peak_at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(3_723),
        });
        d.set_catalog(
            CatalogSnapshot {
                memory_bytes: 2 * GIB,
                memory_limit_bytes: 38 * GIB,
                temp_bytes: 512 * 1024 * 1024,
                memory_top: vec![("BASE_TABLE".into(), GIB), ("HASH_TABLE".into(), 1024)],
                ..CatalogSnapshot::default()
            },
            SystemTime::now(),
        );
        let m = perf_model(&d, &RequeryStats::new(), clock());
        assert_eq!(
            m.process,
            Some(ProcessMemoryModel {
                current: "3.0GB".into(),
                peak: "7.1GB".into(),
                peak_at: "01:02:03".into(),
            }),
            "the peak time reads in the app clock's zone (UTC here)"
        );
        assert_eq!(m.memory, "2.0GB");
        assert_eq!(m.memory_limit, "38.0GB");
        let mut unlimited = d.catalog.clone().unwrap();
        unlimited.memory_limit_bytes = (16383.9 * (1u64 << 50) as f64) as u64;
        d.set_catalog(unlimited, SystemTime::now());
        assert_eq!(
            perf_model(&d, &RequeryStats::new(), clock()).memory_limit,
            "unlimited"
        );
        let mut huge = d.catalog.clone().unwrap();
        huge.memory_limit_bytes = (1 << 50) - 1; // a literal, so the constant is pinned
        d.set_catalog(huge, SystemTime::now());
        assert_eq!(
            perf_model(&d, &RequeryStats::new(), clock()).memory_limit,
            "1024.0TB"
        );
        assert_eq!(m.temp, "512.0MB");
        assert_eq!(
            m.memory_top,
            vec![
                ("BASE_TABLE".to_string(), "1.0GB".to_string()),
                ("HASH_TABLE".to_string(), "1.0KB".to_string())
            ]
        );
    }
}
