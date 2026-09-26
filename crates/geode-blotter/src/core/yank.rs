//! Yank as TSV (Phase 3 spec §6.4): a header line of labels, then one
//! line per selected visible row — tree text indented two spaces per
//! depth, numbers raw and unscaled, blanks for NULL.

use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::snapshot::Snapshot;
use std::fmt::Write as _;
use std::ops::Range;

pub fn tsv(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    visible: &[u32],
    rows: Range<usize>,
    cols: Range<usize>,
) -> String {
    let cols = cols.start.min(plan.columns.len())..cols.end.min(plan.columns.len());
    let mut out = String::new();
    let labels: Vec<&str> = plan.columns[cols.clone()]
        .iter()
        .map(|c| c.label.as_str())
        .collect();
    out.push_str(&labels.join("\t"));
    out.push('\n');
    for &row in visible.iter().skip(rows.start).take(rows.len()) {
        let row = row as usize;
        let mut fields: Vec<String> = Vec::with_capacity(cols.len());
        for column in &plan.columns[cols.clone()] {
            let field = match column.kind {
                ColumnKind::Tree => {
                    let depth = snapshot.tree().depth(row);
                    let mut s = "  ".repeat(depth);
                    if let Some(t) = plan.tree_text(snapshot, row) {
                        s.push_str(t);
                    }
                    s
                }
                ColumnKind::Measure => column
                    .index
                    .and_then(|i| snapshot.f64_at(i, row))
                    .map(|v| {
                        let mut s = String::new();
                        let _ = write!(s, "{v}");
                        s
                    })
                    .unwrap_or_default(),
                ColumnKind::Dimension => column
                    .index
                    .and_then(|i| snapshot.display_at(i, row))
                    .unwrap_or_default(),
            };
            fields.push(field);
        }
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plan::ColumnPlan;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;

    /// Root, L1, L1/SPX with a `delta01` measure column. Shared by the
    /// full-range and block-range tests below.
    fn fixture() -> (Snapshot, ColumnPlan, Vec<u32>) {
        let dim = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 3],
            scope_semantics: ScopeSemantics::Direct,
        };
        let snap = Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Str(vec![None, Some("L1"), Some("L1")]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Str(vec![None, None, Some("SPX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 2])),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![Some(1234567.891), Some(1.5), None]),
                ),
            ],
            2,
        );
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[t.columns]]\nname = \"delta01\"\nformat = { precision = 0, scale = \"k\" }\nlabel = \"Δ\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        (snap, plan, vec![0, 1, 2])
    }

    #[test]
    fn tsv_has_a_header_indented_tree_text_raw_numbers_and_blanks() {
        let (snap, plan, visible) = fixture();
        let out = tsv(&snap, &plan, &visible, 0..3, 0..plan.columns.len());
        assert_eq!(
            out, "lhu / underlying_ref\tΔ (k)\n\t1234567.891\n  L1\t1.5\n    SPX\t\n",
            "raw, unscaled values; blank for NULL; two spaces per depth"
        );
        assert_eq!(
            tsv(&snap, &plan, &visible, 1..2, 0..plan.columns.len()),
            "lhu / underlying_ref\tΔ (k)\n  L1\t1.5\n"
        );
    }

    #[test]
    fn a_block_yanks_only_its_columns_with_their_header() {
        // same fixture as tsv_has_a_header_indented_tree_text_raw_numbers_and_blanks
        let (snap, plan, visible) = fixture();
        let out = tsv(&snap, &plan, &visible, 1..3, 1..2);
        let header = &plan.columns[1].label;
        assert_eq!(out, format!("{header}\n1.5\n\n"));
    }
}
