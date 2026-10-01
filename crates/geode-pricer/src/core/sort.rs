//! Display order under a column sort: each sibling set of the rollup tree
//! ranked by the value its rows paint in one column, the sheet itself
//! untouched.
//!
//! [`rank`] reorders the roots and every group node's children in place;
//! a package's legs keep sheet order beneath it, so a package moves as
//! one unit. A group row ranks by its own folded value (a measure's sum,
//! a dimension's unanimous value), a package by its aggregate over the
//! legs its node holds (what a split or partly hidden package paints), a
//! line by its own cell. Keys are the typed values behind the cells, never
//! their text: numbers compare as numbers, an expiry as a date, text by
//! byte order (the blotter's text order). A package whose legs disagree
//! compares by its distinct values in leg order, the parts its cell
//! joins with `/`.
//!
//! The order is the blotter's: values first, then cells with no single
//! value (`mixed`, a mixed-currency `—`, a failed line's `—`), then
//! blanks, in both directions; ties keep the order the rollup built,
//! which is sheet order beneath the last value level. Pure: no I/O, no
//! entities.

use crate::core::columns::{ColumnKind, column};
use crate::core::package::{aggregates, package_qty};
use crate::core::rollup::{NodeKind, Rollup, legs_under};
use crate::core::sheet::{LineState, RowKind, Sheet};
use crate::core::shorthand::render_barrier_kind;
use chrono::NaiveDate;
use geode_core::pricing::{Expiry, Instrument, OptionKind, PriceResult, Strike};
use geode_core::sort::SortOrder;
use std::cmp::Ordering;

/// The tile's sort: a vocabulary column by name — not by plan position,
/// which a column move or a hidden column would retarget — and its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    pub column: &'static str,
    pub order: SortOrder,
}

/// Whether `kind` has a magnitude for `shift+s` and the absolute orders:
/// the result measures, the `pricer` dataset's measure columns. Quantity,
/// strikes and shifts are dimensions there, as they are in the blotter.
pub fn is_measure(kind: ColumnKind) -> bool {
    matches!(kind, ColumnKind::Measure { .. })
}

/// One typed value behind a cell. Within a column the variants rarely
/// meet (a strike is absolute or percent, an expiry dated or a tenor);
/// where they do, the variant order keeps the comparison total.
#[derive(Debug, Clone, PartialEq)]
enum Part {
    Num(f64),
    /// A percent strike: after every absolute one.
    Pct(f64),
    Date(NaiveDate),
    /// A tenor the pricer never resolves to a date (that is the pricing
    /// library's calendar): after every dated expiry, by its nominal
    /// length in days, then its text.
    Tenor(u32, String),
    Text(String),
}

impl Part {
    fn variant(&self) -> u8 {
        match self {
            Part::Num(_) => 0,
            Part::Pct(_) => 1,
            Part::Date(_) => 2,
            Part::Tenor(..) => 3,
            Part::Text(_) => 4,
        }
    }

    fn cmp(&self, other: &Part, abs: bool) -> Ordering {
        let mag = |v: f64| if abs { v.abs() } else { v };
        match (self, other) {
            (Part::Num(a), Part::Num(b)) | (Part::Pct(a), Part::Pct(b)) => {
                mag(*a).total_cmp(&mag(*b))
            }
            (Part::Date(a), Part::Date(b)) => a.cmp(b),
            (Part::Tenor(a, x), Part::Tenor(b, y)) => a.cmp(b).then_with(|| x.cmp(y)),
            (Part::Text(a), Part::Text(b)) => a.cmp(b),
            (a, b) => a.variant().cmp(&b.variant()),
        }
    }
}

/// A row's sort key in one column.
#[derive(Debug, Clone, PartialEq)]
enum Key {
    /// One value, or a package's distinct values in leg order.
    Values(Vec<Part>),
    /// No single value: `mixed`, or a `—` gap.
    Mixed,
    Blank,
}

impl Key {
    fn one(part: Option<Part>) -> Key {
        match part {
            Some(p) => Key::Values(vec![p]),
            None => Key::Blank,
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Key::Values(_) => 0,
            Key::Mixed => 1,
            Key::Blank => 2,
        }
    }
}

