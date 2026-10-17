//! The grid's `TableDelegate`: it paints the prepared model and reports a
//! row press, a right press and a head press to the tile, which owns the
//! cursor, the selection and the sort. Nothing is formatted here: every
//! text, glyph core and id comes from `Prepared`, and every color from
//! [`Paints`], built once per theme.

use std::ops::Range;
use std::rc::Rc;

use geode_chart::spark::{Spark, SparkPaint, sizes};
use geode_shell::colfit::{
    CURSOR_BORDER_PX, FitMetrics, FittedWidths, TABLE_TEXT_REM, widths_from_record,
};
use geode_shell::fonts;
use geode_shell::shell::scale;
use geode_widgets::cell::delta_chip::{self, ChipPaint};
use geode_widgets::cell::design;
use geode_widgets::cell::heat::{self, HeatPaint};
use geode_widgets::cell::percent_slider::{self, SliderPaint};
use geode_widgets::cell::zero_bar::{self, BarPaint};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Div, ElementId, Entity, Hsla, MouseButton, MouseDownEvent, Pixels,
    Point, SharedString, Stateful, Window, div, px, rems,
};
use gpui_component::table::{
    Column as TableColumn, ColumnGroup, ColumnSort, DataTable, TableDelegate, TableState,
};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, Theme};

use crate::core::model::{
    BAR_ALPHA, Glyph, Prepared, PreparedCell, PreparedRow, SORT_MARK_DESIGN_PX, glyph_width,
};
use crate::core::views::{Column, ColumnKind, View};

/// Whether a cell paints in the muted tone: its own flag (a lookback
/// mismatch, a short rank), an absent row, or, in the Underlying column
/// only, a stale row. A stale row's numbers are still its numbers and keep
/// their sign colors; its name and the header's surface run carry the
/// staleness, as the blotter marks age in its header.
pub(crate) fn td_muted(row: &PreparedRow, cell: &PreparedCell, column: &Column) -> bool {
    cell.muted || !row.present || (column.kind == ColumnKind::Underlying && row.stale)
}

/// Whether alternate rows are striped. Every geode grid paints unstriped;
/// the ring choice follows this, so flipping it keeps sparkline rings true.
const STRIPE: bool = false;

/// Every color the grid paints with, resolved once per theme. The rings
/// are the row fills a sparkline's end dot sits on: the table's ground,
/// the cursor row's highlight, a stripe, a selected row's tint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Paints {
    pub(crate) chip: ChipPaint,
    /// A change painted from a board asked over another lookback.
    pub(crate) chip_muted: ChipPaint,
    pub(crate) slider: SliderPaint,
    /// A rank over too short a history.
    pub(crate) slider_muted: SliderPaint,
    pub(crate) bar: BarPaint,
    pub(crate) bar_muted: BarPaint,
    pub(crate) heat: HeatPaint,
    pub(crate) heat_muted: HeatPaint,
    pub(crate) spark: SparkPaint,
    pub(crate) cursor_ring: Hsla,
    pub(crate) stripe_ring: Hsla,
    pub(crate) plain_ring: Hsla,
    pub(crate) selection_ring: Hsla,
}

/// A selected row's tint, under the table's own cursor-row highlight.
fn selection_tint(t: &Theme) -> Hsla {
    t.selection.opacity(0.35)
}

