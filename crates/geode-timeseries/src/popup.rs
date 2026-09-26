//! The tile's one overlay (spec §9.5–§9.8): the series list
//! ([`Popup::Series`], §9.5), the add picker ([`Popup::Picker`], §9.6),
//! the expression field ([`Popup::Expr`], §9.7) and the range dialog
//! ([`Popup::Range`], §9.8), the action menu ([`Popup::Menu`]) and the
//! colour picker ([`Popup::Colour`] — gpui-component's own, which this
//! module does not paint).
//!
//! **Four of the six hold the keyboard.** The add picker's and the
//! expression field's `InputState`s are tile-owned and focused, the
//! range dialog owns a bare [`gpui::FocusHandle`] with its two date
//! fields' keys on it, and the colour picker's popover and hex field
//! sit under the component state's handle; all four put the tile's key
//! context into `insert` mode — and all four are why
//! `TimeseriesTile::close_popup_with_window` is the ONE closer: a
//! focused handle dropped without a blur leaves `Window::focused`
//! pointing at nothing for the rest of the session (CLAUDE.md). The
//! series list and the action menu hold no field and keep the tile's
//! own keyboard.
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

use std::rc::Rc;

use geode_core::health::Health;
use geode_core::series::{Frequency, SeriesResult, SlotKind};
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::fonts;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::listrow::row_paint;
use geode_shell::shell::scale;
use geode_widgets::datefield::{DateTimeField, SegmentPaint, SegmentText};
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Deferred, Div, ElementId, Entity, FocusHandle,
    Focusable as _, Hsla, MouseButton, SharedString, Stateful, Window, anchored, deferred, div, px,
};
use gpui_component::color_picker::ColorPickerState;
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Theme, ThemeStyled as _, h_flex, v_flex};

use crate::core::Preset;
use crate::core::menu::MenuRow;
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
/// The `from`/`to` label column in the range popup, at the design rem.
const LABEL_WIDTH: f32 = 32.0;

/// The gpui key context the range popup's container carries — the
/// scope `crate::init`'s `tab`/`shift+tab` reclaim is bound in, and the
/// only place in this crate that needs one (every other key the tile
/// resolves goes through the shell's matcher, not gpui's).
pub const RANGE_CONTEXT: &str = "GeodeTimeseriesRange";

/// The range popup's one hint line: the digit shortcut is otherwise
/// invisible, and the `edited` rule behind it ("until you start editing
/// a date") is what makes it worth naming.
const RANGE_HINT: &str = "1–7 preset · tab switches · enter commits";

/// What the tile currently has open. `Series` is the series list (spec
/// §9.5) — it holds no field, so it is NOT an insert-mode popup: the key
/// context stays `normal` and gains a `popup == series` pair, which is
/// what its three keys bind against. `Picker` (§9.6) and `Expr` (§9.7)
/// each own a focused field and `Range` (§9.8) its own focus handle, so
/// all three report `insert` and none takes a `popup` pair: their keys
/// are the shared `mode == insert` layer's (`enter`/`escape`/`up`/
/// `down`) plus, for `Range`, its own container listener — the
/// market-data panel's own split.
pub(crate) enum Popup {
    Series(SeriesPopup),
    Picker(PickerState),
    Expr(ExprField),
    Range(RangePopup),
    /// The action list (mouse pass, 2026-09-24): every verb as a row,
    /// fieldless like the series list, keyed by `popup == menu`.
    Menu(MenuState),
    /// gpui-component's colour picker, open over one slot's chip. Not
    /// an overlay this module paints: the header draws the
    /// `ColorPicker` element in place of the target chip's swatch, and
    /// the component's popover owns its own surface and keys.
    Colour(ColourPick),
}

/// The colour picker, open for slot `target` — what the header needs
/// to draw it: the chip it stands in, the featured row
/// ([`PickContext::featured`]'s colours, prepared once for
/// `ColorPicker::featured_colors`) and the component state.
pub(crate) struct ColourPick {
    pub target: u8,
    pub swatches: Vec<Hsla>,
    pub picker: Entity<ColorPickerState>,
}