/// Values in the asked direction; `mixed` then blank after them in
/// either direction.
fn compare(a: &Key, b: &Key, desc: bool, abs: bool) -> Ordering {
    match (a, b) {
        (Key::Values(x), Key::Values(y)) => {
            let o = x
                .iter()
                .zip(y)
                .map(|(p, q)| p.cmp(q, abs))
                .find(|o| o.is_ne())
                .unwrap_or_else(|| x.len().cmp(&y.len()));
            if desc { o.reverse() } else { o }
        }
        (a, b) => a.rank().cmp(&b.rank()),
    }
}

fn number(v: f64) -> Option<Part> {
    (!v.is_nan()).then_some(Part::Num(v))
}

/// A tenor's nominal length: `3m` is 90 days, `1y` 365.
fn tenor_days(t: &str) -> u32 {
    let (digits, unit) = t.split_at(t.len().saturating_sub(1));
    let n: u32 = digits.parse().unwrap_or(u32::MAX);
    let per = match unit {
        "d" => 1,
        "w" => 7,
        "m" => 30,
        _ => 365,
    };
    n.saturating_mul(per)
}

/// A measure, status or pricing-time cell over a line's or a fold's
/// pricing state, keyed as `columns::number` paints it: a failure's `—`
/// and an unlike-currency local sum's `—` are gaps, an unpriced cell
/// blank.
fn priced_key(
    kind: ColumnKind,
    state: &LineState,
    result: Option<&PriceResult>,
    priced_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Key {
    match kind {
        ColumnKind::Measure { measure, usd } => match (state, result) {
            (LineState::Failed(_), _) => Key::Mixed,
            (_, None) => Key::Blank,
            (_, Some(r)) if !usd && r.currency.is_mixed() => Key::Mixed,
            (_, Some(r)) => Key::one(number(r.get(measure, usd))),
        },
        ColumnKind::PricedAt => Key::one(priced_at.map(|t| Part::Num(t.timestamp_millis() as f64))),
        ColumnKind::Status => Key::one(match state {
            LineState::Fresh => None,
            LineState::Stale => Some(Part::Text("pricing…".into())),
            LineState::Failed(m) => Some(Part::Text(m.clone())),
        }),
        _ => Key::Blank,
    }
}

/// What line `row` reads in an instrument-side column, as its cell paints
/// it; `None` is a blank cell. `template` is the package's token here,
/// the reading a grouping row's unanimity compares (`leg_reading`).
fn line_part(sheet: &Sheet, row: usize, kind: ColumnKind) -> Option<Part> {
    let instrument = sheet.instrument(row);
    let barrier = match instrument {
        Some(Instrument::Barrier(b)) => Some(b),
        _ => None,
    };
    match kind {
        ColumnKind::SheetName => Some(Part::Text(sheet.name.clone())),
        ColumnKind::PositionRef => Some(Part::Num(
            sheet.id(sheet.parent(row).unwrap_or(row)).0 as f64,
        )),
        ColumnKind::InstrumentRef => Some(Part::Num(sheet.id(row).0 as f64)),
        ColumnKind::Template => match sheet.parent(row).map(|p| sheet.kind(p)) {
            Some(RowKind::Package { template }) => Some(Part::Text(template.token().to_string())),
            _ => None,
        },
        ColumnKind::Qty => Some(Part::Num(sheet.qty(row) as f64)),
        ColumnKind::UnderlyingRef => instrument.map(|i| Part::Text(i.underlying().to_string())),
        ColumnKind::Expiry => instrument.map(|i| match i.expiry() {
            Expiry::Date(d) => Part::Date(*d),
            Expiry::Tenor(t) => Part::Tenor(tenor_days(t), t.clone()),
        }),
        ColumnKind::Strike => instrument.map(|i| match i.strike() {
            Strike::Absolute(v) => Part::Num(v),
            Strike::Percent(v) => Part::Pct(v),
        }),
        ColumnKind::OptionType => instrument.map(|i| {
            Part::Text(
                match i.kind() {
                    OptionKind::Call => "C",
                    OptionKind::Put => "P",
                }
                .into(),
            )
        }),
        ColumnKind::Currency => sheet
            .result(row)
            .map(|r| Part::Text(r.currency.as_str().into())),
        ColumnKind::Barrier => barrier.and_then(|b| number(b.level)),
        ColumnKind::BarrierType => {
            barrier.map(|b| Part::Text(render_barrier_kind(b.barrier).into()))
        }
        ColumnKind::SpotShift => sheet
            .shift(row)
            .spot_pct
            .or(sheet.sheet_shift().spot_pct)
            .and_then(number),
        ColumnKind::VolShift => sheet
            .shift(row)
            .vol_pts
            .or(sheet.sheet_shift().vol_pts)
            .and_then(number),
        ColumnKind::Measure { .. } | ColumnKind::PricedAt | ColumnKind::Status => None,
    }
}

fn priced(kind: ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::Measure { .. } | ColumnKind::PricedAt | ColumnKind::Status
    )
}

