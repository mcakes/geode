//! The grid's `TableDelegate`, over rows the tile prepares from its
//! `GridModel` whenever the rows, the filter or the sort change. The tile
//! owns the truth (cursor, selection, sort); the delegate paints it and
//! reports a row press, a right press and a header sort click as events.

use std::ops::Range;
use std::rc::Rc;

use geode_shell::shell::{listrow, scale};
use geode_shell::{fonts, palette};
use gpui::prelude::*;
use gpui::{
    App, Context, Div, ElementId, Entity, MouseButton, MouseDownEvent, Pixels, Point, SharedString,
    Stateful, Window, div, px,
};
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, Theme, h_flex, v_flex};

use crate::core::grid::{GridModel, NAME_COL, REFERENCE_COL};
use crate::core::rows::origin_text;
use crate::core::session::SortCol;

/// The mark beside a name the reference table does not hold.
pub(crate) const NOT_IN_REFERENCE: &str = "not in reference";

/// The columns in table order, with their design-pixel widths.
const COLUMNS: [(SortCol, f32); 3] = [
    (SortCol::Name, 120.0),
    (SortCol::Reference, 200.0),
    (SortCol::Origin, 160.0),
];

/// One painted row: prepared text and highlight ranges.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreparedRow {
    pub name: SharedString,
    /// The reference table's name; `None` paints blank.
    pub reference: Option<SharedString>,
    pub in_reference: bool,
    pub origin: SharedString,
    /// Painted in the muted tone throughout.
    pub excluded: bool,
    pub name_marks: Vec<Range<usize>>,
    pub reference_marks: Vec<Range<usize>>,
}

/// The shown rows, in painted order.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    pub rows: Vec<PreparedRow>,
}

impl Prepared {
    pub(crate) fn build(grid: &GridModel) -> Prepared {
        let rows = grid
            .visible()
            .iter()
            .enumerate()
            .map(|(at, &i)| {
                let row = grid.row(i);
                let marks = grid.marks(at);
                let take = |c: usize| marks.map(|m| m.get(c).to_vec()).unwrap_or_default();
                PreparedRow {
                    name: row.name.clone().into(),
                    reference: row.reference.clone().map(SharedString::from),
                    in_reference: row.in_reference,
                    origin: origin_text(&row.origin, row.pending).into(),
                    excluded: row.is_excluded(),
                    name_marks: take(NAME_COL),
                    reference_marks: take(REFERENCE_COL),
                }
            })
            .collect();
        Prepared { rows }
    }
}

/// A press on shown row `row`: `shift` extends a selection. The click
/// count is not reported: a double-click on a members row does nothing
/// more than the press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowPressed {
    pub row: usize,
    pub shift: bool,
}

/// A right press on shown row `row`, at `position` in window coordinates:
/// the `⋯` menu opens there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RowContext {
    pub row: usize,
    pub position: Point<Pixels>,
}

/// A header's sort control was pressed on this column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SortClicked(pub SortCol);

impl gpui::EventEmitter<RowPressed> for TableState<GridDelegate> {}
impl gpui::EventEmitter<SortClicked> for TableState<GridDelegate> {}
impl gpui::EventEmitter<RowContext> for TableState<GridDelegate> {}

/// The table inside a wrapper that keeps window focus where the shell put
/// it. `DataTable`'s root tracks its own focus handle, so a row press
/// would otherwise move focus into the table, whose own keys (escape
/// clears its selected row) would then shadow the tile's. The capture-phase
/// `prevent_default` suppresses that focus transfer; the row's press and
/// click still fire, and the shell's click-to-focus still runs. Without
/// `min_h_0` the virtualised list would take its intrinsic height and paint
/// no rows.
pub(crate) fn table_el(state: &Entity<TableState<GridDelegate>>, tile_id: u64) -> Div {
    div()
        .debug_selector(move || format!("watchlist-table-{tile_id}"))
        .flex_1()
        .min_h_0()
        .w_full()
        .capture_any_mouse_down(|_event, window, _cx| window.prevent_default())
        .child(
            DataTable::new(state)
                .with_size(Size::XSmall)
                .bordered(false)
                .stripe(false),
        )
}

pub(crate) struct GridDelegate {
    prepared: Rc<Prepared>,
    /// The selected span of shown rows; painted as a tint under the text.
    selected: Option<Range<usize>>,
    sort: Option<(SortCol, bool)>,
    rem_px: f32,
    /// Widths the trader dragged a column to, in pixels; `None` keeps the
    /// rem-scaled default. A `TableState::refresh` re-reads `column()`, so
    /// a dragged width lives here or the next refresh undoes the drag.
    widths: [Option<f32>; COLUMNS.len()],
    empty_title: SharedString,
    empty_help: SharedString,
    accent: listrow::TableAccent,
}

impl GridDelegate {
    pub(crate) fn new() -> GridDelegate {
        GridDelegate {
            prepared: Rc::default(),
            selected: None,
            sort: None,
            rem_px: scale::DESIGN_REM,
            widths: [None; COLUMNS.len()],
            empty_title: SharedString::default(),
            empty_help: SharedString::default(),
            accent: listrow::TableAccent::default(),
        }
    }

    /// Install new rows. The headings are fixed, so no refresh is needed:
    /// the row count is read live.
    pub(crate) fn set(&mut self, prepared: Rc<Prepared>) {
        self.prepared = prepared;
    }

    pub(crate) fn set_selected(&mut self, selected: Option<Range<usize>>) {
        self.selected = selected;
    }

