//! The declared shape of the desk's data (spec §3). Parsed from the
//! `datasets` config doc; every parse failure degrades to a Diagnostic and
//! skips the offending column, never panics (spec §5.7, config §8).

mod column;
mod grain;

pub use column::{Aggregate, ColumnRole, ColumnSpec, ColumnType};
pub use grain::Grain;

use crate::config::{Diagnostic, MergedDoc, Severity};

/// Which of the two dataset families a dataset belongs to (market-data
/// spec §3; roadmap ruling 7). The measure family is the grain
/// vocabulary as it always was; the document family is keyed by a
/// declared identity plus axes and has no grain at all. The two are
/// side by side rather than one declared-key model because attribution
/// rests on the grains forming a prefix chain, and nothing a document
/// dataset does needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Family {
    #[default]
    Measures,
    Document,
}

impl Family {
    pub fn parse(s: &str) -> Option<Family> {
        match s {
            "measures" => Some(Family::Measures),
            "document" => Some(Family::Document),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DatasetSpec {
    pub name: String,
    pub columns: Vec<ColumnSpec>,
    pub family: Family,
    /// Document family: the identity key, in declared order. One document
    /// per distinct key tuple; the batch a publish replaces (spec §4.1).
    /// Empty for the measure family.
    pub key: Vec<String>,
    /// Document family: the row identity within a document, in declared
    /// order — also the order a document request sorts by (spec §7).
    /// Empty for the measure family.
    pub axes: Vec<String>,
}

impl DatasetSpec {
    pub fn is_document(&self) -> bool {
        self.family == Family::Document
    }

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

    /// Every declared column a view or a grouping slot may group by, in
    /// schema order: the ones some *declared* grain ([`Self::grains`] —
    /// the grains a measure or attribute gives a table to) [`Self::carries`]
    /// as a dimension. That is a grain's key columns and every carried
    /// dimension, categorical or not; never an attribute or a measure. A
    /// dataset declaring no grain has no table to scan and offers nothing.
    /// This is the query compiler's own rule (`carries_all` over
    /// `finest_carrying`), and the one place it is spelled out — the
    /// Groupings dialog and the blotter's `:group` completion both read it.
    pub fn groupable_columns(&self) -> Vec<&str> {
        let grains = self.grains();
        self.columns
            .iter()
            .filter(|c| grains.iter().any(|g| self.carries(*g, &c.name)))
            .map(|c| c.name.as_str())
            .collect()
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
            let family = match ds_value.get("family").and_then(|v| v.as_str()) {
                None => Family::Measures,
                Some(s) => match Family::parse(s) {
                    Some(f) => f,
                    None => {
                        diags.push(Diagnostic {
                            severity: Severity::Error,
                            layer: None,
                            file: None,
                            message: format!(
                                "dataset '{ds_name}': unknown family '{s}'; dataset dropped"
                            ),
                            path: Some(format!("{ds_name}.family")),
                        });
                        continue;
                    }
                },
            };
            let string_list =
                |field: &str, diags: &mut Vec<Diagnostic>| -> Result<Vec<String>, ()> {
                    match ds_value.get(field) {
                        None => Ok(Vec::new()),
                        Some(v) => v
                            .as_array()
                            .and_then(|a| {
                                a.iter()
                                    .map(|x| x.as_str().map(str::to_string))
                                    .collect::<Option<Vec<_>>>()
                            })
                            .ok_or_else(|| {
                                diags.push(Diagnostic {
                                    severity: Severity::Error,
                                    layer: None,
                                    file: None,
                                    message: format!(
                                        "dataset '{ds_name}': '{field}' must be an array of \
                                     column names; dataset dropped"
                                    ),
                                    path: Some(format!("{ds_name}.{field}")),
                                })
                            }),
                    }
                };
            let (Ok(mut key), Ok(mut axes)) = (
                string_list("key", &mut diags),
                string_list("axes", &mut diags),
            ) else {
                continue;
            };
            if family == Family::Measures {
                for (field, list) in [("key", &mut key), ("axes", &mut axes)] {
                    if !list.is_empty() {
                        diags.push(Diagnostic {
                            severity: Severity::Warning,
                            layer: None,
                            file: None,
                            message: format!(
                                "dataset '{ds_name}': '{field}' is ignored on the measure family"
                            ),
                            path: Some(format!("{ds_name}.{field}")),
                        });
                        list.clear();
                    }
                }
            }
            let mut dataset = DatasetSpec {
                name: ds_name.clone(),
                columns: Vec::new(),
                family,
                key,
                axes,
            };
            let Some(cols) = ds_value.get("columns").and_then(|v| v.as_table()) else {
                diags.push(note(format!("dataset '{ds_name}': no [columns] table")));
                out.datasets.push(dataset);
                continue;
            };
            for (col_name, col_value) in cols {
                match parse_column(ds_name, family, col_name, col_value) {
                    Ok((spec, warning)) => {
                        dataset.columns.push(spec);
                        diags.extend(warning);
                    }
                    Err(d) => diags.push(d),
                }
            }
            diags.extend(validate_dataset(&mut dataset));
            if dataset.is_document() && dataset.columns.is_empty() {
                // `validate_document` empties a dataset it refused — never
                // push a document dataset with no columns, it could not
                // be stored or queried.
                continue;
            }
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

    if ds.is_document() {
        return validate_document(ds);
    }

    // Document vocabulary on a measure dataset (market-data spec §3.2):
    // refused per column, never guessed at. The document family has the
    // mirror rule in `validate_document`.
    let foreign: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| {
            matches!(
                c.role,
                ColumnRole::Axis | ColumnRole::Value | ColumnRole::Attribute { grain: None }
            )
        })
        .map(|c| c.name.clone())
        .collect();
    for name in &foreign {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!(
                "dataset '{}' column '{name}': 'axis', 'value' and a grainless 'attribute' belong to the \
                 document family; this is a measure family dataset — column dropped",
                ds.name
            ),
            path: Some(format!("{}.columns.{name}.role", ds.name)),
        });
    }
    ds.columns.retain(|c| !foreign.contains(&c.name));

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

/// The document family's load-time rules (market-data spec §3.2). Every
/// failure is a diagnostic with `path` set. A document dataset has no
/// grain, so none of `validate_dataset`'s grain-key checks apply — this
/// validates the key/axes/value shape instead and returns in place of
/// them.
fn validate_document(ds: &mut DatasetSpec) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let err = |message: String, path: String| Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message,
        path: Some(path),
    };
    let name = ds.name.clone();

