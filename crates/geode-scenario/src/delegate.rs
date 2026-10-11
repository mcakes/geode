//! The table behind a scenario tile: a label column pinned left, the
//! pivot's columns, a Σ column where the pivot has row sums, a Σ row where
//! it has column sums. Cells are prepared outside render into a
//! `WindowCache` (text, sign, heat fill, color); `render_td` only reads
//! it. The cursor is a cell; a selection is rows or a block over data
//! cells only (Σ edges are never selected).

use crate::core::pivot::{Head, Pivot};
use crate::core::prepare::{self, CellColor, Inputs, Layout, Palette, PreparedCell};
use crate::core::readout::head_text;
use geode_core::colour::scale::Diverging;
use geode_core::colour::{Anchors, NamedColours, Tokens};
use geode_core::grid::selection::{self, Accumulator, Resolved, SelectKind, Selection, describe};
use geode_core::view::Scale;
use geode_shell::colfit::{FitMetrics, FittedWidths};
use geode_shell::fonts;
use geode_shell::shell::aggregates::AggregateCell;
use geode_shell::shell::colours::{
    anchors_from_theme, theme_signature, to_hsla, to_rgb, tokens_from_theme,
};
use geode_shell::shell::scale;
use geode_tile::colour::ColourCache;
use geode_tile::grid::{FIRST_WINDOW, WindowCache, WindowRequest};
use geode_tile::motion::Motion;
use gpui::prelude::*;
use gpui::{
    App, Context, Div, EventEmitter, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, SharedString, Stateful, Window, div, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::table::{Column, ColumnFixed, TableDelegate, TableState};
use gpui_component::theme::Theme;
use std::ops::Range;
use std::sync::Arc;

/// What a cell press means to the tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellPointer {
    Press {
        row: usize,
        col: usize,
        shift: bool,
    },
    Drag {
        row: usize,
        col: usize,
    },
    /// A right press: focus only (the shell's route); no row menu.
    Context {
        row: usize,
        col: usize,
    },
}

impl EventEmitter<CellPointer> for TableState<ScenarioDelegate> {}

/// The label column's and each value column's default width, in design
/// pixels (scaled to the window's rem); a fitted width replaces either.
pub const LABEL_WIDTH: f32 = 72.0;
pub const CELL_WIDTH: f32 = 84.0;

/// The theme inputs every prepared cell and named color read, resolved
/// once per theme signature.
struct ThemeInputs {
    signature: [Hsla; 28],
    palette: Palette,
    anchors: Anchors,
    tokens: Tokens,
}

pub struct ScenarioDelegate {
    pub pivot: Option<Arc<Pivot>>,
    pub layout: Layout,
    /// The rows axis name, the label column's header.
    pub rows_axis: SharedString,
    theme: Option<ThemeInputs>,
    pub scale: Option<Scale>,
    pub heat: bool,
    /// Data coordinates: the cursor never enters a Σ edge.
    pub cursor: (usize, usize),
    pub selection: Option<Selection<usize, usize>>,
    pub resolved: Option<Resolved>,
    /// One total per selected column, over the selected rows.
    pub summary: Vec<AggregateCell>,
    /// `2 rows × 3 cols`, while a selection is live.
    pub extent: Option<SharedString>,
    /// Whether the rows axis is a scenario axis: a column sum down it is unsummable.
    rows_axis_is_scenario: bool,
    cache: WindowCache<PreparedCell>,
    requested: WindowRequest,
    pub fitted: FittedWidths,
    /// The window's rem in pixels; default widths are design pixels at it.
    rem_px: f32,
    colours: NamedColours,
    named: ColourCache,
    drag_last: Option<(usize, usize)>,
    drag_origin: bool,
}

impl ScenarioDelegate {
    pub fn new(rows_axis: &str, rows_axis_is_scenario: bool, colours: NamedColours) -> Self {
        ScenarioDelegate {
            pivot: None,
            layout: EMPTY,
            rows_axis: rows_axis.to_string().into(),
            theme: None,
            scale: None,
            heat: true,
            cursor: (0, 0),
            selection: None,
            resolved: None,
            summary: Vec::new(),
            extent: None,
            rows_axis_is_scenario,
            cache: WindowCache::default(),
            requested: WindowRequest::with_first(FIRST_WINDOW),
            fitted: FittedWidths::new(),
            rem_px: scale::DESIGN_REM,
            colours,
            named: ColourCache::new(),
            drag_last: None,
            drag_origin: false,
        }
    }

