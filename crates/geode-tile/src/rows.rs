//! The named-row grid model a list tile runs: its rows, the order a sort
//! puts them in, the rows the `/` filter keeps with their highlights, a
//! cursor and a row selection. The cursor and the selection's anchor are
//! held by name, so a rebuild (a new snapshot, a pending edit, a value
//! change that moves a row) keeps them on the rows they were on rather
//! than on whatever now paints at the old index.
//!
//! The order and the filtered rows are rebuilt only when the rows, the
//! filter or the sort change; a cursor move reads them.
//!
//! The row type is the module's own behind [`GridRow`]: a name (unique
//! within the rows: the cursor, the anchor and a seed find their row by
//! it), the columns `/` searches, whether the row is excluded (counted
//! apart and sorted last by default), and the order rows take with no sort
//! (`default_cmp`, by default the order they were given in).

use std::cmp::Ordering;
use std::fmt;
use std::ops::Range;
use std::rc::Rc;

use geode_core::grid::selection::{SelectKind, Selection};
use geode_shell::listfilter::{ColumnMarks, Narrow};

use crate::motion::{self, Motion};

/// A row the model holds.
pub trait GridRow {
    /// The row's identity: what the cursor, a selection's anchor and a
    /// seed are held by, and what [`RowModel::targets`] returns.
    fn name(&self) -> &str;
    /// The columns `/` searches, indexed by `ColumnMarks`.
    fn narrow_fields(&self) -> Vec<&str>;
    /// Counted apart and sorted last by default; still shown and still a
    /// cursor target. [`RowModel::counts`] reports it apart.
    fn excluded(&self) -> bool {
        false
    }
    /// The order rows take with no sort, and the tie-break under one:
    /// `Equal` keeps the rows' given order (a stable sort).
    fn default_cmp(_a: &Self, _b: &Self) -> Ordering {
        Ordering::Equal
    }
}

/// An order's comparator over two rows.
pub type Compare<R> = Rc<dyn Fn(&R, &R) -> Ordering>;

/// A sort the model keeps across rebuilds: a key the session saves and the
/// chip shows, and the comparator. Two orders are equal when their keys
/// are: the comparator is the key's meaning.
pub struct Order<R> {
    pub key: String,
    pub cmp: Compare<R>,
}

impl<R> Clone for Order<R> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            cmp: Rc::clone(&self.cmp),
        }
    }
}

impl<R> PartialEq for Order<R> {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key
    }
}

impl<R> fmt::Debug for Order<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Order").field(&self.key).finish()
    }
}

/// Rows held by name: their order, the `/` filter's kept rows, a cursor and a row selection.
#[derive(Debug)]
pub struct RowModel<R: GridRow> {
    rows: Vec<R>,
    /// Excluded rows, counted when the rows are set.
    excluded: usize,
    sort: Option<Order<R>>,
    /// Indices into `rows` in sort order.
    order: Vec<usize>,
    query: String,
    /// `order` narrowed by the filter, with each kept row's highlights.
    visible: Vec<usize>,
    marks: Vec<ColumnMarks>,
    /// Index into `visible`; `None` while nothing is shown.
    cursor: Option<usize>,
    /// The row under the cursor, by name. Kept while nothing is shown, so
    /// the cursor returns to its row when the rows (or a wider filter) do.
    cursor_name: Option<String>,
    /// A put cursor's row that the filter hides while it is still held:
    /// the cursor rests on the nearest shown row meanwhile, and returns
    /// here when a filter shows the row again (escape restoring the filter
    /// in force before a search). Dropped by a move, or when the rows no
    /// longer hold it.
    hidden: Option<String>,
    /// Whether the cursor was put on its row (a move, a press, a restored
    /// cursor) rather than resting where a rebuild left it. Only a put
    /// cursor follows its row: a resting one keeps its index, so a tile
    /// nobody has moved in keeps its cursor on the top row when a snapshot
    /// reorders the rows beneath it.
    put: bool,
    /// The row selection's anchor, by name; `None` with no selection.
    anchor: Option<String>,
    /// A restored cursor not yet placed: the snapshot that holds its row
    /// may still be on its way. It takes the cursor the first time a
    /// rebuild shows it, and is dropped by a move or by
    /// [`RowModel::forget_seed`].
    seed: Option<String>,
}

