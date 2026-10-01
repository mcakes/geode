//! The cells a grid table shows, prepared outside render.
//!
//! A module fills a [`WindowCache`] for exactly the rows its table reports
//! (`TableDelegate::visible_rows_changed`) and again after an invalidation;
//! `render_td` only reads it. Cells are one row-major `Vec` with a stride,
//! so a row is a slice rather than an object, and a window move keeps every
//! row still in view. [`WindowRequest`] remembers the range the table last
//! asked for: the pinned gpui-component 0.6.2 table reports a range only
//! when it changes and never one of length zero or one, so after an
//! invalidation the module refills the recorded range itself. A fuzzy
//! `/` result table shows a ranking, not a range: its module keeps a
//! [`RowCache`] of the rows the find table reports instead.
//!
//! Pure: no element, entity or window. The cell type stays each module's
//! own; colors stay resolved at paint, so a theme change never invalidates.

use std::collections::HashMap;
use std::ops::Range;

/// Rows a window prepares before its table has reported any range. A grid
/// of one row never gets a report (the pinned table skips ranges of length
/// one), so without this its only row would paint blank. The first report
/// drops whatever it does not cover.
pub const FIRST_WINDOW: usize = 64;

/// Prepared cells for rows `start..start + rows`, `cols` to a row. A `None`
/// cell is blank; a cell outside the window reads as `None` too.
#[derive(Debug, Clone)]
pub struct WindowCache<C> {
    start: usize,
    rows: usize,
    cols: usize,
    cells: Vec<Option<C>>,
    /// The previous window's buffer, reused by the next move, so scrolling
    /// allocates nothing once the window has reached its size.
    spare: Vec<Option<C>>,
}

impl<C> Default for WindowCache<C> {
    fn default() -> Self {
        WindowCache {
            start: 0,
            rows: 0,
            cols: 0,
            cells: Vec::new(),
            spare: Vec::new(),
        }
    }
}

impl<C> WindowCache<C> {
    /// The rows prepared now.
    pub fn window(&self) -> Range<usize> {
        self.start..self.start + self.rows
    }

    /// Drop every prepared cell; the next `set_window` fills its whole range.
    pub fn clear(&mut self) {
        self.cells.clear();
        self.rows = 0;
    }

    /// Move the window to `range`, `cols` cells a row. Rows already prepared
    /// and still in `range` are moved, not refilled: `fill(row, col)` runs
    /// only for rows entering it. A different `cols` clears the cache first,
    /// so a cell prepared for another column layout is never shown.
    ///
    /// `fill` runs for every entering row in `range`, even past the
    /// document's end: the cache does not know the document length, so the
    /// caller clamps the range, through [`WindowRequest::refill_range`] or by
    /// bounding what its table reports.
    pub fn set_window(
        &mut self,
        range: Range<usize>,
        cols: usize,
        mut fill: impl FnMut(usize, usize) -> Option<C>,
    ) {
        if cols != self.cols {
            self.clear();
            self.cols = cols;
        }
        let old = self.window();
        let mut next = std::mem::take(&mut self.spare);
        next.clear();
        next.reserve(range.len() * cols);
        for r in range.clone() {
            if old.contains(&r) {
                let at = (r - old.start) * cols;
                next.extend(self.cells[at..at + cols].iter_mut().map(Option::take));
            } else {
                next.extend((0..cols).map(|c| fill(r, c)));
            }
        }
        self.start = range.start;
        self.rows = range.len();
        self.spare = std::mem::replace(&mut self.cells, next);
        self.spare.clear();
    }

    /// The prepared cell at (`row`, `col`); `None` for a blank cell and for
    /// any cell outside the window.
    pub fn get(&self, row: usize, col: usize) -> Option<&C> {
        if col >= self.cols {
            return None;
        }
        let i = row.checked_sub(self.start).filter(|&i| i < self.rows)?;
        self.cells.get(i * self.cols + col)?.as_ref()
    }

    /// Re-prepare one cell in place, leaving every other cell as it is. A
    /// cell outside the window is not prepared: `fill` does not run.
    pub fn refill_cell(&mut self, row: usize, col: usize, fill: impl FnOnce() -> Option<C>) {
        if col >= self.cols {
            return;
        }
        let Some(i) = row.checked_sub(self.start).filter(|&i| i < self.rows) else {
            return;
        };
        self.cells[i * self.cols + col] = fill();
    }
}