    /// A new picture: keep the cursor where it was if it still exists,
    /// re-resolve the selection, refill the window.
    pub fn set_picture(&mut self, pivot: Option<Arc<Pivot>>) {
        self.layout = pivot.as_deref().map(Layout::of).unwrap_or(EMPTY);
        self.pivot = pivot;
        self.clamp_cursor();
        self.refresh_selection();
        self.invalidate_cells();
    }

    /// Rebuild the palette when the theme signature moved; true when it
    /// did. Called at construction and from the tile's theme observer,
    /// never from render.
    pub fn ensure_palette(&mut self, theme: &Theme) -> bool {
        let signature = theme_signature(theme);
        if self
            .theme
            .as_ref()
            .is_some_and(|t| t.signature == signature)
        {
            return false;
        }
        // The palette reads exactly the tokens the signature covers, and
        // the heat scale's background is the theme's, as
        // `Diverging::for_tokens` takes it.
        let tokens = tokens_from_theme(theme);
        self.theme = Some(ThemeInputs {
            signature,
            palette: Palette {
                bullish: tokens.bullish,
                bearish: tokens.bearish,
                background: tokens.background,
                foreground: tokens.foreground,
                muted: tokens.muted,
            },
            anchors: anchors_from_theme(theme),
            tokens,
        });
        self.invalidate_cells();
        true
    }

    /// Paint on or off (`p`) and the `:scale` override; re-prepares only
    /// on a change.
    pub fn set_paint(&mut self, heat: bool, scale: Option<Scale>) {
        if self.heat != heat || self.scale != scale {
            self.heat = heat;
            self.scale = scale;
            self.invalidate_cells();
            self.refresh_selection();
        }
    }

    /// Default widths follow the window's rem; true when it moved.
    pub fn set_rem(&mut self, rem_px: f32) -> bool {
        if self.rem_px == rem_px {
            return false;
        }
        self.rem_px = rem_px;
        true
    }

    fn invalidate_cells(&mut self) {
        let window = self.requested.refill_range(self.layout.rows());
        self.cache.clear();
        if let Some(window) = window {
            self.fill_window(window);
        }
    }

    fn fill_window(&mut self, window: Range<usize>) {
        let (Some(pivot), Some(theme)) = (&self.pivot, &self.theme) else {
            return;
        };
        let inputs = Inputs {
            pivot,
            palette: &theme.palette,
            scale: self.scale,
            heat: self.heat,
        };
        let cols = self.layout.cols();
        self.cache
            .set_window(window, cols, |row, col| prepare::cell(&inputs, row, col));
    }

    fn clamp_cursor(&mut self) {
        let (r, c) = self.cursor;
        self.cursor = (
            r.min(self.layout.data_rows.saturating_sub(1)),
            c.min(self.layout.data_cols.saturating_sub(1)),
        );
    }

    /// Apply a shared grid motion over data coordinates.
    pub fn apply_motion(&mut self, motion: Motion) {
        let (rows, cols) = (self.layout.data_rows, self.layout.data_cols);
        if rows == 0 || cols == 0 {
            return;
        }
        let selecting = self.selection.is_some();
        if motion.moves_rows() {
            self.cursor.0 = geode_tile::motion::row(self.cursor.0, rows, motion, selecting);
        } else {
            self.cursor.1 = geode_tile::motion::col(self.cursor.1, cols, motion);
        }
        self.refresh_selection();
    }

    /// `V`/`v`: start a selection of `kind` at the cursor, switch a live
    /// one to `kind`, or end it when it already is `kind`.
    pub fn start_selection(&mut self, kind: SelectKind) {
        match &mut self.selection {
            Some(s) if s.kind == kind => self.selection = None,
            Some(s) => s.kind = kind,
            None => {
                self.selection = Some(Selection {
                    kind,
                    anchor_row: self.cursor.0,
                    anchor_col: self.cursor.1,
                });
            }
        }
        self.refresh_selection();
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.refresh_selection();
    }

