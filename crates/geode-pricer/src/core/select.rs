//! What a grid selection reaches on the sheet: the lines an edit
//! writes (a package stands for its legs), the top-most rows a verb
//! or a total acts on (a package already carries its legs), the group
//! and move plans with their refusals, position risk totals, and the
//! one notice line that counts what a bulk edit wrote and skipped.

use crate::core::columns::ColumnKind;
use crate::core::edit::Edit;
use crate::core::sheet::{LineState, Sheet};
use std::collections::BTreeMap;

/// The footer's risk columns, in [`risk_totals`]' order.
pub const RISK: [ColumnKind; 6] = [
    ColumnKind::Price,
    ColumnKind::Delta,
    ColumnKind::Gamma,
    ColumnKind::Vega,
    ColumnKind::Theta,
    ColumnKind::Rho,
];

/// The leaf lines under `rows`, each once, in sheet order: a line is
/// itself, a package is its legs whether it is open or not. A package
/// selected together with its own legs must never edit a leg twice.
pub fn lines_of(sheet: &Sheet, rows: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = rows
        .iter()
        .flat_map(|&r| {
            if sheet.is_package(r) {
                sheet.children(r)
            } else {
                r..r + 1
            }
        })
        .filter(|&r| sheet.is_line(r))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The rows of `rows` with no selected ancestor, in input order. A
/// package already carries its legs, so a verb or a total over both
/// would double them.
pub fn top_most(sheet: &Sheet, rows: &[usize]) -> Vec<usize> {
    geode_core::grid::selection::top_most(rows, sheet.len(), |r| sheet.parent(r))
}

/// `(first, count)` for `Edit::Group` over top-most `top`: every row a
/// root line, and together a contiguous run. A gap would sweep an
/// unselected line into the package, since `Group` takes a run.
pub fn group_plan(sheet: &Sheet, top: &[usize]) -> Result<(usize, usize), &'static str> {
    if top.iter().any(|&r| sheet.is_package(r)) {
        return Err("can't group: selection includes a package");
    }
    if top.iter().any(|&r| sheet.parent(r).is_some()) {
        return Err("can't group: lines are inside a package");
    }
    let (Some(&first), Some(&last)) = (top.iter().min(), top.iter().max()) else {
        return Err("no row");
    };
    let count = last - first + 1;
    if (first..=last).any(|r| !top.contains(&r)) {
        return Err("can't group: lines are not contiguous");
    }
    Ok((first, count))
}

/// One `Edit::Move` that slides the whole block one sibling step: the
/// neighbouring sibling hops across the block by its length. Every
/// member must share a parent, or the block has no single sibling order.
pub fn move_plan(sheet: &Sheet, top: &[usize], down: bool) -> Result<Edit, &'static str> {
    let Some(&first) = top.first() else {
        return Err("no row");
    };
    let parent = sheet.parent(first);
    if top.iter().any(|&r| sheet.parent(r) != parent) {
        return Err("can't move: selection spans packages");
    }
    let siblings = sheet.siblings(first);
    let positions = || {
        top.iter()
            .filter_map(|&r| siblings.iter().position(|s| *s == r))
    };
    let (Some(lo), Some(hi)) = (positions().min(), positions().max()) else {
        return Err("no row");
    };
    let len = (hi - lo + 1) as isize;
    if down {
        let next = *siblings.get(hi + 1).ok_or("cannot move past the end")?;
        Ok(Edit::Move {
            row: next,
            delta: -len,
        })
    } else {
        let prev = *lo
            .checked_sub(1)
            .and_then(|p| siblings.get(p))
            .ok_or("cannot move past the end")?;
        Ok(Edit::Move {
            row: prev,
            delta: len,
        })
    }
}

/// Position totals over top-most rows, in [`RISK`] order: `qty × value`
/// for a line, the package's own folded sum (already qty-weighted) for
/// a package. A column with any row unpriced or failed is `None`: an
/// incomplete total would read as a real one. A failed line keeps its
/// old result, so the state is checked, not only the result.
///
/// No rows is `None` too, not zero: a total of nothing reads as a flat
/// position, which is a claim the selection never made.
pub fn risk_totals(sheet: &Sheet, top: &[usize]) -> [Option<f64>; 6] {
    if top.is_empty() {
        return [None; 6];
    }
    let mut sums = [Some(0.0f64); 6];
    for &r in top {
        let weight = if sheet.is_package(r) {
            1.0
        } else {
            sheet.qty(r) as f64
        };
        let picks = match (sheet.state(r), sheet.result(r)) {
            (LineState::Failed(_), _) | (_, None) => None,
            (_, Some(v)) => Some([v.price, v.delta, v.gamma, v.vega, v.theta, v.rho]),
        };
        for (i, s) in sums.iter_mut().enumerate() {
            *s = match (*s, picks) {
                (Some(a), Some(p)) => Some(a + weight * p[i]),
                _ => None,
            };
        }
    }
    sums
}

