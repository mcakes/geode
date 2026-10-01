//! Table delegate over a prepared `Rc<GridIndex>` installed by the tile, and
//! the window of measure cells it fills outside render (an install, or the
//! table's `visible_rows_changed`); `render_td` only reads the window.
//! Cursor, loading state, and cell editor are read-only mirrors of tile
//! state. Column zero
//! is a pinned connector tree the cell cursor does not enter: a package
//! paints a chevron, its template as a neutral chip, its shorthand summary
//! and a muted leg count; a leg its drawn connector lines (a hairline the
//! full row height, stopping at the stub on the last leg) in
//! its package's chevron lane and its full shorthand; a bare line its
//! shorthand alone, on the edge the legs' text shares.
//!
//! A grouping row paints a chevron and its value at medium weight
//! (`pricer-group-{row}`). Two rows have a ground of their own, set by
//! `render_tr` (`row_ground`): a grouping row (`Paints::group`) and a
//! package leg (`Paints::leg`, a faint tint marking it as inside its
//! package). Each row's text, cells and gutter take its palette floored on
//! that ground. Bare lines and package rows leave the ground to the table:
//! the tree column carries a package's structure, and hover and selection
//! are the table's row grounds, which replace a row's own and which
//! further per-cell fills would obscure.

use crate::core::columns::CellState;
use crate::grid::{CellPass, FillSource, GridCell, GridIndex, GridRowKind};
use crate::paint::{CellColour, Paints, RowPalette, cell_colour};
use crate::popup::{ChoicePaint, render_choice};
use crate::tile::PricerTile;
use geode_core::colour::{Anchors, NamedColours, Sign, Tokens};
use geode_core::grid::selection::{Resolved, SelectKind};
use geode_core::view::Colour;
use geode_shell::colfit::{FitMetrics, FittedWidths};
use geode_shell::fonts;
use geode_shell::linenumbers::{GUTTER_GAP_PX, LineNumbers, gutter_number, gutter_px};
use geode_shell::shell::colours::{anchors_from_theme, theme_signature, tokens_from_theme};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::scale;
use geode_tile::colour::{ColourCache, Resolved as ColourResolved};
use geode_tile::grid::{FIRST_WINDOW, RowCache, WindowCache, WindowRequest};
use geode_widgets::datefield::{self, DateTimeField, SegmentPaint, SegmentText};
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, FontWeight, Hsla,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, SharedString, Stateful, TextAlign,
    WeakEntity, Window, div, px, relative,
};
use gpui_component::input::{Input, InputState};
use gpui_component::table::{Column, ColumnFixed, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Size, Theme, h_flex};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// The tree column: fits a two-leg call spread's package row
/// (`▾ CS Z26 4800/5200 · 2 legs`, a real 13-character summary) at the
/// largest font (checked below), in pixels like every width here; not
/// resizable. Longer summaries ellipsize; the leg count stays.
const TREE_WIDTH: f32 = 230.0;
/// One depth step, and the slot every row reserves at its lane, both on
/// the rem scale. The slot holds a package's chevron or a leg's connector
/// and is empty on a bare line, so roots share one leading edge whether or
/// not they carry a chevron. A leg's connector takes its parent's lane
/// (`lane_depth`), so it hangs directly under the package's chevron and the
/// leg's text starts where the package's chip starts.
const INDENT: f32 = 14.0;
const CHEVRON_SLOT: f32 = 14.0;
/// A template chip's horizontal padding, each side, and the gap between
/// the tree cell's parts (slot, chip, text, note), in design px.
const CHIP_PAD_X: f32 = 4.0;
const TREE_GAP: f32 = 6.0;

/// The table's size, which sets its row height and cell padding. The tree
/// column keeps this size's horizontal padding and drops its vertical
/// padding, so the tree cell spans the full row and a leg's drawn
/// connector joins the next leg's across the row boundary.
pub(crate) const TABLE_SIZE: Size = Size::XSmall;

/// A leg's vertical connector line: a hairline through its slot's centre
/// from the row's top to its bottom, so consecutive legs' lines join into
/// one; on a package's last leg it stops at the stub (mid-height), closing
/// the package with a square corner. The slot is `relative` and spans the
/// full row height (the tree column has no vertical padding). `px(1.)` is
/// a hairline, not a layout size; the half-pixel margins centre it.
fn connector_line(row_ix: usize, last: bool, colour: Hsla) -> Div {
    div()
        .absolute()
        .top_0()
        .left(relative(0.5))
        .ml(px(-0.5))
        .w(px(1.))
        .map(|el| {
            if last {
                // To the stub's bottom edge, so the corner is square.
                el.bottom(relative(0.5)).mb(px(-0.5))
            } else {
                el.bottom_0()
            }
        })
        .bg(colour)
        .debug_selector(move || format!("pricer-connector-line-{row_ix}"))
}

/// A leg's connector stub: a hairline at mid-height from the vertical
/// line to the slot's right edge, pointing at the leg's text.
fn connector_stub(row_ix: usize, colour: Hsla) -> Div {
    div()
        .absolute()
        .top(relative(0.5))
        .mt(px(-0.5))
        .left(relative(0.5))
        .ml(px(-0.5))
        .right_0()
        .h(px(1.))
        .bg(colour)
        .debug_selector(move || format!("pricer-connector-stub-{row_ix}"))
}

/// How many `TREE_GAP`s the tree cell paints: one between each pair of
/// adjacent parts. The slot and the text are always present; a chip and a
/// note only when the row has them. The cell lays its parts out with one
/// flex `gap`, so this is what `render_cell` paints and what
/// `fit_columns` and the width test measure.
pub(crate) fn tree_gaps(chip: bool, note: bool) -> usize {
    1 + usize::from(chip) + usize::from(note)
}

/// The depth a row's tree cell indents by: a leg's connector sits in its
/// package's chevron slot, one step out from the leg's own depth.
fn lane_depth(kind: GridRowKind, depth: usize) -> usize {
    match kind {
        GridRowKind::Leg { .. } => depth.saturating_sub(1),
        _ => depth,
    }
}
/// What the empty table says: the next action, not an icon.
pub(crate) const EMPTY_TEXT: &str = "No lines — press o to add one";
pub(crate) const LOADING_TEXT: &str = "Loading sheet…";
pub(crate) const TREE_COL: usize = 0;
/// The tree column's stable key, in `column()` and in fitted widths.
pub(crate) const TREE_KEY: &str = "__tree";

/// The gutter number of every painted grid row. Relative mode measures
/// from the cursor row; with no cursor row it numbers absolutely. Off
/// numbers nothing.
pub(crate) fn number_rows(
    mode: LineNumbers,
    len: usize,
    cursor: Option<usize>,
) -> Vec<Option<usize>> {
    let (mode, at) = match (mode, cursor) {
        (LineNumbers::Relative, Some(c)) => (mode, c),
        (LineNumbers::Relative, None) => (LineNumbers::On, 0),
        (mode, _) => (mode, 0),
    };
    (0..len).map(|row| gutter_number(mode, row, at)).collect()
}

/// What `SheetDelegate::refresh_numbers` last derived from: row count,
/// the cursor row (relative mode only), and the mode.
type NumbersStamp = (usize, Option<usize>, LineNumbers);

/// Requests a package expansion toggle at a grid row. Emitted by the table after
/// selecting the clicked row; the tile owns the expansion state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChevronClicked(pub usize);

impl EventEmitter<ChevronClicked> for TableState<SheetDelegate> {}

