//! The grid's pure model: one classification's rows (`classification::
//! rows`), the order a sort puts them in, the rows the `/` filter keeps, a
//! cursor and a row selection. The cursor and the selection's anchor are
//! held by source value, so a rebuild (new values, a reload, a label edit
//! that moves a row) keeps them on the rows they were on rather than on
//! whatever now paints at the old index.
//!
//! The order and the filtered rows are rebuilt only when the rows, the
//! filter or the sort change; a cursor move reads them.

use std::cmp::Ordering;
use std::ops::Range;

use geode_core::classification::ClassRow;
use geode_core::grid::selection::{SelectKind, Selection};
use geode_shell::listfilter::{ColumnMarks, Narrow};
use geode_tile::motion::{self, Motion};

pub use crate::core::session::SortCol;

/// The filter's columns: what `/` searches and what [`GridModel::marks`]
/// indexes. The row count is not searched.
pub const SOURCE_COL: usize = 0;
pub const LABEL_COL: usize = 1;

/// A row's label as the grid shows it: trimmed, `None` when absent or
/// blank. A hand-written `''` in the TOML means unclassified, and the grid
/// shows the effective meaning, not the raw text.
pub fn label_text(row: &ClassRow) -> Option<&str> {
    row.label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
}

/// Present keys in `dir`'s direction, then absent ones: a row a sort
/// cannot rank (unclassified, not in the data) never leads.
fn absent_last<T: Ord>(a: Option<T>, b: Option<T>, desc: bool) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) if desc => y.cmp(&x),
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[derive(Debug, Default)]
pub struct GridModel {
    rows: Vec<ClassRow>,
    /// Rows without a label, counted when the rows are set.
    unclassified: usize,
    sort: Option<(SortCol, bool)>,
    /// Indices into `rows` in sort order.
    order: Vec<usize>,
    query: String,
    /// `order` narrowed by the filter, with each kept row's highlights.
    visible: Vec<usize>,
    marks: Vec<ColumnMarks>,
    /// Index into `visible`; `None` while nothing is shown.
    cursor: Option<usize>,
    /// The row under the cursor, by identity. Kept while nothing is shown,
    /// so the cursor returns to its row when the rows (or a wider filter)
    /// do.
    cursor_source: Option<String>,
    /// A put cursor's row that the filter hides while it is still held:
    /// the cursor rests on the nearest shown row meanwhile, and returns
    /// here when a filter shows the row again (escape restoring the filter
    /// in force before a search). Dropped by a move, or when the rows no
    /// longer hold it.
    hidden: Option<String>,
    /// Whether the cursor was put on its row (a move, a press, a restored
    /// cursor) rather than resting where a rebuild left it. Only a put
    /// cursor follows its row: a resting one keeps its index, so a tile
    /// nobody has moved in keeps its cursor on the top row when the values
    /// arrive and reorder the map's rows beneath it.
    put: bool,
    /// The row selection's anchor, by identity; `None` with no selection.
    anchor: Option<String>,
    /// A restored cursor not yet placed: the rows that hold it may still
    /// be on their way (a value only the data holds is not in the map's
    /// rows). It takes the cursor the first time a rebuild shows it, and
    /// is dropped by a move or by [`GridModel::forget_seed`].
    seed: Option<String>,
}

