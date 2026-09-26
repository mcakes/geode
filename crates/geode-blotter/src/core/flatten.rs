//! The visible-row list (Phase 3 spec §6.1, §6.3): a DFS over the tree
//! index that descends only into open nodes, so its cost tracks the
//! output, not the materialised rows. Runs on snapshot arrival, expand,
//! collapse and sort — never per frame. Siblings are sorted here when a
//! sort is set; view-shaping in-app (PHILOSOPHY §1) that leaves the
//! compiler's order untouched.

use crate::core::expansion::{Expansion, Path};
use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::snapshot::Snapshot;
use std::cmp::Ordering;

/// How siblings are ranked on the sort column (spec §6.3). The two
/// absolute orders compare magnitudes — a trader hunting the biggest
/// exposure does not care which way it points — and only mean anything
/// on a measure: on a text column they fall back to their signed
/// direction rather than refusing or doing something odd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
    AbsDesc,
    AbsAsc,
}

impl SortOrder {
    pub fn descending(self) -> bool {
        matches!(self, SortOrder::Desc | SortOrder::AbsDesc)
    }

    pub fn absolute(self) -> bool {
        matches!(self, SortOrder::AbsDesc | SortOrder::AbsAsc)
    }

    /// The order a column of the given kind can actually show: a text
    /// column has no magnitude, so an absolute order asked of it is its
    /// signed direction. `:sort <textcol> abs` lands here so the tile's
    /// state, the header and the rows all say the same thing.
    pub fn on_column(self, measure: bool) -> SortOrder {
        match (self, measure) {
            (SortOrder::AbsDesc, false) => SortOrder::Desc,
            (SortOrder::AbsAsc, false) => SortOrder::Asc,
            (order, _) => order,
        }
    }

    /// What `s` (`absolute == false`) or `S` (`absolute == true`) does to
    /// a column whose current order is `current`: `s` walks asc → desc →
    /// clear, `S` walks abs desc → abs asc → clear, and either key pressed
    /// while the other's order is showing starts its own cycle afresh
    /// rather than continuing a cycle the trader did not choose. `S` on a
    /// column with no magnitude (`measure == false`) leaves `current` as
    /// it is — a sort it did apply would just be `s`'s.
    pub fn cycle(current: Option<SortOrder>, absolute: bool, measure: bool) -> Option<SortOrder> {
        if absolute && !measure {
            return current;
        }
        if absolute {
            match current {
                Some(SortOrder::AbsDesc) => Some(SortOrder::AbsAsc),
                Some(SortOrder::AbsAsc) => None,
                _ => Some(SortOrder::AbsDesc),
            }
        } else {
            match current {
                Some(SortOrder::Asc) => Some(SortOrder::Desc),
                Some(SortOrder::Desc) => None,
                _ => Some(SortOrder::Asc),
            }
        }
    }