/// A header drag dropped: the PLAN column at `from` now sits at `to`.
/// Emitted by the table's `move_column` hook; the tile owns the plan, so
/// the delegate reorders nothing of its own and the next `install_model`
/// hands it the permuted model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnMoved {
    pub from: usize,
    pub to: usize,
}

impl EventEmitter<ColumnMoved> for TableState<SheetDelegate> {}

/// Every mouse selection gesture a cell, the tree cell or the line-number
/// gutter recognises, carried to the tile's `pointer`: the one door a
/// shift+click and a drag go through to `start_selection` and
/// `clear_selection`, so the mouse never reaches a selection state the
/// keys could not. `col` is a PLAN column; `None` is the tree cell (or
/// its gutter), the row's handle, which is never a selection member. A
/// `Drag`'s `tree` is where its press landed, not where the pointer is
/// now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellPointer {
    Press {
        row: usize,
        col: Option<usize>,
        shift: bool,
    },
    Drag {
        row: usize,
        col: Option<usize>,
        tree: bool,
    },
}

impl EventEmitter<CellPointer> for TableState<SheetDelegate> {}

/// A press an element inside a cell owns, recorded on the way up so the
/// cell and the row it bubbles through next do not report it as their
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InnerPress {
    /// The open editor's own cell: caret placement, a text selection.
    Editor,
    /// A package chevron: a toggle, never a selection gesture.
    Chevron,
}

/// A paint-time copy of the tile's open editor (`PricerTile::sync_editor`):
/// the grid cell it sits on, its field, and the typeahead's prepared rows.
#[derive(Clone)]
pub(crate) struct EditorPaint {
    pub row: usize,
    /// The plan column (never the tree).
    pub col: usize,
    pub field: EditorField,
    pub choice: Option<Rc<ChoicePaint>>,
}

/// What the open editor paints in its cell: a text `Input` (every text
/// and typeahead cell), or an expiry's segmented date field — its
/// prepared segments and the focus handle the painted field tracks, so
/// the shell's insert-focus predicate and the field's key listener see
/// the same focus.
#[derive(Clone)]
pub(crate) enum EditorField {
    Text(Entity<InputState>),
    Date {
        paint: DateFieldPaint,
        focus: FocusHandle,
    },
}

/// The date field as painted: the segments exactly as
/// [`DateTimeField::segments`] answers them and the per-tile selector the
/// painter hangs on each segment, prepared by [`DateFieldPaint::of`]
/// whenever the field changes — never in render. Cloning is two refcount
/// bumps (`Rc<[_]>`, `SharedString`), so the delegate's mirror costs no
/// allocation per frame.
#[derive(Clone)]
pub(crate) struct DateFieldPaint {
    pub segments: Rc<[SegmentText]>,
    pub selector: SharedString,
}

impl DateFieldPaint {
    pub(crate) fn of(field: &DateTimeField, tile_id: u64) -> Self {
        Self {
            segments: field.segments().into(),
            selector: format!("pricer-date-seg-{tile_id}").into(),
        }
    }
}

/// The segmented date field in a grid cell: flush segments, no frame —
/// the cell's cursor border is the only chrome, as with the text editor.
/// The row tracks the field's focus handle and routes keys to the tile
/// (`PricerTile::date_field_key`) before they bubble on; a handled key
/// stops there, a chord or a key the field does not take bubbles to the
/// shell. A mouse-down on a segment selects it and stops propagation, so
/// the table's own cell click (which cancels an open editor) never fires
/// for a click aimed into the field.
fn render_date_field(
    paint: &DateFieldPaint,
    focus: &FocusHandle,
    paints: &Paints,
    (own, muted): (Hsla, Hsla),
    theme: &Theme,
    tile: &Entity<PricerTile>,
) -> impl IntoElement {
    let segment_paint = SegmentPaint {
        rest_text: own,
        rest_fill: None,
        active_text: paints.date_active_text,
        active_fill: theme.primary,
        typing_text: paints.date_typing_text,
        typing_fill: theme.accent,
        separator: muted,
        suffix: muted,
        radius: theme.radius_tokens().sm,
        flush: true,
    };
    let keys = tile.clone();
    let clicks = tile.clone();
    h_flex()
        .track_focus(focus)
        .h_full()
        .items_center()
        .font_family(fonts::MONO)
        // gpui's default line height (phi, ~1.6em) makes a segment's fill
        // taller than an XSmall row's content box, so the active fill
        // would clip against the cursor border; 1.25em fits inside it
        // with the glyphs where the cell's own text sat (both centred).
        .line_height(relative(1.25))
        .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
            if keys.update(cx, |t, cx| t.date_field_key(event, window, cx)) {
                cx.stop_propagation();
            }
        })
        .child(datefield::paint(
            &paint.segments,
            None,
            segment_paint,
            paint.selector.clone(),
            move |segment, window, cx| {
                clicks.update(cx, |t, cx| t.date_segment_clicked(segment, window, cx));
            },
        ))
}

/// What a fuzzy `/` result table paints from: a painter over the all-open
/// index `/` built, and the measure cells of the rows the find table last
/// reported ([`FindPaint::show`]), formatted there through [`CellPass`]
/// over the tile's live sheet. The paint callback only reads it; a row it
/// does not hold paints blank measure cells.
pub(crate) struct FindPaint {
    pub(crate) painter: SheetDelegate,
    /// The theme the painter's paints were derived under.
    signature: [Hsla; 28],
    cells: RowCache<GridCell>,
    /// The tile's index when `/` opened. The find index's rows name rollup
    /// nodes and sheet rows as that build saw them; once the tile installs
    /// another index they may name others, so the measure cells paint
    /// blank rather than read through a stale index. A price-only refresh
    /// keeps the tile's index, and so the find's cells read the new prices
    /// as rows enter view.
    opened_on: Rc<GridIndex>,
    /// Cells formatted, ever; each `/` open starts a new `FindPaint`.
    #[cfg(test)]
    pub(crate) fills: usize,
}

impl FindPaint {
    pub(crate) fn new(painter: SheetDelegate, opened_on: Rc<GridIndex>, theme: &Theme) -> Self {
        FindPaint {
            painter,
            signature: theme_signature(theme),
            cells: RowCache::default(),
            opened_on,
            #[cfg(test)]
            fills: 0,
        }
    }

    /// Hold exactly the reported grid `rows`: keep those already
    /// formatted, format the rest from `src`, drop the others. When the
    /// tile's index is no longer the one `/` opened on, hold nothing.
    pub(crate) fn show(&mut self, rows: &[usize], tile_model: &Rc<GridIndex>, src: FillSource<'_>) {
        if !Rc::ptr_eq(tile_model, &self.opened_on) {
            self.cells.clear();
            return;
        }
        let model = Rc::clone(&self.painter.model);
        let mut pass = CellPass::new(src, &model);
        #[cfg(test)]
        let fills = &mut self.fills;
        self.cells.set_rows(rows, model.columns.len(), |g, c| {
            #[cfg(test)]
            {
                *fills += 1;
            }
            pass.cell(g, c)
        });
    }

    /// Drop every cell; the next report refills the rows shown.
    pub(crate) fn clear(&mut self) {
        self.cells.clear();
    }

    /// Paint one find cell, re-deriving the paints after a theme change.
    pub(crate) fn render(
        &mut self,
        find_row: &geode_shell::fuzzyfind::FindRow<'_>,
        col: usize,
        cx: &App,
    ) -> gpui::AnyElement {
        let signature = theme_signature(cx.theme());
        if self.signature != signature {
            self.painter.set_paints(Paints::derive(cx.theme()));
            self.signature = signature;
        }
        self.painter
            .render_find_cell(&self.cells, find_row, col, cx)
    }

