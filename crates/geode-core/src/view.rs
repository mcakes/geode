//! View definitions (spec §5.1, §6.1): dataset, joins, columns, derived
//! columns, grouping and sort, declared as config. Users create views
//! through the UI or by writing config; both produce the same file
//! (PHILOSOPHY §5).
//!
//! A view is data, not code — it names columns and expressions, and the
//! compiler (geode-data) turns it into one statement.

use std::collections::BTreeMap;

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::dimensions::DerivedDimensions;
use crate::schema::{ColumnRole, Grain, SchemaSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinSpec {
    pub dataset: String,
    /// Join key columns, declared in schema config (spec §5.4).
    pub on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewColumn {
    Dimension {
        name: String,
    },
    Measure {
        name: String,
    },
    /// A SQL expression over other columns of the same view.
    Derived {
        name: String,
        sql: String,
    },
}

impl ViewColumn {
    pub fn name(&self) -> &str {
        match self {
            ViewColumn::Dimension { name }
            | ViewColumn::Measure { name }
            | ViewColumn::Derived { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub column: String,
    pub descending: bool,
}

/// How a negative number is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Negative {
    Minus,
    Parens,
}

/// Whether a number's sign colours the cell (`chart_bullish` /
/// `chart_bearish` in the theme).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    None,
    Sign,
}

/// Divide before display: `k` by a thousand, `M` by a million.
/// `precision` applies to the divided number (Phase 3 §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    None,
    Thousands,
    Millions,
}

impl Scale {
    pub fn divisor(self) -> f64 {
        match self {
            Scale::None => 1.0,
            Scale::Thousands => 1_000.0,
            Scale::Millions => 1_000_000.0,
        }
    }

    /// What the header shows after the label.
    pub fn suffix(self) -> &'static str {
        match self {
            Scale::None => "",
            Scale::Thousands => "k",
            Scale::Millions => "M",
        }
    }
}

/// A resolved format: every field decided (Phase 3 §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnFormat {
    pub precision: u8,
    pub thousands: bool,
    pub negative: Negative,
    pub colour: Colour,
    pub scale: Scale,
}

impl ColumnFormat {
    /// The default for a measure or derived number.
    pub const MEASURE: ColumnFormat = ColumnFormat {
        precision: 2,
        thousands: true,
        negative: Negative::Minus,
        colour: Colour::Sign,
        scale: Scale::None,
    };
    /// The default for a dimension or attribute.
    pub const TEXT: ColumnFormat = ColumnFormat {
        precision: 0,
        thousands: false,
        negative: Negative::Minus,
        colour: Colour::None,
        scale: Scale::None,
    };

    /// This default with the presentation's overrides applied.
    pub fn with(self, p: &ColumnPresentation) -> ColumnFormat {
        ColumnFormat {
            precision: p.precision.unwrap_or(self.precision),
            thousands: p.thousands.unwrap_or(self.thousands),
            negative: p.negative.unwrap_or(self.negative),
            colour: p.colour.unwrap_or(self.colour),
            scale: p.scale.unwrap_or(self.scale),
        }
    }
}

/// What a view says about how a column looks — each field optional, so
/// a per-kind default fills the rest at plan time. Keyed by column name
/// on the view rather than carried on `ViewColumn`, so the compiler's
/// matching on that enum is untouched.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ColumnPresentation {
    pub precision: Option<u8>,
    pub thousands: Option<bool>,
    pub negative: Option<Negative>,
    pub colour: Option<Colour>,
    pub scale: Option<Scale>,
    pub label: Option<String>,
    pub width: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct ViewSpec {
    pub name: String,
    pub dataset: String,
    pub joins: Vec<JoinSpec>,
    pub columns: Vec<ViewColumn>,
    /// Ordered: each prefix is one level of the rollup tree (§6.3).
    pub grouping: Vec<String>,
    pub sort: Vec<SortKey>,
    pub presentation: BTreeMap<String, ColumnPresentation>,
}

