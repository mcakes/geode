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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    /// Index into `ColumnPlan::columns`.
    pub column: usize,
    pub descending: bool,
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
    let Some(column) = plan.columns.get(spec.column) else {
        return;
    };
    let idx = column.index;
    let numeric = column.kind == ColumnKind::Measure;
    rows.sort_by(|a, b| {
        let (a, b) = (*a as usize, *b as usize);
        let ord = match idx {
            None => Ordering::Equal,
            Some(i) if numeric => match (snapshot.f64_at(i, a), snapshot.f64_at(i, b)) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
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
        match (spec.descending, ord) {
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

fn is_null(snapshot: &Snapshot, idx: Option<usize>, numeric: bool, row: usize) -> bool {
    match idx {
        None => true,
        Some(i) if numeric => snapshot.f64_at(i, row).is_none(),
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
            column: 1,
            descending: true,
        };
        assert_eq!(
            visible(&e, Some(&delta)),
            vec![0, 1, 5, 3, 8, 7, 2, 4, 6],
            "L1 (60) before L2 (40); NDX 50 before SPX 10; P2 4 before P1 NULL"
        );
        let asc = SortSpec {
            column: 1,
            descending: false,
        };
        assert_eq!(
            visible(&e, Some(&asc)),
            vec![0, 2, 6, 4, 1, 3, 8, 7, 5],
            "ascending, NULL still last"
        );
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