    #[cfg(test)]
    pub(crate) fn cells(&self) -> &RowCache<GridCell> {
        &self.cells
    }
}

pub struct SheetDelegate {
    /// The grid index shared with the tile; installs replace it.
    pub(crate) model: Rc<GridIndex>,
    /// The formatted measure cells on screen, filled outside render;
    /// `render_td` only reads it. A miss paints a blank cell, and an open
    /// editor on it still paints.
    pub(crate) window: WindowCache<GridCell>,
    /// The range the table last reported; before any report the first
    /// `FIRST_WINDOW` rows (a one-row sheet never gets a report).
    requested: WindowRequest,
    /// `(grid row, plan column)`; `None` with no cursor row.
    pub(crate) cursor: Option<(usize, usize)>,
    /// The tile's resolved selection, mirrored by `sync_cursor`: grid
    /// rows × plan columns. The tree column is never a member; it tints
    /// only as a whole selected row's handle.
    pub(crate) selected: Option<Resolved>,
    pub(crate) paints: Paints,
    /// The tile's `loading`, mirrored by `install_model`: the empty table
    /// says `Loading sheet…` rather than inviting an `o` the tile would
    /// refuse.
    pub(crate) loading: bool,
    /// The chevron's pointer states, cached per rest paint: `[line
    /// palette, group palette]` (a group row's chevron rests in the group
    /// muted paint, floored on its ground).
    chevron: [Option<(control::ControlInputs, control::ControlPaint)>; 2],
    /// Named column colours floored on a row palette's grounds (a group
    /// row's or a leg's), keyed by the resolved colour and the row ground
    /// they were floored under: such rows paint few distinct colours, so a
    /// linear memo keeps the floor out of every frame. Two palettes whose
    /// grounds coincide floor alike (hover and selected are shared).
    ground_named: Vec<(Hsla, Hsla, Hsla)>,
    /// The tile's open cell editor, a read-only mirror of the tile's.
    pub(crate) editor: Option<EditorPaint>,
    /// The typeahead's rows call back into the tile; a dropped tile
    /// paints no popup.
    pub(crate) tile: WeakEntity<PricerTile>,
    /// `[ui] line_numbers`, mirrored from the `UiSettings` global by the
    /// tile (`PricerTile::on_ui_settings`), which refreshes the table on a
    /// change: the tree column's width includes the gutter.
    pub(crate) line_numbers: LineNumbers,
    /// Gutter text per grid row and the
    /// gutter's width. `refresh_numbers` prepares both outside render, so
    /// `render_td` only clones a refcount.
    numbers: Vec<SharedString>,
    gutter: f32,
    numbers_stamp: Option<NumbersStamp>,
    /// Widths `:autosize` fitted, keyed by the vocabulary's column name
    /// ([`TREE_KEY`] for the tree), in pixels without the gutter.
    /// `column()` prefers an entry over the view's width, so every
    /// `install_model` refresh keeps it; a name the current view lacks is
    /// ignored and a column with no entry keeps the view's width.
    pub(crate) fitted: FittedWidths,
    /// The `(row, col)` a mouse move last emitted a `CellPointer::Drag`
    /// for. gpui fires a move per pixel, not per cell; without this a held
    /// drag would re-run the tile's `pointer` on every frame. Stale after
    /// a drag no cell saw released, which costs one extra emission at most.
    drag_last: Option<(usize, Option<usize>)>,
    /// `Some` while the primary button is down because of a press a cell,
    /// the tree cell or the gutter of this table caught — `true` when it
    /// was the tree cell or the gutter. `None` while another element owns
    /// the drag (a scrollbar, the header, a tile divider, a chevron), so a
    /// button held over the cells from elsewhere never starts or extends
    /// a selection. Cleared by any release: each cell's `on_mouse_up` /
    /// `on_mouse_up_out` pair sees every release wherever it lands.
    drag_origin: Option<bool>,
    /// Set by a press the open editor's cell or a chevron owns, and taken
    /// by the row's press handler, which bubbles after the cell's: without
    /// it the row would report the press at the cursor column — for the
    /// editor, its own cell — and so cancel the edit the press was aimed
    /// into.
    inner_press: Option<InnerPress>,
    /// The `colors.toml` definitions the tile last handed down
    /// (`set_colours`), the named-colour resolutions cached against them,
    /// and the theme inputs those resolutions were made under: the
    /// theme's full colour signature with the `Anchors`/`Tokens` pair
    /// derived from it, re-derived only when the signature moves.
    colours: Arc<NamedColours>,
    colour_cache: ColourCache,
    theme_inputs: Option<([Hsla; 28], Anchors, Tokens)>,
    /// The text each painted find measure cell carried, by (grid row,
    /// table column), as `render_find_cell` put it into the element.
    #[cfg(test)]
    pub(crate) find_painted: std::collections::HashMap<(usize, usize), String>,
}

/// The name column `plan_col` carries a `Colour::Named` of, if it does.
///
/// A free function over the model rather than a `&self` method: the
/// returned `&str` borrows `model` alone, leaving the delegate's other
/// fields free for the `&mut` the colour cache needs.
fn named_colour_of(model: &GridIndex, plan_col: usize) -> Option<&str> {
    match model.columns.get(plan_col).map(|c| &c.colour) {
        Some(Colour::Named(name)) => Some(name.as_str()),
        _ => None,
    }
}

impl SheetDelegate {
    pub(crate) fn new(theme: &Theme, tile: WeakEntity<PricerTile>) -> Self {
        SheetDelegate {
            model: Rc::new(GridIndex::default()),
            window: WindowCache::default(),
            requested: WindowRequest::with_first(FIRST_WINDOW),
            cursor: None,
            selected: None,
            paints: Paints::derive(theme),
            loading: false,
            chevron: [None, None],
            ground_named: Vec::new(),
            editor: None,
            tile,
            line_numbers: LineNumbers::Off,
            numbers: Vec::new(),
            gutter: 0.0,
            numbers_stamp: None,
            fitted: FittedWidths::new(),
            drag_last: None,
            drag_origin: None,
            inner_press: None,
            colours: Arc::new(NamedColours::default()),
            colour_cache: ColourCache::new(),
            theme_inputs: None,
            #[cfg(test)]
            find_painted: Default::default(),
        }
    }

    /// The tile hands these down with every model it installs. A
    /// different `Arc` means a reloaded `colors.toml`: everything resolved
    /// so far was resolved from the old definitions, so the cache goes
    /// with it. Pointer equality, not a deep compare — the factory shares
    /// exactly one `Arc` per loaded doc, so the same pointer IS the same
    /// definitions, and the common case (every rebuild, no reload) costs
    /// one pointer compare.
    pub(crate) fn set_colours(&mut self, colours: Arc<NamedColours>) {
        if !Arc::ptr_eq(&self.colours, &colours) {
            self.colours = colours;
            self.colour_cache.invalidate();
        }
    }

    /// Re-derive `theme_inputs` if and only if one of the twenty-eight
    /// theme colours the derivation reads has moved.
    ///
    /// The compare is the FULL signature, not a sentinel or two: a theme
    /// change that leaves `background`/`foreground` equal while moving an
    /// anchor would otherwise keep painting the old colour, and the
    /// `ColourCache` sitting behind this could never catch it — the stale
    /// derived pair IS its key. The steady path is 28 `Hsla` copies and
    /// 28 `Hsla` compares, with no `Hsla -> Rgb` conversion at all.
    fn ensure_theme_inputs(&mut self, theme: &Theme) {
        let signature = theme_signature(theme);
        match &self.theme_inputs {
            Some((have, ..)) if *have == signature => {}
            _ => {
                self.theme_inputs = Some((
                    signature,
                    anchors_from_theme(theme),
                    tokens_from_theme(theme),
                ));
            }
        }
    }

