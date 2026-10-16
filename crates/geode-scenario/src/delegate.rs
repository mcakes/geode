//! The table behind a scenario tile: a label column pinned left, the
//! pivot's columns, a Σ column where the pivot has row sums, a Σ row where
//! it has column sums. Cells are prepared outside render into a
//! `WindowCache` (text, sign, heat fill, color); `render_td` only reads
//! it. The cursor is a cell; a selection is rows or a block over data
//! cells only (Σ edges are never selected).

use crate::core::pivot::{Head, Pivot};
use crate::core::prepare::{self, CellColor, Inputs, Layout, Palette, PreparedCell};
use crate::core::readout::{cursor_readout, head_text};
use geode_core::colour::scale::Diverging;
use geode_core::colour::{Anchors, NamedColours, Rgb, Sign, Tokens};
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
    MouseUpEvent, Pixels, SharedString, Stateful, Window, div, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::table::{Column, ColumnFixed, ColumnGroup, TableDelegate, TableState};
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

/// A table column's header text and unit, prepared with the picture.
#[derive(Debug, Clone, Default)]
struct PreparedHead {
    key: SharedString,
    unit: Option<SharedString>,
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
    /// The footer's cursor readout; `None` before a picture.
    pub cursor_readout: Option<SharedString>,
    /// One per table column, label and Σ included.
    heads: Vec<PreparedHead>,
    /// A ladder's header: the measure names ride a group row above the
    /// leaf heads, which carry the units. The table gives every header row
    /// one row's height, so a name and its unit stacked in one cell clip.
    units_row: bool,
    /// One per table column: its widest cell or header text, in
    /// characters, which floors the column's default width.
    widest: Vec<usize>,
    /// Whether the rows axis is a scenario axis: a column sum down it is unsummable.
    rows_axis_is_scenario: bool,
    cache: WindowCache<PreparedCell>,
    requested: WindowRequest,
    pub fitted: FittedWidths,
    /// The window's rem in pixels; default widths are design pixels at it.
    rem_px: f32,
    colours: Arc<NamedColours>,
    named: ColourCache,
    drag_last: Option<(usize, usize)>,
    drag_origin: bool,
}

impl ScenarioDelegate {
    pub fn new(rows_axis: &str, rows_axis_is_scenario: bool, colours: Arc<NamedColours>) -> Self {
        let mut d = ScenarioDelegate {
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
            cursor_readout: None,
            heads: Vec::new(),
            units_row: false,
            widest: Vec::new(),
            rows_axis_is_scenario,
            cache: WindowCache::default(),
            requested: WindowRequest::with_first(FIRST_WINDOW),
            fitted: FittedWidths::new(),
            rem_px: scale::DESIGN_REM,
            colours,
            named: ColourCache::new(),
            drag_last: None,
            drag_origin: false,
        };
        d.prepare_heads();
        d.prepare_widest();
        d
    }

    /// Resolve named colors against `colours` from now on. New definitions
    /// drop every resolved name and re-prepare the window, so none of the
    /// old mapping paints.
    pub fn set_colours(&mut self, colours: Arc<NamedColours>) {
        if Arc::ptr_eq(&self.colours, &colours) {
            return;
        }
        self.colours = colours;
        self.named = ColourCache::new();
        self.invalidate_cells();
    }

