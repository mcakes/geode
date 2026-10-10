//! State and painting for six mutually exclusive transient surfaces: series
//! list, add picker, expression field, custom dates editor, the menus (action
//! list, range menu and frequency menu, painted by `geode_tile::menu`), and
//! color picker.
//!
//! Add/expression inputs, the dates editor's container, and the component
//! color picker use insert-mode routing. Series and the menus retain normal
//! mode with popup-specific context. The shared closer blurs only a popup that
//! owns focus. Color-picker ownership includes focused descendants such as its
//! hex field.
//!
//! Series labels, state text, and swatches are prepared alongside header chips.
//! Picker labels, completion labels, menu rows, and date segments are retained
//! for reuse by their painters. Series, picker and completion rows share
//! `geode_tile::popover::row_shell`; the menus are `geode_tile::menu`'s.
//!
//! The lists, add picker, range editor, and expression completion list use
//! deferred anchored popovers above the chart. The expression field itself
//! paints inline below the header. The component color picker replaces its
//! target chip's swatch and owns its own popup surface.

use std::rc::Rc;

use geode_core::health::Health;
use geode_core::series::{SeriesResult, SlotKind};
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::fonts;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::listrow::row_paint;
use geode_shell::shell::scale;
use geode_tile::menu::{Menu, MenuIds};
use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET, anchor_popup, empty_row, row_shell};
use geode_widgets::datefield::{DateTimeField, SegmentPaint, SegmentText};
use gpui::prelude::*;
use gpui::{
    Anchor, App, Deferred, ElementId, Entity, FocusHandle, Focusable as _, Hsla, SharedString, div,
};
use gpui_component::color_picker::ColorPickerState;
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Theme, h_flex};

use crate::core::complete::{Completion, Help};
use crate::core::menu::{MenuKind, Pick};
use crate::core::model::{Color, Model, SlotState};
use crate::tile::TimeseriesTile;

/// The swatch beside a row's label, matching the header chip's.
const SWATCH: f32 = 8.0;
/// The `from`/`to` label column in the dates editor, at the design rem.
const LABEL_WIDTH: f32 = 32.0;

/// Dates-editor key context. `crate::init` unbinds GPUI's focus-cycling
/// Tab actions here so the container can switch its two date fields.
pub const RANGE_CONTEXT: &str = "GeodeTimeseriesRange";

/// Expression-field key context. `crate::init` unbinds GPUI's
/// focus-cycling Tab actions here so the field's listener completes a
/// series name.
pub const EXPR_CONTEXT: &str = "GeodeTimeseriesExpr";

/// The completion list's line when no series is loaded: an expression
/// may reference only a loaded one. Backtick-quoted runs are keys.
const EXPR_NOTHING_LOADED: &str = "no series loaded — `a` adds one";

/// The dates editor's hint: `tab` and `escape` do things a trader cannot see
/// from the two fields alone. Backtick-quoted runs are keys, painted as chips
/// by `kbd::marked`.
const RANGE_HINT: &str = "`tab` switches · `enter` applies · `escape` goes back";

/// Mutually exclusive transient state. Series and Menu add their popup pair
/// to normal-mode context (a menu also its `menu` kind pair). Picker, Expr,
/// Range (the custom dates editor), and Color use insert routing; Range and
/// the component color picker also own their focused key handlers.
pub(crate) enum Popup {
    Series(SeriesPopup),
    Picker(PickerState),
    Expr(ExprField),
    Range(RangePopup),
    /// Fieldless menu — the action list, the range menu or the frequency menu
    /// ([`MenuKind`]) — routed through `popup == menu` and a `menu` kind pair.
    Menu(MenuState),
    /// Component color picker anchored at one slot's chip. The header renders
    /// its trigger in place of that chip's swatch; the component owns the popover.
    Color(ColorPick),
}

/// Which popup is up, without its state: what a painted popup's outside-press
/// listener hands the tile, so the press closes that popup only if it is still
/// the one up. A menu carries its kind; two menus are different popups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PopupKind {
    Series,
    Picker,
    Expr,
    Range,
    Menu(MenuKind),
    Color,
}