/// Why a selected cell took no part in a bulk edit. The order is the
/// notice's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Skip {
    ReadOnly,
    /// The column does not apply to this row's instrument.
    NotApplicable,
    NotNumeric,
    /// The sheet refused the value for this cell.
    Refused,
}

impl Skip {
    fn phrase(self) -> &'static str {
        match self {
            Skip::ReadOnly => "read-only",
            Skip::NotApplicable => "n/a",
            Skip::NotNumeric => "not numeric",
            Skip::Refused => "refused",
        }
    }
}

/// Skipped cells counted per reason, kept in [`Skip`]'s order.
#[derive(Debug, Clone, Default)]
pub struct Skips(BTreeMap<Skip, usize>);

impl Skips {
    pub fn add(&mut self, skip: Skip) {
        *self.0.entry(skip).or_default() += 1;
    }

    pub fn total(&self) -> usize {
        self.0.values().sum()
    }

    /// `""` when nothing was skipped, else `", skipped 3 (2 read-only, 1 refused)"`.
    pub fn describe(&self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|(skip, n)| format!("{n} {}", skip.phrase()))
            .collect();
        format!(", skipped {} ({})", self.total(), parts.join(", "))
    }
}

/// `N cell` or `N cells`: every bulk notice counts through this one door.
fn cells(n: usize) -> String {
    format!("{n} cell{}", if n == 1 { "" } else { "s" })
}

pub fn set_notice(n: usize, skips: &Skips) -> String {
    format!("set {}{}", cells(n), skips.describe())
}