    /// A pointer press or drag on table cell `(row, col)`: the cursor moves
    /// to it (a Σ row keeps the cursor's row), a shift press or a drag
    /// extends a block from the cursor, a plain press ends any selection.
    /// The label and Σ columns are not cells the cursor takes.
    pub fn point(&mut self, row: usize, col: usize, extend: bool) {
        let Some(c) = self.layout.data_col(col) else {
            return;
        };
        if !extend {
            self.selection = None;
        } else if self.selection.is_none() {
            self.selection = Some(Selection {
                kind: SelectKind::Block,
                anchor_row: self.cursor.0,
                anchor_col: self.cursor.1,
            });
        }
        let r = if row < self.layout.data_rows {
            row
        } else {
            self.cursor.0
        };
        self.cursor = (r, c);
        self.refresh_selection();
    }

    /// Resolve the selection to ranges over data cells and summarise each
    /// selected column over its selected rows.
    pub fn refresh_selection(&mut self) {
        let Some(s) = &self.selection else {
            self.resolved = None;
            self.summary.clear();
            self.extent = None;
            return;
        };
        let resolved = selection::resolve(
            s.kind,
            (s.anchor_row, s.anchor_col),
            self.cursor,
            self.layout.data_cols,
        );
        let (rows, cols) = (resolved.rows.clone(), resolved.cols.clone());
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        self.extent = Some(
            format!(
                "{} row{} × {} col{}",
                rows.len(),
                plural(rows.len()),
                cols.len(),
                plural(cols.len())
            )
            .into(),
        );
        self.summary = match &self.pivot {
            None => Vec::new(),
            Some(p) => cols
                .filter_map(|c| {
                    let column = p.columns.get(c)?;
                    // Scenario rows are alternative worlds: summing down
                    // them is no total.
                    let mut acc = if self.rows_axis_is_scenario {
                        Accumulator::unsummable()
                    } else {
                        Accumulator::default()
                    };
                    for r in rows.clone() {
                        acc.add(p.cell(r, c), true);
                    }
                    let mut format = column.format.clone();
                    if let Some(scale) = self.scale {
                        format.scale = scale;
                    }
                    let total = describe(&acc.finish(), &format);
                    Some(AggregateCell {
                        label: head_text(&column.head).into(),
                        text: total.text.into(),
                        sign: total.sign,
                        refused: total.refused,
                    })
                })
                .collect(),
        };
        self.resolved = Some(resolved);
    }

    /// The clipboard text for the selection, or the cursor cell alone;
    /// `None` before a picture.
    pub fn yank_text(&self) -> Option<String> {
        let p = self.pivot.as_ref()?;
        if p.rows.is_empty() || p.columns.is_empty() {
            return None;
        }
        let (rows, cols) = match &self.resolved {
            Some(r) => (r.rows.clone(), r.cols.clone()),
            None => (
                self.cursor.0..self.cursor.0 + 1,
                self.cursor.1..self.cursor.1 + 1,
            ),
        };
        Some(prepare::tsv(p, &self.rows_axis, rows, cols))
    }

    /// The cursor's prepared text, for the footer readout.
    pub fn cursor_text(&self) -> &str {
        self.cache
            .get(self.cursor.0, self.cursor.1 + 1)
            .map_or("", |c| &*c.text)
    }

    /// Fit the label column and every value column to the header and the
    /// rows in the window; `None` before a picture.
    pub fn fit_columns(&self, m: &FitMetrics) -> Option<FittedWidths> {
        let p = self.pivot.as_ref()?;
        let rows = self.cache.window();
        if rows.is_empty() {
            return None;
        }
        let texts = |col: usize| {
            rows.clone()
                .filter_map(move |r| self.cache.get(r, col).map(|c| &*c.text))
        };
        let mut out = FittedWidths::new();
        out.insert(
            self.rows_axis.to_string(),
            m.fit_text(&self.rows_axis, texts(0)),
        );
        for (i, column) in p.columns.iter().enumerate() {
            let head = head_text(&column.head);
            let width = m.fit_text(&head, texts(i + 1));
            out.insert(head, width);
        }
        if self.layout.sigma_col {
            out.insert(
                SIGMA.into(),
                m.fit_text(SIGMA, texts(self.layout.cols() - 1)),
            );
        }
        Some(out)
    }

    fn column_key(&self, col: usize) -> String {
        if col == 0 {
            return self.rows_axis.to_string();
        }
        if self.layout.is_sigma_col(col) {
            return SIGMA.into();
        }
        self.layout
            .data_col(col)
            .and_then(|c| {
                self.pivot
                    .as_ref()?
                    .columns
                    .get(c)
                    .map(|column| head_text(&column.head))
            })
            .unwrap_or_default()
    }

