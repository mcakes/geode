//! Schema inspection and dataset-level presentation editing.
//!
//! Dataset fields and derived-dimension descriptions are read-only. Enter or a click on
//! a declared column opens its seven presentation fields and writes
//! `dataset_presentation.toml`; it never edits `datasets.toml`. Derived rows do not
//! open that stage. All write gates use the current stage so the inspector remains
//! read-only outside this presentation editor.

use super::{Destination, Draft, Field, FieldKind, dataset_columns};
use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::{Aggregate, ColumnRole, ColumnSpec, ColumnType, SchemaSpec};

pub const DOC: &str = "datasets";

/// `<n> columns · <m> measures · <k> dimensions`, counted off the raw
/// table rather than the parsed spec, so a dataset the reader drops a
/// column from still counts what the file says.
pub fn summary(value: &toml::Value) -> String {
    let Some(columns) = value.get("columns").and_then(|v| v.as_table()) else {
        return "no [columns] table".to_string();
    };
    let role = |name: &str| {
        columns
            .values()
            .filter(|c| c.get("role").and_then(|r| r.as_str()) == Some(name))
            .count()
    };
    let (n, m, k) = (columns.len(), role("measure"), role("dimension"));
    format!(
        "{n} column{} · {m} measure{} · {k} dimension{}",
        plural(n),
        plural(m),
        plural(k)
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn type_name(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Utf8 => "utf8",
        ColumnType::F64 => "f64",
        ColumnType::I64 => "i64",
        ColumnType::Date => "date",
        ColumnType::Timestamp => "timestamp",
        ColumnType::Bool => "bool",
    }
}

/// The same spelling `parse_column` reads back off `[[columns]] aggregate
/// = "..."` (`geode-core/src/schema/column.rs`'s `Aggregate::parse`), so
/// a trader reading this row sees the exact word they would type to
/// change it.
fn aggregate_name(aggregate: Aggregate) -> &'static str {
    match aggregate {
        Aggregate::Sum => "sum",
        Aggregate::Min => "min",
        Aggregate::Max => "max",
        Aggregate::Any => "any",
    }
}

/// Summarize type, role, applicable grain, and required/textual/categorical flags.
/// Measures include their aggregate in the role description. The ingest-only source
/// rename is omitted because this inspector cannot edit it.
pub fn describe_column(column: &ColumnSpec) -> String {
    let mut out = format!("{} · ", type_name(column.ty));
    match &column.role {
        ColumnRole::Key => out.push_str("key"),
        ColumnRole::Dimension { grain: None } => out.push_str("dimension"),
        ColumnRole::Dimension { grain: Some(g) } => {
            out.push_str(&format!("dimension · carried by {}", g.short()))
        }
        ColumnRole::Measure { grain, aggregate } => out.push_str(&format!(
            "measure ({}) · grain {}",
            aggregate_name(*aggregate),
            grain.short()
        )),
        ColumnRole::Attribute { grain: Some(g) } => {
            out.push_str(&format!("attribute · grain {}", g.short()))
        }
        // The document family's vocabulary: a grainless attribute is document-level,
        // and an axis or a value has no grain to name at all — a document dataset
        // declares none.
        ColumnRole::Attribute { grain: None } => out.push_str("attribute"),
        ColumnRole::Axis => out.push_str("axis"),
        ColumnRole::Value => out.push_str("value"),
    }
    if column.required {
        out.push_str(" · required");
    }
    if column.textual {
        out.push_str(" · textual");
    }
    if column.categorical {
        out.push_str(" · categorical");
    }
    out
}

