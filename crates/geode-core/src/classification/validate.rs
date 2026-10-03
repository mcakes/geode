//! Rules for creating, renaming and deleting a classification: what may be
//! named, what may be classified, and who refers to a name today.

use crate::dimensions::DerivedDimensions;
use crate::groupings::GroupingSlots;
use crate::named::{NamedExpr, NamedExpressions};
use crate::schema::ColumnType;
use crate::schema::SchemaSpec;
use crate::scopes::SavedScopes;
use crate::view::ViewSpec;

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A new or renamed classification's name: an identifier (scope expressions
/// name it bare), not the version stamp, and not already a column or a
/// dimension — a shadowing name would make grouping by it ambiguous.
pub fn validate_name(
    name: &str,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
) -> Result<(), String> {
    if !is_identifier(name) {
        return Err(format!(
            "'{name}' is not a valid name: use letters, digits and _, not starting with a digit"
        ));
    }
    if name == "config_version" {
        return Err("'config_version' is reserved".into());
    }
    if schema.datasets.iter().any(|ds| ds.column(name).is_some()) {
        return Err(format!("'{name}' is already a dataset column"));
    }
    if dims.get(name).is_some() {
        return Err(format!("'{name}' already exists"));
    }
    Ok(())
}

/// Columns a classification may be made over: groupable in some dataset,
/// utf8 wherever declared, and not themselves derived. First-mention order
/// across datasets, each once.
pub fn source_columns(schema: &SchemaSpec, dims: &DerivedDimensions) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for ds in &schema.datasets {
        for name in ds.groupable_columns() {
            if out.iter().any(|c| c == name) || dims.get(name).is_some() {
                continue;
            }
            let all_utf8 = schema
                .datasets
                .iter()
                .filter_map(|d| d.column(name))
                .all(|c| c.ty == ColumnType::Utf8);
            if all_utf8 {
                out.push(name.to_string());
            }
        }
    }
    out
}

pub fn validate_source(
    from: &str,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
) -> Result<(), String> {
    if dims.get(from).is_some() {
        return Err(format!(
            "'{from}' is a classification; classifications do not chain"
        ));
    }
    if source_columns(schema, dims).iter().any(|c| c == from) {
        Ok(())
    } else {
        Err(format!("'{from}' is not a groupable text column"))
    }
}

/// Who names a classification today. Rename and delete do not rewrite these;
/// their confirm says how many will break.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct References {
    pub groupings: Vec<u8>,
    pub views: Vec<String>,
    pub scopes: Vec<String>,
    pub expressions: Vec<String>,
}

impl References {
    pub fn is_empty(&self) -> bool {
        self.groupings.is_empty()
            && self.views.is_empty()
            && self.scopes.is_empty()
            && self.expressions.is_empty()
    }

    /// `"2 groupings, 1 view"` — only the kinds present, in this order.
    pub fn summary(&self) -> String {
        let part = |n: usize, one: &str, many: &str| match n {
            0 => None,
            1 => Some(format!("1 {one}")),
            n => Some(format!("{n} {many}")),
        };
        [
            part(self.groupings.len(), "grouping", "groupings"),
            part(self.views.len(), "view", "views"),
            part(self.scopes.len(), "scope", "scopes"),
            part(self.expressions.len(), "expression", "expressions"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    }
}

pub fn references(
    name: &str,
    groupings: &GroupingSlots,
    views: &[ViewSpec],
    scopes: &SavedScopes,
    named: &NamedExpressions,
) -> References {
    let mut out = References::default();
    for slot in 1..=9u8 {
        if groupings
            .get(slot)
            .is_some_and(|g| g.iter().any(|c| c == name))
        {
            out.groupings.push(slot);
        }
    }
    for view in views {
        if view.grouping.iter().any(|c| c == name) || view.columns.iter().any(|c| c.name() == name)
        {
            out.views.push(view.name.clone());
        }
    }
    for (scope_name, scope) in scopes {
        if scope.columns().iter().any(|c| c == name) {
            out.scopes.push(scope_name.clone());
        }
    }
    for expr_name in named.names() {
        if let Some(NamedExpr::Valid { expr, .. }) = named.get(expr_name)
            && expr.columns().contains(&name)
        {
            out.expressions.push(expr_name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn schema() -> SchemaSpec {
        let text = r#"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.strike]
type = "f64"
role = "dimension"
grain = "instrument"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.delta]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
        let (schema, diags) = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", text).unwrap()],
        ));
        assert!(
            diags
                .iter()
                .all(|d| d.severity != crate::config::Severity::Error),
            "{diags:?}"
        );
        schema
    }

    fn dims() -> DerivedDimensions {
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin(
                "dimensions",
                "[desk]\nfrom = \"book\"\n[desk.values]\nEU = [\"BK0\"]\n",
            )
            .unwrap()],
        );
        DerivedDimensions::from_doc(&doc).0
    }

