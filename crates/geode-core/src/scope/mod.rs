//! What every tile is looking at (spec §4.1). Three predicate kinds
//! composed with AND: dimension selections, a text filter, and a validated
//! expression.
//!
//! This is the *value*. Scope state lives in the shell and the compiler
//! lives in the data layer, and those crates may never depend on each
//! other — so the type they share sits below both (spec §6.2).

pub mod expr;

pub use expr::{CompareOp, Expr, Literal, ParseError, parse_expr};

use crate::config::{Diagnostic, Severity};
use crate::schema::DatasetSpec;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DimensionSelection {
    pub column: String,
    /// Empty means "no constraint", not "match nothing" — an empty
    /// selection is dropped during composition rather than emitting a
    /// predicate that excludes every row.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scope {
    pub dimensions: Vec<DimensionSelection>,
    /// Matched case-insensitively against columns declared textual.
    pub text: Option<String>,
    pub expression: Option<Expr>,
}

impl Scope {
    pub fn is_empty(&self) -> bool {
        self.dimensions.iter().all(|d| d.values.is_empty())
            && self.text.is_none()
            && self.expression.is_none()
    }

    /// Compose two layers (spec §4.2: global AND workspace AND tile).
    /// Selections on the same dimension **intersect**: narrowing twice
    /// narrows, and a layer can never widen what a coarser layer allowed.
    pub fn and_then(&self, inner: &Scope) -> Scope {
        let mut dimensions = self.dimensions.clone();
        for sel in &inner.dimensions {
            if sel.values.is_empty() {
                continue;
            }
            match dimensions.iter_mut().find(|d| d.column == sel.column) {
                Some(existing) => {
                    existing.values.retain(|v| sel.values.contains(v));
                }
                None => dimensions.push(sel.clone()),
            }
        }
        dimensions.retain(|d| !d.values.is_empty());

        Scope {
            dimensions,
            text: inner.text.clone().or_else(|| self.text.clone()),
            expression: match (&self.expression, &inner.expression) {
                (Some(a), Some(b)) => Some(Expr::And(Box::new(a.clone()), Box::new(b.clone()))),
                (Some(a), None) => Some(a.clone()),
                (None, b) => b.clone(),
            },
        }
    }

    /// Every column the scope constrains. The text filter is excluded: it
    /// targets whatever the schema declares textual, not a named column.
    pub fn columns(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .dimensions
            .iter()
            .filter(|d| !d.values.is_empty())
            .map(|d| d.column.clone())
            .collect();
        if let Some(e) = &self.expression {
            out.extend(e.columns().into_iter().map(str::to_string));
        }
        out
    }

    /// Columns must exist in the dataset. Failures are Diagnostics, never
    /// panics — a bad scope is a user error reported at the point of entry
    /// (spec §10.1).
    pub fn validate(&self, ds: &DatasetSpec) -> Vec<Diagnostic> {
        self.columns()
            .into_iter()
            .filter(|c| ds.column(c).is_none())
            .map(|c| Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("scope references unknown column '{c}'"),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;

    fn dataset() -> crate::schema::DatasetSpec {
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
textual = true
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
textual = true
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk")
            .unwrap()
            .clone()
    }

    #[test]
    fn an_empty_scope_selects_everything() {
        assert!(Scope::default().is_empty());
        assert!(Scope::default().columns().is_empty());
    }

    #[test]
    fn layers_compose_by_conjunction() {
        // global AND workspace AND tile (spec §4.2).
        let global = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        let tile = Scope {
            text: Some("SPX".into()),
            ..Scope::default()
        };
        let effective = global.and_then(&tile);
        assert_eq!(effective.dimensions.len(), 1);
        assert_eq!(effective.text.as_deref(), Some("SPX"));
    }

    #[test]
    fn composing_the_same_dimension_intersects_its_values() {
        // Narrowing twice must narrow, never widen.
        let a = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into(), "BK001".into()],
            }],
            ..Scope::default()
        };
        let b = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK001".into(), "BK002".into()],
            }],
            ..Scope::default()
        };
        let e = a.and_then(&b);
        assert_eq!(e.dimensions.len(), 1);
        assert_eq!(e.dimensions[0].values, vec!["BK001".to_string()]);
    }

    #[test]
    fn columns_lists_every_dimension_the_scope_touches() {
        let s = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            expression: Some(parse_expr("lhu = 'L0'").unwrap()),
            text: None,
        };
        let mut cols = s.columns();
        cols.sort_unstable();
        assert_eq!(cols, vec!["book".to_string(), "lhu".to_string()]);
    }

    #[test]
    fn validation_rejects_unknown_columns_with_a_diagnostic() {
        let s = Scope {
            expression: Some(parse_expr("nonesuch = 'x'").unwrap()),
            ..Scope::default()
        };
        let diags = s.validate(&dataset());
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("nonesuch"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn validation_accepts_a_well_formed_scope() {
        let s = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            text: Some("SPX".into()),
            expression: Some(parse_expr("delta01 > 100").unwrap()),
        };
        assert!(s.validate(&dataset()).is_empty());
    }

    #[test]
    fn the_text_filter_targets_only_declared_textual_columns() {
        // spec §4.1: matched against columns declared textual, not all.
        let ds = dataset();
        let textual: Vec<&str> = ds.textual_columns().map(|c| c.name.as_str()).collect();
        assert_eq!(textual, vec!["book", "underlying_ref"]);
    }
}
