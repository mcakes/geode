//! The tile's one overlay (spec §9.5–§9.8): the add picker, the
//! expression editor, the series list, the range dialog. Task 8 built
//! the series list ([`Popup::Series`]), Task 9 the add picker
//! ([`Popup::Picker`], §9.6) and the expression field ([`Popup::Expr`],
//! §9.7); the range dialog is Task 10.
//!
//! **Two of the three hold the keyboard.** The picker's and the
//! expression field's `InputState`s are tile-owned and focused, which
//! is what puts the tile's key context into `insert` mode — and what
//! makes `TimeseriesTile::close_popup_with_window` the ONE closer:
//! a focused `InputState` dropped without a blur leaves `Window::
//! focused` pointing at a dead handle for the rest of the session
//! (CLAUDE.md).
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
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Deferred, Div, Entity, Focusable as _, Hsla, MouseButton,
    SharedString, anchored, deferred, div, px,
};
use gpui_component::input::{Input, InputState};
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
/// what its three keys bind against. `Picker` (§9.6) and `Expr` (§9.7)
/// each own a focused field, so both report `insert` and neither takes
/// a `popup` pair: their keys are the shared `mode == insert` layer's
/// (`enter`/`escape`/`up`/`down`), the market-data panel's own split.
/// Task 10 adds the range dialog.
pub(crate) enum Popup {
    Series(SeriesPopup),
    Picker(PickerState),
    Expr(ExprField),
}

impl Popup {
    /// Whether this popup holds the keyboard as a text field — what puts
    /// the tile's key context into `insert` mode.
    pub(crate) fn is_insert(&self) -> bool {
        match self {
            Popup::Series(_) => false,
            Popup::Picker(_) | Popup::Expr(_) => true,
        }
    }

    /// Whether one of this popup's own inputs holds WINDOW focus right
    /// now (`TileContent::holds_focus`'s ownership half) — answered off
    /// the focus handle, never off the mode: a tile-focus move can leave
    /// a field open without the keyboard (the market-data panel's I-3).
    pub(crate) fn holds_focus(&self, window: &gpui::Window, cx: &gpui::App) -> bool {
        match self {
            // No field: the tile itself keeps the keyboard, which is
            // what lets `j`/`k` reach the matcher at all.
            Popup::Series(_) => false,
            Popup::Picker(p) => p.input.read(cx).focus_handle(cx).is_focused(window),
            Popup::Expr(f) => f.input.read(cx).focus_handle(cx).is_focused(window),
        }
    }

    /// The value this popup gives the key context's `popup` pair, or
    /// `None` for one whose keys are the shared `mode == insert`
    /// layer's.
    pub(crate) fn context_pair(&self) -> Option<&'static str> {
        match self {
            Popup::Series(_) => Some("series"),
            Popup::Picker(_) | Popup::Expr(_) => None,
        }
    }
}

/// Which of the picker's two lists is up (spec §9.6). `Sources` carries
/// the identity the trader typed — it is not in any catalogue, so the
/// second step is the only place it can be paired with a source.
pub(crate) enum PickerStage {
    Identities,
    Sources { identity: String },
}

/// The add picker's state (spec §9.6): a tile-owned field that holds the
/// keyboard, one [`ChoiceList`] beneath it (the 2026-09-19 choice core —
/// one ranking, one identity-across-a-re-rank rule, one twelve-row
/// painted window), and the two things this surface adds to a plain
/// choice field.
///
/// `loaded` and `labels` run PARALLEL to `list.options()` and are both
/// prepared here, never in `render`: a row paints `identity` and
/// `@source` as two columns, and splitting the option string per painted
/// row per frame is the allocation the market-data picker's own review
/// took out (IMPORTANT-3 there).
pub(crate) struct PickerState {
    pub input: Entity<InputState>,
    pub list: ChoiceList,
    pub stage: PickerStage,
    /// `model.holds_pair(source, identity)` per option — a marked row is
    /// still pickable (a second slot over the same pair with another
    /// rule is legitimate, spec §9.6).
    pub loaded: Vec<bool>,
    /// The prepared `(identity, @source)` pair per option; the second
    /// half is empty in the `Sources` stage, whose options are bare
    /// source names.
    pub labels: Vec<(SharedString, SharedString)>,
    /// `add "<text>"…` while the typed text matches nothing — the door
    /// to the `Sources` stage, recomputed on every keystroke.
    pub add_row: Option<String>,
}

