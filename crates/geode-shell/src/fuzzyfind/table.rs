//! Native table geometry for the temporary result order. Cell formatting stays
//! with the tile; this delegate owns neither its model nor its tree cursor.
//!
//! The tile formats only the rows shown. This delegate reports them, as
//! source rows in display order, through the tile's `on_rows` callback: on
//! each range the table reports (`visible_rows_changed`), and after every
//! result-set change. The pinned gpui-component 0.6.2 table never reports
//! an unchanged range or one of length zero or one, so a new ranking under
//! the same range — or a narrowing to one match — would otherwise paint rows
//! the tile never prepared. The re-report runs at the next table layout (the
//! first `render_tr`), never inside the `FuzzyFind` update that changed the
//! results: that update can run inside the tile's own, and the callback may
//! read the tile.
use super::{FindRow, FuzzyFind, Items, Pick, Rows};
use crate::linenumbers::{UiSettings, gutter_number, gutter_px, window_gutter_px};
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _,
    table::{Column, TableDelegate, TableState},
};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

type HeaderRenderer = dyn Fn(usize, &mut Window, &mut App) -> AnyElement;

type CellRenderer = dyn Fn(&FindRow<'_>, usize, &mut App) -> AnyElement;

pub(super) type RowsReporter = dyn Fn(&[usize], &mut App);

/// Display rows reported before the table has reported any range, so the
/// opening frame paints: a result set of one row never gets a report.
pub(super) const FIRST_WINDOW: usize = 64;

/// The display range to report over `len` result rows: the range the table
/// last `asked` for (the first `0..FIRST_WINDOW` before any) clamped to
/// `len`; when nothing of it is left, the last rows of the same height (at
/// least one), which the table scrolls back onto without reporting. `None`
/// with no rows. The rule of `geode_tile::grid::WindowRequest::refill_range`,
/// restated because the shell does not depend on `geode-tile`.
pub(super) fn report_range(asked: Option<&Range<usize>>, len: usize) -> Option<Range<usize>> {
    if len == 0 {
        return None;
    }
    let asked = asked.cloned().unwrap_or(0..FIRST_WINDOW);
    let end = asked.end.min(len);
    if asked.start < end {
        return Some(asked.start..end);
    }
    Some(len.saturating_sub(asked.len().max(1))..len)
}

pub(super) struct FindTable {
    columns: Vec<Column>,
    render_header: Rc<HeaderRenderer>,
    render_cell: Rc<CellRenderer>,
    owner: WeakEntity<FuzzyFind>,
    highlights: HashMap<usize, Vec<usize>>,
    visible: Range<usize>,
    on_rows: Rc<RowsReporter>,
    /// The range the table last reported; `None` before its first report.
    asked: Option<Range<usize>>,
    /// The results changed since the last report: re-report at the next
    /// layout, whether or not the table reports a range.
    stale: bool,
    /// The last report's source rows, reused as the next report's buffer.
    reported: Vec<usize>,
    pub items: Items,
    pub ranked: Rows,
    pub message: String,
}

impl FindTable {
    pub fn clear_highlights(&mut self) {
        self.highlights.clear();
    }

    /// The results changed: report the shown rows at the next layout.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Tell the tile which source rows show now, in display order.
    fn report(&mut self, cx: &mut App) {
        self.stale = false;
        let Some(range) = report_range(self.asked.as_ref(), self.ranked.len()) else {
            return;
        };
        let mut rows = std::mem::take(&mut self.reported);
        rows.clear();
        rows.extend(range.filter_map(|position| self.ranked.get(position)));
        (self.on_rows)(&rows, cx);
        self.reported = rows;
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
        on_rows: Rc<RowsReporter>,
        owner: WeakEntity<FuzzyFind>,
    ) -> Self {
        Self {
            columns,
            render_header,
            render_cell,
            owner,
            highlights: HashMap::new(),
            visible: 0..0,
            on_rows,
            asked: None,
            stale: true,
            reported: Vec::new(),
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
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.visible = range.clone();
        self.asked = Some(range);
        self.report(cx);
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
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        // Layout time, before this frame's cells: the tile is not borrowed.
        if self.stale {
            self.report(cx);
        }
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

#[cfg(test)]
mod tests {
    use super::{FIRST_WINDOW, report_range};

    /// The re-report clamps the asked range to the rows that remain, and
    /// when none of it remains reports the tail of the same height — one
    /// row at least, the lone match the table itself never reports.
    #[test]
    fn a_re_report_clamps_the_asked_range_and_falls_back_to_the_tail() {
        assert_eq!(report_range(None, 0), None, "no rows");
        assert_eq!(report_range(None, 500), Some(0..FIRST_WINDOW));
        assert_eq!(report_range(None, 1), Some(0..1), "first window, one row");
        let asked = 40..60;
        assert_eq!(report_range(Some(&asked), 100), Some(40..60));
        assert_eq!(report_range(Some(&asked), 50), Some(40..50), "clamped");
        assert_eq!(
            report_range(Some(&asked), 30),
            Some(10..30),
            "the tail, asked height"
        );
        assert_eq!(report_range(Some(&asked), 40), Some(20..40), "start == len");
        assert_eq!(report_range(Some(&asked), 1), Some(0..1), "one match");
        assert_eq!(report_range(Some(&asked), 0), None);
        assert_eq!(report_range(Some(&(5..5)), 3), Some(2..3), "an empty ask");
    }
}