/// Header paint state for a color picker: target slot number, featured swatches
/// resolved at open, and the reusable component state.
pub(crate) struct ColorPick {
    pub target: u8,
    pub swatches: Vec<Hsla>,
    pub picker: Entity<ColorPickerState>,
}

/// Commit context captured at open and retained after popup closure. Hex Enter
/// can close before its Change event arrives; dropping this context on close
/// would lose that commit.
///
/// Target is a slot number, independent of cursor movement. Featured entries are
/// the palette followed by configured names, resolved with the opening theme.
/// Their snapshot maps near-equal picked RGB bytes back to theme-following choices;
/// the current painted target color is checked separately for no-op commits.
#[derive(Clone)]
pub(crate) struct PickContext {
    pub target: u8,
    pub featured: Vec<(Hsla, Color)>,
}

/// A menu's kind, its element names, and the door's menu (rows, including
/// hints and the frequency rows' cap refusals, and the one highlight
/// keyboard and pointer share). Rows refresh at open, on chrome rebuilds,
/// on frame changes and on a keymap publish.
pub(crate) struct MenuState {
    pub kind: MenuKind,
    pub ids: MenuIds,
    pub menu: Menu<Pick>,
}

impl Popup {
    /// Whether this popup requires insert-mode routing. Actual keyboard ownership
    /// is checked separately by [`Self::holds_focus`].
    pub(crate) fn is_insert(&self) -> bool {
        match self {
            Popup::Series(_) | Popup::Menu(_) => false,
            // The dates editor owns a focus handle and Color owns a component
            // focus subtree; both require insert routing just as focused text
            // inputs do.
            Popup::Picker(_) | Popup::Expr(_) | Popup::Range(_) | Popup::Color(_) => true,
        }
    }

    /// Check actual window focus, independently of insert mode. A popup may
    /// remain open after focus has moved to another surface.
    pub(crate) fn holds_focus(&self, window: &gpui::Window, cx: &gpui::App) -> bool {
        match self {
            // Fieldless popups use the shell matcher rather than an input handle.
            Popup::Series(_) | Popup::Menu(_) => false,
            Popup::Picker(p) => p.input.read(cx).focus_handle(cx).is_focused(window),
            Popup::Expr(f) => f.input.read(cx).focus_handle(cx).is_focused(window),
            // The container owns focus; date fields are pure state.
            Popup::Range(r) => r.focus.is_focused(window),
            // Include descendants: the popover and hex input focus beneath the state handle.
            Popup::Color(c) => c
                .picker
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx),
        }
    }

    /// The value this popup gives the key context's `popup` pair, or
    /// `None` for one whose keys are the shared `mode == insert`
    /// layer's.
    pub(crate) fn context_pair(&self) -> Option<&'static str> {
        match self {
            Popup::Series(_) => Some("series"),
            Popup::Menu(_) => Some("menu"),
            Popup::Picker(_) | Popup::Expr(_) | Popup::Range(_) | Popup::Color(_) => None,
        }
    }

    /// Which popup this is, for the outside-press guard
    /// (`TimeseriesTile::outside_press`).
    pub(crate) fn kind(&self) -> PopupKind {
        match self {
            Popup::Series(_) => PopupKind::Series,
            Popup::Picker(_) => PopupKind::Picker,
            Popup::Expr(_) => PopupKind::Expr,
            Popup::Range(_) => PopupKind::Range,
            Popup::Menu(m) => PopupKind::Menu(m.kind),
            Popup::Color(_) => PopupKind::Color,
        }
    }

    /// The key context's `menu` pair while a menu is open — what the
    /// range menu's own `c` binds against.
    pub(crate) fn menu_pair(&self) -> Option<&'static str> {
        match self {
            Popup::Menu(m) => Some(m.kind.word()),
            _ => None,
        }
    }
}

/// The date field receiving range edits. Tab and Shift-Tab switch fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    From,
    To,
}

impl Which {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Which::From => "from",
            Which::To => "to",
        }
    }

    fn other(self) -> Which {
        match self {
            Which::From => Which::To,
            Which::To => Which::From,
        }
    }
}

