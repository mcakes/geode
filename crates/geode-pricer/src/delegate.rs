//! TableDelegate over a prepared Rc<GridModel> installed by the tile. Cursor, loading
//! state, entry field, and cell editor are read-only mirrors of tile state. Column zero
//! is a pinned tree column with indentation, a fixed chevron slot, and shorthand; the
//! cell cursor does not enter it.
//!
//! Package and entry backgrounds belong to render_tr. The table replaces row
//! backgrounds for hover and selection; per-cell fills would obscure those states.

use crate::grid::{GridModel, GridRowKind};
use crate::paint::Paints;
use crate::popup::{ChoicePaint, render_choice};
use crate::tile::PricerTile;
use geode_shell::fonts;
use geode_shell::linenumbers::{GUTTER_GAP_PX, LineNumbers, gutter_number, gutter_px};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, SharedString, Stateful, TextAlign,
    WeakEntity, Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::table::{Column, ColumnFixed, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Theme, h_flex};
use std::rc::Rc;

/// The tree column: pixels, like every width here (the vocabulary's own
/// known gap); not resizable, since a dragged width has nowhere to live.
const TREE_WIDTH: f32 = 260.0;
/// One depth step, and the chevron slot every non-entry row reserves
/// (empty on a line or leg), both on the rem scale: roots share one
/// leading edge whether or not they carry a chevron, and a leg sits
/// exactly one step in from its package.
const INDENT: f32 = 14.0;
const CHEVRON_SLOT: f32 = 14.0;
/// What the empty table says: the next action, not an icon.
pub(crate) const EMPTY_TEXT: &str = "No lines — press o to add one";
pub(crate) const LOADING_TEXT: &str = "Loading sheet…";
pub(crate) const TREE_COL: usize = 0;

/// The gutter number of every painted grid row: `None` on the entry
/// placeholder, which no motion can land on. Rows are numbered by their
/// ordinal among cursor rows — the index `NG` jumps to and `Nj`/`Nk`
/// count — so an open placeholder never shifts the numbers below it.
/// Relative mode measures from the cursor row; with no cursor row it
/// numbers absolutely. Off numbers nothing.
pub(crate) fn number_rows(
    mode: LineNumbers,
    len: usize,
    entry: Option<usize>,
    cursor: Option<usize>,
) -> Vec<Option<usize>> {
    // A grid row's ordinal among cursor rows: the placeholder above it
    // takes no number.
    let ordinal = |row: usize| row - usize::from(entry.is_some_and(|e| e < row));
    let (mode, at) = match (mode, cursor.filter(|c| Some(*c) != entry)) {
        (LineNumbers::Relative, Some(c)) => (mode, ordinal(c)),
        (LineNumbers::Relative, None) => (LineNumbers::On, 0),
        (mode, _) => (mode, 0),
    };
    (0..len)
        .map(|row| {
            (Some(row) != entry)
                .then(|| gutter_number(mode, ordinal(row), at))
                .flatten()
        })
        .collect()
}

/// What `SheetDelegate::refresh_numbers` last derived from: row count,
/// placeholder row, the cursor row (relative mode only — absolute
/// numbers ignore it), and the mode.
type NumbersStamp = (usize, Option<usize>, Option<usize>, LineNumbers);

/// A chevron click, re-implemented from the blotter (spec §8.2: "the
/// blotter's idiom re-implemented, nothing lifted"); the tile toggles
/// the package at this grid row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChevronClicked(pub usize);

impl EventEmitter<ChevronClicked> for TableState<SheetDelegate> {}

/// A paint-time copy of the tile's open editor (`PricerTile::sync_editor`):
/// the grid cell it sits on, its field, and the typeahead's prepared rows.
#[derive(Clone)]
pub(crate) struct EditorPaint {
    pub row: usize,
    /// The plan column (never the tree).
    pub col: usize,
    pub input: Entity<InputState>,
    pub choice: Option<Rc<ChoicePaint>>,
}

pub struct SheetDelegate {
    pub(crate) model: Rc<GridModel>,
    /// `(grid row, plan column)`; `None` with no cursor row.
    pub(crate) cursor: Option<(usize, usize)>,
    pub(crate) paints: Paints,
    /// The tile's `loading`, mirrored by `install_model`: the empty table
    /// says `Loading sheet…` rather than inviting an `o` the tile would
    /// refuse.
    pub(crate) loading: bool,
    chevron: Option<(control::ControlInputs, control::ControlPaint)>,
    /// The tile's open entry field, mirrored here so `render_td` can paint
    /// it (the `Entry` row); the tile's `entry` is the source of
    /// truth, this is a read-only mirror.
    pub(crate) entry: Option<Entity<InputState>>,
    /// The tile's open cell editor, mirrored the same way.
    pub(crate) editor: Option<EditorPaint>,
    /// The typeahead's rows call back into the tile; a dropped tile
    /// paints no popup.
    pub(crate) tile: WeakEntity<PricerTile>,
    /// `[ui] line_numbers`, mirrored from the `UiSettings` global by the
    /// tile (`PricerTile::on_ui_settings`), which refreshes the table on a
    /// change: the tree column's width includes the gutter.
    pub(crate) line_numbers: LineNumbers,
    /// Gutter text per grid row, empty on the placeholder, and the
    /// gutter's width. `refresh_numbers` prepares both outside render, so
    /// `render_td` only clones a refcount.
    numbers: Vec<SharedString>,
    gutter: f32,
    numbers_stamp: Option<NumbersStamp>,
}

