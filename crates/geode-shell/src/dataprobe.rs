//! The throwaway data probe (spec §7's vertical slice).
//!
//! **This is deliberately throwaway.** It exists because the §7.1 requery
//! budget is specified end-to-end — query, snapshot handoff, first painted
//! frame — and the first two can be benchmarked headless while the third
//! cannot. The blotter deletes this in phase 3, so nothing here is worth
//! making pretty. It is worth making *truthful*: every line it paints is a
//! claim phase 2 makes, shown against a real database.
//!
//! **Why it holds no `DataService`.** The workspace layering (CLAUDE.md)
//! is that `shell` and `data` never depend on each other, and only
//! `geode-app` sees both. So the probe renders from `geode-core` types
//! alone — a `Snapshot` and its `Provenance` — and the binary owns the
//! service, polls its result channel, and pushes readings in through
//! [`ProbeState`]. Keeping the tile on the shell side of that line is what
//! stops a throwaway diagnostic from quietly inverting the dependency
//! graph on its way out.
//!
//! Rendering cost is bounded by [`MAX_ROWS`], not by the result: a query
//! returning 400k rows paints 50. Cells are formatted per painted frame,
//! which is the one place this file departs from the no-per-frame-churn
//! rule — acceptable for a diagnostic that only paints when toggled on,
//! and another reason it is not the blotter.

use gpui::prelude::*;
use gpui::{App, IntoElement, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};
use std::sync::Arc;

use crate::fonts;
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::snapshot::Snapshot;

/// Rows painted. The point is to see the tree's shape and the attribution
/// rules, both of which are visible in the first screenful.
pub const MAX_ROWS: usize = 50;

const MARGIN: f32 = 8.0;

/// Marks a value that is real for its own row but must not be totalled.
const DETERMINED_MARK: &str = " †";

/// What the probe shows, in `geode-core` terms only — see the module doc
/// for why it cannot name a `DataService`.
#[derive(Default)]
pub struct ProbeState {
    pub snapshot: Option<Arc<Snapshot>>,
    /// `(book, as-of, generation)`, so per-book staleness is visible
    /// rather than collapsed into one headline time (spec §4.5).
    pub freshness: Vec<(String, String, i64)>,
    /// Submit-to-snapshot, the §7.1 path minus the paint.
    pub query_micros: u64,
    /// Shown instead of a table when the query failed.
    pub error: Option<String>,
}

impl ProbeState {
    /// Whether anything has arrived yet.
    pub fn is_empty(&self) -> bool {
        self.snapshot.is_none() && self.error.is_none()
    }
}

/// One cell's text, or `None` where the column has no value for this row.
///
/// A rolled-up level carries NULL in the grouping columns below it, and a
/// measure is blanked outright where it is `NonAttributable`, so "no text"
/// is a normal outcome rather than a lookup failure.
fn cell_text(snap: &Snapshot, column: &str, row: usize) -> Option<String> {
    // Per-cell, not over `f64_column`: the raw value buffer cannot express
    // a NULL, so a measure blanked as `NonAttributable` would render as
    // "0.00" — a number the data does not claim (spec §6.3).
    if snap.f64_column(column).is_some() {
        return snap.f64_value(column, row).map(|v| format!("{v:.2}"));
    }
    if let Some(values) = snap.i64_column(column) {
        return values.get(row).map(|v| v.to_string());
    }
    // Dimension columns come back dictionary-encoded on the live path and
    // as plain strings under as-of (spec §6.5, §7.2). `text_value` reads
    // either, so the probe does not have to know which era it is showing.
    snap.text_value(column, row).map(str::to_string)
}

/// A `label → value` line, matching the perf overlay's readout shape.
fn line(label: &'static str, value: String, cx: &App) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .w(px(96.))
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().font_family(fonts::MONO).child(value))
}

