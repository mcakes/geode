//! What a grid selection reaches on the sheet: the lines an edit
//! writes (a package stands for its legs), the top-most rows a verb
//! or a total acts on (a package already carries its legs), the group
//! and move plans with their refusals, position risk totals, and the
//! one notice line that counts what a bulk edit wrote and skipped.

use crate::core::edit::Edit;
use crate::core::sheet::{LineState, Sheet};
use crate::core::visibility::Visibility;
use geode_core::pricing::{Currency, Measure};
use std::collections::BTreeMap;

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

/// `group_plan`'s refusal of a selection with a gap in sheet order.
pub const NOT_CONTIGUOUS: &str = "can't group: lines are not contiguous";

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
        return Err(NOT_CONTIGUOUS);
    }
    Ok((first, count))
}

/// A move's refusal at the end of its sibling order (the roots, or a
/// package's legs).
pub const OFF_END: &str = "cannot move past the end";

/// A move's refusal at the end of its rollup group under a value
/// grouping: every further sibling paints under another group, or
/// nowhere.
pub const OFF_GROUP_END: &str = "cannot move past the end of the group";

/// One `Edit::Move` that slides the whole block one step among the
/// siblings `lands` accepts — the shown ones, and under a value grouping
/// only those painted in the block's own group node
/// ([`crate::core::reorder::Placements::same_node`]). The nearest such
/// neighbour hops across the block and every sibling `lands` refuses
/// between them (hidden, or another group's), so the painted order inside
/// the group changes as the key says and no other group's order does.
/// Every member must share a parent, or the block has no single sibling
/// order. `edge` is the refusal when no such neighbour lies that way
/// ([`OFF_END`] or [`OFF_GROUP_END`]).
pub fn move_plan(
    sheet: &Sheet,
    top: &[usize],
    down: bool,
    lands: impl Fn(usize) -> bool,
    edge: &'static str,
) -> Result<Edit, &'static str> {
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
    if down {
        let q = (hi + 1..siblings.len())
            .find(|&q| lands(siblings[q]))
            .ok_or(edge)?;
        Ok(Edit::Move {
            row: siblings[q],
            delta: -((q - lo) as isize),
        })
    } else {
        let q = (0..lo).rev().find(|&q| lands(siblings[q])).ok_or(edge)?;
        Ok(Edit::Move {
            row: siblings[q],
            delta: (hi - q) as isize,
        })
    }
}

/// Position totals over top-most rows, one per `(measure, usd)` in
/// `measures`' order: `qty × value` for a line, the package's own folded
/// sum (already qty-weighted) for a package. A column with any row
/// unpriced or failed is `None`: an incomplete total would read as a
/// real one. A failed line keeps its old result, so the state is
/// checked, not only the result.
///
/// No rows is `None` too, not zero: a total of nothing reads as a flat
/// position, which is a claim the selection never made.
///
/// A local-currency total (`usd == false`) over rows whose results are
/// not all in one currency is `None` as well — a mixed package counts
/// as differing — since a sum of unlike units would read as a real
/// one. The `_usd` totals are converted per row and still sum.
pub fn risk_totals(sheet: &Sheet, top: &[usize], measures: &[(Measure, bool)]) -> Vec<Option<f64>> {
    totals(sheet, top, measures, None)
}

/// [`risk_totals`] under a scope: a hidden row counts nothing, and a
/// package whose scope hides some legs counts its shown legs' fold
/// ([`Sheet::fold_legs`], the fold its row paints), never its full sum.
/// A selection left with no shown row is `None`, as no rows is.
pub fn risk_totals_visible(
    sheet: &Sheet,
    top: &[usize],
    measures: &[(Measure, bool)],
    visibility: &Visibility,
) -> Vec<Option<f64>> {
    totals(sheet, top, measures, Some(visibility))
}