    /// Column `plan_col`'s named colour resolved under `theme`, with its
    /// sign variants: `None` for a plain or `sign` column and for a name
    /// `colors.toml` does not define (whose cells paint as if plain).
    fn themed_cell_colour(&mut self, plan_col: usize, theme: &Theme) -> Option<ColourResolved> {
        self.ensure_theme_inputs(theme);
        // Four disjoint field borrows in one body — `model`, `colours` and
        // `theme_inputs` shared, `colour_cache` mutable. A `&self` method
        // for the name would borrow the whole delegate and shut the
        // cache's own `&mut` out.
        let name = named_colour_of(&self.model, plan_col)?;
        let (_, anchors, tokens) = self.theme_inputs.as_ref().expect("set just above");
        self.colour_cache.get(&self.colours, name, anchors, tokens)
    }

    /// The text colour `render_cell` paints the cell at (`row_ix`,
    /// `plan_col`) with: the state paint, unless the cell is an own value
    /// in a column whose `color` says otherwise (`paint::cell_colour`).
    /// A row the model lacks, and a cell the window lacks, is the own
    /// paint.
    pub(crate) fn text_colour(&mut self, row_ix: usize, plan_col: usize, theme: &Theme) -> Hsla {
        let cell = self.window.get(row_ix, plan_col).map(|c| (c.state, c.sign));
        self.colour_of(row_ix, plan_col, cell, theme)
    }

    /// [`Self::text_colour`] over a cell's `(state, sign)` read from
    /// wherever it was prepared: the window, or a find table's rows.
    fn colour_of(
        &mut self,
        row_ix: usize,
        plan_col: usize,
        cell: Option<(CellState, Option<Sign>)>,
        theme: &Theme,
    ) -> Hsla {
        let model = Rc::clone(&self.model);
        if row_ix >= model.len() {
            return self.paints.own;
        }
        // A group row or a leg: the palette floored on its own ground.
        let palette = self.row_palette(row_ix);
        let Some((state, sign)) = cell else {
            return palette.map_or(self.paints.own, |p| p.own);
        };
        let colour = model
            .columns
            .get(plan_col)
            .map_or(&Colour::None, |c| &c.colour);
        if let Some(palette) = palette {
            let base = palette.text(state);
            return match cell_colour(colour, state, sign) {
                CellColour::State => base,
                CellColour::Bearish => palette.bearish,
                CellColour::Bullish => palette.bullish,
                CellColour::Named(sign) => match self.themed_cell_colour(plan_col, theme) {
                    Some(c) => self.on_ground(palette, c.for_sign(Some(sign))),
                    None => base,
                },
            };
        }
        let base = self.paints.text(state);
        match cell_colour(colour, state, sign) {
            CellColour::State => base,
            CellColour::Bearish => theme.chart_bearish,
            CellColour::Bullish => theme.chart_bullish,
            CellColour::Named(sign) => self
                .themed_cell_colour(plan_col, theme)
                .map_or(base, |c| c.for_sign(Some(sign))),
        }
    }