    fn text_color(&mut self, cell: &PreparedCell, theme: &Theme) -> Hsla {
        match &cell.color {
            CellColor::Fixed(rgb) => to_hsla(*rgb),
            CellColor::Named { name, sign } => {
                let Some(inputs) = &self.theme else {
                    return theme.foreground;
                };
                let resolved = self
                    .named
                    .get(&self.colours, name, &inputs.anchors, &inputs.tokens);
                let hsla = resolved.map_or(theme.foreground, |r| r.for_sign(*sign));
                match cell.fill {
                    Some(fill) => to_hsla(Diverging::text_over(fill, to_rgb(hsla))),
                    None => hsla,
                }
            }
        }
    }

    fn wire_pointer(el: Div, cx: &Context<TableState<Self>>, row: usize, col: usize) -> Div {
        el.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                let d = this.delegate_mut();
                d.drag_last = Some((row, col));
                d.drag_origin = true;
                cx.emit(CellPointer::Press {
                    row,
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
            if !d.drag_origin || d.drag_last == Some((row, col)) {
                return;
            }
            d.drag_last = Some((row, col));
            cx.emit(CellPointer::Drag { row, col });
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| this.delegate_mut().drag_origin = false),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| this.delegate_mut().drag_origin = false),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                cx.emit(CellPointer::Context { row, col })
            }),
        )
    }
}

const EMPTY: Layout = Layout {
    data_rows: 0,
    data_cols: 0,
    sigma_row: false,
    sigma_col: false,
};

/// The Σ column's key and header.
const SIGMA: &str = "Σ";

/// The selection tint: an absolute overlay under the text, so the heat
/// fill still shows through.
fn selection_tint(theme: &Theme) -> Div {
    div().absolute().inset_0().bg(theme.selection.opacity(0.35))
}

