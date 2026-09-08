//! Pure row builders for the five diagnostics sections (spec §4.6). No
//! gpui rendering here beyond `SharedString` (a plain ref-counted string,
//! not a paint) — `tile.rs` maps `Tone` to theme colours and paints the
//! rows a builder returns. Every builder is a pure function of its
//! arguments: same inputs, same rows, every time — the diagnostics tile
//! calls one only when an observed version actually changed.

use std::collections::BTreeSet;
use std::time::SystemTime;

use chrono::{DateTime, Local, Utc};
use gpui::SharedString;

use geode_core::config::{Config, Diagnostic, Severity};
use geode_core::log::Record;
use geode_core::query::AsOf;
use geode_shell::diagnostics::{Diagnostics, Health};
use geode_shell::perf::{RequeryStats, format_ms};

/// A row's visual weight — `tile.rs` maps each to a theme colour; `Marked`
/// is the as-of-resolved-generation highlight (spec §4.6's "data" section).
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

/// `HH:MM:SS` on the trader's local clock (Phase 4a's ruling: local time
/// throughout) — the diagnostics tile's own convention for every
/// timestamp it shows, matching the frame's `short_time`-style readouts.
fn local_hms(t: SystemTime) -> String {
    DateTime::<Utc>::from(t)
        .with_timezone(&Local)
        .format("%H:%M:%S")
        .to_string()
}

fn local_hms_utc(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M:%S").to_string()
}

/// The "sources" section (spec §4.6): name, path, priority, readiness
/// rule, health with detail, since, last poll, next poll — sorted worst
/// health first (`Health`'s `Ord` is severity order, so `Failed` sorts
/// last there; the tile wants it first, hence `.rev()`), a source with no
/// real health note yet (`SourceState::health` is `None`) shown as "no
/// report yet" rather than counted as any particular health (matching
/// `Diagnostics::summary`'s own CRIT-1 fix), and placed after every
/// reported source.
pub fn sources_rows(d: &Diagnostics, now: SystemTime) -> Vec<Row> {
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
    for (name, state) in reported.into_iter().chain(unreported) {
        let Some(health) = &state.health else {
            out.push(row(format!("{name}: no report yet"), 0, Tone::Muted));
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
        let since = local_hms(state.since);
        let mut text = format!("{name}: {label}");
        if let Some(reason) = &reason
            && !reason.is_empty()
        {
            text.push_str(&format!(" — {reason}"));
        }
        // MIN-9 (fix round 1), accepted as-is: `now` is the instant this
        // whole section rebuilt, and a rebuild only happens on an
        // observed version change — a healthy, quiet source's "Ns ago"
        // freezes at whatever it read on the last real change until
        // something else bumps a version (the reload-poll tick's own
        // ~500ms `refresh_frame_hist`, most commonly). Honest but
        // occasionally stale; the absolute `since` timestamp right next
        // to it is always correct, which is why this stays a decoration
        // rather than the only clock reading on the row.
        let elapsed = now
            .duration_since(state.since)
            .map(|d| format!(" · {}s ago", d.as_secs()))
            .unwrap_or_default();
        text.push_str(&format!(" (since {since}{elapsed})"));
        out.push(row(text, 0, tone));
        push_spec_detail(&mut out, state);
        let mut poll = String::new();
        if let Some(last) = state.last_poll {
            poll.push_str(&format!("last poll {}", local_hms(last)));
        }
        if let Some(next) = state.next_poll {
            if !poll.is_empty() {
                poll.push_str(" · ");
            }
            poll.push_str(&format!("next poll {}", local_hms(next)));
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
    let paths = spec.paths.join(", ");
    out.push(row(
        format!(
            "path: {paths} · priority: {} · readiness: {}",
            spec.priority, spec.readiness
        ),
        1,
        Tone::Muted,
    ));
}

/// The "data" section (spec §4.6): dataset › partition (batch/book), each
/// generation with its id, published time, rows, live-or-archive, and the
/// generation the current `as_of` resolves to marked `Tone::Marked`. A
/// dataset in `collapsed` shows only its own header row.
pub fn data_rows(d: &Diagnostics, as_of: &AsOf, collapsed: &BTreeSet<String>) -> Vec<Row> {
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
                let marked = !as_of.is_live() && part.resolved_gen == Some(generation.gen_id);
                let tone = if marked { Tone::Marked } else { Tone::Normal };
                let loaded = generation
                    .loaded_at
                    .map(local_hms_utc)
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
                        local_hms_utc(generation.source_time)
                    ),
                    2,
                    tone,
                ));
            }
        }
    }
    out
}

