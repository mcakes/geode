//! TableDelegate over the panel's prepared grid. The tile owns authoritative
//! cursor, editor, and choice state; this delegate holds synchronized paint-time
//! mirrors and shares the model through Rc. Rendering does not decide edits,
//! yanks, or navigation.
//!
//! Panels with shown row labels insert a fixed label column at table index zero,
//! shifting model columns by one. Hidden-label panels have no offset and pin the
//! first value column instead. The cursor always uses model coordinates; it
//! never enters the separate label column.

use crate::core::matrix::RowState;
use crate::core::{MatrixModel, PanelSpec};
use crate::header;
use crate::popup::{ChoicePaint, render_choice};
use crate::tile::{DateFieldPaint, FlooredTones, MarketDataTile};
use geode_core::grid::selection::{Resolved, SelectKind};
use geode_shell::colfit::{FitMetrics, FittedWidths};
use geode_shell::fonts;
use geode_shell::linenumbers::{GUTTER_GAP_PX, LineNumbers, gutter_number, gutter_px};
use gpui::prelude::*;
use gpui::{
    App, Context, Div, Entity, EventEmitter, FocusHandle, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, SharedString, Stateful, TextAlign, WeakEntity, Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::table::{Column, ColumnFixed, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Theme};
use std::rc::Rc;

/// Default pixel widths for the label and value columns. Columns cannot be
/// dragged: a dragged width has nowhere to live and table refresh would
/// replace it on the next model install. `:autosize` replaces them with
/// fitted widths (`MatrixDelegate::fitted`), which do persist. These widths
/// are not rem-scaled because column() has no Window from which to read the
/// rem size.
const LABEL_WIDTH: f32 = 128.0;
const CELL_WIDTH: f32 = 84.0;

/// The row-label column's stable key, in `column()` and in fitted widths.
pub(crate) const ROW_AXIS_KEY: &str = "__row_axis";

/// Index of the fixed row-label column when labels are shown. Hidden-label
/// panels instead pin their first value column; offset() owns that distinction.
pub(crate) const LABEL_COL: usize = 0;

/// Pinned table column: the row label when shown, otherwise the first value.
/// The line-number gutter widens this column so it stays visible during
/// horizontal scrolling without reducing the data cell's width.
const PINNED_COL: usize = 0;

/// Paint-time editor mirror. Row and optional column use model coordinates;
/// None identifies a typed row-label editor. Text inputs retain their entity,
/// while date editors share prepared segments and the tile-owned focus handle.
/// The tile synchronizes this mirror after editing state changes.
#[derive(Clone)]
pub(crate) struct DelegateEditor {
    pub row: usize,
    pub col: Option<usize>,
    pub paint: DelegateEditorPaint,
}

/// The two forms an editor paints in a cell — the tile's `EditorState`,
/// with the date form reduced to what `render_td` needs: the pure
/// `DateTimeField` itself stays on the tile, since only the tile's key
/// path ever steps it.
#[derive(Clone)]
pub(crate) enum DelegateEditorPaint {
    Text(Entity<InputState>),
    Date {
        paint: DateFieldPaint,
        focus: FocusHandle,
    },
}

/// Choice-popup paint and its target cell in model coordinates. The tile
/// owns Popup::Choice and refreshes this mirror through sync_editor; the delegate
/// renders its shared prepared rows beneath the cell without reading tile state.
#[derive(Clone)]
pub(crate) struct DelegateChoice {
    pub row: usize,
    pub col: usize,
    pub paint: Rc<ChoicePaint>,
}

/// Every mouse selection gesture a cell, a row label or the line-number
/// gutter recognises, carried to the tile's `pointer`, the one door a
/// shift+click and a drag go through to `start_selection` and
/// `clear_selection`, so the mouse never reaches a selection state the
/// keys could not. `col` is a MODEL column; `None` is the row-label
/// column (or the gutter), which is never a selection member. A `Drag`'s
/// `label` is where its press landed, not where the pointer is now.
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
        label: bool,
    },
}

impl EventEmitter<CellPointer> for TableState<MatrixDelegate> {}