    /// The search table shares value formatting, colours, and tree depth, without
    /// edit/expansion handlers belonging to the original table. Measure
    /// cells come from `cells`, the rows the find table last reported; a
    /// row it does not hold paints blank.
    pub(crate) fn render_find_cell(
        &mut self,
        cells: &RowCache<GridCell>,
        find_row: &geode_shell::fuzzyfind::FindRow<'_>,
        col_ix: usize,
        cx: &App,
    ) -> gpui::AnyElement {
        let row_ix = find_row.source_row();
        let model = self.model.clone();
        let Some(row) = model.tree(row_ix) else {
            return div().into_any_element();
        };
        let el = div()
            .size_full()
            .flex()
            .items_center()
            .font_family(fonts::MONO)
            .overflow_hidden()
            .whitespace_nowrap();
        if col_ix == 0 {
            let indices: Vec<_> = find_row
                .indices()
                .iter()
                .copied()
                .filter(|ix| *ix < row.search.chars().count())
                .collect();
            return el
                .text_color(if find_row.is_context() {
                    cx.theme().muted_foreground
                } else {
                    cx.theme().foreground
                })
                .child(find_row.gutter(cx))
                .child(
                    div()
                        .w(scale::design(
                            lane_depth(row.kind, row.depth) as f32 * INDENT,
                        ))
                        .flex_shrink_0(),
                )
                .child(find_row.disclosure(scale::design(CHEVRON_SLOT), "", cx))
                .child(div().w(scale::design(TREE_GAP)).flex_shrink_0())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(geode_shell::palette::highlighted_title(
                            row.search,
                            &indices,
                            cx.theme().foreground,
                        )),
                )
                .into_any_element();
        }
        let col = col_ix - 1;
        let cell = cells.get(row_ix, col);
        let text = cell.map(|c| c.text.clone()).unwrap_or_default();
        #[cfg(test)]
        self.find_painted.insert((row_ix, col_ix), text.to_string());
        let colour = if find_row.is_context() {
            cx.theme().muted_foreground
        } else {
            self.colour_of(row_ix, col, cell.map(|c| (c.state, c.sign)), cx.theme())
        };
        el.when(model.columns[col].right, |el| el.justify_end())
            .text_color(colour)
            .child(text)
            .into_any_element()
    }

    /// Install freshly derived paints (a theme change) and drop the
    /// named-colour memo, whose floors were taken on the old grounds: the
    /// memo is keyed by the row ground, but a theme can change the hover
    /// or selected grounds it also floors on while keeping that one.
    pub(crate) fn set_paints(&mut self, paints: Paints) {
        self.paints = paints;
        self.ground_named.clear();
    }

    /// A named column colour on a row with a ground of its own: floored
    /// on `palette`'s ground and the hover and selected grounds that
    /// replace it, memoised. Entries for grounds the current paints no
    /// longer hold are dropped as they are met.
    fn on_ground(&mut self, palette: RowPalette, c: Hsla) -> Hsla {
        let ground = palette.ground;
        if let Some((.., out)) = self
            .ground_named
            .iter()
            .find(|(input, at, _)| *input == c && *at == ground)
        {
            return *out;
        }
        let live = [self.paints.group.ground, self.paints.leg.ground];
        self.ground_named.retain(|(_, at, _)| live.contains(at));
        let out = palette.floor(c);
        self.ground_named.push((c, ground, out));
        out
    }

    /// The palette of grid row `row` when it has a ground of its own: a
    /// grouping row's, or a package leg's (every leg, the last included);
    /// `None` for a bare line, a package row and a filler row past the
    /// model, whose ground is the table's and whose text the line palette.
    pub(crate) fn row_palette(&self, row: usize) -> Option<RowPalette> {
        match self.model.kind(row)? {
            GridRowKind::Group { .. } => Some(self.paints.group),
            GridRowKind::Leg { .. } => Some(self.paints.leg),
            GridRowKind::Package { .. } | GridRowKind::Line => None,
        }
    }

    /// The paint of grid row `row`'s tree-column text: a leg's shorthand
    /// muted, every other row's own, each in the row's palette
    /// (`row_palette`) so a leg's and a group row's read on their grounds.
    pub(crate) fn tree_text_paint(&self, row: usize) -> Hsla {
        let palette = self.row_palette(row);
        match self.model.kind(row) {
            Some(GridRowKind::Leg { .. }) => palette.map_or(self.paints.muted, |p| p.muted),
            _ => palette.map_or(self.paints.own, |p| p.own),
        }
    }

    /// The date field's rest and muted text paints on grid row `row`: the
    /// row's palette (a leg's, on the leg ground), else the line's. Its
    /// segment fills are composited over the same grounds in `Paints`.
    pub(crate) fn date_text_paints(&self, row: usize) -> (Hsla, Hsla) {
        self.row_palette(row)
            .map_or((self.paints.own, self.paints.muted), |p| (p.own, p.muted))
    }

    /// The ground `render_tr` paints under grid row `row`: its palette's
    /// (`row_palette`), `None` where the table's shows.
    pub(crate) fn row_ground(&self, row: usize) -> Option<Hsla> {
        self.row_palette(row).map(|p| p.ground)
    }

    /// The colour `render_th` paints column `plan_col`'s label with: a
    /// named colour's base (a header has no sign), `None` for the
    /// component's own foreground.
    pub(crate) fn header_colour(&mut self, plan_col: usize, theme: &Theme) -> Option<Hsla> {
        self.themed_cell_colour(plan_col, theme).map(|c| c.base)
    }

    /// Clear the window and refill the range the table last asked for,
    /// clamped to the rows that exist: the table does not re-report an
    /// unchanged range, so an install must not wait for it.
    pub(crate) fn refill_window(&mut self, src: FillSource<'_>) {
        self.window.clear();
        if let Some(range) = self.requested.refill_range(self.model.len()) {
            self.fill_window(range, src);
        }
    }

    fn fill_window(&mut self, range: Range<usize>, src: FillSource<'_>) {
        let model = Rc::clone(&self.model);
        let mut pass = CellPass::new(src, &model);
        self.window
            .set_window(range, model.columns.len(), |g, c| pass.cell(g, c));
    }

    /// Fit the tree column and every plan column to its header and the
    /// rows in the window — what the table last showed, as the blotter
    /// measures. The tree column measures exactly what `render_cell`
    /// paints: the row's lane indent, the chevron slot, the gaps between
    /// its parts (`tree_gaps`) and, when present, the chip (its padding
    /// and tag), then the text and the note; indent, slot, padding and
    /// gaps on the rem scale.
    ///
    /// `None` with nothing to measure: the sheet is still loading, or the
    /// window is empty.
    pub(crate) fn fit_columns(&self, m: &FitMetrics) -> Option<FittedWidths> {
        let rows = self.window.window();
        if self.loading || rows.is_empty() {
            return None;
        }
        let design = |px: f32| px * m.rem_px / scale::DESIGN_REM;
        let mut out = FittedWidths::new();
        out.insert(
            TREE_KEY.to_string(),
            m.fit(rows.clone().filter_map(|g| self.model.tree(g)).map(|r| {
                let (chip, note) = (!r.tag.is_empty(), !r.note.is_empty());
                let depth = lane_depth(r.kind, r.depth);
                let chip_px = if chip {
                    design(2.0 * CHIP_PAD_X) + m.text_px(r.tag)
                } else {
                    0.0
                };
                design(
                    depth as f32 * INDENT + CHEVRON_SLOT + tree_gaps(chip, note) as f32 * TREE_GAP,
                ) + chip_px
                    + m.text_px(r.text)
                    + m.text_px(r.note)
            })),
        );
        for (col, c) in self.model.columns.iter().enumerate() {
            let cells = rows
                .clone()
                .filter_map(|g| self.window.get(g, col).map(|c| c.text.as_ref()));
            out.insert(c.name.to_string(), m.fit_text(&c.label, cells));
        }
        Some(out)
    }

    /// Re-derive the gutter text and width when the model's shape, the
    /// mode, or (in relative mode) the cursor row moved. The tile calls it
    /// after every model install, cursor sync and mode change, BEFORE the
    /// table re-reads `column()`: a stale width would clip the tree text
    /// or leave a hole where the gutter was.
    pub(crate) fn refresh_numbers(&mut self) {
        let mode = self.line_numbers;
        let len = self.model.len();
        let cursor = match mode {
            LineNumbers::Relative => self.cursor.map(|(row, _)| row),
            _ => None,
        };
        let stamp = (len, cursor, mode);
        if self.numbers_stamp == Some(stamp) {
            return;
        }
        self.numbers_stamp = Some(stamp);
        self.gutter = gutter_px(mode, len);
        self.numbers.clear();
        self.numbers
            .extend(number_rows(mode, len, cursor).into_iter().map(|n| {
                n.map(|n| SharedString::from(n.to_string()))
                    .unwrap_or_default()
            }));
    }

    /// The plan column behind table column `col_ix`; `None` is the tree.
    pub(crate) fn plan_col(col_ix: usize) -> Option<usize> {
        (col_ix != TREE_COL).then(|| col_ix - 1)
    }

    /// The gutter's width in px — `0` when off.
    pub(crate) fn gutter_px(&self) -> f32 {
        self.gutter
    }

    /// The gutter's text paint on grid row `row`: the row's own paint on
    /// the cursor row, muted elsewhere — the row's palette on a group row
    /// or a leg, which have a ground of their own (`row_palette`), the
    /// line palette on every other.
    pub(crate) fn gutter_paint(&self, row: usize) -> Hsla {
        let on_cursor = self.cursor.is_some_and(|(r, _)| r == row);
        match (on_cursor, self.row_palette(row)) {
            (true, None) => self.paints.own,
            (false, None) => self.paints.muted,
            (true, Some(p)) => p.own,
            (false, Some(p)) => p.muted,
        }
    }

    /// The cached gutter text for grid row `row`; `None` when off. Reads
    /// the cache as painted — it does not refresh it.
    #[cfg(test)]
    pub(crate) fn gutter_text(&self, row: usize) -> Option<SharedString> {
        if self.line_numbers == LineNumbers::Off {
            return None;
        }
        self.numbers.get(row).cloned()
    }

    /// What the empty table paints: `Loading sheet…` while the tile's
    /// load is pending, else the next action.
    pub(crate) fn empty_text(&self) -> &'static str {
        if self.loading {
            LOADING_TEXT
        } else {
            EMPTY_TEXT
        }
    }

    /// Derive chevron pointer states against row_hover, the background the table paints
    /// under the pointer. The chevron's rest text is its row palette's muted paint
    /// (the group palette's on a group row), already floored on that hover ground.
    fn chevron_states(&mut self, theme: &Theme, group: bool) -> control::ControlPaint {
        let rest = if group {
            self.paints.group.muted
        } else {
            self.paints.muted
        };
        let inputs =
            control::ControlInputs::new(theme, control::Rest::Bare, self.paints.row_hover, rest);
        let slot = &mut self.chevron[usize::from(group)];
        match slot {
            Some((have, paint)) if *have == inputs => *paint,
            _ => {
                let paint = control::control_paint(&inputs);
                *slot = Some((inputs, paint));
                paint
            }
        }
    }
}

/// An in-grid text field: no input chrome (background, border, radius,
/// focus ring) and no horizontal padding, so its text sits exactly where
/// the cell's own text sat and the cell's cursor border is the only
/// frame; the row's full height; the cell's own alignment. Default-sized,
/// so its text is the table's `text_sm`, not a smaller field's.
fn cell_input(state: &Entity<InputState>, align: TextAlign) -> Input {
    Input::new(state)
        .appearance(false)
        .px_0()
        .h_full()
        .text_align(align)
}

