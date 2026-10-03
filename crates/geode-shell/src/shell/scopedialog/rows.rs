//! The Current screen: the lane's scope as rows by ingredient — dimension
//! selections, expression terms then named references, and the text filter
//! — each with an identity that survives the frame changing under the
//! dialog, so the cursor stays on what the user was pointing at.

use geode_core::named::{NamedExpr, NamedExpressions};
use geode_core::scope::{Expr, Scope};
use geode_core::scopes::SavedScopes;

/// Painted above the sections for a contradictory scope. `Scope` records
/// only that a contradiction happened, not on which column.
pub(crate) const CONTRADICTION: &str = "nothing can match — selections don't overlap";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Dimensions,
    Expressions,
    Text,
}

/// The muted row an empty section paints, naming the key that fills it.
pub(crate) fn empty_hint(section: Section) -> &'static str {
    match section {
        Section::Dimensions => "no dimensions · p",
        Section::Expressions => "no expressions · x",
        Section::Text => "no text · t",
    }
}

/// What a row stands for, independent of its index. A term is its grammar
/// text plus its occurrence among equal texts: two identical terms are two
/// rows, and an identity shared between them would move the cursor, or a
/// removal, onto the wrong one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RowId {
    Dimension(String),
    Term(String, usize),
    Named(String),
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NamedState {
    Valid { text: String },
    Invalid { text: String, reason: String },
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RowKind {
    Dimension {
        column: String,
        values: Vec<String>,
    },
    /// `index` is the term's position in `Expr::conjuncts` order and `term`
    /// the value a term edit passes as `expected`, so an edit refuses when
    /// the scope moved under the dialog.
    Term {
        index: usize,
        term: Expr,
        text: String,
    },
    Named {
        name: String,
        state: NamedState,
    },
    Text {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub id: RowId,
    pub section: Section,
    pub kind: RowKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct CurrentRows {
    /// Selectable rows in paint order: dimensions in scope order, then
    /// expression terms, then named references, then the text.
    pub rows: Vec<Row>,
    pub contradiction: bool,
}

impl CurrentRows {
    pub(crate) fn derive(scope: &Scope, named: &NamedExpressions) -> CurrentRows {
        let mut rows: Vec<Row> = Vec::new();
        // One row per column: a composed scope can select a column twice,
        // and `d` removes every selection of it at once.
        for d in &scope.dimensions {
            if d.values.is_empty() && !scope.impossible {
                continue;
            }
            let existing = rows.iter_mut().find_map(|r| match &mut r.kind {
                RowKind::Dimension { column, values } if *column == d.column => Some(values),
                _ => None,
            });
            match existing {
                Some(values) => {
                    for v in &d.values {
                        if !values.contains(v) {
                            values.push(v.clone());
                        }
                    }
                }
                None => rows.push(Row {
                    id: RowId::Dimension(d.column.clone()),
                    section: Section::Dimensions,
                    kind: RowKind::Dimension {
                        column: d.column.clone(),
                        values: d.values.clone(),
                    },
                }),
            }
        }
        if let Some(expr) = &scope.expression {
            let mut seen: Vec<String> = Vec::new();
            for (index, term) in expr.conjuncts().into_iter().enumerate() {
                let text = term.to_string();
                let occurrence = seen.iter().filter(|t| **t == text).count();
                seen.push(text.clone());
                rows.push(Row {
                    id: RowId::Term(text.clone(), occurrence),
                    section: Section::Expressions,
                    kind: RowKind::Term {
                        index,
                        term: term.clone(),
                        text,
                    },
                });
            }
        }
        for name in &scope.named {
            let state = match named.get(name) {
                None => NamedState::Missing,
                Some(NamedExpr::Valid { text, .. }) => NamedState::Valid { text: text.clone() },
                Some(NamedExpr::Invalid { text, reason }) => NamedState::Invalid {
                    text: text.clone(),
                    reason: reason.clone(),
                },
            };
            rows.push(Row {
                id: RowId::Named(name.clone()),
                section: Section::Expressions,
                kind: RowKind::Named {
                    name: name.clone(),
                    state,
                },
            });
        }
        if let Some(text) = &scope.text {
            rows.push(Row {
                id: RowId::Text,
                section: Section::Text,
                kind: RowKind::Text { text: text.clone() },
            });
        }
        CurrentRows {
            rows,
            contradiction: scope.impossible,
        }
    }

    pub(crate) fn section_is_empty(&self, section: Section) -> bool {
        !self.rows.iter().any(|r| r.section == section)
    }

    /// Where the cursor lands after the rows re-derive: on the row it was
    /// on, by identity; else at its old index, clamped; `None` with no rows.
    pub(crate) fn place_cursor(
        &self,
        previous: Option<&RowId>,
        previous_index: usize,
    ) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        if let Some(id) = previous
            && let Some(i) = self.rows.iter().position(|r| &r.id == id)
        {
            return Some(i);
        }
        Some(previous_index.min(self.rows.len() - 1))
    }
}

/// Where the lane's scope stands against the saved scope it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Provenance {
    Empty,
    Unsaved,
    From(String),
    Changed(String),
}

impl Provenance {
    /// The title's right-hand text; `None` paints nothing.
    pub(crate) fn label(&self) -> Option<String> {
        match self {
            Provenance::Empty => None,
            Provenance::Unsaved => Some("unsaved".into()),
            Provenance::From(name) => Some(format!("from {name}")),
            Provenance::Changed(name) => Some(format!("from {name}, changed")),
        }
    }
}

/// An empty scope has no provenance to show. A source deleted since it was
/// loaded names nothing, so the scope reads as unsaved rather than as a
/// change against a definition that no longer exists.
pub(crate) fn provenance(
    scope: &Scope,
    loaded_from: Option<&str>,
    saved: &SavedScopes,
) -> Provenance {
    if scope.is_empty() {
        return Provenance::Empty;
    }
    match loaded_from.and_then(|name| saved.get(name).map(|s| (name, s))) {
        Some((name, s)) if s == scope => Provenance::From(name.to_string()),
        Some((name, _)) => Provenance::Changed(name.to_string()),
        None => Provenance::Unsaved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::scope::{DimensionSelection, parse_expr};

    fn named(text: &str) -> NamedExpressions {
        use geode_core::config::{EXPRESSIONS_DOC, LayerDoc, merge_docs};
        let doc = LayerDoc::builtin(EXPRESSIONS_DOC, text).unwrap();
        NamedExpressions::from_doc(
            &merge_docs(EXPRESSIONS_DOC, &[doc]),
            &geode_core::scope::complete::ExprVocab::default(),
        )
        .0
    }

    fn dims(pairs: &[(&str, &[&str])]) -> Vec<DimensionSelection> {
        pairs
            .iter()
            .map(|(c, vs)| DimensionSelection {
                column: c.to_string(),
                values: vs.iter().map(|v| v.to_string()).collect(),
            })
            .collect()
    }

    fn ids(rows: &CurrentRows) -> Vec<RowId> {
        rows.rows.iter().map(|r| r.id.clone()).collect()
    }

    #[test]
    fn rows_follow_section_order_and_scope_order() {
        let scope = Scope {
            dimensions: dims(&[("book", &["B1", "B2"]), ("underlying_ref", &["SPX"])]),
            text: Some("dec".into()),
            expression: Some(parse_expr("npv > 0 and delta < 5").unwrap()),
            impossible: false,
            named: vec!["liq".into()],
        };
        let rows = CurrentRows::derive(&scope, &named("[liq]\nexpression = \"npv > 1\"\n"));
        assert_eq!(
            ids(&rows),
            vec![
                RowId::Dimension("book".into()),
                RowId::Dimension("underlying_ref".into()),
                RowId::Term("npv > 0".into(), 0),
                RowId::Term("delta < 5".into(), 0),
                RowId::Named("liq".into()),
                RowId::Text,
            ]
        );
        assert!(!rows.contradiction);
        let RowKind::Term { index, text, .. } = &rows.rows[3].kind else {
            panic!("{:?}", rows.rows[3]);
        };
        assert_eq!((*index, text.as_str()), (1, "delta < 5"));
        assert_eq!(
            rows.rows[4].kind,
            RowKind::Named {
                name: "liq".into(),
                state: NamedState::Valid {
                    text: "npv > 1".into()
                }
            }
        );
    }

    #[test]
    fn an_unconstrained_selection_is_no_row_and_empty_sections_say_so() {
        let scope = Scope {
            dimensions: dims(&[("book", &[])]),
            ..Scope::default()
        };
        let rows = CurrentRows::derive(&scope, &NamedExpressions::default());
        assert!(rows.rows.is_empty());
        for s in [Section::Dimensions, Section::Expressions, Section::Text] {
            assert!(rows.section_is_empty(s));
        }
        assert_eq!(empty_hint(Section::Expressions), "no expressions · x");
    }

    #[test]
    fn a_column_selected_twice_is_one_row_with_its_values_in_order() {
        let scope = Scope {
            dimensions: dims(&[("book", &["B1"]), ("book", &["B2", "B1"])]),
            ..Scope::default()
        };
        let rows = CurrentRows::derive(&scope, &NamedExpressions::default());
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(
            rows.rows[0].kind,
            RowKind::Dimension {
                column: "book".into(),
                values: vec!["B1".into(), "B2".into()]
            }
        );
    }

    #[test]
    fn identical_terms_get_distinct_identities() {
        let scope = Scope {
            expression: Some(parse_expr("npv > 0 and npv > 0").unwrap()),
            ..Scope::default()
        };
        let rows = CurrentRows::derive(&scope, &NamedExpressions::default());
        assert_eq!(
            ids(&rows),
            vec![
                RowId::Term("npv > 0".into(), 0),
                RowId::Term("npv > 0".into(), 1)
            ]
        );
    }

    #[test]
    fn broken_references_say_why() {
        let scope = Scope {
            named: vec!["gone".into(), "bad".into()],
            ..Scope::default()
        };
        let rows = CurrentRows::derive(&scope, &named("[bad]\nexpression = \"npv >\"\n"));
        assert_eq!(
            rows.rows[0].kind,
            RowKind::Named {
                name: "gone".into(),
                state: NamedState::Missing
            }
        );
        assert!(matches!(
            &rows.rows[1].kind,
            RowKind::Named { state: NamedState::Invalid { text, .. }, .. } if text == "npv >"
        ));
    }

    #[test]
    fn a_contradiction_is_flagged_and_keeps_its_columns() {
        let scope = Scope {
            dimensions: dims(&[("book", &[])]),
            impossible: true,
            ..Scope::default()
        };
        let rows = CurrentRows::derive(&scope, &NamedExpressions::default());
        assert!(rows.contradiction);
        assert_eq!(ids(&rows), vec![RowId::Dimension("book".into())]);
        assert_eq!(
            CONTRADICTION,
            "nothing can match — selections don't overlap"
        );
    }

    #[test]
    fn the_cursor_keeps_its_row_by_identity_else_its_index_clamped() {
        let scope = Scope {
            dimensions: dims(&[("book", &["B1"]), ("desk", &["EQ"])]),
            text: Some("x".into()),
            ..Scope::default()
        };
        let rows = CurrentRows::derive(&scope, &NamedExpressions::default());
        // The desk row moved from index 1 to index 0 after book was dropped.
        let mut fewer = scope.clone();
        fewer.dimensions.remove(0);
        let after = CurrentRows::derive(&fewer, &NamedExpressions::default());
        assert_eq!(
            after.place_cursor(Some(&RowId::Dimension("desk".into())), 1),
            Some(0)
        );
        // Gone: the same index, clamped to the last row.
        assert_eq!(
            after.place_cursor(Some(&RowId::Dimension("book".into())), 5),
            Some(1)
        );
        assert_eq!(rows.place_cursor(None, 0), Some(0));
        let empty = CurrentRows::derive(&Scope::default(), &NamedExpressions::default());
        assert_eq!(empty.place_cursor(Some(&RowId::Text), 0), None);
    }

    #[test]
    fn provenance_reads_equal_changed_unsaved_or_nothing() {
        let mut saved = SavedScopes::new();
        saved.insert("eu".into(), Scope::one("book", "B1"));
        let eu = Scope::one("book", "B1");
        let other = Scope::one("book", "B2");
        assert_eq!(
            provenance(&eu, Some("eu"), &saved),
            Provenance::From("eu".into())
        );
        assert_eq!(
            provenance(&other, Some("eu"), &saved),
            Provenance::Changed("eu".into())
        );
        assert_eq!(provenance(&other, None, &saved), Provenance::Unsaved);
        // A source deleted since it was loaded no longer names anything.
        assert_eq!(
            provenance(&other, Some("gone"), &saved),
            Provenance::Unsaved
        );
        assert_eq!(
            provenance(&Scope::default(), Some("eu"), &saved),
            Provenance::Empty
        );
        assert_eq!(
            Provenance::From("eu".into()).label().as_deref(),
            Some("from eu")
        );
        assert_eq!(
            Provenance::Changed("eu".into()).label().as_deref(),
            Some("from eu, changed")
        );
        assert_eq!(Provenance::Unsaved.label().as_deref(), Some("unsaved"));
        assert_eq!(Provenance::Empty.label(), None);
    }
}