    /// What a header click does to a column whose current order is
    /// `current`: one cycle through every order the column can show,
    /// desc first because that is where gpui-component's own click cycle
    /// started before this crate took it over (user ruling 2026-09-12:
    /// clicking through the header reaches the absolute orders too). A
    /// measure walks cleared → desc → asc → abs desc → abs asc → cleared;
    /// a column with no magnitude skips the absolute pair. A click on a
    /// column that is not the sorted one starts at desc.
    pub fn click_cycle(current: Option<SortOrder>, measure: bool) -> Option<SortOrder> {
        match current {
            Some(SortOrder::Desc) => Some(SortOrder::Asc),
            Some(SortOrder::Asc) if measure => Some(SortOrder::AbsDesc),
            Some(SortOrder::Asc) => None,
            Some(SortOrder::AbsDesc) => Some(SortOrder::AbsAsc),
            Some(SortOrder::AbsAsc) => None,
            None => Some(SortOrder::Desc),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSpec {
    /// The snapshot column name, as `PlannedColumn::name` spells it. A name
    /// rather than a position because the plan reorders under a column drag
    /// and shortens when a column is hidden or folded into the tree, and an
    /// index that survives either change names a different column.
    pub column: String,
    pub order: SortOrder,
}

pub fn flatten(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    expansion: &Expansion,
    sort: Option<&SortSpec>,
    out: &mut Vec<u32>,
) {
    out.clear();
    let tree = snapshot.tree();
    let mut path: Path = Vec::new();
    let mut scratch: Vec<u32> = Vec::new();
    // The root is always open: a blotter showing one row is not a
    // blotter. Several roots (a flat result) are all listed.
    for &root in tree.roots() {
        out.push(root);
        descend(
            snapshot,
            plan,
            expansion,
            sort,
            root as usize,
            &mut path,
            &mut scratch,
            out,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn descend(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    expansion: &Expansion,
    sort: Option<&SortSpec>,
    node: usize,
    path: &mut Path,
    scratch: &mut Vec<u32>,
    out: &mut Vec<u32>,
) {
    let children = snapshot.tree().children(node);
    if children.is_empty() {
        return;
    }
    match sort {
        None => {
            // No allocation: the CSR slice is already the sibling order.
            for &child in children {
                visit_child(snapshot, plan, expansion, sort, child, path, scratch, out);
            }
        }
        Some(spec) => {
            scratch.clear();
            scratch.extend_from_slice(children);
            sort_siblings(snapshot, plan, spec, scratch);
            // `scratch` is reused by the recursive calls below, so the
            // order decided for this level is copied out first.
            let ordered: Vec<u32> = scratch.clone();
            for child in ordered {
                visit_child(snapshot, plan, expansion, sort, child, path, scratch, out);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn visit_child(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    expansion: &Expansion,
    sort: Option<&SortSpec>,
    child: u32,
    path: &mut Path,
    scratch: &mut Vec<u32>,
    out: &mut Vec<u32>,
) {
    out.push(child);
    let c = child as usize;
    if !snapshot.tree().has_children(c) {
        return;
    }
    path.push(plan.tree_text(snapshot, c).map(str::to_string));
    if expansion.is_open(path) {
        descend(snapshot, plan, expansion, sort, c, path, scratch, out);
    }
    path.pop();
}

fn sort_siblings(snapshot: &Snapshot, plan: &ColumnPlan, spec: &SortSpec, rows: &mut [u32]) {
    let Some(column) = plan
        .position_of(&spec.column)
        .and_then(|i| plan.columns.get(i))
    else {
        return;
    };
    let idx = column.index;
    let numeric = column.kind == ColumnKind::Measure;
    // Magnitude only ever applies to a number; a text column's `abs` is
    // its signed direction.
    let absolute = numeric && spec.order.absolute();
    let key = |v: f64| if absolute { v.abs() } else { v };
    rows.sort_by(|a, b| {
        let (a, b) = (*a as usize, *b as usize);
        let ord = match idx {
            None => Ordering::Equal,
            Some(i) if numeric => match (number_at(snapshot, i, a), number_at(snapshot, i, b)) {
                (Some(x), Some(y)) => key(x).total_cmp(&key(y)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            Some(i) => match (snapshot.text_at(i, a), snapshot.text_at(i, b)) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        };
        // NULL last in both directions; ties keep row order (stable sort).
        match (spec.order.descending(), ord) {
            (_, Ordering::Equal) => Ordering::Equal,
            (true, o)
                if is_null(snapshot, idx, numeric, a) || is_null(snapshot, idx, numeric, b) =>
            {
                o
            }
            (true, o) => o.reverse(),
            (false, o) => o,
        }
    });
}

/// A measure cell as the comparator sees it: NULL (a `NonAttributable`
/// cell included) and NaN are both `None`. NaN — a `SUM` over a source
/// column holding one — would otherwise compare `Equal` to everything
/// while everything else orders, which is not a total order, and
/// `sort_by` panics on one of those (Rust ≥ 1.81); as `None` it sorts
/// last like a NULL and the remaining values compare totally.
fn number_at(snapshot: &Snapshot, i: usize, row: usize) -> Option<f64> {
    snapshot.f64_at(i, row).filter(|v| !v.is_nan())
}

fn is_null(snapshot: &Snapshot, idx: Option<usize>, numeric: bool, row: usize) -> bool {
    match idx {
        None => true,
        Some(i) if numeric => number_at(snapshot, i, row).is_none(),
        Some(i) => snapshot.text_at(i, row).is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::expansion::{Expansion, path_of};
    use crate::core::plan::ColumnPlan;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;

    fn dim(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 4],
            scope_semantics: ScopeSemantics::Direct,
        }
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// Root; L1, L2; L1/SPX, L2/SPX, L1/NDX, L2/NDX; L1/SPX/P1, L1/SPX/P2.
    /// Rows 3 and 5 are L1's children, interleaved with L2's.
    fn snapshot() -> Snapshot {
        Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Dict(vec![
                        None,
                        s("L1"),
                        s("L2"),
                        s("L1"),
                        s("L2"),
                        s("L1"),
                        s("L2"),
                        s("L1"),
                        s("L1"),
                    ]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Dict(vec![
                        None,
                        None,
                        None,
                        s("SPX"),
                        s("SPX"),
                        s("NDX"),
                        s("NDX"),
                        s("SPX"),
                        s("SPX"),
                    ]),
                ),
                (
                    dim("position_ref"),
                    TestColumn::Str(vec![
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some("P1"),
                        Some("P2"),
                    ]),
                ),
                (
                    dim("row_depth"),
                    TestColumn::I32(vec![0, 1, 1, 2, 2, 2, 2, 3, 3]),
                ),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![
                        Some(100.0),
                        Some(60.0),
                        Some(40.0),
                        Some(10.0),
                        Some(30.0),
                        Some(50.0),
                        Some(10.0),
                        None,
                        Some(4.0),
                    ]),
                ),
            ],
            3,
        )
    }

    fn view() -> ViewSpec {
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\", \"position_ref\"]\n[[t.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.remove(0)
    }

    fn visible(expansion: &Expansion, sort: Option<&SortSpec>) -> Vec<u32> {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut out = Vec::new();
        flatten(&snap, &plan, expansion, sort, &mut out);
        out
    }

    /// Like `visible`, but against a caller-supplied plan: needed for a
    /// plan mutated between two flattens, such as a column move. Always
    /// flattens with the default (root-only) expansion; unlike `visible`,
    /// there is no expansion parameter to vary.
    fn visible_with(snap: &Snapshot, plan: &ColumnPlan, sort: Option<&SortSpec>) -> Vec<u32> {
        let mut out = Vec::new();
        flatten(snap, plan, &Expansion::default(), sort, &mut out);
        out
    }

    #[test]
    fn collapsed_shows_the_root_and_its_children_only_when_the_root_is_open() {
        // The grand total is always open: a blotter that shows one row is
        // not a blotter. Its children are the first level.
        assert_eq!(visible(&Expansion::default(), None), vec![0, 1, 2]);
    }

    #[test]
    fn opening_a_node_shows_its_children_in_row_order_and_descends_only_into_open_nodes() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut e = Expansion::default();
        e.open(path_of(&snap, &plan, 1));
        assert_eq!(visible(&e, None), vec![0, 1, 3, 5, 2]);
        e.open(path_of(&snap, &plan, 3));
        assert_eq!(visible(&e, None), vec![0, 1, 3, 7, 8, 5, 2]);
        e.open_all();
        assert_eq!(visible(&e, None), vec![0, 1, 3, 7, 8, 5, 2, 4, 6]);
    }

    #[test]
    fn a_sort_orders_siblings_within_their_parent_with_null_last() {
        let mut e = Expansion::default();
        e.open_all();
        let delta = SortSpec {
            column: "delta01".to_string(),
            order: SortOrder::Desc,
        };
        assert_eq!(
            visible(&e, Some(&delta)),
            vec![0, 1, 5, 3, 8, 7, 2, 4, 6],
            "L1 (60) before L2 (40); NDX 50 before SPX 10; P2 4 before P1 NULL"
        );
        let asc = SortSpec {
            column: "delta01".to_string(),
            order: SortOrder::Asc,
        };
        assert_eq!(
            visible(&e, Some(&asc)),
            vec![0, 2, 6, 4, 1, 3, 8, 7, 5],
            "ascending, NULL still last"
        );
    }

    #[test]
    fn a_sort_survives_a_column_move_because_it_names_the_column() {
        let snap = signed_snapshot();
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[t.columns]]\nname = \"delta01\"\n[[t.columns]]\nname = \"desk\"\nkind = \"dimension\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let mut plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        let delta = plan
            .columns
            .iter()
            .position(|c| c.name == "delta01")
            .expect("the fixture has delta01");
        let spec = SortSpec {
            column: "delta01".to_string(),
            order: SortOrder::Desc,
        };
        let before = visible_with(&snap, &plan, Some(&spec));
        let unsorted = visible_with(&snap, &plan, None);
        assert_ne!(
            before, unsorted,
            "the fixture must actually reorder rows under this sort, or a lookup that always \
             misses and falls back to default order would pass this test too"
        );

        // Moving a column must not change which column the sort names.
        plan.move_column(delta, delta + 1);
        let after = visible_with(&snap, &plan, Some(&spec));

        assert_eq!(
            before, after,
            "the sort follows delta01, not the position it used to hold"
        );
    }

    /// Root; four children at depth 1 carrying -60, 40, 10 and NULL on
    /// `delta01` (plan column 1) and a `desk` text column (plan column 2)
    /// reading C, A, B, NULL. The absolute orders rank by magnitude —
    /// -60 is the biggest exposure — where the signed orders put it
    /// last; NULL is last in all four.
    fn signed_snapshot() -> Snapshot {
        Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Dict(vec![None, s("A"), s("B"), s("C"), s("D")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 1, 1])),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![Some(-10.0), Some(-60.0), Some(40.0), Some(10.0), None]),
                ),
                (
                    dim("desk"),
                    TestColumn::Dict(vec![None, s("C"), s("A"), s("B"), None]),
                ),
            ],
            1,
        )
    }

    fn signed_visible(column: &str, order: SortOrder) -> Vec<u32> {
        let snap = signed_snapshot();
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[t.columns]]\nname = \"delta01\"\n[[t.columns]]\nname = \"desk\"\nkind = \"dimension\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        assert_eq!(plan.columns[2].kind, ColumnKind::Dimension, "sanity");
        let spec = SortSpec {
            column: column.to_string(),
            order,
        };
        let mut out = Vec::new();
        flatten(&snap, &plan, &Expansion::default(), Some(&spec), &mut out);
        out
    }