impl<R: GridRow> Default for RowModel<R> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            excluded: 0,
            sort: None,
            order: Vec::new(),
            query: String::new(),
            visible: Vec::new(),
            marks: Vec::new(),
            cursor: None,
            cursor_name: None,
            hidden: None,
            put: false,
            anchor: None,
            seed: None,
        }
    }
}

impl<R: GridRow> RowModel<R> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Place the cursor on `name` once rows holding it arrive: a restored
    /// session's cursor. Until then the cursor rests as it would without
    /// one.
    pub fn seed_cursor(&mut self, name: String) {
        self.seed = Some(name);
    }

    /// Stop waiting for a seeded cursor's row: the rows it could arrive in
    /// have arrived without it.
    pub fn forget_seed(&mut self) {
        self.seed = None;
    }

    /// The cursor a session saves: a seed still waiting, else a put
    /// cursor's row the filter hides (the row the trader chose, not the
    /// one the cursor rests on meanwhile), else the cursor's.
    pub fn saved_cursor(&self) -> Option<&str> {
        self.seed
            .as_deref()
            .or(self.hidden.as_deref())
            .or(self.cursor_name.as_deref())
    }

    /// Replace the rows. The cursor stays on its name, else falls to the
    /// nearest index; a selection stays while its anchor and the cursor's
    /// row are shown.
    pub fn set_rows(&mut self, rows: Vec<R>) {
        self.excluded = rows.iter().filter(|r| r.excluded()).count();
        self.rows = rows;
        self.reorder();
        self.refilter();
    }

    /// Replace the rows after a verb that changed them: the row it removed
    /// is gone or now sorts last. The cursor keeps its shown index rather
    /// than follow its row, so the trader working down the list finds the
    /// next one under the cursor. The cursor is put on whatever row now holds that index. A
    /// selection ends, as the verb consumed it, and a seed still waiting
    /// is dropped: the trader has acted where the cursor is.
    pub fn after_verb(&mut self, rows: Vec<R>) {
        let at = self.cursor;
        self.anchor = None;
        self.seed = None;
        self.hidden = None;
        self.set_rows(rows);
        if let Some(at) = at
            && !self.visible.is_empty()
        {
            self.place(at.min(self.visible.len() - 1));
        }
    }

    /// Narrow to rows whose [`GridRow::narrow_fields`] fuzzy-match
    /// `query`, in sort order. A selection ends: its span would cover rows
    /// it never did.
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

    /// Order by `sort`'s comparator, or `None` for the default order:
    /// excluded rows last, then [`GridRow::default_cmp`]. The sort is
    /// stable over the default order, so its ties fall to `default_cmp`
    /// (and, where that ties too, to the rows' given order). A selection
    /// ends, as for a filter.
    pub fn set_sort(&mut self, sort: Option<Order<R>>) {
        self.sort = sort;
        self.anchor = None;
        self.reorder();
        self.refilter();
    }

    pub fn sort(&self) -> Option<&Order<R>> {
        self.sort.as_ref()
    }

    /// The shown rows, as indices for [`RowModel::row`].
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    pub fn row(&self, i: usize) -> &R {
        &self.rows[i]
    }

    /// Every row, whatever the filter shows, in the order they were set.
    pub fn rows(&self) -> &[R] {
        &self.rows
    }

    /// The cursor's index into [`RowModel::visible`].
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn cursor_name(&self) -> Option<&str> {
        self.cursor_name.as_deref()
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
            self.anchor = self.cursor_name.clone();
        }
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub fn selecting(&self) -> bool {
        self.anchor.is_some()
    }

    /// The selected span of [`RowModel::visible`], anchor to cursor.
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

    /// The names a verb acts on: the selection's in shown order, else the
    /// cursor's, else none.
    pub fn targets(&self) -> Vec<String> {
        match (self.selected(), self.cursor) {
            (Some(span), _) => self.visible[span]
                .iter()
                .map(|&i| self.rows[i].name().to_string())
                .collect(),
            (None, Some(c)) => vec![self.rows[self.visible[c]].name().to_string()],
            (None, None) => Vec::new(),
        }
    }

    /// `(live, excluded)` over every row, whatever the filter shows.
    pub fn counts(&self) -> (usize, usize) {
        (self.rows.len() - self.excluded, self.excluded)
    }

    /// Shown row `visible_i`'s filter highlights, by column.
    pub fn marks(&self, visible_i: usize) -> Option<&ColumnMarks> {
        self.marks.get(visible_i)
    }

    /// Where `name` shows, if it does.
    fn position(&self, name: &str) -> Option<usize> {
        self.visible
            .iter()
            .position(|&i| self.rows[i].name() == name)
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
        self.cursor_name = Some(self.rows[self.visible[to]].name().to_string());
    }

    /// The default order: excluded rows last, then the row type's
    /// [`GridRow::default_cmp`]; a stable sort keeps the given order
    /// where both tie.
    fn default_order(a: &R, b: &R) -> Ordering {
        a.excluded()
            .cmp(&b.excluded())
            .then_with(|| R::default_cmp(a, b))
    }

    fn reorder(&mut self) {
        self.order.clear();
        self.order.extend(0..self.rows.len());
        let rows = &self.rows;
        self.order
            .sort_by(|&a, &b| Self::default_order(&rows[a], &rows[b]));
        let Some(order) = &self.sort else {
            return;
        };
        // Stable over the default order, which breaks every tie.
        self.order.sort_by(|&a, &b| (order.cmp)(&rows[a], &rows[b]));
    }

    /// Rebuild the shown rows from the order and the filter, then reseat
    /// the cursor on its name when shown, else at the nearest index. A
    /// selection whose anchor no longer shows ends.
    fn refilter(&mut self) {
        let mut narrow = Narrow::new(&self.query);
        self.visible.clear();
        self.marks.clear();
        for &i in &self.order {
            let columns = self.rows[i].narrow_fields();
            if let Some(m) = narrow.row(&columns) {
                self.visible.push(i);
                self.marks.push(m);
            }
        }
        // A selection whose anchor or cursor row is no longer shown ends:
        // the cursor would fall to a neighbour, and the span re-resolved
        // to it would cover rows the trader never chose.
        let lost = |s: Option<&str>| s.is_none_or(|s| self.position(s).is_none());
        if self.anchor.is_some()
            && (lost(self.anchor.as_deref()) || lost(self.cursor_name.as_deref()))
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
            .is_some_and(|h| !self.rows.iter().any(|r| r.name() == h))
        {
            self.hidden = None;
        }
        if let Some(at) = self.hidden.as_deref().and_then(|h| self.position(h)) {
            self.hidden = None;
            self.set_cursor(at);
            return;
        }
        let put = self.cursor_name.as_deref().filter(|_| self.put);
        match put.and_then(|s| self.position(s)) {
            Some(at) => self.cursor = Some(at),
            None => {
                // A put cursor's row still held but filtered out: remember
                // it, so a filter that shows it again returns the cursor.
                if self.hidden.is_none()
                    && let Some(s) = put
                    && self.rows.iter().any(|r| r.name() == s)
                {
                    self.hidden = Some(s.to_string());
                }
                self.set_cursor(self.cursor.map_or(0, |c| c.min(len - 1)));
            }
        }
    }
}

