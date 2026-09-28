//! Named column sets over the pricer vocabulary, read from ordinary `views.toml`
//! views whose `dataset` is `pricer`. `geode_core::config::load_views` has
//! already merged the view definition, `dataset_presentation` and
//! `view_presentation` (order permuted, `hidden` set) into each `ViewSpec`;
//! this module keeps the pricer's views, refuses what it cannot evaluate
//! (joins, derived SQL) and resolves each column against the vocabulary.

use crate::core::columns::{ColumnDef, column};
use crate::core::dataset::PRICER_DATASET;
use geode_core::config::{Diagnostic, LayerDoc, Severity, merge_docs};
use geode_core::view::ViewColumn as SpecColumn;
use geode_core::view::{ColumnFormat, ColumnPresentation, ViewSpec};

/// Retired document name. Nothing reads it; the app reports a document by
/// this name so a desk or user layer still carrying one learns where its
/// views now live.
pub const PRICER_VIEWS_DOC: &str = "pricer_views";

/// The two bundled views, a `views` doc over the computed `pricer` dataset
/// that the application installs in its builtin layer. Both end in `status`
/// so pending pricing and failures are visible in text as well as through
/// cell colors. A non-measure column carries `kind = "dimension"` so the
/// view validates against the dataset declaration at load.
pub const BUILTIN_VIEWS: &str = r#"[vanilla]
dataset = "pricer"
[[vanilla.columns]]
name = "qty"
kind = "dimension"
[[vanilla.columns]]
name = "underlying_ref"
kind = "dimension"
[[vanilla.columns]]
name = "expiry"
kind = "dimension"
[[vanilla.columns]]
name = "strike"
kind = "dimension"
[[vanilla.columns]]
name = "option_type"
kind = "dimension"
[[vanilla.columns]]
name = "currency"
kind = "dimension"
[[vanilla.columns]]
name = "spot_shift"
kind = "dimension"
[[vanilla.columns]]
name = "vol_shift"
kind = "dimension"
[[vanilla.columns]]
name = "npv"
[[vanilla.columns]]
name = "delta01"
[[vanilla.columns]]
name = "gamma01"
[[vanilla.columns]]
name = "vega01"
[[vanilla.columns]]
name = "clean_theta_business_day"
[[vanilla.columns]]
name = "rho010"
[[vanilla.columns]]
name = "status"
kind = "dimension"

[barrier]
dataset = "pricer"
[[barrier.columns]]
name = "qty"
kind = "dimension"
[[barrier.columns]]
name = "underlying_ref"
kind = "dimension"
[[barrier.columns]]
name = "expiry"
kind = "dimension"
[[barrier.columns]]
name = "strike"
kind = "dimension"
[[barrier.columns]]
name = "option_type"
kind = "dimension"
[[barrier.columns]]
name = "barrier"
kind = "dimension"
[[barrier.columns]]
name = "barrier_type"
kind = "dimension"
[[barrier.columns]]
name = "currency"
kind = "dimension"
[[barrier.columns]]
name = "spot_shift"
kind = "dimension"
[[barrier.columns]]
name = "vol_shift"
kind = "dimension"
[[barrier.columns]]
name = "npv"
[[barrier.columns]]
name = "delta01"
[[barrier.columns]]
name = "gamma01"
[[barrier.columns]]
name = "vega01"
[[barrier.columns]]
name = "clean_theta_business_day"
[[barrier.columns]]
name = "rho010"
[[barrier.columns]]
name = "status"
kind = "dimension"
"#;

/// A vocabulary column with the presentation `load_views` merged for it
/// (the view's own keys under the two overlays).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewColumn {
    pub def: &'static ColumnDef,
    pub presentation: ColumnPresentation,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PricerView {
    pub name: String,
    pub columns: Vec<ViewColumn>,
}

/// The loaded pricer views, in `views` doc order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Views {
    views: Vec<PricerView>,
}