impl PickerState {
    /// The identities stage, ranked over `options` under an empty query.
    pub(crate) fn new(input: Entity<InputState>, options: Vec<String>, loaded: Vec<bool>) -> Self {
        PickerState {
            input,
            labels: labels_for(&options),
            list: ChoiceList::new(options, DEFAULT_CAP),
            stage: PickerStage::Identities,
            loaded,
            add_row: None,
        }
    }

    /// Swap in a fresh catalogue, keeping the live query and the
    /// highlighted option by TEXT ([`ChoiceList::replace_options`]).
    pub(crate) fn set_options(&mut self, options: Vec<String>, loaded: Vec<bool>) {
        self.labels = labels_for(&options);
        self.loaded = loaded;
        self.list.replace_options(options);
        self.refresh_add_row();
    }

    /// Step into the source stage: the sources become the options, the
    /// default is highlighted, and the typed identity is carried.
    pub(crate) fn enter_sources(
        &mut self,
        identity: String,
        sources: Vec<String>,
        default_source: Option<&str>,
    ) {
        self.stage = PickerStage::Sources { identity };
        self.labels = labels_for(&sources);
        self.loaded = vec![false; sources.len()];
        self.list = ChoiceList::new(sources, DEFAULT_CAP);
        self.list.place(default_source);
        self.add_row = None;
    }

    /// `add "<text>"…` exactly while the ranked list is empty and the
    /// query names something — the identities stage only: a source that
    /// matches nothing cannot be invented here.
    ///
    /// The text is TRIMMED (Task 9 review, minor 2), and the trimmed
    /// form is what the row shows, because it is what the commit stores
    /// as the identity: a query of nothing but spaces ranks nothing and
    /// would otherwise offer an `add "   "…` row that can only be inert.
    pub(crate) fn refresh_add_row(&mut self) {
        let text = self.list.query().trim().to_string();
        self.add_row = (matches!(self.stage, PickerStage::Identities)
            && self.list.ranked().is_empty()
            && !text.is_empty())
        .then(|| format!("add \"{text}\"…"));
    }

    /// The option at declared index `i`.
    pub(crate) fn option(&self, i: usize) -> &str {
        &self.list.options()[i]
    }

