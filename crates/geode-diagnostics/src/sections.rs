//! Pure row builders for the five diagnostics sections. Each builder returns
//! the same rows for the same inputs, including explicit time and clock values.
//! `tile.rs` prepares these rows when inputs change and maps their `Tone` to
//! theme colours during paint.

use std::collections::BTreeSet;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use gpui::SharedString;

use geode_core::config::{Config, Diagnostic, Severity};
use geode_core::log::Record;
use geode_core::query::AsOf;
use geode_shell::diagnostics::{Diagnostics, Health, SourceShape};
use geode_shell::perf::{RequeryStats, format_ms};

/// A row's visual weight, mapped to theme colours by `tile.rs`. `Marked`
/// highlights the generation resolved under a historical as-of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Muted,
    Warn,
    Error,
    Marked,
}

/// One line a section wants painted. `depth` drives indentation (0 is a
/// section/dataset/source header); `collapsible` is `Some(open)` on a row
/// the user can fold with `zo`/`zc` — only the "data" section's dataset
/// headers use it today.
#[derive(Debug, Clone)]
pub struct Row {
    pub text: SharedString,
    pub depth: u8,
    pub tone: Tone,
    pub collapsible: Option<bool>,
}

fn row(text: impl Into<SharedString>, depth: u8, tone: Tone) -> Row {
    Row {
        text: text.into(),
        depth,
        tone,
        collapsible: None,
    }
}

/// Format `HH:MM:SS` using the supplied display clock, shared with the
/// frame's time readouts. The tile supplies the configured `AppClock`.
fn local_hms(t: SystemTime, clock: Clock) -> String {
    clock.hms(DateTime::<Utc>::from(t))
}

fn local_hms_utc(t: DateTime<Utc>, clock: Clock) -> String {
    clock.hms(t)
}

/// Source descriptions, health, and poll times, sorted worst health first.
/// `Health::Ord` orders by severity ascending, so reported health is reversed.
/// Sources with no health report appear last as "no report yet"; they are
/// not assigned an assumed health status.
pub fn sources_rows(d: &Diagnostics, now: SystemTime, clock: Clock) -> Vec<Row> {
    let mut reported: Vec<(&String, &geode_shell::diagnostics::SourceState)> = d
        .sources
        .iter()
        .filter(|(_, s)| s.health.is_some())
        .collect();
    reported.sort_by(|a, b| b.1.health.cmp(&a.1.health).then_with(|| a.0.cmp(b.0)));
    let mut unreported: Vec<(&String, &geode_shell::diagnostics::SourceState)> = d
        .sources
        .iter()
        .filter(|(_, s)| s.health.is_none())
        .collect();
    unreported.sort_by(|a, b| a.0.cmp(b.0));

    let mut out = Vec::new();
    // A stopped data thread leads the section the status segment opens:
    // it is why the bar went red, and it outlives every source row below.
    if !d.stopped.is_empty() {
        out.push(row(
            "stopped threads — restart Geode to recover them",
            0,
            Tone::Error,
        ));
        for t in &d.stopped {
            out.push(row(
                format!("{}: {} (at {})", t.label, t.reason, local_hms(t.at, clock)),
                1,
                Tone::Error,
            ));
        }
    }
    for (name, state) in reported.into_iter().chain(unreported) {
        // Computed once per source, ahead of the reported/unreported
        // split, and pushed in BOTH arms below: an ingest `Started` can
        // land before that source's first-ever `Health` note (the
        // exact cold-start moment this row exists for), so a source
        // still reading "no report yet" must show it too, not only a
        // source with a health row already.
        let loading = d.ingest.as_ref().filter(|a| a.source == *name);
        let Some(health) = &state.health else {
            out.push(row(format!("{name}: no report yet"), 0, Tone::Muted));
            if let Some(a) = loading {
                out.push(row(
                    format!("loading {} since {}", a.path, local_hms(a.since, clock)),
                    1,
                    Tone::Muted,
                ));
            }
            push_spec_detail(&mut out, state);
            continue;
        };
        let (label, reason) = health.to_parts();
        let tone = match health {
            Health::Ok => Tone::Normal,
            Health::Pending | Health::PendingTooLong => Tone::Muted,
            Health::Degraded { .. } => Tone::Warn,
            Health::Failed { .. } => Tone::Error,
        };
        let since = local_hms(state.since, clock);
        let mut text = format!("{name}: {label}");
        if let Some(reason) = &reason
            && !reason.is_empty()
        {
            text.push_str(&format!(" — {reason}"));
        }
        // Relative age uses the rebuild time and stays fixed until the next
        // source-section rebuild. The absolute timestamp beside it remains
        // valid while the source is quiet; perf-only ticks do not refresh it.
        let elapsed = now
            .duration_since(state.since)
            .map(|d| format!(" · {}s ago", d.as_secs()))
            .unwrap_or_default();
        text.push_str(&format!(" (since {since}{elapsed})"));
        out.push(row(text, 0, tone));
        if let Some(a) = loading {
            out.push(row(
                format!("loading {} since {}", a.path, local_hms(a.since, clock)),
                1,
                Tone::Muted,
            ));
        }
        push_spec_detail(&mut out, state);
        let mut poll = String::new();
        if let Some(last) = state.last_poll {
            poll.push_str(&format!("last poll {}", local_hms(last, clock)));
        }
        if let Some(next) = state.next_poll {
            if !poll.is_empty() {
                poll.push_str(" · ");
            }
            poll.push_str(&format!("next poll {}", local_hms(next, clock)));
        }
        if !poll.is_empty() {
            poll.push_str(&format!(" · ready {}", state.last_ready));
            out.push(row(poll, 1, Tone::Muted));
        }
    }
    out
}