    #[test]
    fn a_name_must_be_an_identifier() {
        for bad in ["", "1abc", "has space", "a.b", "q'uote", "dash-ed"] {
            assert!(validate_name(bad, &schema(), &dims()).is_err(), "{bad:?}");
        }
        assert!(validate_name("gics_sector2", &schema(), &dims()).is_ok());
    }

    #[test]
    fn a_name_may_not_shadow_a_column_a_dimension_or_the_version_stamp() {
        assert_eq!(
            validate_name("book", &schema(), &dims()).unwrap_err(),
            "'book' is already a dataset column"
        );
        assert_eq!(
            validate_name("desk", &schema(), &dims()).unwrap_err(),
            "'desk' already exists"
        );
        assert!(validate_name("config_version", &schema(), &dims()).is_err());
    }

    #[test]
    fn source_columns_are_groupable_utf8_and_never_derived() {
        let cols = source_columns(&schema(), &dims());
        assert!(cols.contains(&"underlying_ref".to_string()), "{cols:?}");
        assert!(cols.contains(&"book".to_string()));
        assert!(
            !cols.contains(&"strike".to_string()),
            "f64 is not classifiable"
        );
        assert!(
            !cols.contains(&"delta".to_string()),
            "a measure is not groupable"
        );
        assert!(
            !cols.contains(&"desk".to_string()),
            "derived dimensions do not chain"
        );
        assert_eq!(
            validate_source("desk", &schema(), &dims()).unwrap_err(),
            "'desk' is a classification; classifications do not chain"
        );
        assert!(validate_source("strike", &schema(), &dims()).is_err());
        assert!(validate_source("underlying_ref", &schema(), &dims()).is_ok());
    }

    #[test]
    fn references_count_groupings_views_scopes_and_expressions() {
        use crate::scope::{DimensionSelection, Scope, parse_expr};
        let mut groupings = GroupingSlots::default();
        groupings.set(2, vec!["sector".into(), "underlying_ref".into()]);
        groupings.set(3, vec!["book".into()]);
        let view = ViewSpec {
            name: "by_sector".into(),
            grouping: vec!["sector".into()],
            ..ViewSpec::default()
        };
        let mut scopes = SavedScopes::new();
        scopes.insert(
            "tech".into(),
            Scope {
                dimensions: vec![DimensionSelection {
                    column: "sector".into(),
                    values: vec!["Tech".into()],
                }],
                ..Scope::default()
            },
        );
        scopes.insert(
            "expr".into(),
            Scope {
                expression: Some(parse_expr("sector = 'Energy'").unwrap()),
                ..Scope::default()
            },
        );
        let named = NamedExpressions::from_entries([
            ("energy", "sector != 'Energy'"),
            ("other", "book = 'BK0'"),
        ]);

        let refs = references("sector", &groupings, &[view], &scopes, &named);
        assert_eq!(refs.groupings, vec![2]);
        assert_eq!(refs.views, vec!["by_sector".to_string()]);
        assert_eq!(refs.scopes, vec!["expr".to_string(), "tech".to_string()]);
        assert_eq!(refs.expressions, vec!["energy".to_string()]);
        assert_eq!(refs.summary(), "1 grouping, 1 view, 2 scopes, 1 expression");
        assert!(references("nothing", &groupings, &[], &scopes, &named).is_empty());
    }
}