impl Views {
    /// Keep the specs whose `dataset` is the pricer's and resolve their
    /// columns against the vocabulary. A view with a join or a derived
    /// column is refused whole: the pricer evaluates nothing, so painting
    /// the columns it could resolve would show a view that is not the one
    /// declared. An unknown column is dropped from the view with an error;
    /// a view left with no column is dropped. Diagnostics are rooted at
    /// `views.<view>`, the path the Views dialog and the diagnostics tile
    /// already know.
    pub fn from_specs(specs: &[ViewSpec]) -> (Views, Vec<Diagnostic>) {
        let mut out = Views::default();
        let mut diags = Vec::new();
        for spec in specs.iter().filter(|s| s.dataset == PRICER_DATASET) {
            let name = &spec.name;
            let bad = |m: String| Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("view '{name}': {m}"),
                path: Some(format!("views.{name}")),
            };
            let mut refused = false;
            if !spec.joins.is_empty() {
                diags.push(bad(format!(
                    "joins are not supported on computed dataset '{PRICER_DATASET}'"
                )));
                refused = true;
            }
            if spec
                .columns
                .iter()
                .any(|c| matches!(c, SpecColumn::Derived { .. }))
            {
                diags.push(bad(format!(
                    "derived columns are not supported on computed dataset '{PRICER_DATASET}'"
                )));
                refused = true;
            }
            if refused {
                // Unhonourable: refuse rather than paint a partial view.
                continue;
            }
            let mut columns: Vec<ViewColumn> = Vec::with_capacity(spec.columns.len());
            for c in &spec.columns {
                let Some(def) = column(c.name()) else {
                    diags.push(bad(format!("unknown column '{}'; dropped", c.name())));
                    continue;
                };
                if columns.iter().any(|v| v.def.name == def.name) {
                    // `ViewSpec::from_doc` already warns on a repeat.
                    continue;
                }
                columns.push(ViewColumn {
                    def,
                    presentation: spec.presentation_of(c.name()),
                });
            }
            if columns.is_empty() {
                diags.push(bad("no valid column; view dropped".into()));
                continue;
            }
            out.views.push(PricerView {
                name: name.clone(),
                columns,
            });
        }
        (out, diags)
    }

    /// The bundled views read as the application reads them. Invalid TOML
    /// is a programmer error; tests require these definitions to produce
    /// no diagnostics.
    pub fn builtin() -> Views {
        let doc = merge_docs(
            "views",
            &[LayerDoc::builtin("views", BUILTIN_VIEWS)
                .expect("BUILTIN_VIEWS is well-formed TOML")],
        );
        Views::from_specs(&ViewSpec::from_doc(&doc).0).0
    }

    pub fn get(&self, name: &str) -> Option<&PricerView> {
        self.views.iter().find(|v| v.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.views.iter().map(|v| v.name.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

/// One column as the table paints it: label, width and format resolved
/// from the vocabulary's defaults under the view's presentation.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedColumn {
    pub def: &'static ColumnDef,
    pub label: String,
    pub width: f32,
    pub format: ColumnFormat,
}

/// Columns in view order, excluding the delegate's tree column and any
/// column the merged presentation hides. The plan depends on the view
/// alone, so an empty sheet retains the same columns; a view whose every
/// column is hidden plans none, which the tile reports in its header.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnPlan {
    pub columns: Vec<PlannedColumn>,
}

impl ColumnPlan {
    pub fn build(view: &PricerView) -> ColumnPlan {
        ColumnPlan {
            columns: view
                .columns
                .iter()
                .filter(|c| !c.presentation.hidden.unwrap_or(false))
                .map(|c| PlannedColumn {
                    def: c.def,
                    label: c
                        .presentation
                        .label
                        .clone()
                        .unwrap_or_else(|| c.def.label.to_string()),
                    width: c.presentation.width.unwrap_or(c.def.default_width),
                    format: c.def.default_format.clone().with(&c.presentation),
                })
                .collect(),
        }
    }

    /// Move the column at plan index `from` so it sits at `to`, as a
    /// header drag asks. An index off the plan, or a no-op move, leaves
    /// the plan as it was: the order is the open tile's alone and never
    /// reaches the view.
    pub fn move_column(&mut self, from: usize, to: usize) {
        if from < self.columns.len() && to < self.columns.len() && from != to {
            let c = self.columns.remove(from);
            self.columns.insert(to, c);
        }
    }

    /// The plan index of the column named `name`, so a cursor that names
    /// its column can find it again after a move.
    pub fn position_of(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.def.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, Severity, merge_docs};
    use geode_core::view::ViewSpec;

    fn specs(text: &str) -> Vec<ViewSpec> {
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0
    }

    #[test]
    fn move_column_reorders_the_plan_and_position_of_finds_by_name() {
        let (views, _) = Views::from_specs(&specs(BUILTIN_VIEWS));
        let mut plan = ColumnPlan::build(views.get("vanilla").unwrap());
        plan.move_column(0, 2); // qty after expiry
        assert_eq!(
            plan.columns
                .iter()
                .take(3)
                .map(|c| c.def.name)
                .collect::<Vec<_>>(),
            ["underlying_ref", "expiry", "qty"]
        );
        assert_eq!(plan.position_of("qty"), Some(2));
        assert_eq!(plan.position_of("nonesuch"), None);
        // Out of range or a no-op: the plan is left as it was.
        let before = plan.clone();
        plan.move_column(0, 99);
        plan.move_column(99, 0);
        plan.move_column(1, 1);
        assert_eq!(plan, before);
    }

    #[test]
    fn the_two_bundled_views_load_clean() {
        let (views, diags) = Views::from_specs(&specs(BUILTIN_VIEWS));
        assert!(diags.is_empty(), "{diags:?}");
        // `ViewSpec::from_doc` orders views by name when the doc names no
        // `default`; the pricer keeps that order.
        assert_eq!(
            views.names().collect::<Vec<_>>(),
            vec!["barrier", "vanilla"]
        );
        let names = |v: &str| {
            views
                .get(v)
                .unwrap()
                .columns
                .iter()
                .map(|c| c.def.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("vanilla"),
            [
                "qty",
                "underlying_ref",
                "expiry",
                "strike",
                "option_type",
                "currency",
                "spot_shift",
                "vol_shift",
                "npv",
                "delta01",
                "gamma01",
                "vega01",
                "clean_theta_business_day",
                "rho010",
                "status"
            ]
        );
        assert_eq!(
            names("barrier"),
            [
                "qty",
                "underlying_ref",
                "expiry",
                "strike",
                "option_type",
                "barrier",
                "barrier_type",
                "currency",
                "spot_shift",
                "vol_shift",
                "npv",
                "delta01",
                "gamma01",
                "vega01",
                "clean_theta_business_day",
                "rho010",
                "status"
            ]
        );
    }

    #[test]
    fn builtin_is_the_bundled_doc_read_through_from_specs() {
        assert_eq!(Views::builtin(), Views::from_specs(&specs(BUILTIN_VIEWS)).0);
        assert!(!Views::builtin().is_empty());
    }

    #[test]
    fn a_view_over_another_dataset_is_not_a_pricer_view() {
        let (views, diags) = Views::from_specs(&specs(
            "[tree]\ndataset = \"risk_snapshot\"\n[[tree.columns]]\nname = \"npv\"\n",
        ));
        assert!(views.is_empty() && diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn a_pricer_view_with_a_derived_column_or_join_is_refused() {
        let text = "[x]\ndataset = \"pricer\"\njoins = [{ dataset = \"ref\", on = [\"underlying_ref\"] }]\n[[x.columns]]\nname = \"npv\"\n[[x.columns]]\nname = \"twice\"\nkind = \"derived\"\nsql = \"npv * 2\"\n";
        let (views, diags) = Views::from_specs(&specs(text));
        assert!(
            views.get("x").is_none(),
            "the view is dropped, not painted partially"
        );
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            messages
                .iter()
                .any(|m| m
                    .contains("derived columns are not supported on computed dataset 'pricer'")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("joins are not supported on computed dataset 'pricer'")),
            "{messages:?}"
        );
        assert!(
            diags
                .iter()
                .all(|d| d.severity == Severity::Error && d.path.as_deref() == Some("views.x")),
            "{diags:?}"
        );
    }

    #[test]
    fn an_unknown_measure_is_dropped_from_the_pricer_view_not_the_view() {
        let (views, diags) = Views::from_specs(&specs(
            "[x]\ndataset = \"pricer\"\n[[x.columns]]\nname = \"npv\"\n[[x.columns]]\nname = \"daily_pnl\"\n",
        ));
        assert_eq!(views.get("x").unwrap().columns.len(), 1);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("unknown column 'daily_pnl'")
                    && d.severity == Severity::Error),
            "{diags:?}"
        );
    }

    #[test]
    fn a_view_with_no_valid_column_is_dropped_and_a_repeat_is_kept_once() {
        let (views, diags) = Views::from_specs(&specs(
            "[x]\ndataset = \"pricer\"\n[[x.columns]]\nname = \"nonesuch\"\n",
        ));
        assert!(views.is_empty());
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("no valid column; view dropped")
                    && d.path.as_deref() == Some("views.x")),
            "{diags:?}"
        );
        let (views, _) = Views::from_specs(&specs(
            "[x]\ndataset = \"pricer\"\n[[x.columns]]\nname = \"npv\"\n[[x.columns]]\nname = \"npv\"\n",
        ));
        assert_eq!(views.get("x").unwrap().columns.len(), 1);
    }

    #[test]
    fn a_hidden_column_is_left_out_of_the_plan() {
        let mut s = specs(
            "[x]\ndataset = \"pricer\"\n[[x.columns]]\nname = \"qty\"\nkind = \"dimension\"\n[[x.columns]]\nname = \"npv\"\n",
        );
        s[0].presentation.entry("qty".into()).or_default().hidden = Some(true);
        let (views, _) = Views::from_specs(&s);
        let plan = ColumnPlan::build(views.get("x").unwrap());
        assert_eq!(
            plan.columns.iter().map(|c| c.def.name).collect::<Vec<_>>(),
            ["npv"]
        );
    }

    #[test]
    fn a_plan_resolves_label_width_and_format_from_the_merged_presentation() {
        let text = "[x]\ndataset = \"pricer\"\n[[x.columns]]\nname = \"npv\"\nlabel = \"PX\"\nwidth = 120\nformat = { precision = 4, thousands = false }\n";
        let (views, _) = Views::from_specs(&specs(text));
        let c = &ColumnPlan::build(views.get("x").unwrap()).columns[0];
        assert_eq!(
            (
                c.label.as_str(),
                c.width,
                c.format.precision,
                c.format.thousands
            ),
            ("PX", 120.0, 4, false)
        );
    }

    #[test]
    fn a_plan_falls_back_to_the_vocabulary_defaults() {
        let (views, _) = Views::from_specs(&specs(
            "[x]\ndataset = \"pricer\"\n[[x.columns]]\nname = \"qty\"\nkind = \"dimension\"\n[[x.columns]]\nname = \"spot_shift\"\nkind = \"dimension\"\n[[x.columns]]\nname = \"barrier_type\"\nkind = \"dimension\"\n[[x.columns]]\nname = \"npv\"\nformat = { precision = 4 }\n",
        ));
        let plan = ColumnPlan::build(views.get("x").unwrap());
        let qty = &plan.columns[0];
        assert_eq!(qty.label, "qty", "the default label when none is set");
        assert_eq!(qty.width, column("qty").unwrap().default_width);
        assert_eq!(qty.format, column("qty").unwrap().default_format);
        let labels: Vec<&str> = plan.columns.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            ["qty", "spot %", "barrier type", "npv"],
            "readable words, never the snake_case name"
        );
        let price = &plan.columns[3];
        assert_eq!(price.format.precision, 4);
        assert!(
            price.format.thousands,
            "the default fills what the presentation left"
        );
        // Both bundled views plan every column they name.
        for name in ["vanilla", "barrier"] {
            let v = Views::builtin();
            let view = v.get(name).unwrap();
            assert_eq!(
                ColumnPlan::build(view).columns.len(),
                view.columns.len(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_view_with_every_column_hidden_paints_no_column_and_says_so() {
        // ColumnPlan side: an empty plan is legal.
        let mut s = specs("[x]\ndataset = \"pricer\"\n[[x.columns]]\nname = \"npv\"\n");
        s[0].presentation.entry("npv".into()).or_default().hidden = Some(true);
        let (views, _) = Views::from_specs(&s);
        assert!(
            ColumnPlan::build(views.get("x").unwrap())
                .columns
                .is_empty()
        );
    }
}