fn push_spec_detail(out: &mut Vec<Row>, state: &geode_shell::diagnostics::SourceState) {
    let Some(spec) = &state.spec else { return };
    // Use two short rows because fixed-height list slots clip instead of wrap.
    // The bridge resolves `SourceSpec::shape` using the dataset family: neither
    // the adapter name nor an empty topic list can distinguish all three shapes.
    // Directories show paths and readiness, subscriptions show adapter and
    // topics, and fetch sources show adapter and on-demand span retrieval.
    match spec.shape {
        SourceShape::Directory => {
            let paths = spec.paths.join(", ");
            out.push(row(format!("path: {paths}"), 1, Tone::Muted));
            out.push(row(
                format!(
                    "adapter: {} · priority: {} · readiness: {}",
                    spec.adapter, spec.priority, spec.readiness
                ),
                1,
                Tone::Muted,
            ));
        }
        SourceShape::Fetch => {
            out.push(row(format!("adapter: {}", spec.adapter), 1, Tone::Muted));
            out.push(row("fetch", 1, Tone::Muted));
        }
        SourceShape::Subscribed => {
            out.push(row(format!("adapter: {}", spec.adapter), 1, Tone::Muted));
            out.push(row(
                format!("topics: {}", spec.topics.join(", ")),
                1,
                Tone::Muted,
            ));
        }
    }
}

/// Dataset and partition generations with IDs, publication times, row counts,
/// and live/archive state. For a historical as-of, mark the resolved generation
/// only when the catalog and frame as-of agree. Collapsed datasets show only
/// their header.
pub fn data_rows(
    d: &Diagnostics,
    as_of: &AsOf,
    collapsed: &BTreeSet<String>,
    clock: Clock,
) -> Vec<Row> {
    // Per-dataset catalog slices share the snapshot's as-of. After a frame
    // as-of change, hide the resolved marker until a matching catalog arrives
    // so the displayed generation cannot imply a different historical instant.
    let snapshot_matches_as_of = d.catalog.as_ref().is_some_and(|c| &c.as_of == as_of);
    let mut out = Vec::new();
    for (name, state) in &d.datasets {
        let open = !collapsed.contains(name);
        let Some(catalog) = &state.catalog else {
            let mut r = row(format!("{name}: (no catalog yet)"), 0, Tone::Muted);
            r.collapsible = Some(open);
            out.push(r);
            continue;
        };
        let mut header = row(
            format!(
                "{name}: {} rows (est., live) · {} rows (est., archive)",
                catalog.live_rows, catalog.archive_rows
            ),
            0,
            Tone::Normal,
        );
        header.collapsible = Some(open);
        out.push(header);
        if !open {
            continue;
        }
        for part in &catalog.partitions {
            let book = part.book.as_deref().unwrap_or("(bookless)");
            out.push(row(format!("{} · {book}", part.batch), 1, Tone::Muted));
            for generation in &part.generations {
                let marked = !as_of.is_live()
                    && snapshot_matches_as_of
                    && part.resolved_gen == Some(generation.gen_id);
                let tone = if marked { Tone::Marked } else { Tone::Normal };
                let loaded = generation
                    .loaded_at
                    .map(|t| local_hms_utc(t, clock))
                    .unwrap_or_else(|| "?".to_string());
                let rows = generation
                    .file_rows
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let kind = if generation.live { "live" } else { "archive" };
                out.push(row(
                    format!(
                        "gen {} · {} · loaded {loaded} · rows {rows} · {kind}",
                        generation.gen_id,
                        local_hms_utc(generation.source_time, clock)
                    ),
                    2,
                    tone,
                ));
            }
        }
    }
    out
}