/// What a colour the picker commits is written against, captured at
/// open and kept by the tile PAST the popup's close: the hex field's
/// `enter` closes the popover (the action propagates to it) in the same
/// keystroke whose `Change` arrives a beat later, so a context that
/// died with the popup would drop that commit.
///
/// `target` is the slot NUMBER, not the cursor: the cursor may move
/// while the picker is up (a `:` line, a chip click behind it), and a
/// pick still lands on the slot it was opened for — a number is never
/// reused while any slot lives. `featured` is the five palette colours
/// then every `[colours]` name, each resolved against the theme as it
/// stood, so an exact `Hsla` match maps a pick back to the
/// theme-following colour it came from (`core::colour_from_pick`).
#[derive(Clone)]
pub(crate) struct PickContext {
    pub target: u8,
    pub featured: Vec<(Hsla, Colour)>,
}

/// The action menu's state: its prepared rows (`core::menu`, hints
/// resolved at open) and the highlighted index the keyboard and the
/// pointer share.
pub(crate) struct MenuState {
    pub rows: Vec<MenuRow>,
    pub highlighted: usize,
}

impl Popup {
    /// Whether this popup holds the keyboard as a text field — what puts
    /// the tile's key context into `insert` mode.
    pub(crate) fn is_insert(&self) -> bool {
        match self {
            Popup::Series(_) | Popup::Menu(_) => false,
            // The range popup holds no `InputState`, but it DOES hold
            // the keyboard — its own focus handle, with the two date
            // fields' keys on it — so it is an insert popup in every
            // sense the shell and the `popup_survives` gate care about.
            // The picker's popover takes the keyboard (its hex field,
            // its swatches): typing there must never reach the tile's
            // bare-key verbs, which is what `insert` routes away.
            Popup::Picker(_) | Popup::Expr(_) | Popup::Range(_) | Popup::Colour(_) => true,
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
            Popup::Series(_) | Popup::Menu(_) => false,
            Popup::Picker(p) => p.input.read(cx).focus_handle(cx).is_focused(window),
            Popup::Expr(f) => f.input.read(cx).focus_handle(cx).is_focused(window),
            // Its own handle, not an `InputState`'s: the segmented
            // fields are pure state and the CONTAINER is what is
            // focused (the market-data date field's shape).
            Popup::Range(r) => r.focus.is_focused(window),
            // The component's root tracks the state's handle, and its
            // popover surface and hex field sit beneath it in the
            // dispatch tree — so "contains", not "is".
            Popup::Colour(c) => c
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
            Popup::Picker(_) | Popup::Expr(_) | Popup::Range(_) | Popup::Colour(_) => None,
        }
    }
}

/// Which of the range popup's two date fields the keyboard is on
/// (spec §9.8). `tab`/`shift+tab` move between them; everything else a
/// keystroke does, it does to this one.
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

/// One date field's painted segments plus its selector — prepared
/// whenever the field changes, never in `render` (the market-data
/// panel's `DateFieldPaint`, spelled the same way so the two cannot
/// drift). `segments` is an `Rc<[SegmentText]>`: the painter takes it
/// straight through, so a frame costs a refcount bump rather than a
/// `Vec` of six `SegmentText`s.
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

/// The range popup (spec §9.8): two segmented date fields, a row of
/// preset chips, and the inline refusal a bad commit leaves behind.
///
/// It holds no `InputState` — the fields are `geode-widgets`' pure
/// state — so the KEYBOARD is its own [`FocusHandle`], tracked on the
/// popup's container with an `on_key_down` listener over it. That is the
/// market-data date field's shape, and the reason
/// `TimeseriesTile::close_popup_with_window` is still the one closer: a
/// focused handle dropped without a blur leaves `Window::focused`
/// pointing at nothing for the rest of the session (CLAUDE.md).
pub(crate) struct RangePopup {
    pub from: DateTimeField,
    pub to: DateTimeField,
    pub active: Which,
    pub focus: FocusHandle,
    pub from_paint: DateFieldPaint,
    pub to_paint: DateFieldPaint,
    /// A backwards range, an unfinished segment or the point cap —
    /// painted UNDER the fields, like the expression field's parse
    /// error, because the popup stays open and the reason belongs
    /// beside what caused it.
    pub error: Option<SharedString>,
    /// Whether a keystroke has reached either field since the popup
    /// opened — what decides whether a bare `1`..`7` is a PRESET or a
    /// digit (see [`RangePopup::digit_is_preset`]).
    pub edited: bool,
    /// The frequency in force, mirrored from the model at open and on
    /// every chip click so the frequency row paints the tick without
    /// reading the model in `render`.
    pub frequency: Frequency,
}

impl RangePopup {
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

    /// `tab`/`shift+tab`: with two fields both directions are the same
    /// move, and neither counts as an edit — a trader who tabbed over to
    /// read the other date has typed nothing.
    pub(crate) fn switch(&mut self) {
        self.active = self.active.other();
    }