impl TableDelegate for ScenarioDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.layout.cols()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.layout.rows()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let key = self.column_key(col_ix);
        let is_label = col_ix == 0;
        let default = if is_label { LABEL_WIDTH } else { CELL_WIDTH };
        let width = self
            .fitted
            .get(&key)
            .copied()
            .unwrap_or_else(|| scale::design_px(default, px(self.rem_px)));
        Column {
            key: SharedString::from(key.clone()),
            name: SharedString::from(key),
            align: if is_label {
                gpui::TextAlign::Left
            } else {
                gpui::TextAlign::Right
            },
            sort: None,
            width: px(width),
            fixed: is_label.then_some(ColumnFixed::Left),
            movable: false,
            resizable: false,
            ..Column::default()
        }
    }

    fn visible_rows_changed(
        &mut self,
        visible_range: Range<usize>,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        self.requested.record(visible_range.clone());
        self.fill_window(visible_range);
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let unit = self.layout.data_col(col_ix).and_then(|c| {
            match &self.pivot.as_ref()?.columns.get(c)?.head {
                Head::Measure { unit, .. } => unit.clone(),
                Head::Axis { .. } => None,
            }
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .when(col_ix == 0, |el| el.items_start())
            .when(col_ix != 0, |el| el.items_end())
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .debug_selector(move || format!("scenario-th-{col_ix}"))
            .child(self.column_key(col_ix))
            .when_some(unit, |el, unit| el.child(div().text_xs().child(unit)))
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let base = self
            .pivot
            .as_ref()
            .is_some_and(|p| p.base_row == Some(row_ix));
        let in_rows = self
            .resolved
            .as_ref()
            .is_some_and(|r| r.kind == SelectKind::Rows && r.contains_row(row_ix));
        div()
            .id(("scenario-row", row_ix))
            // The diff base: `list_active` at low opacity under the whole row.
            .when(base, |el| el.bg(theme.list_active.opacity(0.25)))
            .when(in_rows, |el| el.bg(theme.selection.opacity(0.35)))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let data = self.layout.data_col(col_ix).map(|c| (row_ix, c));
        let at_cursor = data == Some(self.cursor) && !self.layout.is_sigma_row(row_ix);
        let in_block = data.is_some_and(|(r, c)| {
            self.resolved
                .as_ref()
                .is_some_and(|s| s.kind == SelectKind::Block && s.contains(r, c))
        });
        let cell = self.cache.get(row_ix, col_ix).cloned();
        let (text, fill, color) = match &cell {
            Some(c) => (
                SharedString::from(c.text.to_string()),
                c.fill.map(to_hsla),
                self.text_color(c, &theme),
            ),
            None => (SharedString::default(), None, theme.foreground),
        };
        let el = div()
            .size_full()
            .flex()
            .items_center()
            .when(col_ix == 0, |el| el.justify_start())
            .when(col_ix != 0, |el| el.justify_end())
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            .relative()
            .debug_selector(move || format!("scenario-cell-{row_ix}-{col_ix}"))
            .when_some(fill, |el, fill| el.bg(fill))
            .text_color(color)
            .when(in_block, |el| el.child(selection_tint(&theme)))
            // The cursor cell: a border in the table's active-border color,
            // never a fill, so the heat fill shows through it.
            .when(at_cursor, |el| {
                el.border_1().border_color(theme.table_active_border)
            });
        // Data cells take the cursor; the label and Σ cells only focus.
        let el = if data.is_some() {
            Self::wire_pointer(el, cx, row_ix, col_ix)
        } else {
            el
        };
        el.child(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::view::Scale;

    fn ladder() -> ScenarioDelegate {
        ScenarioDelegate::new("tenor", false, NamedColours::default())
    }

    fn with_layout(mut d: ScenarioDelegate, rows: usize, cols: usize) -> ScenarioDelegate {
        d.layout = Layout {
            data_rows: rows,
            data_cols: cols,
            sigma_row: true,
            sigma_col: true,
        };
        d
    }

    #[test]
    fn a_press_on_a_sigma_or_label_column_moves_nothing() {
        let mut d = with_layout(ladder(), 3, 2);
        d.point(1, 0, false);
        assert_eq!(d.cursor, (0, 0), "the label column");
        d.point(1, 3, false);
        assert_eq!(d.cursor, (0, 0), "the Σ column");
        d.point(3, 2, false);
        assert_eq!(d.cursor, (0, 1), "the Σ row keeps the row");
    }

    #[test]
    fn a_shift_press_extends_a_block_from_the_cursor_and_a_plain_one_ends_it() {
        let mut d = with_layout(ladder(), 3, 2);
        d.point(2, 2, true);
        let r = d.resolved.clone().expect("a block");
        assert_eq!((r.kind, r.rows, r.cols), (SelectKind::Block, 0..3, 0..2));
        assert_eq!(d.extent.as_deref(), Some("3 rows × 2 cols"));
        d.point(1, 1, false);
        assert!(d.selection.is_none() && d.resolved.is_none() && d.extent.is_none());
        assert_eq!(d.cursor, (1, 0));
    }

    #[test]
    fn selecting_the_same_kind_again_ends_it_and_the_other_kind_switches() {
        let mut d = with_layout(ladder(), 3, 2);
        d.start_selection(SelectKind::Block);
        d.start_selection(SelectKind::Rows);
        assert_eq!(d.resolved.as_ref().map(|r| r.cols.clone()), Some(0..2));
        d.start_selection(SelectKind::Rows);
        assert!(d.selection.is_none());
    }

    #[test]
    fn motions_stay_inside_the_data_cells() {
        let mut d = with_layout(ladder(), 3, 2);
        d.apply_motion(Motion::LineEnd);
        d.apply_motion(Motion::Bottom(None));
        assert_eq!(d.cursor, (2, 1));
        d.apply_motion(Motion::Rows {
            by: 5,
            counted: true,
        });
        assert_eq!(d.cursor, (2, 1));
    }

    #[test]
    fn a_new_smaller_picture_clamps_the_cursor() {
        let mut d = with_layout(ladder(), 3, 2);
        d.cursor = (2, 1);
        d.set_picture(None);
        assert_eq!(d.cursor, (0, 0));
        assert_eq!(d.layout.cols(), 1, "the label column alone");
    }

    #[test]
    fn yank_needs_a_picture() {
        assert_eq!(ladder().yank_text(), None);
    }

    #[test]
    fn the_scale_override_is_kept_and_unchanged_paint_is_a_no_op() {
        let mut d = ladder();
        d.set_paint(true, Some(Scale::Thousands));
        assert_eq!(d.scale, Some(Scale::Thousands));
        d.set_paint(false, Some(Scale::Thousands));
        assert!(!d.heat);
    }
}
