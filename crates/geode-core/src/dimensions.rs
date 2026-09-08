//! Dimensions the desk groups by that are not in the source files
//! (spec §6.8). `desk` is the standing case: the CSVs carry `book`, and
//! which desk a book belongs to is desk knowledge kept in config.
//!
//! The `from` clause does two jobs. It says where the values come from,
//! and it declares a functional dependency — `book` determines `desk` —
//! which is what lets the attribution rule treat a desk-level rollup as
//! additive rather than blanking it (§6.3).

use crate::config::{Diagnostic, MergedDoc, Severity};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedDimension {
    pub name: String,
    /// The column this is computed from. Must be a real dataset column.
    pub from: String,
    /// Source value -> derived value. Many-to-one by construction.
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct DerivedDimensions {
    dims: Vec<DerivedDimension>,
}

impl DerivedDimensions {
    pub fn get(&self, name: &str) -> Option<&DerivedDimension> {
        self.dims.iter().find(|d| d.name == name)
    }

    pub fn all(&self) -> impl Iterator<Item = &DerivedDimension> {
        self.dims.iter()
    }

    /// The column a grouping or scope column ultimately resolves to: a
    /// derived dimension resolves to its source, anything else to itself.
    /// The attribution rule (§6.3) compares base columns, which is how a
    /// desk-level grouping counts as determined by `book`.
    pub fn base_column<'a>(&'a self, column: &'a str) -> &'a str {
        match self.get(column) {
            Some(d) => &d.from,
            None => column,
        }
    }

    pub fn from_doc(doc: &MergedDoc) -> (DerivedDimensions, Vec<Diagnostic>) {
        let mut out = DerivedDimensions::default();
        let mut diags = Vec::new();

        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let bad = |m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("dimension '{name}': {m}"),
                path: None,
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("not a table".into()));
                continue;
            };
            let Some(from) = table.get("from").and_then(|v| v.as_str()) else {
                diags.push(bad("missing 'from'".into()));
                continue;
            };

            let mut values: BTreeMap<String, String> = BTreeMap::new();
            if let Some(map) = table.get("values").and_then(|v| v.as_table()) {
                for (derived_value, sources) in map {
                    let Some(list) = sources.as_array() else {
                        diags.push(bad(format!("'{derived_value}' is not an array")));
                        continue;
                    };
                    for source in list.iter().filter_map(|s| s.as_str()) {
                        if let Some(existing) = values.get(source)
                            && existing != derived_value
                        {
                            // Many-to-many would break the functional
                            // dependency the additivity rule rests on.
                            diags.push(bad(format!(
                                "'{source}' is mapped to both '{existing}' and \
                                 '{derived_value}'; the map must be many-to-one"
                            )));
                            continue;
                        }
                        values.insert(source.to_string(), derived_value.clone());
                    }
                }
            }

            out.dims.push(DerivedDimension {
                name: name.clone(),
                from: from.to_string(),
                values,
            });
        }

        (out, diags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", text).unwrap()],
        )
    }

    const SAMPLE: &str = r#"
[desk]
from = "book"
[desk.values]
IDX_EXO_EU = ["BK000", "BK001"]
IDX_EXO_US = ["BK003"]
"#;

    #[test]
    fn parses_a_many_to_one_map() {
        let (dims, diags) = DerivedDimensions::from_doc(&doc(SAMPLE));
        assert!(diags.is_empty(), "{diags:?}");
        let desk = dims.get("desk").expect("desk");
        assert_eq!(desk.from, "book");
        assert_eq!(
            desk.values.get("BK000").map(String::as_str),
            Some("IDX_EXO_EU")
        );
        assert_eq!(
            desk.values.get("BK003").map(String::as_str),
            Some("IDX_EXO_US")
        );
    }

    #[test]
    fn base_column_resolves_a_derived_dimension_to_its_source() {
        let (dims, _) = DerivedDimensions::from_doc(&doc(SAMPLE));
        // This is the functional dependency the attribution rule needs.
        assert_eq!(dims.base_column("desk"), "book");
        // A column that is not derived resolves to itself.
        assert_eq!(dims.base_column("book"), "book");
        assert_eq!(dims.base_column("lhu"), "lhu");
    }

    #[test]
    fn a_book_mapped_to_two_desks_is_a_diagnostic() {
        // Many-to-one, not many-to-many: otherwise `book` would not
        // determine `desk` and grouping by desk could not be additive.
        let (_dims, diags) = DerivedDimensions::from_doc(&doc(
            "[desk]\nfrom = \"book\"\n[desk.values]\nA = [\"BK000\"]\nB = [\"BK000\"]\n",
        ));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("BK000"), "{}", diags[0].message);
    }

    #[test]
    fn a_missing_from_clause_is_a_diagnostic() {
        let (dims, diags) =
            DerivedDimensions::from_doc(&doc("[desk]\n[desk.values]\nA = [\"BK000\"]\n"));
        assert!(dims.get("desk").is_none());
        assert!(
            diags.iter().any(|d| d.message.contains("from")),
            "{diags:?}"
        );
    }

    #[test]
    fn an_empty_document_yields_no_dimensions() {
        let (dims, diags) = DerivedDimensions::from_doc(&doc(""));
        assert!(dims.all().next().is_none());
        assert!(diags.is_empty());
    }

    #[test]
    fn a_dimension_config_version_header_is_not_a_spurious_diagnostic() {
        // Every config doc carries this header by convention
        // (`groupings.toml`'s own `GroupingSlots::from_doc` already
        // skips it) — it must not be treated as a malformed dimension.
        let (dims, diags) =
            DerivedDimensions::from_doc(&doc(&format!("config_version = 1\n{SAMPLE}")));
        assert!(diags.is_empty(), "{diags:?}");
        assert!(dims.get("desk").is_some());
    }
}