    /// `true` when the sort changed: the headers' sort marks are read only
    /// on a refresh.
    pub(crate) fn set_sort(&mut self, sort: Option<(SortCol, bool)>) -> bool {
        std::mem::replace(&mut self.sort, sort) != sort
    }

    /// Record the widths a header drag reports (every column's, in table
    /// order). Only a column whose width differs from what `column()` gives
    /// is recorded, so an untouched column keeps following the rem.
    pub(crate) fn record_widths(&mut self, widths: &[gpui::Pixels]) {
        for (ix, width) in widths.iter().enumerate().take(COLUMNS.len()) {
            let width = f32::from(*width);
            if width != self.width(ix) {
                self.widths[ix] = Some(width);
            }
        }
    }

    /// Column `ix`'s width in pixels: a dragged one, else the default at
    /// the window's rem.
    pub(crate) fn width(&self, ix: usize) -> f32 {
        self.widths[ix].unwrap_or_else(|| scale::design_px(COLUMNS[ix].1, px(self.rem_px)))
    }

    pub(crate) fn set_empty(&mut self, title: SharedString, help: SharedString) {
        self.empty_title = title;
        self.empty_help = help;
    }

    /// Column widths are design pixels scaled to the window's rem, which
    /// `column()` cannot read itself.
    pub(crate) fn set_rem(&mut self, rem_px: f32) {
        self.rem_px = rem_px;
    }

    #[cfg(test)]
    pub(crate) fn prepared(&self) -> &Rc<Prepared> {
        &self.prepared
    }

    fn highlighted(
        &self,
        text: &SharedString,
        marks: &[Range<usize>],
        theme: &Theme,
    ) -> gpui::AnyElement {
        if marks.is_empty() {
            text.clone().into_any_element()
        } else {
            palette::highlighted_runs(text, marks, self.accent.get(theme)).into_any_element()
        }
    }
}

/// The selection tint: an absolute overlay painted as a cell's first
/// child, so it sits under the text and the table's cursor ground still
/// shows through (the pricer's treatment).
fn selection_tint(theme: &Theme) -> Div {
    div().absolute().inset_0().bg(theme.selection.opacity(0.35))
}

impl TableDelegate for GridDelegate {
    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .debug_selector(|| "watchlist-grid-empty".to_string())
            .child(div().text_sm().child(self.empty_title.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.empty_help.clone()),
            )
    }

    fn columns_count(&self, _cx: &App) -> usize {
        COLUMNS.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.prepared.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let (col, _) = COLUMNS[col_ix];
        Column {
            key: SharedString::new_static(col.name()),
            name: SharedString::new_static(col.name()),
            sort: Some(match self.sort {
                Some((c, true)) if c == col => ColumnSort::Descending,
                Some((c, false)) if c == col => ColumnSort::Ascending,
                _ => ColumnSort::Default,
            }),
            width: px(self.width(col_ix)),
            fixed: None,
            movable: false,
            resizable: true,
            ..Column::default()
        }
    }

    /// The tile owns the sort and its cycle; the table's own proposed
    /// order is ignored.
    fn perform_sort(
        &mut self,
        col_ix: usize,
        _proposed: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if let Some((col, _)) = COLUMNS.get(col_ix) {
            cx.emit(SortClicked(*col));
        }
    }

    /// Each row is identified by its name, and reports a press (with
    /// shift) to the tile's one pointer door. A press with a command,
    /// control or alt modifier is the shell's gesture and is left alone. A
    /// right press reports [`RowContext`] at the pointer, for the `⋯`
    /// menu.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(row) = self.prepared.rows.get(row_ix) else {
            return div().id(("row", row_ix));
        };
        let name = row.name.clone();
        div()
            .id(ElementId::Name(row.name.clone()))
            .debug_selector(move || format!("watchlist-row-{name}"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, e: &MouseDownEvent, _, cx| {
                    let m = e.modifiers;
                    if m.platform || m.control || m.alt {
                        return;
                    }
                    cx.emit(RowPressed {
                        row: row_ix,
                        shift: m.shift,
                    });
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, e: &MouseDownEvent, _, cx| {
                    cx.emit(RowContext {
                        row: row_ix,
                        position: e.position,
                    });
                }),
            )
    }

    /// An excluded row paints in the muted tone throughout; so do the
    /// origin, a `not in reference` mark and an absent reference name.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let Some(row) = self.prepared.rows.get(row_ix) else {
            return div();
        };
        let tinted = self.selected.as_ref().is_some_and(|s| s.contains(&row_ix));
        let muted = theme.muted_foreground;
        let tone = if row.excluded {
            muted
        } else {
            theme.foreground
        };
        let cell = div()
            .size_full()
            .flex()
            .items_center()
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .text_color(tone)
            .when(tinted, |el| el.relative().child(selection_tint(theme)));
        match COLUMNS[col_ix].0 {
            SortCol::Name => cell.child(
                h_flex()
                    .gap_2()
                    .child(self.highlighted(&row.name, &row.name_marks, theme))
                    .when(!row.in_reference, |el| {
                        el.child(div().text_xs().text_color(muted).child(NOT_IN_REFERENCE))
                    }),
            ),
            SortCol::Reference => match &row.reference {
                Some(r) => cell.child(self.highlighted(r, &row.reference_marks, theme)),
                None => cell,
            },
            SortCol::Origin => cell.text_color(muted).child(row.origin.clone()),
        }
    }
}
