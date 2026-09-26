//! The `pricer_views` doc (line-pricer spec §6.5): named column sets
//! over the fixed vocabulary in `columns`, with the blotter's
//! presentation keys, resolved through `geode_core::view::
//! {ColumnFormat, ColumnPresentation}` so formatting code is shared.
//! Two bundled views ship as [`BUILTIN_VIEWS`]; desk and user layers
//! override by name (`merge::atomic_depth`).

use crate::core::columns::{ColumnDef, column};
use geode_core::config::{Diagnostic, MergedDoc, Severity};
use geode_core::view::{ColumnFormat, ColumnPresentation};
use std::cell::RefCell;

pub const PRICER_VIEWS_DOC: &str = "pricer_views";

/// Bundled column sets installed through the factory's builtin documents. Both include
/// status so pending pricing and failures are visible in text as well as through cell
/// colours.
pub const BUILTIN_VIEWS: &str = r#"[vanilla]
columns = ["qty", "underlying", "expiry", "strike", "type", "spot_shift", "vol_shift",
           "price", "delta", "gamma", "vega", "theta", "rho", "status"]

[barrier]
columns = ["qty", "underlying", "expiry", "strike", "type", "barrier", "barrier_type", "spot_shift", "vol_shift",
           "price", "delta", "gamma", "vega", "theta", "rho", "status"]
"#;

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

/// The loaded views, in doc order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Views {
    views: Vec<PricerView>,
}

