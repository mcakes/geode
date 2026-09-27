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
use geode_shell::fonts;
use geode_shell::linenumbers::{GUTTER_GAP_PX, LineNumbers, gutter_number, gutter_px};
use gpui::prelude::*;
use gpui::{
    App, Context, Div, Entity, FocusHandle, Hsla, SharedString, TextAlign, WeakEntity, Window, div,
    px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::table::{Column, ColumnFixed, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Theme};
use std::rc::Rc;

/// Fixed pixel widths for the label and value columns. Columns cannot be
/// resized or moved: there is no persisted width state, and table refresh would
/// replace transient widths on the next model install. These widths are not
/// rem-scaled because column() has no Window from which to read the rem size.
const LABEL_WIDTH: f32 = 128.0;
const CELL_WIDTH: f32 = 84.0;

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
}

impl MatrixDelegate {
    pub(crate) fn new(
        spec: &'static PanelSpec,
        tile: WeakEntity<MarketDataTile>,
        tile_id: u64,
        tones: FlooredTones,
    ) -> MatrixDelegate {
        MatrixDelegate {
            model: Rc::new(MatrixModel::default()),
            row_axis: SharedString::from(spec.rows.column),
            label_column: spec.rows.shown(),
            cursor: Some((0, 0)),
            editor: None,
            choice: None,
            tile,
            tile_id,
            tones,
            line_numbers: LineNumbers::Off,
            numbers: Vec::new(),
            numbers_stamp: None,
        }
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
                key: SharedString::from("__row_axis"),
                name: self.row_axis.clone(),
                align: TextAlign::Left,
                // Preserve document row order and column identity: no sorting or movement.
                sort: None,
                width: px(LABEL_WIDTH + self.gutter_px()),
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
        Column {
            key: name.clone(),
            name,
            // Right-align value-column headers. Value cells use the same alignment,
            // including typed text/date/choice columns in flat panels.
            align: TextAlign::Right,
            sort: None,
            width: px(CELL_WIDTH
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
        div()
            .size_full()
            .flex()
            .child(
                div()
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
                .debug_selector(|| format!("marketdata-cell-{row_ix}-{col_ix}"));
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
            .when(strike, |el| el.line_through());
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
    use crate::core::{CVI, DIVIDEND};
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
