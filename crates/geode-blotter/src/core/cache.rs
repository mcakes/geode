//! The blotter's cell formatter and the cell it prepares. The delegate
//! fills a `geode_tile::grid::WindowCache<CachedCell>` with these for the
//! window the table reports and on snapshot delivery, outside `render_td`;
//! callers invalidate when the snapshot, row order or plan changes.

use crate::core::format::{Sign, format_number};
use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::attribution::Attribution;
use geode_core::snapshot::Snapshot;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedCell {
    pub text: Arc<str>,
    /// `Some` for a number; drives the sign colour.
    pub sign: Option<Sign>,
    pub attribution: Attribution,
    /// The cell is an ungrouped dimension whose rows disagree: `text` is
    /// [`MIXED`] and paints muted. Distinct from a blank cell (no value at
    /// all), which is not cached.
    pub mixed: bool,
}

/// What an ungrouped dimension cell says when the rows under it disagree
/// (the compiler's unanimity rule). A word rather than a glyph: it is read
/// as text, and it must not look like a value or like a blank.
pub const MIXED: &str = "mixed";

/// Format one snapshot cell for display. Missing columns and NULL values
/// return `None`. In particular, compiler-supplied NULL for a
/// `NonAttributable` cell stays blank rather than becoming `0.00`.
pub fn cell(snapshot: &Snapshot, plan: &ColumnPlan, row: usize, col: usize) -> Option<CachedCell> {
    let column = plan.columns.get(col)?;
    if row >= snapshot.rows() {
        return None;
    }
    let depth = snapshot.tree().depth(row);
    let attribution = plan.attribution(col, depth);
    match column.kind {
        ColumnKind::Tree => plan.tree_text(snapshot, row).map(|t| CachedCell {
            text: t.into(),
            sign: None,
            attribution,
            mixed: false,
        }),
        ColumnKind::Measure => {
            let idx = column.index?;
            let value = snapshot.f64_at(idx, row)?;
            let f = format_number(value, &column.format);
            Some(CachedCell {
                text: f.text.into(),
                sign: Some(f.sign),
                attribution,
                mixed: false,
            })
        }
        ColumnKind::Dimension => {
            let idx = column.index?;
            // Checked before the value: a mixed cell is NULL in the data,
            // and reading the value first would paint it blank — the one
            // thing it must not look like.
            if snapshot.is_mixed_at(idx, row) {
                return Some(CachedCell {
                    text: MIXED.into(),
                    sign: None,
                    attribution,
                    mixed: true,
                });
            }
            let text = dimension_text(snapshot, idx, row)?;
            Some(CachedCell {
                text: text.into(),
                sign: None,
                attribution,
                mixed: false,
            })
        }
    }
}

