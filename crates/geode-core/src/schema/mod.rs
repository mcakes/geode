//! The declared shape of the desk's data (spec §3). Parsed from the
//! `datasets` config doc; every parse failure degrades to a Diagnostic and
//! skips the offending column, never panics (spec §5.7, config §8).

mod column;
mod grain;

pub use column::{Aggregate, ColumnRole, ColumnSpec, ColumnType};
pub use grain::Grain;

use crate::config::{Diagnostic, MergedDoc, Severity};

#[derive(Debug, Clone, Default)]
pub struct DatasetSpec {
    pub name: String,
    pub columns: Vec<ColumnSpec>,
}

impl DatasetSpec {
    pub fn column(&self, name: &str) -> Option<&ColumnSpec> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// Distinct grains present, coarse first — from measures *and*
    /// attributes. A grain carrying only attributes still needs its table:
    /// omitting it would silently drop those columns at ingest.
    pub fn grains(&self) -> Vec<Grain> {
        let mut out: Vec<Grain> = self.columns.iter().filter_map(|c| c.grain()).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn measures_at(&self, grain: Grain) -> impl Iterator<Item = &ColumnSpec> {
        self.columns.iter().filter(move |c| {
            matches!(c.role, ColumnRole::Measure { .. }) && c.grain() == Some(grain)
        })
    }

    pub fn attributes_at(&self, grain: Grain) -> impl Iterator<Item = &ColumnSpec> {
        self.columns.iter().filter(move |c| {
            matches!(c.role, ColumnRole::Attribute { .. }) && c.grain() == Some(grain)
        })
    }

    pub fn textual_columns(&self) -> impl Iterator<Item = &ColumnSpec> {
        self.columns.iter().filter(|c| c.textual)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SchemaSpec {
    pub datasets: Vec<DatasetSpec>,
}

impl SchemaSpec {
    pub fn dataset(&self, name: &str) -> Option<&DatasetSpec> {
        self.datasets.iter().find(|d| d.name == name)
    }

    pub fn from_doc(doc: &MergedDoc) -> (SchemaSpec, Vec<Diagnostic>) {
        let mut out = SchemaSpec::default();
        let mut diags = Vec::new();
        for (ds_name, ds_value) in &doc.value {
            let mut dataset = DatasetSpec {
                name: ds_name.clone(),
                columns: Vec::new(),
            };
            let Some(cols) = ds_value.get("columns").and_then(|v| v.as_table()) else {
                diags.push(note(format!("dataset '{ds_name}': no [columns] table")));
                out.datasets.push(dataset);
                continue;
            };
            for (col_name, col_value) in cols {
                match parse_column(ds_name, col_name, col_value) {
                    Ok(spec) => dataset.columns.push(spec),
                    Err(d) => diags.push(d),
                }
            }
            diags.extend(validate_dataset(&dataset));
            out.datasets.push(dataset);
        }
        (out, diags)
    }
}

/// Column names the storage layer adds to every table (spec §4.2, §4.3).
/// A dataset declaring one of these would generate DDL with a duplicate
/// column and fail at table creation with a raw engine error.
pub const RESERVED_COLUMNS: &[&str] = &["batch", "source_file_id", "gen_id", "source_time"];

/// Checks that can only be made once every column is parsed. Each failure
/// is a Diagnostic, never a panic — bad config degrades (spec §5.7).
fn validate_dataset(ds: &DatasetSpec) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    for c in &ds.columns {
        if RESERVED_COLUMNS.contains(&c.name.as_str()) {
            diags.push(note(format!(
                "dataset '{}' column '{}': name is reserved by the storage \
                 layer ({})",
                ds.name,
                c.name,
                RESERVED_COLUMNS.join(", ")
            )));
        }
    }

    // Every grain in use groups by its key columns, so each must be
    // declared. Undeclared, the generated SQL references a column the
    // staging table does not have and the whole load fails on a binder
    // error rather than a readable diagnostic.
    for grain in ds.grains() {
        for key in grain.key_columns() {
            if ds.column(key).is_none() {
                diags.push(note(format!(
                    "dataset '{}': grain {:?} requires key column '{}', which \
                     is not declared",
                    ds.name, grain, key
                )));
            }
        }
    }

    diags
}

fn note(message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        layer: None,
        file: None,
        message,
    }
}

fn parse_column(ds: &str, name: &str, value: &toml::Value) -> Result<ColumnSpec, Diagnostic> {
    let bad = |m: String| note(format!("dataset '{ds}' column '{name}': {m}"));
    let table = value.as_table().ok_or_else(|| bad("not a table".into()))?;

    let ty_str = table
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("missing 'type'".into()))?;
    let ty = ColumnType::parse(ty_str).ok_or_else(|| bad(format!("unknown type '{ty_str}'")))?;

