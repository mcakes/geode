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
use crate::dimensions::DerivedDimensions;
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
    /// Set when composition produced a contradiction — two layers whose
    /// selections on one dimension do not overlap. Nothing can match, and
    /// that has to be said explicitly: an empty `values` already means "no
    /// constraint", so an emptied selection cannot carry the difference
    /// between "everything" and "nothing" (see [`Scope::and_then`]).
    pub impossible: bool,
}

impl Scope {
    /// Whether the scope constrains nothing. A contradiction is *not*
    /// empty — it selects no rows at all.
    pub fn is_empty(&self) -> bool {
        !self.impossible
            && self.dimensions.iter().all(|d| d.values.is_empty())
            && self.text.is_none()
            && self.expression.is_none()
    }

    /// Compose two layers (spec §4.2: global AND workspace AND tile).
    /// Selections on the same dimension **intersect**: narrowing twice
    /// narrows, and a layer can never widen what a coarser layer allowed.
    ///
    /// Disjoint selections are the case worth naming. Composing
    /// `book in [BK000]` with `book in [BK001]` yields *nothing*, and
    /// dropping the emptied selection would yield *everything* — a tile
    /// silently showing the whole desk because its scope contradicted the
    /// workspace's. So the contradiction is recorded in
    /// [`Scope::impossible`] rather than encoded as an empty selection,
    /// which already means the opposite.
    pub fn and_then(&self, inner: &Scope) -> Scope {
        let mut dimensions = self.dimensions.clone();
        let mut impossible = self.impossible || inner.impossible;
        for sel in &inner.dimensions {
            if sel.values.is_empty() {
                continue;
            }
            match dimensions.iter_mut().find(|d| d.column == sel.column) {
                Some(existing) => {
                    existing.values.retain(|v| sel.values.contains(v));
                    impossible |= existing.values.is_empty();
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
            impossible,
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
    ///
    /// A derived dimension (§6.8) is a legitimate scope column even though
    /// no dataset declares it — `desk = "Flow"` is the standing case — so
    /// a name is resolved through `dims` before being called unknown. What
    /// must exist is the column it derives *from*.
    pub fn validate(&self, ds: &DatasetSpec, dims: &DerivedDimensions) -> Vec<Diagnostic> {
        let bad = |message: String| Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message,
        };
        self.columns()
            .into_iter()
            .filter_map(|c| match dims.get(&c) {
                Some(d) if ds.column(&d.from).is_none() => Some(bad(format!(
                    "scope references '{c}', derived from '{}', which dataset '{}' does not have",
                    d.from, ds.name
                ))),
                Some(_) => None,
                None if ds.column(&c).is_none() => {
                    Some(bad(format!("scope references unknown column '{c}'")))
                }
                None => None,
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

        // The other half: `is_empty` must be able to say no, or it is a
        // constant. One case per field that can constrain.
        let constrained = |s: Scope| assert!(!s.is_empty(), "{s:?}");
        constrained(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        });
        constrained(Scope {
            text: Some("SPX".into()),
            ..Scope::default()
        });
        constrained(Scope {
            expression: Some(crate::scope::expr::parse_expr("delta01 > 1").unwrap()),
            ..Scope::default()
        });

        // A selection with no values really does mean "no constraint" —
        // the meaning `impossible` exists to stop overloading.
        assert!(
            Scope {
                dimensions: vec![DimensionSelection {
                    column: "book".into(),
                    values: Vec::new(),
                }],
                ..Scope::default()
            }
            .is_empty()
        );
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
        assert!(!e.impossible, "the sets overlap");
    }

    #[test]
    fn disjoint_selections_compose_to_nothing_rather_than_everything() {
        // The failure this guards against is silent and inverted: drop the
        // emptied selection and the composed scope constrains no book at
        // all, so a tile shows the whole desk precisely because its scope
        // contradicted the workspace's.
        let only = |book: &str| Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec![book.into()],
            }],
            ..Scope::default()
        };
        let e = only("BK000").and_then(&only("BK001"));
        assert!(e.impossible, "disjoint selections must contradict");
        assert!(
            !e.is_empty(),
            "a contradiction selects nothing; empty selects everything"
        );
    }

    #[test]
    fn a_contradiction_survives_further_composition() {
        // Otherwise a third, unrelated layer would launder it away.
        let contradiction = Scope {
            impossible: true,
            ..Scope::default()
        };
        assert!(contradiction.and_then(&Scope::default()).impossible);
        assert!(Scope::default().and_then(&contradiction).impossible);
    }

    #[test]
    fn an_inner_layer_may_introduce_a_dimension_the_outer_did_not_constrain() {
        let outer = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        let inner = Scope {
            dimensions: vec![DimensionSelection {
                column: "underlying_ref".into(),
                values: vec!["SPX".into()],
            }],
            ..Scope::default()
        };
        let e = outer.and_then(&inner);
        assert!(!e.impossible);
        let mut columns = e.columns();
        columns.sort();
        assert_eq!(columns, vec!["book".to_string(), "underlying_ref".into()]);
    }

    #[test]
    fn composing_expressions_conjoins_them_and_the_inner_text_wins() {
        let outer = Scope {
            text: Some("outer".into()),
            expression: Some(crate::scope::expr::parse_expr("delta01 > 1").unwrap()),
            ..Scope::default()
        };
        let inner = Scope {
            text: Some("inner".into()),
            expression: Some(crate::scope::expr::parse_expr("delta01 < 9").unwrap()),
            ..Scope::default()
        };
        let e = outer.and_then(&inner);
        assert_eq!(e.text.as_deref(), Some("inner"), "the finer layer wins");
        assert!(
            matches!(e.expression, Some(Expr::And(..))),
            "both expressions must survive: {:?}",
            e.expression
        );
        // And a layer with no expression must not erase the other's.
        let one_sided = outer.and_then(&Scope::default());
        assert!(matches!(one_sided.expression, Some(Expr::Compare { .. })));
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
            impossible: false,
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
        let diags = s.validate(&dataset(), &DerivedDimensions::default());
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
            impossible: false,
        };
        assert!(
            s.validate(&dataset(), &DerivedDimensions::default())
                .is_empty()
        );
    }

    #[test]
    fn the_text_filter_targets_only_declared_textual_columns() {
        // spec §4.1: matched against columns declared textual, not all.
        let ds = dataset();
        let textual: Vec<&str> = ds.textual_columns().map(|c| c.name.as_str()).collect();
        assert_eq!(textual, vec!["book", "underlying_ref"]);
    }
}