/// A bare line's or a leg's own cell.
fn leaf_key(sheet: &Sheet, row: usize, kind: ColumnKind) -> Key {
    if priced(kind) {
        return priced_key(
            kind,
            sheet.state(row),
            sheet.result(row),
            sheet.priced_at(row),
        );
    }
    match kind {
        // A leg's own template cell is blank; only a grouping row reads
        // the package's token through it.
        ColumnKind::Template => Key::Blank,
        _ => Key::one(line_part(sheet, row, kind)),
    }
}

/// Package `row`'s cell over `legs`, the legs its node holds.
fn package_key(sheet: &Sheet, row: usize, legs: &[usize], partial: bool, kind: ColumnKind) -> Key {
    if priced(kind) {
        if partial {
            let f = sheet.fold_legs(legs.iter().copied());
            return priced_key(kind, &f.state, f.result.as_ref(), f.priced_at);
        }
        return priced_key(
            kind,
            sheet.state(row),
            sheet.result(row),
            sheet.priced_at(row),
        );
    }
    if kind == ColumnKind::InstrumentRef {
        return Key::Blank;
    }
    if kind == ColumnKind::Qty
        && !partial
        && let Some((q, _)) = package_qty(sheet, row)
    {
        return Key::one(Some(Part::Num(q as f64)));
    }
    if aggregates(kind) {
        // Distinct values in leg order: the parts the cell joins.
        let mut parts: Vec<Part> = Vec::new();
        for &leg in legs {
            if let Some(p) = line_part(sheet, leg, kind)
                && !parts.contains(&p)
            {
                parts.push(p);
            }
        }
        return if parts.is_empty() {
            Key::Blank
        } else {
            Key::Values(parts)
        };
    }
    match kind {
        ColumnKind::SheetName => Key::one(line_part(sheet, row, kind)),
        ColumnKind::PositionRef => Key::one(Some(Part::Num(sheet.id(row).0 as f64))),
        ColumnKind::Template => match sheet.kind(row) {
            RowKind::Package { template } => Key::one(Some(Part::Text(template.token().into()))),
            _ => Key::Blank,
        },
        _ => Key::Blank,
    }
}

/// A grouping row's cell over every shown leg beneath it: a measure's
/// sum, status and pricing time from their fold, `qty` its leg count, a
/// dimension its legs' one value where unanimous (`mixed` where they
/// differ or some have none), as `group_cell_text` paints them.
fn group_key(sheet: &Sheet, legs: &[usize], kind: ColumnKind) -> Key {
    if priced(kind) {
        let f = sheet.fold_legs(legs.iter().copied());
        return priced_key(kind, &f.state, f.result.as_ref(), f.priced_at);
    }
    match kind {
        ColumnKind::Qty => Key::one(Some(Part::Num(legs.len() as f64))),
        ColumnKind::PositionRef | ColumnKind::InstrumentRef => Key::Blank,
        _ => {
            let mut value: Option<Part> = None;
            let (mut blanks, mut differ) = (false, false);
            for &leg in legs {
                match line_part(sheet, leg, kind) {
                    None => blanks = true,
                    Some(p) => match &value {
                        None => value = Some(p),
                        Some(v) if *v != p => differ = true,
                        Some(_) => {}
                    },
                }
            }
            match value {
                None => Key::Blank,
                Some(_) if blanks || differ => Key::Mixed,
                Some(v) => Key::Values(vec![v]),
            }
        }
    }
}

