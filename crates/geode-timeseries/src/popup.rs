//! The tile's one overlay (spec §9.5–§9.8): the add picker, the
//! expression editor, the series list, the range dialog. Tasks 8–10
//! build them; this task builds the series list ([`Popup::Series`]).
//!
//! **One popup at a time, and it is prepared, never formatted.** The
//! list's rows are built in the tile's `rebuild_chrome` — the same door
//! the header's chips go through, on the same changes — so a row's
//! label, its `source · rule`, its axis letter, its state word and its
//! already-resolved swatch are `SharedString`s and `Hsla`s the painter
//! clones. Resolving a slot's colour costs a `Palette::from_theme` plus
//! the named-colour wheel; doing that per frame for an open list would
//! pay it for a value that moves only when the model or the theme does
//! (the header module's own rule).
//!
//! The surface is the market-data popup's, deliberately: gpui-
//! component's `popover_style` with `PopupMenu`'s row geometry on
//! Geode's rem scale, `deferred(anchored(..))` so it escapes the tile's
//! clip and paints above its neighbours, `occlude()` so the chart below
//! stops hit-testing under it, and an `on_mouse_down_out` into the ONE
//! closer. No animation, no gpui-component `Dialog`.

use geode_core::health::Health;
use geode_core::series::{SeriesResult, SlotKind};
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Deferred, Div, Entity, Hsla, MouseButton, SharedString,
    anchored, deferred, div, px,
};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};

use crate::core::model::{Colour, Model, SlotState};
use crate::tile::TimeseriesTile;

/// A row's height, in pixels at the design rem — gpui-component's own
/// `PopupMenu` item height, so this popup keeps the menu family's
/// geometry (design guide: "preserve the component family's geometry")
/// while following Geode's rem.
const ROW_HEIGHT: f32 = 26.0;
/// A row's horizontal inset — `PopupMenu`'s `INNER_PADDING`.
const ROW_INSET: f32 = 8.0;
/// The popup's minimum width at the design rem.
const MIN_WIDTH: f32 = 240.0;
/// The swatch beside a row's label, matching the header chip's.
const SWATCH: f32 = 8.0;

/// What the tile currently has open. `Series` is the series list (spec
/// §9.5) — it holds no field, so it is NOT an insert-mode popup: the key
/// context stays `normal` and gains a `popup == series` pair, which is
/// what its three keys bind against. Tasks 9–10 add the add picker, the
/// expression editor and the range dialog, whose fields DO hold the
/// keyboard.
pub(crate) enum Popup {
    Series(SeriesPopup),
}

impl Popup {
    /// Whether this popup holds the keyboard as a text field — what puts
    /// the tile's key context into `insert` mode.
    pub(crate) fn is_insert(&self) -> bool {
        match self {
            Popup::Series(_) => false,
        }
    }

    /// Whether one of this popup's own inputs holds WINDOW focus right
    /// now (`TileContent::holds_focus`'s ownership half) — answered off
    /// the focus handle, never off the mode.
    pub(crate) fn holds_focus(&self, _window: &gpui::Window, _cx: &gpui::App) -> bool {
        match self {
            // No field: the tile itself keeps the keyboard, which is
            // what lets `j`/`k` reach the matcher at all.
            Popup::Series(_) => false,
        }
    }

    /// The value this popup gives the key context's `popup` pair, or
    /// `None` for one whose keys are the shared `mode == insert`
    /// layer's.
    pub(crate) fn context_pair(&self) -> Option<&'static str> {
        match self {
            Popup::Series(_) => Some("series"),
        }
    }
}

/// The series list, prepared (spec §9.5): one row per slot, in slot
/// order, highlighted at the CHIPS' cursor — the list and the strip show
/// one cursor between them, which is why a row click and a chip click
/// are the same door.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SeriesPopup {
    pub rows: Vec<SeriesRow>,
}

/// One slot as the list paints it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SeriesRow {
    /// The chip's own label: the identity (`@source` only when it is not
    /// the default), or the expression's text.
    pub label: SharedString,
    /// `{source} · {rule}` for a source slot; `expr` for an expression,
    /// which has neither.
    pub source_rule: SharedString,
    /// `L`, `R`, `L2`, `R2` — the axis letter the chip shows.
    pub axis: &'static str,
    /// `fetching`, `failed: <reason>`, `degraded`, `failed`, or empty.
    pub state: SharedString,
    /// Resolved against the theme here, like the chip's (see the module
    /// doc).
    pub swatch: Hsla,
    pub hidden: bool,
}