/// Crate-private state keeps external callers from replacing the model
/// without the paired TableState::refresh. Structural model installs go through
/// MarketDataTile::install_model so cached columns and headers stay synchronized.
pub struct MatrixDelegate {
    /// Prepared grid shared with the tile. Rebuilds replace it; ordinary cell
    /// commits can patch it after the tile temporarily removes the delegate's share.
    pub(crate) model: Rc<MatrixModel>,
    /// The row axis's name (`term`), painted as column 0's header while
    /// `label_column` holds.
    row_axis: SharedString,
    /// Whether table column [`LABEL_COL`] is the row-label column
    /// (`spec.rows.label == Shown`). When false, every table column is a
    /// model column and the label is never painted — the row's identity
    /// still lives on `RowModel::label` for the draft and the session.
    label_column: bool,
    /// The tile's cursor, mirrored. `Some((model row, model column))` —
    /// NOT a table column index — while the cursor is on a grid cell;
    /// `None` while it is in the header strip (`Cursor::Attr`), which
    /// paints no cursor cell here at all (`MarketDataTile::sync_cursor`
    /// clears the table's own selection for that case).
    pub(crate) cursor: Option<(usize, usize)>,
    /// The tile's resolved selection, mirrored by `sync_cursor`;
    /// `render_cell` only looks it up.
    pub(crate) selected: Option<Resolved>,
    /// The open cell editor, mirrored from the tile: the cell it was
    /// opened on (again in model coordinates) and what to paint there.
    /// Painted IN that cell, which is what makes it typeable at all
    /// (`MarketDataTile`'s `Editing::state`).
    pub(crate) editor: Option<DelegateEditor>,
    /// The open choice popup, mirrored from the tile: painted hanging
    /// under its cell (`render_td`'s value arm), the one place the popup
    /// can know where that cell is.
    pub(crate) choice: Option<DelegateChoice>,
    /// The tile, for the date field's key and click routing
    /// (`header::render_date_field` takes the entity, and the field's
    /// `on_key_down` runs `MarketDataTile::date_field_key` through it).
    /// Weak: the table is owned by the tile, and a strong handle here
    /// would be a cycle neither side could ever drop.
    pub(crate) tile: WeakEntity<MarketDataTile>,
    /// The tile's id, for the field's debug selectors — the same
    /// `marketdata-date-{id}` the strip paints, so a test finds the field
    /// wherever it is painted.
    pub(crate) tile_id: u64,
    /// The floored tones the date field paints its active segment through
    /// (`FlooredTones::primary_text`). The delegate's own copy, refreshed
    /// at the one paint that reads it — `render_td`'s date arm, one cell
    /// per frame at most — exactly as the tile refreshes its own at the
    /// top of `render`: a compare per painted editor, a derivation only
    /// when the theme moved.
    pub(crate) tones: FlooredTones,
    /// `[ui] line_numbers`, mirrored from the `UiSettings` global by the
    /// tile (`MarketDataTile::on_ui_settings`), which refreshes the table
    /// on a change: the pinned column's width includes the gutter.
    pub(crate) line_numbers: LineNumbers,
    /// Cached gutter text per painted row. `ensure_numbers` refreshes it during
    /// rendering when the stamp changes; subsequent cells clone the prepared text.
    numbers: Vec<SharedString>,
    /// Cache key: row count, mode, and cursor row for relative numbering.
    /// Absolute numbering ignores cursor movement.
    numbers_stamp: Option<(usize, usize, LineNumbers)>,
    /// The `(row, col)` a mouse move last emitted a `CellPointer::Drag`
    /// for. gpui fires a move per pixel, not per cell; without this a held
    /// drag would re-run the tile's `pointer` on every frame. Stale after
    /// a drag no cell saw released, which costs one extra emission at most.
    drag_last: Option<(usize, Option<usize>)>,
    /// `Some` while the primary button is down because of a press a cell,
    /// label or gutter of this table caught — `true` when it was a label
    /// or the gutter. `None` while another element owns the drag (a
    /// scrollbar, the header, a tile divider, another tile's text
    /// selection), so a button held over the cells from elsewhere never
    /// starts or extends a selection. Cleared by any release: gpui runs
    /// every registered mouse listener for every event and each one's own
    /// hit test decides, so a cell's `on_mouse_up`/`on_mouse_up_out` pair
    /// sees every release exactly once wherever it lands.
    drag_origin: Option<bool>,
    /// Set by a press the open editor's own cell swallowed (see
    /// `wire_pointer`), and taken by the row's press handler, which
    /// bubbles after the cell's: without it the row would report that
    /// press at the cursor column, which is the editor's cell, and so
    /// cancel the edit the press was aimed into.
    editor_press: bool,
    /// Widths `:autosize` fitted, keyed by column key ([`ROW_AXIS_KEY`] for
    /// the row labels, the column's label for a value column), in pixels
    /// without the gutter. `column()` prefers an entry over the default, so
    /// every model install's refresh keeps it; a key the current model no
    /// longer has is ignored and a new column gets the default.
    pub(crate) fitted: FittedWidths,
}

