//! The grid's `TableDelegate`, over rows the tile prepares from its
//! `GridModel` whenever the rows, the filter or the sort change. The tile
//! owns the truth (cursor, selection, sort); the delegate paints it and
//! reports a row press and a header sort click as events.

use std::ops::Range;
use std::rc::Rc;

use geode_shell::shell::{listrow, scale};
use geode_shell::{fonts, palette};
use gpui::prelude::*;
use gpui::{
    App, Context, Div, ElementId, Entity, MouseButton, MouseDownEvent, SharedString, Stateful,
    TextAlign, Window, div, px,
};
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, Theme, h_flex, v_flex};

use crate::core::grid::{GridModel, LABEL_COL, SOURCE_COL, label_text};
use crate::core::session::SortCol;
use crate::tile::editor::{self, EditorPaint};

/// What an unclassified label paints: the effective meaning, never the raw
/// (absent or blank) text.
pub(crate) const UNCLASSIFIED: &str = "\u{2014}";
/// The mark beside a label whose value is only in the map.
pub(crate) const NOT_IN_DATA: &str = "not in data";

/// The columns in table order, with their design-pixel widths.
const COLUMNS: [(SortCol, f32); 3] = [
    (SortCol::Source, 160.0),
    (SortCol::Label, 200.0),
    (SortCol::Rows, 80.0),
];

/// One painted row: prepared text and highlight ranges.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreparedRow {
    pub source: SharedString,
    /// Trimmed; `None` is unclassified.
    pub label: Option<SharedString>,
    /// The stored row count, blank when the value is not in the data.
    pub count: SharedString,
    pub in_data: bool,
    pub source_marks: Vec<Range<usize>>,
    pub label_marks: Vec<Range<usize>>,
}

/// The shown rows and the two text columns' headings: the source column
/// is headed by the column it maps, the label column by the
/// classification's name.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    pub rows: Vec<PreparedRow>,
    pub source_head: SharedString,
    pub label_head: SharedString,
}

impl Prepared {
    pub(crate) fn build(grid: &GridModel, from: &str, name: &str) -> Prepared {
        let rows = grid
            .visible()
            .iter()
            .enumerate()
            .map(|(at, &i)| {
                let row = grid.row(i);
                let marks = grid.marks(at);
                let take = |c: usize| marks.map(|m| m.get(c).to_vec()).unwrap_or_default();
                PreparedRow {
                    source: row.source.clone().into(),
                    label: label_text(row).map(|l| SharedString::from(l.to_string())),
                    count: row.count.map(|n| n.to_string()).unwrap_or_default().into(),
                    in_data: row.in_data,
                    source_marks: take(SOURCE_COL),
                    label_marks: take(LABEL_COL),
                }
            })
            .collect();
        Prepared {
            rows,
            source_head: SharedString::from(from.to_string()),
            label_head: SharedString::from(name.to_string()),
        }
    }
}

/// A press on shown row `row`: `shift` extends a selection; `clicks` is
/// the press's click count, so a double-click opens the label editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowPressed {
    pub row: usize,
    pub shift: bool,
    pub clicks: usize,
}

/// A header's sort control was pressed on this column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SortClicked(pub SortCol);

impl gpui::EventEmitter<RowPressed> for TableState<GridDelegate> {}
impl gpui::EventEmitter<SortClicked> for TableState<GridDelegate> {}

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
        .debug_selector(move || format!("classifications-table-{tile_id}"))
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
    /// The open label editor, painted in its row's label cell.
    editor: Option<EditorPaint>,
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
            editor: None,
        }
    }

    pub(crate) fn set_editor(&mut self, editor: Option<EditorPaint>) {
        self.editor = editor;
    }

    /// Install new rows; `true` when a column heading changed, which the
    /// table reads only on a refresh. The row count is read live.
    pub(crate) fn set(&mut self, prepared: Rc<Prepared>) -> bool {
        let headed = prepared.source_head != self.prepared.source_head
            || prepared.label_head != self.prepared.label_head;
        self.prepared = prepared;
        headed
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
            .debug_selector(|| "classifications-grid-empty".to_string())
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
        let name = match col {
            SortCol::Source => self.prepared.source_head.clone(),
            SortCol::Label => self.prepared.label_head.clone(),
            SortCol::Rows => SharedString::new_static("rows"),
        };
        Column {
            key: SharedString::new_static(col.name()),
            name,
            align: if col == SortCol::Rows {
                TextAlign::Right
            } else {
                TextAlign::Left
            },
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

    /// Each row is identified by its source value, and reports a press
    /// (with shift and the click count) to the tile's one pointer door. A
    /// press with a command, control or alt modifier is the shell's gesture
    /// and is left alone.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(row) = self.prepared.rows.get(row_ix) else {
            return div().id(("row", row_ix));
        };
        let source = row.source.clone();
        div()
            .id(ElementId::Name(row.source.clone()))
            .debug_selector(move || format!("classifications-row-{source}"))
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
                        clicks: e.click_count,
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
        let Some(row) = self.prepared.rows.get(row_ix) else {
            return div();
        };
        let tinted = self.selected.as_ref().is_some_and(|s| s.contains(&row_ix));
        let cell = div()
            .size_full()
            .flex()
            .items_center()
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .when(tinted, |el| el.relative().child(selection_tint(theme)));
        let muted = theme.muted_foreground;
        match COLUMNS[col_ix].0 {
            SortCol::Source => cell.text_color(theme.foreground).child(self.highlighted(
                &row.source,
                &row.source_marks,
                theme,
            )),
            SortCol::Label if let Some(e) = self.editor.as_ref().filter(|e| e.row == row_ix) => {
                cell.child(editor::render_cell(e, cx))
            }
            SortCol::Label => {
                let label = match &row.label {
                    Some(l) => div().text_color(theme.foreground).child(self.highlighted(
                        l,
                        &row.label_marks,
                        theme,
                    )),
                    None => div().text_color(muted).child(UNCLASSIFIED),
                };
                cell.child(h_flex().gap_2().child(label).when(!row.in_data, |el| {
                    el.child(div().text_xs().text_color(muted).child(NOT_IN_DATA))
                }))
            }
            SortCol::Rows => cell
                .justify_end()
                .text_color(theme.foreground)
                .child(row.count.clone()),
        }
    }
}