    let role_str = table
        .get("role")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("missing 'role'".into()))?;

    let grain_of = |table: &toml::Table| -> Result<Grain, Diagnostic> {
        let g = table
            .get("grain")
            .and_then(|v| v.as_str())
            .ok_or_else(|| bad("missing 'grain'".into()))?;
        Grain::parse(g).ok_or_else(|| bad(format!("unknown grain '{g}'")))
    };

    let role = match role_str {
        "key" => ColumnRole::Key,
        "dimension" => ColumnRole::Dimension,
        "attribute" => ColumnRole::Attribute {
            grain: grain_of(table)?,
        },
        "measure" => {
            let agg_str = table
                .get("aggregate")
                .and_then(|v| v.as_str())
                .unwrap_or("sum");
            let aggregate = Aggregate::parse(agg_str)
                .ok_or_else(|| bad(format!("unknown aggregate '{agg_str}'")))?;
            ColumnRole::Measure {
                grain: grain_of(table)?,
                aggregate,
            }
        }
        other => return Err(bad(format!("unknown role '{other}'"))),
    };

    Ok(ColumnSpec {
        name: name.to_string(),
        source_name: table
            .get("source_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ty,
        required: table
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        textual: table
            .get("textual")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        role,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()])
    }

    const SAMPLE: &str = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
textual = true

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

[risk_snapshot.columns.underlying2_ref]
type = "utf8"
role = "dimension"

[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
source_name = "Delta01"

[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
source_name = "DailyTradingPNL"

[risk_snapshot.columns.cross_gamma02]
type = "f64"
role = "measure"
grain = "underlying_pair"
source_name = "CrossGamma02"
required = false
"#;

    #[test]
    fn parses_columns_with_grain_and_source_names() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(SAMPLE));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk_snapshot").expect("dataset");
        assert_eq!(ds.column("delta01").unwrap().source_name(), "Delta01");
        assert_eq!(
            ds.column("delta01").unwrap().grain(),
            Some(Grain::Underlying)
        );
        assert!(ds.column("book").unwrap().textual);
        assert!(
            ds.column("delta01").unwrap().required,
            "required defaults true"
        );
        assert!(!ds.column("cross_gamma02").unwrap().required);
    }

    #[test]
    fn grains_are_the_distinct_measure_grains_coarse_first() {
        let (schema, _) = SchemaSpec::from_doc(&doc(SAMPLE));
        let ds = schema.dataset("risk_snapshot").unwrap();
        assert_eq!(
            ds.grains(),
            vec![Grain::Position, Grain::Underlying, Grain::UnderlyingPair]
        );
        let at_underlying: Vec<_> = ds
            .measures_at(Grain::Underlying)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(at_underlying, vec!["delta01"]);
    }

    #[test]
    fn a_reserved_column_name_is_a_diagnostic() {
        let (_schema, diags) = SchemaSpec::from_doc(&doc(
            "[risk.columns.batch]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        ));
        assert!(
            diags.iter().any(|d| d.message.contains("reserved")),
            "{diags:?}"
        );
    }

    #[test]
    fn an_undeclared_grain_key_column_is_a_diagnostic() {
        // `counterparty` is part of every grain key but is not declared, so
        // the generated GROUP BY would reference a column that does not
        // exist and fail with a raw engine error at load time.
        let (_schema, diags) = SchemaSpec::from_doc(&doc(
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
        ));
        assert!(
            diags.iter().any(|d| d.message.contains("counterparty")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_grain_carrying_only_attributes_still_gets_a_table() {
        let (schema, _) = SchemaSpec::from_doc(&doc(
            "[risk.columns.strike]\ntype = \"f64\"\nrole = \"attribute\"\ngrain = \"instrument\"\n",
        ));
        assert_eq!(
            schema.dataset("risk").unwrap().grains(),
            vec![Grain::Instrument],
            "attribute-only grains must not be silently dropped at ingest"
        );
    }

    #[test]
    fn bad_grain_is_a_diagnostic_not_a_panic() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(
            "[risk.columns.x]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"galaxy\"\n",
        ));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("galaxy"), "{}", diags[0].message);
        assert!(schema.dataset("risk").unwrap().column("x").is_none());
    }
}