    /// Apply one key to the active field, re-preparing that field's
    /// segments. Answers whether anything moved.
    ///
    /// Only a key that MOVED something counts as an edit: `right` on
    /// the last segment under `Precision::Date`, or
    /// `backspace` with nothing typed, change nothing on screen, and a
    /// trader who pressed one and then reached for a preset digit would
    /// have found the digit typing itself into the day instead — the
    /// popup looking exactly as it did when it opened.
    pub(crate) fn apply(&mut self, key: geode_widgets::datefield::FieldKey, tile_id: u64) -> bool {
        let which = self.active;
        let moved = self.active_field_mut().apply(key);
        self.edited |= moved;
        self.reprepare(which, tile_id);
        moved
    }

    pub(crate) fn select(
        &mut self,
        which: Which,
        segment: geode_widgets::datefield::Segment,
        tile_id: u64,
    ) {
        self.active = which;
        self.edited = true;
        self.active_field_mut().select(segment);
        self.reprepare(which, tile_id);
    }

    fn reprepare(&mut self, which: Which, tile_id: u64) {
        match which {
            Which::From => self.from_paint = DateFieldPaint::of(&self.from, tile_id, which),
            Which::To => self.to_paint = DateFieldPaint::of(&self.to, tile_id, which),
        }
    }

    /// Whether a bare `1`..`7` means a PRESET rather than a digit typed
    /// into the active segment (spec §9.8).
    ///
    /// Two conditions, and both are load-bearing. `!typing()` is the
    /// obvious one: a second digit always belongs to the segment being
    /// typed. `!edited` is the one the two readings of §9.8 disagree on,
    /// and it is what makes both halves of the popup reachable — every
    /// preset digit but `8`, `9` and `0` is also a legal first digit of
    /// a year, so a popup that read `1` as a preset AFTER the trader had
    /// moved onto the year segment could never be used to type `1990`.
    /// The rule a trader learns is therefore "a digit is a preset until
    /// you start editing a date, and the date's from then on" — and
    /// `escape`, then `r` again, is how you get back to the presets
    /// without the mouse.
    pub(crate) fn digit_is_preset(&self) -> bool {
        !self.edited && !self.active_field().typing()
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
    /// The text is TRIMMED, and the trimmed
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
    let hover = row_paint(theme).hover;
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
        return anchor_popup(list.child(empty_row(theme, crate::header::EMPTY_HINT)));
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
                    // The mouse form of `j`/`k`, and the chip click's own
                    // door: one cursor between the strip and the list.
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
    anchor_popup(list)
}

/// The one row every popup list paints: fixed height and inset, the
/// cursor's fill when `highlighted`, the shell's list-row hover fill
/// under the pointer otherwise (`listrow::row_paint` — the design
/// guide's "subtle pointer feedback, never the only cue"; the
/// highlight stays the state, the hover only says the row is
/// clickable), and a left press that stops propagation — the chart
/// beneath must not also take it — before running `on_down`.
///
/// `id` is the row's stable identity, needed for the hover state:
/// `(kind, index)` per popup, and a popup never shares a frame with
/// another. `hover` is `row_paint(theme).hover`, derived ONCE per
/// popup paint by the caller: `row_paint` floors its accent with a
/// possible OKLab bisection, which is not a per-row cost.
fn row_shell(
    theme: &Theme,
    hover: Hsla,
    id: ElementId,
    highlighted: bool,
    selector: impl FnOnce() -> String,
    on_down: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .gap_2()
        .rounded(theme.radius)
        .items_center()
        .when(highlighted, |d| {
            d.bg(theme.accent).text_color(theme.accent_foreground)
        })
        .when(!highlighted, |d| {
            d.text_color(theme.popover_foreground)
                .hover(move |s| s.bg(hover))
        })
        .debug_selector(selector)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_down(window, cx);
        })
}

/// A popup list's "asked and answered" line: a muted row naming why it
/// is empty, never a blank rectangle.
fn empty_row(theme: &Theme, text: &'static str) -> Div {
    div()
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .flex()
        .items_center()
        .text_color(theme.muted_foreground)
        .child(text)
}

/// The anchored, deferred wrapper every one of this tile's popups
/// takes: `Local` position mode against the `relative()` wrapper the
/// tile paints round its header, snapped inside the window.
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
    let hover = row_paint(theme).hover;
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
        // "Asked and answered", never a blank rectangle — the series
        // list's own empty-state rule.
        list = list.child(empty_row(theme, "no identities known"));
    }
    anchor_popup(list)
}