/// `total_steps` is the signed count since the editor opened, so the line
/// says where the block stands, not only the last press.
pub fn step_notice(n: usize, total_steps: i64, skips: &Skips) -> String {
    format!("stepped {} {total_steps:+}{}", cells(n), skips.describe())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use geode_core::pricing::OptionKind;

    fn put(strike: f64, qty: i64) -> crate::core::sheet::RowSpec {
        line(spx(strike, OptionKind::Put), qty)
    }

    fn call(strike: f64, qty: i64) -> crate::core::sheet::RowSpec {
        line(spx(strike, OptionKind::Call), qty)
    }

    fn sheet(rows: Vec<crate::core::sheet::RowSpec>) -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, rows);
        s
    }

    /// Install `price` as row `row`'s result at its current revision.
    fn price(s: &mut Sheet, row: usize, price: f64) {
        let d = s.deliver(s.id(row), s.revision(row), Ok(result(price)), at(0));
        assert_eq!(d, crate::core::sheet::Delivered::Installed);
    }

    #[test]
    fn lines_of_dedupes_a_package_and_its_legs() {
        // roots: 0 line A; 1 package P (legs 2, 3); 4 line B
        let s = sheet(vec![call(5000.0, 1), callspread(-5), put(4000.0, 1)]);
        assert_eq!(s.children(1), 2..4);
        assert_eq!(
            lines_of(&s, &[1]),
            vec![2, 3],
            "a package stands for its legs"
        );
        assert_eq!(
            lines_of(&s, &[1, 2]),
            vec![2, 3],
            "its own leg is not edited twice"
        );
        assert_eq!(lines_of(&s, &[4, 1, 0]), vec![0, 2, 3, 4], "sheet order");
    }

    #[test]
    fn top_most_drops_a_selected_packages_legs() {
        let s = sheet(vec![call(5000.0, 1), callspread(-5), put(4000.0, 1)]);
        assert_eq!(top_most(&s, &[1, 2, 3, 4]), vec![1, 4]);
        assert_eq!(
            top_most(&s, &[2, 3]),
            vec![2, 3],
            "legs without their package stand alone"
        );
    }

    #[test]
    fn group_plan_needs_contiguous_root_lines_and_names_why() {
        // 0 A; 1 B; 2 package (legs 3, 4); 5 D
        let s = sheet(vec![
            call(5000.0, 1),
            put(4000.0, 1),
            callspread(-5),
            put(3000.0, 1),
        ]);
        assert_eq!(s.children(2), 3..5);
        assert_eq!(group_plan(&s, &[0, 1]), Ok((0, 2)));
        assert_eq!(
            group_plan(&s, &[1, 2]),
            Err("can't group: selection includes a package")
        );
        assert_eq!(
            group_plan(&s, &[3]),
            Err("can't group: lines are inside a package")
        );
        let three = sheet(vec![call(1.0, 1), call(2.0, 1), call(3.0, 1)]);
        assert_eq!(
            group_plan(&three, &[0, 2]),
            Err("can't group: lines are not contiguous"),
            "an unselected line between would be swept into the package"
        );
    }

    #[test]
    fn move_plan_moves_the_neighbouring_sibling_across_the_block() {
        let s = sheet(vec![
            call(5000.0, 1),
            put(4000.0, 1),
            put(3000.0, 1),
            put(2000.0, 1),
        ]);
        // block = roots 0..2; down: root 2 hops up over two siblings
        assert_eq!(
            move_plan(&s, &[0, 1], true),
            Ok(Edit::Move { row: 2, delta: -2 })
        );
        assert_eq!(
            move_plan(&s, &[2, 3], true),
            Err("cannot move past the end")
        );
        assert_eq!(
            move_plan(&s, &[1, 2], false),
            Ok(Edit::Move { row: 0, delta: 2 })
        );
        assert_eq!(
            move_plan(&s, &[0, 1], false),
            Err("cannot move past the end")
        );
    }

    #[test]
    fn move_plan_hops_a_package_block_over_a_root() {
        // 0 A; 1 package (legs 2, 3); 4 B: the sibling step is a root, not a row
        let s = sheet(vec![call(5000.0, 1), callspread(-5), put(4000.0, 1)]);
        assert_eq!(
            move_plan(&s, &[0, 1], true),
            Ok(Edit::Move { row: 4, delta: -2 })
        );
        assert_eq!(
            move_plan(&s, &[1, 4], false),
            Ok(Edit::Move { row: 0, delta: 2 })
        );
        assert_eq!(
            move_plan(&s, &[2], true),
            Ok(Edit::Move { row: 3, delta: -1 }),
            "a leg moves among its package's legs"
        );
    }

    #[test]
    fn move_plan_refuses_a_selection_across_parents() {
        let s = sheet(vec![call(5000.0, 1), callspread(-5)]);
        assert_eq!(s.children(1), 2..4);
        assert_eq!(
            move_plan(&s, &[0, 2], true),
            Err("can't move: selection spans packages")
        );
    }

    #[test]
    fn risk_totals_are_position_totals_and_refuse_an_incomplete_column() {
        let mut s = sheet(vec![call(5000.0, 2), put(4000.0, 1)]);
        price(&mut s, 0, 1.5);
        price(&mut s, 1, 0.25);
        let t = risk_totals(&s, &[0, 1]);
        assert_eq!(t[0], Some(2.0 * 1.5 + 0.25), "qty × unit price per line");
        assert_eq!(t[1], Some(2.0 * 0.15 + 0.025), "delta is weighted too");
        let unpriced = sheet(vec![call(5000.0, 1), put(4000.0, 1)]);
        assert_eq!(risk_totals(&unpriced, &[0, 1])[0], None, "no partial sum");
    }

    #[test]
    fn risk_totals_take_a_packages_folded_sum_unweighted() {
        // 0 package qty -5 (legs 1, 2); the fold already weights by leg qty
        let mut s = sheet(vec![callspread(-5)]);
        price(&mut s, 1, 3.0);
        price(&mut s, 2, 1.0);
        let folded = s.result(0).expect("folded").price;
        assert_eq!(risk_totals(&s, &[0])[0], Some(folded));
    }

    #[test]
    fn risk_totals_refuse_a_failed_line_even_with_a_retained_result() {
        let mut s = sheet(vec![call(5000.0, 1)]);
        price(&mut s, 0, 1.0);
        s.deliver(s.id(0), s.revision(0), Err("boom".into()), at(1));
        assert!(s.result(0).is_some(), "a failure retains the old result");
        assert_eq!(risk_totals(&s, &[0]), [None; 6]);
    }

    #[test]
    fn risk_totals_count_a_stale_row_that_still_shows_a_result() {
        let mut s = sheet(vec![call(5000.0, 1)]);
        price(&mut s, 0, 1.0);
        s.apply(Edit::SetQty { row: 0, qty: 3 }).unwrap();
        // A refresh tick restales every line without dropping its result.
        s.mark_all_stale();
        assert_eq!(s.state(0), &LineState::Stale);
        assert!(s.result(0).is_some(), "the old result is still painted");
        assert_eq!(
            risk_totals(&s, &[0])[0],
            Some(3.0),
            "what the grid shows counts"
        );
    }

    #[test]
    fn risk_totals_over_no_rows_are_incomplete_not_zero() {
        let mut s = sheet(vec![call(5000.0, 1)]);
        price(&mut s, 0, 1.0);
        assert_eq!(risk_totals(&s, &[]), [None; 6]);
    }

    #[test]
    fn notices_count_cells_and_name_each_skip_in_a_fixed_order() {
        let mut k = Skips::default();
        assert_eq!(set_notice(1, &k), "set 1 cell");
        k.add(Skip::Refused);
        k.add(Skip::ReadOnly);
        k.add(Skip::ReadOnly);
        assert_eq!(
            set_notice(6, &k),
            "set 6 cells, skipped 3 (2 read-only, 1 refused)"
        );
        assert_eq!(k.total(), 3);
        k.add(Skip::NotNumeric);
        k.add(Skip::NotApplicable);
        assert_eq!(
            k.describe(),
            ", skipped 5 (2 read-only, 1 n/a, 1 not numeric, 1 refused)"
        );
        assert_eq!(step_notice(4, -3, &Skips::default()), "stepped 4 cells -3");
        assert_eq!(step_notice(1, 2, &Skips::default()), "stepped 1 cell +2");
    }
}