/// Prepared date segments and a tile/field-specific selector. Rebuilt when
/// field state changes; rendering shares the Rc slice without rebuilding segments.
pub(crate) struct DateFieldPaint {
    pub segments: Rc<[SegmentText]>,
    pub selector: SharedString,
}

impl DateFieldPaint {
    pub(crate) fn of(field: &DateTimeField, tile_id: u64, which: Which) -> Self {
        DateFieldPaint {
            segments: field.segments().into(),
            selector: format!("ts-range-{}-{tile_id}", which.word()).into(),
        }
    }
}

/// The custom dates editor: two segmented dates and inline commit errors. It
/// opens on `from`'s day segment and digits type into the date at once; the
/// presets are the range menu's, one `escape` away. The container owns one focus
/// handle and routes keys to the active pure field; closing must blur that
/// handle before dropping it if it still owns focus.
pub(crate) struct RangePopup {
    pub from: DateTimeField,
    pub to: DateTimeField,
    pub active: Which,
    pub focus: FocusHandle,
    pub from_paint: DateFieldPaint,
    pub to_paint: DateFieldPaint,
    /// Commit refusal displayed below the fields while the popup remains open.
    pub error: Option<SharedString>,
}

impl RangePopup {
    #[cfg(test)]
    pub(crate) fn active_field(&self) -> &DateTimeField {
        match self.active {
            Which::From => &self.from,
            Which::To => &self.to,
        }
    }

    fn active_field_mut(&mut self) -> &mut DateTimeField {
        match self.active {
            Which::From => &mut self.from,
            Which::To => &mut self.to,
        }
    }

    /// Switch active fields. With two fields, Tab and Shift-Tab perform the
    /// same switch.
    pub(crate) fn switch(&mut self) {
        self.active = self.active.other();
    }

    /// Apply a key and refresh the active field's prepared segments.
    pub(crate) fn apply(&mut self, key: geode_widgets::datefield::FieldKey, tile_id: u64) {
        let which = self.active;
        self.active_field_mut().apply(key);
        self.reprepare(which, tile_id);
    }

    pub(crate) fn select(
        &mut self,
        which: Which,
        segment: geode_widgets::datefield::Segment,
        tile_id: u64,
    ) {
        self.active = which;
        self.active_field_mut().select(segment);
        self.reprepare(which, tile_id);
    }

    fn reprepare(&mut self, which: Which, tile_id: u64) {
        match which {
            Which::From => self.from_paint = DateFieldPaint::of(&self.from, tile_id, which),
            Which::To => self.to_paint = DateFieldPaint::of(&self.to, tile_id, which),
        }
    }
}

/// Catalogue identities or source choices for a typed identity. Sources
/// retains the identity while the user chooses where to fetch it.
pub(crate) enum PickerStage {
    Identities,
    Sources { identity: String },
}

/// Tile-owned input and a ranked choice list with a twelve-row moving window.
/// `loaded` and `labels` are indexed by declared option position, alongside
/// `list.options()`, and must be replaced together. Prepared columns avoid
/// splitting identity/source strings during render.
pub(crate) struct PickerState {
    pub input: Entity<InputState>,
    pub list: ChoiceList,
    pub stage: PickerStage,
    /// Whether the model holds this option's source/identity pair. Marked rows
    /// remain pickable, allowing another slot with a different bucket rule.
    pub loaded: Vec<bool>,
    /// The prepared `(identity, @source)` pair per option; the second
    /// half is empty in the `Sources` stage, whose options are bare
    /// source names.
    pub labels: Vec<(SharedString, SharedString)>,
    /// Offer to add unmatched, nonempty identity text. Committing may use an
    /// explicit @source or open the source stage; see `commit_picker`.
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

