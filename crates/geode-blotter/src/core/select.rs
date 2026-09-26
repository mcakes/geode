//! The blotter's selection summary (grid selection spec §3.3, §4.3):
//! per selected measure column, the aggregate over the selection's
//! top-most rows only — a group row already carries its children's
//! total — with non-additive cells refusing a sum.

use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::attribution::Attribution;
use geode_core::grid::selection::{Accumulator, Resolved, describe, top_most};
use geode_core::snapshot::Snapshot;

pub fn summarize(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    shown: &[u32],
    resolved: &Resolved,
) -> Vec<(String, String)> {
    let end = resolved.rows.end.min(shown.len());
    let start = resolved.rows.start.min(end);
    let rows: Vec<usize> = shown[start..end].iter().map(|&r| r as usize).collect();
    let tree = snapshot.tree();
    let rows = top_most(&rows, snapshot.rows(), |r| tree.parent(r));
    let measures: Vec<usize> = resolved
        .cols
        .clone()
        .filter(|&c| {
            plan.columns
                .get(c)
                .is_some_and(|p| p.kind == ColumnKind::Measure)
        })
        .collect();
    let extremes = measures.len() == 1;
    measures
        .into_iter()
        .map(|c| {
            let column = &plan.columns[c];
            let mut acc = Accumulator::default();
            if let Some(idx) = column.index {
                for &r in &rows {
                    let additive = plan.attribution(c, tree.depth(r)) == Attribution::Additive;
                    acc.add(snapshot.f64_at(idx, r), additive);
                }
            }
            (
                column.label.clone(),
                describe(&acc.finish(), &column.format, extremes),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::grid::selection::{SelectKind, resolve};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};

    // Root 9; L1 5 (child SPX 5); L2 4. `det` is DeterminedNonAdditive at depth 1.
    fn fixture() -> (Snapshot, ColumnPlan, Vec<u32>) {
        let meta = |n: &str, a: Vec<Attribution>| ColumnMeta {
            name: n.into(),
            attribution_by_depth: a,
            scope_semantics: ScopeSemantics::Direct,
        };
        let add = || vec![Attribution::Additive; 3];
        let snap = Snapshot::for_tests(
            vec![
                (
                    meta("lhu", add()),
                    TestColumn::Dict(vec![
                        None,
                        Some("L1".into()),
                        Some("L2".into()),
                        Some("L1".into()),
                    ]),
                ),
                (
                    meta("underlying_ref", add()),
                    TestColumn::Dict(vec![None, None, None, Some("SPX".into())]),
                ),
                (meta("row_depth", add()), TestColumn::I32(vec![0, 1, 1, 2])),
                (
                    meta("delta01", add()),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0), Some(5.0)]),
                ),
                (
                    meta(
                        "det",
                        vec![
                            Attribution::Additive,
                            Attribution::DeterminedNonAdditive,
                            Attribution::DeterminedNonAdditive,
                        ],
                    ),
                    TestColumn::F64(vec![Some(1.0), Some(2.0), Some(2.0), Some(2.0)]),
                ),
            ],
            2,
        );
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n\
            [[t.columns]]\nname = \"delta01\"\n[[t.columns]]\nname = \"det\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = geode_core::view::ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        // Fully expanded flatten: root, L1, SPX, L2.
        (snap, plan, vec![0, 1, 3, 2])
    }

    #[test]
    fn a_group_with_its_child_sums_the_group_once() {
        let (snap, plan, shown) = fixture();
        let delta = plan.position_of("delta01").unwrap();
        // L1 and SPX selected (display 1..3).
        let r = resolve(SelectKind::Rows, (1, 0), (2, 0), plan.columns.len());
        let s = summarize(&snap, &plan, &shown, &r);
        let (_, text) = s
            .iter()
            .find(|(l, _)| l == &plan.columns[delta].label)
            .unwrap();
        assert!(text.starts_with("Σ 5.00 "), "{text}");
    }

    #[test]
    fn a_determined_non_additive_column_is_never_totalled() {
        let (snap, plan, shown) = fixture();
        let det = plan.position_of("det").unwrap();
        // L1 and L2 at depth 1 (display rows 1 and 3, with SPX between).
        let r = resolve(SelectKind::Rows, (1, 0), (3, 0), plan.columns.len());
        let s = summarize(&snap, &plan, &shown, &r);
        let (_, text) = s
            .iter()
            .find(|(l, _)| l == &plan.columns[det].label)
            .unwrap();
        assert!(text.starts_with("Σ —†"), "{text}");
    }

    #[test]
    fn a_block_over_one_measure_adds_extremes_and_skips_text_columns() {
        let (snap, plan, shown) = fixture();
        let delta = plan.position_of("delta01").unwrap();
        let r = resolve(SelectKind::Block, (1, 0), (3, delta), plan.columns.len());
        let s = summarize(&snap, &plan, &shown, &r);
        assert_eq!(s.len(), 1, "the tree column is not numeric: {s:?}");
        assert!(
            s[0].1.contains("min") && s[0].1.contains("max"),
            "{:?}",
            s[0]
        );
    }
}