/// One date field's colours, derived from the theme per paint (nine
/// `Hsla` reads — the "cheap enough for `render`" half of the
/// prepare/paint split; what is NOT cheap, the segments themselves, is
/// prepared in the key handler).
///
/// `live` is the field the keyboard is on: its active segment wears the
/// theme's own `primary`/`primary_foreground` pair (gpui-component's,
/// like the market-data field's), and the other field is painted muted
/// throughout so the strip shows at a glance which date a digit lands
/// in.
fn segment_paint(theme: &Theme, live: bool) -> SegmentPaint {
    let muted = theme.muted_foreground;
    SegmentPaint {
        rest_text: if live {
            theme.popover_foreground
        } else {
            muted
        },
        rest_fill: None,
        // The dimmed field's own active segment still wears a fill
        // (`secondary`), so its text is that fill's own pair —
        // `secondary_foreground`, which is exactly `Tone::Neutral`
        // (`shell::chip::chip_paint`) and is swept on every bundled theme
        // by `every_chip_tone_is_readable_on_every_bundled_theme`.
        // `muted_foreground` is the colour of text on the SURFACE and is
        // not floored against `secondary` anywhere.
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

/// Paint the range popup (spec §9.8) on the series list's own surface:
/// a `from` row, a `to` row, the seven presets as chips, the hint and
/// the inline refusal.
///
/// The CONTAINER carries the focus handle and the key listener — one
/// keyboard for both fields, the market-data date field's shape — which
/// is why the fields themselves are plain painted state.
pub(crate) fn render_range(
    p: &RangePopup,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    cx: &App,
) -> Deferred {
    let theme = cx.theme();
    let mut panel = popover_surface(cx)
        .track_focus(&p.focus)
        // `tab` is this popup's own key, and gpui-component's `Root`
        // binds it window-wide to focus cycling; `crate::init` unbinds
        // it in THIS context so the listener below is reached.
        .key_context(RANGE_CONTEXT)
        .debug_selector(move || format!("ts-range-{tile_id}"))
        // The series list's reasons, exactly (see `render_series_popup`).
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_popup_with_window(window, cx))
        })
        .on_key_down({
            let tile = tile.clone();
            move |event: &gpui::KeyDownEvent, window, cx| {
                // The listener sits on the FOCUSED element, so it runs
                // before the shell's own: a key this popup owns stops
                // here, and a chord (`route` answers `None`) falls
                // through to the shell untouched.
                if tile.update(cx, |t, cx| t.range_key(event, window, cx)) {
                    cx.stop_propagation();
                }
            }
        })
        .child(range_row(p, Which::From, theme, tile))
        .child(range_row(p, Which::To, theme, tile))
        .child(freq_row(p.frequency, theme, tile, tile_id));

    let mut presets = h_flex()
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .gap_1()
        .items_center();
    // One derivation for all seven: every preset chip is the same
    // `Tone::Neutral` on the same popover ground, and `for_chip` costs a
    // handful of contrast checks and up to an OKLab bisection per call.
    let chip = chip_paint(theme, Tone::Neutral);
    let states = control::for_chip(theme, &chip, theme.popover);
    for (i, preset) in Preset::ALL.into_iter().enumerate() {
        let word = preset.as_str();
        presets = presets.child(
            div()
                .id(ElementId::NamedInteger(
                    SharedString::new_static("ts-range-preset"),
                    i as u64,
                ))
                .debug_selector(move || format!("ts-range-preset-{tile_id}-{word}"))
                .px_1()
                .text_xs()
                .rounded(theme.radius)
                .text_color(chip.text)
                .when_some(chip.fill, |d, fill| d.bg(fill))
                .pointer_states(states)
                // The mouse's form of the digit: commits at once, like
                // every other preset door (§9.8).
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.range_preset_clicked(preset, window, cx));
                    }
                })
                .child(word),
        );
    }
    panel = panel.child(presets).child(
        div()
            .px(scale::design(ROW_INSET))
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(RANGE_HINT),
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
    anchor_popup(panel)
}