    /// Offer an add row only in the identities stage, with no ranked matches
    /// and nonempty trimmed query text. Its label uses the trimmed text that commit
    /// resolves; whitespace alone never offers an identity, even in an empty catalogue.
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

/// Tile-owned expression input painted below the header. Parse and reference
/// errors keep it open with an inline error. After resolution the model write
/// runs and the field closes; a model refusal appears in the tile's notice.
pub(crate) struct ExprField {
    pub input: Entity<InputState>,
    /// Stable slot number to replace (`e`), or `None` to add a slot (`x`).
    /// Resolution permits source-series references only.
    pub editing: Option<u8>,
    pub error: Option<SharedString>,
    /// The loaded-name list under the field, rebuilt on the input's
    /// Change event and on open, never in render.
    pub completion: Completion,
    /// The text the tile's own last completion write left in the field.
    /// That write's Change event carries it and is skipped: the write has
    /// already placed the list, and a refusal shown after an Enter
    /// expansion must survive its echo.
    pub echo: Option<String>,
    /// The help line under the field: the lit row or the enclosing call
    /// at the caret ([`crate::core::complete::help_for`]). Rebuilt with
    /// the list, on every Change (an echo too: the caret moved), after a
    /// completion write and whenever the input notifies (an arrow key or
    /// a click moved the caret), never in render.
    pub help: Option<Help>,
    /// The text and caret `help` was computed for, so an input notify
    /// that moved neither rebuilds nothing.
    pub help_at: Option<(String, usize)>,
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

/// Prepared rows in model slot order. The list and header chips share the
/// model cursor, and clicking either selects through `chip_clicked`.
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
    /// Theme-resolved swatch prepared with the header's color resolver.
    pub swatch: Hsla,
    pub hidden: bool,
}

impl SeriesPopup {
    /// Prepare rows from the model and retained result during `rebuild_chrome`.
    /// The tile shares one color resolver with the header so swatches agree without
    /// rebuilding palette and named-color data for each row or paint.
    pub(crate) fn prepare(
        model: &Model,
        result: Option<&SeriesResult>,
        default_source: Option<&str>,
        color_of: &dyn Fn(&Color) -> Hsla,
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
                    swatch: color_of(&s.color),
                    hidden: !s.visible,
                }
            })
            .collect();
        SeriesPopup { rows }
    }
}

/// A slot's own fetch state takes precedence over delivered source health.
/// Fetching/failure describes the current request; provenance describes the
/// retained answer and is shown only while the slot is idle.
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
        // Retain a failed load's reason in the row. Degraded health uses a compact
        // state word; its detailed reason remains available in diagnostics.
        Some(Health::Degraded { .. }) => "degraded".into(),
        Some(Health::Failed { reason }) => format!("failed: {reason}").into(),
        _ => SharedString::default(),
    }
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
    let hover = row_paint(theme).hover;
    let mut list = popover::surface(cx)
        .debug_selector(move || format!("ts-list-{tile_id}"))
        // Keep the chart below from receiving pointer hits through the popup.
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| {
                tile.update(cx, |t, cx| t.outside_press(PopupKind::Series, window, cx))
            }
        });
    if p.rows.is_empty() {
        // Keep the empty list informative and show its available actions.
        return anchor_popup(
            list.child(empty_row(theme, crate::header::EMPTY_HINT)),
            Anchor::TopRight,
        );
    }
    for (i, row) in p.rows.iter().enumerate() {
        let highlighted = cursor == Some(i);
        list = list.child(
            row_shell(
                theme,
                hover,
                ElementId::NamedInteger(SharedString::new_static("ts-list-row"), i as u64),
                highlighted,
                move || format!("ts-list-row-{tile_id}-{i}"),
                {
                    // Select the same model cursor used by the header chips.
                    let tile = tile.clone();
                    move |_window, cx| tile.update(cx, |t, cx| t.chip_clicked(i, cx))
                },
            )
            // A hidden series stays in the list, struck through, for
            // the same reason its chip does: `v` is a toggle, and a
            // row that vanished would leave nothing to press again.
            .when(row.hidden, |d| d.opacity(0.5).line_through())
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
    anchor_popup(list, Anchor::TopRight)
}

/// Paint Input above the picker's moving option window. Prepared columns show
/// identity, source, and an already-loaded marker. The unmatched add offer stays
/// highlighted because it is the only available commit.
///
/// Option clicks pass a window-relative index, matching [`ChoiceList::highlighted`],
/// to `picker_pick`; the add offer invokes `commit_picker` directly.
pub(crate) fn render_picker(
    p: &PickerState,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    cx: &App,
) -> Deferred {
    let theme = cx.theme();
    let hover = row_paint(theme).hover;
    let mut list = popover::surface(cx)
        .debug_selector(move || format!("ts-picker-{tile_id}"))
        // Keep the chart below from receiving pointer hits through the popup.
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| {
                tile.update(cx, |t, cx| t.outside_press(PopupKind::Picker, window, cx))
            }
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
            row_shell(
                theme,
                hover,
                ElementId::NamedInteger(SharedString::new_static("ts-picker-row"), row as u64),
                highlighted,
                move || format!("ts-picker-row-{tile_id}-{row}"),
                {
                    let tile = tile.clone();
                    move |window, cx| tile.update(cx, |t, cx| t.picker_pick(row, window, cx))
                },
            )
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
            // The only thing `enter` can take while it is up, so it
            // paints lit.
            row_shell(
                theme,
                hover,
                ElementId::Name(SharedString::new_static("ts-picker-add")),
                true,
                move || format!("ts-picker-add-{tile_id}"),
                {
                    let tile = tile.clone();
                    move |window, cx| {
                        tile.update(cx, |t, cx| t.commit_picker(window, cx));
                    }
                },
            )
            .child(add.clone()),
        );
    } else if p.list.painted_len() == 0 {
        // An empty source-stage match list uses this same fallback text.
        list = list.child(empty_row(theme, "no identities known"));
    }
    anchor_popup(list, Anchor::TopRight)
}

