//! A grid selection (grid selection spec §3.1): whole rows (`V`) or a
//! rectangular block of cells (`v`), anchored by row and column
//! *identity* so a re-sort, a column move or a live redelivery keeps it
//! on the same data. Indices are recomputed from the anchor to the
//! cursor in the current display order on every change; an anchor that
//! is no longer displayed resolves to `None` and the tile clears the
//! selection rather than guessing a neighbour.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    /// Whole rows; every column.
    Rows,
    /// A rectangle: a row range × a column range.
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<R, C> {
    pub kind: SelectKind,
    pub anchor_row: R,
    pub anchor_col: C,
}

/// Display-index ranges, half-open. For `Rows`, `cols` is every column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub kind: SelectKind,
    pub rows: Range<usize>,
    pub cols: Range<usize>,
}

impl Resolved {
    pub fn contains_row(&self, row: usize) -> bool {
        self.rows.contains(&row)
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        self.rows.contains(&row) && self.cols.contains(&col)
    }
}

fn span(a: usize, b: usize) -> Range<usize> {
    a.min(b)..a.max(b) + 1
}

pub fn resolve(
    kind: SelectKind,
    anchor: (usize, usize),
    cursor: (usize, usize),
    col_count: usize,
) -> Resolved {
    let rows = span(anchor.0, cursor.0);
    let cols = match kind {
        SelectKind::Rows => 0..col_count,
        SelectKind::Block => {
            let c = span(anchor.1, cursor.1);
            c.start.min(col_count)..c.end.min(col_count)
        }
    };
    Resolved { kind, rows, cols }
}

impl<R, C> Selection<R, C> {
    /// `None` when the anchor row — or, for a block, the anchor column —
    /// is not displayed. A `Rows` selection never looks its column up: a
    /// hidden anchor column must not drop a row selection.
    pub fn resolve_with(
        &self,
        cursor: (usize, usize),
        col_count: usize,
        find_row: impl FnOnce(&R) -> Option<usize>,
        find_col: impl FnOnce(&C) -> Option<usize>,
    ) -> Option<Resolved> {
        let row = find_row(&self.anchor_row)?;
        let col = match self.kind {
            SelectKind::Rows => 0,
            SelectKind::Block => find_col(&self.anchor_col)?,
        };
        Some(resolve(self.kind, (row, col), cursor, col_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_span_anchor_to_cursor_either_way_and_every_column() {
        let r = resolve(SelectKind::Rows, (5, 3), (2, 1), 4);
        assert_eq!((r.rows.clone(), r.cols.clone()), (2..6, 0..4));
        let r = resolve(SelectKind::Rows, (0, 0), (2, 3), 4);
        assert_eq!(r.rows, 0..3);
    }

    #[test]
    fn a_block_spans_the_rectangle_between_anchor_and_cursor() {
        let r = resolve(SelectKind::Block, (4, 3), (1, 1), 6);
        assert_eq!((r.rows.clone(), r.cols.clone()), (1..5, 1..4));
        assert!(r.contains(1, 1) && r.contains(4, 3));
        assert!(!r.contains(4, 4) && !r.contains(0, 2));
        assert!(r.contains_row(2));
    }

    #[test]
    fn a_block_column_range_is_clamped_to_the_column_count() {
        let r = resolve(SelectKind::Block, (0, 9), (0, 1), 4);
        assert_eq!(r.cols, 1..4);
    }

    #[test]
    fn resolution_goes_through_identity_and_a_lost_anchor_is_none() {
        let s = Selection {
            kind: SelectKind::Block,
            anchor_row: "b",
            anchor_col: "x",
        };
        let rows = ["a", "c", "b"]; // "b" moved from index 1 to 2 (a re-sort)
        let cols = ["w", "x"];
        let found = s
            .resolve_with(
                (0, 0),
                2,
                |r| rows.iter().position(|k| k == r),
                |c| cols.iter().position(|k| k == c),
            )
            .unwrap();
        assert_eq!((found.rows, found.cols), (0..3, 0..2));
        let gone = Selection {
            kind: SelectKind::Block,
            anchor_row: "z",
            anchor_col: "x",
        };
        assert!(
            gone.resolve_with((0, 0), 2, |r| rows.iter().position(|k| k == r), |_| Some(0))
                .is_none()
        );
    }

    #[test]
    fn a_rows_selection_never_needs_its_anchor_column() {
        let s = Selection {
            kind: SelectKind::Rows,
            anchor_row: 1usize,
            anchor_col: "hidden",
        };
        let r = s.resolve_with((3, 0), 5, |r| Some(*r), |_| None).unwrap();
        assert_eq!((r.rows, r.cols), (1..4, 0..5));
    }
}