/// Prepared cells for a set of rows that need not be contiguous, keyed by
/// document row: what a fuzzy `/` result table shows, whose rows are a
/// ranking rather than a range. [`RowCache::set_rows`] keeps the rows still
/// reported, prepares the rows entering and drops the rest, so the cache
/// holds one screenful however far the results scroll. A row it does not
/// hold reads as `None`, which paints blank.
#[derive(Debug, Clone)]
pub struct RowCache<C> {
    cols: usize,
    rows: HashMap<usize, Box<[Option<C>]>>,
    /// The previous report's map, reused by the next, so a steady scroll
    /// allocates no map once the report has reached its size.
    spare: HashMap<usize, Box<[Option<C>]>>,
}

impl<C> Default for RowCache<C> {
    fn default() -> Self {
        RowCache {
            cols: 0,
            rows: HashMap::new(),
            spare: HashMap::new(),
        }
    }
}

impl<C> RowCache<C> {
    /// Hold exactly `rows`, `cols` cells a row. A row already held keeps
    /// its cells; `fill(row, col)` runs only for rows entering. A different
    /// `cols` clears the cache first, so a cell prepared for another column
    /// layout is never shown. A row repeated in `rows` is prepared once.
    pub fn set_rows(
        &mut self,
        rows: &[usize],
        cols: usize,
        mut fill: impl FnMut(usize, usize) -> Option<C>,
    ) {
        if cols != self.cols {
            self.rows.clear();
            self.cols = cols;
        }
        let mut next = std::mem::take(&mut self.spare);
        next.clear();
        for &r in rows {
            if next.contains_key(&r) {
                continue;
            }
            let cells = self
                .rows
                .remove(&r)
                .unwrap_or_else(|| (0..cols).map(|c| fill(r, c)).collect());
            next.insert(r, cells);
        }
        self.spare = std::mem::replace(&mut self.rows, next);
        self.spare.clear();
    }

    /// The prepared cell at (`row`, `col`); `None` for a blank cell and for
    /// any row the last report did not hold.
    pub fn get(&self, row: usize, col: usize) -> Option<&C> {
        self.rows.get(&row)?.get(col)?.as_ref()
    }

    /// Whether `row` is held.
    pub fn contains(&self, row: usize) -> bool {
        self.rows.contains_key(&row)
    }

    /// How many rows are held.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Drop every row.
    pub fn clear(&mut self) {
        self.rows.clear();
    }
}

/// The last range a table reported, kept across invalidations so the module
/// can refill it when the table reports nothing new.
#[derive(Debug, Clone, Default)]
pub struct WindowRequest {
    asked: Option<Range<usize>>,
    /// Rows to prepare before any report ([`FIRST_WINDOW`]); 0 for none.
    first: usize,
}

impl WindowRequest {
    /// A request that prepares `0..first` until the table first reports.
    pub fn with_first(first: usize) -> Self {
        WindowRequest { asked: None, first }
    }

    /// The table asked for `range`.
    pub fn record(&mut self, range: Range<usize>) {
        self.asked = Some(range);
    }