/// Paint the expression field's completion list under the field: the
/// painted window of ranked loaded names with the lit row, or the one
/// muted line when nothing is loaded. `None` when there is nothing to
/// offer at the caret (a number, or no name matches).
///
/// A row press writes that name at the caret through `expr_pick`, the
/// same write `tab` makes. The press never leaves the popup: rows stop
/// propagation, and the surface occludes, so no hitbox below it — the
/// shell root's `track_focus` among them, which focuses only a hovered
/// hitbox — is hovered, and nothing takes the keyboard from the field.
pub(crate) fn render_expr_list(
    f: &ExprField,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    cx: &App,
) -> Option<Deferred> {
    let c = &f.completion;
    if !c.nothing_loaded() && c.candidate_count() == 0 {
        return None;
    }
    let theme = cx.theme();
    let hover = row_paint(theme).hover;
    let mut list = popover::surface(cx)
        .debug_selector(move || format!("ts-expr-list-{tile_id}"))
        // Keep the chart below from receiving pointer hits through the
        // popup, and the shell root from taking focus on a press here.
        .occlude();
    if c.nothing_loaded() {
        list = list.child(
            empty_row(theme, EXPR_NOTHING_LOADED)
                .debug_selector(move || format!("ts-expr-empty-{tile_id}")),
        );
        return Some(anchor_popup(list, Anchor::TopLeft));
    }
    for (i, label) in c.painted() {
        let highlighted = i == c.highlighted();
        let selector_label = label.clone();
        list = list.child(
            row_shell(
                theme,
                hover,
                ElementId::Name(label.clone()),
                highlighted,
                move || format!("ts-expr-row-{tile_id}-{selector_label}"),
                {
                    let tile = tile.clone();
                    move |window, cx| tile.update(cx, |t, cx| t.expr_pick(i, window, cx))
                },
            )
            .child(label.clone()),
        );
    }
    Some(anchor_popup(list, Anchor::TopLeft))
}

/// Read date-field colors from the current theme; segment strings are already
/// prepared. The active field uses primary selection colors, while the other
/// field uses secondary selection colors and muted surrounding text.
fn segment_paint(theme: &Theme, live: bool) -> SegmentPaint {
    let muted = theme.muted_foreground;
    SegmentPaint {
        rest_text: if live {
            theme.popover_foreground
        } else {
            muted
        },
        rest_fill: None,
        // Pair secondary fill with secondary_foreground. Surface-muted text is not
        // contrast-adjusted for that selection background.
        active_text: if live {
            theme.primary_foreground
        } else {
            theme.secondary_foreground
        },
        active_fill: if live { theme.primary } else { theme.secondary },
        typing_text: if live { theme.accent_foreground } else { muted },
        typing_fill: if live { theme.accent } else { theme.secondary },
        separator: muted,
        suffix: muted,
        radius: theme.radius_tokens().sm,
        flush: false,
    }
}

