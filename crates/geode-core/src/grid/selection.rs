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

/// The rows of `rows` with no ancestor also in `rows` (spec §1 ruling 2):
/// a parent row already carries its children's total, so counting both
/// would double it. Walks each row's ancestor chain against a dense
/// membership bitmap of size `universe` — independent of display order,
/// which fzf narrowing does not keep in tree order. Output keeps `rows`'
/// order.
pub fn top_most(
    rows: &[usize],
    universe: usize,
    parent: impl Fn(usize) -> Option<usize>,
) -> Vec<usize> {
    let mut selected = vec![false; universe];
    for &r in rows {
        if let Some(s) = selected.get_mut(r) {
            *s = true;
        }
    }
    rows.iter()
        .copied()
        .filter(|&r| {
            let mut at = parent(r);
            while let Some(p) = at {
                if selected.get(p).copied().unwrap_or(false) {
                    return false;
                }
                at = parent(p);
            }
            true
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColumnAggregate {
    /// Non-NULL, non-NaN values seen.
    pub count: usize,
    /// `None` when `count == 0` or any value was non-additive.
    pub sum: Option<f64>,
    pub mean: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Some contributing value must never be totalled
    /// (`Attribution::DeterminedNonAdditive`).
    pub non_additive: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Accumulator {
    count: usize,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    non_additive: bool,
}

impl Accumulator {
    pub fn add(&mut self, value: Option<f64>, additive: bool) {
        let Some(v) = value.filter(|v| !v.is_nan()) else {
            return;
        };
        self.count += 1;
        self.sum += v;
        self.min = Some(self.min.map_or(v, |m| m.min(v)));
        self.max = Some(self.max.map_or(v, |m| m.max(v)));
        self.non_additive |= !additive;
    }

    pub fn finish(&self) -> ColumnAggregate {
        let totals = self.count > 0 && !self.non_additive;
        ColumnAggregate {
            count: self.count,
            sum: totals.then_some(self.sum),
            mean: totals.then(|| self.sum / self.count as f64),
            min: self.min,
            max: self.max,
            non_additive: self.non_additive,
        }
    }
}

/// The footer text for one column (spec §3.3), formatted with that
/// column's own `ColumnFormat`. `extremes` adds `min`/`max` (a selection
/// covering a single numeric column).
pub fn describe(
    agg: &ColumnAggregate,
    format: &crate::view::ColumnFormat,
    extremes: bool,
) -> String {
    use crate::format::format_number;
    let f = |v: f64| format_number(v, format).text;
    let mut parts: Vec<String> = Vec::new();
    if agg.non_additive {
        parts.push("Σ —†".into());
    } else if let (Some(s), Some(m)) = (agg.sum, agg.mean) {
        parts.push(format!("Σ {}", f(s)));
        parts.push(format!("μ {}", f(m)));
    }
    parts.push(format!("n {}", agg.count));
    if extremes && let (Some(lo), Some(hi)) = (agg.min, agg.max) {
        parts.push(format!("min {}", f(lo)));
        parts.push(format!("max {}", f(hi)));
    }
    parts.join(" · ")
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

    // Tree: 0 root; 1, 2 children of 0; 3 child of 1; 4 child of 3.
    fn parent(r: usize) -> Option<usize> {
        [None, Some(0), Some(0), Some(1), Some(3)][r]
    }

    #[test]
    fn a_group_and_its_children_count_once_as_the_group() {
        assert_eq!(top_most(&[1, 3, 4, 2], 5, parent), vec![1, 2]);
    }

    #[test]
    fn some_children_alone_are_summed_themselves() {
        assert_eq!(top_most(&[3, 2], 5, parent), vec![3, 2]);
    }

    #[test]
    fn a_grandparent_hides_a_grandchild_even_without_the_middle_row() {
        assert_eq!(top_most(&[1, 4], 5, parent), vec![1]);
    }

    #[test]
    fn top_most_ignores_display_order() {
        // fzf narrowing shows rows in score order: child before parent.
        assert_eq!(top_most(&[4, 2, 1], 5, parent), vec![2, 1]);
    }

    #[test]
    fn a_collapsed_group_is_summed_as_itself() {
        // Row 1 collapsed: 3 and 4 are not displayed, so not in `rows`.
        assert_eq!(top_most(&[1], 5, parent), vec![1]);
    }

    #[test]
    fn nulls_and_nan_are_excluded_from_every_statistic() {
        let mut a = Accumulator::default();
        for v in [Some(1.0), None, Some(f64::NAN), Some(3.0)] {
            a.add(v, true);
        }
        let g = a.finish();
        assert_eq!(g.count, 2);
        assert_eq!(g.sum, Some(4.0));
        assert_eq!(g.mean, Some(2.0));
        assert_eq!((g.min, g.max), (Some(1.0), Some(3.0)));
    }

    #[test]
    fn a_non_additive_value_suppresses_sum_and_mean_but_not_extremes() {
        let mut a = Accumulator::default();
        a.add(Some(5.0), true);
        a.add(Some(7.0), false);
        let g = a.finish();
        assert!(g.non_additive);
        assert_eq!((g.sum, g.mean), (None, None));
        assert_eq!((g.count, g.min, g.max), (2, Some(5.0), Some(7.0)));
    }

    #[test]
    fn an_empty_column_has_no_statistics() {
        let g = Accumulator::default().finish();
        assert_eq!(g, ColumnAggregate::default());
    }

    #[test]
    fn describe_uses_the_column_format_and_marks_non_additive() {
        use crate::view::ColumnFormat;
        let mut a = Accumulator::default();
        a.add(Some(1000.0), true);
        a.add(Some(2000.0), true);
        let f = ColumnFormat::MEASURE;
        assert_eq!(
            describe(&a.finish(), &f, false),
            "Σ 3,000.00 · μ 1,500.00 · n 2"
        );
        assert_eq!(
            describe(&a.finish(), &f, true),
            "Σ 3,000.00 · μ 1,500.00 · n 2 · min 1,000.00 · max 2,000.00"
        );
        let mut b = Accumulator::default();
        b.add(Some(1.0), false);
        assert_eq!(describe(&b.finish(), &f, false), "Σ —† · n 1");
        assert_eq!(describe(&Accumulator::default().finish(), &f, false), "n 0");
    }
}