    /// A new picture: keep the cursor where it was if it still exists,
    /// re-resolve the selection, refill the window.
    pub fn set_picture(&mut self, pivot: Option<Arc<Pivot>>) {
        self.layout = pivot.as_deref().map(Layout::of).unwrap_or(EMPTY);
        self.pivot = pivot;
        self.prepare_heads();
        self.prepare_widest();
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
            let rescaled = self.scale != scale;
            self.heat = heat;
            self.scale = scale;
            if rescaled {
                self.prepare_widest();
            }
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
        self.prepare_cursor_readout();
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
        let (named, colours) = (&mut self.named, &self.colours);
        // A named color resolves here, once per prepared cell, so the cache
        // holds only fixed colors and render never runs the contrast search.
        self.cache.set_window(window, cols, |row, col| {
            let mut cell = prepare::cell(&inputs, row, col)?;
            if let CellColor::Named { name, sign } = &cell.color {
                let rgb = resolve_named(named, colours, theme, name, *sign, cell.fill);
                cell.color = CellColor::Fixed(rgb);
            }
            Some(cell)
        });
        // The cursor's text may have just entered the window.
        self.prepare_cursor_readout();
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

    /// After the cursor, the selection or the picture moved: resolve the
    /// selection and re-prepare the cursor readout.
    pub fn refresh_selection(&mut self) {
        self.resolve_selection();
        self.prepare_cursor_readout();
    }

    /// Resolve the selection to ranges over data cells and summarise each
    /// selected column over its selected rows.
    fn resolve_selection(&mut self) {
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

    /// Table column `col`'s header text, as prepared with the picture.
    fn column_key(&self, col: usize) -> SharedString {
        self.heads
            .get(col)
            .map(|h| h.key.clone())
            .unwrap_or_default()
    }

    /// Every table column's header text and unit, formatted once per
    /// picture so neither the column list nor a header cell formats in
    /// render.
    fn prepare_heads(&mut self) {
        let head = |col: usize| -> PreparedHead {
            if col == 0 {
                return PreparedHead {
                    key: self.rows_axis.clone(),
                    unit: None,
                };
            }
            if self.layout.is_sigma_col(col) {
                return PreparedHead {
                    key: SharedString::new_static(SIGMA),
                    unit: None,
                };
            }
            let column = self
                .layout
                .data_col(col)
                .and_then(|c| self.pivot.as_ref()?.columns.get(c));
            PreparedHead {
                key: column
                    .map(|column| head_text(&column.head).into())
                    .unwrap_or_default(),
                unit: column.and_then(|column| match &column.head {
                    Head::Measure { unit, .. } => unit.clone().map(SharedString::from),
                    Head::Axis { .. } => None,
                }),
            }
        };
        self.heads = (0..self.layout.cols()).map(head).collect();
        self.units_row = self.pivot.as_ref().is_some_and(|p| {
            p.columns
                .iter()
                .any(|c| matches!(&c.head, Head::Measure { unit: Some(_), .. }))
        });
    }

    /// Column `col`'s width without a fitted one: `design` at the window's
    /// rem, widened to hold the picture's widest text in it. A clipped
    /// right-aligned number loses its leading characters, a minus first.
    fn default_width(&self, col: usize, design: f32) -> f32 {
        let metrics = FitMetrics::xsmall_mono(px(self.rem_px));
        let chars = self.widest.get(col).copied().unwrap_or(0);
        let content = metrics.fit([chars as f32 * metrics.advance_px]);
        scale::design_px(design, px(self.rem_px)).max(content)
    }

    /// A ladder's name row: each measure's name over its column, blank
    /// over the label and Σ columns; `None` off a ladder.
    fn name_row(&self) -> Option<Vec<SharedString>> {
        self.units_row.then(|| {
            self.heads
                .iter()
                .enumerate()
                .map(|(col, h)| {
                    if col == 0 || self.layout.is_sigma_col(col) {
                        SharedString::default()
                    } else {
                        h.key.clone()
                    }
                })
                .collect()
        })
    }

    /// Each table column's widest text at the current scale: every cell,
    /// Σ included, and its header lines. Re-run with each picture and
    /// `:scale`; the text does not depend on paint.
    fn prepare_widest(&mut self) {
        let mut widest = self
            .pivot
            .as_deref()
            .map(|p| prepare::widest_chars(p, self.scale))
            .unwrap_or_default();
        widest.resize(self.heads.len(), 0);
        for (w, head) in widest.iter_mut().zip(&self.heads) {
            let unit = head.unit.as_ref().map_or(0, |u| u.chars().count());
            *w = (*w).max(head.key.chars().count()).max(unit);
        }
        self.widest = widest;
    }

    /// The footer's `1Y × 100 · 254.5` for the cursor cell, re-prepared
    /// whenever the cursor, the picture or the prepared window moves.
    fn prepare_cursor_readout(&mut self) {
        let readout = self.pivot.as_ref().and_then(|p| {
            let (r, c) = self.cursor;
            Some(cursor_readout(p.rows.get(r)?, &p.columns.get(c)?.head, self.cursor_text()).into())
        });
        self.cursor_readout = readout;
    }

    /// The two-level header's groups: consecutive columns sharing an outer
    /// label, in column order, as `(outer, count)`. Empty unless every
    /// column carries an outer label (a one-level header, a ladder, no
    /// picture).
    pub fn band_groups(&self) -> Vec<(String, usize)> {
        let Some(p) = &self.pivot else {
            return Vec::new();
        };
        let mut groups: Vec<(String, usize)> = Vec::new();
        for column in &p.columns {
            let Head::Axis {
                outer: Some(outer), ..
            } = &column.head
            else {
                return Vec::new();
            };
            match groups.last_mut() {
                Some((last, n)) if *last == outer.text => *n += 1,
                _ => groups.push((outer.text.clone(), 1)),
            }
        }
        groups
    }

    /// The cursor in table coordinates (the label column is column 0), or
    /// `None` while there are no data cells to stand on.
    pub fn table_cursor(&self) -> Option<(usize, usize)> {
        (self.layout.data_rows > 0 && self.layout.data_cols > 0)
            .then_some((self.cursor.0, self.cursor.1 + 1))
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
            .get(key.as_ref())
            .copied()
            .unwrap_or_else(|| self.default_width(col_ix, default));
        Column {
            key: key.clone(),
            name: key,
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

    /// One group row over a two-level header: an empty group over the
    /// label column, one per outer value over its inner columns, an empty
    /// one over Σ. The table lays the row with the leaf columns (the label
    /// group with the pinned column, the rest scrolling with theirs) and
    /// re-reads it on `refresh`, which every new picture and width change
    /// already calls.
    fn group_headers(&self, _cx: &App) -> Option<Vec<Vec<ColumnGroup>>> {
        if let Some(names) = self.name_row() {
            return Some(vec![
                names
                    .into_iter()
                    .map(|name| ColumnGroup::new(name, 1))
                    .collect(),
            ]);
        }
        let groups = self.band_groups();
        if groups.is_empty() {
            return None;
        }
        let mut row = Vec::with_capacity(groups.len() + 2);
        row.push(ColumnGroup::new(SharedString::default(), 1));
        row.extend(
            groups
                .into_iter()
                .map(|(outer, count)| ColumnGroup::new(outer, count)),
        );
        if self.layout.sigma_col {
            row.push(ColumnGroup::new(SharedString::default(), 1));
        }
        Some(vec![row])
    }

    /// An outer value centred over its columns, in the leaf headers' muted
    /// mono, with a hairline closing each group; the empty label and Σ
    /// groups paint no rule.
    fn render_group_th(
        &mut self,
        label: &SharedString,
        _col_span: usize,
        width: Pixels,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        // A ladder's name row aligns with the numbers under it and draws
        // no group rule; a two-level header centres each outer value over
        // its group and closes it with a hairline.
        let names = self.units_row;
        let named = !label.is_empty() && !names;
        let selector = label.clone();
        div()
            .w(width)
            .h_full()
            .flex_shrink_0()
            .flex()
            .items_end()
            .when(names, |el| el.justify_end())
            .when(!names, |el| el.justify_center())
            .px_1()
            .overflow_hidden()
            .whitespace_nowrap()
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .when(named, |el| {
                el.border_r_1()
                    .border_color(theme.border)
                    .debug_selector(move || format!("scenario-band-{selector}"))
            })
            .child(label.clone())
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (mut key, unit) = match self.heads.get(col_ix) {
            Some(h) => (h.key.clone(), h.unit.clone()),
            None => (SharedString::default(), None),
        };
        // Under a name row the leaf carries the unit alone: one line, as
        // the table gives each header row one row's height.
        let unit = if self.units_row && col_ix != 0 {
            key = unit.unwrap_or_default();
            None
        } else {
            unit
        };
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
            .child(key)
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
        let theme = cx.theme();
        let data = self.layout.data_col(col_ix).map(|c| (row_ix, c));
        let at_cursor = data == Some(self.cursor) && !self.layout.is_sigma_row(row_ix);
        let in_block = data.is_some_and(|(r, c)| {
            self.resolved
                .as_ref()
                .is_some_and(|s| s.kind == SelectKind::Block && s.contains(r, c))
        });
        // `fill_window` resolved every named color, so a cached cell's color
        // is fixed; the Arc clone does not allocate.
        let (text, fill, color) = match self.cache.get(row_ix, col_ix) {
            Some(c) => (
                SharedString::from(c.text.clone()),
                c.fill.map(to_hsla),
                match c.color {
                    CellColor::Fixed(rgb) => to_hsla(rgb),
                    CellColor::Named { .. } => theme.foreground,
                },
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
            .when(in_block, |el| el.child(selection_tint(theme)))
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

/// A named cell color as it paints: the definition resolved against the
/// theme (the theme's foreground when the name is undefined), its sign's
/// variant, then made readable over the cell's `fill` when it has one.
fn resolve_named(
    named: &mut ColourCache,
    colours: &NamedColours,
    theme: &ThemeInputs,
    name: &str,
    sign: Option<Sign>,
    fill: Option<Rgb>,
) -> Rgb {
    let base = named
        .get(colours, name, &theme.anchors, &theme.tokens)
        .map_or(theme.tokens.foreground, |r| to_rgb(r.for_sign(sign)));
    match fill {
        Some(fill) => Diverging::text_over(fill, base),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::view::Scale;

    fn ladder() -> ScenarioDelegate {
        ScenarioDelegate::new("tenor", false, Arc::new(NamedColours::default()))
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

    fn ladder_picture() -> Pivot {
        use crate::core::pivot::fixtures::{ladder_main, spot_ladder};
        use crate::core::pivot::{AxisDomains, Currency, Mode};
        Pivot::build(
            &ladder_main().snapshot("spot_ladder", Some(1)),
            None,
            &spot_ladder(),
            Mode {
                currency: Currency::Native,
                diff: false,
            },
            &AxisDomains::default(),
        )
        .unwrap()
    }

    fn totalled_picture() -> Pivot {
        use crate::core::pivot::fixtures::{vega_kt, vega_main, vega_totals};
        use crate::core::pivot::{AxisDomains, Currency, Mode};
        Pivot::build(
            &vega_main().snapshot("vega_buckets", Some(1)),
            Some(&vega_totals().snapshot("vega_buckets", Some(1))),
            &vega_kt(),
            Mode {
                currency: Currency::Native,
                diff: false,
            },
            &AxisDomains::default(),
        )
        .unwrap()
    }

    /// The table gives each header row one row's height, so a ladder's
    /// measure name and unit take a row each: the name in a group row
    /// over the column, the unit alone in the leaf head.
    #[test]
    fn a_ladder_header_puts_each_name_on_a_row_above_its_unit() {
        let mut d = ladder();
        d.ensure_palette(&gpui_component::Theme::default());
        assert_eq!(d.name_row(), None, "no picture, no name row");
        let picture = ladder_picture();
        let (name, unit) = picture
            .columns
            .iter()
            .enumerate()
            .find_map(|(i, c)| match &c.head {
                Head::Measure {
                    label,
                    unit: Some(unit),
                } => Some(((i, label.clone()), unit.clone())),
                _ => None,
            })
            .expect("the fixture ladder has a measure with a unit");
        d.set_picture(Some(Arc::new(picture)));
        let names = d.name_row().expect("a ladder with units has a name row");
        assert_eq!(names.len(), d.layout.cols());
        assert_eq!(names[0], "", "the label column has no name");
        assert_eq!(names[name.0 + 1], name.1.as_str());
        assert_eq!(d.heads[name.0 + 1].unit.as_deref(), Some(unit.as_str()));
        d.set_picture(Some(Arc::new(named_picture())));
        assert_eq!(d.name_row(), None, "an axis grid keeps one header row");
    }

    /// A default width holds the column's widest text, Σ included: a
    /// right-aligned total wider than the column lost its minus sign.
    #[test]
    fn a_default_width_holds_the_widest_text_sigma_included() {
        let mut d = ladder();
        d.ensure_palette(&gpui_component::Theme::default());
        // A large negative total: wider than every cell in its column,
        // as a sum over many rows is.
        let mut picture = totalled_picture();
        picture
            .column_sums
            .as_mut()
            .expect("the fixture is totalled")[0] = Some(-987_654_321.5);
        d.set_picture(Some(Arc::new(picture)));
        d.set_paint(true, Some(Scale::None));
        let sigma_row = d.layout.rows() - 1;
        assert!(d.layout.is_sigma_row(sigma_row));
        d.fill_window(0..d.layout.rows());
        let chars = |r: usize, c: usize| d.cache.get(r, c).map_or(0, |c| c.text.chars().count());
        assert!(
            (1..d.layout.cols()).any(|c| (0..sigma_row).all(|r| chars(sigma_row, c) > chars(r, c))),
            "the fixture has a total wider than its column's cells"
        );
        for col in 0..d.layout.cols() {
            let longest = (0..d.layout.rows())
                .filter_map(|r| d.cache.get(r, col).map(|c| c.text.chars().count()))
                .max()
                .unwrap_or(0);
            assert!(d.widest[col] >= longest, "column {col} under-measured");
        }
        let metrics = FitMetrics::xsmall_mono(px(d.rem_px));
        let design = scale::design_px(CELL_WIDTH, px(d.rem_px));
        d.widest[1] = 30;
        let wide = d.default_width(1, CELL_WIDTH);
        assert_eq!(wide, metrics.fit([30.0 * metrics.advance_px]));
        assert!(wide > design, "thirty characters overflow the design width");
        d.widest[1] = 1;
        assert_eq!(
            d.default_width(1, CELL_WIDTH),
            design,
            "narrow text keeps the design width"
        );
        // `:scale` re-measures: unscaled values print more characters than millions.
        d.set_paint(true, Some(Scale::Millions));
        let millions = d.widest.clone();
        d.set_paint(true, Some(Scale::None));
        assert!(d.widest.iter().zip(&millions).any(|(u, m)| u > m));
    }

    /// A reload's colors reach the paint: new definitions drop the names
    /// resolved under the old ones, and the same definitions keep them.
    #[test]
    fn new_named_colours_resolve_afresh_and_the_same_ones_are_kept() {
        use geode_core::colour::{Definition, Token};
        use gpui_component::Theme;
        let mut d = ladder();
        // Distinct tokens: the default theme's colors can coincide.
        d.ensure_palette(&Theme {
            colors: gpui_component::ThemeColor {
                info: gpui::green(),
                danger: gpui::red(),
                ..Theme::default().colors
            },
            ..Theme::default()
        });
        d.set_paint(false, None);
        d.set_picture(Some(Arc::new(named_picture())));
        let fg = d.theme.as_ref().unwrap().tokens.foreground;
        assert_eq!(
            cell_color(&d),
            CellColor::Fixed(fg),
            "undefined: foreground"
        );
        let colours = |token| {
            let mut c = NamedColours::default();
            c.insert("delta".into(), Definition::token(token));
            Arc::new(c)
        };
        let info = colours(Token::Info);
        // `set_colours` alone re-prepares the window.
        d.set_colours(info.clone());
        let resolved = cell_color(&d);
        assert_ne!(
            resolved,
            CellColor::Fixed(fg),
            "the reloaded definition paints"
        );
        d.set_colours(info);
        assert_eq!(cell_color(&d), resolved, "the same definitions");
        d.set_colours(colours(Token::Danger));
        let danger = cell_color(&d);
        assert_ne!(danger, resolved, "nothing of the old mapping paints");
    }

    /// A Vega K/T picture whose value column carries the named color
    /// `delta`.
    fn named_picture() -> Pivot {
        use crate::core::pivot::fixtures::{vega_kt, vega_main};
        use crate::core::pivot::{AxisDomains, Currency, Mode};
        use geode_core::scenario::MeasureColumn;
        use geode_core::view::{Colour, ColumnFormat};
        let mut shape = vega_kt();
        shape.value = shape.value.map(|v| MeasureColumn {
            format: ColumnFormat {
                colour: Colour::Named("delta".into()),
                ..ColumnFormat::MEASURE
            },
            ..v
        });
        Pivot::build(
            &vega_main().snapshot("vega_buckets", Some(1)),
            None,
            &shape,
            Mode {
                currency: Currency::Native,
                diff: false,
            },
            &AxisDomains::default(),
        )
        .unwrap()
    }

    /// The first data cell's prepared color.
    fn cell_color(d: &ScenarioDelegate) -> CellColor {
        d.cache.get(0, 1).expect("a prepared cell").color.clone()
    }

    /// A named color is resolved and made readable over its heat fill when
    /// the window is prepared, so render reads a fixed color and never runs
    /// the contrast search.
    #[test]
    fn a_named_color_arrives_in_the_cache_fixed_and_readable_over_its_fill() {
        use geode_core::colour::{Definition, TEXT_READABLE_RATIO, Token, contrast_ratio};
        use gpui_component::Theme;
        let mut colours = NamedColours::default();
        colours.insert("delta".into(), Definition::token(Token::Info));
        let mut d = ScenarioDelegate::new("tenor", false, Arc::new(colours));
        d.ensure_palette(&Theme::default());
        d.set_picture(Some(Arc::new(named_picture())));
        let mut cells = 0;
        for row in 0..d.layout.rows() {
            for col in 0..d.layout.cols() {
                let Some(cell) = d.cache.get(row, col) else {
                    continue;
                };
                let CellColor::Fixed(rgb) = cell.color else {
                    panic!("({row}, {col}) is still named: {:?}", cell.color);
                };
                if let Some(fill) = cell.fill {
                    assert!(contrast_ratio(rgb, fill) >= TEXT_READABLE_RATIO);
                    cells += 1;
                }
            }
        }
        assert!(cells > 0, "the fixture paints heat under the named color");
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

    /// The scroll target is the cursor's table cell (past the label
    /// column), and there is none to scroll to without data cells.
    #[test]
    fn the_table_cursor_is_offset_past_the_label_column() {
        assert_eq!(ladder().table_cursor(), None);
        let mut d = with_layout(ladder(), 3, 2);
        d.cursor = (2, 1);
        assert_eq!(d.table_cursor(), Some((2, 2)));
    }

    #[test]
    fn yank_needs_a_picture() {
        assert_eq!(ladder().yank_text(), None);
    }

    #[test]
    fn the_scale_override_is_kept_and_unchanged_paint_is_a_no_op() {
        let mut d = ladder();
        d.ensure_palette(&gpui_component::Theme::default());
        d.set_picture(Some(Arc::new(named_picture())));
        d.set_paint(true, Some(Scale::Thousands));
        assert_eq!(d.scale, Some(Scale::Thousands));
        // A blanked cell survives an identical call: nothing is re-prepared.
        d.cache.refill_cell(0, 1, || None);
        d.set_paint(true, Some(Scale::Thousands));
        assert!(d.cache.get(0, 1).is_none(), "unchanged paint re-prepared");
        d.set_paint(false, Some(Scale::Thousands));
        assert!(!d.heat);
        assert!(d.cache.get(0, 1).is_some(), "a change re-prepares");
    }
}