impl Views {
    /// Every diagnostic carries `path` (`pricer_views.<view>…`); `layer`
    /// and `file` are `None`, as every reader's are.
    pub fn from_doc(doc: &MergedDoc) -> (Views, Vec<Diagnostic>) {
        let mut out = Views::default();
        let mut diags = Vec::new();
        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let at = |suffix: &str| {
                if suffix.is_empty() {
                    format!("{PRICER_VIEWS_DOC}.{name}")
                } else {
                    format!("{PRICER_VIEWS_DOC}.{name}.{suffix}")
                }
            };
            let report = |severity: Severity, suffix: &str, m: String| Diagnostic {
                severity,
                layer: None,
                file: None,
                message: format!("pricer view '{name}': {m}"),
                path: Some(at(suffix)),
            };
            let Some(table) = value.as_table() else {
                diags.push(report(Severity::Error, "", "not a table; dropped".into()));
                continue;
            };
            let Some(cols) = table.get("columns").and_then(|v| v.as_array()) else {
                diags.push(report(
                    Severity::Error,
                    "",
                    "missing 'columns' array; view dropped".into(),
                ));
                continue;
            };
            let mut columns: Vec<ViewColumn> = Vec::with_capacity(cols.len());
            // `enumerate()` before any filtering, so an index in a path is
            // the element's real position in the file (4c §19.5).
            for (i, c) in cols.iter().enumerate() {
                let (col_name, table) = match (c.as_str(), c.as_table()) {
                    (Some(s), _) => (s, None),
                    (None, Some(t)) => match t.get("name").and_then(|v| v.as_str()) {
                        Some(s) => (s, Some(t)),
                        None => {
                            diags.push(report(
                                Severity::Error,
                                &format!("columns.{i}.name"),
                                "column table has no 'name'; dropped".into(),
                            ));
                            continue;
                        }
                    },
                    (None, None) => {
                        diags.push(report(
                            Severity::Error,
                            &format!("columns.{i}"),
                            format!("column must be a name or a table (got {c}); dropped"),
                        ));
                        continue;
                    }
                };
                let Some(def) = column(col_name) else {
                    diags.push(report(
                        Severity::Error,
                        &format!("columns.{i}"),
                        format!("unknown column '{col_name}'; dropped"),
                    ));
                    continue;
                };
                if columns.iter().any(|existing| existing.def.name == def.name) {
                    diags.push(report(
                        Severity::Warning,
                        &format!("columns.{i}"),
                        format!("column '{col_name}' repeated; the repeat is dropped"),
                    ));
                    continue;
                }
                let mut presentation = ColumnPresentation::default();
                if let Some(t) = table {
                    // A `RefCell` so `warn` stays a `Fn` for the two
                    // `&dyn Fn` readers, the way `ViewSpec::from_doc` does it.
                    let col_diags = RefCell::new(Vec::new());
                    let warn = |key: &str, m: String| {
                        col_diags.borrow_mut().push(report(
                            Severity::Warning,
                            &format!("columns.{i}.{key}"),
                            format!("column '{col_name}': {m}"),
                        ));
                    };
                    presentation.parse_column_keys(t, false, &warn);
                    if let Some(f) = t.get("format") {
                        match f.as_table() {
                            None => warn("format", "'format' is not a table".into()),
                            Some(f) => presentation
                                .parse_format_keys(f, &|key, m| warn(&format!("format.{key}"), m)),
                        }
                    }
                    diags.extend(col_diags.into_inner());
                }
                columns.push(ViewColumn { def, presentation });
            }
            if columns.is_empty() {
                diags.push(report(
                    Severity::Error,
                    "",
                    "no valid column; view dropped".into(),
                ));
                continue;
            }
            out.views.push(PricerView {
                name: name.clone(),
                columns,
            });
        }
        (out, diags)
    }

    /// `BUILTIN_VIEWS` parsed. The constant is authored with the binary;
    /// `the_two_bundled_views_load_clean` pins that it parses with no
    /// diagnostic, so the `expect`s cannot fire in a shipped build.
    pub fn builtin() -> Views {
        let doc = geode_core::config::LayerDoc::builtin(PRICER_VIEWS_DOC, BUILTIN_VIEWS)
            .expect("BUILTIN_VIEWS is well-formed TOML");
        let merged = geode_core::config::merge_docs(PRICER_VIEWS_DOC, &[doc]);
        Views::from_doc(&merged).0
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

/// The table's columns in view order, after the tree column (which is the
/// delegate's own, Part 3). Takes no sheet: the column set never depends
/// on the rows (planning decision 7).
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, Severity, merge_docs};

    fn doc(text: &str) -> MergedDoc {
        merge_docs(
            PRICER_VIEWS_DOC,
            &[LayerDoc::builtin(PRICER_VIEWS_DOC, text).expect("well-formed test TOML")],
        )
    }

    fn names(v: &PricerView) -> Vec<&str> {
        v.columns.iter().map(|c| c.def.name).collect()
    }

    #[test]
    fn the_two_bundled_views_load_clean() {
        let (views, diags) = Views::from_doc(&doc(BUILTIN_VIEWS));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            views.names().collect::<Vec<_>>(),
            vec!["vanilla", "barrier"]
        );
        let vanilla = views.get("vanilla").unwrap();
        assert_eq!(
            names(vanilla),
            vec![
                "qty",
                "underlying",
                "expiry",
                "strike",
                "type",
                "spot_shift",
                "vol_shift",
                "price",
                "delta",
                "gamma",
                "vega",
                "theta",
                "rho",
                "status"
            ]
        );
        let barrier = views.get("barrier").unwrap();
        assert_eq!(
            names(barrier),
            vec![
                "qty",
                "underlying",
                "expiry",
                "strike",
                "type",
                "barrier",
                "barrier_type",
                "spot_shift",
                "vol_shift",
                "price",
                "delta",
                "gamma",
                "vega",
                "theta",
                "rho",
                "status"
            ]
        );
        assert_eq!(Views::builtin(), views);
        assert!(views.get("npv").is_none());
    }

    #[test]
    fn an_unknown_column_is_an_error_and_dropped() {
        let (views, diags) =
            Views::from_doc(&doc("[v]\ncolumns = [\"qty\", \"npv\", \"price\"]\n"));
        assert_eq!(names(views.get("v").unwrap()), vec!["qty", "price"]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].path.as_deref(), Some("pricer_views.v.columns.1"));
        assert!(diags[0].message.contains("npv"), "{}", diags[0].message);
        assert!(diags[0].message.contains("dropped"), "{}", diags[0].message);
    }

    #[test]
    fn a_view_with_no_valid_column_is_dropped_with_an_error() {
        let (views, diags) = Views::from_doc(&doc(
            "config_version = 1\n[empty]\ncolumns = []\n[bad]\ncolumns = [\"npv\"]\n[ok]\ncolumns = [\"price\"]\n[notatable]\n",
        ));
        assert_eq!(views.names().collect::<Vec<_>>(), vec!["ok"]);
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert!(paths.contains(&"pricer_views.empty"), "{diags:?}");
        assert!(paths.contains(&"pricer_views.bad"), "{diags:?}");
        assert!(paths.contains(&"pricer_views.bad.columns.0"), "{diags:?}");
        assert!(paths.contains(&"pricer_views.notatable"), "{diags:?}");
        assert!(
            diags
                .iter()
                .filter(|d| d.path.as_deref() == Some("pricer_views.empty"))
                .all(|d| d.severity == Severity::Error)
        );
        // A view whose `columns` is missing or not an array.
        let (views, diags) = Views::from_doc(&doc("[v]\nname = \"x\"\n[w]\ncolumns = \"price\"\n"));
        assert!(views.is_empty());
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .count(),
            2,
            "{diags:?}"
        );
    }

    #[test]
    fn the_table_form_carries_label_width_and_format() {
        let (views, diags) = Views::from_doc(&doc(
            "[v]\ncolumns = [\n  \"qty\",\n  { name = \"price\", label = \"PX\", width = 120, format = { precision = 4, thousands = false } },\n  { name = \"delta\", format = { precision = 99 } },\n  { label = \"no name\" },\n  { name = \"qty\" },\n  { name = \"vega\", width = -1 },\n]\n",
        ));
        let v = views.get("v").unwrap();
        assert_eq!(
            names(v),
            vec!["qty", "price", "delta", "vega"],
            "the nameless and the repeat are dropped"
        );
        let price = &v.columns[1];
        assert_eq!(price.presentation.label.as_deref(), Some("PX"));
        assert_eq!(price.presentation.width, Some(120.0));
        assert_eq!(price.presentation.precision, Some(4));
        assert_eq!(price.presentation.thousands, Some(false));
        assert_eq!(
            v.columns[2].presentation.precision, None,
            "99 is out of range and warned"
        );
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert!(
            paths.contains(&"pricer_views.v.columns.2.format.precision"),
            "{diags:?}"
        );
        assert!(
            paths.contains(&"pricer_views.v.columns.3.name"),
            "{diags:?}"
        );
        assert!(
            paths.contains(&"pricer_views.v.columns.4"),
            "the repeat: {diags:?}"
        );
        assert!(
            paths.contains(&"pricer_views.v.columns.5.width"),
            "{diags:?}"
        );
        assert!(
            diags
                .iter()
                .filter(|d| d.path.as_deref() == Some("pricer_views.v.columns.4"))
                .all(|d| d.severity == Severity::Warning)
        );
        // A non-string, non-table element.
        let (views, diags) = Views::from_doc(&doc("[v]\ncolumns = [\"qty\", 3]\n"));
        assert_eq!(names(views.get("v").unwrap()), vec!["qty"]);
        assert_eq!(diags[0].path.as_deref(), Some("pricer_views.v.columns.1"));
    }

    #[test]
    fn a_plan_resolves_label_width_and_format_from_the_defaults_under_the_presentation() {
        let (views, _) = Views::from_doc(&doc(
            "[v]\ncolumns = [\"qty\", { name = \"price\", label = \"PX\", width = 120, format = { precision = 4 } }, \"barrier\"]\n",
        ));
        let plan = ColumnPlan::build(views.get("v").unwrap());
        assert_eq!(plan.columns.len(), 3);
        let qty = &plan.columns[0];
        assert_eq!(qty.def.name, "qty");
        assert_eq!(qty.label, "qty", "the default label when none is set");
        assert_eq!(qty.width, column("qty").unwrap().default_width);
        assert_eq!(qty.format, column("qty").unwrap().default_format);
        let price = &plan.columns[1];
        assert_eq!(price.label, "PX");
        assert_eq!(price.width, 120.0);
        assert_eq!(price.format.precision, 4);
        assert!(
            price.format.thousands,
            "the default fills what the presentation left"
        );
        let (views, _) =
            Views::from_doc(&doc("[v]\ncolumns = [\"spot_shift\", \"barrier_type\"]\n"));
        let labels: Vec<String> = ColumnPlan::build(views.get("v").unwrap())
            .columns
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(
            labels,
            vec!["spot %", "barrier type"],
            "readable words, never the snake_case name"
        );
        assert_eq!(
            plan.columns[2].def.name, "barrier",
            "a column is planned whether or not any row is a barrier"
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
}
