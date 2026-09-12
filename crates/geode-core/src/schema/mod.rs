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

    /// Whether `grain` carries `column` as a dimension (spec §3.3): one
    /// of the grain's dimension keys, or a carried dimension whose
    /// declaring grain's key is contained in this grain's dimension key
    /// — which is how "every finer grain" is defined, and why the pair
    /// grain (dimension key = the instrument key) carries an
    /// instrument-grain dimension while the position grain does not.
    pub fn carries(&self, grain: Grain, column: &str) -> bool {
        if grain.dimension_key_columns().contains(&column) {
            return true;
        }
        self.column(column)
            .and_then(|c| c.carried_grain())
            .is_some_and(|declared| {
                declared
                    .key_columns()
                    .iter()
                    .all(|k| grain.dimension_key_columns().contains(k))
            })
    }

    /// The columns a view may group or scope by at `grain`: the dimension
    /// keys first, then every carried dimension, in schema order.
    pub fn dimensions_at(&self, grain: Grain) -> Vec<&str> {
        let mut out: Vec<&str> = grain.dimension_key_columns().to_vec();
        out.extend(
            self.carried_dimensions_at(grain)
                .into_iter()
                .map(|c| c.name.as_str()),
        );
        out
    }

    /// Carried dimensions this grain's table stores as payload columns.
    pub fn carried_dimensions_at(&self, grain: Grain) -> Vec<&ColumnSpec> {
        self.columns
            .iter()
            .filter(|c| c.carried_grain().is_some() && self.carries(grain, &c.name))
            .collect()
    }

    /// Columns interned as ENUMs at ingest, pickable, and matched by
    /// dictionary in the text filter (spec §3.3).
    pub fn categorical_columns(&self) -> Vec<&str> {
        self.columns
            .iter()
            .filter(|c| c.categorical)
            .map(|c| c.name.as_str())
            .collect()
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
                    Ok((spec, warning)) => {
                        dataset.columns.push(spec);
                        diags.extend(warning);
                    }
                    Err(d) => diags.push(d),
                }
            }
            diags.extend(validate_dataset(&mut dataset));
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
/// Returns the diagnostics; a bare dimension outside every built-in key is
/// also dropped from `ds.columns` in the process, so it cannot reach the
/// query path referencing a table that does not exist.
fn validate_dataset(ds: &mut DatasetSpec) -> Vec<Diagnostic> {
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

    // A bare dimension must be a column of some built-in grain key;
    // otherwise no table would carry it (ddl.rs) and every reference to
    // it would fail inside the query path. Error, and drop the column so
    // a view naming it gets the view validator's "unknown column".
    let bare_outside_key: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| c.role == ColumnRole::Dimension { grain: None })
        .filter(|c| {
            !Grain::ALL
                .iter()
                .any(|g| g.key_columns().contains(&c.name.as_str()))
        })
        .map(|c| c.name.clone())
        .collect();
    for name in &bare_outside_key {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!(
                "dataset '{}' column '{name}': a dimension must be a built-in key column \
                 ({}) or declare the grain that carries it (`grain = \"instrument\"`); dropped",
                ds.name,
                Grain::UnderlyingPair.key_columns().join(", ")
            ),
            path: None,
        });
    }
    ds.columns.retain(|c| !bare_outside_key.contains(&c.name));

    // A carried dimension must actually be carriable: its declaring
    // grain's key must be contained in *some* grain's dimension key
    // (`DatasetSpec::carries`'s own test), or no grain ever carries it —
    // `carried_dimensions_at` never returns it, so `ddl.rs` creates no
    // column for it and `payload_columns` never selects it: declared in
    // the schema, silently absent from every table. The pair grain is
    // the one built-in grain this can happen for: its *dimension* key
    // collapses to the instrument key (`Grain::dimension_key_columns`'s
    // doc comment), so `grain = "underlying_pair"` names a key no
    // grain's dimension key — not even the pair grain's own — ever
    // contains.
    let uncarriable: Vec<(String, Grain)> = ds
        .columns
        .iter()
        .filter_map(|c| match c.role {
            ColumnRole::Dimension { grain: Some(g) } => Some((c.name.clone(), g)),
            _ => None,
        })
        .filter(|(_, g)| {
            !Grain::ALL.iter().any(|grain| {
                g.key_columns()
                    .iter()
                    .all(|k| grain.dimension_key_columns().contains(k))
            })
        })
        .collect();
    for (name, g) in &uncarriable {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!(
                "dataset '{}' column '{name}': declares grain = {g:?}, but no grain's \
                 dimension key contains {g:?}'s key, so nothing can ever carry it as a \
                 dimension; dropped",
                ds.name
            ),
            path: None,
        });
    }
    ds.columns
        .retain(|c| !uncarriable.iter().any(|(name, _)| name == &c.name));

    // `textual` needs a grain that can evaluate the column: a dimension
    // some grain carries, or a measure/attribute declared at a grain.
    // Found by measurement (spec §7): one unroutable textual column
    // fails every text-filtered query on the dataset.
    let routable =
        |c: &ColumnSpec| c.grain().is_some() || Grain::ALL.iter().any(|g| ds.carries(*g, &c.name));
    let unroutable: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| c.textual && !routable(c))
        .map(|c| c.name.clone())
        .collect();
    for name in &unroutable {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!(
                "dataset '{}' column '{name}': textual = true, but no grain carries it as a \
                 dimension, so the text filter cannot route it; textual ignored",
                ds.name
            ),
            path: None,
        });
    }
    for c in &mut ds.columns {
        if unroutable.contains(&c.name) {
            c.textual = false;
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
        path: None,
    }
}

