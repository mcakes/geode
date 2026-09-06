//! The column plan (Phase 3 spec §6.1): every view column resolved to a
//! snapshot index once, with its kind, format, width and per-depth
//! attribution. Built when a snapshot arrives whose column set differs
//! from the last; never touched per cell. The probe's five name searches
//! per cell are what this replaces.

use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::groupings::GroupingSlots;
use geode_core::snapshot::Snapshot;
use geode_core::view::{ColumnFormat, ViewColumn, ViewSpec};

pub const TREE_WIDTH: f32 = 260.0;
pub const MEASURE_WIDTH: f32 = 110.0;
pub const TEXT_WIDTH: f32 = 140.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// The grouping value for the row's own depth, indented, with a
    /// disclosure glyph. Always first; never moved.
    Tree,
    Measure,
    Dimension,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedColumn {
    /// The snapshot column name; empty for the tree column.
    pub name: String,
    pub label: String,
    /// The snapshot column index, or `None` when the snapshot lacks it —
    /// such a cell paints blank and never panics.
    pub index: Option<usize>,
    pub kind: ColumnKind,
    pub format: ColumnFormat,
    pub width: f32,
    /// Indexed by row depth (§6.5).
    pub attribution: Vec<Attribution>,
    /// Dimensions applied by membership rather than directly.
    pub semi_joined: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnPlan {
    pub columns: Vec<PlannedColumn>,
    pub grouping: Vec<String>,
    /// Snapshot index of each grouping column, for `tree_text`.
    pub grouping_indices: Vec<Option<usize>>,
}

impl ColumnPlan {
    pub fn build(view: &ViewSpec, grouping: &[String], snapshot: &Snapshot) -> ColumnPlan {
        let grouping_indices: Vec<Option<usize>> =
            grouping.iter().map(|g| snapshot.column_index(g)).collect();
        let mut columns = vec![PlannedColumn {
            name: String::new(),
            label: GroupingSlots::label_of(grouping),
            index: None,
            kind: ColumnKind::Tree,
            format: ColumnFormat::TEXT,
            width: TREE_WIDTH,
            attribution: Vec::new(),
            semi_joined: Vec::new(),
        }];
        for column in &view.columns {
            let name = column.name();
            let kind = match column {
                ViewColumn::Dimension { .. } => {
                    if grouping.iter().any(|g| g == name) {
                        continue; // folded into the tree column
                    }
                    ColumnKind::Dimension
                }
                ViewColumn::Measure { .. } | ViewColumn::Derived { .. } => ColumnKind::Measure,
            };
            let presentation = view.presentation_of(name);
            let format = match kind {
                ColumnKind::Measure => ColumnFormat::MEASURE,
                _ => ColumnFormat::TEXT,
            }
            .with(&presentation);
            let base_label = presentation
                .label
                .clone()
                .unwrap_or_else(|| name.to_string());
            let label = if format.scale.suffix().is_empty() {
                base_label
            } else {
                format!("{base_label} ({})", format.scale.suffix())
            };
            let width = presentation.width.unwrap_or(match kind {
                ColumnKind::Measure => MEASURE_WIDTH,
                _ => TEXT_WIDTH,
            });
            let index = snapshot.column_index(name);
            let meta = index.and_then(|i| snapshot.meta_at(i));
            let attribution = meta
                .map(|m| m.attribution_by_depth.clone())
                .unwrap_or_default();
            let semi_joined = match meta.map(|m| &m.scope_semantics) {
                Some(ScopeSemantics::SemiJoined { dimensions }) => dimensions.clone(),
                _ => Vec::new(),
            };
            columns.push(PlannedColumn {
                name: name.to_string(),
                label,
                index,
                kind,
                format,
                width,
                attribution,
                semi_joined,
            });
        }
        ColumnPlan {
            columns,
            grouping: grouping.to_vec(),
            grouping_indices,
        }
    }

    /// The row's own level: `grouping[depth - 1]` at that row. `None` for
    /// the grand total.
    pub fn tree_text<'a>(&self, snapshot: &'a Snapshot, row: usize) -> Option<&'a str> {
        let depth = snapshot.tree().depth(row);
        let col = (*self.grouping_indices.get(depth.checked_sub(1)?)?)?;
        snapshot.text_at(col, row)
    }

    /// The marker for a cell (§6.5). Absent columns and depths past what
    /// the compiler declared are `Additive`, which paints the value plain
    /// — the compiler blanks a `NonAttributable` cell itself, so this can
    /// only ever err towards showing a number that is really there.
    pub fn attribution(&self, col: usize, depth: usize) -> Attribution {
        self.columns
            .get(col)
            .and_then(|c| c.attribution.get(depth).copied())
            .unwrap_or(Attribution::Additive)
    }

    /// Reorder a column; the tree column stays first whatever is asked.
    pub fn move_column(&mut self, from: usize, to: usize) {
        if from == 0 || to == 0 || from >= self.columns.len() || to >= self.columns.len() {
            return;
        }
        let column = self.columns.remove(from);
        self.columns.insert(to, column);
    }

    /// Whether this plan still describes `snapshot`'s columns.
    pub fn same_columns(&self, snapshot: &Snapshot) -> bool {
        self.columns.iter().all(|c| match c.kind {
            ColumnKind::Tree => true,
            _ => snapshot.column_index(&c.name) == c.index,
        }) && self
            .grouping
            .iter()
            .zip(&self.grouping_indices)
            .all(|(g, i)| snapshot.column_index(g) == *i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;

    fn view() -> ViewSpec {
        let text = r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu", "underlying_ref"]
[[tree.columns]]
name = "lhu"
kind = "dimension"
[[tree.columns]]
name = "model_code"
kind = "dimension"
[[tree.columns]]
name = "delta01"
format = { precision = 0, scale = "k" }
label = "Δ"
width = 90
[[tree.columns]]
name = "daily_trading_pnl"
[[tree.columns]]
name = "missing_in_snapshot"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.remove(0)
    }

    fn meta(name: &str, by_depth: Vec<Attribution>, semantics: ScopeSemantics) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: by_depth,
            scope_semantics: semantics,
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot::for_tests(
            vec![
                (
                    meta(
                        "lhu",
                        vec![Attribution::Additive; 3],
                        ScopeSemantics::Direct,
                    ),
                    TestColumn::Str(vec![None, Some("L1"), Some("L1")]),
                ),
                (
                    meta(
                        "underlying_ref",
                        vec![Attribution::Additive; 3],
                        ScopeSemantics::Direct,
                    ),
                    TestColumn::Str(vec![None, None, Some("SPX")]),
                ),
                (
                    meta(
                        "row_depth",
                        vec![Attribution::Additive; 3],
                        ScopeSemantics::Direct,
                    ),
                    TestColumn::I32(vec![0, 1, 2]),
                ),
                (
                    meta(
                        "delta01",
                        vec![Attribution::Additive; 3],
                        ScopeSemantics::Direct,
                    ),
                    TestColumn::F64(vec![Some(1.0), Some(1.0), Some(1.0)]),
                ),
                (
                    meta(
                        "daily_trading_pnl",
                        vec![
                            Attribution::Additive,
                            Attribution::Additive,
                            Attribution::NonAttributable,
                        ],
                        ScopeSemantics::SemiJoined {
                            dimensions: vec!["underlying_ref".into()],
                        },
                    ),
                    TestColumn::F64(vec![Some(7.0), Some(7.0), None]),
                ),
                (
                    meta(
                        "model_code",
                        vec![Attribution::Additive; 3],
                        ScopeSemantics::Direct,
                    ),
                    TestColumn::Str(vec![None, None, Some("EURP")]),
                ),
            ],
            2,
        )
    }

    #[test]
    fn the_tree_column_leads_and_grouping_dimensions_fold_into_it() {
        let plan = ColumnPlan::build(
            &view(),
            &["lhu".into(), "underlying_ref".into()],
            &snapshot(),
        );
        let names: Vec<&str> = plan.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "",
                "model_code",
                "delta01",
                "daily_trading_pnl",
                "missing_in_snapshot"
            ]
        );
        assert_eq!(plan.columns[0].kind, ColumnKind::Tree);
        assert_eq!(plan.columns[0].label, "lhu / underlying_ref");
        assert_eq!(plan.columns[0].width, TREE_WIDTH);
        assert_eq!(plan.columns[1].kind, ColumnKind::Dimension);
        assert_eq!(plan.columns[1].width, TEXT_WIDTH);
        assert_eq!(plan.grouping_indices, vec![Some(0), Some(1)]);
    }

    #[test]
    fn every_column_is_resolved_once_and_an_absent_one_is_none_not_a_panic() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        assert_eq!(plan.columns[2].index, Some(3));
        assert_eq!(plan.columns[4].index, None);
        assert!(plan.same_columns(&snap));
    }

    #[test]
    fn presentation_and_kind_defaults_are_applied() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        let d = &plan.columns[2];
        assert_eq!(d.label, "Δ (k)", "the scale suffix follows the label");
        assert_eq!(d.width, 90.0);
        assert_eq!(d.format.precision, 0);
        assert_eq!(d.format.scale, geode_core::view::Scale::Thousands);
        assert!(
            d.format.thousands,
            "the measure default survives a partial override"
        );
        let p = &plan.columns[3];
        assert_eq!(p.label, "daily_trading_pnl");
        assert_eq!(p.width, MEASURE_WIDTH);
        assert_eq!(p.format, geode_core::view::ColumnFormat::MEASURE);
    }

    #[test]
    fn attribution_is_per_column_per_depth_and_semi_joined_dimensions_are_named() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        assert_eq!(plan.attribution(3, 2), Attribution::NonAttributable);
        assert_eq!(plan.attribution(3, 1), Attribution::Additive);
        assert_eq!(
            plan.attribution(4, 0),
            Attribution::Additive,
            "an absent column is treated as additive"
        );
        assert_eq!(
            plan.attribution(3, 9),
            Attribution::Additive,
            "past the declared depths, additive"
        );
        assert_eq!(
            plan.columns[3].semi_joined,
            vec!["underlying_ref".to_string()]
        );
        assert!(plan.columns[2].semi_joined.is_empty());
    }

    #[test]
    fn tree_text_is_the_rows_own_level() {
        let snap = snapshot();
        let plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        assert_eq!(
            plan.tree_text(&snap, 0),
            None,
            "the grand total has no level of its own"
        );
        assert_eq!(plan.tree_text(&snap, 1), Some("L1"));
        assert_eq!(plan.tree_text(&snap, 2), Some("SPX"));
    }

    #[test]
    fn move_column_reorders_but_never_moves_the_tree_column() {
        let snap = snapshot();
        let mut plan = ColumnPlan::build(&view(), snap.grouping(), &snap);
        plan.move_column(3, 1);
        let names: Vec<&str> = plan.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "",
                "daily_trading_pnl",
                "model_code",
                "delta01",
                "missing_in_snapshot"
            ]
        );
        plan.move_column(0, 2);
        assert_eq!(
            plan.columns[0].kind,
            ColumnKind::Tree,
            "the tree column stays first"
        );
        plan.move_column(2, 0);
        assert_eq!(plan.columns[0].kind, ColumnKind::Tree);
    }
}