impl TableDelegate for SheetDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        1 + self.model.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.model.len()
    }

    /// Record the reported range and fill it. The sheet, rollup and plan
    /// are the tile's live ones, read (never updated) through the weak
    /// handle: the table calls this during its layout, outside any tile
    /// update.
    fn visible_rows_changed(
        &mut self,
        visible_range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.requested.record(visible_range.clone());
        let end = visible_range.end.min(self.model.len());
        let Some(tile) = self.tile.upgrade() else {
            return;
        };
        if visible_range.start < end {
            self.fill_window(visible_range.start..end, tile.read(cx).fill_source());
        }
    }

    /// Read only on prepare and `TableState::refresh`, which is why every
    /// model swap goes through `install_model`.
    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(c) = Self::plan_col(col_ix).and_then(|i| self.model.columns.get(i)) else {
            let pad = TABLE_SIZE.table_cell_padding();
            return Column {
                // Full row height (see `TABLE_SIZE`); every part of the
                // tree cell centres itself vertically.
                paddings: Some(gpui::Edges {
                    top: px(0.),
                    bottom: px(0.),
                    ..pad
                }),
                key: SharedString::from(TREE_KEY),
                name: SharedString::from(""),
                align: TextAlign::Left,
                sort: None,
                width: px(
                    self.fitted.get(TREE_KEY).copied().unwrap_or(TREE_WIDTH) + self.gutter_px()
                ),
                fixed: Some(ColumnFixed::Left),
                movable: false,
                resizable: false,
                ..Column::default()
            };
        };
        Column {
            key: SharedString::new_static(c.name),
            name: c.label.clone(),
            align: if c.right {
                TextAlign::Right
            } else {
                TextAlign::Left
            },
            sort: None,
            width: px(self.fitted.get(c.name).copied().unwrap_or(c.width)),
            movable: true,
            resizable: true,
            ..Column::default()
        }
    }

    /// A header drag dropped. Both indices are TABLE indices; the tree
    /// column never moves and nothing moves before it (it is `fixed`, so
    /// the table refuses those drops itself, and this guard keeps a plan
    /// index from going negative should that change). The tile owns the
    /// plan, so the move is reported, not applied here.
    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let (Some(from), Some(to)) = (Self::plan_col(col_ix), Self::plan_col(to_ix)) else {
            return;
        };
        cx.emit(ColumnMoved { from, to });
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        // A named colour's base on the label; the component owns the rest
        // of the header (padding, borders) and its foreground otherwise.
        let colour = Self::plan_col(col_ix).and_then(|c| self.header_colour(c, cx.theme()));
        div()
            .size_full()
            .flex()
            .items_center()
            .when(matches!(column.align, TextAlign::Right), |el| {
                el.justify_end()
            })
            .font_family(fonts::MONO)
            .when_some(colour, |el, c| el.text_color(c))
            .debug_selector(|| format!("pricer-th-{col_ix}"))
            .child(column.name)
    }

    /// A group row's or a leg's ground (`row_ground`; no other row paints
    /// one, see the module doc) and the row's press door. A filler row past the
    /// model paints and reports nothing. The table's hover and selected
    /// grounds replace the row's, as they replace the table's own.
    ///
    /// A press on the row outside every cell (the table's trailing filler)
    /// is still a click on that row: it reports a press at the cursor's
    /// column so the tile's one pointer door clears or extends exactly as
    /// a cell press would. The row bubbles after its cells, so a press a
    /// cell already caught has set `drag_origin` (or `inner_press`) and is
    /// not reported twice; this press arms no drag.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        // A leg that is not its package's last drops the table's row
        // separator: the separator takes the row's bottom pixel, which
        // the tree cell (clipped to the row's content box) cannot paint,
        // so it would cut the leg's connector line from the next leg's.
        let joined = matches!(
            self.model.kind(row_ix),
            Some(GridRowKind::Leg { last: false })
        );
        let row = div()
            .id(("row", row_ix))
            .when(joined, |el| el.border_b_0())
            .when_some(self.row_ground(row_ix), |el, g| el.bg(g));
        if row_ix >= self.model.len() {
            return row;
        }
        row.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                let d = this.delegate_mut();
                // Taken on every row press, so it never outlives the
                // press that set it.
                if d.inner_press.take().is_some() || d.drag_origin.is_some() {
                    return;
                }
                let Some((_, col)) = d.cursor else {
                    return;
                };
                cx.emit(CellPointer::Press {
                    row: row_ix,
                    col: Some(col),
                    shift: e.modifiers.shift,
                });
            }),
        )
    }

    /// Paint loading or entry guidance in full-opacity muted text contrast-adjusted
    /// against the table background.
    fn render_empty(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_color(self.paints.muted)
            .debug_selector(|| "pricer-empty".into())
            .child(self.empty_text())
    }

    /// A cell, with the gutter beside the tree cell when line numbers are
    /// on. The gutter sits OUTSIDE the tree cell, so the depth indent
    /// starts after it (one lane of numbers whatever the depth) and the
    /// cell's own contents never cover it. The row's ground (the table's
    /// own, a group's or a leg's, hover, selection) paints under it, so it
    /// takes the row palette's floored muted paint, and the cursor row the
    /// own text paint (`gutter_paint`).
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = self.render_cell(row_ix, col_ix, window, cx);
        if col_ix != TREE_COL || self.line_numbers == LineNumbers::Off {
            return cell;
        }
        let paint = self.gutter_paint(row_ix);
        let text = self.numbers.get(row_ix).cloned().unwrap_or_default();
        div()
            .size_full()
            .flex()
            .child(
                // The gutter is the row's handle, as the tree cell is.
                Self::wire_pointer(div(), cx, row_ix, None)
                    .flex()
                    .flex_shrink_0()
                    .h_full()
                    .items_center()
                    .justify_end()
                    .w(px(self.gutter_px()))
                    .pr(px(GUTTER_GAP_PX))
                    .font_family(fonts::MONO)
                    .text_color(paint)
                    .debug_selector(|| format!("pricer-gutter-{row_ix}"))
                    .child(text),
            )
            .child(div().flex_1().min_w_0().h_full().child(cell))
            .into_any_element()
    }
}

/// The selection tint: an absolute overlay painted as a cell's first
/// child, so it sits under the text, the table's row ground still shows
/// through, and the cursor's border paints over it.
fn selection_tint(theme: &Theme) -> Div {
    div().absolute().inset_0().bg(theme.selection.opacity(0.35))
}