/// The "config" section (spec §4.6): the current config-load batch, then
/// the data layer's own diagnostics, then the historical batches, then the
/// effective-config explainer (each known doc as a flat leaf list,
/// `path = value  [layer]` via `Config::explain`). `filter` is a plain
/// substring match on the whole row's text, applied uniformly across
/// every part of the section — so filtering to "theme" both narrows the
/// explainer to matching leaves and drops diagnostics that don't mention
/// it.
pub fn config_rows(d: &Diagnostics, config: &Config, filter: &str) -> Vec<Row> {
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
            out.push(row(format!("batch at {}", local_hms(*at)), 1, Tone::Muted));
            for diag in diags {
                out.push(diagnostic_row_at_depth(diag, 2));
            }
        }
    }

    out.push(row("effective config", 0, Tone::Muted));
    // MIN-3 (fix round 1): every doc `Config` actually holds, not a
    // hand-maintained list that could fall behind it.
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

/// A cap on how many leaf rows one doc contributes to the explainer
/// (Phase 4b Task 5 fix round 1, MAJ-8), a final "… N more" row standing
/// in for the rest — a pathological doc (or a future one nobody sized
/// this for) must not turn one `:section config` render into thousands
/// of rows.
const MAX_LEAVES_PER_DOC: usize = 2_000;

/// Walk every leaf of a merged doc's table, recursing into both nested
/// tables AND arrays (Phase 4b Task 5 fix round 1, MAJ-8) — the merged
/// `keymap` doc's `bindings` is an array of tables (`[[bindings]]`), and
/// before this fix `walk_value`'s `toml::Value::Array` case did not
/// exist, so an array was stringified whole via `Value::to_string()`:
/// one `Row` holding the entire keymap's bindings serialised onto a
/// single unbroken line, several kilobytes long, that `uniform_list`
/// cannot wrap and gpui reshapes on every paint while it's in the
/// visible range. Indexed paths (`keymap.bindings.0.keys.j`) keep every
/// leaf its own row instead.
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