/// Current config-load diagnostics, data-layer diagnostics, historical load
/// batches, and effective-config leaves with provenance from `Config::explain`.
/// Apply the same substring filter to every formatted row, including diagnostics.
pub fn config_rows(d: &Diagnostics, config: &Config, filter: &str, clock: Clock) -> Vec<Row> {
    let mut out = Vec::new();
    out.push(row("current config diagnostics", 0, Tone::Muted));
    if d.config.is_empty() {
        out.push(row("(none)", 1, Tone::Normal));
    }
    for diag in &d.config {
        out.push(diagnostic_row(diag));
    }

    out.push(row("data diagnostics", 0, Tone::Muted));
    if d.data_diagnostics.is_empty() {
        out.push(row("(none)", 1, Tone::Normal));
    }
    for (_, diag) in &d.data_diagnostics {
        out.push(diagnostic_row(diag));
    }

    if d.config_history.len() > 1 {
        out.push(row("history", 0, Tone::Muted));
        for (at, diags) in d.config_history.iter().skip(1) {
            out.push(row(
                format!("batch at {}", local_hms(*at, clock)),
                1,
                Tone::Muted,
            ));
            for diag in diags {
                out.push(diagnostic_row_at_depth(diag, 2));
            }
        }
    }

    out.push(row("effective config", 0, Tone::Muted));
    // Use the loaded document inventory so new documents appear automatically.
    for doc_name in config.doc_names() {
        let Some(doc) = config.doc(doc_name) else {
            continue;
        };
        let mut leaves = Vec::new();
        walk_leaves(&doc.value, "", &mut leaves);
        let total = leaves.len();
        let capped = total > MAX_LEAVES_PER_DOC;
        for (path, value) in leaves.iter().take(MAX_LEAVES_PER_DOC) {
            let layer = config
                .explain(doc_name, path)
                .map(|l| l.name())
                .unwrap_or("?");
            out.push(row(
                format!("{doc_name}.{path} = {value}  [{layer}]"),
                1,
                Tone::Normal,
            ));
        }
        if capped {
            out.push(row(
                format!("… {} more", total - MAX_LEAVES_PER_DOC),
                1,
                Tone::Muted,
            ));
        }
    }

    if filter.is_empty() {
        out
    } else {
        out.into_iter()
            .filter(|r| r.text.contains(filter))
            .collect()
    }
}

/// Maximum leaf rows displayed per document, followed by an omitted-count row.
/// This bounds the prepared list size; leaf discovery still walks the document.
const MAX_LEAVES_PER_DOC: usize = 2_000;

/// Walk nested tables and arrays, giving each leaf its own indexed path
/// (e.g. `keymap.bindings.0.keys.j`). Stringifying an entire array of tables
/// would create a long, clipped row with unnecessary text-shaping work.
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

fn diagnostic_row(diag: &Diagnostic) -> Row {
    diagnostic_row_at_depth(diag, 1)
}

fn diagnostic_row_at_depth(diag: &Diagnostic, depth: u8) -> Row {
    let tone = match diag.severity {
        Severity::Error => Tone::Error,
        Severity::Warning => Tone::Warn,
    };
    row(diag.to_string(), depth, tone)
}

/// Log records, oldest first, filtered by substring against each formatted
/// row. The filter can match a target such as `ingest` or a level such as `WARN`.
pub fn log_rows(records: &[Record], filter: &str, clock: Clock) -> Vec<Row> {
    records
        .iter()
        .map(|r| {
            let tone = match r.level {
                geode_core::log::Level::ERROR => Tone::Error,
                geode_core::log::Level::WARN => Tone::Warn,
                _ => Tone::Normal,
            };
            row(
                format!(
                    "{} {:>5} {} {}",
                    local_hms(r.at, clock),
                    r.level,
                    r.target,
                    r.message
                ),
                0,
                tone,
            )
        })
        .filter(|r| filter.is_empty() || r.text.contains(filter))
        .collect()
}