#[cfg(test)]
struct Row {
    name: &'static str,
    reference: Option<&'static str>,
}

#[cfg(test)]
impl GridRow for Row {
    fn name(&self) -> &str {
        self.name
    }
    fn narrow_fields(&self) -> Vec<&str> {
        vec![self.name, self.reference.unwrap_or("")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME_COL: usize = 0;
    const REFERENCE_COL: usize = 1;

    fn row(name: &'static str) -> Row {
        Row {
            name,
            reference: None,
        }
    }

    fn named(name: &'static str, reference: &'static str) -> Row {
        Row {
            name,
            reference: Some(reference),
        }
    }

    fn rows(names: &[&'static str]) -> Vec<Row> {
        names.iter().map(|&n| row(n)).collect()
    }

    /// By name, `desc` reversing it.
    fn by_name(desc: bool) -> Order<Row> {
        Order {
            key: format!("name {}", if desc { "desc" } else { "asc" }),
            cmp: Rc::new(move |a: &Row, b: &Row| {
                let o = a.name.cmp(b.name);
                if desc { o.reverse() } else { o }
            }),
        }
    }

    /// By reference name ascending; a missing one ties with another.
    fn by_reference_present() -> Order<Row> {
        Order {
            key: "reference".into(),
            cmp: Rc::new(|a: &Row, b: &Row| b.reference.is_some().cmp(&a.reference.is_some())),
        }
    }

    fn down(by: i64) -> Motion {
        Motion::Rows {
            by,
            counted: by != 1,
        }
    }

    fn names(g: &RowModel<Row>) -> Vec<&str> {
        g.visible().iter().map(|&i| g.row(i).name).collect()
    }

    /// With no `default_cmp` override and no sort, rows keep the order
    /// they were given in.
    #[test]
    fn the_default_order_keeps_the_given_order() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["SPX", "DAX", "NDX"]));
        assert_eq!(names(&g), ["SPX", "DAX", "NDX"]);
        let set: Vec<&str> = g.rows().iter().map(|r| r.name).collect();
        assert_eq!(set, ["SPX", "DAX", "NDX"]);
        assert_eq!(g.counts(), (3, 0), "no row is excluded by default");
    }