/// The "log" section (spec §4.6): the ring's tail, oldest first. `filter`
/// is a plain substring match against the whole formatted row (so it
/// catches both a target like `ingest` — matched inside `geode::ingest` —
/// and a level like `WARN`).
pub fn log_rows(records: &[Record], filter: &str) -> Vec<Row> {
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
                    local_hms(r.at),
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

/// The "perf" section (spec §4.6): the frame-time histogram's
/// percentiles, `RequeryStats`' two halves and its last reading, and the
/// dropped-event count.
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
    out
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
    fn sources_are_sorted_worst_first_with_their_detail() {
        let mut d = Diagnostics::new(LogLevels::default());
        let t = SystemTime::UNIX_EPOCH;
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec!["/data/*.csv".into()],
                priority: "latest_risk".into(),
                readiness: "sentinel".into(),
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
            },
        );

        let rows = sources_rows(&d, t);
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
    fn a_described_but_unreported_source_shows_no_report_yet_not_pending() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.describe_source(
            "risk",
            SourceSummary {
                paths: vec![],
                priority: "p".into(),
                readiness: "r".into(),
            },
        );
        let rows = sources_rows(&d, SystemTime::UNIX_EPOCH);
        assert_eq!(rows[0].text.as_ref(), "risk: no report yet");
        assert_eq!(rows[0].tone, Tone::Muted);
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
        }
    }

    #[test]
    fn data_rows_mark_the_resolved_generation_under_an_as_of() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.set_catalog(CatalogSnapshot {
            datasets: vec![dataset_catalog()],
            ..Default::default()
        });
        let as_of = AsOf::At(chrono::DateTime::UNIX_EPOCH);
        let rows = data_rows(&d, &as_of, &BTreeSet::new());
        let gen1 = rows.iter().find(|r| r.text.contains("gen 1")).unwrap();
        assert_eq!(gen1.tone, Tone::Marked);
        let gen2 = rows.iter().find(|r| r.text.contains("gen 2")).unwrap();
        assert_eq!(gen2.tone, Tone::Normal);
    }

    #[test]
    fn a_collapsed_dataset_shows_only_its_header_row() {
        let mut d = Diagnostics::new(LogLevels::default());
        d.set_catalog(CatalogSnapshot {
            datasets: vec![dataset_catalog()],
            ..Default::default()
        });
        let mut collapsed = BTreeSet::new();
        collapsed.insert("risk".to_string());
        let rows = data_rows(&d, &AsOf::Live, &collapsed);
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
        let rows = config_rows(&d, &config, "");
        let joined: String = rows
            .iter()
            .map(|r| r.text.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("[user] app.toml: bad"));
        assert!(joined.contains("theme.name = \"Solarized\""));
        assert!(joined.contains("[builtin]"));

        let filtered = config_rows(&d, &config, "theme");
        assert!(filtered.iter().all(|r| r.text.contains("theme")));
        assert!(!filtered.is_empty());
    }

    /// MAJ-8: an array of tables (`[[bindings]]`, the real shape the
    /// merged `keymap` doc's `bindings` key takes) must be recursed into
    /// with an indexed path per leaf, not stringified whole onto one row.
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
        let rows = config_rows(&d, &config, "");
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

    /// MAJ-8: a doc with more than `MAX_LEAVES_PER_DOC` leaves is capped,
    /// with a trailing "… N more" row rather than an unbounded list.
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
        let rows = config_rows(&d, &config, "");
        let more_row = rows.iter().find(|r| r.text.contains("more"));
        assert!(more_row.is_some(), "expected a trailing '… N more' row");
        assert!(more_row.unwrap().text.contains("50"));
        let leaf_rows = rows
            .iter()
            .filter(|r| r.text.starts_with("app.huge."))
            .count();
        assert_eq!(leaf_rows, MAX_LEAVES_PER_DOC);
    }

    /// MAJ-4's display-free measurement recipe (final review, ruling 4):
    /// times `config_rows` on the largest shipped config — the `--demo`
    /// layer's five docs (`app`, `datasets`, `dimensions`, `groupings`,
    /// `views`) plus the compiled-in builtin keymap, the biggest single
    /// doc in a real session (one leaf per binding key, after MAJ-8's
    /// array recursion) — standing in for a display, since no display
    /// was available to measure the tile's actual paint. Run with
    /// `cargo test -p geode-diagnostics --lib sections::tests::
    /// config_rows_on_the_demo_config_stays_under_budget -- --nocapture`
    /// to see the printed number; `docs/perf.md`'s Phase 4b section
    /// records what this measured. The assertion is a generous sanity
    /// bound (10ms — well inside §7.1's 8ms *pure-UI* budget would be a
    /// coincidence worth flagging, not the actual per-frame cost this
    /// stands in for; twice a second, not every frame), not a tight
    /// regression gate.
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
        let rows = config_rows(&d, &config, "");
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
             docs/perf.md and reconsider MAJ-4's fix (b)/(c) if this budget ever tightens"
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
        let by_target = log_rows(&records, "ingest");
        assert_eq!(by_target.len(), 1);
        assert!(by_target[0].text.contains("geode::ingest"));

        let by_level = log_rows(&records, "WARN");
        assert_eq!(by_level.len(), 1);
        assert!(by_level[0].text.contains("geode::query"));
        assert_eq!(by_level[0].tone, Tone::Warn);
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
}