impl MatrixDelegate {
    pub(crate) fn new(
        spec: &PanelSpec,
        tile: WeakEntity<MarketDataTile>,
        tile_id: u64,
        tones: FlooredTones,
    ) -> MatrixDelegate {
        MatrixDelegate {
            model: Rc::new(MatrixModel::default()),
            row_axis: SharedString::from(spec.rows.column.clone()),
            label_column: spec.rows.shown(),
            cursor: Some((0, 0)),
            selected: None,
            editor: None,
            choice: None,
            tile,
            tile_id,
            tones,
            line_numbers: LineNumbers::Off,
            numbers: Vec::new(),
            numbers_stamp: None,
            drag_last: None,
            drag_origin: None,
            editor_press: false,
            fitted: FittedWidths::new(),
        }
    }

    /// Fit the row-label column (when shown) and every value column to its
    /// header and every row's prepared text. The whole model is measured:
    /// a document grid is small and already formatted.
    ///
    /// `None` with no rows to measure (no document yet, or an empty one).
    pub(crate) fn fit_columns(&self, m: &FitMetrics) -> Option<FittedWidths> {
        if self.model.rows.is_empty() {
            return None;
        }
        let mut out = FittedWidths::new();
        if self.label_column {
            out.insert(
                ROW_AXIS_KEY.to_string(),
                m.fit_text(
                    &self.row_axis,
                    self.model.rows.iter().map(|r| r.label.as_ref()),
                ),
            );
        }
        for (col, name) in self.model.columns.iter().enumerate() {
            let cells = self
                .model
                .rows
                .iter()
                .filter_map(|r| r.cells.get(col).map(|c| c.text.as_ref()));
            out.insert(name.to_string(), m.fit_text(name, cells));
        }
        Some(out)
    }

    /// The fitted width for `key`, else `default`.
    fn width_of(&self, key: &str, default: f32) -> f32 {
        self.fitted.get(key).copied().unwrap_or(default)
    }

    /// The gutter's width in px — `0` when off. Read by `column` (the
    /// pinned column widens by it, so its own text keeps its room) and by
    /// `render_td` (the gutter's own width).
    pub(crate) fn gutter_px(&self) -> f32 {
        gutter_px(self.line_numbers, self.model.rows.len())
    }

    /// Rebuild numbers when the mode, row count, or relative cursor changes.
    /// Count all painted rows, including inserts and marked deletions. Relative
    /// mode shows distances except on the cursor row, which shows its absolute
    /// number. With the cursor in the attribute strip, use absolute numbers.
    fn ensure_numbers(&mut self) {
        let mode = self.line_numbers;
        let len = self.model.rows.len();
        let cursor = match (mode, self.cursor) {
            (LineNumbers::Relative, Some((row, _))) => row,
            _ => usize::MAX,
        };
        let stamp = (len, cursor, mode);
        if self.numbers_stamp == Some(stamp) {
            return;
        }
        let mode = match (mode, self.cursor) {
            (LineNumbers::Relative, None) => LineNumbers::On,
            _ => mode,
        };
        self.numbers.clear();
        self.numbers.extend((0..len).map(|row| {
            gutter_number(mode, row, cursor)
                .map(|n| SharedString::from(n.to_string()))
                .unwrap_or_default()
        }));
        self.numbers_stamp = Some(stamp);
    }

    /// The gutter text for `row` — `None` when the gutter is off.
    /// Test-only: production code goes through `render_td`.
    #[cfg(test)]
    pub(crate) fn gutter_text(&mut self, row: usize) -> Option<SharedString> {
        if self.line_numbers == LineNumbers::Off {
            return None;
        }
        self.ensure_numbers();
        self.numbers.get(row).cloned()
    }