    #[test]
    fn absolute_orders_compare_magnitudes_and_keep_null_last() {
        let by = |order| signed_visible("delta01", order);
        assert_eq!(
            by(SortOrder::Desc),
            vec![0, 2, 3, 1, 4],
            "40, 10, -60, NULL"
        );
        assert_eq!(by(SortOrder::Asc), vec![0, 1, 3, 2, 4], "-60, 10, 40, NULL");
        assert_eq!(
            by(SortOrder::AbsDesc),
            vec![0, 1, 2, 3, 4],
            "|-60|, |40|, |10|, NULL"
        );
        assert_eq!(
            by(SortOrder::AbsAsc),
            vec![0, 3, 2, 1, 4],
            "|10|, |40|, |-60|, NULL"
        );
    }

    /// Forty siblings, every fourth one NaN: enough for `sort_by`'s
    /// total-order check to trip if NaN compared `Equal` to everything.
    /// NaN sorts last like NULL, in both directions.
    #[test]
    fn nan_sorts_last_like_null_and_never_panics_the_sort() {
        let n = 40usize;
        let lhu: Vec<Option<String>> = std::iter::once(None)
            .chain((0..n).map(|i| s(&format!("R{i:02}"))))
            .collect();
        let depth: Vec<i32> = std::iter::once(0).chain((0..n).map(|_| 1)).collect();
        let values: Vec<Option<f64>> = std::iter::once(Some(0.0))
            .chain((0..n).map(|i| {
                if i % 4 == 3 {
                    Some(f64::NAN)
                } else {
                    Some((n - i) as f64 * if i % 2 == 0 { -1.0 } else { 1.0 })
                }
            }))
            .collect();
        let snap = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(lhu)),
                (dim("row_depth"), TestColumn::I32(depth)),
                (dim("delta01"), TestColumn::F64(values.clone())),
            ],
            1,
        );
        let text =
            "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[t.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        for order in [
            SortOrder::Asc,
            SortOrder::Desc,
            SortOrder::AbsDesc,
            SortOrder::AbsAsc,
        ] {
            let spec = SortSpec {
                column: "delta01".to_string(),
                order,
            };
            let mut out = Vec::new();
            flatten(&snap, &plan, &Expansion::default(), Some(&spec), &mut out);
            let nan_count = (0..n).filter(|i| i % 4 == 3).count();
            let tail = &out[out.len() - nan_count..];
            assert!(
                tail.iter().all(|&r| values[r as usize].unwrap().is_nan()),
                "{order:?}: every NaN row is at the end, got {tail:?}"
            );
            let head = &out[1..out.len() - nan_count];
            assert!(
                head.iter().all(|&r| !values[r as usize].unwrap().is_nan()),
                "{order:?}: no NaN before the tail"
            );
        }
    }

    #[test]
    fn the_key_cycles_walk_their_own_orders_and_restart_from_the_others() {
        use SortOrder::*;
        let s = |current| SortOrder::cycle(current, false, true);
        let big_s = |current| SortOrder::cycle(current, true, true);
        assert_eq!(s(None), Some(Asc));
        assert_eq!(s(Some(Asc)), Some(Desc));
        assert_eq!(s(Some(Desc)), None);
        assert_eq!(big_s(None), Some(AbsDesc));
        assert_eq!(big_s(Some(AbsDesc)), Some(AbsAsc));
        assert_eq!(big_s(Some(AbsAsc)), None);
        // Crossing over restarts the pressed key's own cycle.
        assert_eq!(s(Some(AbsDesc)), Some(Asc));
        assert_eq!(s(Some(AbsAsc)), Some(Asc));
        assert_eq!(big_s(Some(Asc)), Some(AbsDesc));
        assert_eq!(big_s(Some(Desc)), Some(AbsDesc));
    }

    #[test]
    fn a_header_click_walks_every_order_a_measure_can_show_desc_first() {
        use SortOrder::*;
        let mut current = None;
        let mut seen = Vec::new();
        for _ in 0..5 {
            current = SortOrder::click_cycle(current, true);
            seen.push(current);
        }
        assert_eq!(
            seen,
            vec![Some(Desc), Some(Asc), Some(AbsDesc), Some(AbsAsc), None]
        );
    }

    #[test]
    fn a_header_click_on_a_text_column_skips_the_absolute_pair() {
        use SortOrder::*;
        let mut current = None;
        let mut seen = Vec::new();
        for _ in 0..3 {
            current = SortOrder::click_cycle(current, false);
            seen.push(current);
        }
        assert_eq!(seen, vec![Some(Desc), Some(Asc), None]);
        // A text column can never hold an absolute order (`on_column`),
        // but were one there, a click still ends the cycle rather than
        // looping inside the pair.
        assert_eq!(SortOrder::click_cycle(Some(AbsDesc), false), Some(AbsAsc));
        assert_eq!(SortOrder::click_cycle(Some(AbsAsc), false), None);
    }

    #[test]
    fn shift_s_is_inert_on_a_column_with_no_magnitude_where_s_is_not() {
        use SortOrder::*;
        for current in [None, Some(Asc), Some(Desc)] {
            assert_eq!(
                SortOrder::cycle(current, true, false),
                current,
                "{current:?}"
            );
        }
        assert_eq!(SortOrder::cycle(None, false, false), Some(Asc));
        assert_eq!(SortOrder::cycle(Some(Asc), false, false), Some(Desc));
    }

    #[test]
    fn an_absolute_order_asked_of_a_text_column_becomes_its_signed_direction() {
        use SortOrder::*;
        assert_eq!(AbsDesc.on_column(false), Desc);
        assert_eq!(AbsAsc.on_column(false), Asc);
        assert_eq!(Desc.on_column(false), Desc);
        for order in [Asc, Desc, AbsDesc, AbsAsc] {
            assert_eq!(order.on_column(true), order, "a measure keeps {order:?}");
        }
    }

    #[test]
    fn an_absolute_order_on_a_text_column_is_its_signed_direction() {
        // Text has no magnitude; `abs` on it means the plain direction
        // rather than an odd or refused sort.
        let by = |order| signed_visible("desk", order);
        assert_eq!(by(SortOrder::Asc), vec![0, 2, 3, 1, 4], "A, B, C, NULL");
        assert_eq!(by(SortOrder::Desc), vec![0, 1, 3, 2, 4], "C, B, A, NULL");
        assert_eq!(by(SortOrder::AbsAsc), by(SortOrder::Asc));
        assert_eq!(by(SortOrder::AbsDesc), by(SortOrder::Desc));
    }

    #[test]
    fn expansion_survives_a_snapshot_that_reorders_siblings() {
        // The same tree with L2 before L1 at depth 1: the open path still
        // names L1 and L1 still opens.
        let reordered = Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Dict(vec![None, s("L2"), s("L1"), s("L1")]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Dict(vec![None, None, None, s("SPX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2])),
            ],
            2,
        );
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut e = Expansion::default();
        e.open(path_of(&snap, &plan, 1)); // L1
        let plan2 = ColumnPlan::build(&view(), reordered.grouping(), &reordered);
        let mut out = Vec::new();
        flatten(&reordered, &plan2, &e, None, &mut out);
        assert_eq!(out, vec![0, 1, 2, 3]);
    }

    #[test]
    fn the_output_buffer_is_reused() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let mut out = Vec::with_capacity(64);
        let ptr = out.as_ptr();
        flatten(&snap, &plan, &Expansion::default(), None, &mut out);
        assert_eq!(out.as_ptr(), ptr, "no reallocation for a small tree");
    }
}