/// Read-only fields in schema column order, followed by derived dimensions whose source
/// belongs to this dataset. Dataset rows use the dataset's provenance; derived rows use
/// their dimension definition's provenance. Keys distinguish `columns.<name>` and
/// `derived.<name>` for diagnostics and stage entry.
///
/// Read the dataset overlay once and append each column's presentation summary.
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let Some(name) = object else {
        return Vec::new();
    };
    let schema = config
        .doc(DOC)
        .map(|doc| SchemaSpec::from_doc(doc).0)
        .unwrap_or_default();
    let Some(dataset) = schema.dataset(name) else {
        return Vec::new();
    };
    let dataset_layer = config.explain(DOC, name);
    let overlay = dataset_columns::overlay_object(config, name);
    let mut out: Vec<Field> = dataset
        .columns
        .iter()
        .map(|column| {
            let suffix = dataset_columns::item_for(dataset, &column.name, &overlay)
                .map(|item| dataset_columns::row_summary(&item))
                .unwrap_or_default();
            Field {
                key: format!("columns.{}", column.name),
                label: column.name.clone(),
                kind: FieldKind::Text(format!("{}{suffix}", describe_column(column))),
                dest: Destination::Doc,
                layer: dataset_layer,
            }
        })
        .collect();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    for dim in dims.all().filter(|d| dataset.column(&d.from).is_some()) {
        out.push(Field {
            key: format!("derived.{}", dim.name),
            label: format!("{} (derived)", dim.name),
            kind: FieldKind::Text(format!(
                "from {} · {} value{}",
                dim.from,
                dim.values.len(),
                plural(dim.values.len())
            )),
            dest: Destination::Doc,
            layer: config.explain("dimensions", &dim.name),
        });
    }
    out
}

/// What this domain writes, by destination — and the branch is the whole safety
/// property.
///
/// [`Destination::DatasetPresentation`] is the column stage's own, and it
/// renders the dataset's `[<ds>]` table of `dataset_presentation.toml`.
/// The other two are unreachable behind `Domain::writable(stage)`, which
/// answers `false` everywhere but the column stage, and they render the
/// source unchanged so the exhaustive [`Domain::to_table`] has an honest
/// arm.
///
/// Ignoring `dest` here — the shape this had before the column stage
/// existed — would have `edits_for` render the whole `datasets` object
/// and write it into `dataset_presentation.toml` under the dataset's
/// name: a personalisation file holding a copy of the schema, and the
/// column's own keys nowhere.
///
/// [`Domain::to_table`]: super::Domain::to_table
pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item {
    match dest {
        Destination::DatasetPresentation => toml_edit::Item::Table(dataset_columns::table(draft)),
        Destination::Doc | Destination::Presentation => {
            toml_edit::Item::Table(super::toml_table_to_edit(&draft.source))
        }
    }
}

/// The dataset's own reader diagnostics, over this dataset alone (the
/// same one-object document `views::validate` builds, for the same
/// reason: the whole doc would report every other dataset's problems
/// against this one).
pub fn validate(draft: &Draft, _config: &Config) -> Vec<Diagnostic> {
    let mut table = toml::Table::new();
    table.insert(draft.name.clone(), toml::Value::Table(draft.source.clone()));
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table,
        }],
    );
    SchemaSpec::from_doc(&doc).1
}