    /// A sort is stable over the default order: with the default hook,
    /// rows its comparator ties keep their given order.
    #[test]
    fn a_sorts_ties_keep_the_given_order_under_the_default_hook() {
        let mut g = RowModel::new();
        g.set_rows(vec![row("C"), named("B", "b"), row("A"), named("D", "d")]);
        g.set_sort(Some(by_reference_present()));
        assert_eq!(names(&g), ["B", "D", "C", "A"]);
        assert_eq!(g.sort().map(|o| o.key.as_str()), Some("reference"));
        g.set_sort(Some(by_name(true)));
        assert_eq!(names(&g), ["D", "C", "B", "A"]);
        g.set_sort(None);
        assert_eq!(names(&g), ["C", "B", "A", "D"]);
        assert!(g.sort().is_none());
    }

    #[test]
    fn orders_are_equal_by_key() {
        assert_eq!(by_name(true), by_name(true));
        assert_ne!(by_name(true), by_name(false));
    }

    #[test]
    fn the_cursor_follows_its_name_across_a_row_rebuild() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["A", "B"]));
        g.move_cursor(down(1), false);
        assert_eq!(g.cursor_name(), Some("B"));
        g.set_rows(rows(&["A", "AB", "B"]));
        assert_eq!(g.cursor_name(), Some("B"));
        assert_eq!(g.cursor(), Some(2));
    }

    /// A cursor nobody put anywhere keeps its index across a rebuild: the
    /// top row stays under it when a snapshot reorders the rows.
    #[test]
    fn a_resting_cursor_keeps_its_index() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["B"]));
        assert_eq!(g.cursor_name(), Some("B"));
        g.set_rows(rows(&["A", "B"]));
        assert_eq!(g.cursor_name(), Some("A"));
    }

    #[test]
    fn a_vanished_cursor_row_falls_to_the_nearest_index() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["A", "B", "C"]));
        g.move_cursor(Motion::Bottom(None), false);
        assert_eq!(g.cursor_name(), Some("C"));
        g.set_rows(rows(&["A", "B"]));
        assert_eq!(g.cursor(), Some(1));
        assert_eq!(g.cursor_name(), Some("B"));
    }

    #[test]
    fn the_filter_narrows_over_every_field_and_keeps_order() {
        let mut g = RowModel::new();
        g.set_rows(vec![
            named("SPX", "S&P 500"),
            named("DAX", "DAX 40"),
            row("NKY"),
        ]);
        g.set_filter("500");
        assert_eq!(names(&g), ["SPX"]);
        assert_eq!(g.query(), "500");
        assert_eq!(
            g.marks(0).map(|m| m.get(REFERENCE_COL).to_vec()),
            Some(vec![Range { start: 4, end: 7 }])
        );
        g.set_filter("nk");
        assert_eq!(names(&g), ["NKY"], "the name column is searched");
        assert_eq!(
            g.marks(0).map(|m| m.get(NAME_COL).to_vec()),
            Some(vec![Range { start: 0, end: 2 }])
        );
        g.set_filter("");
        assert_eq!(names(&g), ["SPX", "DAX", "NKY"]);
    }

    #[test]
    fn targets_are_the_selection_in_visible_order_else_the_cursor() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["A", "B", "C", "D"]));
        g.start_selection();
        g.move_cursor(down(2), true);
        assert_eq!(g.targets(), ["A", "B", "C"]);
        assert_eq!(g.selected(), Some(0..3));
        assert!(g.selecting());
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
        let mut g = RowModel::new();
        g.set_rows(rows(&["A", "B"]));
        g.move_cursor(down(1), true);
        assert_eq!(g.targets(), ["A", "B"]);
        g.move_cursor(down(1), false);
        assert_eq!(g.cursor_name(), Some("B"), "clamped, not wrapped");
        g.click(0, false);
        assert_eq!(g.selected(), None, "a plain click ends a selection");
        assert_eq!(g.cursor_name(), Some("A"));
        g.click(1, true);
        assert_eq!(g.targets(), ["A", "B"]);
    }

    #[test]
    fn a_sort_or_filter_ends_a_selection_and_a_rebuild_keeps_it() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["A", "B"]));
        g.start_selection();
        g.move_cursor(down(1), true);
        g.set_rows(rows(&["A", "B"]));
        assert_eq!(g.targets(), ["A", "B"], "same rows: the selection stays");
        g.set_rows(rows(&["B"]));
        assert_eq!(g.selected(), None, "its anchor is gone");
        g.set_rows(rows(&["A", "B"]));
        g.start_selection();
        g.set_sort(Some(by_name(true)));
        assert_eq!(g.selected(), None);
        g.start_selection();
        g.set_filter("a");
        assert_eq!(g.selected(), None);
    }

    #[test]
    fn a_seeded_cursor_lands_on_its_name_when_rows_arrive() {
        let mut g = RowModel::new();
        g.seed_cursor("C".into());
        assert_eq!(g.cursor(), None);
        assert_eq!(g.saved_cursor(), Some("C"));
        // The rows do not hold it yet: the cursor rests on the first row
        // and the seed waits.
        g.set_rows(rows(&["A"]));
        assert_eq!(g.cursor_name(), Some("A"));
        assert_eq!(g.saved_cursor(), Some("C"), "the session still names C");
        g.set_rows(rows(&["A", "C"]));
        assert_eq!(g.cursor(), Some(1));
        assert_eq!(g.saved_cursor(), Some("C"));
        // Placed once: a later rebuild follows the cursor, not the seed.
        g.move_cursor(Motion::Top(None), false);
        g.set_rows(rows(&["A", "C"]));
        assert_eq!(g.cursor_name(), Some("A"));
    }

    #[test]
    fn a_move_or_forget_drops_a_waiting_seed() {
        let mut g = RowModel::new();
        g.seed_cursor("C".into());
        g.set_rows(rows(&["A", "B"]));
        g.move_cursor(Motion::Bottom(None), false);
        g.set_rows(rows(&["B", "C"]));
        assert_eq!(g.cursor_name(), Some("B"), "the trader's move wins");
        let mut g = RowModel::new();
        g.seed_cursor("C".into());
        g.set_rows(rows(&["A"]));
        g.forget_seed();
        assert_eq!(g.saved_cursor(), Some("A"));
        g.set_rows(rows(&["A", "C"]));
        assert_eq!(g.cursor_name(), Some("A"));
    }

    #[test]
    fn the_filter_hiding_the_cursor_row_moves_the_cursor_to_a_shown_one() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["AB", "C"]));
        g.move_cursor(down(1), false);
        g.set_filter("a");
        assert_eq!(g.cursor_name(), Some("AB"));
        g.set_filter("zzz");
        assert_eq!(g.cursor(), None);
        assert!(g.targets().is_empty());
    }

    /// Starting a selection puts a resting cursor: otherwise the anchor
    /// follows its row across a rebuild while the cursor keeps its index,
    /// and the span silently grows.
    #[test]
    fn a_selection_from_a_resting_cursor_survives_a_reordering_rebuild() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["DAX", "SX5E"]));
        assert_eq!(g.cursor_name(), Some("DAX"), "resting on the top row");
        g.start_selection();
        assert_eq!(g.targets(), ["DAX"]);
        g.set_rows(rows(&["CAC", "DAX", "SX5E"]));
        assert_eq!(g.targets(), ["DAX"]);
        assert_eq!(g.cursor_name(), Some("DAX"));
    }

    /// A filter hiding a put cursor's row rests the cursor on a shown one;
    /// the filter that shows the row again (escape restoring the one in
    /// force) returns the cursor to it.
    #[test]
    fn a_filter_hiding_the_cursor_row_returns_it_when_the_row_shows_again() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["AB", "AC", "XY"]));
        g.move_cursor(Motion::Bottom(None), false);
        assert_eq!(g.cursor_name(), Some("XY"));
        g.set_filter("a");
        assert_eq!(g.cursor_name(), Some("AC"), "the nearest shown row");
        g.set_filter("ab");
        g.set_filter("");
        assert_eq!(g.cursor_name(), Some("XY"), "back on its own row");
        // A move while the row is hidden is the trader's new choice.
        g.set_filter("a");
        g.move_cursor(Motion::Top(None), false);
        g.set_filter("");
        assert_eq!(g.cursor_name(), Some("AB"));
        // A row the rows no longer hold is not waited for.
        g.move_cursor(Motion::Bottom(None), false);
        g.set_filter("a");
        g.set_rows(rows(&["AB", "AC"]));
        g.set_rows(rows(&["AB", "AC", "XY"]));
        g.set_filter("");
        assert_eq!(g.cursor_name(), Some("AC"));
    }

    /// After a verb the cursor keeps its shown index, whatever put it
    /// there, and is put on the row now holding it.
    #[test]
    fn a_verb_rebuild_keeps_the_cursor_index_even_when_put() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["A", "B", "C"]));
        g.move_cursor(down(1), false);
        assert_eq!(g.cursor_name(), Some("B"));
        g.after_verb(rows(&["A", "C"]));
        assert_eq!(g.cursor(), Some(1));
        assert_eq!(g.cursor_name(), Some("C"));
        g.set_rows(rows(&["A", "B", "C"]));
        assert_eq!(g.cursor_name(), Some("C"), "put: a rebuild follows C");
    }

    /// A rebuild that removes the cursor's row while a selection is live
    /// ends the selection: the cursor falls to a neighbour, and the span
    /// from the anchor to it would cover rows nobody chose.
    #[test]
    fn a_rebuild_removing_the_cursor_row_ends_a_selection() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["A", "B", "C"]));
        g.start_selection();
        g.move_cursor(down(1), true);
        assert_eq!(g.targets(), ["A", "B"]);
        g.set_rows(rows(&["A", "C"]));
        assert_eq!(g.selected(), None);
        assert_eq!(g.targets(), ["C"]);
    }

    /// The session saves a seed still waiting, then a put cursor's row the
    /// filter hides, then the cursor's own row.
    #[test]
    fn the_saved_cursor_prefers_seed_then_hidden_then_cursor() {
        let mut g = RowModel::new();
        g.set_rows(rows(&["AB", "XY"]));
        g.move_cursor(Motion::Bottom(None), false);
        g.set_filter("a");
        assert_eq!(g.cursor_name(), Some("AB"));
        assert_eq!(g.saved_cursor(), Some("XY"), "the hidden row");
        g.seed_cursor("Q".into());
        assert_eq!(g.saved_cursor(), Some("Q"), "a waiting seed first");
        g.forget_seed();
        g.set_filter("");
        assert_eq!(g.saved_cursor(), Some("XY"));
        g.move_cursor(Motion::Top(None), false);
        assert_eq!(g.saved_cursor(), Some("AB"));
    }

    #[test]
    fn an_empty_grid_has_no_cursor_and_no_targets() {
        let mut g = RowModel::<Row>::new();
        assert!(g.targets().is_empty());
        assert_eq!(g.cursor(), None);
        g.move_cursor(down(1), false);
        g.start_selection();
        assert!(g.targets().is_empty());
        assert_eq!(g.counts(), (0, 0));
    }
}