impl SeriesPopup {
    /// Build every row from the model and the last good result. Called
    /// from the tile's `rebuild_chrome` while the list is open, and
    /// nowhere else.
    ///
    /// Takes the colour resolver the header's own `prepare` takes, and
    /// for the same reason: the tile derives the wheel ONCE per chrome
    /// rebuild and hands it to both, so a row's swatch and its chip's
    /// agree by construction rather than by two call sites keeping step.
    pub(crate) fn prepare(
        model: &Model,
        result: Option<&SeriesResult>,
        default_source: Option<&str>,
        colour_of: &dyn Fn(&Colour) -> Hsla,
    ) -> SeriesPopup {
        let rows = model
            .slots()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let source_rule = match &s.kind {
                    SlotKind::Source { source, rule, .. } => {
                        format!("{source} · {}", rule.as_str())
                    }
                    SlotKind::Expr(_) => "expr".to_string(),
                };
                SeriesRow {
                    label: model.label(i, default_source).into(),
                    source_rule: source_rule.into(),
                    axis: s.axis.letter(),
                    state: state_text(s.number, &s.state, result),
                    swatch: colour_of(&s.colour),
                    hidden: !s.visible,
                }
            })
            .collect();
        SeriesPopup { rows }
    }
}

/// A slot's state word. The SLOT's own state outranks the delivered
/// provenance: a fetch that is out or that failed is news about this
/// tile's own request, while health is news about the source behind the
/// answer it already has.
fn state_text(number: u8, state: &SlotState, result: Option<&SeriesResult>) -> SharedString {
    match state {
        SlotState::Fetching => return "fetching".into(),
        SlotState::Failed(why) => return format!("failed: {why}").into(),
        SlotState::Idle => {}
    }
    // Keyed by slot NUMBER: a result's slots are the request's, and a
    // removal makes the model's index a different thing entirely.
    let health = result
        .and_then(|r| r.slots.iter().find(|s| s.slot == number))
        .and_then(|s| s.provenance.health.as_ref());
    match health {
        Some(Health::Degraded { .. }) => "degraded".into(),
        Some(Health::Failed { .. }) => "failed".into(),
        _ => SharedString::default(),
    }
}

/// The popup surface: gpui-component's own popover treatment
/// (`popover_style` — the popover background and foreground, the
/// ring-in-shadow edge, `theme.radius`), then the item container's `p_1`
/// inset. The market-data popup's own surface, spelled the same way, so
/// the two cannot drift apart.
fn popover_surface(cx: &App) -> Div {
    v_flex()
        .min_w(scale::design(MIN_WIDTH))
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .popover_style(cx)
}

/// Paint the series list. `cursor` is the CHIPS' cursor — the strip and
/// the list share one, so the highlighted row is the chip with the fill.
pub(crate) fn render_series_popup(
    p: &SeriesPopup,
    cursor: Option<usize>,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    cx: &App,
) -> Deferred {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(move || format!("ts-list-{tile_id}"))
        // Without this gpui keeps hit-testing the chart painted beneath
        // the popup (market-data's own user report, 2026-09-17).
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_popup_with_window(window, cx))
        });
    if p.rows.is_empty() {
        // "Asked and answered" rather than a blank rectangle — and it
        // names the keys that end the state, per the empty-state rule.
        return anchor_popup(
            list.child(
                div()
                    .h(scale::design(ROW_HEIGHT))
                    .px(scale::design(ROW_INSET))
                    .flex()
                    .items_center()
                    .text_color(theme.muted_foreground)
                    .child(crate::header::EMPTY_HINT),
            ),
        );
    }
    for (i, row) in p.rows.iter().enumerate() {
        let highlighted = cursor == Some(i);
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .gap_2()
                .rounded(theme.radius)
                .items_center()
                // No hover state: the highlight follows the CURSOR, and
                // a second fill under the pointer would read as a second
                // selection (CLAUDE.md — the market-data popup's rows
                // take none either).
                .when(highlighted, |d| {
                    d.bg(theme.accent).text_color(theme.accent_foreground)
                })
                .when(!highlighted, |d| d.text_color(theme.popover_foreground))
                // A hidden series stays in the list, struck through, for
                // the same reason its chip does: `v` is a toggle, and a
                // row that vanished would leave nothing to press again.
                .when(row.hidden, |d| d.opacity(0.5).line_through())
                .debug_selector(move || format!("ts-list-row-{tile_id}-{i}"))
                // The mouse form of `j`/`k`, and the chip click's own
                // door: one cursor between the strip and the list.
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, _window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.chip_clicked(i, cx));
                    }
                })
                .child(
                    div()
                        .size(scale::design(SWATCH))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(row.swatch),
                )
                .child(div().flex_1().child(row.label.clone()))
                .child(
                    div()
                        .text_xs()
                        .when(!highlighted, |d| d.text_color(theme.muted_foreground))
                        .child(row.source_rule.clone()),
                )
                .child(div().text_xs().child(row.axis))
                .when(!row.state.is_empty(), |d| {
                    d.child(div().text_xs().child(row.state.clone()))
                }),
        );
    }
    anchor_popup(list)
}