/// One `from`/`to` row: the label, then the segmented field.
fn range_row(
    p: &RangePopup,
    which: Which,
    theme: &Theme,
    tile: &Entity<TimeseriesTile>,
) -> impl IntoElement {
    let live = p.active == which;
    let paint = match which {
        Which::From => &p.from_paint,
        Which::To => &p.to_paint,
    };
    let tile = tile.clone();
    h_flex()
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .gap_2()
        .items_center()
        .child(
            div()
                .w(scale::design(LABEL_WIDTH))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(which.word()),
        )
        .child(
            h_flex()
                .items_center()
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .border_1()
                .border_color(if live {
                    theme.table_active_border
                } else {
                    theme.border
                })
                .font_family(fonts::MONO)
                .child(geode_widgets::datefield::paint(
                    &paint.segments,
                    None,
                    segment_paint(theme, live),
                    paint.selector.clone(),
                    move |segment, window, cx| {
                        tile.update(cx, |t, cx| {
                            t.range_segment_clicked(which, segment, window, cx)
                        });
                    },
                )),
        )
}

/// Paint the custom dates editor under the range trigger: both dates, the hint,
/// and any refusal. One focused container routes keys for both pure date fields.
pub(crate) fn render_range(
    p: &RangePopup,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    cx: &App,
) -> Deferred {
    let theme = cx.theme();
    let mut panel = popover::surface(cx)
        .track_focus(&p.focus)
        // `tab` is this editor's own key, and gpui-component's `Root`
        // binds it window-wide to focus cycling; `crate::init` unbinds
        // it in THIS context so the listener below is reached.
        .key_context(RANGE_CONTEXT)
        .debug_selector(move || format!("ts-range-{tile_id}"))
        // Keep the chart below from receiving pointer hits through the popup.
        .occlude()
        // Only while THIS editor is still up: a press on a trigger
        // runs first (capture phase) and may already have replaced it.
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| {
                tile.update(cx, |t, cx| t.outside_press(PopupKind::Range, window, cx))
            }
        })
        .on_key_down({
            let tile = tile.clone();
            move |event: &gpui::KeyDownEvent, window, cx| {
                // Consume keys handled by the focused range container before the shell
                // routes them. Unhandled chords continue to the shell.
                if tile.update(cx, |t, cx| t.range_key(event, window, cx)) {
                    cx.stop_propagation();
                }
            }
        })
        .child(range_row(p, Which::From, theme, tile))
        .child(range_row(p, Which::To, theme, tile))
        .child(
            div()
                .px(scale::design(ROW_INSET))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(geode_shell::shell::kbd::marked(RANGE_HINT)),
        );
    if let Some(error) = &p.error {
        panel = panel.child(
            div()
                .px(scale::design(ROW_INSET))
                .text_xs()
                .text_color(chip_paint(theme, Tone::DangerText).text)
                .child(error.clone()),
        );
    }
    anchor_popup(panel, Anchor::TopLeft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::series::expr::Expr;
    use geode_core::series::{BucketRule, SlotProvenance, SlotResult};

    /// Map palette indices to distinct hues for window-free swatch assertions.
    fn stub(color: &Color) -> Hsla {
        match color {
            Color::Palette(i) => gpui::hsla(*i as f32 / 10.0, 1.0, 0.5, 1.0),
            Color::Named(_) => gpui::black(),
            Color::Custom(c) => c.to_hsla(),
        }
    }

    fn model() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_rest", "series").unwrap();
        m.add_expr("SPX.close / VIX@demo_rest", Expr::Ref(1))
            .unwrap();
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
        assert_eq!(p.rows[2].label.as_ref(), "SPX.close / VIX@demo_re…");
        assert_eq!(p.rows[0].axis, "L");
        assert_eq!(p.rows[0].swatch, stub(&Color::Palette(0)));
        assert_eq!(p.rows[1].swatch, stub(&Color::Palette(1)));
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
        assert_eq!(p.rows[1].state.as_ref(), "fetching", "VIX is still waiting");
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