impl Paints {
    /// Sign colors are the chart tokens, as the blotter's unconfigured
    /// sign columns paint.
    pub(crate) fn from_theme(t: &Theme) -> Paints {
        let radius = t.radius_tokens().sm;
        let muted = t.muted_foreground;
        let chip = ChipPaint {
            positive: t.chart_bullish,
            negative: t.chart_bearish,
            zero: muted,
            text: t.foreground,
            radius,
        };
        let slider = SliderPaint {
            track: t.muted,
            fill: t.primary.opacity(0.6),
            tick: t.background,
            marker: t.foreground,
            warning: t.warning,
            text: muted,
            radius,
        };
        let bar = BarPaint {
            positive: t.chart_bullish,
            negative: t.chart_bearish,
            zero: muted,
            axis: t.border,
            text: t.foreground,
            fill_alpha: BAR_ALPHA,
            radius,
        };
        let heat = HeatPaint {
            hue: t.primary,
            text: t.foreground,
        };
        // The table paints its ground over the tile's background; the
        // cursor row's highlight replaces a stripe, as the table does.
        let ground = t.background.blend(t.table);
        let active = if t.list.active_highlight {
            t.table_active
        } else {
            t.accent
        };
        Paints {
            chip,
            chip_muted: ChipPaint {
                positive: muted,
                negative: muted,
                text: muted,
                ..chip
            },
            slider,
            slider_muted: SliderPaint {
                marker: muted,
                fill: t.muted_foreground.opacity(0.4),
                ..slider
            },
            bar,
            bar_muted: BarPaint {
                positive: muted,
                negative: muted,
                text: muted,
                ..bar
            },
            heat,
            heat_muted: HeatPaint {
                text: muted,
                ..heat
            },
            spark: SparkPaint {
                line: muted,
                end: t.primary,
                mark: t.primary,
                ring: ground,
            },
            cursor_ring: ground.blend(active),
            stripe_ring: ground.blend(t.table_even),
            plain_ring: ground,
            selection_ring: ground.blend(selection_tint(t)),
        }
    }
}

/// What the grid reports to the tile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum GridEvent {
    /// A left press on shown row `row`; `shift` extends a selection.
    RowPressed { row: usize, shift: bool },
    /// A right press on shown row `row`, at `position` in the window.
    RowContext { row: usize, position: Point<Pixels> },
    /// A press on a sortable column's head or its sort control.
    HeadPressed(usize),
}

impl gpui::EventEmitter<GridEvent> for TableState<GridDelegate> {}

/// The table inside a wrapper that keeps window focus where the shell put
/// it: the capture-phase `prevent_default` stops `DataTable` taking focus
/// on a press (its own keys would shadow the tile's), while the row's
/// press still fires. `min_h_0` lets the virtualized list size to the tile.
pub(crate) fn table_el(state: &Entity<TableState<GridDelegate>>, tile_id: u64) -> Div {
    div()
        .debug_selector(move || format!("volwatch-table-{tile_id}"))
        .flex_1()
        .min_h_0()
        .w_full()
        .capture_any_mouse_down(|_event, window, _cx| window.prevent_default())
        .child(
            DataTable::new(state)
                .with_size(Size::XSmall)
                .bordered(false)
                .stripe(STRIPE),
        )
}

pub(crate) use crate::core::views::sortable;

/// The table's horizontal chrome inside a column's width, in real px: its
/// own cell padding (read from the registry size, not restated; it is
/// fixed px, not rem) on both sides and the cursor cell's border. A width
/// that forgot it clips the cell's text.
pub(crate) fn chrome_px() -> f32 {
    let pad = Size::XSmall.table_cell_padding();
    f32::from(pad.left) + f32::from(pad.right) + CURSOR_BORDER_PX
}

/// The sort mark's width at the fit's rem: [`SORT_MARK_DESIGN_PX`] (the
/// table's `Icon::size_3` in `p(px(2.))`) on the rem scale.
fn sort_mark_px(m: &FitMetrics) -> f32 {
    scale::design_px(SORT_MARK_DESIGN_PX, px(m.rem_px))
}

/// A width belongs to a column in a view: the same measure paints as a
/// chip in Changes and a heat tint in Term, and its head differs, so a
/// width fitted in one view is wrong in another. The session key is
/// `<view>:<column id>`.
pub(crate) fn sized_key(view: View, id: &str) -> String {
    format!("{}:{}", view.key(), id)
}

/// The sized widths a restored record holds, keyed by view. A record
/// written before widths were keyed by view holds bare column ids: an
/// autosize in Skew once wrote `skew-3M-level = 30`, which Levels then
/// applied to a column whose head needs 60 px. An entry without a known
/// view prefix is dropped, as every malformed session value is.
pub(crate) fn restored_widths(record: Option<&toml::Table>) -> FittedWidths {
    let mut fitted = widths_from_record(record);
    fitted.retain(|k, _| {
        k.split_once(':')
            .is_some_and(|(v, _)| View::ALL.iter().any(|view| view.key() == v))
    });
    fitted
}

