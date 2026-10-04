//! The blotter's dimension context: every column with one value at a row.
//! Read in three passes, each adding only columns not yet listed: the
//! grouping path (a subtotal knows only its own levels and those above),
//! the view's shown dimension columns, then every other unanimity column
//! in the snapshot — the hidden context columns the compiler added for the
//! shell's registry. A NULL, empty or mixed value is absent.

use geode_core::grid::selection::{Resolved, top_most};
use geode_core::link::{CursorScope, path_scope};
use geode_core::snapshot::Snapshot;

use super::expansion::path_of;
use super::plan::{ColumnKind, ColumnPlan};

pub fn values_at(snapshot: &Snapshot, plan: &ColumnPlan, row: usize) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value) in plan.grouping.iter().zip(path_of(snapshot, plan, row)) {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            push(&mut out, name, v);
        }
    }
    for c in plan
        .columns
        .iter()
        .filter(|c| c.kind == ColumnKind::Dimension)
    {
        if let Some(v) = c.index.and_then(|i| single(snapshot, i, row)) {
            push(&mut out, &c.name, v);
        }
    }
    for (i, meta) in (0..).map_while(|i| snapshot.meta_at(i).map(|m| (i, m))) {
        if meta.mixed_flag.is_some()
            && let Some(v) = single(snapshot, i, row)
        {
            push(&mut out, &meta.name, v);
        }
    }
    out
}

/// What the cursor row posts into a link group: its grouping path, one
/// value per level, and on a leaf row (the deepest grouping level) its own
/// single-valued columns too, as `values_at` reads them, after the path.
/// A NULL or empty value on the path refuses: a scope cannot select NULL,
/// and leaving the level out would widen every follower to all its values.
/// A group row adds nothing beyond its path, so a follower sees the whole
/// group, never a value merely unanimous today. The total row has an empty
/// path.
pub fn cursor_scope(snapshot: &Snapshot, plan: &ColumnPlan, row: usize) -> CursorScope {
    let path = path_of(snapshot, plan, row);
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(path.len());
    for (name, value) in plan.grouping.iter().zip(&path) {
        match value.as_deref().filter(|v| !v.is_empty()) {
            Some(v) => pairs.push((name.clone(), v.to_string())),
            None => return CursorScope::NullIn(name.clone()),
        }
    }
    if !path.is_empty() && path.len() == plan.grouping.len() {
        for (name, value) in values_at(snapshot, plan, row) {
            push(&mut pairs, &name, value);
        }
    }
    CursorScope::Path(path_scope(&pairs))
}

/// The values of each selected top-most row (a group row already stands
/// for its children), in display order.
pub fn selection_values(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    shown: &[u32],
    resolved: &Resolved,
) -> Vec<Vec<(String, String)>> {
    let end = resolved.rows.end.min(shown.len());
    let start = resolved.rows.start.min(end);
    let rows: Vec<usize> = shown[start..end].iter().map(|&r| r as usize).collect();
    let tree = snapshot.tree();
    top_most(&rows, snapshot.rows(), |r| tree.parent(r))
        .into_iter()
        .map(|r| values_at(snapshot, plan, r))
        .collect()
}

fn push(out: &mut Vec<(String, String)>, name: &str, value: String) {
    if !out.iter().any(|(n, _)| n == name) {
        out.push((name.to_string(), value));
    }
}

