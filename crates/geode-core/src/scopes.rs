//! Saved scopes (Phase 4 spec §3.9): `scopes.toml`, a doc atomic at depth
//! one, each named scope replaced whole by a finer layer.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::dimensions::DerivedDimensions;
use crate::schema::SchemaSpec;
use crate::scope::{DimensionSelection, Scope, parse_expr};
use std::collections::BTreeMap;

pub type SavedScopes = BTreeMap<String, Scope>;

/// Read every named scope out of a merged `scopes` doc, dropping (with a
/// warning) any that isn't a table, names a column no dataset declares, or
/// carries an unparseable expression. Validated against every dataset in
/// `schema` — the dataset reporting the fewest problems wins, so a scope
/// naming columns from more than one dataset isn't unfairly penalised by
/// checking it against just one.
pub fn saved_scopes_from_doc(
    doc: &MergedDoc,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
) -> (SavedScopes, Vec<Diagnostic>) {
    let mut out = SavedScopes::new();
    let mut diags = Vec::new();
    let warn = |m: String| Diagnostic {
        severity: Severity::Warning,
        layer: None,
        file: None,
        message: m,
        path: None,
    };
    for (name, value) in &doc.value {
        if name == "config_version" {
            continue;
        }
        let Some(table) = value.as_table() else {
            diags.push(warn(format!("scopes: '{name}' must be a table; ignored")));
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
                    diags.push(warn(format!(
                        "scopes: '{name}': expression: {e}; scope ignored"
                    )));
                    continue;
                }
            }
        }
        // Validate against every dataset that has the columns; a scope
        // naming a column no dataset declares is an error for that scope.
        let bad: Vec<Diagnostic> = schema
            .datasets
            .iter()
            .map(|ds| scope.validate(ds, dims))
            .min_by_key(|d| d.len())
            .unwrap_or_default();
        if !bad.is_empty() {
            for d in bad {
                diags.push(warn(format!(
                    "scopes: '{name}': {}; scope ignored",
                    d.message
                )));
            }
            continue;
        }
        out.insert(name.clone(), scope);
    }
    (out, diags)
}

/// The TOML table `persist_scope_to_user_config` (geode-shell) writes for
/// one scope — the inverse of the table shape [`saved_scopes_from_doc`]
/// reads.
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
}