pub(crate) struct GridDelegate {
    pub(crate) prepared: Rc<Prepared>,
    pub(crate) columns: Rc<Vec<Column>>,
    /// The view the columns belong to: the key space of `fitted`.
    view: View,
    /// Shown rows in painted order, as indices into `prepared.rows`.
    pub(crate) visible: Vec<usize>,
    /// (shown row, column) under the cursor.
    pub(crate) cursor: Option<(usize, usize)>,
    pub(crate) selection: Option<Range<usize>>,
    /// The sorted column and whether it descends, for the head's mark.
    sorted: Option<(usize, bool)>,
    pub(crate) paints: Paints,
    rem_px: f32,
    /// Column widths at the window's rem, from the prepared design px.
    widths: Vec<Pixels>,
    /// Widths `:autosize` fitted and a header drag set, keyed
    /// [`sized_key`] (`view:id`), in pixels at the rem they were set at;
    /// each replaces its column's prepared width in that view.
    pub(crate) fitted: FittedWidths,
    /// One run of equal `Column::group`s per entry; empty when no column
    /// is grouped.
    groups: Vec<ColumnGroup>,
    /// Each column's head, shared so a frame's heads allocate nothing.
    heads: Vec<SharedString>,
    ids: Vec<SharedString>,
    empty: SharedString,
}

impl GridDelegate {
    pub(crate) fn new(paints: Paints) -> GridDelegate {
        GridDelegate {
            prepared: Rc::default(),
            columns: Rc::default(),
            view: View::Changes,
            visible: Vec::new(),
            cursor: None,
            selection: None,
            sorted: None,
            paints,
            rem_px: scale::DESIGN_REM,
            widths: Vec::new(),
            fitted: FittedWidths::new(),
            groups: Vec::new(),
            heads: Vec::new(),
            ids: Vec::new(),
            empty: SharedString::default(),
        }
    }

    /// Install a prepared model over `columns`, the columns of `view`.
    /// `true` when the column layout (widths, heads, groups, the view's
    /// sized widths or the sort mark) changed: the table re-reads columns
    /// only on a refresh.
    pub(crate) fn set_model(
        &mut self,
        prepared: Rc<Prepared>,
        columns: Rc<Vec<Column>>,
        sorted: Option<(usize, bool)>,
        view: View,
    ) -> bool {
        let relayout = !Rc::ptr_eq(&self.columns, &columns)
            || self.prepared.widths != prepared.widths
            || self.sorted != sorted
            || self.view != view;
        self.prepared = prepared;
        self.sorted = sorted;
        self.view = view;
        if !Rc::ptr_eq(&self.columns, &columns) {
            self.groups = groups(&columns);
            self.heads = columns
                .iter()
                .map(|c| SharedString::from(c.head.clone()))
                .collect();
            self.ids = columns
                .iter()
                .map(|c| SharedString::from(c.id.clone()))
                .collect();
            self.columns = columns;
        }
        if relayout {
            self.relayout();
        }
        relayout
    }

    pub(crate) fn set_view(
        &mut self,
        visible: Vec<usize>,
        cursor: Option<(usize, usize)>,
        selection: Option<Range<usize>>,
        empty: SharedString,
    ) {
        self.visible = visible;
        self.cursor = cursor;
        self.selection = selection;
        self.empty = empty;
    }

    pub(crate) fn set_paints(&mut self, paints: Paints) {
        self.paints = paints;
    }

    /// `true` when the rem changed: the widths follow it.
    pub(crate) fn set_rem(&mut self, rem_px: f32) -> bool {
        if self.rem_px == rem_px {
            return false;
        }
        self.rem_px = rem_px;
        self.relayout();
        true
    }

    /// A prepared width is design px of content, so the table's chrome
    /// is added here where real px are known; a sized width (fitted or
    /// dragged, in this view) already holds it and is taken as is.
    fn relayout(&mut self) {
        let rem = px(self.rem_px);
        let chrome = chrome_px();
        self.widths = self
            .prepared
            .widths
            .iter()
            .enumerate()
            .map(|(ix, w)| {
                match self
                    .ids
                    .get(ix)
                    .and_then(|id| self.fitted.get(&sized_key(self.view, id)))
                {
                    Some(sized) => px(*sized),
                    None => px(scale::design_px(*w, rem) + chrome),
                }
            })
            .collect();
    }