/// The anchored, deferred wrapper this popup and Tasks 9–10's will both
/// take: `Local` position mode against the `relative()` wrapper the tile
/// paints round its header, snapped inside the window.
fn anchor_popup(list: Div) -> Deferred {
    deferred(
        anchored()
            .anchor(Anchor::TopRight)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(8.))
            .child(list),
    )
    .with_priority(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::series::expr::Expr;
    use geode_core::series::{BucketRule, SlotProvenance, SlotResult};

    /// The header tests' resolver: the palette index straight into the
    /// hue, so one row's swatch can be told from another's without a
    /// window.
    fn stub(colour: &Colour) -> Hsla {
        match colour {
            Colour::Palette(i) => gpui::hsla(*i as f32 / 10.0, 1.0, 0.5, 1.0),
            Colour::Named(_) => gpui::black(),
        }
    }

    fn model() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_rest", "series").unwrap();
        m.add_expr("s1 / s2", Expr::Ref(1)).unwrap();
        m
    }

    fn result(slot: u8, health: Option<Health>) -> SeriesResult {
        SeriesResult {
            buckets: vec![0],
            slots: vec![SlotResult {
                slot,
                values: vec![1.0],
                percentiles: Vec::new(),
                bins: Vec::new(),
                provenance: SlotProvenance {
                    loaded: None,
                    latest_received_at: None,
                    health,
                },
            }],
        }
    }

    #[test]
    fn a_row_carries_the_label_the_source_and_rule_the_axis_and_the_swatch() {
        let mut m = model();
        m.set_rule(2, BucketRule::Mean).unwrap();
        let p = SeriesPopup::prepare(&m, None, Some("demo_kdb"), &stub);
        assert_eq!(p.rows.len(), 3);
        assert_eq!(p.rows[0].label.as_ref(), "SPX.close");
        assert_eq!(p.rows[0].source_rule.as_ref(), "demo_kdb · last");
        assert_eq!(
            p.rows[1].label.as_ref(),
            "VIX@demo_rest",
            "the source shows when it is not the default"
        );
        assert_eq!(p.rows[1].source_rule.as_ref(), "demo_rest · mean");
        assert_eq!(
            p.rows[2].source_rule.as_ref(),
            "expr",
            "an expression has neither a source nor a rule"
        );
        assert_eq!(p.rows[2].label.as_ref(), "s1 / s2");
        assert_eq!(p.rows[0].axis, "L");
        assert_eq!(p.rows[0].swatch, stub(&Colour::Palette(0)));
        assert_eq!(p.rows[1].swatch, stub(&Colour::Palette(1)));
        assert!(!p.rows[0].hidden);
    }

    #[test]
    fn the_slots_own_state_outranks_the_delivered_health() {
        let mut m = model();
        // Every slot starts out waiting for its first fetch.
        let p = SeriesPopup::prepare(&m, None, Some("demo_kdb"), &stub);
        assert_eq!(p.rows[0].state.as_ref(), "fetching");
        m.set_state(1, SlotState::Failed("no route".into()));
        let p = SeriesPopup::prepare(&m, None, Some("demo_kdb"), &stub);
        assert_eq!(p.rows[0].state.as_ref(), "failed: no route");
        // Degraded health reaches an IDLE slot, keyed by slot NUMBER…
        m.set_state(1, SlotState::Idle);
        let degraded = result(
            1,
            Some(Health::Degraded {
                reason: "stale".into(),
            }),
        );
        let p = SeriesPopup::prepare(&m, Some(&degraded), Some("demo_kdb"), &stub);
        assert_eq!(p.rows[0].state.as_ref(), "degraded");
        assert_eq!(p.rows[1].state.as_ref(), "fetching", "s2 is still waiting");
        // …and never over the tile's own failure.
        m.set_state(1, SlotState::Failed("no route".into()));
        let p = SeriesPopup::prepare(&m, Some(&degraded), Some("demo_kdb"), &stub);
        assert_eq!(p.rows[0].state.as_ref(), "failed: no route");
        // An idle slot with a healthy answer says nothing at all.
        m.set_state(1, SlotState::Idle);
        let ok = result(1, None);
        let p = SeriesPopup::prepare(&m, Some(&ok), Some("demo_kdb"), &stub);
        assert_eq!(p.rows[0].state.as_ref(), "");
    }

    #[test]
    fn a_hidden_slot_stays_in_the_list() {
        let mut m = model();
        m.set_visible(1, false).unwrap();
        let p = SeriesPopup::prepare(&m, None, Some("demo_kdb"), &stub);
        assert_eq!(p.rows.len(), 3);
        assert!(p.rows[0].hidden);
        assert!(!p.rows[1].hidden);
    }
}