    #[cfg(test)]
    pub(crate) fn ranked_options(&self) -> Vec<String> {
        self.list
            .ranked()
            .iter()
            .map(|r| self.list.options()[r.row].clone())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn ranked_loaded(&self) -> Vec<bool> {
        self.list
            .ranked()
            .iter()
            .map(|r| self.loaded[r.row])
            .collect()
    }
}

/// The expression field (spec §9.7): a one-line tile-owned `Input` on
/// the strip below the header, its parse error painted under it. `error`
/// is the inline half — a bad expression keeps the field open, exactly
/// as a cell parse error keeps the market-data editor open; it is the
/// tile's `notice` that a REFUSED model write goes to instead.
pub(crate) struct ExprField {
    pub input: Entity<InputState>,
    /// The slot being replaced (`e`), or `None` for a fresh one (`x`) —
    /// what `core::resolve` excludes from the references the text may
    /// name and checks for a cycle through.
    pub editing: Option<u8>,
    pub error: Option<SharedString>,
}

/// Split each option into the two columns a picker row paints. The
/// options this surface builds are `{identity}@{source}`, so the source
/// is the LAST `@`-separated piece; a bare option (the sources stage)
/// paints its whole self in the first column.
fn labels_for(options: &[String]) -> Vec<(SharedString, SharedString)> {
    options
        .iter()
        .map(|o| match o.rsplit_once('@') {
            Some((identity, source)) => (identity.into(), format!("@{source}").into()),
            None => (o.clone().into(), SharedString::default()),
        })
        .collect()
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
    /// `fetching`, `failed: <reason>` (the slot's own fetch, or the
    /// delivered load lane's), `degraded`, or empty.
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
        // The reason rides along on a FAILURE, spelled the way the
        // slot's own failure above spells it (review round 1): a load
        // lane that failed under an answer this tile is still painting
        // is the one health state a trader has to act on, and "failed"
        // alone says nothing about what to do. `Degraded` stays the bare
        // word — the answer on screen is usable, and its reason belongs
        // to the diagnostics tile rather than a row in a popup.
        Some(Health::Degraded { .. }) => "degraded".into(),
        Some(Health::Failed { reason }) => format!("failed: {reason}").into(),
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

/// Paint the add picker (spec §9.6) on the series list's own surface:
/// the field on top, one row per PAINTED option below — `identity` in
/// the first column, `@source` muted in the second, a `•` where this
/// tile already holds the pair — and, where the typed text matches
/// nothing, the single `add "<text>"…` row that opens the source stage.
///
/// Every row is a click door into the same `picker_pick` `enter` takes,
/// by the same WINDOW-relative index [`ChoiceList::highlighted`] is in.
pub(crate) fn render_picker(
    p: &PickerState,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    cx: &App,
) -> Deferred {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(move || format!("ts-picker-{tile_id}"))
        // The series list's reasons, exactly (see `render_series_popup`).
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_popup_with_window(window, cx))
        })
        .child(
            div()
                .w_full()
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(theme.border)
                // The placeholder is set once on the `InputState`, at
                // open: an `Input` element has no builder for it.
                .child(Input::new(&p.input).appearance(false).w_full()),
        );
    for (row, ranked) in p.list.painted().iter().enumerate() {
        let (identity, source) = p.labels[ranked.row].clone();
        let loaded = p.loaded[ranked.row];
        let highlighted = row == p.list.highlighted();
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .gap_2()
                .rounded(theme.radius)
                .items_center()
                .when(highlighted, |d| {
                    d.bg(theme.accent).text_color(theme.accent_foreground)
                })
                .when(!highlighted, |d| d.text_color(theme.popover_foreground))
                .debug_selector(move || format!("ts-picker-row-{tile_id}-{row}"))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.picker_pick(row, window, cx));
                    }
                })
                .child(div().flex_1().child(identity))
                .child(
                    div()
                        .text_xs()
                        .when(!highlighted, |d| d.text_color(theme.muted_foreground))
                        .child(source),
                )
                // Already on this tile — still pickable, since a second
                // slot over one pair with another rule is legitimate.
                .child(
                    div()
                        .w(scale::design(SWATCH))
                        .child(if loaded { "•" } else { "" }),
                ),
        );
    }
    if let Some(add) = &p.add_row {
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .rounded(theme.radius)
                .items_center()
                // The only thing `enter` can take while it is up, so it
                // paints lit.
                .bg(theme.accent)
                .text_color(theme.accent_foreground)
                .debug_selector(move || format!("ts-picker-add-{tile_id}"))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.commit_picker(window, cx));
                    }
                })
                .child(add.clone()),
        );
    } else if p.list.painted_len() == 0 {
        // "Asked and answered", never a blank rectangle — the series
        // list's own empty-state rule.
        list = list.child(
            div()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .flex()
                .items_center()
                .text_color(theme.muted_foreground)
                .child("no identities known"),
        );
    }
    anchor_popup(list)
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
        // A FAILED load lane names its reason, exactly as the slot's own
        // failure does — and is outranked by that failure just the same.
        m.set_state(1, SlotState::Idle);
        let failed = result(
            1,
            Some(Health::Failed {
                reason: "no generation".into(),
            }),
        );
        let p = SeriesPopup::prepare(&m, Some(&failed), Some("demo_kdb"), &stub);
        assert_eq!(p.rows[0].state.as_ref(), "failed: no generation");
        m.set_state(1, SlotState::Failed("no route".into()));
        let p = SeriesPopup::prepare(&m, Some(&failed), Some("demo_kdb"), &stub);
        assert_eq!(
            p.rows[0].state.as_ref(),
            "failed: no route",
            "this tile's own fetch failure is the news, not the lane's"
        );
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
