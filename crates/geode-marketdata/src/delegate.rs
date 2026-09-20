//! The panel's `TableDelegate` (user ruling 2026-09-14: the body is
//! gpui-component's table, for visual unity with the blotter — roadmap
//! ruling 6's uniform-row-list form is superseded).
//!
//! It owns nothing the tile does not already have: the prepared
//! [`MatrixModel`] as an `Rc` swapped wholesale on every rebuild, a mirror
//! of the tile's cursor, and mirrors of the open cell editor and of a
//! choice cell's open typeahead (painted under its cell). **The
//! tile's own cursor stays the truth** — this is a paint-time copy, kept
//! in step by `MarketDataTile::sync_cursor`, so nothing here decides
//! anything a keystroke, a yank or a commit reads back.
//!
//! One structural difference from the model's own grid: the table's column
//! 0 is the ROW-LABEL column (the row axis — `term` for CVI), so a value
//! cell at model column `c` is table column `c + 1`. The cursor never
//! enters column 0 (`h` at model column 0 stays put), which is why
//! [`MatrixDelegate::model_col`] answers `None` for it rather than
//! saturating to 0 — a click there moves the cursor's row and leaves its
//! column alone.

use crate::core::{MatrixModel, PanelSpec};
use crate::header;
use crate::popup::{ChoicePaint, render_choice};
use crate::tile::{DateFieldPaint, FlooredTones, MarketDataTile};
use geode_shell::fonts;
use gpui::prelude::*;
use gpui::{
    App, Context, Entity, FocusHandle, Hsla, SharedString, TextAlign, WeakEntity, Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::table::{Column, ColumnFixed, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Theme};
use std::rc::Rc;

/// The row-label column's width, and one value cell's. Fixed: a document's
/// columns are a ladder the desk chose (spec §8.2 — "no horizontal
/// virtualisation, since no sketched document has more than a few dozen
/// columns").
///
/// **Columns are deliberately NOT resizable** (controller ruling
/// 2026-09-14, review Minor 2), here and on `TableState::col_resizable`.
/// A dragged width would have nowhere to live: a panel has no presentation
/// document (`view_presentation.toml` belongs to a view), so the width
/// would be this delegate's in-memory `column()` answer — and
/// `TableState::refresh` re-prepares `col_groups` from exactly that. Since
/// every model swap refreshes ([`crate::tile::MarketDataTile`]'s
/// `install_model`), a drag would snap back on the next delivery (about
/// every 5 s on the demo bus) or the next committed edit. A handle that
/// undoes itself seconds later is worse than no handle; offer it again
/// when a width has somewhere to be written.
const LABEL_WIDTH: f32 = 128.0;
const CELL_WIDTH: f32 = 84.0;

/// The table column the row labels live in. Column 0, pinned left for the
/// blotter's own reason (user ruling 2026-09-12 on the tree column): the
/// row's identity must stay readable however far right the values scroll.
pub(crate) const LABEL_COL: usize = 0;

/// What the tile's open editor looks like from the delegate: which cell it
/// sits in (`col: None` is the row-label column, Task 8's row-label
/// editor — built into the type and painted by `render_td`'s label arm
/// now so the two arms cannot drift, though nothing opens it yet) and what
/// to paint there — the text `Input`, or the segmented date field's
/// prepared segments plus the focus handle its keys route through.
///
/// A paint-time COPY, like the cursor mirror beside it: the tile's
/// `Editing` is the truth, and `MarketDataTile::sync_cursor`/`sync_editor`
/// re-mirror it on every change (a date field's segments change on every
/// keystroke, which is why the field's own key path re-mirrors too).
#[derive(Clone)]
pub(crate) struct DelegateEditor {
    pub row: usize,
    pub col: Option<usize>,
    pub paint: DelegateEditorPaint,
}

/// The two forms an editor paints in a cell — the tile's `EditorState`,
/// with the date form reduced to what `render_td` needs: the pure
/// `DateField` itself stays on the tile, since only the tile's key path
/// ever steps it.
#[derive(Clone)]
pub(crate) enum DelegateEditorPaint {
    Text(Entity<InputState>),
    Date {
        paint: DateFieldPaint,
        focus: FocusHandle,
    },
}

/// The open `Choice` cell's typeahead as the delegate paints it (dividend
/// spec §4.4): the cell it hangs under, in model coordinates, and the
/// prepared rows. A paint-time COPY like [`DelegateEditor`] — the tile's
/// `Popup::Choice` is the truth, re-mirrored on every change through
/// `MarketDataTile::sync_editor`; the `Rc` is the tile's own prepared
/// paint, so a re-mirror is a refcount bump and `render_td` reads
/// prepared strings.
#[derive(Clone)]
pub(crate) struct DelegateChoice {
    pub row: usize,
    pub col: usize,
    pub paint: Rc<ChoicePaint>,
}

/// Every field is `pub(crate)`, never `pub` (review Minor 4): a model swap
/// is only correct when it is paired with a `TableState::refresh`, and
/// `MarketDataTile::install_model` is the one place that pairs them. Crate
/// visibility is what keeps that invariant compiler-kept — a caller
/// outside this crate could otherwise write `model` on its own and paint
/// the previous document's columns.
pub struct MatrixDelegate {
    /// The prepared grid. An `Rc` swapped by the tile on every rebuild —
    /// never cloned per frame, and never mutated in place.
    pub(crate) model: Rc<MatrixModel>,
    /// The row axis's name (`term`), painted as column 0's header.
    row_axis: SharedString,
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
            cursor: Some((0, 0)),
            editor: None,
            choice: None,
            tile,
            tile_id,
            tones,
        }
    }

    /// The editor to paint in table cell (`row_ix`, `col_ix`), if the
    /// open one sits there — `col: None` is the row-label column (table
    /// column [`LABEL_COL`]), a model column is every other.
    fn editor_at(&self, row_ix: usize, col_ix: usize) -> Option<&DelegateEditor> {
        self.editor
            .as_ref()
            .filter(|e| e.row == row_ix && e.col == Self::model_col(col_ix))
    }

    /// One editor's element, painted in whichever cell it belongs to:
    /// the text `Input`, or the strip's own segmented date field
    /// (`header::render_date_field`, shared so a date cell and a date
    /// attribute cannot paint or route keys differently). A tile that
    /// has been dropped paints nothing — the caller falls back to the
    /// cell's own text — since there is nothing left to route a key to.
    fn render_editor(
        &mut self,
        editor: &DelegateEditor,
        theme: &Theme,
    ) -> Option<gpui::AnyElement> {
        match &editor.paint {
            DelegateEditorPaint::Text(state) => Some(Input::new(state).into_any_element()),
            DelegateEditorPaint::Date { paint, focus } => {
                let tile = self.tile.upgrade()?;
                self.tones.refresh(theme);
                Some(
                    header::render_date_field(
                        paint,
                        focus,
                        theme,
                        &self.tones,
                        &tile,
                        self.tile_id,
                    )
                    .into_any_element(),
                )
            }
        }
    }

    /// The model column a table column carries, or `None` for the
    /// row-label column — which is every caller's cue that the cursor has
    /// no business there.
    pub fn model_col(table_col: usize) -> Option<usize> {
        table_col.checked_sub(LABEL_COL + 1)
    }

    /// Whether table column `col_ix` is the LAST slice-value column — the
    /// one whose right edge closes the slice block ahead of the ladder.
    /// Never true for a model with no slice columns.
    pub fn closes_slice_block(&self, col_ix: usize) -> bool {
        self.model.slice_columns > 0
            && Self::model_col(col_ix) == Some(self.model.slice_columns - 1)
    }

    /// The table column a model column is painted in.
    pub fn table_col(model_col: usize) -> usize {
        model_col + LABEL_COL + 1
    }
}