/// Build the probe panel. A pure function of state + theme, like
/// [`crate::shell::perf_overlay::render`] — no stored state, no timer, and
/// no side effects.
pub fn render(state: &ProbeState, toolbar_height: f32, cx: &App) -> impl IntoElement {
    let theme = cx.theme();

    let mut panel = v_flex()
        .absolute()
        .left(px(MARGIN))
        .right(px(MARGIN))
        .top(px(toolbar_height + MARGIN))
        .bottom(px(MARGIN))
        .gap_2()
        .p_3()
        .text_sm()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(px(8.))
        .overflow_hidden()
        // Test-only hook (no-op outside test builds), same pattern as the
        // perf overlay, so a #[gpui::test] can confirm it painted.
        .debug_selector(|| "data-probe".to_string());

    // Per-book freshness first: a result is only as meaningful as the
    // staleness of what fed it.
    let freshness = if state.freshness.is_empty() {
        "—".to_string()
    } else {
        state
            .freshness
            .iter()
            .map(|(book, as_of, generation)| format!("{book} · {as_of} · gen {generation}"))
            .collect::<Vec<_>>()
            .join("    ")
    };
    panel = panel.child(line("freshness", freshness, cx));

    if let Some(error) = &state.error {
        return panel.child(
            div()
                .text_color(theme.danger)
                .child(format!("query failed: {error}")),
        );
    }
    let Some(snap) = &state.snapshot else {
        return panel.child(
            div()
                .text_color(theme.muted_foreground)
                .child("no query has returned yet"),
        );
    };

    panel = panel.child(line(
        "result",
        format!(
            "{} rows · {:.1} ms",
            snap.rows(),
            state.query_micros as f64 / 1000.0
        ),
        cx,
    ));

    let columns = snap.column_names();
    let painted = snap.rows().min(MAX_ROWS);

    // Header. `row_depth` is the compiler's own marker, shown as the
    // leading column because the tree's shape is the thing to check.
    let cell = |width: f32| div().w(px(width)).overflow_hidden();
    let width_of = |name: &str| if name == "row_depth" { 56.0 } else { 132.0 };
    let mut header = h_flex().w_full().gap_2().text_color(theme.muted_foreground);
    for name in &columns {
        header = header.child(cell(width_of(name)).child(name.to_string()));
    }
    panel = panel.child(header);

    let mut any_determined = false;
    let mut semi_joined: Vec<String> = Vec::new();
    for row in 0..painted {
        let depth = snap.depth_of_row(row).unwrap_or(0);
        let mut line = h_flex().w_full().gap_2().font_family(fonts::MONO);
        for name in &columns {
            let meta = snap.meta(name);
            let attribution = meta
                .and_then(|m| m.attribution_by_depth.get(depth).copied())
                .unwrap_or(Attribution::Additive);
            if let Some(m) = meta
                && let ScopeSemantics::SemiJoined { dimensions } = &m.scope_semantics
            {
                for d in dimensions {
                    if !semi_joined.contains(d) {
                        semi_joined.push(d.clone());
                    }
                }
            }

            let element = match attribution {
                // The value belongs to an ancestor row, not this one, so
                // there is nothing honest to put here.
                Attribution::NonAttributable => cell(width_of(name)),
                // Real for this row, wrong to total — dimmed and marked,
                // never silently mixed in with the additive columns.
                Attribution::DeterminedNonAdditive => {
                    any_determined = true;
                    match cell_text(snap, name, row) {
                        None => cell(width_of(name)),
                        Some(text) => cell(width_of(name))
                            .text_color(theme.muted_foreground)
                            .child(format!("{text}{DETERMINED_MARK}")),
                    }
                }
                Attribution::Additive => match cell_text(snap, name, row) {
                    None => cell(width_of(name)),
                    Some(text) => cell(width_of(name)).child(text),
                },
            };
            line = line.child(element);
        }
        panel = panel.child(line);
    }

    if snap.rows() > painted {
        panel = panel.child(
            div()
                .text_color(theme.muted_foreground)
                .child(format!("… {} more rows", snap.rows() - painted)),
        );
    }
    if any_determined {
        panel = panel.child(div().text_color(theme.muted_foreground).child(format!(
            "{DETERMINED_MARK} shown for this row, do not total"
        )));
    }
    if !semi_joined.is_empty() {
        // "positions with SPX risk" must never read as "the SPX share".
        panel = panel.child(div().text_color(theme.muted_foreground).child(format!(
            "scoped by membership on {}: these are the whole entities that \
             qualify, not their share of {}",
            semi_joined.join(", "),
            semi_joined.join(", ")
        )));
    }

    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::snapshot::{ColumnMeta, TestColumn};

    fn meta(name: &str, by_depth: Vec<Attribution>, semantics: ScopeSemantics) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: by_depth,
            scope_semantics: semantics,
        }
    }

    /// Two levels of a tree with one additive and one coarse measure —
    /// the §6.3 example, small enough to assert on.
    fn snapshot() -> Snapshot {
        Snapshot::for_tests(
            vec![
                (
                    meta(
                        "lhu",
                        vec![Attribution::Additive; 2],
                        ScopeSemantics::Direct,
                    ),
                    TestColumn::Str(vec![None, Some("LHU1")]),
                ),
                (
                    meta(
                        "row_depth",
                        vec![Attribution::Additive; 2],
                        ScopeSemantics::Direct,
                    ),
                    TestColumn::I64(vec![0, 1]),
                ),
                (
                    meta(
                        "delta01",
                        vec![Attribution::Additive; 2],
                        ScopeSemantics::Direct,
                    ),
                    TestColumn::F64(vec![Some(30.0), Some(30.0)]),
                ),
                (
                    meta(
                        "daily_trading_pnl",
                        vec![Attribution::Additive, Attribution::DeterminedNonAdditive],
                        ScopeSemantics::SemiJoined {
                            dimensions: vec!["underlying_ref".into()],
                        },
                    ),
                    TestColumn::F64(vec![Some(7.0), Some(7.0)]),
                ),
            ],
            1,
        )
    }

    #[test]
    fn cells_are_read_from_whichever_column_type_holds_them() {
        let s = snapshot();
        assert_eq!(cell_text(&s, "delta01", 0).as_deref(), Some("30.00"));
        assert_eq!(cell_text(&s, "row_depth", 1).as_deref(), Some("1"));
        assert_eq!(cell_text(&s, "lhu", 1).as_deref(), Some("LHU1"));
    }

    #[test]
    fn a_rolled_up_grouping_column_has_no_text_rather_than_an_empty_string() {
        // Depth 0 is the grand total: it belongs to no LHU, and rendering
        // "" would claim it belongs to one with an empty name.
        let s = snapshot();
        assert_eq!(cell_text(&s, "lhu", 0), None);
    }

    #[test]
    fn an_absent_column_is_none_not_a_panic() {
        let s = snapshot();
        assert_eq!(cell_text(&s, "nonesuch", 0), None);
        assert_eq!(cell_text(&s, "delta01", 99), None);
    }

    #[test]
    fn a_measure_blanked_as_non_attributable_renders_blank_not_zero() {
        // Cross gamma at an underlying-level grouping belongs to no single
        // underlying, so the compiler emits NULL rather than a number
        // (spec §6.3). Rendering that as "0.00" states a quantity the data
        // does not claim — and the row beside it carries a real 0.0, which
        // must still render.
        let s = Snapshot::for_tests(
            vec![(
                meta(
                    "cross_gamma",
                    vec![Attribution::NonAttributable, Attribution::Additive],
                    ScopeSemantics::Direct,
                ),
                TestColumn::F64(vec![None, Some(0.0)]),
            )],
            1,
        );
        assert_eq!(cell_text(&s, "cross_gamma", 0), None, "NULL is not 0.00");
        assert_eq!(
            cell_text(&s, "cross_gamma", 1).as_deref(),
            Some("0.00"),
            "a real zero still renders"
        );
    }

    #[test]
    fn an_empty_probe_knows_it_has_nothing_to_show() {
        let mut state = ProbeState::default();
        assert!(state.is_empty());
        state.snapshot = Some(Arc::new(snapshot()));
        assert!(!state.is_empty());
    }
}