/// Frame histogram percentiles, requery timing, dropped events, and catalog
/// resource metrics. Inputs are snapshots; this builder performs no I/O.
pub fn perf_rows(d: &Diagnostics, requery: &RequeryStats) -> Vec<Row> {
    let mut out = Vec::new();
    let h = &d.frame_hist;
    if h.count() == 0 {
        out.push(row("frame: no samples yet", 0, Tone::Muted));
    } else {
        out.push(row(
            format!(
                "frame p50 {} · p95 {} · max {} · n={}",
                h.percentile_micros(50.0).map(format_ms).unwrap_or_default(),
                h.percentile_micros(95.0).map(format_ms).unwrap_or_default(),
                format_ms(h.max_micros()),
                h.count()
            ),
            0,
            Tone::Normal,
        ));
    }
    let submit = requery.submit_to_snapshot();
    let paint = requery.snapshot_to_paint();
    if submit.count() > 0 {
        out.push(row(
            format!(
                "requery submit→snapshot p50 {} · p95 {}",
                submit
                    .percentile_micros(50.0)
                    .map(format_ms)
                    .unwrap_or_default(),
                submit
                    .percentile_micros(95.0)
                    .map(format_ms)
                    .unwrap_or_default(),
            ),
            0,
            Tone::Normal,
        ));
    }
    if paint.count() > 0 {
        out.push(row(
            format!(
                "requery snapshot→paint p50 {} · p95 {}",
                paint
                    .percentile_micros(50.0)
                    .map(format_ms)
                    .unwrap_or_default(),
                paint
                    .percentile_micros(95.0)
                    .map(format_ms)
                    .unwrap_or_default(),
            ),
            0,
            Tone::Normal,
        ));
    }
    if let Some((first, second)) = requery.last() {
        out.push(row(
            format!("last requery {} + {}", format_ms(first), format_ms(second)),
            0,
            Tone::Muted,
        ));
    }
    let tone = if d.dropped_events > 0 {
        Tone::Warn
    } else {
        Tone::Muted
    };
    out.push(row(
        format!("dropped events: {}", d.dropped_events),
        0,
        tone,
    ));
    // Catalog resource metrics come from DuckDB queries on the data thread.
    // They remain absent until the first catalog outcome arrives.
    if let Some(catalog) = &d.catalog {
        out.push(row(
            format!(
                "database {} (checkpointed) · memory {} · threads {}",
                format_bytes(catalog.database_bytes),
                format_bytes(catalog.memory_bytes),
                catalog.threads,
            ),
            0,
            Tone::Muted,
        ));
    }
    out
}