/// Rank every sibling set of `rollup` by `spec` (see the module doc). A
/// column the vocabulary lacks leaves the rollup as built.
pub fn rank(rollup: &mut Rollup, sheet: &Sheet, spec: &SortSpec) {
    let Some(def) = column(spec.column) else {
        return;
    };
    let kind = def.kind;
    let desc = spec.order.descending();
    let abs = is_measure(kind) && spec.order.absolute();
    // Keys for every node that ranks among siblings: the roots and the
    // children of grouping nodes (a package's legs never move).
    let mut keys: Vec<Option<Key>> = vec![None; rollup.nodes.len()];
    let ranked: Vec<usize> = rollup
        .roots
        .iter()
        .copied()
        .chain(
            rollup
                .nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Group { .. }))
                .flat_map(|n| n.children.iter().copied()),
        )
        .collect();
    for id in ranked {
        let key = match &rollup.nodes[id].kind {
            NodeKind::Group { .. } => group_key(sheet, &legs_under(rollup, id), kind),
            NodeKind::Package {
                row, legs, partial, ..
            } => package_key(sheet, *row, legs, *partial, kind),
            NodeKind::Leaf { row } => leaf_key(sheet, *row, kind),
        };
        keys[id] = Some(key);
    }
    let by_key = |a: &usize, b: &usize| match (&keys[*a], &keys[*b]) {
        (Some(x), Some(y)) => compare(x, y, desc, abs),
        _ => Ordering::Equal,
    };
    // Stable: ties keep the rollup's own order.
    rollup.roots.sort_by(by_key);
    for n in 0..rollup.nodes.len() {
        if matches!(rollup.nodes[n].kind, NodeKind::Group { .. }) {
            rollup.nodes[n].children.sort_by(by_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rollup::{self, effective_chain};
    use crate::core::sheet::tests::{at, push, result};
    use crate::core::shorthand::parse_builtin;
    use crate::core::visibility::Visibility;
    use geode_core::clock::Clock;
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::pricing::Currency;

    fn sheet(lines: &[&str]) -> Sheet {
        let mut s = Sheet::new("t");
        for l in lines {
            push(&mut s, vec![parse_builtin(l).unwrap()]);
        }
        s
    }

    /// Price every line at `npv` per its index in `npvs` (sheet rows).
    fn price(s: &mut Sheet, npvs: &[(usize, f64)]) {
        for &(row, v) in npvs {
            let id = s.id(row);
            let rev = s.revision(row);
            s.deliver(id, rev, Ok(result(v)), at(0));
        }
        s.fold_packages();
    }

    fn tree(s: &Sheet, chain: &[&str]) -> Rollup {
        let dims = DerivedDimensions::default();
        let chain: Vec<String> = chain.iter().map(|c| c.to_string()).collect();
        rollup::build(
            s,
            &Visibility::all(s),
            &effective_chain(&chain, &dims),
            &dims,
            Clock::utc(),
        )
    }

    fn spec(column: &'static str, order: SortOrder) -> SortSpec {
        SortSpec { column, order }
    }

    /// The display order as each ranked node's first sheet row: a leaf's
    /// row, a package's row, a group's label.
    fn order(s: &Sheet, r: &Rollup) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(s: &Sheet, r: &Rollup, id: usize, out: &mut Vec<String>) {
            let n = &r.nodes[id];
            match &n.kind {
                NodeKind::Group { label, .. } => out.push(format!("[{label}]")),
                NodeKind::Package { row, .. } | NodeKind::Leaf { row } => {
                    out.push(s.shorthand(*row).lines().next().unwrap_or("").to_string())
                }
            }
            for &c in &n.children {
                walk(s, r, c, out);
            }
        }
        for &root in &r.roots {
            walk(s, r, root, &mut out);
        }
        out
    }

    fn ranked(s: &Sheet, chain: &[&str], spec: SortSpec) -> Vec<String> {
        let mut r = tree(s, chain);
        rank(&mut r, s, &spec);
        order(s, &r)
    }

    const FLAT: [&str; 4] = [
        "SPX Z26 5000 C",
        "SPX H27 4000 P",
        "SPX Z26 4800/5200 CS",
        "SPX M27 4500 C",
    ];

    #[test]
    fn a_measure_sort_ranks_lines_and_packages_and_legs_ride_with_their_package() {
        let mut s = sheet(&FLAT);
        // Rows: 0 line, 1 line, 2 CS (legs 3, 4), 5 line.
        price(
            &mut s,
            &[(0, 30.0), (1, -50.0), (3, 20.0), (4, 5.0), (5, 10.0)],
        );
        // CS npv = 20 - 5 = 15.
        assert_eq!(
            ranked(&s, &[], spec("npv", SortOrder::Desc)),
            vec![
                "SPX Z26 5000 C",
                "SPX Z26 4800/5200 CS",
                "SPX Z26 4800 C",
                "-1 SPX Z26 5200 C",
                "SPX M27 4500 C",
                "SPX H27 4000 P",
            ],
            "30, 15 (its legs beneath it in sheet order), 10, -50"
        );
        assert_eq!(
            ranked(&s, &[], spec("npv", SortOrder::AbsDesc))[0],
            "SPX H27 4000 P",
            "|-50| is the biggest"
        );
        assert_eq!(
            ranked(&s, &[], spec("npv", SortOrder::Asc))[0],
            "SPX H27 4000 P"
        );
    }

    #[test]
    fn unpriced_and_failed_rows_sort_after_values_in_both_directions() {
        let mut s = sheet(&[
            "SPX Z26 5000 C",
            "SPX Z26 4000 C",
            "SPX Z26 3000 C",
            "SPX Z26 2000 C",
        ]);
        price(&mut s, &[(0, 1.0), (3, 2.0)]);
        let id = s.id(1);
        let rev = s.revision(1);
        s.deliver(id, rev, Err("no vol".into()), at(0));
        // Row 1 failed (`—`), row 2 unpriced (blank).
        for order in [SortOrder::Asc, SortOrder::Desc] {
            let got = ranked(&s, &[], spec("npv", order));
            assert_eq!(
                &got[2..],
                &["SPX Z26 4000 C", "SPX Z26 3000 C"],
                "{order:?}: the failed gap, then the blank"
            );
        }
    }

    #[test]
    fn a_mixed_currency_local_sum_is_a_gap_and_its_usd_twin_a_value() {
        let mut s = sheet(&["SPX Z26 5000 C", "SPX Z26 4800/5200 CS", "SPX Z26 4000 C"]);
        price(&mut s, &[(0, 1.0), (3, 9.0), (4, 2.0)]);
        // Price the long leg (row 2) in EUR: the package's local npv is a gap.
        let id = s.id(2);
        let rev = s.revision(2);
        let mut eur = result(100.0);
        eur.currency = Currency::parse("EUR").unwrap();
        s.deliver(id, rev, Ok(eur), at(0));
        s.fold_packages();
        assert!(s.result(1).unwrap().currency.is_mixed(), "precondition");
        let local = ranked(&s, &[], spec("npv", SortOrder::Desc));
        assert_eq!(local.first().unwrap(), "SPX Z26 4000 C");
        assert_eq!(
            local[2], "SPX Z26 4800/5200 CS",
            "the gap ranks after values"
        );
        let usd = ranked(&s, &[], spec("npv_usd", SortOrder::Desc));
        assert_eq!(
            usd[0], "SPX Z26 4800/5200 CS",
            "the usd twin sums: 100·1.08 − 9·1.08"
        );
    }

    #[test]
    fn an_expiry_sorts_as_a_date_not_as_its_code() {
        // As text H27 < M27 < Z26; as dates Z26 < H27 < M27.
        let s = sheet(&[
            "SPX M27 4500 C",
            "SPX Z26 5000 C",
            "SPX H27 4000 P",
            "SPX 3m 100% C",
        ]);
        assert_eq!(
            ranked(&s, &[], spec("expiry", SortOrder::Asc)),
            vec![
                "SPX Z26 5000 C",
                "SPX H27 4000 P",
                "SPX M27 4500 C",
                "SPX 3m 100% C"
            ],
            "dates in order, the tenor after them"
        );
        assert_eq!(
            ranked(&s, &[], spec("expiry", SortOrder::Desc)),
            vec![
                "SPX 3m 100% C",
                "SPX M27 4500 C",
                "SPX H27 4000 P",
                "SPX Z26 5000 C"
            ],
        );
    }

    #[test]
    fn a_strike_sorts_by_number_not_text() {
        let s = sheet(&["SPX Z26 950 C", "SPX Z26 10000 C", "SPX Z26 1000 C"]);
        assert_eq!(
            ranked(&s, &[], spec("strike", SortOrder::Asc)),
            vec!["SPX Z26 950 C", "SPX Z26 1000 C", "SPX Z26 10000 C"]
        );
    }

    #[test]
    fn ties_keep_sheet_order_and_text_compares_by_bytes() {
        let s = sheet(&["NDX Z26 1 C", "SPX Z26 2 C", "NDX Z26 3 C", "HSI Z26 4 C"]);
        assert_eq!(
            ranked(&s, &[], spec("underlying_ref", SortOrder::Asc)),
            vec!["HSI Z26 4 C", "NDX Z26 1 C", "NDX Z26 3 C", "SPX Z26 2 C"]
        );
        assert_eq!(
            ranked(&s, &[], spec("underlying_ref", SortOrder::Desc)),
            vec!["SPX Z26 2 C", "NDX Z26 1 C", "NDX Z26 3 C", "HSI Z26 4 C"],
            "descending reverses values, never ties"
        );
    }

    #[test]
    fn group_rows_rank_by_their_folded_value_at_every_level() {
        let mut s = sheet(&[
            "SPX Z26 5000 C",
            "NDX Z26 4000 P",
            "SPX H27 4500 C",
            "NDX H27 3000 C",
        ]);
        price(&mut s, &[(0, 1.0), (1, 40.0), (2, 9.0), (3, 2.0)]);
        // SPX: Z26 1, H27 9 → 10. NDX: Z26 40, H27 2 → 42.
        let got = ranked(
            &s,
            &["underlying_ref", "expiry"],
            spec("npv", SortOrder::Desc),
        );
        assert_eq!(
            got,
            vec![
                "[NDX]",
                "[2026-12-18]",
                "NDX Z26 4000 P",
                "[2027-03-19]",
                "NDX H27 3000 C",
                "[SPX]",
                "[2027-03-19]",
                "SPX H27 4500 C",
                "[2026-12-18]",
                "SPX Z26 5000 C",
            ]
        );
    }

    #[test]
    fn a_group_dimension_ranks_unanimous_values_then_mixed_then_blank() {
        // Grouped by underlying, sorted by option type: SPX all calls,
        // NDX all puts, HSI mixed.
        let s = sheet(&["HSI Z26 1 C", "SPX Z26 1 C", "HSI Z26 2 P", "NDX Z26 1 P"]);
        let got = ranked(
            &s,
            &["underlying_ref"],
            spec("option_type", SortOrder::Desc),
        );
        let groups: Vec<&String> = got.iter().filter(|l| l.starts_with('[')).collect();
        assert_eq!(groups, vec!["[NDX]", "[SPX]", "[HSI]"], "P, C, then mixed");
    }

    #[test]
    fn a_partly_hidden_package_ranks_by_the_legs_it_paints() {
        let mut s = sheet(&["SPX Z26 4800/5200 CS", "SPX Z26 4000 C"]);
        // Legs 1 (+), 2 (−); line 3.
        price(&mut s, &[(1, 100.0), (2, 90.0), (3, 50.0)]);
        // Whole package: 100 − 90 = 10 < 50. Hide leg 2: the row paints 100.
        let dims = DerivedDimensions::default();
        let mut vis = Visibility::all(&s);
        let scope = geode_core::scope::Scope {
            expression: Some(geode_core::scope::parse_expr("strike != 5200").unwrap()),
            ..Default::default()
        };
        vis = crate::core::visibility::apply_scope(&s, &scope, &dims, Clock::utc()).unwrap_or(vis);
        assert!(vis.is_partial(&s, 0), "precondition: leg 2 hidden");
        let mut r = rollup::build(&s, &vis, &effective_chain(&[], &dims), &dims, Clock::utc());
        rank(&mut r, &s, &spec("npv", SortOrder::Desc));
        assert_eq!(
            order(&s, &r)[0],
            "SPX Z26 4800/5200 CS",
            "100 (shown leg) > 50"
        );
    }

    #[test]
    fn a_package_whose_legs_disagree_compares_by_its_parts_in_leg_order() {
        // CS strikes 4800/5200 sit between 4700 and 4900 by the first part.
        let s = sheet(&["SPX Z26 4900 C", "SPX Z26 4800/5200 CS", "SPX Z26 4700 C"]);
        assert_eq!(
            ranked(&s, &[], spec("strike", SortOrder::Asc)),
            vec![
                "SPX Z26 4700 C",
                "SPX Z26 4800/5200 CS",
                "SPX Z26 4800 C",
                "-1 SPX Z26 5200 C",
                "SPX Z26 4900 C",
            ]
        );
    }

    #[test]
    fn ranking_never_changes_the_sheet_and_an_unknown_column_changes_nothing() {
        let mut s = sheet(&FLAT);
        price(&mut s, &[(0, 3.0), (1, 2.0), (3, 1.0), (5, 4.0)]);
        let before: Vec<String> = (0..s.len()).map(|r| s.shorthand(r)).collect();
        let built = tree(&s, &[]);
        let mut r = built.clone();
        rank(&mut r, &s, &spec("nonesuch", SortOrder::Desc));
        assert_eq!(r, built);
        rank(&mut r, &s, &spec("npv", SortOrder::Desc));
        assert_ne!(r, built);
        assert_eq!(
            before,
            (0..s.len()).map(|r| s.shorthand(r)).collect::<Vec<_>>()
        );
    }
}