impl ViewSpec {
    /// Distinct grains of the measures this view selects, coarse first.
    /// The compiler emits one aggregate subquery per grain.
    pub fn measure_grains(&self, schema: &SchemaSpec) -> Vec<Grain> {
        let Some(ds) = schema.dataset(&self.dataset) else {
            return Vec::new();
        };
        let mut out: Vec<Grain> = self
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Measure { name } => ds.column(name),
                _ => None,
            })
            .filter(|c| matches!(c.role, ColumnRole::Measure { .. }))
            .filter_map(|c| c.grain())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The presentation declared for a column, or the empty one.
    pub fn presentation_of(&self, column: &str) -> ColumnPresentation {
        self.presentation.get(column).cloned().unwrap_or_default()
    }

    /// Check the view against the schema it will compile against.
    ///
    /// Takes the derived dimensions because §6.8 names are not dataset
    /// columns: `desk` is computed from `book` and is absent from every
    /// CSV. Validating without them would report the feature's own
    /// vocabulary as unknown, which is why this could not simply be wired
    /// up as it stood.
    pub fn validate(&self, schema: &SchemaSpec, dims: &DerivedDimensions) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        let bad = |m: String| Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!("view '{}': {m}", self.name),
        };

        let Some(ds) = schema.dataset(&self.dataset) else {
            diags.push(bad(format!("unknown dataset '{}'", self.dataset)));
            return diags;
        };

        for j in &self.joins {
            if schema.dataset(&j.dataset).is_none() {
                diags.push(bad(format!("join names unknown dataset '{}'", j.dataset)));
            }
        }

        // A derived column may reference other view columns, so only
        // dimension and measure columns are checked against the schema.
        //
        // A derived *dimension* (§6.8) is resolved here for the same
        // reason it is in the grouping loop below: `desk` is computed from
        // `book` and is in no CSV, so checking it against the dataset
        // alone reports the feature's own vocabulary as unknown. Fixing
        // only the grouping loop left a view that names `desk` in both
        // places still rejected.
        for c in &self.columns {
            match c {
                ViewColumn::Derived { .. } => {}
                other => {
                    let name = other.name();
                    if let Some(d) = dims.get(name) {
                        if ds.column(&d.from).is_none() {
                            diags.push(bad(format!(
                                "column '{name}' is derived from '{}', which dataset '{}' does not have",
                                d.from, self.dataset
                            )));
                        }
                        continue;
                    }
                    if ds.column(name).is_none()
                        && !self.joins.iter().any(|j| {
                            schema
                                .dataset(&j.dataset)
                                .is_some_and(|d| d.column(name).is_some())
                        })
                    {
                        diags.push(bad(format!("unknown column '{name}'")));
                    }
                }
            }
        }

        for g in &self.grouping {
            // A derived dimension that shadows a real column is worse than
            // an unknown one: the group-by resolves to whichever the
            // compiler reaches first, so the answer is quietly the wrong
            // one rather than absent. Reported here, at load, because
            // nothing downstream can tell which was meant.
            if let Some(d) = dims.get(g) {
                if ds.column(g).is_some() {
                    diags.push(bad(format!(
                        "grouping '{g}' shadows a real column of dataset '{}': \
                         it is also a derived dimension from '{}'. Rename one.",
                        self.dataset, d.from
                    )));
                } else if ds.column(&d.from).is_none() {
                    diags.push(bad(format!(
                        "grouping '{g}' is derived from '{}', which dataset '{}' does not have",
                        d.from, self.dataset
                    )));
                }
                continue;
            }
            if ds.column(g).is_none() {
                diags.push(bad(format!("grouping names unknown column '{g}'")));
            }
        }

        diags
    }

    pub fn from_doc(doc: &MergedDoc) -> (Vec<ViewSpec>, Vec<Diagnostic>) {
        let mut out = Vec::new();
        let mut diags = Vec::new();

        for (name, value) in &doc.value {
            let bad = |m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("view '{name}': {m}"),
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("not a table".into()));
                continue;
            };

            let mut view = ViewSpec {
                name: name.clone(),
                dataset: table
                    .get("dataset")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                ..ViewSpec::default()
            };
            if view.dataset.is_empty() {
                diags.push(bad("missing 'dataset'".into()));
            }

            view.grouping = table
                .get("grouping")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            if let Some(joins) = table.get("joins").and_then(|v| v.as_array()) {
                for j in joins.iter().filter_map(|v| v.as_table()) {
                    let Some(dataset) = j.get("dataset").and_then(|v| v.as_str()) else {
                        diags.push(bad("join missing 'dataset'".into()));
                        continue;
                    };
                    view.joins.push(JoinSpec {
                        dataset: dataset.to_string(),
                        on: j
                            .get("on")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                }
            }

            if let Some(cols) = table.get("columns").and_then(|v| v.as_array()) {
                for c in cols.iter().filter_map(|v| v.as_table()) {
                    let Some(col_name) = c.get("name").and_then(|v| v.as_str()) else {
                        diags.push(bad("column missing 'name'".into()));
                        continue;
                    };
                    let kind = c.get("kind").and_then(|v| v.as_str()).unwrap_or("measure");
                    let column = match kind {
                        "dimension" => ViewColumn::Dimension {
                            name: col_name.to_string(),
                        },
                        "measure" => ViewColumn::Measure {
                            name: col_name.to_string(),
                        },
                        "derived" => match c.get("sql").and_then(|v| v.as_str()) {
                            Some(sql) => ViewColumn::Derived {
                                name: col_name.to_string(),
                                sql: sql.to_string(),
                            },
                            None => {
                                diags
                                    .push(bad(format!("derived column '{col_name}' has no 'sql'")));
                                continue;
                            }
                        },
                        other => {
                            diags.push(bad(format!(
                                "column '{col_name}' has unknown kind '{other}'"
                            )));
                            continue;
                        }
                    };
                    view.columns.push(column);

                    let mut p = ColumnPresentation::default();
                    let warn = |m: String| bad(format!("column '{col_name}': {m}"));
                    if let Some(f) = c.get("format") {
                        match f.as_table() {
                            None => diags.push(warn("'format' is not a table".into())),
                            Some(f) => {
                                match f.get("precision") {
                                    None => {}
                                    Some(v) => match v.as_integer() {
                                        Some(n) if (0..=12).contains(&n) => {
                                            p.precision = Some(n as u8)
                                        }
                                        _ => diags.push(warn(format!(
                                            "'precision' must be an integer 0–12 (got {v})"
                                        ))),
                                    },
                                }
                                match f.get("thousands") {
                                    None => {}
                                    Some(v) => match v.as_bool() {
                                        Some(b) => p.thousands = Some(b),
                                        None => diags.push(warn(format!(
                                            "'thousands' must be true or false (got {v})"
                                        ))),
                                    },
                                }
                                match f.get("negative").and_then(|v| v.as_str()) {
                                    None if f.get("negative").is_none() => {}
                                    Some("minus") => p.negative = Some(Negative::Minus),
                                    Some("parens") => p.negative = Some(Negative::Parens),
                                    other => diags.push(warn(format!(
                                        "'negative' must be \"minus\" or \"parens\" (got {other:?})"
                                    ))),
                                }
                                let colour = f.get("colour").or_else(|| f.get("color"));
                                match colour.and_then(|v| v.as_str()) {
                                    None if colour.is_none() => {}
                                    Some("none") => p.colour = Some(Colour::None),
                                    Some("sign") => p.colour = Some(Colour::Sign),
                                    other => diags.push(warn(format!(
                                        "'colour' must be \"none\" or \"sign\" (got {other:?})"
                                    ))),
                                }
                                match f.get("scale").and_then(|v| v.as_str()) {
                                    None if f.get("scale").is_none() => {}
                                    Some("none") => p.scale = Some(Scale::None),
                                    Some("k") => p.scale = Some(Scale::Thousands),
                                    Some("M") => p.scale = Some(Scale::Millions),
                                    other => diags.push(warn(format!(
                                        "'scale' must be \"none\", \"k\" or \"M\" (got {other:?})"
                                    ))),
                                }
                            }
                        }
                    }
                    if let Some(l) = c.get("label") {
                        match l.as_str() {
                            Some(s) => p.label = Some(s.to_string()),
                            None => diags.push(warn("'label' must be a string".into())),
                        }
                    }
                    if let Some(w) = c.get("width") {
                        match w.as_float().or_else(|| w.as_integer().map(|i| i as f64)) {
                            Some(x) if x > 0.0 => p.width = Some(x as f32),
                            _ => diags
                                .push(warn(format!("'width' must be a positive number (got {w})"))),
                        }
                    }
                    if p != ColumnPresentation::default() {
                        view.presentation.insert(col_name.to_string(), p);
                    }
                }
            }

            if let Some(sorts) = table.get("sort").and_then(|v| v.as_array()) {
                for s in sorts.iter().filter_map(|v| v.as_table()) {
                    let Some(column) = s.get("column").and_then(|v| v.as_str()) else {
                        diags.push(bad("sort entry missing 'column'".into()));
                        continue;
                    };
                    view.sort.push(SortKey {
                        column: column.to_string(),
                        descending: s
                            .get("descending")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    });
                }
            }

            out.push(view);
        }

        (out, diags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()])
    }

    fn dimensions(text: &str) -> DerivedDimensions {
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", text).unwrap()],
        );
        DerivedDimensions::from_doc(&doc).0
    }

    const SAMPLE: &str = r#"