    /// The editor to paint in table cell (`row_ix`, `col_ix`), if the
    /// open one sits there — `col: None` is the row-label column (table
    /// column [`LABEL_COL`]), a model column is every other.
    fn editor_at(&self, row_ix: usize, col_ix: usize) -> Option<&DelegateEditor> {
        self.editor
            .as_ref()
            .filter(|e| e.row == row_ix && e.col == self.model_col(col_ix))
    }

    /// Render a text input or shared date field aligned with its cell's text.
    /// Both omit their own frame so the cell's cursor border and draft fill stay
    /// visible. Text inputs remove horizontal padding; grid date fields use flush
    /// segments. A date field needs a live tile for event routing; otherwise the
    /// caller falls back to the cell's prepared text.
    fn render_editor(
        &mut self,
        editor: &DelegateEditor,
        align: TextAlign,
        theme: &Theme,
    ) -> Option<gpui::AnyElement> {
        match &editor.paint {
            DelegateEditorPaint::Text(state) => Some(
                Input::new(state)
                    .appearance(false)
                    .px_0()
                    .text_align(align)
                    .into_any_element(),
            ),
            DelegateEditorPaint::Date { paint, focus } => {
                let tile = self.tile.upgrade()?;
                self.tones.refresh(theme);
                Some(
                    div()
                        .flex()
                        .when(matches!(align, TextAlign::Right), |el| el.justify_end())
                        .child(header::render_date_field(
                            paint,
                            focus,
                            theme,
                            &self.tones,
                            &tile,
                            self.tile_id,
                        ))
                        .into_any_element(),
                )
            }
        }
    }

    /// How many table columns sit ahead of the first model column: one
    /// (the row-label column) or none.
    fn offset(&self) -> usize {
        if self.label_column { LABEL_COL + 1 } else { 0 }
    }

    /// The model column a table column carries, or `None` for the
    /// row-label column — which is every caller's cue that the cursor has
    /// no business there. Never `None` under a hidden label.
    pub fn model_col(&self, table_col: usize) -> Option<usize> {
        table_col.checked_sub(self.offset())
    }

    /// Whether table column `col_ix` is the LAST slice-value column — the
    /// one whose right edge closes the slice block ahead of the ladder.
    /// Never true for a model with no slice columns.
    pub fn closes_slice_block(&self, col_ix: usize) -> bool {
        self.model.slice_columns > 0 && self.model_col(col_ix) == Some(self.model.slice_columns - 1)
    }

    /// The table column a model column is painted in.
    pub fn table_col(&self, model_col: usize) -> usize {
        model_col + self.offset()
    }

    /// Wire a cell's, a row label's or the gutter's selection gestures
    /// onto `el`: a press (plain or shift) and, only while the button has
    /// stayed down since a press this table caught, a drag. `col` is the
    /// model column (`None` for a label or the gutter); a drag carries the
    /// `label` flag of the element its PRESS landed on, so the selection's
    /// kind is decided by where it started, not by what is under the
    /// pointer now.
    ///
    /// A press in the cell holding the open editor (`holds_editor`: a
    /// value cell or row label, never the gutter) belongs to the editor —
    /// caret placement, text selection, a date separator — so it reports
    /// nothing and arms no drag: it must never cancel the edit or start a
    /// selection.
    ///
    /// None of the four listeners stops propagation: the table's own
    /// `SelectCell` click and the shell's tile-focus press must still
    /// arrive, and a fast double-click still reaches gpui's click-count
    /// tracking and so `DoubleClickedCell`.
    fn wire_pointer(
        el: Div,
        cx: &Context<TableState<Self>>,
        row_ix: usize,
        col: Option<usize>,
        holds_editor: bool,
    ) -> Div {
        let label = col.is_none();
        el.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                let d = this.delegate_mut();
                if holds_editor
                    && d.editor
                        .as_ref()
                        .is_some_and(|ed| ed.row == row_ix && ed.col == col)
                {
                    d.editor_press = true;
                    return;
                }
                d.drag_last = Some((row_ix, col));
                // `get_or_insert` so a wired element nested in another
                // wired one keeps the innermost press's kind; siblings
                // (the gutter beside its cell) never both see one press.
                d.drag_origin.get_or_insert(label);
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
            let Some(started_on_label) = d.drag_origin else {
                return;
            };
            if d.drag_last == Some((row_ix, col)) {
                return;
            }
            d.drag_last = Some((row_ix, col));
            cx.emit(CellPointer::Drag {
                row: row_ix,
                col,
                label: started_on_label,
            });
        }))
        // A release anywhere ends the drag: `on_mouse_up` when it lands
        // here, `on_mouse_up_out` everywhere else.
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| {
                let d = this.delegate_mut();
                d.drag_origin = None;
                d.editor_press = false;
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| {
                let d = this.delegate_mut();
                d.drag_origin = None;
                d.editor_press = false;
            }),
        )
    }
}