fn totals(
    sheet: &Sheet,
    top: &[usize],
    measures: &[(Measure, bool)],
    visibility: Option<&Visibility>,
) -> Vec<Option<f64>> {
    let shown = |r: usize| visibility.is_none_or(|v| v.is_shown(r));
    if !top.iter().any(|&r| shown(r)) {
        return vec![None; measures.len()];
    }
    let mut sums = vec![Some(0.0f64); measures.len()];
    // The one currency the contributing results share, or `MIXED` once
    // two differ (a package already folded mixed differs from any code).
    let mut currency: Option<Currency> = None;
    for &r in top.iter().filter(|&&r| shown(r)) {
        let weight = if sheet.is_package(r) {
            1.0
        } else {
            sheet.qty(r) as f64
        };
        let subset = visibility
            .filter(|v| sheet.is_package(r) && v.is_partial(sheet, r))
            .map(|v| sheet.fold_legs(v.shown_legs(sheet, r)));
        let (state, result) = match &subset {
            Some(f) => (&f.state, f.result.as_ref()),
            None => (sheet.state(r), sheet.result(r)),
        };
        let picked = match (state, result) {
            (LineState::Failed(_), _) | (_, None) => None,
            (_, Some(v)) => Some(v),
        };
        if let Some(v) = picked {
            currency = Some(match currency {
                Some(c) if c != v.currency => Currency::MIXED,
                _ => v.currency,
            });
        }
        for (s, &(m, usd)) in sums.iter_mut().zip(measures) {
            *s = match (*s, picked) {
                (Some(a), Some(v)) => Some(a + weight * v.get(m, usd)),
                _ => None,
            };
        }
    }
    if currency.is_some_and(|c| c.is_mixed()) {
        for (s, &(_, usd)) in sums.iter_mut().zip(measures) {
            if !usd {
                *s = None;
            }
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

    /// The two measures the totals tests read: npv, then delta01.
    const NPV_DELTA: [(Measure, bool); 2] = [(Measure::Npv, false), (Measure::Delta01, false)];

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
            move_plan(&s, &[0, 1], true, |_| true, OFF_END),
            Ok(Edit::Move { row: 2, delta: -2 })
        );
        assert_eq!(
            move_plan(&s, &[2, 3], true, |_| true, OFF_END),
            Err("cannot move past the end")
        );
        assert_eq!(
            move_plan(&s, &[1, 2], false, |_| true, OFF_END),
            Ok(Edit::Move { row: 0, delta: 2 })
        );
        assert_eq!(
            move_plan(&s, &[0, 1], false, |_| true, OFF_END),
            Err("cannot move past the end")
        );
    }

    #[test]
    fn move_plan_hops_a_package_block_over_a_root() {
        // 0 A; 1 package (legs 2, 3); 4 B: the sibling step is a root, not a row
        let s = sheet(vec![call(5000.0, 1), callspread(-5), put(4000.0, 1)]);
        assert_eq!(
            move_plan(&s, &[0, 1], true, |_| true, OFF_END),
            Ok(Edit::Move { row: 4, delta: -2 })
        );
        assert_eq!(
            move_plan(&s, &[1, 4], false, |_| true, OFF_END),
            Ok(Edit::Move { row: 0, delta: 2 })
        );
        assert_eq!(
            move_plan(&s, &[2], true, |_| true, OFF_END),
            Ok(Edit::Move { row: 3, delta: -1 }),
            "a leg moves among its package's legs"
        );
    }

    #[test]
    fn move_plan_refuses_a_selection_across_parents() {
        let s = sheet(vec![call(5000.0, 1), callspread(-5)]);
        assert_eq!(s.children(1), 2..4);
        assert_eq!(
            move_plan(&s, &[0, 2], true, |_| true, OFF_END),
            Err("can't move: selection spans packages")
        );
    }

    #[test]
    fn risk_totals_are_position_totals_and_refuse_an_incomplete_column() {
        let mut s = sheet(vec![call(5000.0, 2), put(4000.0, 1)]);
        price(&mut s, 0, 1.5);
        price(&mut s, 1, 0.25);
        let t = risk_totals(&s, &[0, 1], &NPV_DELTA);
        assert_eq!(t[0], Some(2.0 * 1.5 + 0.25), "qty × unit price per line");
        assert_eq!(t[1], Some(2.0 * 0.15 + 0.025), "delta is weighted too");
        let usd = risk_totals(&s, &[0, 1], &[(Measure::Npv, true)]);
        assert_eq!(
            usd[0],
            Some((2.0 * 1.5 + 0.25) * 1.08),
            "a usd twin totals the usd array"
        );
        let unpriced = sheet(vec![call(5000.0, 1), put(4000.0, 1)]);
        assert_eq!(
            risk_totals(&unpriced, &[0, 1], &NPV_DELTA)[0],
            None,
            "no partial sum"
        );
    }

    /// Install `price` as row `row`'s result in `currency`.
    fn price_in(s: &mut Sheet, row: usize, price: f64, currency: &str) {
        let mut r = result(price);
        r.currency = Currency::parse(currency).unwrap();
        let d = s.deliver(s.id(row), s.revision(row), Ok(r), at(0));
        assert_eq!(d, crate::core::sheet::Delivered::Installed);
    }

    #[test]
    fn risk_totals_over_differing_currencies_are_a_gap_locally_and_a_sum_in_usd() {
        const NPV_BOTH: [(Measure, bool); 2] = [(Measure::Npv, false), (Measure::Npv, true)];
        let mut s = sheet(vec![call(5000.0, 2), put(4000.0, 1)]);
        price_in(&mut s, 0, 1.5, "USD");
        price_in(&mut s, 1, 0.25, "EUR");
        assert_eq!(
            risk_totals(&s, &[0, 1], &NPV_BOTH),
            vec![None, Some((2.0 * 1.5 + 0.25) * 1.08)],
            "USD + EUR: no local sum, the usd twin sums"
        );
        assert_eq!(
            risk_totals(&s, &[1], &NPV_BOTH),
            vec![Some(0.25), Some(0.25 * 1.08)],
            "one EUR line alone totals in EUR"
        );
        price_in(&mut s, 1, 0.25, "USD");
        assert_eq!(
            risk_totals(&s, &[0, 1], &NPV_BOTH),
            vec![Some(2.0 * 1.5 + 0.25), Some((2.0 * 1.5 + 0.25) * 1.08)],
            "two USD lines total both ways"
        );
        // A package folded over USD and EUR legs is mixed on its own.
        let mut p = sheet(vec![callspread(-5)]);
        price_in(&mut p, 1, 3.0, "USD");
        price_in(&mut p, 2, 1.0, "EUR");
        let t = risk_totals(&p, &[0], &NPV_BOTH);
        assert_eq!(t[0], None, "a mixed package is a gap");
        assert_eq!(t[1], Some(p.result(0).unwrap().get(Measure::Npv, true)));
    }

    #[test]
    fn risk_totals_take_a_packages_folded_sum_unweighted() {
        // 0 package qty -5 (legs 1, 2); the fold already weights by leg qty
        let mut s = sheet(vec![callspread(-5)]);
        price(&mut s, 1, 3.0);
        price(&mut s, 2, 1.0);
        let folded = s.result(0).expect("folded").get(Measure::Npv, false);
        assert_eq!(risk_totals(&s, &[0], &NPV_DELTA)[0], Some(folded));
    }

    #[test]
    fn risk_totals_refuse_a_failed_line_even_with_a_retained_result() {
        let mut s = sheet(vec![call(5000.0, 1)]);
        price(&mut s, 0, 1.0);
        s.deliver(s.id(0), s.revision(0), Err("boom".into()), at(1));
        assert!(s.result(0).is_some(), "a failure retains the old result");
        assert_eq!(risk_totals(&s, &[0], &NPV_DELTA), vec![None; 2]);
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
            risk_totals(&s, &[0], &NPV_DELTA)[0],
            Some(3.0),
            "what the grid shows counts"
        );
    }

    #[test]
    fn totals_over_a_partly_hidden_package_count_only_its_shown_legs() {
        // [line 5000 C ×2, CS(-5): 4800 C ×-5, 5200 C ×5], scoped to
        // `strike > 5000`: the line and the 4800 leg are hidden.
        let mut s = sheet(vec![call(5000.0, 2), callspread(-5)]);
        assert_eq!((s.qty(2), s.qty(3)), (-5, 5));
        price(&mut s, 0, 4.0);
        price(&mut s, 2, 1.0);
        price(&mut s, 3, 0.5);
        let scope = geode_core::scope::Scope {
            expression: Some(geode_core::scope::parse_expr("strike > 5000").unwrap()),
            ..Default::default()
        };
        let v = crate::core::visibility::apply_scope(
            &s,
            &scope,
            &Default::default(),
            geode_core::clock::Clock::utc(),
        )
        .unwrap();
        assert_eq!(
            risk_totals(&s, &[0, 1], &NPV_DELTA)[0],
            Some(2.0 * 4.0 - 5.0 * 1.0 + 5.0 * 0.5),
            "unscoped: the line and the package's full fold"
        );
        let t = risk_totals_visible(&s, &[0, 1], &NPV_DELTA, &v);
        assert_eq!(t[0], Some(5.0 * 0.5), "the shown 5200 leg only");
        assert_eq!(t[1], Some(5.0 * 0.05));
        assert_eq!(
            risk_totals_visible(&s, &[0], &NPV_DELTA, &v),
            vec![None; 2],
            "a hidden row alone totals nothing"
        );
        assert_eq!(
            risk_totals_visible(&s, &[0, 1], &NPV_DELTA, &Visibility::all(&s)),
            risk_totals(&s, &[0, 1], &NPV_DELTA),
            "everything shown is the unscoped total"
        );
    }

    #[test]
    fn risk_totals_over_no_rows_are_incomplete_not_zero() {
        let mut s = sheet(vec![call(5000.0, 1)]);
        price(&mut s, 0, 1.0);
        assert_eq!(risk_totals(&s, &[], &NPV_DELTA), vec![None; 2]);
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
