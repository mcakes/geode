//! Saved scopes from `scopes.toml`. Each named scope replaces whole across
//! configuration layers.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::dimensions::DerivedDimensions;
use crate::schema::SchemaSpec;
use crate::scope::{DimensionSelection, Scope, parse_expr};
use std::collections::BTreeMap;

pub type SavedScopes = BTreeMap<String, Scope>;

/// Names reserved by scope actions. A saved scope named `save_current`
/// would collide with the action that opens the save prompt. Creation paths
/// share this list; action registration also skips occupied IDs so a
/// hand-edited configuration cannot panic the registry on a collision.
pub const RESERVED_NAMES: [&str; 1] = ["save_current"];

/// Read named scopes, warning and dropping non-table objects or strings
/// containing malformed expressions.
/// Validation runs against each dataset separately; the fewest diagnostics
/// are reported. With a nonempty schema, at least one dataset must validate
/// the entire scope. Columns spread across different datasets do not form a
/// union. With no datasets, validation yields no diagnostics.
///
/// Field-shape checks are permissive: non-table dimensions and non-string
/// text/expressions are ignored; non-array dimension values become empty
/// selections, and mixed arrays keep only strings. These silent fallbacks
/// can remove constraints from the loaded scope.
///
/// `named` is read as an ordered, deduplicated list of strings; a non-array
/// value warns and is ignored, but the scope is kept. Names are not checked
/// against the named-expressions document here — [`crate::scope::Scope::resolve`]
/// reports a missing or invalid name when the scope is used.
///
/// This reader does not reject reserved scope names; creation and action
/// registration enforce action-name conflicts separately.
pub fn saved_scopes_from_doc(
    doc: &MergedDoc,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
) -> (SavedScopes, Vec<Diagnostic>) {
    let mut out = SavedScopes::new();
    let mut diags = Vec::new();
    // Point diagnostics at `scopes.<name>` or the offending expression or
    // dimension-selection field.
    let warn = |path: String, m: String| Diagnostic {
        severity: Severity::Warning,
        layer: None,
        file: None,
        message: m,
        path: Some(path),
    };
    for (name, value) in &doc.value {
        if name == "config_version" {
            continue;
        }
        let Some(table) = value.as_table() else {
            diags.push(warn(
                format!("scopes.{name}"),
                format!("scopes: '{name}' must be a table; ignored"),
            ));
            continue;
        };
        let mut scope = Scope::default();
        if let Some(dimensions) = table.get("dimensions").and_then(|v| v.as_table()) {
            for (column, values) in dimensions {
                let values: Vec<String> = values
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                scope.dimensions.push(DimensionSelection {
                    column: column.clone(),
                    values,
                });
            }
        }
        scope.text = table
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(src) = table
            .get("expression")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            match parse_expr(src) {
                Ok(e) => scope.expression = Some(e),
                Err(e) => {
                    diags.push(warn(
                        format!("scopes.{name}.expression"),
                        format!("scopes: '{name}': expression: {e}; scope ignored"),
                    ));
                    continue;
                }
            }
        }
        if let Some(v) = table.get("named") {
            match v.as_array() {
                Some(a) => {
                    for s in a.iter().filter_map(|v| v.as_str()) {
                        if !scope.named.iter().any(|n| n == s) {
                            scope.named.push(s.to_string());
                        }
                    }
                }
                None => diags.push(warn(
                    format!("scopes.{name}.named"),
                    format!("scopes: '{name}': named must be an array of strings; ignored"),
                )),
            }
        }
        // Accept when one dataset validates the whole scope; otherwise report
        // the smallest diagnostic set. An empty schema supplies no checks.
        let bad: Vec<Diagnostic> = schema
            .datasets
            .iter()
            .map(|ds| scope.validate(ds, dims))
            .min_by_key(|d| d.len())
            .unwrap_or_default();
        if !bad.is_empty() {
            for d in bad {
                // `Scope::validate`'s message always quotes the offending
                // column first (`scope references unknown column 'x'`,
                // `'x' is a derived dimension, so...` — every message
                // shape it produces), so the first single-quoted run is
                // the column name. When that name is one of this scope's
                // own dimension selections the diagnostic lands on that
                // selection's row; otherwise (an expression-only column)
                // it lands on the expression field, since that is the
                // only other place a column name can come from here.
                let column = d.message.split('\'').nth(1).unwrap_or("");
                let path = if scope.dimensions.iter().any(|sel| sel.column == column) {
                    format!("scopes.{name}.dimensions.{column}")
                } else {
                    format!("scopes.{name}.expression")
                };
                diags.push(warn(
                    path,
                    format!("scopes: '{name}': {}; scope ignored", d.message),
                ));
            }
            continue;
        }
        out.insert(name.clone(), scope);
    }
    (out, diags)
}