/// A dimension cell's text. A numeric dimension (an ungrouped `strike`
/// arrives as a number) prints its shortest exact form — `4250`, `4250.5` —
/// rather than going through the text format, whose precision of 0 would
/// round `4250.5` to a strike that does not exist. NaN is blank.
pub fn dimension_text(snapshot: &Snapshot, idx: usize, row: usize) -> Option<String> {
    match snapshot.f64_at(idx, row) {
        Some(v) if v.is_nan() => None,
        Some(v) => Some(v.to_string()),
        None => snapshot.display_at(idx, row),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plan::ColumnPlan;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;
    use std::cell::Cell;

    #[test]
    fn a_window_move_refills_only_the_rows_that_entered() {
        let mut c = geode_tile::grid::WindowCache::<CachedCell>::default();
        let fills = Cell::new(0);
        let fill = |r: usize, _c: usize| {
            fills.set(fills.get() + 1);
            Some(CachedCell {
                text: format!("r{r}").into(),
                sign: None,
                attribution: Attribution::Additive,
                mixed: false,
            })
        };
        c.set_window(0..10, 2, fill);
        assert_eq!(fills.get(), 20);
        assert_eq!(c.get(3, 1).map(|x| &*x.text), Some("r3"));
        c.set_window(5..15, 2, fill);
        assert_eq!(fills.get(), 30, "five new rows, two columns");
        assert_eq!(c.get(14, 0).map(|x| &*x.text), Some("r14"));
        assert!(c.get(4, 0).is_none(), "left the window");
        c.clear();
        assert!(c.get(7, 0).is_none());
        c.set_window(5..15, 2, fill);
        assert_eq!(fills.get(), 50, "everything refilled after invalidation");
    }

    #[test]
    fn cells_honour_the_read_paths_opinions() {
        // NonAttributable is NULL and paints blank, never 0.00; a
        // DeterminedNonAdditive cell carries its value and its marker; a
        // real zero paints.
        let meta = |n: &str, by_depth: Vec<Attribution>| ColumnMeta {
            name: n.into(),
            attribution_by_depth: by_depth,
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        };
        let snap = Snapshot::for_tests(
            vec![
                (
                    meta("lhu", vec![Attribution::Additive; 3]),
                    TestColumn::Str(vec![None, Some("L1"), Some("L1")]),
                ),
                (
                    meta("underlying_ref", vec![Attribution::Additive; 3]),
                    TestColumn::Str(vec![None, None, Some("SPX")]),
                ),
                (
                    meta("row_depth", vec![Attribution::Additive; 3]),
                    TestColumn::I32(vec![0, 1, 2]),
                ),
                (
                    meta(
                        "cross_gamma02",
                        vec![
                            Attribution::Additive,
                            Attribution::NonAttributable,
                            Attribution::Additive,
                        ],
                    ),
                    TestColumn::F64(vec![Some(0.0), None, Some(2.5)]),
                ),
                (
                    meta(
                        "daily_trading_pnl",
                        vec![
                            Attribution::Additive,
                            Attribution::Additive,
                            Attribution::DeterminedNonAdditive,
                        ],
                    ),
                    TestColumn::F64(vec![Some(7.0), Some(7.0), Some(7.0)]),
                ),
            ],
            2,
        );
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[t.columns]]\nname = \"cross_gamma02\"\n[[t.columns]]\nname = \"daily_trading_pnl\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);

        assert_eq!(
            cell(&snap, &plan, 0, 1).map(|c| c.text.to_string()),
            Some("0.00".into()),
            "a real zero paints"
        );
        assert_eq!(
            cell(&snap, &plan, 1, 1),
            None,
            "NonAttributable is blank, never 0.00"
        );
        let leaf = cell(&snap, &plan, 2, 2).unwrap();
        assert_eq!(&*leaf.text, "7.00");
        assert_eq!(leaf.attribution, Attribution::DeterminedNonAdditive);
        assert_eq!(
            cell(&snap, &plan, 0, 0),
            None,
            "the grand total has no tree text"
        );
        assert_eq!(
            cell(&snap, &plan, 2, 0).map(|c| c.text.to_string()),
            Some("SPX".into())
        );
        assert_eq!(cell(&snap, &plan, 9, 1), None, "past the end");
        assert_eq!(cell(&snap, &plan, 0, 9), None, "no such column");
    }

    /// Root plus three LHUs whose ungrouped `strike` is a value (A), mixed
    /// (B, and the root) and blank (C). The value column is NULL wherever
    /// the flag is set, as the compiler emits it.
    fn unanimity_fixture() -> (Snapshot, ColumnPlan) {
        let meta = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        };
        let snap = Snapshot::for_tests(
            vec![
                (
                    meta("lhu"),
                    TestColumn::Str(vec![None, Some("A"), Some("B"), Some("C")]),
                ),
                (meta("row_depth"), TestColumn::I32(vec![0, 1, 1, 1])),
                (
                    ColumnMeta {
                        mixed_flag: Some(3),
                        ..meta("strike")
                    },
                    TestColumn::F64(vec![None, Some(4250.5), None, None]),
                ),
                (
                    meta("strike#mixed"),
                    TestColumn::Bool(vec![Some(true), Some(false), Some(true), Some(false)]),
                ),
            ],
            1,
        );
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[t.columns]]\nname = \"strike\"\nkind = \"dimension\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        (snap, plan)
    }

    /// Mixed and blank are different facts and must not look alike: a mixed
    /// cell paints the marker, flagged so the delegate mutes it, and a blank
    /// one paints nothing. The companion flag column is not a view column,
    /// so the plan never shows it.
    #[test]
    fn a_mixed_dimension_cell_paints_the_marker_and_a_blank_one_paints_nothing() {
        let (snap, plan) = unanimity_fixture();
        assert_eq!(
            plan.columns.len(),
            2,
            "tree and strike; the flag is not shown"
        );
        let value = cell(&snap, &plan, 1, 1).unwrap();
        assert_eq!((&*value.text, value.mixed), ("4250.5", false));
        let mixed = cell(&snap, &plan, 2, 1).unwrap();
        assert_eq!((&*mixed.text, mixed.mixed), (MIXED, true));
        assert_eq!(cell(&snap, &plan, 3, 1), None, "blank is not mixed");
        assert!(cell(&snap, &plan, 0, 1).unwrap().mixed, "the root too");
    }
}