/// The range popup's frequency row (mouse pass, 2026-09-24): one chip
/// per `Frequency`, the one in force filled, the rest bare. A click
/// writes it at once through `range_freq_clicked` and the popup stays
/// open — it is a setting the row shows, not a commit of the two
/// dates. The keyboard's forms are `f`/`F` and `:freq`.
fn freq_row(
    current: Frequency,
    theme: &Theme,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
) -> impl IntoElement {
    let filled = chip_paint(theme, Tone::Neutral);
    let filled_states = control::for_chip(theme, &filled, theme.popover);
    let mut bare = chip_paint(theme, Tone::Neutral);
    bare.fill = None;
    bare.text = theme.muted_foreground;
    let bare_states = control::for_chip(theme, &bare, theme.popover);
    let mut row = h_flex()
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .gap_1()
        .items_center()
        .child(
            div()
                .w(scale::design(LABEL_WIDTH))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("freq"),
        );
    for (i, f) in Frequency::ALL.into_iter().enumerate() {
        let word = f.as_str();
        let on = f == current;
        let (paint, states) = if on {
            (&filled, filled_states)
        } else {
            (&bare, bare_states)
        };
        row = row.child(
            div()
                .id(ElementId::NamedInteger(
                    SharedString::new_static("ts-range-freq"),
                    i as u64,
                ))
                .debug_selector(move || format!("ts-range-freq-{tile_id}-{word}"))
                .px_1()
                .text_xs()
                .rounded(theme.radius)
                .text_color(paint.text)
                .when_some(paint.fill, |d, fill| d.bg(fill))
                .pointer_states(states)
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, _window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.range_freq_clicked(f, cx));
                    }
                })
                .child(word),
        );
    }
    row
}

/// Paint the action menu (mouse pass, 2026-09-24) on the series list's
/// surface: one row per `MenuRow`, the highlighted one lit, a disabled
/// one muted with its reason where its chord would be, a tick column
/// ahead of a toggle's title. A hover moves the highlight (the mouse
/// form of `j`/`k`), a click picks (`menu_pick`, `enter`'s own path).
pub(crate) fn render_menu(
    m: &MenuState,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    cx: &App,
) -> Deferred {
    let theme = cx.theme();
    let hover = row_paint(theme).hover;
    let mut list = popover_surface(cx)
        .debug_selector(move || format!("ts-menu-{tile_id}"))
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_popup_with_window(window, cx))
        });
    for (i, row) in m.rows.iter().enumerate() {
        list = list.child(match row {
            MenuRow::Separator => div()
                .my_0p5()
                .mx_neg_1()
                .border_b(px(2.))
                .border_color(theme.border)
                .into_any_element(),
            MenuRow::Section(s) => div()
                .px(scale::design(ROW_INSET))
                .pt_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .overflow_hidden()
                .text_ellipsis()
                .child(s.clone())
                .into_any_element(),
            MenuRow::Action {
                title,
                hint,
                enabled,
                checked,
                ..
            } => {
                let disabled = enabled.is_err();
                let trailing: SharedString = match enabled {
                    Err(r) => (*r).into(),
                    Ok(()) => hint.clone(),
                };
                let tick: Option<&'static str> = checked.map(|on| if on { "\u{2713}" } else { "" });
                let lit = i == m.highlighted && !disabled;
                h_flex()
                    .id(ElementId::NamedInteger(
                        SharedString::new_static("ts-menu-row"),
                        i as u64,
                    ))
                    .h(scale::design(ROW_HEIGHT))
                    .px(scale::design(ROW_INSET))
                    .rounded(theme.radius)
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .when(lit, |d| {
                        d.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .when(!lit, |d| {
                        d.text_color(if disabled {
                            theme.muted_foreground
                        } else {
                            theme.popover_foreground
                        })
                        .when(!disabled, |d| d.hover(move |s| s.bg(hover)))
                    })
                    .debug_selector(move || format!("ts-menu-row-{tile_id}-{i}"))
                    // Stops, like every popup row: a click that picks a
                    // row must not also run the shell's tile click under
                    // the popup (the series list's own rule).
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                        }
                    })
                    .on_mouse_move({
                        let tile = tile.clone();
                        move |_, _, cx| tile.update(cx, |t, cx| t.menu_hover(i, cx))
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .when_some(tick, |d, tick| {
                                d.child(div().w(scale::design(14.)).flex_shrink_0().child(tick))
                            })
                            .child(title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .when(lit, |d| d.text_color(theme.accent_foreground))
                            .when(!lit, |d| d.text_color(theme.muted_foreground))
                            .child(trailing),
                    )
                    .into_any_element()
            }
        });
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
            Colour::Custom(c) => c.to_hsla(),
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