/// A plain binary-unit byte count, no fractional precision beyond one
/// decimal place — this is a diagnostics row, not a UI a trader reads
/// with any regularity, so simplicity over a full humanize crate.
fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}GB", b / GB)
    } else if b >= MB {
        format!("{:.1}MB", b / MB)
    } else if b >= KB {
        format!("{:.1}KB", b / KB)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, Layer, LayerDoc};
    use geode_core::log::LogLevels;
    use geode_core::query::{CatalogSnapshot, DatasetCatalog, GenerationInfo, PartitionCatalog};
    use geode_shell::diagnostics::SourceSummary;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn stopped_threads_lead_the_sources_section() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_health("risk", Health::Ok, String::new(), SystemTime::UNIX_EPOCH);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(3_600);
        d.note_thread_stopped("geode-ingest", "boom".into(), at);
        let rows = sources_rows(&d, at, Clock::utc());
        assert_eq!(rows[0].tone, Tone::Error);
        assert!(
            rows[0].text.contains("stopped threads"),
            "{:?}",
            rows[0].text
        );
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].tone, Tone::Error);
        assert_eq!(
            rows[1].text.as_ref(),
            format!("ingest: boom (at {})", local_hms(at, Clock::utc()))
        );
        assert!(rows[2].text.starts_with("risk"), "sources follow");
    }

    #[test]
    fn sources_are_sorted_worst_first_with_their_detail() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::UNIX_EPOCH;
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
        d.note_health(
            "risk",
            Health::Degraded {
                reason: "column missing".into(),
            },
            "column missing".into(),
            t,
        );
        d.note_polled("risk", 3, t, t + Duration::from_secs(30));
        d.note_health("gamma", Health::Ok, "".into(), t);
        d.note_health(
            "delta",
            Health::Failed {
                reason: "torn read".into(),
            },
            "torn read".into(),
            t,
        );
        d.describe_source(
            "unreported",
            SourceSummary {
                paths: vec![],
                priority: "p".into(),
                readiness: "r".into(),
                adapter: "csv_dir".into(),
                topics: Vec::new(),
                shape: SourceShape::Directory,
            },
        );

        let rows = sources_rows(&d, t, Clock::utc());
        let headers: Vec<&str> = rows
            .iter()
            .filter(|r| r.depth == 0)
            .map(|r| r.text.as_ref())
            .collect();
        // Failed before Degraded before Ok; the unreported source last.
        assert!(headers[0].starts_with("delta: failed"));
        assert!(headers[1].starts_with("risk: degraded"));
        assert!(headers[2].starts_with("gamma: ok"));
        assert!(headers[3].starts_with("unreported: no report yet"));

        // The whole block belonging to "risk" — its header plus every
        // sub-row up to the next depth-0 header — not just rows whose own
        // text happens to repeat the source name.
        let start = rows
            .iter()
            .position(|r| r.text.starts_with("risk:"))
            .unwrap();
        let end = rows[start + 1..]
            .iter()
            .position(|r| r.depth == 0)
            .map(|i| start + 1 + i)
            .unwrap_or(rows.len());
        let risk_text: String = rows[start..end]
            .iter()
            .map(|r| r.text.to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(risk_text.contains("column missing"));
        assert!(risk_text.contains("since"));
        assert!(risk_text.contains("last poll"));
        assert!(risk_text.contains("next poll"));
    }

    #[test]
    fn a_loading_source_shows_what_it_is_loading_under_its_health_row() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::UNIX_EPOCH;
        d.note_health("risk", Health::Ok, "".into(), t);
        let at = SystemTime::now();
        d.note_loading("risk", "/data/risk/EOD.csv", 1, at);
        let rows = sources_rows(&d, at, Clock::utc());
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_ref()).collect();
        assert!(
            text.iter()
                .any(|t| t.starts_with("loading /data/risk/EOD.csv since ")),
            "{text:?}"
        );
        d.note_load_ended();
        let rows = sources_rows(&d, at, Clock::utc());
        assert!(!rows.iter().any(|r| r.text.starts_with("loading ")));
    }

    #[test]
    fn a_sources_spec_detail_is_split_into_short_rows() {
        // Separate field groups keep long source paths from sharing a fixed-height
        // row with priority and readiness.
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "demo",
            SourceSummary {
                paths: vec!["/very/long/tmp/path/geode-demo/1000000-42/src/*.csv".into()],
                priority: "LatestRisk".into(),
                readiness: "Sentinel".into(),
                adapter: "csv_dir".into(),
                topics: Vec::new(),
                shape: SourceShape::Directory,
            },
        );
        let rows = sources_rows(&d, SystemTime::UNIX_EPOCH, Clock::utc());
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_ref()).collect();
        let path_row = texts
            .iter()
            .find(|t| t.starts_with("path: "))
            .expect("a path row");
        assert!(
            !path_row.contains("priority"),
            "the path row carries only the path: {path_row}"
        );
        assert!(
            texts.contains(&"adapter: csv_dir · priority: LatestRisk · readiness: Sentinel"),
            "the adapter, priority and readiness share one short row: {texts:?}"
        );
    }

    /// A subscribed source has no path to poll and no readiness rule —
    /// both were painted anyway, so a market-data feed read as a
    /// directory source with an empty path and a sentinel convention it
    /// has never used. It gets its adapter and its topics instead.
    #[test]
    fn a_subscribed_source_shows_its_adapter_and_topics_not_paths() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "cvi",
            SourceSummary {
                paths: Vec::new(),
                priority: "LatestOther".into(),
                readiness: "Sentinel".into(),
                adapter: "demo_bus".into(),
                topics: vec!["marketdata/cvi/>".into()],
                shape: SourceShape::Subscribed,
            },
        );
        let rows = sources_rows(&d, SystemTime::UNIX_EPOCH, Clock::utc());
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_ref()).collect();
        assert!(texts.contains(&"adapter: demo_bus"), "{texts:?}");
        assert!(texts.contains(&"topics: marketdata/cvi/>"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.starts_with("path: ")),
            "a subscribed source has no path to poll: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("readiness")),
            "nor a readiness rule: {texts:?}"
        );
    }

    /// The declared shape determines subscribed-source rows even when the topic
    /// list is empty. Diagnostics must handle callers that bypass config validation.
    #[test]
    fn a_subscribed_source_with_no_topics_still_shows_the_subscribed_shape() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "cvi",
            SourceSummary {
                paths: Vec::new(),
                priority: "LatestOther".into(),
                readiness: "Sentinel".into(),
                adapter: "demo_bus".into(),
                topics: Vec::new(),
                shape: SourceShape::Subscribed,
            },
        );
        let rows = sources_rows(&d, SystemTime::UNIX_EPOCH, Clock::utc());
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_ref()).collect();
        assert!(texts.contains(&"adapter: demo_bus"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.starts_with("path: ")),
            "a subscribed source has no path to poll even with no topics: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("readiness")),
            "nor a readiness rule: {texts:?}"
        );
        assert!(
            texts.contains(&"topics: "),
            "and it is still the subscribed shape, empty topic list and all: {texts:?}"
        );
    }

    /// Fetch sources display adapter and retrieval details without directory
    /// paths, readiness rules, or subscription topics.
    #[test]
    fn a_fetch_source_shows_its_adapter_and_that_it_is_fetched() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "kdb_hist",
            SourceSummary {
                paths: Vec::new(),
                priority: "LatestOther".into(),
                readiness: "Sentinel".into(),
                adapter: "kdb".into(),
                topics: Vec::new(),
                shape: SourceShape::Fetch,
            },
        );
        let rows = sources_rows(&d, SystemTime::UNIX_EPOCH, Clock::utc());
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_ref()).collect();
        assert!(texts.contains(&"adapter: kdb"), "{texts:?}");
        assert!(texts.contains(&"fetch"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.starts_with("topics")),
            "a fetch source has no topics, not an empty list of them: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.starts_with("path: ")),
            "nor a path to poll: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("readiness")),
            "nor a readiness rule: {texts:?}"
        );
    }

    #[test]
    fn a_described_but_unreported_source_shows_no_report_yet_not_pending() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec![],
                priority: "p".into(),
                readiness: "r".into(),
                adapter: "csv_dir".into(),
                topics: Vec::new(),
                shape: SourceShape::Directory,
            },
        );
        let rows = sources_rows(&d, SystemTime::UNIX_EPOCH, Clock::utc());
        assert_eq!(rows[0].text.as_ref(), "risk: no report yet");
        assert_eq!(rows[0].tone, Tone::Muted);
    }

    /// Loading can precede the first health report. Show the loading row even
    /// when the source still reads "no report yet".
    #[test]
    fn a_loading_source_with_no_report_yet_still_shows_what_it_is_loading() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec![],
                priority: "p".into(),
                readiness: "r".into(),
                adapter: "csv_dir".into(),
                topics: Vec::new(),
                shape: SourceShape::Directory,
            },
        );
        let at = SystemTime::now();
        d.note_loading("risk", "/data/risk/EOD.csv", 1, at);
        let rows = sources_rows(&d, at, Clock::utc());
        assert_eq!(rows[0].text.as_ref(), "risk: no report yet");
        assert_eq!(
            rows[1].text.as_ref(),
            format!(
                "loading /data/risk/EOD.csv since {}",
                local_hms(at, Clock::utc())
            ),
            "the loading row sits directly under the no-report-yet row"
        );

        d.note_load_ended();
        let rows = sources_rows(&d, at, Clock::utc());
        assert!(!rows.iter().any(|r| r.text.starts_with("loading ")));
    }

    fn dataset_catalog() -> DatasetCatalog {
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
    fn data_rows_mark_the_resolved_generation_under_an_as_of() {
        let mut d = Diagnostics::new(LogLevels::default());
        let as_of = AsOf::At(chrono::DateTime::UNIX_EPOCH);
        // A matching catalog and frame as-of allow the resolved marker to show.
        d.set_catalog(
            CatalogSnapshot {
                as_of: as_of.clone(),
                datasets: vec![dataset_catalog()],
                ..Default::default()
            },
            SystemTime::UNIX_EPOCH,
        );
        let rows = data_rows(&d, &as_of, &BTreeSet::new(), Clock::utc());
        let gen1 = rows.iter().find(|r| r.text.contains("gen 1")).unwrap();
        assert_eq!(gen1.tone, Tone::Marked);
        let gen2 = rows.iter().find(|r| r.text.contains("gen 2")).unwrap();
        assert_eq!(gen2.tone, Tone::Normal);
    }

    /// After the frame changes as-of, suppress resolved-generation markers
    /// until a matching catalog arrives. The held snapshot may resolve a
    /// different generation.
    #[test]
    fn data_rows_suppresses_the_marker_when_the_snapshot_as_of_does_not_match_the_frames() {
        let mut d = Diagnostics::new(LogLevels::default());
        let stale = AsOf::At(chrono::DateTime::UNIX_EPOCH);
        d.set_catalog(
            CatalogSnapshot {
                as_of: stale,
                datasets: vec![dataset_catalog()],
                ..Default::default()
            },
            SystemTime::UNIX_EPOCH,
        );
        let current = AsOf::At(chrono::DateTime::UNIX_EPOCH + chrono::Duration::hours(1));
        let rows = data_rows(&d, &current, &BTreeSet::new(), Clock::utc());
        let gen1 = rows.iter().find(|r| r.text.contains("gen 1")).unwrap();
        assert_eq!(
            gen1.tone,
            Tone::Normal,
            "a snapshot resolved under a DIFFERENT as-of must not mark a generation"
        );
    }

    #[test]
    fn a_collapsed_dataset_shows_only_its_header_row() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.set_catalog(
            CatalogSnapshot {
                datasets: vec![dataset_catalog()],
                ..Default::default()
            },
            SystemTime::UNIX_EPOCH,
        );
        let mut collapsed = BTreeSet::new();
        collapsed.insert("risk".to_string());
        let rows = data_rows(&d, &AsOf::Live, &collapsed, Clock::utc());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].collapsible, Some(false));
    }

    #[test]
    fn config_rows_list_diagnostics_then_the_explainer_filtered_by_path() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.note_config(
            vec![Diagnostic::error(
                Layer::User,
                PathBuf::from("app.toml"),
                "bad",
            )],
            SystemTime::UNIX_EPOCH,
        );
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "config_version = 1\n[theme]\nname = \"Solarized\"\n")
                    .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let rows = config_rows(&d, &config, "", Clock::utc());
        let joined: String = rows
            .iter()
            .map(|r| r.text.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("[user] app.toml: bad"));
        assert!(joined.contains("theme.name = \"Solarized\""));
        assert!(joined.contains("[builtin]"));

        let filtered = config_rows(&d, &config, "theme", Clock::utc());
        assert!(filtered.iter().all(|r| r.text.contains("theme")));
        assert!(!filtered.is_empty());
    }

    /// An array of tables such as keymap `[[bindings]]` must produce an indexed
    /// path per leaf instead of one stringified array row.
    #[test]
    fn config_rows_recurses_into_arrays_with_indexed_paths() {
        let d = Diagnostics::new(LogLevels::default());
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "keymap",
                    "[[bindings]]\ncontext = \"tile\"\n[bindings.keys]\nj = \"down\"\n\
                     [[bindings]]\ncontext = \"other\"\n[bindings.keys]\nk = \"up\"\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let rows = config_rows(&d, &config, "", Clock::utc());
        let texts: Vec<String> = rows.iter().map(|r| r.text.to_string()).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("keymap.bindings.0.context") && t.contains("\"tile\"")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("keymap.bindings.1.keys.k") && t.contains("\"up\"")),
            "{texts:?}"
        );
        assert!(
            texts.iter().all(|t| t.len() < 200),
            "no row should be a whole array stringified onto one line: {texts:?}"
        );
    }

    /// Documents exceeding `MAX_LEAVES_PER_DOC` display a trailing omitted-count
    /// row instead of an unbounded list.
    #[test]
    fn config_rows_caps_leaves_per_doc_with_a_more_row() {
        let d = Diagnostics::new(LogLevels::default());
        // No `config_version` key — builtin docs skip that check
        // (`LayerDoc::builtin`'s own doc comment), and adding one would
        // be an extra leaf outside `[huge]`, throwing off the exact
        // "N more" count this test pins.
        let mut text = String::from("[huge]\n");
        for i in 0..(MAX_LEAVES_PER_DOC + 50) {
            text.push_str(&format!("k{i} = {i}\n"));
        }
        let config = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", &text).unwrap()],
            desk: None,
            user: None,
        });
        let rows = config_rows(&d, &config, "", Clock::utc());
        let more_row = rows.iter().find(|r| r.text.contains("more"));
        assert!(more_row.is_some(), "expected a trailing '… N more' row");
        assert!(more_row.unwrap().text.contains("50"));
        let leaf_rows = rows
            .iter()
            .filter(|r| r.text.starts_with("app.huge."))
            .count();
        assert_eq!(leaf_rows, MAX_LEAVES_PER_DOC);
    }

    /// Measure `config_rows` with demo documents and the builtin keymap,
    /// including indexed array leaves. This measures row construction only;
    /// it excludes layout, text shaping, and painting. The 10 ms assertion is
    /// a generous sanity bound, not proof of the 8 ms pure-UI budget.
    ///
    /// Run with `cargo test -p geode-diagnostics --lib
    /// sections::tests::config_rows_on_the_demo_config_stays_under_budget
    /// -- --nocapture` to print the measurement. See `docs/current/performance.md`
    /// for budgets and measurement limits.
    #[test]
    fn config_rows_on_the_demo_config_stays_under_budget() {
        let d = Diagnostics::new(LogLevels::default());
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "app",
                    include_str!("../../../examples/demo-config/app.toml"),
                )
                .unwrap(),
                LayerDoc::builtin(
                    "datasets",
                    include_str!("../../../examples/demo-config/datasets.toml"),
                )
                .unwrap(),
                LayerDoc::builtin(
                    "dimensions",
                    include_str!("../../../examples/demo-config/dimensions.toml"),
                )
                .unwrap(),
                LayerDoc::builtin(
                    "groupings",
                    include_str!("../../../examples/demo-config/groupings.toml"),
                )
                .unwrap(),
                LayerDoc::builtin(
                    "views",
                    include_str!("../../../examples/demo-config/views.toml"),
                )
                .unwrap(),
                LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP).unwrap(),
            ],
            desk: None,
            user: None,
        });

        let start = std::time::Instant::now();
        let rows = config_rows(&d, &config, "", Clock::utc());
        let elapsed = start.elapsed();
        println!(
            "config_rows on the demo config + builtin keymap: {} rows in {:?}",
            rows.len(),
            elapsed
        );
        assert!(!rows.is_empty());
        assert!(
            elapsed < Duration::from_millis(10),
            "config_rows took {elapsed:?} on the demo config — record the real number in \
             docs/perf.md and profile config leaf traversal and row formatting"
        );
    }

    #[test]
    fn log_rows_filter_by_target_or_level_text() {
        let records = vec![
            Record {
                at: SystemTime::UNIX_EPOCH,
                level: geode_core::log::Level::INFO,
                target: "geode::ingest",
                message: "loaded".into(),
                seq: 1,
            },
            Record {
                at: SystemTime::UNIX_EPOCH,
                level: geode_core::log::Level::WARN,
                target: "geode::query",
                message: "slow".into(),
                seq: 2,
            },
        ];
        let by_target = log_rows(&records, "ingest", Clock::utc());
        assert_eq!(by_target.len(), 1);
        assert!(by_target[0].text.contains("geode::ingest"));

        let by_level = log_rows(&records, "WARN", Clock::utc());
        assert_eq!(by_level.len(), 1);
        assert!(by_level[0].text.contains("geode::query"));
        assert_eq!(by_level[0].tone, Tone::Warn);
    }

    #[test]
    fn log_rows_stamp_each_record_on_the_clock() {
        let at: SystemTime = chrono::DateTime::parse_from_rfc3339("2026-09-18T22:00:00Z")
            .unwrap()
            .to_utc()
            .into();
        let records = vec![Record {
            at,
            level: geode_core::log::Level::INFO,
            target: "geode::ingest",
            message: "loaded".into(),
            seq: 1,
        }];
        let rows = log_rows(&records, "", Clock::in_zone_named("Asia/Tokyo"));
        assert!(rows[0].text.starts_with("07:00:00"), "{}", rows[0].text);
    }

    #[test]
    fn perf_rows_carry_p50_p95_max_and_dropped() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.watch();
        let mut hist = geode_shell::perf::FrameHistogram::new();
        hist.record_micros(1_000);
        hist.record_micros(2_000);
        d.refresh_frame_hist(&hist);
        d.note_dropped(3);

        let mut requery = RequeryStats::new();
        requery.record_submit_to_snapshot(5_000);
        requery.record_snapshot_to_paint(1_000);

        let rows = perf_rows(&d, &requery);
        let joined: String = rows
            .iter()
            .map(|r| r.text.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("p50"));
        assert!(joined.contains("p95"));
        assert!(joined.contains("max"));
        assert!(joined.contains("dropped events: 3"));
    }

    /// The perf section displays database size, memory use, and thread count
    /// from the catalog outcome.
    #[test]
    fn perf_rows_show_database_bytes_memory_bytes_and_threads_once_a_catalog_arrives() {
        let mut d = Diagnostics::new(LogLevels::default());
        let requery = RequeryStats::new();

        // No catalog yet: nothing to show, and nothing claims otherwise.
        let rows = perf_rows(&d, &requery);
        let joined: String = rows.iter().map(|r| r.text.to_string()).collect();
        assert!(!joined.contains("database"), "{joined}");

        d.set_catalog(
            CatalogSnapshot {
                datasets: Vec::new(),
                database_bytes: 12_345_678,
                used_blocks: 10,
                block_size: 262_144,
                memory_bytes: 987_654,
                threads: 8,
                ..Default::default()
            },
            SystemTime::UNIX_EPOCH,
        );
        let rows = perf_rows(&d, &requery);
        let joined: String = rows
            .iter()
            .map(|r| r.text.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("database"), "{joined}");
        assert!(joined.contains("memory"), "{joined}");
        assert!(joined.contains("threads 8"), "{joined}");
    }
}
