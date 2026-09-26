//! Shared scope values: dimension selections, a text filter, and an
//! expression. The query combines these predicate kinds with AND. Across
//! scope layers, selections intersect, expressions combine with AND, and an
//! inner text filter replaces an outer text filter.
//!
//! Shell state and data compilation use these types without depending on
//! each other.

pub mod complete;
pub mod expr;

pub use expr::{CompareOp, Expr, Literal, ParseError, derived_op_error, parse_expr};

use crate::config::{Diagnostic, Severity};
use crate::dimensions::DerivedDimensions;
use crate::schema::DatasetSpec;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DimensionSelection {
    pub column: String,
    /// Empty represents no constraint unless `Scope::impossible` is set.
    /// Composition skips empty inner selections but can treat an empty
    /// outer selection as a contradiction; see [`Scope::and_then`].
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

    /// Compose scope layers. Selections on the same dimension intersect;
    /// expressions combine with AND. Inner text replaces outer text when set.
    /// Disjoint selections set `impossible` and retain the dimension name:
    /// an empty selection alone means no constraint, so it cannot represent
    /// the contradiction without this flag.
    ///
    /// Empty inner selections are skipped. Empty outer selections are
    /// cloned before intersection: if the inner scope supplies values for
    /// that column, the empty intersection sets `impossible`. This is an
    /// asymmetry for unnormalized inputs, including reader-produced empty
    /// selections; an empty outer entry is not always treated as absent.
    pub fn and_then(&self, inner: &Scope) -> Scope {
        let mut dimensions = self.dimensions.clone();
        let mut impossible = self.impossible || inner.impossible;
        // Carry the names of existing contradictions through further composition;
        // otherwise a later layer could hide which dimension selected no rows.
        let mut contradicted: Vec<String> = if self.impossible {
            self.dimensions
                .iter()
                .filter(|d| d.values.is_empty())
                .map(|d| d.column.clone())
                .collect()
        } else {
            Vec::new()
        };
        for sel in &inner.dimensions {
            if sel.values.is_empty() {
                continue;
            }
            match dimensions.iter_mut().find(|d| d.column == sel.column) {
                Some(existing) => {
                    existing.values.retain(|v| sel.values.contains(v));
                    if existing.values.is_empty() {
                        impossible = true;
                        contradicted.push(existing.column.clone());
                    }
                }
                None => dimensions.push(sel.clone()),
            }
        }
        // Retain emptied intersections so `columns()` can name the contradiction.
        // Drop other empty outer entries. An initially empty entry intersected
        // above is already marked contradictory and is retained.
        dimensions.retain(|d| !d.values.is_empty() || contradicted.contains(&d.column));

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

    /// Column references from selections and expressions, with duplicates
    /// retained. The text filter has no explicit column name. Contradictory
    /// selections still contribute their dimension names for scope chips.
    pub fn columns(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .dimensions
            .iter()
            .filter(|d| self.impossible || !d.values.is_empty())
            .map(|d| d.column.clone())
            .collect();
        if let Some(e) = &self.expression {
            out.extend(e.columns().into_iter().map(str::to_string));
        }
        out
    }

    /// Validate referenced columns against one dataset and return errors.
    /// Derived dimensions must resolve to source columns in that dataset.
    /// Comparisons on derived labels accept equality and inequality only;
    /// membership is also allowed. This checks names and those operators,
    /// not literal types or every column's storage/query eligibility.
    pub fn validate(&self, ds: &DatasetSpec, dims: &DerivedDimensions) -> Vec<Diagnostic> {
        let bad = |message: String| Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message,
            path: None,
        };
        let mut diags: Vec<Diagnostic> = self
            .columns()
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
            .collect();

        // Derived dimensions are mapped labels; ordering and `like` are not
        // supported. Diagnose these at entry before the compiler rejects them.
        if let Some(e) = &self.expression {
            e.for_each_comparison(&mut |column, op| {
                if dims.get(column).is_some()
                    && let Some(message) = derived_op_error(column, op.grammar())
                {
                    diags.push(bad(message));
                }
            });
        }
        diags
    }

    /// Remove dimension selections whose columns the dataset lacks, resolving
    /// derived dimensions through their sources. Return the removed names for
    /// provenance. Text, expressions, and `impossible` pass through unchanged;
    /// this does not validate or remove unknown expression references.
    pub fn applicable_to(
        &self,
        ds: &DatasetSpec,
        dims: &DerivedDimensions,
    ) -> (Scope, Vec<String>) {
        let mut kept = self.clone();
        let mut dropped = Vec::new();
        kept.dimensions.retain(|d| {
            let present = match dims.get(&d.column) {
                Some(derived) => ds.column(&derived.from).is_some(),
                None => ds.column(&d.column).is_some(),
            };
            if !present {
                dropped.push(d.column.clone());
            }
            present
        });
        (kept, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::{ColumnRole, ColumnSpec, ColumnType, DatasetSpec, Family, SchemaSpec};

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
        // Compose global, workspace, and tile scope in order.
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
    fn an_ordering_comparison_on_a_derived_dimension_is_caught_at_entry() {
        // Mapped labels reject ordering at validation time as well as compilation.
        let dims = merge_docs(
            "dimensions",
            &[LayerDoc::builtin(
                "dimensions",
                "[desk]\nfrom = \"book\"\n[desk.values]\nBK000 = \"Flow\"\n",
            )
            .unwrap()],
        );
        let dims = crate::dimensions::DerivedDimensions::from_doc(&dims).0;

        let ordered = Scope {
            expression: Some(parse_expr("desk > 'EU'").unwrap()),
            ..Scope::default()
        };
        let diags = ordered.validate(&dataset(), &dims);
        assert!(
            diags.iter().any(|d| d.message.contains("no meaning")),
            "{diags:?}"
        );

        // Equality and membership are exactly what a mapped label supports,
        // so they must not be reported — or the rule is just "no
        // expressions on derived dimensions".
        for text in ["desk = 'Flow'", "desk != 'Flow'"] {
            let ok = Scope {
                expression: Some(parse_expr(text).unwrap()),
                ..Scope::default()
            };
            assert!(
                ok.validate(&dataset(), &dims).is_empty(),
                "{text} is meaningful on a derived dimension"
            );
        }

        // And an ordering comparison on a real column is fine.
        let real = Scope {
            expression: Some(parse_expr("delta01 > 1").unwrap()),
            ..Scope::default()
        };
        assert!(real.validate(&dataset(), &dims).is_empty());
    }

    #[test]
    fn a_contradiction_still_names_the_dimension_that_caused_it() {
        // Scope chips must name the contradictory dimension even when no rows match.
        let selection = |v: &str| Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec![v.into()],
            }],
            ..Scope::default()
        };
        let contradiction = selection("BK000").and_then(&selection("BK001"));

        assert!(contradiction.impossible, "precondition");
        assert_eq!(
            contradiction.columns(),
            vec!["book".to_string()],
            "the contradicted dimension is still what the scope is about"
        );
        assert!(
            !contradiction.is_empty(),
            "and it is not an empty scope, which selects everything"
        );

        // Further composition must preserve the original contradiction name.
        let third = Scope {
            dimensions: vec![DimensionSelection {
                column: "lhu".into(),
                values: vec!["L0".into()],
            }],
            ..Scope::default()
        };
        let deeper = contradiction.and_then(&third);
        assert!(deeper.impossible, "a contradiction cannot be laundered");
        assert!(
            deeper.columns().contains(&"book".to_string()),
            "the dimension that contradicted is still named: {:?}",
            deeper.columns()
        );

        let deepest = deeper.and_then(&Scope::default());
        assert!(deepest.impossible);
        assert!(
            deepest.columns().contains(&"book".to_string()),
            "and again through a layer that constrains nothing: {:?}",
            deepest.columns()
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
        // The text filter targets declared textual columns.
        let ds = dataset();
        let textual: Vec<&str> = ds.textual_columns().map(|c| c.name.as_str()).collect();
        assert_eq!(textual, vec!["book", "underlying_ref"]);
    }

    /// A minimal document dataset with one identity dimension.
    fn document_dataset_with_dimension(column: &str) -> DatasetSpec {
        DatasetSpec {
            name: "cvi".into(),
            columns: vec![ColumnSpec {
                name: column.into(),
                source_name: None,
                ty: ColumnType::Utf8,
                required: true,
                textual: false,
                categorical: true,
                role: ColumnRole::Dimension { grain: None },
            }],
            family: Family::Document,
            key: vec![column.into()],
            axes: vec![],
            local: false,
            series_retention: None,
        }
    }

    #[test]
    fn applicable_to_drops_selections_on_columns_the_dataset_lacks_and_names_them() {
        let ds = document_dataset_with_dimension("underlying_ref");
        let scope = Scope {
            dimensions: vec![
                DimensionSelection {
                    column: "book".into(),
                    values: vec!["EQD".into()],
                },
                DimensionSelection {
                    column: "underlying_ref".into(),
                    values: vec!["SPX.Z".into()],
                },
                DimensionSelection {
                    column: "lhu".into(),
                    values: vec!["A".into()],
                },
            ],
            text: Some("spx".into()),
            expression: None,
            impossible: false,
        };
        let (kept, dropped) = scope.applicable_to(&ds, &DerivedDimensions::default());
        assert_eq!(dropped, vec!["book".to_string(), "lhu".to_string()]);
        assert_eq!(kept.dimensions.len(), 1);
        assert_eq!(kept.dimensions[0].column, "underlying_ref");
        assert_eq!(kept.text.as_deref(), Some("spx"), "text passes through");

        // A derived dimension over a column the dataset has is kept.
        let dims_doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", "[region]\nfrom = \"underlying_ref\"\n").unwrap()],
        );
        let dims = crate::dimensions::DerivedDimensions::from_doc(&dims_doc).0;
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "region".into(),
                values: vec!["US".into()],
            }],
            ..Scope::default()
        };
        let (kept, dropped) = scope.applicable_to(&ds, &dims);
        assert!(dropped.is_empty());
        assert_eq!(kept.dimensions.len(), 1);
    }
}