impl TableDelegate for MatrixDelegate {
    /// The row-label column plus one per value column. Always at least
    /// one, so an empty panel still paints its row axis's name.
    fn columns_count(&self, _cx: &App) -> usize {
        1 + self.model.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.model.rows.len()
    }

    /// Read only on prepare and `TableState::refresh` — which is why every
    /// model swap on the tile goes through `install_model`.
    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(model_col) = Self::model_col(col_ix) else {
            return Column {
                key: SharedString::from("__row_axis"),
                name: self.row_axis.clone(),
                align: TextAlign::Left,
                // Not sortable: a document's rows are the desk's own
                // ladder (the `/` find moves the cursor rather than
                // narrowing, for the same reason — spec §8.3), and not
                // movable, since the labels are the grid's identity.
                sort: None,
                width: px(LABEL_WIDTH),
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
            // A document's values are numbers (`PanelSpec::value_type` is
            // `f64`/`i64`), formatted by the panel's own `ColumnFormat`,
            // so they read down the column's right edge — `render_td`
            // does the aligning, since the component only carries this
            // field for a delegate to read back.
            align: TextAlign::Right,
            sort: None,
            width: px(CELL_WIDTH),
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

    /// One prepared cell: its text, or the editor when this is the cell
    /// being edited.
    ///
    /// Allocation discipline (PHILOSOPHY §6): nothing is formatted or
    /// allocated per cell beyond the `debug_selector` closure, which gpui
    /// drops unevaluated outside a test/`test-support` build. The text is
    /// a `SharedString` clone (a refcount) out of the model the tile
    /// prepared, and the colours are `Copy` theme reads.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let Some(model_col) = Self::model_col(col_ix) else {
            // The row-label column: the label in the data face, in the
            // full foreground — it is what identifies the row. The editor
            // slot is here too (Task 8's row-label editor opens into it;
            // nothing does yet), so the label arm and the value arm paint
            // an editor through the one `render_editor`.
            let label = self
                .model
                .rows
                .get(row_ix)
                .map(|r| r.label.clone())
                .unwrap_or_default();
            let editor = self
                .editor_at(row_ix, col_ix)
                .cloned()
                .and_then(|e| self.render_editor(&e, theme));
            let el = div()
                .size_full()
                .flex()
                .items_center()
                .font_family(fonts::MONO)
                .text_color(theme.foreground)
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
        let cell = self
            .model
            .rows
            .get(row_ix)
            .and_then(|r| r.cells.get(model_col));
        let mut el = div()
            .size_full()
            .flex()
            .items_center()
            .justify_end()
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            // Lets a test locate this exact cell with `cx.debug_bounds`,
            // the blotter's own I4 door; a gpui no-op in release, where
            // the closure is dropped unevaluated.
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
        let CellPaint { fill, text } = cell_paint(theme, cell.sent, cell.edited);
        el = el.when_some(fill, |el, fill| el.bg(fill)).text_color(text);
        // The editor is cloned out (three refcounts at most) because
        // `render_editor` needs `&mut self` for the tones refresh while
        // `editor_at` borrows `self.editor`; the cell's own text is what
        // paints when there is no editor here, or no tile left to route
        // its keys to.
        let text = cell.text.clone();
        let editor = self
            .editor_at(row_ix, col_ix)
            .cloned()
            .and_then(|e| self.render_editor(&e, theme));
        // The choice popup hangs under THIS cell when it is the one being
        // edited (dividend spec §4.4): a zero-size absolute child at the
        // cell's bottom-left is the anchor `render_choice`'s `TopLeft`
        // positions against — the header popups' own arrangement, with
        // the cell as the anchor's frame instead of the header. Painted
        // through `deferred`, so the table's clip never cuts it. A tile
        // that has been dropped paints nothing, as `render_editor` does.
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

/// How a value cell paints in one draft state: an optional fill and the
/// text colour over it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CellPaint {
    pub fill: Option<Hsla>,
    pub text: Hsla,
}

/// The one answer to "what colour is a cell in this state", read by
/// `render_td` and by the test that checks it against every bundled theme.
///
/// The STATE lives in the fill and the text is always the theme's own
/// `foreground` (user report 2026-09-14: dirty cells were unreadable on
/// most themes). The paired tokens look right but are not: `warning_foreground`
/// is for text on a SOLID warning fill and falls back to `primary_foreground`
/// — the background family — at the pinned rev, so over a 25% tint it was
/// cream on cream (1.00:1 on Nord, 1.13:1 on Default Light); and
/// `muted_foreground` is secondary text on the BACKGROUND, not on `muted`
/// itself, where 15 bundled themes put it under 3:1. `foreground` is the
/// one colour every theme author made readable on their own background,
/// which a translucent tint or the muted band barely moves.
///
/// `sent` is checked first: a sent cell is also an edited one (the draft
/// keeps its edits until the echo clears them, spec §9.4), and what it
/// needs to say is that it is out the door.
pub(crate) fn cell_paint(theme: &Theme, sent: bool, edited: bool) -> CellPaint {
    let fill = if sent {
        Some(theme.muted)
    } else if edited {
        Some(theme.warning.opacity(0.25))
    } else {
        None
    };
    CellPaint {
        fill,
        text: theme.foreground,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::CVI;
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

    /// An edited or sent cell's text must be readable over its own fill on
    /// EVERY bundled theme, at the same 3:1 floor Part 2c holds named
    /// colours to. The first build painted an edited cell's text in
    /// `warning_foreground` — the token for text on a SOLID warning fill,
    /// which falls back to `primary_foreground` (the background family) at
    /// the pinned rev — over a 25% tint of `warning`, so on Gruvbox Light
    /// the text was the background colour on a barely-tinted background.
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
                for (state, sent, edited) in [("edited", false, true), ("sent", true, false)] {
                    let paint = cell_paint(theme, sent, edited);
                    let fill = paint.fill.expect("both marked states carry a fill");
                    let ratio = contrast_ratio(to_rgb(paint.text), over(fill, ground(theme)));
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {state} text at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(
            failures.is_empty(),
            "unreadable cells:\n{}",
            failures.join("\n")
        );
    }

    /// The label column is index 0 and every value column sits one to its
    /// right — the one arithmetic every cursor mirror, click and editor
    /// lookup in this crate shares.
    #[test]
    fn the_label_column_is_index_zero_and_the_cursor_never_enters_it() {
        assert_eq!(MatrixDelegate::model_col(0), None);
        assert_eq!(MatrixDelegate::model_col(1), Some(0));
        assert_eq!(MatrixDelegate::model_col(4), Some(3));
        assert_eq!(MatrixDelegate::table_col(0), 1);
        assert_eq!(MatrixDelegate::table_col(3), 4);
    }

    /// A fresh delegate paints the row axis's name and nothing else: one
    /// column, no rows. The panel's header says why (`no document
    /// received for <key>`), so an empty grid needs no invented row.
    /// A `gpui::test` only for the theme the tones derive from; the tile
    /// handle is never upgraded here.
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