[desk_risk]
dataset = "risk_snapshot"
grouping = ["book", "lhu", "position_ref"]

[[desk_risk.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]

[[desk_risk.columns]]
name = "book"
kind = "dimension"

[[desk_risk.columns]]
name = "delta01"
kind = "measure"

[[desk_risk.columns]]
name = "delta_per_vega"
kind = "derived"
sql = "delta01 / nullif(vega01, 0)"

[[desk_risk.sort]]
column = "delta01"
descending = true
"#;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.vega01]
type = "f64"
role = "measure"
grain = "underlying"
[instrument_ref.columns.instrument_ref]
type = "utf8"
role = "key"
[instrument_ref.columns.strike]
type = "f64"
role = "attribute"
grain = "instrument"
"#;
        let d = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&d).0
    }

    #[test]
    fn parses_a_view_with_joins_columns_grouping_and_sort() {
        let (views, diags) = ViewSpec::from_doc(&doc(SAMPLE));
        assert!(diags.is_empty(), "{diags:?}");
        let v = views.iter().find(|v| v.name == "desk_risk").expect("view");
        assert_eq!(v.dataset, "risk_snapshot");
        assert_eq!(v.grouping, vec!["book", "lhu", "position_ref"]);
        assert_eq!(v.joins.len(), 1);
        assert_eq!(v.joins[0].dataset, "instrument_ref");
        assert_eq!(v.joins[0].on, vec!["instrument_ref"]);
        assert_eq!(
            v.sort,
            vec![SortKey {
                column: "delta01".into(),
                descending: true
            }]
        );
    }

    #[test]
    fn distinguishes_the_three_column_kinds() {
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let v = &views[0];
        assert_eq!(
            v.columns,
            vec![
                ViewColumn::Dimension {
                    name: "book".into()
                },
                ViewColumn::Measure {
                    name: "delta01".into()
                },
                ViewColumn::Derived {
                    name: "delta_per_vega".into(),
                    sql: "delta01 / nullif(vega01, 0)".into(),
                },
            ]
        );
    }

    #[test]
    fn a_derived_column_without_sql_is_a_diagnostic() {
        let (_v, diags) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"d\"\n[[v.columns]]\nname = \"x\"\nkind = \"derived\"\n",
        ));
        assert!(diags.iter().any(|d| d.message.contains("sql")), "{diags:?}");
    }

    #[test]
    fn validation_catches_unknown_columns_grouping_and_datasets() {
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"nosuch\"\ngrouping = [\"nocolumn\"]\n",
        ));
        let diags = views[0].validate(&schema(), &DerivedDimensions::default());
        assert!(
            diags.iter().any(|d| d.message.contains("nosuch")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_well_formed_view_validates_clean() {
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let diags = views[0].validate(&schema(), &DerivedDimensions::default());
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn grouping_by_a_derived_dimension_is_not_an_unknown_column() {
        // §6.8: `desk` is not in the CSVs — it is computed from `book`.
        // Validation that only knows the dataset calls it unknown, so
        // wiring validation up without the derived dimensions would reject
        // every view the feature exists for.
        let dims = dimensions("[desk]\nfrom = \"book\"\n[desk.values]\nBK000 = \"Flow\"\n");
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"risk_snapshot\"\ngrouping = [\"desk\"]\n\
             [[v.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        ));
        let diags = views[0].validate(&schema(), &dims);
        assert!(
            diags.is_empty(),
            "a derived grouping is not an unknown column: {diags:?}"
        );
    }

    #[test]
    fn a_derived_dimension_is_accepted_as_a_column_not_only_as_a_grouping() {
        // §6.8 was honoured in the grouping loop and not the columns loop,
        // so a view naming `desk` in both — the ordinary way to group by a
        // derived dimension and show it — was still rejected as unknown.
        let dims = dimensions("[desk]\nfrom = \"book\"\n[desk.values]\nBK000 = \"Flow\"\n");
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"risk_snapshot\"\ngrouping = [\"desk\"]\n\
             [[v.columns]]\nname = \"desk\"\nkind = \"dimension\"\n\
             [[v.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        ));
        let diags = views[0].validate(&schema(), &dims);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn a_derived_column_whose_source_is_absent_is_reported() {
        let dims = dimensions("[desk]\nfrom = \"nosuch\"\n[desk.values]\nX = \"Flow\"\n");
        let (views, _) = ViewSpec::from_doc(&doc("[v]\ndataset = \"risk_snapshot\"\n\
             [[v.columns]]\nname = \"desk\"\nkind = \"dimension\"\n"));
        let diags = views[0].validate(&schema(), &dims);
        assert!(
            diags.iter().any(|d| d.message.contains("nosuch")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_derived_dimension_that_shadows_a_real_column_is_reported() {
        // Accepted silently, the group-by then runs against whichever the
        // compiler resolves first — the derived mapping or the column of
        // the same name in the data. Both are plausible and only one is
        // meant, so the answer is quietly wrong rather than absent.
        let dims = dimensions("[book]\nfrom = \"lhu\"\n[book.values]\nL0 = \"Flow\"\n");
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
             [[v.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        ));
        let diags = views[0].validate(&schema(), &dims);
        assert!(
            diags.iter().any(|d| d.message.contains("shadows")),
            "expected a shadowing diagnostic, got {diags:?}"
        );
    }

    #[test]
    fn measure_grains_reports_the_distinct_grains_the_view_touches() {
        // The compiler builds one aggregate subquery per grain (§6.3), so
        // this is what decides how many it emits.
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let ds = schema();
        assert_eq!(
            views[0].measure_grains(&ds),
            vec![crate::schema::Grain::Underlying]
        );
    }

    #[test]
    fn presentation_is_parsed_per_column_and_defaults_are_per_kind() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { precision = 0, thousands = true, negative = "parens", colour = "sign", scale = "k" }
label = "NPV"
width = 110
[[tree.columns]]
name = "delta01"
format = { precision = 4 }
[[tree.columns]]
name = "lhu"
kind = "dimension"
"#));
        assert!(diags.is_empty(), "{diags:?}");
        let v = &views[0];
        let npv = v.presentation_of("npv");
        assert_eq!(npv.label.as_deref(), Some("NPV"));
        assert_eq!(npv.width, Some(110.0));
        let f = ColumnFormat::MEASURE.with(&npv);
        assert_eq!(f.precision, 0);
        assert!(f.thousands);
        assert_eq!(f.negative, Negative::Parens);
        assert_eq!(f.colour, Colour::Sign);
        assert_eq!(f.scale, Scale::Thousands);
        assert_eq!(Scale::Thousands.divisor(), 1_000.0);
        assert_eq!(Scale::Millions.divisor(), 1_000_000.0);
        assert_eq!(Scale::None.divisor(), 1.0);
        assert_eq!(Scale::Thousands.suffix(), "k");
        assert_eq!(Scale::Millions.suffix(), "M");
        assert_eq!(Scale::None.suffix(), "");

        let d = ColumnFormat::MEASURE.with(&v.presentation_of("delta01"));
        assert_eq!(d.precision, 4, "one field overrides, the rest default");
        assert!(d.thousands);
        assert_eq!(d.negative, Negative::Minus);
        assert_eq!(d.scale, Scale::None, "unscaled by default");

        let l = ColumnFormat::TEXT.with(&v.presentation_of("lhu"));
        assert_eq!(l.colour, Colour::None);
        assert_eq!(v.presentation_of("nonesuch"), ColumnPresentation::default());
    }

    #[test]
    fn bad_presentation_values_warn_and_are_ignored() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { precision = 40, negative = "red", colour = "loud", thousands = "yes", scale = "bn" }
width = -5
"#));
        let p = views[0].presentation_of("npv");
        assert_eq!(p, ColumnPresentation::default(), "{p:?}");
        assert_eq!(diags.len(), 6, "{diags:?}");
        assert!(diags.iter().all(|d| d.message.contains("npv")));
    }

    #[test]
    fn color_is_accepted_as_a_spelling_of_colour() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { color = "none" }
"#));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(views[0].presentation_of("npv").colour, Some(Colour::None));
    }
}