    /// Record the widths a header drag reports (every column's, in table
    /// order) for this view. Only a column whose width differs from what
    /// `column()` gives is recorded, so an untouched column keeps
    /// following the rem and its fit.
    pub(crate) fn record_widths(&mut self, widths: &[Pixels]) {
        for (ix, w) in widths.iter().enumerate().take(self.ids.len()) {
            if *w != self.width(ix) {
                self.fitted
                    .insert(sized_key(self.view, &self.ids[ix]), f32::from(*w));
            }
        }
        self.relayout();
    }

    /// Install the sized widths (none for a view returns it to the
    /// prepared ones).
    pub(crate) fn set_fitted(&mut self, fitted: FittedWidths) {
        self.fitted = fitted;
        self.relayout();
    }

    /// Each column fitted to its head (with the sort mark when sortable)
    /// and widest cell in the real mono metrics, keyed for this view: a
    /// glyph column keeps the glyph's own minimum around its text. `None`
    /// while there are no rows to measure.
    pub(crate) fn fit_columns(&self, m: &FitMetrics) -> Option<FittedWidths> {
        if self.prepared.rows.is_empty() || self.columns.is_empty() {
            return None;
        }
        let rem = px(m.rem_px);
        // The glyph minima are design px over a design-rem text advance.
        let design_advance = m.advance_px * scale::DESIGN_REM / m.rem_px;
        Some(
            self.columns
                .iter()
                .enumerate()
                .map(|(ci, c)| {
                    let cells = self
                        .prepared
                        .rows
                        .iter()
                        .filter_map(|r| r.cells.get(ci))
                        .map(|cell| {
                            let text = cell.text.chars().count() as f32 * design_advance;
                            scale::design_px(glyph_width(&c.kind, text), rem)
                        });
                    let head = if sortable(&c.kind) {
                        m.text_px(&c.head) + sort_mark_px(m)
                    } else {
                        m.text_px(&c.head)
                    };
                    let width = m.fit(std::iter::once(head).chain(cells));
                    (sized_key(self.view, &c.id), width)
                })
                .collect(),
        )
    }

    /// Column `ix`'s width. `prepare` measures every column of the view,
    /// so a missing width is only the instant before the first model.
    pub(crate) fn width(&self, ix: usize) -> Pixels {
        self.widths.get(ix).copied().unwrap_or_default()
    }

    /// The fill under row `row_ix`, for a sparkline's ring.
    fn ring(&self, row_ix: usize) -> Hsla {
        let p = &self.paints;
        if self.cursor.is_some_and(|(r, _)| r == row_ix) {
            p.cursor_ring
        } else if self.selection.as_ref().is_some_and(|s| s.contains(&row_ix)) {
            p.selection_ring
        } else if STRIPE && !row_ix.is_multiple_of(2) {
            p.stripe_ring
        } else {
            p.plain_ring
        }
    }
}

/// One group per run of equal `group`s; an ungrouped column gets an empty
/// one. Empty when no column is grouped.
fn groups(columns: &[Column]) -> Vec<ColumnGroup> {
    if columns.iter().all(|c| c.group.is_none()) {
        return Vec::new();
    }
    let mut out: Vec<(Option<&str>, usize)> = Vec::new();
    for c in columns {
        let g = c.group.as_deref();
        match out.last_mut() {
            Some((last, n)) if g.is_some() && *last == g => *n += 1,
            _ => out.push((g, 1)),
        }
    }
    out.into_iter()
        .map(|(g, n)| ColumnGroup::new(SharedString::from(g.unwrap_or("").to_string()), n))
        .collect()
}

