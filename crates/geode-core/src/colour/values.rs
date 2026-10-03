//! Value colors: a text dimension's value mapped to a named color, so the
//! cell showing `SPX` paints in SPX's color wherever it appears. The
//! document is `value_colors.toml`; a color is a `colors.toml` name.
//! Everything here is pure: the reader, the check against the schema and
//! the color definitions, and the layer arithmetic behind the pick list.

use crate::colour::RESERVED_PREFIX;
use crate::config::{Diagnostic, MergedDoc, Severity, VALUE_COLORS_DOC};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The entry that means "no color": how a higher layer clears a lower
/// layer's color. It reads as an unmapped value.
pub const NO_COLOR: &str = "none";

/// One dimension's colored values. A color name is an `Arc<str>` so a
/// prepared grid cell shares it rather than copying it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DimensionColors {
    by_value: BTreeMap<String, Arc<str>>,
}

impl DimensionColors {
    /// The color name for `value`, matched exactly.
    pub fn get(&self, value: &str) -> Option<&Arc<str>> {
        self.by_value.get(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<str>)> {
        self.by_value.iter().map(|(v, c)| (v.as_str(), c))
    }
}

/// Dimension → value → color name. Holds only colored values: an entry of
/// [`NO_COLOR`] and a refused entry are both absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValueColors {
    by_dimension: BTreeMap<String, DimensionColors>,
}

impl ValueColors {
    /// Read the merged `value_colors` document. Knows nothing of the schema
    /// or the color definitions; `check_value_colors` does.
    pub fn from_doc(doc: &MergedDoc) -> (ValueColors, Vec<Diagnostic>) {
        let mut out = ValueColors::default();
        let mut diags = Vec::new();
        let mut refuse = |path: String, message: String| {
            diags.push(Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message,
                path: Some(path),
            })
        };
        for (dimension, entry) in &doc.value {
            if dimension == "config_version" {
                continue;
            }
            let Some(table) = entry.as_table() else {
                refuse(
                    format!("{VALUE_COLORS_DOC}.{dimension}"),
                    format!(
                        "value colors '{dimension}': not a table of value = \"color\" entries; dropped"
                    ),
                );
                continue;
            };
            for (value, color) in table {
                let path = format!("{VALUE_COLORS_DOC}.{dimension}.{value}");
                let Some(name) = color.as_str() else {
                    refuse(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}' must name a color (got {color}); dropped"
                        ),
                    );
                    continue;
                };
                if name == "sign" {
                    refuse(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}': sign is a column color mode, not a color; dropped"
                        ),
                    );
                    continue;
                }
                if name.starts_with(RESERVED_PREFIX) {
                    refuse(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}': absolute colors are not accepted here, name a color from colors.toml; dropped"
                        ),
                    );
                    continue;
                }
                if value.is_empty() {
                    refuse(
                        path,
                        format!("value colors '{dimension}': an empty value; dropped"),
                    );
                    continue;
                }
                if name == NO_COLOR {
                    continue;
                }
                out.insert(dimension, value, name);
            }
        }
        (out, diags)
    }

    pub fn dimension(&self, name: &str) -> Option<&DimensionColors> {
        self.by_dimension.get(name)
    }

    /// The color name for `value` of `dimension`; `None` when unmapped.
    pub fn get(&self, dimension: &str, value: &str) -> Option<&Arc<str>> {
        self.dimension(dimension)?.get(value)
    }

    /// The dimensions holding at least one colored value.
    pub fn dimensions(&self) -> impl Iterator<Item = &str> {
        self.by_dimension.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.by_dimension.is_empty()
    }

    pub fn insert(&mut self, dimension: &str, value: &str, color: &str) {
        self.by_dimension
            .entry(dimension.to_string())
            .or_default()
            .by_value
            .insert(value.to_string(), color.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, Severity, merge_docs};

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs(
            VALUE_COLORS_DOC,
            &[LayerDoc::builtin(VALUE_COLORS_DOC, text).expect("fixture parses")],
        )
    }

    #[test]
    fn a_value_names_its_color() {
        let (values, diags) = ValueColors::from_doc(&doc(
            "[underlying_ref]\nSPX = \"blue\"\n\"SX5E Index\" = \"teal\"\n",
        ));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            values.get("underlying_ref", "SPX").map(|c| &**c),
            Some("blue")
        );
        assert_eq!(
            values.get("underlying_ref", "SX5E Index").map(|c| &**c),
            Some("teal")
        );
        assert_eq!(values.get("underlying_ref", "NDX"), None, "unmapped");
        assert_eq!(values.get("underlying_ref", "spx"), None, "case-sensitive");
        assert_eq!(values.get("book", "SPX"), None, "another dimension");
        assert_eq!(values.dimensions().collect::<Vec<_>>(), ["underlying_ref"]);
    }

    #[test]
    fn none_reads_as_unmapped_without_a_diagnostic() {
        let (values, diags) =
            ValueColors::from_doc(&doc("[underlying_ref]\nSPX = \"none\"\nNDX = \"amber\"\n"));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(values.get("underlying_ref", "SPX"), None);
        assert!(values.get("underlying_ref", "NDX").is_some());
    }

    #[test]
    fn refused_entries_are_dropped_with_an_error_at_their_path() {
        let (values, diags) = ValueColors::from_doc(&doc("config_version = 1\n\
             book = \"blue\"\n\
             [underlying_ref]\n\
             SPX = 3\n\
             NDX = \"sign\"\n\
             RUT = \"#ff0000\"\n\
             \"\" = \"blue\"\n\
             DAX = \"blue\"\n"));
        assert_eq!(
            values.get("underlying_ref", "DAX").map(|c| &**c),
            Some("blue")
        );
        for dropped in ["SPX", "NDX", "RUT", ""] {
            assert_eq!(values.get("underlying_ref", dropped), None, "{dropped:?}");
        }
        assert!(values.dimension("book").is_none());
        let paths: Vec<_> = diags.iter().map(|d| d.path.clone().unwrap()).collect();
        assert_eq!(
            paths,
            [
                "value_colors.book",
                "value_colors.underlying_ref.SPX",
                "value_colors.underlying_ref.NDX",
                "value_colors.underlying_ref.RUT",
                "value_colors.underlying_ref.",
            ]
        );
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        assert!(
            diags[2].message.contains("sign is a column color mode"),
            "{}",
            diags[2].message
        );
    }

    #[test]
    fn a_dimension_with_only_cleared_values_is_absent() {
        let (values, _) = ValueColors::from_doc(&doc("[underlying_ref]\nSPX = \"none\"\n"));
        assert!(values.is_empty());
        assert!(values.dimension("underlying_ref").is_none());
    }
}