impl SheetDelegate {
    /// Build a cell's elements from prepared text and colours. Cell values are
    /// `SharedString` clones; palette values are copied from the tile's theme cache.
    /// Chevron pointer colours have their own input-keyed cache. An editing cell
    /// builds its field and optional typeahead from the tile's prepared editor state.
    fn render_cell(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui::AnyElement {
        let paints = self.paints;
        // One `Rc` clone, so `chevron_states(&mut self)` can run while a
        // row is borrowed.
        let model = Rc::clone(&self.model);
        let Some(row) = model.tree(row_ix) else {
            return div().into_any_element();
        };
        let (active_border, radius) = {
            let t = cx.theme();
            (t.table_active_border, t.radius_tokens().sm)
        };
        let base = div()
            .size_full()
            .flex()
            .items_center()
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            .debug_selector(|| format!("pricer-cell-{row_ix}-{col_ix}"));
        let tinted = self
            .selected
            .as_ref()
            .is_some_and(|s| match Self::plan_col(col_ix) {
                Some(c) => s.contains(row_ix, c),
                None => s.kind == SelectKind::Rows && s.contains_row(row_ix),
            });
        let base = base.when(tinted, |el| el.relative().child(selection_tint(cx.theme())));
        let Some(plan_col) = Self::plan_col(col_ix) else {
            // The tree column: the lane indent, then the fixed slot (a
            // package's chevron, a leg's connector, empty on a bare line),
            // then a package's chip, the row's text and a package's leg
            // count, one `TREE_GAP` between each (`tree_gaps`).
            let slot = div()
                .w(scale::design(CHEVRON_SLOT))
                .h_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center();
            let depth = lane_depth(row.kind, row.depth);
            let el = base
                .pl(scale::design(depth as f32 * INDENT))
                .gap(scale::design(TREE_GAP));
            let group = matches!(row.kind, GridRowKind::Group { .. });
            let (slot, text_paint) = match row.kind {
                GridRowKind::Package { open, .. } | GridRowKind::Group { open, .. } => {
                    let states = self.chevron_states(cx.theme(), group);
                    let rest = if group {
                        paints.group.muted
                    } else {
                        paints.muted
                    };
                    let chevron = div()
                        .id(("pricer-chevron", row_ix))
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(radius)
                        .text_color(rest)
                        .pointer_states(states)
                        .debug_selector(|| format!("pricer-chevron-{row_ix}"))
                        // Recorded, not stopped: the tree cell reports
                        // it as a plain press (it clears a selection
                        // and never starts one) and the shell's
                        // tile-level press still arrives.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, _| {
                                this.delegate_mut().inner_press = Some(InnerPress::Chevron);
                            }),
                        )
                        .on_click(cx.listener(move |this, e: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            // Toggle only on the first press of a double-click.
                            if e.click_count() > 1 {
                                return;
                            }
                            this.set_selected_row(row_ix, cx);
                            cx.emit(ChevronClicked(row_ix));
                        }))
                        .child(if open { "▾" } else { "▸" });
                    (slot.child(chevron), self.tree_text_paint(row_ix))
                }
                GridRowKind::Leg { last } => (
                    slot.relative()
                        .child(connector_line(row_ix, last, paints.connector))
                        .child(connector_stub(row_ix, paints.connector)),
                    self.tree_text_paint(row_ix),
                ),
                GridRowKind::Line => (slot, self.tree_text_paint(row_ix)),
            };
            let chip = (!row.tag.is_empty()).then(|| {
                div()
                    .flex_shrink_0()
                    .px(scale::design(CHIP_PAD_X))
                    .rounded(radius)
                    .bg(paints.chip_fill)
                    .text_color(paints.chip_text)
                    .debug_selector(|| format!("pricer-chip-{row_ix}"))
                    .child(row.tag.clone())
            });
            // The note stays whole when the summary ellipsizes.
            let note = (!row.note.is_empty()).then(|| {
                div()
                    .flex_shrink_0()
                    .text_color(paints.muted)
                    .debug_selector(|| format!("pricer-note-{row_ix}"))
                    .child(row.note.clone())
            });
            return Self::wire_pointer(el, cx, row_ix, None)
                .child(slot)
                .children(chip)
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(text_paint)
                        .when(group, |el| el.font_weight(FontWeight::MEDIUM))
                        .debug_selector(move || {
                            if group {
                                format!("pricer-group-{row_ix}")
                            } else {
                                format!("pricer-tree-text-{row_ix}")
                            }
                        })
                        .child(row.text.clone()),
                )
                .children(note)
                .into_any_element();
        };
        let at_cursor = self.cursor == Some((row_ix, plan_col));
        let right = model.columns.get(plan_col).is_some_and(|c| c.right);
        let el = Self::wire_pointer(base, cx, row_ix, Some(plan_col))
            .when(right, |el| el.justify_end())
            .when(at_cursor, |el| el.border_1().border_color(active_border));
        // The editor replaces this cell's text. Its typeahead anchors its top-left
        // corner to a zero-size absolute child at the cell's bottom-left, so the
        // popup follows the edited cell when the table scrolls.
        let editing = self
            .editor
            .as_ref()
            .filter(|e| e.row == row_ix && e.col == plan_col)
            .cloned();
        match editing {
            Some(e) => {
                let popup = e.choice.as_ref().and_then(|paint| {
                    let tile = self.tile.upgrade()?;
                    Some(render_choice(paint, &tile, cx).into_any_element())
                });
                let align = if right {
                    TextAlign::Right
                } else {
                    TextAlign::Left
                };
                let field = match &e.field {
                    EditorField::Text(input) => Some(cell_input(input, align).into_any_element()),
                    // The field's keys and clicks route through the tile;
                    // a dropped tile paints an empty cell.
                    EditorField::Date { paint, focus } => self.tile.upgrade().map(|tile| {
                        let text = self.date_text_paints(row_ix);
                        render_date_field(paint, focus, &paints, text, cx.theme(), &tile)
                            .into_any_element()
                    }),
                };
                el.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .when(right, |el| el.justify_end())
                        .debug_selector(|| format!("pricer-editor-{row_ix}-{col_ix}"))
                        .children(field),
                )
                .when_some(popup, |el, popup| {
                    el.relative()
                        .child(div().absolute().left_0().bottom_0().child(popup))
                })
                .into_any_element()
            }
            // Ellipsize left-aligned text; the footer retains full failure reasons.
            // Numeric cells keep their digits and rely on column width rather than
            // ellipsis.
            None => {
                let colour = self.text_colour(row_ix, plan_col, cx.theme());
                // A window miss paints a blank cell.
                let text = self.window.get(row_ix, plan_col).map(|c| c.text.clone());
                el.when_some(text, |el, text| {
                    el.text_color(colour).map(|el| {
                        if right {
                            el.child(text)
                        } else {
                            el.child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(text),
                            )
                        }
                    })
                })
                .into_any_element()
            }
        }
    }
}