impl TableDelegate for MatrixDelegate {
    /// The row-label column (when shown) plus one per value column. A
    /// shown label keeps an empty panel painting its row axis's name; a
    /// hidden one paints nothing until a document arrives.
    fn columns_count(&self, _cx: &App) -> usize {
        self.offset() + self.model.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.model.rows.len()
    }

    /// Supply column metadata used by table preparation and refresh. Structural
    /// model replacements must refresh the table's cached headers and column groups.
    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(model_col) = self.model_col(col_ix) else {
            return Column {
                key: SharedString::from(ROW_AXIS_KEY),
                name: self.row_axis.clone(),
                align: TextAlign::Left,
                // Preserve document row order and column identity: no sorting or movement.
                sort: None,
                width: px(self.width_of(ROW_AXIS_KEY, LABEL_WIDTH) + self.gutter_px()),
                fixed: Some(ColumnFixed::Left),
                movable: false,
                // See `LABEL_WIDTH`'s own note: a dragged width has
                // nowhere to live and `refresh` would undo it.
                resizable: false,
                ..Column::default()
            };
        };
        let name = self
            .model
            .columns
            .get(model_col)
            .cloned()
            .unwrap_or_default();
        let width = self.width_of(&name, CELL_WIDTH);
        Column {
            key: name.clone(),
            name,
            // Right-align value-column headers. Value cells use the same alignment,
            // including typed text/date/choice columns in flat panels.
            align: TextAlign::Right,
            sort: None,
            width: px(width
                + if col_ix == PINNED_COL {
                    self.gutter_px()
                } else {
                    0.0
                }),
            // Under a hidden label the FIRST value column is what keeps
            // a row identifiable under horizontal scroll, so it takes
            // the pin the label column would have had.
            fixed: (!self.label_column && model_col == 0).then_some(ColumnFixed::Left),
            movable: false,
            resizable: false,
            ..Column::default()
        }
    }

    /// The column's own label, aligned as its cells are — a node label
    /// must sit over its numbers. Everything around it (padding, borders,
    /// the resize handle) stays the component's own, which is what
    /// overriding here rather than replacing `render_header` keeps.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .items_center()
            .when(matches!(column.align, TextAlign::Right), |el| {
                el.justify_end()
            })
            .when(self.closes_slice_block(col_ix), |el| {
                el.border_r_1().border_color(theme.border)
            })
            .font_family(fonts::MONO)
            .debug_selector(|| format!("marketdata-th-{col_ix}"))
            .child(column.name)
    }

    /// A press on the row outside every cell (the table's trailing filler)
    /// is still a click on that row: it reports a press at the cursor's
    /// column so the tile's one pointer door clears or extends exactly as
    /// a cell press would. The row bubbles after its cells, so a press a
    /// cell already caught has set `drag_origin` (or, for the open
    /// editor's cell, `editor_press`) and is not reported twice; this
    /// press arms no drag. The filler rows past the model and
    /// a cursor in the header strip report nothing.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let row = div().id(("row", row_ix));
        if row_ix >= self.model.rows.len() {
            return row;
        }
        row.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                let d = this.delegate_mut();
                // `|` not `||`: the editor's flag is taken on every row
                // press, so it never outlives the press that set it.
                if std::mem::take(&mut d.editor_press) | d.drag_origin.is_some() {
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

    /// Paint a data cell with an optional gutter beside the pinned column.
    /// The gutter is outside the cell element so cursor borders, draft fills,
    /// and deletion strikes apply only to data.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = self.render_cell(row_ix, col_ix, cx);
        if col_ix != PINNED_COL || self.line_numbers == LineNumbers::Off {
            return cell;
        }
        self.ensure_numbers();
        let text = self.numbers.get(row_ix).cloned().unwrap_or_default();
        let theme = cx.theme();
        let on_cursor_row = self.cursor.is_some_and(|(row, _)| row == row_ix);
        // The gutter is the row's handle whatever column it rides in: a
        // press there means "rows", like a row label's (under a hidden
        // label it sits beside the first VALUE cell, whose own press
        // still means "block").
        let gutter = Self::wire_pointer(div(), cx, row_ix, None, false);
        div()
            .size_full()
            .flex()
            .child(
                gutter
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_end()
                    .w(px(self.gutter_px()))
                    .pr(px(GUTTER_GAP_PX))
                    .font_family(fonts::MONO)
                    .text_color(if on_cursor_row {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .debug_selector(|| format!("marketdata-gutter-{row_ix}"))
                    .child(text),
            )
            .child(cell.flex_1().min_w_0())
    }
}

/// The selection tint: an absolute overlay painted as a cell's first
/// child, so it sits under the text and never replaces an edited or sent
/// cell's own fill, and the cursor's border still paints over it.
fn selection_tint(theme: &Theme) -> Div {
    div().absolute().inset_0().bg(theme.selection.opacity(0.35))
}

impl MatrixDelegate {
    /// Paint a prepared cell or its active editor. Normal cell text is cloned
    /// from SharedString; date and choice editors share prepared paint. Debug
    /// selectors identify coordinates without formatting cell values.
    fn render_cell(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> Div {
        let theme = cx.theme();
        let Some(model_col) = self.model_col(col_ix) else {
            // Paint the row label in its row state, including insertion tint and
            // deletion strike-through. Typed row-label editors use the same renderer as
            // value-cell editors.
            let (label, state, sent) = self
                .model
                .rows
                .get(row_ix)
                .map(|r| {
                    (
                        r.label.clone(),
                        r.state,
                        // Every cell of an inserted row shares the
                        // draft's `sent`; a document row's cells vary,
                        // and its label carries no fill of its own.
                        r.state == RowState::Inserted && r.cells.first().is_some_and(|c| c.sent),
                    )
                })
                .unwrap_or((SharedString::default(), RowState::Document, false));
            let editor = self
                .editor_at(row_ix, col_ix)
                .cloned()
                .and_then(|e| self.render_editor(&e, TextAlign::Left, theme));
            let CellPaint { fill, text, strike } = cell_paint(theme, sent, false, state);
            // A `Rows` selection tints its labels too; a `Block` never
            // includes the label column.
            let in_selection = self
                .selected
                .as_ref()
                .is_some_and(|r| r.kind == SelectKind::Rows && r.contains_row(row_ix));
            let el = div()
                .size_full()
                .flex()
                .items_center()
                .font_family(fonts::MONO)
                .text_color(text)
                .when_some(fill, |el, fill| el.bg(fill))
                .when(strike, |el| el.line_through())
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .debug_selector(|| format!("marketdata-cell-{row_ix}-{col_ix}"))
                .relative()
                .when(in_selection, |el| el.child(selection_tint(theme)));
            let el = Self::wire_pointer(el, cx, row_ix, None, true);
            return match editor {
                Some(editor) => el.child(
                    div()
                        .flex_1()
                        .debug_selector(|| format!("marketdata-editor-{row_ix}-{col_ix}"))
                        .child(editor),
                ),
                None => el.child(label),
            };
        };
        let at_cursor = self.cursor == Some((row_ix, model_col));
        let in_selection = self
            .selected
            .as_ref()
            .is_some_and(|r| r.contains(row_ix, model_col));
        let row = self.model.rows.get(row_ix);
        let state = row.map_or(RowState::Document, |r| r.state);
        let cell = row.and_then(|r| r.cells.get(model_col));
        let mut el = div()
            .size_full()
            .flex()
            .items_center()
            .justify_end()
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            // Identify this table cell in debug/test builds without formatting its value.
            .debug_selector(|| format!("marketdata-cell-{row_ix}-{col_ix}"))
            // The slice values read as their own block: a right border on
            // the last of them, so the ladder starts visibly after the
            // term's forward/atm/skew. Applied before the cursor border,
            // which then wins on the cursor cell.
            .when(self.closes_slice_block(col_ix), |el| {
                el.border_r_1().border_color(theme.border)
            })
            // The cursor cell reads as the blotter's does: a border in the
            // table's own active-border colour, never a fill, so an edited
            // or sent cell's own background still shows through it.
            .when(at_cursor, |el| {
                el.border_1().border_color(theme.table_active_border)
            });
        let Some(cell) = cell else {
            return el;
        };
        let CellPaint { fill, text, strike } = cell_paint(theme, cell.sent, cell.edited, state);
        el = el
            .when_some(fill, |el, fill| el.bg(fill))
            .text_color(text)
            .when(strike, |el| el.line_through())
            .relative()
            .when(in_selection, |el| el.child(selection_tint(theme)));
        // The editor is cloned out (three refcounts at most) because
        // `render_editor` needs `&mut self` for the tones refresh while
        // `editor_at` borrows `self.editor`; the cell's own text is what
        // paints when there is no editor here, or no tile left to route
        // its keys to.
        let text = cell.text.clone();
        let editor = self
            .editor_at(row_ix, col_ix)
            .cloned()
            .and_then(|e| self.render_editor(&e, TextAlign::Right, theme));
        // Anchor the choice popup at its target cell's bottom-left. Deferred
        // painting escapes the table clip; a dropped tile cannot receive popup events.
        let choice = self
            .choice
            .as_ref()
            .filter(|c| c.row == row_ix && c.col == model_col)
            .map(|c| Rc::clone(&c.paint))
            .and_then(|paint| {
                let tile = self.tile.upgrade()?;
                Some(render_choice(&paint, &tile, self.tile_id, cx).into_any_element())
            });
        let el = Self::wire_pointer(el, cx, row_ix, Some(model_col), true);
        let el = match editor {
            Some(editor) => el.child(
                div()
                    .flex_1()
                    .debug_selector(|| format!("marketdata-editor-{row_ix}-{col_ix}"))
                    .child(editor),
            ),
            None => el.child(text),
        };
        el.when_some(choice, |el, popup| {
            el.relative()
                .child(div().absolute().left_0().bottom_0().child(popup))
        })
    }
}

/// How a value cell paints in one draft state: an optional fill, the
/// text colour over it, and whether the text is struck through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CellPaint {
    pub fill: Option<Hsla>,
    pub text: Hsla,
    pub strike: bool,
}

/// Shared cell-color rules for rendering and bundled-theme contrast tests.
/// Deleted rows override every flag: muted text, strike-through, no fill. Other
/// cells keep foreground text, with fill precedence sent, inserted, edited.
///
/// Sent uses muted; inserted uses an 18% success tint; edited uses a 25% warning
/// tint. Foreground text is tested over those actual backgrounds rather than
/// using text tokens intended for solid status fills.
pub(crate) fn cell_paint(theme: &Theme, sent: bool, edited: bool, state: RowState) -> CellPaint {
    if state == RowState::Deleted {
        return CellPaint {
            fill: None,
            text: theme.muted_foreground,
            strike: true,
        };
    }
    let fill = if sent {
        Some(theme.muted)
    } else if state == RowState::Inserted {
        Some(theme.success.opacity(0.18))
    } else if edited {
        Some(theme.warning.opacity(0.25))
    } else {
        None
    };
    CellPaint {
        fill,
        text: theme.foreground,
        strike: false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::test_fixtures::{CVI, DIVIDEND};
    use geode_core::colour::{READABLE_RATIO, Rgb, contrast_ratio};
    use geode_shell::shell::colours::to_rgb;

    /// `top` at its own alpha composited over an opaque `under`, in sRGB —
    /// what the GPU paints for a translucent fill over what is beneath it.
    pub(crate) fn over(top: Hsla, under: Rgb) -> Rgb {
        let (t, a) = (to_rgb(top), top.a);
        Rgb {
            r: t.r * a + under.r * (1.0 - a),
            g: t.g * a + under.g * (1.0 - a),
            b: t.b * a + under.b * (1.0 - a),
        }
    }

    /// The ground a cell fill lands on: the table body at ITS own alpha
    /// over the opaque window background — a theme may make the body
    /// transparent (Modus Operandi's `table.background = #00000000`), and
    /// reading it as opaque would test against black.
    pub(crate) fn ground(theme: &Theme) -> Rgb {
        over(theme.table, to_rgb(theme.background))
    }

    /// Check foreground contrast over edited, inserted, and sent fills on every
    /// bundled theme, composited over the actual table/window background.
    #[gpui::test]
    fn dirty_and_sent_cells_are_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                for (state, sent, edited, row) in [
                    ("edited", false, true, RowState::Document),
                    ("sent", true, false, RowState::Document),
                    ("inserted", false, true, RowState::Inserted),
                ] {
                    let paint = cell_paint(theme, sent, edited, row);
                    let fill = paint.fill.expect("every marked state carries a fill");
                    let ratio = contrast_ratio(to_rgb(paint.text), over(fill, ground(theme)));
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {state} text at {ratio:.2}:1"));
                    }
                }
                // Deleted rows use the theme's muted text on bare ground. Verify that
                // pairing and strike-through separately from the marked-fill contrast checks.
                let deleted = cell_paint(theme, false, false, RowState::Deleted);
                assert_eq!(deleted.fill, None, "{name}: a deleted row has no fill");
                assert_eq!(deleted.text, theme.muted_foreground, "{name}");
                assert!(deleted.strike, "{name}: a deleted row is struck through");
            });
        }
        assert!(
            failures.is_empty(),
            "unreadable cells:\n{}",
            failures.join("\n")
        );
    }

    /// The row state decides ahead of the cell's own flags: a deleted
    /// row's cells strike through whatever edit they carry (the row is
    /// going), a sent inserted row reads as out the door like any sent
    /// cell, and an inserted row's cells — every one of which is
    /// `edited` — take the `success` tint, never the warning one.
    #[gpui::test]
    fn the_row_state_decides_a_cells_paint_ahead_of_its_flags(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            let plain = cell_paint(theme, false, false, RowState::Document);
            assert_eq!(plain.fill, None);
            assert!(!plain.strike);
            let edited = cell_paint(theme, false, true, RowState::Document);
            assert_eq!(edited.fill, Some(theme.warning.opacity(0.25)));
            let inserted = cell_paint(theme, false, true, RowState::Inserted);
            assert_eq!(inserted.fill, Some(theme.success.opacity(0.18)));
            assert!(!inserted.strike);
            let inserted_sent = cell_paint(theme, true, true, RowState::Inserted);
            assert_eq!(inserted_sent.fill, Some(theme.muted));
            let deleted_edited = cell_paint(theme, false, true, RowState::Deleted);
            assert_eq!(deleted_edited.fill, None);
            assert!(deleted_edited.strike);
            assert_eq!(deleted_edited.text, theme.muted_foreground);
        });
    }

    /// The label column is index 0 and every value column sits one to its
    /// right — the one arithmetic every cursor mirror, click and editor
    /// lookup in this crate shares. Under a hidden label there is no
    /// offset at all: table column 0 IS model column 0.
    #[gpui::test]
    fn the_label_column_is_index_zero_and_the_cursor_never_enters_it(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let tones = cx.update(|cx| FlooredTones::derive(cx.theme()));
        let shown = MatrixDelegate::new(&CVI, WeakEntity::new_invalid(), 1, tones);
        assert_eq!(shown.model_col(0), None);
        assert_eq!(shown.model_col(1), Some(0));
        assert_eq!(shown.model_col(4), Some(3));
        assert_eq!(shown.table_col(0), 1);
        assert_eq!(shown.table_col(3), 4);
        let hidden = MatrixDelegate::new(&DIVIDEND, WeakEntity::new_invalid(), 1, tones);
        assert_eq!(hidden.model_col(0), Some(0));
        assert_eq!(hidden.model_col(3), Some(3));
        assert_eq!(hidden.table_col(0), 0);
        assert_eq!(hidden.table_col(3), 3);
    }

    /// An empty shown-label delegate still exposes its row-axis header. The test
    /// needs theme state but never upgrades its intentionally invalid tile handle.
    #[gpui::test]
    fn an_empty_model_still_names_its_row_axis(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let tones = cx.update(|cx| FlooredTones::derive(cx.theme()));
        let d = MatrixDelegate::new(&CVI, WeakEntity::new_invalid(), 7, tones);
        assert_eq!(d.row_axis.as_ref(), "term");
        assert!(d.model.rows.is_empty());
        assert!(d.editor.is_none());
        assert_eq!(d.cursor, Some((0, 0)));
        assert_eq!(d.tile_id, 7);
        assert!(d.tile.upgrade().is_none());
    }
}
