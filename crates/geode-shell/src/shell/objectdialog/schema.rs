//! The read-only schema inspector (spec §9, §19.4) — `Domain::Schema`.
//!
//! Not an editor: a schema is the desk's contract with the data, a
//! `datasets` change is restart-required, and the edit and its effect
//! would be far apart. The other adapters read this doc to build their
//! `Choice`s and catalogues; this dialog makes that vocabulary
//! inspectable. Every field is a display-only `Text`, every row carries
//! the layer it came from, and `Domain::writable()` answers `false`, so
//! the scaffold refuses every verb with `READ_ONLY_NOTICE` in one place.

use super::{Destination, Draft, Field, FieldKind};
use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::{ColumnRole, ColumnSpec, ColumnType, SchemaSpec};

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

/// One column, one line: type, role, the grain where the role has one,
/// then whichever of `required`/`textual`/`categorical` hold.
pub fn describe_column(column: &ColumnSpec) -> String {
    let mut out = format!("{} · ", type_name(column.ty));
    match &column.role {
        ColumnRole::Key => out.push_str("key"),
        ColumnRole::Dimension { grain: None } => out.push_str("dimension"),
        ColumnRole::Dimension { grain: Some(g) } => {
            out.push_str(&format!("dimension · carried by {}", g.short()))
        }
        ColumnRole::Measure { grain, .. } => {
            out.push_str(&format!("measure · grain {}", grain.short()))
        }
        ColumnRole::Attribute { grain } => {
            out.push_str(&format!("attribute · grain {}", grain.short()))
        }
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

/// One display-only `Text` per column of `object`, in schema (file)
/// order, then one per derived dimension whose `from` is a column of
/// this dataset. `layer` is `Config::explain` on the dataset (atomic at
/// depth 1, so every column of one dataset carries that dataset's
/// layer) and on the `dimensions` doc for a derived row, which can
/// differ. Keys are `columns.<name>` / `derived.<name>` so §19.5's
/// path matching lands a `datasets.<ds>.columns.<name>.type` diagnostic
/// on its column's row.
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
    let mut out: Vec<Field> = dataset
        .columns
        .iter()
        .map(|column| Field {
            key: format!("columns.{}", column.name),
            label: column.name.clone(),
            kind: FieldKind::Text(describe_column(column)),
            dest: Destination::Doc,
            layer: dataset_layer,
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

/// Unreachable behind `Domain::writable()`; the source, unchanged, so
/// the exhaustive `Domain::to_table` has an honest arm.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    toml_edit::Item::Table(super::toml_table_to_edit(&draft.source))
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

#[cfg(test)]
mod tests {
    use super::super::Domain;
    use super::*;
    use geode_core::config::{Config, ConfigSources, Layer, LayerDoc};

    // `required` defaults to true and `categorical` to true for a
    // dimension (`schema::parse_column`), so the expected strings below
    // carry both flags for `book` and neither for `note`.
    const DATASETS: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                            [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                            [risk.columns.note]\ntype = \"utf8\"\nrole = \"attribute\"\ngrain = \"position\"\nrequired = false\n";

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
        assert_eq!(summary(&value), "3 columns · 0 measures · 1 dimension");
    }

    // Column order is FILE order, not alphabetical — the workspace-wide
    // `preserve_order` ruling (CLAUDE.md: "a schema's column order is
    // file order everywhere it is iterated") — so `book`, `position_ref`,
    // `note` here matches `DATASETS`'s own declaration order, and the
    // `note` descriptor this test pins is at index 2, not 1.
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
                "derived.region"
            ]
        );
        assert!(
            matches!(&fields[0].kind, FieldKind::Text(t) if t == "utf8 · dimension · required · categorical")
        );
        assert!(
            matches!(&fields[2].kind, FieldKind::Text(t) if t == "utf8 · attribute · grain position")
        );
        assert!(
            !fields.iter().any(|f| f.key == "derived.other"),
            "not this dataset's"
        );
        assert!(matches!(&fields[3].kind, FieldKind::Text(t) if t == "from book · 1 value"));
        assert_eq!(fields[3].label, "region (derived)");
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
        assert!(!Domain::Schema.writable());
        assert!(Domain::Views.writable());
        assert!(Domain::Groupings.writable());
        assert!(Domain::Scopes.writable());
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