    /// The range to refill after an invalidation: the recorded range (or the
    /// first window before any report) clamped to the `len` rows that exist
    /// now. When a shrink leaves no asked row, the last rows of the same
    /// height instead: the table scrolls back onto them, but never reports a
    /// range of one row, so a window left empty would paint a lone row blank
    /// until something else refilled it. `None` with no rows, and before any
    /// report when there is no first window. The record itself is kept, so a
    /// shrink followed by a regrowth refills the whole range again.
    pub fn refill_range(&self, len: usize) -> Option<Range<usize>> {
        if len == 0 || (self.asked.is_none() && self.first == 0) {
            return None;
        }
        let asked = self.asked.clone().unwrap_or(0..self.first);
        let end = asked.end.min(len);
        if asked.start < end {
            return Some(asked.start..end);
        }
        Some(len.saturating_sub(asked.len().max(1))..len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn text(r: usize, c: usize) -> Option<String> {
        Some(format!("r{r}c{c}"))
    }

    #[test]
    fn a_move_keeps_the_overlap_and_fills_only_entering_rows() {
        let mut w = WindowCache::default();
        let fills = Cell::new(0);
        let fill = |r: usize, c: usize| {
            fills.set(fills.get() + 1);
            text(r, c)
        };
        w.set_window(0..10, 2, fill);
        assert_eq!(fills.get(), 20);
        w.set_window(5..15, 2, fill);
        assert_eq!(fills.get(), 30, "five rows entered, two columns each");
        assert_eq!(w.window(), 5..15);
        assert_eq!(w.get(5, 1).map(String::as_str), Some("r5c1"), "kept");
        assert_eq!(w.get(14, 0).map(String::as_str), Some("r14c0"), "entered");
        w.set_window(0..8, 2, fill);
        assert_eq!(fills.get(), 40, "rows 0..5 entered, 5..8 kept");
        assert_eq!(w.get(7, 1).map(String::as_str), Some("r7c1"));
    }

    #[test]
    fn the_same_range_again_fills_nothing() {
        let mut w = WindowCache::default();
        w.set_window(3..9, 2, text);
        let fills = Cell::new(0);
        w.set_window(3..9, 2, |r, c| {
            fills.set(fills.get() + 1);
            text(r, c)
        });
        assert_eq!(fills.get(), 0);
        assert_eq!(w.get(8, 1).map(String::as_str), Some("r8c1"), "kept");
    }

    #[test]
    fn a_disjoint_move_fills_every_row_and_keeps_none() {
        let mut w = WindowCache::default();
        w.set_window(0..5, 2, text);
        let fills = Cell::new(0);
        w.set_window(10..15, 2, |r, c| {
            fills.set(fills.get() + 1);
            Some(format!("new{r}{c}"))
        });
        assert_eq!(fills.get(), 10, "five rows, two columns, all entering");
        assert_eq!(w.window(), 10..15);
        assert_eq!(w.get(0, 0), None, "the old window is gone");
        assert_eq!(w.get(12, 1).map(String::as_str), Some("new121"));
    }

    #[test]
    fn an_empty_range_gives_an_empty_window() {
        let mut w = WindowCache::default();
        w.set_window(0..5, 2, text);
        let fills = Cell::new(0);
        w.set_window(7..7, 2, |r, c| {
            fills.set(fills.get() + 1);
            text(r, c)
        });
        assert_eq!(fills.get(), 0);
        assert_eq!(w.window(), 7..7);
        assert!(w.window().is_empty());
        assert_eq!(w.get(2, 0), None);
        assert_eq!(w.get(7, 0), None);
    }

    #[test]
    fn a_pure_grow_fills_only_the_new_rows() {
        let mut w = WindowCache::default();
        w.set_window(4..8, 2, text);
        let filled = std::cell::RefCell::new(Vec::new());
        w.set_window(2..12, 2, |r, c| {
            filled.borrow_mut().push(r);
            text(r, c)
        });
        let mut rows = filled.into_inner();
        rows.dedup();
        assert_eq!(rows, vec![2, 3, 8, 9, 10, 11], "4..8 kept");
        assert_eq!(w.window(), 2..12);
        assert_eq!(w.get(5, 1).map(String::as_str), Some("r5c1"));
        assert_eq!(w.get(11, 0).map(String::as_str), Some("r11c0"));
    }

    #[test]
    fn a_request_with_no_rows_refills_nothing() {
        assert_eq!(WindowRequest::default().refill_range(0), None);
        let mut q = WindowRequest::with_first(FIRST_WINDOW);
        assert_eq!(q.refill_range(0), None, "first window, no rows");
        q.record(0..10);
        assert_eq!(q.refill_range(0), None, "recorded, no rows");
    }

    #[test]
    fn a_column_count_change_clears_the_window() {
        let mut w = WindowCache::default();
        let fills = Cell::new(0);
        let fill = |r: usize, c: usize| {
            fills.set(fills.get() + 1);
            text(r, c)
        };
        // Narrow first: a kept window read at a smaller stride would show
        // cells prepared for the wider layout rather than fail outright.
        w.set_window(0..4, 3, fill);
        w.set_window(0..4, 2, fill);
        assert_eq!(
            fills.get(),
            12 + 8,
            "every row refilled at the narrower width"
        );
        assert_eq!(w.get(3, 1).map(String::as_str), Some("r3c1"));
        assert_eq!(w.get(3, 2), None, "the dropped column");
        w.set_window(0..4, 3, fill);
        assert_eq!(
            fills.get(),
            20 + 12,
            "every row refilled at the wider width"
        );
        assert_eq!(w.get(3, 2).map(String::as_str), Some("r3c2"));
        assert_eq!(w.get(0, 0).map(String::as_str), Some("r0c0"));
    }

    #[test]
    fn get_outside_the_window_is_none() {
        let mut w = WindowCache::default();
        w.set_window(5..15, 2, text);
        assert_eq!(w.get(4, 0), None, "just above");
        assert_eq!(w.get(2, 0), None, "well above");
        assert_eq!(w.get(15, 0), None, "below");
        assert_eq!(w.get(7, 2), None, "past the last column");
        assert_eq!(w.get(7, 0).map(String::as_str), Some("r7c0"));
        w.clear();
        assert_eq!(w.get(7, 0), None, "cleared");
        assert_eq!(w.window(), 5..5);
    }

    #[test]
    fn refill_cell_touches_one_cell() {
        let mut w = WindowCache::default();
        w.set_window(0..3, 2, text);
        w.refill_cell(1, 1, || Some("edited".to_string()));
        assert_eq!(w.get(1, 1).map(String::as_str), Some("edited"));
        assert_eq!(
            w.get(1, 0).map(String::as_str),
            Some("r1c0"),
            "its neighbour"
        );
        assert_eq!(
            w.get(2, 1).map(String::as_str),
            Some("r2c1"),
            "the row below"
        );
        let called = Cell::new(false);
        w.refill_cell(9, 0, || {
            called.set(true);
            None
        });
        assert!(!called.get(), "outside the window nothing is prepared");
    }

    #[test]
    fn a_request_refills_its_range_clamped_to_the_rows() {
        let mut q = WindowRequest::default();
        assert_eq!(
            q.refill_range(100),
            None,
            "nothing asked and no first window"
        );
        q.record(10..40);
        assert_eq!(q.refill_range(100), Some(10..40));
        assert_eq!(q.refill_range(25), Some(10..25), "fewer rows now");
        assert_eq!(
            q.refill_range(5),
            Some(0..5),
            "every asked row is gone: the tail, at most the asked height"
        );
        assert_eq!(
            q.refill_range(100),
            Some(10..40),
            "the record outlives a shrink"
        );
    }

    /// A shrink past the recorded start refills the tail of the asked
    /// height — a lone row included, which the table never reports.
    #[test]
    fn a_shrink_past_the_recorded_start_refills_the_tail() {
        let mut q = WindowRequest::default();
        q.record(40..60);
        assert_eq!(q.refill_range(1), Some(0..1), "a one-row document");
        assert_eq!(q.refill_range(30), Some(10..30), "the last 20 rows");
        assert_eq!(q.refill_range(40), Some(20..40), "start == len");
        assert_eq!(q.refill_range(0), None, "no rows");
        q.record(5..5);
        assert_eq!(q.refill_range(3), Some(2..3), "an empty record: one row");
    }

    #[test]
    fn before_any_report_the_first_window_is_filled() {
        let mut q = WindowRequest::with_first(FIRST_WINDOW);
        assert_eq!(
            q.refill_range(1),
            Some(0..1),
            "a one-row grid the table never reports"
        );
        assert_eq!(q.refill_range(500), Some(0..FIRST_WINDOW));
        q.record(3..9);
        assert_eq!(q.refill_range(500), Some(3..9), "a report replaces it");
    }

    #[test]
    fn an_invalidation_refills_when_the_table_reports_nothing_new() {
        let mut q = WindowRequest::default();
        let mut w = WindowCache::default();
        q.record(0..10);
        w.set_window(0..10, 1, text);
        w.clear();
        if let Some(range) = q.refill_range(50) {
            w.set_window(range, 1, |r, c| Some(format!("new{r}{c}")));
        }
        assert_eq!(w.get(9, 0).map(String::as_str), Some("new90"));
        assert_eq!(w.window(), 0..10);
    }

    #[test]
    fn a_row_cache_keeps_reported_rows_fills_entering_ones_and_drops_the_rest() {
        let mut w = RowCache::default();
        let filled = std::cell::RefCell::new(Vec::new());
        let fill = |r: usize, c: usize| {
            filled.borrow_mut().push((r, c));
            text(r, c)
        };
        w.set_rows(&[7, 3, 90], 2, fill);
        assert_eq!(filled.borrow().len(), 6);
        assert_eq!(w.len(), 3);
        filled.borrow_mut().clear();
        w.set_rows(&[3, 4, 3], 2, fill);
        assert_eq!(
            *filled.borrow(),
            vec![(4, 0), (4, 1)],
            "3 kept, 4 entered once"
        );
        assert_eq!(w.len(), 2, "7 and 90 dropped");
        assert_eq!(w.get(7, 0), None);
        assert!(!w.contains(90));
        assert_eq!(w.get(3, 1).map(String::as_str), Some("r3c1"));
        assert_eq!(w.get(4, 2), None, "past the last column");
        filled.borrow_mut().clear();
        w.set_rows(&[3], 1, fill);
        assert_eq!(*filled.borrow(), vec![(3, 0)], "a new width refills");
        w.clear();
        assert!(w.is_empty());
    }
}