/// `Ok`'s `Option<Diagnostic>` is a non-fatal warning attached to a column
/// that is still kept — `categorical = true` on a non-string type, for
/// one, where dropping the column would silently remove a real attribute
/// over a flag typo (the wrong severity for that mistake).
fn parse_column(
    ds: &str,
    name: &str,
    value: &toml::Value,
) -> Result<(ColumnSpec, Option<Diagnostic>), Diagnostic> {
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
        "dimension" => ColumnRole::Dimension {
            grain: match table.get("grain").and_then(|v| v.as_str()) {
                None => None,
                Some(g) => {
                    Some(Grain::parse(g).ok_or_else(|| bad(format!("unknown grain '{g}'")))?)
                }
            },
        },
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

    // Only a string column can be an ENUM, so the dimension default is
    // gated on the type too: a numeric dimension (a strike a desk groups
    // by) is silently uncategorical, where an explicit `categorical =
    // true` on the same column is an opt-in worth a diagnostic.
    let categorical_default =
        matches!(role, ColumnRole::Dimension { .. }) && ty == ColumnType::Utf8;
    let mut warning = None;
    let categorical = match table.get("categorical").and_then(|v| v.as_bool()) {
        None => categorical_default,
        Some(true) if ty != ColumnType::Utf8 => {
            warning = Some(bad(format!(
                "categorical = true needs type = \"utf8\" (got '{ty_str}'); \
                 only a string column can be an ENUM — categorical ignored"
            )));
            false
        }
        Some(v) => v,
    };

    Ok((
        ColumnSpec {
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
            categorical,
            role,
        },
        warning,
    ))
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

    const CARRIED: &str = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk.columns.expiry]