fn single(snapshot: &Snapshot, idx: usize, row: usize) -> Option<String> {
    if snapshot.is_mixed_at(idx, row) {
        return None;
    }
    snapshot.display_at(idx, row).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::grid::selection::SelectKind;
    use geode_core::snapshot::{ColumnMeta, TestColumn};
    use geode_core::view::ViewSpec;

    fn meta(name: &str, mixed_flag: Option<usize>) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 4],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag,
        }
    }
    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }
    fn view(text: &str) -> ViewSpec {
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.remove(0)
    }

    /// Rows: 0 root; 1 L1; 2 L2; 3 L1/SPX; 4 L1/NDX. Grouping lhu,
    /// underlying_ref. Hidden context column position_ref (index 4, flag 5,
    /// named as the compiler names it): P7 on row 3, mixed on row 1 and the
    /// root, NULL on row 2, P9 on row 4. Row 1 carries a value under its
    /// mixed flag, so the reader must honour the flag, not rely on the
    /// compiler writing NULL there.
    fn fixture() -> (Snapshot, ColumnPlan) {
        let snap = Snapshot::for_tests(
            vec![
                (
                    meta("lhu", None),
                    TestColumn::Dict(vec![None, s("L1"), s("L2"), s("L1"), s("L1")]),
                ),
                (
                    meta("underlying_ref", None),
                    TestColumn::Dict(vec![None, None, None, s("SPX"), s("NDX")]),
                ),
                (
                    meta("row_depth", None),
                    TestColumn::I32(vec![0, 1, 1, 2, 2]),
                ),
                (
                    meta("delta01", None),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0), Some(2.0), Some(3.0)]),
                ),
                (
                    meta("position_ref", Some(5)),
                    TestColumn::Str(vec![None, Some("P1"), None, Some("P7"), Some("P9")]),
                ),
                (
                    meta("position_ref#mixed", None),
                    TestColumn::Bool(vec![
                        Some(true),
                        Some(true),
                        Some(false),
                        Some(false),
                        Some(false),
                    ]),
                ),
            ],
            2,
        );
        let view = view(
            "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[t.columns]]\nname = \"delta01\"\n",
        );
        let grouping = vec!["lhu".to_string(), "underlying_ref".to_string()];
        let plan = ColumnPlan::build(&view, &grouping, &snap);
        (snap, plan)
    }
    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    /// [`fixture`] with `lhu` NULL on row 1 and its two leaves: rows 3
    /// (SPX) and 4 (NDX) sit under a group whose grouping value is NULL.
    fn null_lhu_fixture() -> (Snapshot, ColumnPlan) {
        let snap = Snapshot::for_tests(
            vec![
                (
                    meta("lhu", None),
                    TestColumn::Dict(vec![None, None, s("L2"), None, None]),
                ),
                (
                    meta("underlying_ref", None),
                    TestColumn::Dict(vec![None, None, None, s("SPX"), s("NDX")]),
                ),
                (
                    meta("row_depth", None),
                    TestColumn::I32(vec![0, 1, 1, 2, 2]),
                ),
                (
                    meta("delta01", None),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0), Some(2.0), Some(3.0)]),
                ),
            ],
            2,
        );
        let view = view(
            "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[t.columns]]\nname = \"delta01\"\n",
        );
        let grouping = vec!["lhu".to_string(), "underlying_ref".to_string()];
        let plan = ColumnPlan::build(&view, &grouping, &snap);
        (snap, plan)
    }

    #[test]
    fn a_leaf_row_names_its_path_then_its_hidden_key() {
        let (snap, plan) = fixture();
        assert_eq!(
            values_at(&snap, &plan, 3),
            pairs(&[
                ("lhu", "L1"),
                ("underlying_ref", "SPX"),
                ("position_ref", "P7")
            ])
        );
    }

    #[test]
    fn a_mixed_context_column_is_absent() {
        let (snap, plan) = fixture();
        assert_eq!(values_at(&snap, &plan, 1), pairs(&[("lhu", "L1")]));
    }

    #[test]
    fn a_null_context_column_is_absent() {
        let (snap, plan) = fixture();
        assert_eq!(values_at(&snap, &plan, 2), pairs(&[("lhu", "L2")]));
    }

    #[test]
    fn a_grouped_column_below_the_row_is_absent() {
        let (snap, plan) = fixture();
        let v = values_at(&snap, &plan, 1);
        assert!(v.iter().all(|(c, _)| c != "underlying_ref"), "{v:?}");
        assert!(values_at(&snap, &plan, 0).is_empty(), "the grand total");
    }

    #[test]
    fn a_column_is_listed_once() {
        // A grouping column that is also a unanimity column (a view that
        // shows `lhu` while grouping by it cannot produce this today, but a
        // snapshot fixture can): the path's value wins, once.
        let snap = Snapshot::for_tests(
            vec![
                (meta("lhu", Some(2)), TestColumn::Dict(vec![None, s("L1")])),
                (meta("row_depth", None), TestColumn::I32(vec![0, 1])),
                (
                    meta("lhu#mixed", None),
                    TestColumn::Bool(vec![Some(false), Some(false)]),
                ),
            ],
            1,
        );
        let view = view("[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n");
        let plan = ColumnPlan::build(&view, &["lhu".to_string()], &snap);
        assert_eq!(values_at(&snap, &plan, 1), pairs(&[("lhu", "L1")]));
    }

    #[test]
    fn a_null_grouping_value_is_absent() {
        let snap = Snapshot::for_tests(
            vec![
                (meta("lhu", None), TestColumn::Dict(vec![None, None])),
                (meta("row_depth", None), TestColumn::I32(vec![0, 1])),
            ],
            1,
        );
        let view = view("[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n");
        let plan = ColumnPlan::build(&view, &["lhu".to_string()], &snap);
        assert!(values_at(&snap, &plan, 1).is_empty(), "never the text NULL");
    }

    #[test]
    fn an_empty_grouping_label_is_absent() {
        let snap = Snapshot::for_tests(
            vec![
                (meta("lhu", None), TestColumn::Dict(vec![None, s("")])),
                (meta("row_depth", None), TestColumn::I32(vec![0, 1])),
            ],
            1,
        );
        let view = view("[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n");
        let plan = ColumnPlan::build(&view, &["lhu".to_string()], &snap);
        assert!(
            values_at(&snap, &plan, 1).is_empty(),
            "never an empty value"
        );
    }

    fn path_of_scope(c: CursorScope) -> Vec<(String, String)> {
        match c {
            CursorScope::Path(s) => s
                .dimensions
                .into_iter()
                .map(|d| (d.column, d.values.join(",")))
                .collect(),
            other => panic!("expected a path, got {other:?}"),
        }
    }

    #[test]
    fn the_total_row_has_an_empty_path() {
        let (snap, plan) = fixture();
        assert_eq!(path_of_scope(cursor_scope(&snap, &plan, 0)), pairs(&[]));
    }

    /// A group row posts its levels only, even where a hidden context
    /// column is unanimous below it today: a follower must see the whole
    /// group, not the one position it happens to hold.
    #[test]
    fn a_group_row_emits_its_path_and_nothing_unanimous() {
        let (snap, plan) = fixture();
        assert_eq!(
            path_of_scope(cursor_scope(&snap, &plan, 1)),
            pairs(&[("lhu", "L1")])
        );

        // Root; L1 (unanimous on P7); L1/SPX (P7).
        let snap = Snapshot::for_tests(
            vec![
                (
                    meta("lhu", None),
                    TestColumn::Dict(vec![None, s("L1"), s("L1")]),
                ),
                (
                    meta("underlying_ref", None),
                    TestColumn::Dict(vec![None, None, s("SPX")]),
                ),
                (meta("row_depth", None), TestColumn::I32(vec![0, 1, 2])),
                (
                    meta("position_ref", Some(4)),
                    TestColumn::Str(vec![None, Some("P7"), Some("P7")]),
                ),
                (
                    meta("position_ref#mixed", None),
                    TestColumn::Bool(vec![Some(true), Some(false), Some(false)]),
                ),
            ],
            2,
        );
        let view = view("[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n");
        let grouping = vec!["lhu".to_string(), "underlying_ref".to_string()];
        let plan = ColumnPlan::build(&view, &grouping, &snap);
        assert_eq!(
            values_at(&snap, &plan, 1),
            pairs(&[("lhu", "L1"), ("position_ref", "P7")]),
            "sanity: the context reads P7 as unanimous on L1"
        );
        assert_eq!(
            path_of_scope(cursor_scope(&snap, &plan, 1)),
            pairs(&[("lhu", "L1")])
        );
    }

    #[test]
    fn a_leaf_row_adds_its_own_single_values() {
        let (snap, plan) = fixture();
        assert_eq!(
            path_of_scope(cursor_scope(&snap, &plan, 3)),
            pairs(&[
                ("lhu", "L1"),
                ("underlying_ref", "SPX"),
                ("position_ref", "P7")
            ])
        );
        assert_eq!(
            path_of_scope(cursor_scope(&snap, &plan, 4)),
            pairs(&[
                ("lhu", "L1"),
                ("underlying_ref", "NDX"),
                ("position_ref", "P9")
            ])
        );
    }

    #[test]
    fn a_null_on_the_path_refuses_and_names_the_column() {
        let (snap, plan) = null_lhu_fixture();
        assert_eq!(
            cursor_scope(&snap, &plan, 1),
            CursorScope::NullIn("lhu".into())
        );
        assert_eq!(
            cursor_scope(&snap, &plan, 3),
            CursorScope::NullIn("lhu".into())
        );
        assert_eq!(
            path_of_scope(cursor_scope(&snap, &plan, 2)),
            pairs(&[("lhu", "L2")]),
            "a sibling under a real value still posts"
        );
    }

    #[test]
    fn a_selection_lists_its_top_most_rows_only() {
        let (snap, plan) = fixture();
        let shown: Vec<u32> = vec![0, 1, 3, 4, 2];
        // Display rows 1..4 = L1, L1/SPX, L1/NDX: L1 is the only top-most.
        let resolved = Resolved {
            kind: SelectKind::Rows,
            rows: 1..4,
            cols: 0..1,
        };
        assert_eq!(
            selection_values(&snap, &plan, &shown, &resolved),
            vec![pairs(&[("lhu", "L1")])]
        );
        // Display rows 2..4 = the two leaves.
        let leaves = Resolved {
            rows: 2..4,
            ..resolved
        };
        assert_eq!(selection_values(&snap, &plan, &shown, &leaves).len(), 2);
    }
}