impl SheetDelegate {
    /// Wire a cell's, the tree cell's or the gutter's selection gestures
    /// onto `el`: a press (plain or shift) and, only while the button has
    /// stayed down since a press this table caught, a drag. `col` is the
    /// plan column (`None` for the tree cell or the gutter); a drag
    /// carries the `tree` flag of the element its PRESS landed on, so the
    /// selection's kind is decided by where it started, not by what is
    /// under the pointer now.
    ///
    /// A press in the cell holding the open editor belongs to the editor
    /// (caret placement, text selection), so it reports nothing and arms
    /// no drag: it must never cancel the edit or start a selection. A
    /// chevron's press is reported as a plain press whatever its
    /// modifiers, and arms no drag: the chevron toggles its package and
    /// never starts a selection.
    ///
    /// None of the listeners stops propagation: the table's own
    /// `SelectCell` click and the shell's tile-focus press must still
    /// arrive, and a fast double-click still reaches gpui's click-count
    /// tracking and so `DoubleClickedCell`.
    fn wire_pointer(
        el: Div,
        cx: &Context<TableState<Self>>,
        row_ix: usize,
        col: Option<usize>,
    ) -> Div {
        let tree = col.is_none();
        el.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                let d = this.delegate_mut();
                if d.inner_press == Some(InnerPress::Chevron) {
                    cx.emit(CellPointer::Press {
                        row: row_ix,
                        col,
                        shift: false,
                    });
                    return;
                }
                if col.is_some()
                    && d.editor
                        .as_ref()
                        .is_some_and(|ed| ed.row == row_ix && Some(ed.col) == col)
                {
                    d.inner_press = Some(InnerPress::Editor);
                    return;
                }
                d.drag_last = Some((row_ix, col));
                d.drag_origin = Some(tree);
                cx.emit(CellPointer::Press {
                    row: row_ix,
                    col,
                    shift: e.modifiers.shift,
                });
            }),
        )
        .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
            if e.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let d = this.delegate_mut();
            // No press recorded: the button came down on something else
            // and is only passing over this cell.
            let Some(started_on_tree) = d.drag_origin else {
                return;
            };
            if d.drag_last == Some((row_ix, col)) {
                return;
            }
            d.drag_last = Some((row_ix, col));
            cx.emit(CellPointer::Drag {
                row: row_ix,
                col,
                tree: started_on_tree,
            });
        }))
        // A release anywhere ends the drag: `on_mouse_up` when it lands
        // here, `on_mouse_up_out` everywhere else.
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| {
                let d = this.delegate_mut();
                d.drag_origin = None;
                d.inner_press = None;
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| {
                let d = this.delegate_mut();
                d.drag_origin = None;
                d.inner_press = None;
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::number_rows;
    use geode_shell::linenumbers::LineNumbers;

    /// A, P, L1, L2, B: an expanded package's legs are painted rows and
    /// take numbers like any other.
    #[test]
    fn on_numbers_every_painted_row_including_expanded_legs() {
        let n = number_rows(LineNumbers::On, 5, Some(3));
        assert_eq!(n, [1, 2, 3, 4, 5].map(Some));
    }

    #[test]
    fn rel_measures_from_the_cursor_which_shows_its_own_number() {
        let n = number_rows(LineNumbers::Relative, 5, Some(2));
        assert_eq!(n, [2, 1, 3, 1, 2].map(Some));
        assert_eq!(
            number_rows(LineNumbers::Relative, 3, None),
            [1, 2, 3].map(Some),
            "no cursor row: absolute"
        );
    }

    /// The gaps between the tree cell's painted parts: the slot always,
    /// then a chip, the text always, then a note.
    #[test]
    fn the_tree_cell_counts_a_gap_between_each_pair_of_parts() {
        assert_eq!(super::tree_gaps(false, false), 1, "slot, text");
        assert_eq!(super::tree_gaps(true, false), 2);
        assert_eq!(super::tree_gaps(true, true), 3, "slot, chip, text, note");
    }

    #[test]
    fn off_numbers_nothing() {
        assert!(
            number_rows(LineNumbers::Off, 4, Some(0))
                .iter()
                .all(Option::is_none)
        );
    }
}

#[cfg(test)]
mod width_tests {
    use crate::core::columns::{COLUMNS, ColumnDef, ColumnKind, signed};
    use geode_core::format::format_number;
    use geode_shell::fontsize::FontSize;
    use gpui_component::Size;

    /// JetBrains Mono (`fonts::MONO`) advances every glyph 600/1000 em.
    const MONO_ADVANCE_EM: f32 = 0.6;
    /// gpui-component's table paints its text at `text_sm`.
    const TABLE_TEXT_REM: f32 = 0.875;
    /// `border_1` on the cursor cell, both sides.
    const CURSOR_BORDER: f32 = 2.0;

    /// Representative width-test values formatted like cells. A right-aligned number
    /// that exceeds its width can lose leading characters, including its sign, so the
    /// fixtures exercise large magnitudes as well as ordinary labels.
    fn worst_case(def: &ColumnDef) -> String {
        let fmt = |v: f64| format_number(v, &def.default_format).text;
        match def.kind {
            ColumnKind::Qty => fmt(-10000.0),
            ColumnKind::Strike | ColumnKind::Barrier => fmt(12345.67),
            ColumnKind::SpotShift | ColumnKind::VolShift => signed(-99.9, &def.default_format),
            ColumnKind::Measure { .. } => fmt(-1_234_567.89),
            // Representative text values for the width check.
            ColumnKind::SheetName => "untitled-1".into(),
            // Ids are minted per sheet from 1; five digits is a long session.
            ColumnKind::PositionRef => "p10000".into(),
            ColumnKind::InstrumentRef => "i10000".into(),
            // A template name is at most MAX_TEMPLATE_NAME (8) characters.
            ColumnKind::Template => "STRANGLE".into(),
            ColumnKind::Currency => "USD".into(),
            ColumnKind::UnderlyingRef => "SX5E".into(),
            // The cell reads `20DEC26`, but `i` edits it in the date field,
            // which paints `YYYY-MM-DD` inside the same width.
            ColumnKind::Expiry => "2026-12-20".into(),
            ColumnKind::OptionType => "C".into(),
            ColumnKind::BarrierType => "DO".into(),
            ColumnKind::PricedAt => "23:59:59".into(),
            // Prose: a failure's reason may be longer than any width; it
            // ends in `…` and is read whole in the footer.
            ColumnKind::Status => "pricing…".into(),
        }
    }

    /// Check default labels and representative values at the largest font size. Widths
    /// are fixed pixels, so account for monospace advance, XSmall cell padding, and
    /// both cursor borders. This is a sizing check, not a numeric bound.
    #[test]
    fn every_default_label_and_worst_case_value_fits_its_width() {
        let advance = FontSize::Large.rem_px() * TABLE_TEXT_REM * MONO_ADVANCE_EM;
        let pad = Size::XSmall.table_cell_padding();
        let padding = f32::from(pad.left) + f32::from(pad.right) + CURSOR_BORDER;
        let mut failures = Vec::new();
        for c in &COLUMNS {
            for text in [c.label.to_string(), worst_case(c)] {
                let need = text.chars().count() as f32 * advance + padding;
                if need > c.default_width {
                    failures.push(format!(
                        "{}: '{text}' needs {need:.1}px in {}px",
                        c.name, c.default_width
                    ));
                }
            }
            assert!(
                !c.label.contains('_'),
                "{}: a label is words, not the config name",
                c.name
            );
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Column 0 holds a two-leg package row, `▾ CS Z26 4800/5200 · 2
    /// legs` (a call spread's real summary), at the largest font size: the chevron slot, the chip's padding,
    /// the gaps between the four parts and the three texts. Slot, padding
    /// and gaps are on the rem scale; the column is fixed pixels. `·`
    /// is one glyph.
    #[test]
    fn the_tree_column_fits_a_two_leg_package_row() {
        use super::{CHEVRON_SLOT, CHIP_PAD_X, TREE_GAP, TREE_WIDTH, tree_gaps};
        let rem = FontSize::Large.rem_px();
        let advance = rem * TABLE_TEXT_REM * MONO_ADVANCE_EM;
        let pad = Size::XSmall.table_cell_padding();
        let padding = f32::from(pad.left) + f32::from(pad.right);
        let design = |x: f32| x * rem / geode_shell::shell::scale::DESIGN_REM;
        let (tag, text, note) = ("CS", "Z26 4800/5200", "· 2 legs");
        let gaps = tree_gaps(true, true);
        assert_eq!(gaps, 3, "slot, chip, summary and note: three gaps");
        let need = design(CHEVRON_SLOT + 2.0 * CHIP_PAD_X + gaps as f32 * TREE_GAP)
            + (tag.chars().count() + text.chars().count() + note.chars().count()) as f32 * advance
            + padding;
        assert!(need <= TREE_WIDTH, "needs {need:.1}px in {TREE_WIDTH}px");
        assert!(
            TREE_WIDTH - need < 16.0,
            "{TREE_WIDTH}px wastes {:.1}px",
            TREE_WIDTH - need
        );
    }
}