impl TableDelegate for GridDelegate {
    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .text_color(cx.theme().muted_foreground)
            .child(self.empty.clone())
    }

    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.visible.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> TableColumn {
        let Some(c) = self.columns.get(col_ix) else {
            return TableColumn::default();
        };
        let mut col = TableColumn::new(SharedString::from(c.id.clone()), c.head.clone())
            .width(self.width(col_ix))
            .resizable(true)
            .movable(false);
        if c.kind == ColumnKind::Underlying {
            col = col.fixed_left();
        } else {
            col = col.text_right();
        }
        if sortable(&c.kind) {
            col = col.sortable();
            match self.sorted {
                Some((ix, true)) if ix == col_ix => col = col.sort(ColumnSort::Descending),
                Some((ix, false)) if ix == col_ix => col = col.sort(ColumnSort::Ascending),
                _ => {}
            }
        }
        col
    }

    /// The tile owns the sort and its cycle; the table's proposal is
    /// ignored.
    fn perform_sort(
        &mut self,
        col_ix: usize,
        _proposed: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        cx.emit(GridEvent::HeadPressed(col_ix));
    }

    fn group_headers(&self, _cx: &App) -> Option<Vec<Vec<ColumnGroup>>> {
        (!self.groups.is_empty()).then(|| vec![self.groups.clone()])
    }

    /// A group's label centred over its columns in the heads' muted mono,
    /// a hairline closing each named group.
    fn render_group_th(
        &mut self,
        label: &SharedString,
        _col_span: usize,
        width: Pixels,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let selector = label.clone();
        div()
            .w(width)
            .h_full()
            .flex_shrink_0()
            .flex()
            .items_end()
            .justify_center()
            .px_1()
            .overflow_hidden()
            .whitespace_nowrap()
            // The table sizes its heads and cells (`table_cell_size`) but
            // hands a group head back unsized: at the window's default it
            // would paint larger than the head row under it.
            .text_size(rems(TABLE_TEXT_REM))
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .when(!label.is_empty(), |el| {
                el.border_r_1().border_color(theme.border)
            })
            .child(
                div()
                    .whitespace_nowrap()
                    .debug_selector(move || format!("volwatch-group-{selector}"))
                    .child(label.clone()),
            )
    }

    /// The head's text; a press on a sortable head walks its click cycle.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let Some(c) = self.columns.get(col_ix) else {
            return div();
        };
        let id = self.ids.get(col_ix).cloned().unwrap_or_default();
        let text_id = id.clone();
        let underlying = c.kind == ColumnKind::Underlying;
        div()
            .size_full()
            .flex()
            .items_center()
            .when(underlying, |el| el.justify_start())
            .when(!underlying, |el| el.justify_end())
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_color(theme.muted_foreground)
            .debug_selector(move || format!("volwatch-th-{id}"))
            .when(sortable(&c.kind), |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                        cx.emit(GridEvent::HeadPressed(col_ix));
                    }),
                )
            })
            // As a cell's text: a cut head reads `ATFM…`, not `:M 1M`.
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .when(!underlying, |el| el.text_right())
                    .truncate()
                    .text_ellipsis()
                    .debug_selector(move || format!("volwatch-th-text-{text_id}"))
                    .children(self.heads.get(col_ix).cloned()),
            )
    }

    /// Each row is identified by its member's name. A press with a
    /// command, control or alt modifier is the shell's gesture.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(row) = self
            .visible
            .get(row_ix)
            .and_then(|&i| self.prepared.rows.get(i))
        else {
            return div().id(("volwatch-row", row_ix));
        };
        let name = row.name.clone();
        let in_selection = self.selection.as_ref().is_some_and(|s| s.contains(&row_ix));
        let tint = selection_tint(cx.theme());
        div()
            .id(ElementId::Name(row.name.clone()))
            .debug_selector(move || format!("volwatch-row-{name}"))
            .when(in_selection, |el| el.bg(tint))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, e: &MouseDownEvent, _, cx| {
                    let m = e.modifiers;
                    if m.platform || m.control || m.alt {
                        return;
                    }
                    cx.emit(GridEvent::RowPressed {
                        row: row_ix,
                        shift: m.shift,
                    });
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, e: &MouseDownEvent, _, cx| {
                    cx.emit(GridEvent::RowContext {
                        row: row_ix,
                        position: e.position,
                    });
                }),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let Some(row) = self
            .visible
            .get(row_ix)
            .and_then(|&i| self.prepared.rows.get(i))
        else {
            return div();
        };
        let (Some(cell), Some(column)) = (row.cells.get(col_ix), self.columns.get(col_ix)) else {
            return div();
        };
        let at_cursor = self.cursor == Some((row_ix, col_ix));
        let muted = td_muted(row, cell, column);
        let tone = if muted {
            theme.muted_foreground
        } else {
            theme.foreground
        };
        let p = &self.paints;
        let text = cell.text.clone();
        let id = cell.id.clone();
        let cell_id = cell.id.clone();
        let underlying = column.kind == ColumnKind::Underlying;
        let content: AnyElement = match &cell.glyph {
            // Plain text fills the cell and aligns as text, so a column
            // narrower than its number cuts the trailing side into an
            // ellipsis; a flex-justified child would overflow on the
            // leading side and show a plausible wrong number.
            Glyph::None => div()
                .w_full()
                .min_w_0()
                .text_color(tone)
                .when(!underlying, |el| el.text_right())
                .truncate()
                .text_ellipsis()
                .debug_selector(move || format!("{id}-text"))
                .child(text)
                .into_any_element(),
            Glyph::Chip(chip) => {
                let paint = if muted { p.chip_muted } else { p.chip };
                delta_chip::paint(text, *chip, paint, id).into_any_element()
            }
            Glyph::Slider(slider) => {
                let paint = if muted { p.slider_muted } else { p.slider };
                percent_slider::paint(text, *slider, paint, id)
                    .w_full()
                    .into_any_element()
            }
            Glyph::Bar(bar) => {
                let paint = if muted { p.bar_muted } else { p.bar };
                zero_bar::paint(text, *bar, paint, id)
                    .w_full()
                    .into_any_element()
            }
            Glyph::Heat(alpha) => {
                let paint = if muted { p.heat_muted } else { p.heat };
                heat::paint(text, *alpha, paint, id)
                    .w_full()
                    .into_any_element()
            }
            Glyph::Spark {
                values,
                range,
                mark,
                end_dot,
            } => {
                let width = if column.kind == ColumnKind::Term {
                    sizes::WIDTH_TERM
                } else {
                    sizes::WIDTH_30D
                };
                let paint = SparkPaint {
                    ring: self.ring(row_ix),
                    ..p.spark
                };
                div()
                    .w(design(width))
                    .h(design(sizes::HEIGHT))
                    .child(
                        Spark::new(values.clone(), paint)
                            .range(*range)
                            .mark(*mark)
                            .end_dot(*end_dot),
                    )
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .when(underlying, |el| el.justify_start())
            .when(!underlying, |el| el.justify_end())
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_color(tone)
            .debug_selector(move || format!("volwatch-td-{cell_id}"))
            .when(at_cursor, |el| {
                el.border_1().border_color(theme.table_active_border)
            })
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::views::{columns, order_tenors};

    #[test]
    fn groups_are_runs_of_equal_column_groups() {
        let cols = columns(View::Changes, &order_tenors(None));
        let g = groups(&cols);
        assert_eq!(g.iter().map(|g| g.span).sum::<usize>(), cols.len());
        assert_eq!(g[0].label.as_ref(), "", "Underlying is ungrouped");
        assert_eq!((g[2].label.as_ref(), g[2].span), ("ATFM 1M", 2));
        assert!(groups(&columns(View::Levels, &order_tenors(None))).is_empty());
    }

    #[test]
    fn relayout_adds_the_tables_fixed_chrome_to_every_width() {
        let mut d = GridDelegate::new(Paints::from_theme(&Theme::default()));
        let cols = Rc::new(columns(View::Levels, &order_tenors(None)));
        let prepared = Rc::new(Prepared {
            rows: Vec::new(),
            widths: vec![30.0; cols.len()],
        });
        d.set_model(prepared, cols, None, View::Levels);
        d.set_rem(12.0);
        // The chrome as the registry states it, not as `chrome_px` says.
        let pad = Size::XSmall.table_cell_padding();
        let chrome = f32::from(pad.left) + f32::from(pad.right) + CURSOR_BORDER_PX;
        assert_eq!(chrome, 10.0, "4 px each side and a 2 px cursor border");
        assert_eq!(f32::from(d.width(0)), 30.0 + chrome);
        // A fitted width already holds the chrome: it is taken as is.
        d.set_fitted(FittedWidths::from([(
            sized_key(View::Levels, "underlying"),
            50.0,
        )]));
        assert_eq!(f32::from(d.width(0)), 50.0);
        assert_eq!(f32::from(d.width(1)), 30.0 + chrome);
    }
}
