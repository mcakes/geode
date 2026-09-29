//! Native table geometry for the temporary result order. Cell formatting stays
//! with the tile; this delegate owns neither its model nor its tree cursor.
use super::{FindRow, FuzzyFind, Items, Pick, Rows};
use crate::linenumbers::{UiSettings, gutter_number, gutter_px, window_gutter_px};
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _,
    table::{Column, TableDelegate, TableState},
};
use std::collections::HashMap;
use std::rc::Rc;

type HeaderRenderer = dyn Fn(usize, &mut Window, &mut App) -> AnyElement;

type CellRenderer = dyn Fn(&FindRow<'_>, usize, &mut App) -> AnyElement;

pub(super) struct FindTable {
    columns: Vec<Column>,
    render_header: Rc<HeaderRenderer>,
    render_cell: Rc<CellRenderer>,
    owner: WeakEntity<FuzzyFind>,
    highlights: HashMap<usize, Vec<usize>>,
    visible: std::ops::Range<usize>,
    pub items: Items,
    pub ranked: Rows,
    pub message: String,
}

impl FindTable {
    pub fn clear_highlights(&mut self) {
        self.highlights.clear();
    }

    pub fn set_presentation(
        &mut self,
        columns: Vec<Column>,
        render_header: Rc<HeaderRenderer>,
        render_cell: Rc<CellRenderer>,
    ) {
        self.columns = columns;
        self.render_header = render_header;
        self.render_cell = render_cell;
    }
    pub fn new(
        columns: Vec<Column>,
        render_header: Rc<HeaderRenderer>,
        render_cell: Rc<CellRenderer>,
        owner: WeakEntity<FuzzyFind>,
    ) -> Self {
        Self {
            columns,
            render_header,
            render_cell,
            owner,
            highlights: HashMap::new(),
            visible: 0..0,
            items: Items::default(),
            ranked: Rows::All(0),
            message: "Searching…".into(),
        }
    }
}

impl TableDelegate for FindTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.ranked.len()
    }
    fn visible_rows_changed(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.visible = range;
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        let mut column = self.columns[ix].clone();
        column.sort = None;
        column.movable = false;
        column.resizable = false;
        column
    }
    fn render_th(
        &mut self,
        col: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        (self.render_header)(col, window, cx)
    }
    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(source_row) = self.ranked.get(row) else {
            return div().id(("empty-find-row", row));
        };
        let owner = self.owner.clone();
        div()
            .id(self.items.id(source_row))
            .debug_selector(move || format!("find-result-{row}"))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                let _ = owner.update(cx, |state, cx| {
                    if !state.pending {
                        state.selected = row;
                        cx.emit(Pick);
                    }
                });
            })
    }
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(source_row) = self.ranked.get(row) else {
            return div().into_any_element();
        };
        // Cells share one alignment per visible row. Bound retained alignments
        // so scrolling through millions of matches cannot grow this cache.
        if self.highlights.len() >= 256 {
            self.highlights.clear();
        }
        let indices = self.highlights.entry(source_row).or_insert_with(|| {
            self.owner
                .upgrade()
                .map(|owner| owner.read(cx).indices(source_row))
                .unwrap_or_default()
        });
        let Some(owner) = self.owner.upgrade() else {
            return div().into_any_element();
        };
        let state = owner.read(cx);
        let mode = cx
            .try_global::<UiSettings>()
            .copied()
            .unwrap_or_default()
            .line_numbers;
        let paint = FindRow {
            source: source_row,
            position: row,
            indices,
            number: gutter_number(mode, row, state.selected),
            gutter_width: gutter_px(mode, state.ranked.len()),
            visible_gutter_width: window_gutter_px(
                mode,
                self.visible.start.min(row)..self.visible.end.min(state.ranked.len()).max(row + 1),
                state.selected,
            ),
            selected: row == state.selected,
            expanded: state
                .tree
                .as_ref()
                .filter(|tree| tree.branch(source_row))
                .map(|_| !state.folded.contains(&source_row)),
            context: state.hits.as_ref().is_some_and(|hits| !hits[source_row]),
            owner: self.owner.clone(),
        };
        div()
            .size_full()
            .debug_selector(move || format!("find-cell-{row}-{col}"))
            .child((self.render_cell)(&paint, col, cx))
            .into_any_element()
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_2()
            .text_color(cx.theme().muted_foreground)
            .child(self.message.clone())
    }
}
