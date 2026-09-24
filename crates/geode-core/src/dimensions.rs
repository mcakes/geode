//! Derived dimensions map source values to configured labels. For example,
//! `desk` can be derived from the `book` column without being present in a
//! source file. A many-to-one map declares a functional dependency used by
//! attribution: a book determines its desk.
//!
//! Parsing enforces conflicting source mappings, but does not check `from`
//! against a dataset. Consumers validate that the source column exists.

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

    /// Resolve one derived dimension to its configured source column, or
    /// return the name unchanged. This is a single lookup, not recursive
    /// resolution; attribution compares the resulting base columns.
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
            // Point diagnostics at the dimension object, its `from` field, or the
            // specific mapped value whose source array is malformed.
            let bad = |suffix: &str, m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("dimension '{name}': {m}"),
                path: Some(if suffix.is_empty() {
                    format!("dimensions.{name}")
                } else {
                    format!("dimensions.{name}.{suffix}")
                }),
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("", "not a table".into()));
                continue;
            };
            let Some(from) = table.get("from").and_then(|v| v.as_str()) else {
                diags.push(bad("from", "missing 'from'".into()));
                continue;
            };

            let mut values: BTreeMap<String, String> = BTreeMap::new();
            if let Some(map) = table.get("values").and_then(|v| v.as_table()) {
                for (derived_value, sources) in map {
                    let Some(list) = sources.as_array() else {
                        diags.push(bad(
                            &format!("values.{derived_value}"),
                            format!("'{derived_value}' is not an array"),
                        ));
                        continue;
                    };
                    for source in list.iter().filter_map(|s| s.as_str()) {
                        if let Some(existing) = values.get(source)
                            && existing != derived_value
                        {
                            // Many-to-many would break the functional
                            // dependency the additivity rule rests on.
                            diags.push(bad(
                                &format!("values.{derived_value}"),
                                format!(
                                    "'{source}' is mapped to both '{existing}' and \
                                     '{derived_value}'; the map must be many-to-one"
                                ),
                            ));
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
    fn a_missing_from_diagnostic_carries_its_field_path() {
        let (_, diags) =
            DerivedDimensions::from_doc(&doc("[region]\n[region.values]\nA = [\"BK000\"]\n"));
        assert_eq!(
            diags[0].path.as_deref(),
            Some("dimensions.region.from"),
            "{diags:?}"
        );
    }

    #[test]
    fn a_dimension_config_version_header_is_not_a_spurious_diagnostic() {
        // The version header is metadata, not a dimension declaration.
        let (dims, diags) =
            DerivedDimensions::from_doc(&doc(&format!("config_version = 1\n{SAMPLE}")));
        assert!(diags.is_empty(), "{diags:?}");
        assert!(dims.get("desk").is_some());
    }
}