/// What each row means, for the edit footer's help line
/// ([`Domain::help`](super::Domain::help)). The keys are per column
/// (`columns.<name>`, `derived.<name>`), so this matches on the prefix;
/// the row's own text already spells type, role and grain, so the
/// sentence says only what the row does not.
pub fn help(key: &str) -> &'static str {
    if key.starts_with("columns.") {
        "As datasets.toml declares it — open the column to set how it paints"
    } else if key.starts_with("derived.") {
        "A derived dimension from dimensions.toml — groupable like any other"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::super::Domain;
    use super::*;
    use geode_core::config::{Config, ConfigSources, Layer, LayerDoc};

    // The fixture includes a dimension and a measure with an aggregate so schema
    // summaries exercise both roles.
    const DATASETS: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                            [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                            [risk.columns.note]\ntype = \"utf8\"\nrole = \"attribute\"\ngrain = \"position\"\nrequired = false\n\
                            [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\naggregate = \"sum\"\n";

    fn config() -> Config {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("datasets", DATASETS).unwrap(),
                // `other` derives from a column no dataset has: it must
                // NOT appear under `risk` (the filter in `fields`).
                LayerDoc::builtin(
                    "dimensions",
                    "[region]\nfrom = \"book\"\n[region.values]\nEU = [\"BK1\"]\n\
                     [other]\nfrom = \"nothing\"\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        })
    }

    #[test]
    fn summary_counts_columns_by_role() {
        let value = config()
            .doc("datasets")
            .unwrap()
            .value
            .get("risk")
            .cloned()
            .unwrap();
        assert_eq!(summary(&value), "4 columns · 1 measure · 1 dimension");
    }

    // Preserved TOML order determines schema column order; alphabetical order would
    // change the sequence the inspector is meant to show.
    #[test]
    fn fields_are_one_read_only_text_per_column_then_the_derived_dimensions() {
        let fields = fields(&config(), Some("risk"));
        let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "columns.book",
                "columns.position_ref",
                "columns.note",
                "columns.npv",
                "derived.region"
            ]
        );
        assert!(
            matches!(&fields[0].kind, FieldKind::Text(t) if t == "utf8 · dimension · required · categorical")
        );
        assert!(
            matches!(&fields[2].kind, FieldKind::Text(t) if t == "utf8 · attribute · grain position")
        );
        // Measure summaries include their aggregate.
        assert!(
            matches!(&fields[3].kind, FieldKind::Text(t) if t == "f64 · measure (sum) · grain instrument · required")
        );
        assert!(
            !fields.iter().any(|f| f.key == "derived.other"),
            "not this dataset's"
        );
        assert!(matches!(&fields[4].kind, FieldKind::Text(t) if t == "from book · 1 value"));
        assert_eq!(fields[4].label, "region (derived)");
    }

    /// a column the dataset overlay personalises says so on its own row, after the
    /// type/role text every column carries. `npv` alone is personalised here, so the
    /// assertion that `book`'s row is unchanged is what keeps this from passing on a
    /// suffix appended to every row.
    #[test]
    fn a_personalised_column_row_carries_its_summary() {
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("datasets", DATASETS).unwrap(),
                LayerDoc::builtin(
                    "dataset_presentation",
                    "[risk.columns.npv]\nscale = \"k\"\n",
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        });
        let fields = fields(&config, Some("risk"));
        let text = |key: &str| {
            fields
                .iter()
                .find(|f| f.key == key)
                .map(|f| match &f.kind {
                    FieldKind::Text(t) => t.clone(),
                    _ => String::new(),
                })
                .unwrap()
        };
        assert_eq!(
            text("columns.npv"),
            "f64 · measure (sum) · grain instrument · required · k"
        );
        assert_eq!(
            text("columns.book"),
            "utf8 · dimension · required · categorical",
            "a column the overlay says nothing about carries no suffix"
        );
    }

    #[test]
    fn every_row_carries_the_layer_that_defined_it() {
        let fields = fields(&config(), Some("risk"));
        assert!(
            fields.iter().all(|f| f.layer == Some(Layer::Builtin)),
            "{fields:?}"
        );
    }

    #[test]
    fn schema_is_the_one_domain_that_is_not_writable() {
        // Outside the column stage, which is Schema's one writable surface — the browse
        // stage is what this inspector's own rows live in.
        let stage = super::super::Stage::Browse;
        assert!(!Domain::Schema.writable(&stage));
        assert!(Domain::Views.writable(&stage));
        assert!(Domain::Groupings.writable(&stage));
        assert!(Domain::Scopes.writable(&stage));
    }

    #[test]
    fn validate_reports_the_datasets_own_reader_diagnostics() {
        let config = config();
        // A grain in use whose key column is undeclared is the reader's
        // own complaint, and this dataset's alone.
        let draft = Domain::Schema.draft(&config, "risk");
        let diags = validate(&draft, &config);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("requires key column")),
            "{diags:?}"
        );
    }
}