impl SheetDelegate {
    pub(crate) fn new(theme: &Theme, tile: WeakEntity<PricerTile>) -> Self {
        SheetDelegate {
            model: Rc::new(GridModel::default()),
            cursor: None,
            paints: Paints::derive(theme),
            loading: false,
            chevron: None,
            entry: None,
            editor: None,
            tile,
            line_numbers: LineNumbers::Off,
            numbers: Vec::new(),
            gutter: 0.0,
            numbers_stamp: None,
        }
    }

    /// Re-derive the gutter text and width when the model's shape, the
    /// mode, or (in relative mode) the cursor row moved. The tile calls it
    /// after every model install, cursor sync and mode change, BEFORE the
    /// table re-reads `column()`: a stale width would clip the tree text
    /// or leave a hole where the gutter was.
    pub(crate) fn refresh_numbers(&mut self) {
        let mode = self.line_numbers;
        let len = self.model.rows.len();
        let entry = self.model.entry_row();
        let cursor = match mode {
            LineNumbers::Relative => self.cursor.map(|(row, _)| row),
            _ => None,
        };
        let stamp = (len, entry, cursor, mode);
        if self.numbers_stamp == Some(stamp) {
            return;
        }
        self.numbers_stamp = Some(stamp);
        self.gutter = gutter_px(mode, len - usize::from(entry.is_some()));
        self.numbers.clear();
        self.numbers
            .extend(number_rows(mode, len, entry, cursor).into_iter().map(|n| {
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
    /// under the pointer. Package-muted text is contrast-adjusted there too.
    fn chevron_states(&mut self, theme: &Theme) -> control::ControlPaint {
        let inputs = control::ControlInputs::new(
            theme,
            control::Rest::Bare,
            self.paints.row_hover,
            self.paints.package_muted,
        );
        match &self.chevron {
            Some((have, paint)) if *have == inputs => *paint,
            _ => {
                let paint = control::control_paint(&inputs);
                self.chevron = Some((inputs, paint));
                paint
            }
        }
    }
}

impl TableDelegate for SheetDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        1 + self.model.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.model.rows.len()
    }

    /// Read only on prepare and `TableState::refresh`, which is why every
    /// model swap goes through `install_model`.
    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(c) = Self::plan_col(col_ix).and_then(|i| self.model.columns.get(i)) else {
            return Column {
                key: SharedString::from("__tree"),
                name: SharedString::from("line"),
                align: TextAlign::Left,
                sort: None,
                width: px(TREE_WIDTH + self.gutter_px()),
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
            width: px(c.width),
            movable: false,
            resizable: false,
            ..Column::default()
        }
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        div()
            .size_full()
            .flex()
            .items_center()
            .when(matches!(column.align, TextAlign::Right), |el| {
                el.justify_end()
            })
            .font_family(fonts::MONO)
            .debug_selector(|| format!("pricer-th-{col_ix}"))
            .child(column.name)
    }

    /// A package row's ground and the entry row's active fill, on the row
    /// (see the module doc). A filler row past the model paints nothing.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let ground = match self.model.rows.get(row_ix).map(|r| r.kind) {
            Some(GridRowKind::Package { .. }) => Some(self.paints.package_ground),
            Some(GridRowKind::Entry) => Some(cx.theme().table_active),
            _ => None,
        };
        div()
            .id(("row", row_ix))
            .when_some(ground, |el, g| el.bg(g))
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
    /// cell's own contents never cover it. The row's ground (a package's,
    /// hover, selection) paints under it, so it takes the row's floored
    /// muted paint, and the cursor row the row's own text paint.
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
        let package = matches!(
            self.model.rows.get(row_ix).map(|r| r.kind),
            Some(GridRowKind::Package { .. })
        );
        let on_cursor = self.cursor.is_some_and(|(row, _)| row == row_ix);
        let paint = match (on_cursor, package) {
            (true, false) => self.paints.own,
            (true, true) => self.paints.package_own,
            (false, false) => self.paints.muted,
            (false, true) => self.paints.package_muted,
        };
        let text = self.numbers.get(row_ix).cloned().unwrap_or_default();
        div()
            .size_full()
            .flex()
            .child(
                div()
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

impl SheetDelegate {
    /// One prepared cell. Nothing is formatted or allocated here beyond
    /// the `debug_selector` closure (dropped unevaluated outside tests):
    /// the text is a `SharedString` refcount out of the model, the colours
    /// `Copy` reads of the `Paints` memo, which the tile re-derives on a
    /// theme change rather than per cell.
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
        let Some(row) = model.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let package = matches!(row.kind, GridRowKind::Package { .. });
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
        let Some(plan_col) = Self::plan_col(col_ix) else {
            // The tree column: indent by depth, then the fixed chevron
            // slot (a chevron on a package, empty otherwise), then the
            // row's shorthand — except the entry placeholder, which puts
            // the open field (or nothing, mid-transition) where its row
            // will land: the same indent and slot.
            let slot = div()
                .w(scale::design(CHEVRON_SLOT))
                .h_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center();
            let el = base.pl(scale::design(row.depth as f32 * INDENT));
            if row.kind == GridRowKind::Entry {
                return match &self.entry {
                    Some(input) => el
                        .debug_selector(|| "pricer-entry".into())
                        .child(slot)
                        .child(div().flex_1().min_w_0().child(Input::new(input).xsmall()))
                        .into_any_element(),
                    None => el.into_any_element(),
                };
            }
            let slot = match row.kind {
                GridRowKind::Package { open } => {
                    let states = self.chevron_states(cx.theme());
                    slot.child(
                        div()
                            .id(("pricer-chevron", row_ix))
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(radius)
                            .text_color(paints.package_muted)
                            .pointer_states(states)
                            .debug_selector(|| format!("pricer-chevron-{row_ix}"))
                            .on_click(cx.listener(move |this, e: &ClickEvent, _window, cx| {
                                cx.stop_propagation();
                                // Toggle only on the first press of a double-click.
                                if e.click_count() > 1 {
                                    return;
                                }
                                this.set_selected_row(row_ix, cx);
                                cx.emit(ChevronClicked(row_ix));
                            }))
                            .child(if open { "▾" } else { "▸" }),
                    )
                }
                _ => slot,
            };
            return el
                .text_color(paints.text(crate::core::CellState::Own, package))
                .child(slot)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(row.tree.clone()),
                )
                .into_any_element();
        };
        let at_cursor = self.cursor == Some((row_ix, plan_col));
        let right = model.columns.get(plan_col).is_some_and(|c| c.right);
        let el = base
            .when(right, |el| el.justify_end())
            .when(at_cursor, |el| el.border_1().border_color(active_border));
        // The open editor paints its field in place of the text (a
        // refcount clone at most). The typeahead hangs under THIS cell: a
        // zero-size absolute child at the cell's bottom-left is the anchor
        // `render_choice`'s `TopLeft` positions against (the market-data
        // delegate's arrangement).
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
                el.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .debug_selector(|| format!("pricer-editor-{row_ix}-{col_ix}"))
                        .child(Input::new(&e.input).xsmall()),
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
            None => el
                .when_some(row.cells.get(plan_col), |el, cell| {
                    let text = cell.text.clone();
                    el.text_color(paints.text(cell.state, package)).map(|el| {
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
                .into_any_element(),
        }
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
        let n = number_rows(LineNumbers::On, 5, None, Some(3));
        assert_eq!(n, [1, 2, 3, 4, 5].map(Some));
    }

    #[test]
    fn rel_measures_from_the_cursor_which_shows_its_own_number() {
        let n = number_rows(LineNumbers::Relative, 5, None, Some(2));
        assert_eq!(n, [2, 1, 3, 1, 2].map(Some));
        assert_eq!(
            number_rows(LineNumbers::Relative, 3, None, None),
            [1, 2, 3].map(Some),
            "no cursor row: absolute"
        );
    }

    /// The placeholder at grid row 1 is blank and does not shift the
    /// rows below it: their numbers are the index `NG` jumps to, which
    /// counts cursor rows only.
    #[test]
    fn the_entry_placeholder_is_blank_and_shifts_nothing() {
        assert_eq!(
            number_rows(LineNumbers::On, 6, Some(1), Some(0)),
            vec![Some(1), None, Some(2), Some(3), Some(4), Some(5)]
        );
        assert_eq!(
            number_rows(LineNumbers::Relative, 6, Some(1), Some(3)),
            vec![Some(2), None, Some(1), Some(3), Some(1), Some(2)],
            "grid row 3 is the third cursor row; distances skip the placeholder"
        );
    }

    #[test]
    fn off_numbers_nothing() {
        assert!(
            number_rows(LineNumbers::Off, 4, None, Some(0))
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
            ColumnKind::Price
            | ColumnKind::Delta
            | ColumnKind::Gamma
            | ColumnKind::Vega
            | ColumnKind::Theta
            | ColumnKind::Rho => fmt(-1_234_567.89),
            // Representative text values for the width check.
            ColumnKind::Underlying => "SX5E".into(),
            ColumnKind::Expiry => "20DEC26".into(),
            ColumnKind::Type => "C".into(),
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
}