impl GridModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Place the cursor on `source` once rows holding it arrive: a restored
    /// session's cursor. Until then the cursor rests as it would without
    /// one.
    pub fn seed_cursor(&mut self, source: String) {
        self.seed = Some(source);
    }

    /// Stop waiting for a seeded cursor's row: the rows it could arrive in
    /// have arrived without it.
    pub fn forget_seed(&mut self) {
        self.seed = None;
    }

    /// The cursor a session saves: a seed still waiting, else the cursor's.
    pub fn saved_cursor(&self) -> Option<&str> {
        self.seed.as_deref().or(self.cursor_source.as_deref())
    }

    /// Replace the rows. The cursor stays on its source value, else falls
    /// to the nearest index; a selection stays while its anchor is shown.
    pub fn set_rows(&mut self, rows: Vec<ClassRow>) {
        self.unclassified = rows.iter().filter(|r| label_text(r).is_none()).count();
        self.rows = rows;
        self.reorder();
        self.refilter();
    }

    /// Narrow to rows whose source or label fuzzy-match `query`, in sort
    /// order. A selection ends: its span would cover rows it never did.
    pub fn set_filter(&mut self, query: &str) {
        if self.query == query {
            return;
        }
        self.query = query.to_string();
        self.anchor = None;
        self.refilter();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Order by a column (`true` is descending), or `None` for the core
    /// order: unclassified first, then by label. Unclassified labels and
    /// values not in the data sort last in either direction; ties keep the
    /// core order. A selection ends, as for a filter.
    pub fn set_sort(&mut self, sort: Option<(SortCol, bool)>) {
        self.sort = sort;
        self.anchor = None;
        self.reorder();
        self.refilter();
    }

    pub fn sort(&self) -> Option<(SortCol, bool)> {
        self.sort
    }

    /// The shown rows, as indices for [`GridModel::row`].
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    pub fn row(&self, i: usize) -> &ClassRow {
        &self.rows[i]
    }

    /// The cursor's index into [`GridModel::visible`].
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn cursor_source(&self) -> Option<&str> {
        self.cursor_source.as_deref()
    }

    /// Move the cursor by `motion` over the shown rows. `selecting` extends
    /// a row selection, starting one at the cursor when none is live. While
    /// a selection is live a bare step clamps rather than wrap past the
    /// anchor.
    pub fn move_cursor(&mut self, motion: Motion, selecting: bool) {
        let len = self.visible.len();
        if len == 0 {
            return;
        }
        if selecting && self.anchor.is_none() {
            self.start_selection();
        }
        let at = self.cursor.unwrap_or(0);
        let to = motion::row(at, len, motion, self.anchor.is_some());
        self.place(to);
    }

    /// A press on shown row `i`: it takes the cursor; `extend` (shift)
    /// spans a selection to it from the anchor, or from the cursor when
    /// none is live, and a plain press ends one.
    pub fn click(&mut self, i: usize, extend: bool) {
        if i >= self.visible.len() {
            return;
        }
        if !extend {
            self.anchor = None;
        } else if self.anchor.is_none() {
            self.start_selection();
        }
        self.place(i);
    }

    /// Anchor a row selection at the cursor. The cursor is put on its row
    /// with it: the anchor follows its row across a rebuild, and a resting
    /// cursor keeping its index instead would silently widen or shift the
    /// span.
    pub fn start_selection(&mut self) {
        if let Some(at) = self.cursor {
            self.place(at);
            self.anchor = self.cursor_source.clone();
        }
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub fn selecting(&self) -> bool {
        self.anchor.is_some()
    }

    /// The selected span of [`GridModel::visible`], anchor to cursor.
    pub fn selected(&self) -> Option<Range<usize>> {
        let anchor = self.anchor.as_deref()?;
        let cursor = self.cursor?;
        let selection = Selection {
            kind: SelectKind::Rows,
            anchor_row: anchor,
            anchor_col: (),
        };
        selection
            .resolve_with((cursor, 0), 1, |a| self.position(a), |_| None)
            .ok()
            .map(|r| r.rows)
    }

    /// The source values a verb acts on: the selection's in shown order,
    /// else the cursor's, else none.
    pub fn targets(&self) -> Vec<String> {
        match (self.selected(), self.cursor) {
            (Some(span), _) => self.visible[span]
                .iter()
                .map(|&i| self.rows[i].source.clone())
                .collect(),
            (None, Some(c)) => vec![self.rows[self.visible[c]].source.clone()],
            (None, None) => Vec::new(),
        }
    }

    /// `(values, unclassified)` over every row, whatever the filter shows.
    pub fn counts(&self) -> (usize, usize) {
        (self.rows.len(), self.unclassified)
    }

    /// Shown row `visible_i`'s filter highlights, by column.
    pub fn marks(&self, visible_i: usize) -> Option<&ColumnMarks> {
        self.marks.get(visible_i)
    }

    /// Where `source` shows, if it does.
    fn position(&self, source: &str) -> Option<usize> {
        self.visible
            .iter()
            .position(|&i| self.rows[i].source == source)
    }

    /// Put the cursor on shown row `to`; a move by the trader, so a seed
    /// still waiting is dropped with it.
    fn place(&mut self, to: usize) {
        self.seed = None;
        self.hidden = None;
        self.put = true;
        self.set_cursor(to);
    }

    fn set_cursor(&mut self, to: usize) {
        self.cursor = Some(to);
        self.cursor_source = Some(self.rows[self.visible[to]].source.clone());
    }

    fn reorder(&mut self) {
        self.order.clear();
        self.order.extend(0..self.rows.len());
        let Some((col, desc)) = self.sort else {
            return;
        };
        let rows = &self.rows;
        // Stable over the core order, which breaks every tie.
        self.order.sort_by(|&a, &b| {
            let (a, b) = (&rows[a], &rows[b]);
            match col {
                SortCol::Source => absent_last(Some(&a.source), Some(&b.source), desc),
                SortCol::Label => absent_last(label_text(a), label_text(b), desc),
                SortCol::Rows => absent_last(a.count, b.count, desc),
            }
        });
    }

    /// Rebuild the shown rows from the order and the filter, then reseat
    /// the cursor on its source when shown, else at the nearest index. A
    /// selection whose anchor no longer shows ends.
    fn refilter(&mut self) {
        let mut narrow = Narrow::new(&self.query);
        self.visible.clear();
        self.marks.clear();
        for &i in &self.order {
            let row = &self.rows[i];
            let columns = [row.source.as_str(), label_text(row).unwrap_or("")];
            if let Some(m) = narrow.row(&columns) {
                self.visible.push(i);
                self.marks.push(m);
            }
        }
        if self
            .anchor
            .as_deref()
            .is_some_and(|a| self.position(a).is_none())
        {
            self.anchor = None;
        }
        let len = self.visible.len();
        if len == 0 {
            self.cursor = None;
            return;
        }
        if let Some(at) = self.seed.as_deref().and_then(|s| self.position(s)) {
            self.seed = None;
            self.put = true;
            self.set_cursor(at);
            return;
        }
        if self
            .hidden
            .as_deref()
            .is_some_and(|h| !self.rows.iter().any(|r| r.source == h))
        {
            self.hidden = None;
        }
        if let Some(at) = self.hidden.as_deref().and_then(|h| self.position(h)) {
            self.hidden = None;
            self.set_cursor(at);
            return;
        }
        let put = self.cursor_source.as_deref().filter(|_| self.put);
        match put.and_then(|s| self.position(s)) {
            Some(at) => self.cursor = Some(at),
            None => {
                // A put cursor's row still held but filtered out: remember
                // it, so a filter that shows it again returns the cursor.
                if self.hidden.is_none()
                    && let Some(s) = put
                    && self.rows.iter().any(|r| r.source == s)
                {
                    self.hidden = Some(s.to_string());
                }
                self.set_cursor(self.cursor.map_or(0, |c| c.min(len - 1)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str, l: Option<&str>, n: Option<u64>) -> ClassRow {
        ClassRow {
            source: s.into(),
            label: l.map(Into::into),
            count: n,
            in_data: n.is_some(),
        }
    }

    fn down(by: i64) -> Motion {
        Motion::Rows {
            by,
            counted: by != 1,
        }
    }

    fn sources(g: &GridModel) -> Vec<&str> {
        g.visible()
            .iter()
            .map(|&i| g.row(i).source.as_str())
            .collect()
    }

    #[test]
    fn the_cursor_follows_its_source_across_a_row_rebuild() {
        let mut g = GridModel::new();
        g.set_rows(vec![row("A", None, Some(3)), row("B", Some("X"), Some(1))]);
        g.move_cursor(down(1), false);
        assert_eq!(g.cursor_source(), Some("B"));
        g.set_rows(vec![
            row("B", Some("X"), Some(1)),
            row("Z", None, Some(9)),
            row("A", None, Some(3)),
        ]);
        assert_eq!(g.cursor_source(), Some("B"));
        assert_eq!(g.cursor(), Some(0));
    }

    /// A cursor nobody put anywhere keeps its index across a rebuild: the
    /// top row stays under it when the values arrive ahead of the map's
    /// rows.
    #[test]
    fn a_resting_cursor_keeps_its_index() {
        let mut g = GridModel::new();
        g.set_rows(vec![row("B", Some("X"), None)]);
        assert_eq!(g.cursor_source(), Some("B"));
        g.set_rows(vec![row("A", None, Some(3)), row("B", Some("X"), Some(1))]);
        assert_eq!(g.cursor_source(), Some("A"));
    }

    #[test]
    fn a_vanished_cursor_row_falls_to_the_nearest_index() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("A", None, Some(3)),
            row("B", None, Some(2)),
            row("C", None, Some(1)),
        ]);
        g.move_cursor(Motion::Bottom(None), false);
        assert_eq!(g.cursor_source(), Some("C"));
        g.set_rows(vec![row("A", None, Some(3)), row("B", None, Some(2))]);
        assert_eq!(g.cursor(), Some(1));
        assert_eq!(g.cursor_source(), Some("B"));
    }

    #[test]
    fn the_filter_narrows_over_source_and_label_and_keeps_order() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("AAPL", Some("Tech"), Some(1)),
            row("XOM", Some("Energy"), Some(1)),
            row("NKY", None, Some(1)),
        ]);
        g.set_filter("tec");
        assert_eq!(sources(&g), ["AAPL"]);
        assert_eq!(
            g.marks(0).map(|m| m.get(1).to_vec()),
            Some(vec![Range { start: 0, end: 3 }])
        );
        g.set_filter("x");
        assert_eq!(sources(&g), ["XOM"], "the source column is searched");
        g.set_filter("");
        assert_eq!(sources(&g), ["AAPL", "XOM", "NKY"]);
    }

    #[test]
    fn counts_ignore_the_filter_and_treat_a_blank_label_as_unclassified() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("A", None, Some(1)),
            row("B", Some("  "), Some(1)),
            row("C", Some("X"), Some(1)),
        ]);
        g.set_filter("C");
        assert_eq!(g.counts(), (3, 2));
    }

    #[test]
    fn sort_by_rows_descending_then_back_to_default() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("A", None, Some(2)),
            row("M", Some("X"), None),
            row("B", Some("X"), Some(9)),
            row("C", Some("Y"), Some(5)),
        ]);
        g.set_sort(Some((SortCol::Rows, true)));
        assert_eq!(sources(&g), ["B", "C", "A", "M"], "no count sorts last");
        g.set_sort(Some((SortCol::Rows, false)));
        assert_eq!(sources(&g), ["A", "C", "B", "M"], "last either way");
        g.set_sort(None);
        assert_eq!(sources(&g), ["A", "M", "B", "C"]);
    }

    #[test]
    fn sort_by_label_puts_unclassified_last_either_way() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("A", None, Some(2)),
            row("B", Some(" "), Some(1)),
            row("C", Some("Y"), Some(5)),
            row("D", Some("X"), Some(5)),
        ]);
        g.set_sort(Some((SortCol::Label, false)));
        assert_eq!(sources(&g), ["D", "C", "A", "B"]);
        g.set_sort(Some((SortCol::Label, true)));
        assert_eq!(sources(&g), ["C", "D", "A", "B"]);
        g.set_sort(Some((SortCol::Source, true)));
        assert_eq!(sources(&g), ["D", "C", "B", "A"]);
    }

    #[test]
    fn targets_are_the_selection_in_visible_order_else_the_cursor() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("A", None, Some(4)),
            row("B", None, Some(3)),
            row("C", None, Some(2)),
            row("D", None, Some(1)),
        ]);
        g.start_selection();
        g.move_cursor(down(2), true);
        assert_eq!(g.targets(), ["A", "B", "C"]);
        assert_eq!(g.selected(), Some(0..3));
        // Moving back over the anchor spans the other way, still in
        // visible order.
        g.move_cursor(Motion::Top(None), true);
        assert_eq!(g.targets(), ["A"]);
        g.move_cursor(Motion::Bottom(None), true);
        assert_eq!(g.targets(), ["A", "B", "C", "D"]);
        g.clear_selection();
        assert_eq!(g.targets(), ["D"]);
    }

    /// A selecting move with no selection live starts one at the cursor,
    /// as a shift-click does; a live selection clamps a bare step rather
    /// than wrap past the anchor.
    #[test]
    fn a_selecting_move_starts_a_selection_and_clamps() {
        let mut g = GridModel::new();
        g.set_rows(vec![row("A", None, Some(2)), row("B", None, Some(1))]);
        g.move_cursor(down(1), true);
        assert_eq!(g.targets(), ["A", "B"]);
        g.move_cursor(down(1), false);
        assert_eq!(g.cursor_source(), Some("B"), "clamped, not wrapped");
        g.click(0, false);
        assert_eq!(g.selected(), None, "a plain click ends a selection");
        assert_eq!(g.cursor_source(), Some("A"));
        g.click(1, true);
        assert_eq!(g.targets(), ["A", "B"]);
    }

    #[test]
    fn a_sort_or_filter_ends_a_selection_and_a_rebuild_keeps_it() {
        let mut g = GridModel::new();
        let rows = vec![row("A", None, Some(2)), row("B", None, Some(1))];
        g.set_rows(rows.clone());
        g.start_selection();
        g.move_cursor(down(1), true);
        g.set_rows(rows.clone());
        assert_eq!(g.targets(), ["A", "B"], "same rows: the selection stays");
        g.set_rows(vec![row("B", None, Some(1))]);
        assert_eq!(g.selected(), None, "its anchor is gone");
        g.set_rows(rows);
        g.start_selection();
        g.set_sort(Some((SortCol::Source, true)));
        assert_eq!(g.selected(), None);
        g.start_selection();
        g.set_filter("a");
        assert_eq!(g.selected(), None);
    }

    #[test]
    fn a_seeded_cursor_lands_on_its_source_when_rows_arrive() {
        let mut g = GridModel::new();
        g.seed_cursor("C".into());
        assert_eq!(g.cursor(), None);
        assert_eq!(g.saved_cursor(), Some("C"));
        // The map's rows alone do not hold it: the cursor rests on the
        // first row and the seed waits.
        g.set_rows(vec![row("A", None, Some(2))]);
        assert_eq!(g.cursor_source(), Some("A"));
        assert_eq!(g.saved_cursor(), Some("C"), "the session still names C");
        g.set_rows(vec![row("A", None, Some(2)), row("C", None, Some(1))]);
        assert_eq!(g.cursor(), Some(1));
        assert_eq!(g.saved_cursor(), Some("C"));
        // Placed once: a later rebuild follows the cursor, not the seed.
        g.move_cursor(Motion::Top(None), false);
        g.set_rows(vec![row("A", None, Some(2)), row("C", None, Some(1))]);
        assert_eq!(g.cursor_source(), Some("A"));
    }

    #[test]
    fn a_move_or_forget_drops_a_waiting_seed() {
        let mut g = GridModel::new();
        g.seed_cursor("C".into());
        g.set_rows(vec![row("A", None, Some(2)), row("B", None, Some(1))]);
        g.move_cursor(Motion::Bottom(None), false);
        g.set_rows(vec![row("B", None, Some(1)), row("C", None, Some(1))]);
        assert_eq!(g.cursor_source(), Some("B"), "the trader's move wins");
        let mut g = GridModel::new();
        g.seed_cursor("C".into());
        g.set_rows(vec![row("A", None, Some(2))]);
        g.forget_seed();
        assert_eq!(g.saved_cursor(), Some("A"));
        g.set_rows(vec![row("A", None, Some(2)), row("C", None, Some(1))]);
        assert_eq!(g.cursor_source(), Some("A"));
    }

    #[test]
    fn the_filter_hiding_the_cursor_row_moves_the_cursor_to_a_shown_one() {
        let mut g = GridModel::new();
        g.set_rows(vec![row("AB", None, Some(2)), row("C", None, Some(1))]);
        g.move_cursor(down(1), false);
        g.set_filter("a");
        assert_eq!(g.cursor_source(), Some("AB"));
        g.set_filter("zzz");
        assert_eq!(g.cursor(), None);
        assert!(g.targets().is_empty());
    }

    /// Starting a selection puts a resting cursor: otherwise the anchor
    /// follows its row across a rebuild while the cursor keeps its index,
    /// and the span silently grows.
    #[test]
    fn a_selection_from_a_resting_cursor_survives_a_reordering_rebuild() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("DAX", Some("E"), None),
            row("SX5E", Some("E"), None),
        ]);
        assert_eq!(g.cursor_source(), Some("DAX"), "resting on the top row");
        g.start_selection();
        assert_eq!(g.targets(), ["DAX"]);
        g.set_rows(vec![
            row("NKY", None, Some(7)),
            row("DAX", Some("E"), Some(5)),
            row("SX5E", Some("E"), Some(3)),
        ]);
        assert_eq!(g.targets(), ["DAX"]);
        assert_eq!(g.cursor_source(), Some("DAX"));
    }

    /// A filter hiding a put cursor's row rests the cursor on a shown one;
    /// the filter that shows the row again (escape restoring the one in
    /// force) returns the cursor to it.
    #[test]
    fn a_filter_hiding_the_cursor_row_returns_it_when_the_row_shows_again() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("AB", None, Some(3)),
            row("AC", None, Some(2)),
            row("XY", None, Some(1)),
        ]);
        g.move_cursor(Motion::Bottom(None), false);
        assert_eq!(g.cursor_source(), Some("XY"));
        g.set_filter("a");
        assert_eq!(g.cursor_source(), Some("AC"), "the nearest shown row");
        g.set_filter("ab");
        g.set_filter("");
        assert_eq!(g.cursor_source(), Some("XY"), "back on its own row");
        // A move while the row is hidden is the trader's new choice.
        g.set_filter("a");
        g.move_cursor(Motion::Top(None), false);
        g.set_filter("");
        assert_eq!(g.cursor_source(), Some("AB"));
        // A row the rows no longer hold is not waited for.
        g.move_cursor(Motion::Bottom(None), false);
        g.set_filter("a");
        g.set_rows(vec![row("AB", None, Some(3)), row("AC", None, Some(2))]);
        g.set_rows(vec![
            row("AB", None, Some(3)),
            row("AC", None, Some(2)),
            row("XY", None, Some(1)),
        ]);
        g.set_filter("");
        assert_eq!(g.cursor_source(), Some("AC"));
    }

    #[test]
    fn an_empty_grid_has_no_cursor_and_no_targets() {
        let mut g = GridModel::new();
        assert!(g.targets().is_empty());
        assert_eq!(g.cursor(), None);
        g.move_cursor(down(1), false);
        g.start_selection();
        assert!(g.targets().is_empty());
        assert_eq!(g.counts(), (0, 0));
    }
}