    // Measure vocabulary is refused per column, never reinterpreted. The
    // mirror rule lives in `validate_dataset`'s measure-family body.
    let foreign: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| {
            matches!(
                c.role,
                ColumnRole::Key
                    | ColumnRole::Measure { .. }
                    | ColumnRole::Dimension { grain: Some(_) }
                    | ColumnRole::Attribute { grain: Some(_) }
            )
        })
        .map(|c| c.name.clone())
        .collect();
    for c in &foreign {
        diags.push(err(
            format!(
                "dataset '{name}' column '{c}': 'key', 'measure' and 'grain = …' belong to \
                 the measure family; this is a document family dataset — column dropped"
            ),
            format!("{name}.columns.{c}.role"),
        ));
    }
    ds.columns.retain(|c| !foreign.contains(&c.name));

    // A value is a number: it feeds the numeric cell of the document
    // grid, and a non-numeric one would fail every downstream fold.
    let non_numeric: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| {
            c.role == ColumnRole::Value && !matches!(c.ty, ColumnType::F64 | ColumnType::I64)
        })
        .map(|c| c.name.clone())
        .collect();
    for c in &non_numeric {
        diags.push(err(
            format!("dataset '{name}' column '{c}': a value must be f64 or i64 — column dropped"),
            format!("{name}.columns.{c}.type"),
        ));
    }
    ds.columns.retain(|c| !non_numeric.contains(&c.name));

    let mut keep = true;

    if ds.key.is_empty() {
        diags.push(err(
            format!(
                "dataset '{name}': a document dataset needs a non-empty 'key'; dataset dropped"
            ),
            format!("{name}.key"),
        ));
        keep = false;
    }
    if ds.axes.is_empty() {
        diags.push(err(
            format!("dataset '{name}': a document dataset needs non-empty 'axes'; dataset dropped"),
            format!("{name}.axes"),
        ));
        keep = false;
    }
    for k in &ds.key {
        match ds.column(k) {
            None => {
                diags.push(err(
                    format!("dataset '{name}': key names undeclared column '{k}'; dataset dropped"),
                    format!("{name}.key"),
                ));
                keep = false;
            }
            Some(c) if !matches!(c.role, ColumnRole::Dimension { grain: None }) => {
                diags.push(err(
                    format!(
                        "dataset '{name}': key column '{k}' must have role = \"dimension\"; \
                         dataset dropped"
                    ),
                    format!("{name}.key"),
                ));
                keep = false;
            }
            Some(_) => {}
        }
    }
    for a in &ds.axes {
        match ds.column(a) {
            None => {
                diags.push(err(
                    format!(
                        "dataset '{name}': axes names undeclared column '{a}'; dataset dropped"
                    ),
                    format!("{name}.axes"),
                ));
                keep = false;
            }
            Some(c) if c.role != ColumnRole::Axis => {
                diags.push(err(
                    format!(
                        "dataset '{name}': axis '{a}' must have role = \"axis\"; dataset dropped"
                    ),
                    format!("{name}.axes"),
                ));
                keep = false;
            }
            Some(_) => {}
        }
    }
    for c in ds
        .columns
        .iter()
        .filter(|c| c.role == ColumnRole::Axis && !ds.axes.contains(&c.name))
    {
        diags.push(err(
            format!(
                "dataset '{name}' column '{}': role = \"axis\" but not listed in axes; \
                 dataset dropped",
                c.name
            ),
            format!("{name}.columns.{}.role", c.name),
        ));
        keep = false;
    }
    if !ds.columns.iter().any(|c| c.role == ColumnRole::Value) {
        diags.push(err(
            format!("dataset '{name}': a document dataset declares at least one value column; dataset dropped"),
            format!("{name}.columns"),
        ));
        keep = false;
    }

    // `textual` is routable through any dimension of a document dataset
    // (there is no grain to route it): only a textual axis/value/attribute
    // is refused, and it is cleared, not dropped, as on the measure side.
    let unroutable: Vec<String> = ds
        .columns
        .iter()
        .filter(|c| c.textual && !matches!(c.role, ColumnRole::Dimension { .. }))
        .map(|c| c.name.clone())
        .collect();
    for c in &unroutable {
        diags.push(err(
            format!(
                "dataset '{name}' column '{c}': textual = true on a non-dimension of a \
                 document dataset; textual ignored"
            ),
            format!("{name}.columns.{c}.textual"),
        ));
    }
    for c in &mut ds.columns {
        if unroutable.contains(&c.name) {
            c.textual = false;
        }
    }

    if !keep {
        ds.columns.clear();
        ds.key.clear();
        ds.axes.clear();
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
    family: Family,
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
            grain: match family {
                Family::Measures => Some(grain_of(table)?),
                Family::Document => None,
            },
        },
        "axis" => ColumnRole::Axis,
        "value" => ColumnRole::Value,
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

    /// The grouping vocabulary is "a declared column some declared grain
    /// carries": keys and carried dimensions, categorical or not, in
    /// schema order; a categorical attribute (`expiry`) and a measure are
    /// not offered, since no grain carries either as a dimension.
    #[test]
    fn groupable_columns_are_the_declared_columns_some_declared_grain_carries() {
        let text = format!(
            "{CARRIED}\n[risk.columns.strike]\ntype = \"f64\"\nrole = \"dimension\"\ngrain = \"instrument\"\n"
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("risk").unwrap();
        assert_eq!(
            ds.groupable_columns(),
            vec![
                "book",
                "lhu",
                "position_ref",
                "counterparty",
                "instrument_ref",
                "underlying_ref",
                "currency",
                "strike"
            ]
        );
    }

    /// No declared grain means no table the compiler could scan
    /// (`finest_carrying` is over declared grains), so nothing is
    /// groupable — even though `book` is in every grain's dimension key.
    #[test]
    fn a_dataset_declaring_no_grain_has_no_groupable_columns() {
        let (schema, _) = SchemaSpec::from_doc(&doc(
            "[bare.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        ));
        let ds = schema.dataset("bare").unwrap();
        assert!(ds.grains().is_empty());
        assert!(ds.groupable_columns().is_empty());
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

    const CVI: &str = r#"
[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]

[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true

[cvi_params.columns.term]
type = "date"
role = "axis"

[cvi_params.columns.node]
type = "f64"
role = "axis"

[cvi_params.columns.param]
type = "f64"
role = "value"

[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"

[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;

    #[test]
    fn a_document_dataset_parses_its_family_key_and_axes() {
        let (schema, diags) = SchemaSpec::from_doc(&doc(CVI));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("cvi_params").unwrap();
        assert_eq!(ds.family, Family::Document);
        assert!(ds.is_document());
        assert_eq!(ds.key, vec!["underlying_ref".to_string()]);
        assert_eq!(ds.axes, vec!["term".to_string(), "node".to_string()]);
        assert_eq!(ds.column("term").unwrap().role, ColumnRole::Axis);
        assert_eq!(ds.column("param").unwrap().role, ColumnRole::Value);
        // A document-level attribute carries no grain.
        assert_eq!(
            ds.column("spot_ref").unwrap().role,
            ColumnRole::Attribute { grain: None }
        );
    }

    #[test]
    fn a_dataset_without_a_family_is_the_measure_family() {
        let (schema, _) = SchemaSpec::from_doc(&doc(SAMPLE));
        let ds = schema.dataset("risk_snapshot").unwrap();
        assert_eq!(ds.family, Family::Measures);
        assert!(!ds.is_document());
        assert!(ds.key.is_empty() && ds.axes.is_empty());
    }

    #[test]
    fn an_unknown_family_is_an_error_and_the_dataset_is_dropped() {
        let text = CVI.replace("family = \"document\"", "family = \"widget\"");
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(schema.dataset("cvi_params").is_none());
        let d = diags
            .iter()
            .find(|d| d.message.contains("unknown family 'widget'"))
            .unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.path.as_deref(), Some("cvi_params.family"));
    }

    #[test]
    fn a_measure_family_key_and_axes_are_cleared_with_a_warning() {
        // `[risk_snapshot]` must precede the dotted `risk_snapshot.columns.*`
        // headers SAMPLE opens with — TOML lets a `[table]` header appear
        // before its own subtables, never after.
        let text = format!("[risk_snapshot]\nkey = [\"book\"]\naxes = [\"lhu\"]\n{SAMPLE}");
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let ds = schema.dataset("risk_snapshot").unwrap();
        assert!(
            ds.key.is_empty(),
            "key must be cleared, not merely warned about"
        );
        assert!(ds.axes.is_empty());
        for field in ["key", "axes"] {
            let d = diags
                .iter()
                .find(|d| {
                    d.path.as_deref() == Some(&format!("risk_snapshot.{field}"))
                        && d.message.contains("is ignored on the measure family")
                })
                .unwrap_or_else(|| panic!("no warning for '{field}': {diags:?}"));
            assert_eq!(d.severity, Severity::Warning);
        }
    }

    #[test]
    fn a_measure_attribute_still_requires_its_grain() {
        // `Attribute { grain: None }` is the document reading only; on a
        // measure dataset a grainless attribute is the same missing-grain
        // error it always was.
        let text = SAMPLE.to_string()
            + "\n[risk_snapshot.columns.note]\ntype = \"utf8\"\nrole = \"attribute\"\n";
        let (_, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("column 'note': missing 'grain'")),
            "{diags:?}"
        );
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

    fn cvi_with(from: &str, to: &str) -> (SchemaSpec, Vec<Diagnostic>) {
        let text = CVI.replace(from, to);
        assert_ne!(text, CVI, "the replacement must change the fixture");
        SchemaSpec::from_doc(&doc(&text))
    }

    fn error_with_path<'a>(diags: &'a [Diagnostic], path: &str) -> &'a Diagnostic {
        diags
            .iter()
            .find(|d| d.severity == Severity::Error && d.path.as_deref() == Some(path))
            .unwrap_or_else(|| panic!("no error at path {path}: {diags:?}"))
    }

    #[test]
    fn a_document_dataset_needs_a_non_empty_key_and_axes() {
        let (schema, diags) = cvi_with("key = [\"underlying_ref\"]", "key = []");
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.key");
        let (schema, diags) = cvi_with("axes = [\"term\", \"node\"]", "axes = []");
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.axes");
    }

    #[test]
    fn a_key_or_axis_naming_an_undeclared_column_drops_the_dataset() {
        let (schema, diags) = cvi_with("key = [\"underlying_ref\"]", "key = [\"nonesuch\"]");
        assert!(schema.dataset("cvi_params").is_none());
        assert!(
            error_with_path(&diags, "cvi_params.key")
                .message
                .contains("nonesuch")
        );
        let (schema, diags) = cvi_with(
            "axes = [\"term\", \"node\"]",
            "axes = [\"term\", \"nonesuch\"]",
        );
        assert!(schema.dataset("cvi_params").is_none());
        assert!(
            error_with_path(&diags, "cvi_params.axes")
                .message
                .contains("nonesuch")
        );
    }

    #[test]
    fn every_key_column_must_be_a_dimension() {
        // Make the key column an attribute: no longer scopeable, so no
        // longer a legal identity (spec §3.2).
        let (schema, diags) = cvi_with(
            "[cvi_params.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"",
            "[cvi_params.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"attribute\"",
        );
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.key");
    }

    #[test]
    fn axes_and_axis_roles_must_agree_both_ways() {
        // An axis column not listed in `axes`.
        let (schema, diags) = cvi_with("axes = [\"term\", \"node\"]", "axes = [\"term\"]");
        assert!(schema.dataset("cvi_params").is_none());
        assert!(
            error_with_path(&diags, "cvi_params.columns.node.role")
                .message
                .contains("not listed in axes")
        );
        // A listed axis whose role is not `axis`.
        let (schema, diags) = cvi_with(
            "[cvi_params.columns.node]\ntype = \"f64\"\nrole = \"axis\"",
            "[cvi_params.columns.node]\ntype = \"f64\"\nrole = \"value\"",
        );
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.axes");
    }

    #[test]
    fn a_non_numeric_value_column_is_dropped_and_the_dataset_kept() {
        // Only value column gone → the "at least one value" rule fires
        // next, so add a second numeric value to isolate this rule.
        let text = CVI.replace(
            "[cvi_params.columns.param]\ntype = \"f64\"",
            "[cvi_params.columns.param]\ntype = \"utf8\"",
        ) + "\n[cvi_params.columns.param2]\ntype = \"i64\"\nrole = \"value\"\n";
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let ds = schema.dataset("cvi_params").expect("dataset kept");
        assert!(ds.column("param").is_none(), "the utf8 value is dropped");
        assert!(ds.column("param2").is_some());
        error_with_path(&diags, "cvi_params.columns.param.type");
    }

    #[test]
    fn a_document_dataset_declares_at_least_one_value() {
        let (schema, diags) = cvi_with(
            "[cvi_params.columns.param]\ntype = \"f64\"\nrole = \"value\"",
            "[cvi_params.columns.param]\ntype = \"f64\"\nrole = \"attribute\"",
        );
        assert!(schema.dataset("cvi_params").is_none());
        error_with_path(&diags, "cvi_params.columns");
    }

    #[test]
    fn measure_vocabulary_on_a_document_dataset_is_refused_per_column() {
        let text = CVI.to_string()
            + "\n[cvi_params.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n"
            + "\n[cvi_params.columns.book]\ntype = \"utf8\"\nrole = \"key\"\n"
            + "\n[cvi_params.columns.ccy]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"instrument\"\n";
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let ds = schema
            .dataset("cvi_params")
            .expect("the dataset is kept; the columns are dropped");
        for name in ["npv", "book", "ccy"] {
            assert!(ds.column(name).is_none(), "{name} should be dropped");
            let d = error_with_path(&diags, &format!("cvi_params.columns.{name}.role"));
            assert!(d.message.contains("document family"), "{}", d.message);
        }
        assert!(ds.column("param").is_some(), "the legal columns survive");
    }

    #[test]
    fn document_vocabulary_on_a_measure_dataset_is_the_mirror_error() {
        let text = SAMPLE.to_string()
            + "\n[risk_snapshot.columns.tenor]\ntype = \"f64\"\nrole = \"axis\"\n"
            + "\n[risk_snapshot.columns.cell]\ntype = \"f64\"\nrole = \"value\"\n";
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        let ds = schema.dataset("risk_snapshot").unwrap();
        assert!(ds.column("tenor").is_none() && ds.column("cell").is_none());
        for name in ["tenor", "cell"] {
            let d = error_with_path(&diags, &format!("risk_snapshot.columns.{name}.role"));
            assert!(d.message.contains("measure family"), "{}", d.message);
        }
    }

    #[test]
    fn a_document_key_need_not_be_a_built_in_grain_key_column() {
        // `index_ref` is no grain's key column. On the measure family a
        // bare dimension of that name is dropped; on the document family
        // it is the whole point.
        let text = CVI
            .replace("key = [\"underlying_ref\"]", "key = [\"index_ref\"]")
            .replace(
                "[cvi_params.columns.underlying_ref]",
                "[cvi_params.columns.index_ref]",
            );
        let (schema, diags) = SchemaSpec::from_doc(&doc(&text));
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("cvi_params").unwrap();
        assert!(ds.column("index_ref").is_some());
        assert!(
            ds.column("index_ref").unwrap().textual,
            "textual is routable through the key"
        );
    }
}