type = "utf8"
role = "attribute"
grain = "instrument"
categorical = true
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;

    #[test]
    fn a_dimension_may_declare_the_grain_that_carries_it() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(CARRIED));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk").unwrap();
        let currency = ds.column("currency").unwrap();
        assert_eq!(
            currency.role,
            ColumnRole::Dimension {
                grain: Some(Grain::Instrument)
            }
        );
        assert_eq!(currency.carried_grain(), Some(Grain::Instrument));
        assert_eq!(
            currency.grain(),
            None,
            "a carried dimension is not a payload-by-grain column"
        );
        assert_eq!(ds.column("book").unwrap().carried_grain(), None);
    }

    #[test]
    fn a_carried_dimension_is_carried_by_its_grain_and_every_finer_one() {
        let (schema, _) = SchemaSpec::from_doc(&doc(CARRIED));
        let ds = schema.dataset("risk").unwrap();
        assert!(
            !ds.carries(Grain::Position, "currency"),
            "a position spans instruments"
        );
        assert!(ds.carries(Grain::Instrument, "currency"));
        assert!(ds.carries(Grain::Underlying, "currency"));
        assert!(ds.carries(Grain::UnderlyingPair, "currency"));
        // Key dimensions are carried exactly where they were before.
        assert!(ds.carries(Grain::Position, "book"));
        assert!(!ds.carries(Grain::Position, "underlying_ref"));
        assert!(ds.carries(Grain::Underlying, "underlying_ref"));
        // dimensions_at is keys then carried, schema order.
        assert_eq!(
            ds.dimensions_at(Grain::Instrument),
            vec![
                "book",
                "lhu",
                "position_ref",
                "counterparty",
                "instrument_ref",
                "currency"
            ]
        );
        assert_eq!(
            ds.carried_dimensions_at(Grain::Underlying)
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["currency"]
        );
        assert!(ds.carried_dimensions_at(Grain::Position).is_empty());
    }

    #[test]
    fn categorical_defaults_true_for_dimensions_and_false_otherwise_and_attributes_may_opt_in() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(CARRIED));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk").unwrap();
        assert!(ds.column("book").unwrap().categorical);
        assert!(ds.column("currency").unwrap().categorical);
        assert!(
            !ds.column("position_ref").unwrap().categorical,
            "keys are never categorical by default"
        );
        assert!(!ds.column("npv").unwrap().categorical);
        assert!(
            ds.column("expiry").unwrap().categorical,
            "an attribute opted in"
        );
        assert_eq!(
            ds.categorical_columns(),
            vec![
                "book",
                "lhu",
                "counterparty",
                "underlying_ref",
                "currency",
                "expiry"
            ]
        );
    }

    #[test]
    fn a_dimension_may_opt_out_of_categorical() {
        let text = format!(
            "{CARRIED}\n[risk.columns.trade_ref]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"position\"\ncategorical = false\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk").unwrap();
        assert!(!ds.column("trade_ref").unwrap().categorical);
        assert!(
            ds.carries(Grain::Position, "trade_ref"),
            "still a dimension"
        );
    }

    /// A numeric dimension (a strike a desk groups by) defaults to NOT
    /// categorical, silently: only a string column can be an ENUM, and a
    /// default is not an opt-in, so there is nothing to warn about.
    #[test]
    fn a_non_string_dimension_defaults_to_uncategorical_without_a_diagnostic() {
        let text = format!(
            "{CARRIED}\n[risk.columns.strike]\ntype = \"f64\"\nrole = \"dimension\"\ngrain = \"instrument\"\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk").unwrap();
        assert!(!ds.column("strike").unwrap().categorical);
        assert!(
            ds.carries(Grain::Instrument, "strike"),
            "still a carried dimension"
        );
        assert!(!ds.categorical_columns().contains(&"strike"));
    }

    #[test]
    fn categorical_on_a_non_string_column_is_a_diagnostic_and_the_column_is_kept_uncategorical() {
        let text = format!(
            "{CARRIED}\n[risk.columns.strike]\ntype = \"f64\"\nrole = \"attribute\"\ngrain = \"instrument\"\ncategorical = true\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("strike") && d.message.contains("categorical")),
            "{diags:?}"
        );
        let ds = schema.dataset("risk").unwrap();
        assert!(!ds.column("strike").unwrap().categorical);
    }

    #[test]
    fn a_bare_dimension_outside_every_built_in_key_is_an_error_and_is_dropped() {
        let text =
            format!("{CARRIED}\n[risk.columns.desk]\ntype = \"utf8\"\nrole = \"dimension\"\n");
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let d = diags
            .iter()
            .find(|d| d.message.contains("desk"))
            .unwrap_or_else(|| panic!("{diags:?}"));
        assert_eq!(d.severity, Severity::Error);
        assert!(
            d.message.contains("grain ="),
            "the fix is named: {}",
            d.message
        );
        assert!(schema.dataset("risk").unwrap().column("desk").is_none());
    }

    #[test]
    fn a_dimension_carried_by_the_pair_grain_is_uncarriable_and_is_dropped() {
        // The pair grain's own *dimension* key collapses to the
        // instrument key (`dimension_key_columns`'s doc comment), so
        // `grain = "underlying_pair"` names a key no grain's dimension
        // key ever contains — not even the pair grain's own. Nothing
        // could ever carry this column: `carried_dimensions_at` would
        // never return it, so no table would ever have the column.
        let text = format!(
            "{CARRIED}\n[risk.columns.spread_type]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"underlying_pair\"\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let d = diags
            .iter()
            .find(|d| d.message.contains("spread_type"))
            .unwrap_or_else(|| panic!("{diags:?}"));
        assert_eq!(d.severity, Severity::Error);
        let ds = schema.dataset("risk").unwrap();
        assert!(
            ds.column("spread_type").is_none(),
            "dropped, not merely warned about"
        );
        for grain in Grain::ALL {
            assert!(!ds.carries(grain, "spread_type"));
        }
    }

    #[test]
    fn textual_on_a_column_no_grain_can_route_is_an_error_and_textual_is_cleared() {
        // underlying2_ref is in the pair grain's raw key but not its
        // dimension key (it is canonicalised), so nothing can route it.
        let text = format!(
            "{CARRIED}\n[risk.columns.underlying2_ref]\ntype = \"utf8\"\nrole = \"dimension\"\ntextual = true\n[risk.columns.cross_gamma02]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying_pair\"\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let d = diags
            .iter()
            .find(|d| d.message.contains("underlying2_ref") && d.message.contains("textual"))
            .unwrap_or_else(|| panic!("{diags:?}"));
        assert_eq!(d.severity, Severity::Error);
        let ds = schema.dataset("risk").unwrap();
        assert!(!ds.column("underlying2_ref").unwrap().textual);
        assert!(
            ds.column("underlying2_ref").is_some(),
            "the key column itself stays"
        );
    }
}