/// Serialize selections, text, expression, and named-expression references in
/// the saved-scope table shape. Empty selections and an empty `named` list
/// are omitted. The `impossible` flag is not persisted, so this is not a
/// lossless serialization of an arbitrary composed scope.
pub fn scope_to_table(scope: &Scope) -> toml_edit::Table {
    let mut t = toml_edit::Table::new();
    let mut dims = toml_edit::Table::new();
    for d in &scope.dimensions {
        if d.values.is_empty() {
            continue;
        }
        let mut a = toml_edit::Array::new();
        for v in &d.values {
            a.push(v.as_str());
        }
        dims[d.column.as_str()] = toml_edit::value(a);
    }
    t["dimensions"] = toml_edit::Item::Table(dims);
    t["text"] = toml_edit::value(scope.text.clone().unwrap_or_default());
    t["expression"] = toml_edit::value(
        scope
            .expression
            .as_ref()
            .map(|e| e.to_string())
            .unwrap_or_default(),
    );
    if !scope.named.is_empty() {
        let mut a = toml_edit::Array::new();
        for n in &scope.named {
            a.push(n.as_str());
        }
        t["named"] = toml_edit::value(a);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn scope_doc(text: &str) -> MergedDoc {
        merge_docs("scopes", &[LayerDoc::builtin("scopes", text).unwrap()])
    }

    #[test]
    fn two_scopes_round_trip_through_the_table_writer_and_reader() {
        let schema = schema();
        let dims = DerivedDimensions::default();
        let doc = scope_doc(
            "[eu]\ntext = \"spx\"\n[eu.dimensions]\nbook = [\"BK001\", \"BK002\"]\n\n\
             [global]\nexpression = \"book = 'BK003'\"\n",
        );
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(saved.len(), 2);

        // Round-trip each scope through `scope_to_table`, back through a
        // fresh merged doc, and check the read-back set is identical.
        let mut roundtrip_doc = toml_edit::DocumentMut::new();
        for (name, scope) in &saved {
            roundtrip_doc[name.as_str()] = toml_edit::Item::Table(scope_to_table(scope));
        }
        let reparsed: toml::Table = roundtrip_doc.to_string().parse().unwrap();
        let merged = merge_docs(
            "scopes",
            &[LayerDoc {
                layer: crate::config::Layer::Builtin,
                name: "scopes".to_string(),
                file: "<test>".into(),
                table: reparsed,
            }],
        );
        let (saved_again, diags) = saved_scopes_from_doc(&merged, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(saved, saved_again);
    }

    #[test]
    fn a_scope_naming_an_unknown_column_is_dropped_with_a_warning() {
        let schema = schema();
        let dims = DerivedDimensions::default();
        let doc = scope_doc("[bad]\n[bad.dimensions]\nnonesuch = [\"X\"]\n");
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(saved.is_empty());
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("nonesuch"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn a_scope_with_a_bad_expression_is_dropped() {
        let schema = schema();
        let dims = DerivedDimensions::default();
        let doc = scope_doc("[bad]\nexpression = \"book = \"\n");
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(saved.is_empty());
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("expression"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn a_bad_expression_diagnostic_carries_its_field_path() {
        let schema = schema();
        let dims = DerivedDimensions::default();
        let doc = scope_doc("[eod]\nexpression = \"book = \"\n");
        let (_, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert_eq!(
            diags[0].path.as_deref(),
            Some("scopes.eod.expression"),
            "{diags:?}"
        );
    }

    #[test]
    fn an_unknown_column_diagnostic_carries_its_dimension_path() {
        let schema = schema();
        let dims = DerivedDimensions::default();
        let doc = scope_doc("[bad]\n[bad.dimensions]\nnonesuch = [\"X\"]\n");
        let (_, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert_eq!(
            diags[0].path.as_deref(),
            Some("scopes.bad.dimensions.nonesuch"),
            "{diags:?}"
        );
    }

    #[test]
    fn a_user_layer_overriding_one_name_replaces_only_that_name() {
        let schema = schema();
        let dims = DerivedDimensions::default();
        let builtin = LayerDoc::builtin(
            "scopes",
            "[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n[us]\n[us.dimensions]\nbook = [\"BK002\"]\n",
        )
        .unwrap();
        let user = LayerDoc {
            layer: crate::config::Layer::User,
            name: "scopes".to_string(),
            file: "<test:user>".into(),
            table: "[eu]\n[eu.dimensions]\nbook = [\"BK099\"]\n"
                .parse()
                .unwrap(),
        };
        let merged = merge_docs("scopes", &[builtin, user]);
        let (saved, diags) = saved_scopes_from_doc(&merged, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(saved["eu"].dimensions[0].values, vec!["BK099".to_string()]);
        assert_eq!(saved["us"].dimensions[0].values, vec!["BK002".to_string()]);
    }

    #[test]
    fn named_round_trips_and_a_missing_name_never_drops_the_scope() {
        let schema = schema();
        let dims = DerivedDimensions::default();
        let doc =
            scope_doc("[s]\nnamed = [\"liq\", \"hedges\"]\n[s.dimensions]\nbook = [\"BK001\"]\n");
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        let s = saved
            .get("s")
            .expect("kept although 'liq' is not defined anywhere");
        assert_eq!(s.named, ["liq", "hedges"]);
        let table = scope_to_table(s);
        assert_eq!(
            table
                .get("named")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(2)
        );
        assert!(
            scope_to_table(&Scope::default()).get("named").is_none(),
            "empty list omitted"
        );
    }

    #[test]
    fn a_non_array_named_warns_and_is_ignored() {
        let schema = schema();
        let dims = DerivedDimensions::default();
        let doc = scope_doc("[s]\nnamed = \"liq\"\n[s.dimensions]\nbook = [\"BK001\"]\n");
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(saved.get("s").unwrap().named.is_empty());
        assert!(
            diags
                .iter()
                .any(|d| d.path.as_deref() == Some("scopes.s.named"))
        );
    }
}
