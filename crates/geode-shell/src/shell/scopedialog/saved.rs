//! The Saved screen's rows: saved scopes, then saved expressions, each in
//! name order. `enter` means different things in the two sections — a scope
//! replaces the current one, an expression is added to it — so the rows keep
//! the two kinds apart rather than merging them into one list of names.

use geode_core::named::{NamedExpr, NamedExpressions};
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;

pub(crate) const SCOPES_NOTE: &str = "enter replaces the current scope";
pub(crate) const EXPRESSIONS_NOTE: &str = "enter adds to the current scope";
pub(crate) const NO_SCOPES: &str = "no saved scopes · s saves the current one";
pub(crate) const NO_EXPRESSIONS: &str = "no saved expressions · n names a new one";

/// A summary term longer than this is cut, so one long expression does not
/// push the rest of the summary out of the row.
const TERM_CHARS: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SavedId {
    Scope(String),
    Expression(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SavedKind {
    Scope {
        summary: String,
    },
    /// `applied` when the lane's scope refers to it; `broken` carries the
    /// reason a definition does not parse.
    Expression {
        text: String,
        applied: bool,
        broken: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SavedRow {
    pub id: SavedId,
    pub name: String,
    pub kind: SavedKind,
}

pub(crate) fn saved_rows(
    saved: &SavedScopes,
    named: &NamedExpressions,
    current: &Scope,
) -> Vec<SavedRow> {
    let scopes = saved.iter().map(|(name, scope)| SavedRow {
        id: SavedId::Scope(name.clone()),
        name: name.clone(),
        kind: SavedKind::Scope {
            summary: summary(scope),
        },
    });
    let expressions = named.names().filter_map(|name| {
        let def = named.get(name)?;
        let broken = match def {
            NamedExpr::Invalid { reason, .. } => Some(reason.clone()),
            NamedExpr::Valid { .. } => None,
        };
        Some(SavedRow {
            id: SavedId::Expression(name.to_string()),
            name: name.to_string(),
            kind: SavedKind::Expression {
                text: def.text().to_string(),
                applied: current.named.iter().any(|n| n == name),
                broken,
            },
        })
    });
    scopes.chain(expressions).collect()
}

/// One line per saved scope: each constrained column with its one value or
/// its value count, each named reference, each expression term (cut), and
/// the quoted text, joined ` · `. An empty scope reads "everything".
pub(crate) fn summary(scope: &Scope) -> String {
    let mut parts: Vec<String> = Vec::new();
    for d in scope.dimensions.iter().filter(|d| !d.values.is_empty()) {
        parts.push(match d.values.as_slice() {
            [one] => format!("{} {one}", d.column),
            many => format!("{} {}", d.column, many.len()),
        });
    }
    parts.extend(scope.named.iter().map(|n| format!("≡ {n}")));
    if let Some(expr) = &scope.expression {
        for term in expr.conjuncts() {
            let text = term.to_string();
            parts.push(if text.chars().count() > TERM_CHARS {
                let cut: String = text.chars().take(TERM_CHARS - 1).collect();
                format!("{cut}…")
            } else {
                text
            });
        }
    }
    if let Some(text) = &scope.text {
        parts.push(format!("\"{text}\""));
    }
    if parts.is_empty() {
        "everything".into()
    } else {
        parts.join(" · ")
    }
}

/// The "Used by …" sentence the delete question and the definition note carry for a named expression: `scopes` in
/// name order, then the frame when `frame` ticks it, joined "A", "A and B" or
/// "A, B and C". `None` when nothing uses it, so no sentence is painted.
pub(crate) fn used_by_sentence(mut scopes: Vec<String>, frame: bool) -> Option<String> {
    scopes.sort();
    if frame {
        scopes.push("the current scope".to_string());
    }
    let list = match scopes.as_slice() {
        [] => return None,
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    };
    Some(format!("Used by {list}."))
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

    #[test]
    fn the_used_by_sentence_lists_scopes_in_order_then_the_frame() {
        use super::used_by_sentence;
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(used_by_sentence(Vec::new(), false), None);
        assert_eq!(
            used_by_sentence(names(&["EQ liquid"]), false).as_deref(),
            Some("Used by EQ liquid.")
        );
        assert_eq!(
            used_by_sentence(names(&["RATES liquid", "EQ liquid"]), false).as_deref(),
            Some("Used by EQ liquid and RATES liquid.")
        );
        assert_eq!(
            used_by_sentence(names(&["c", "a", "b"]), false).as_deref(),
            Some("Used by a, b and c.")
        );
        assert_eq!(
            used_by_sentence(names(&["RATES liquid", "EQ liquid"]), true).as_deref(),
            Some("Used by EQ liquid, RATES liquid and the current scope.")
        );
        assert_eq!(
            used_by_sentence(Vec::new(), true).as_deref(),
            Some("Used by the current scope.")
        );
    }

    #[test]
    fn scopes_then_expressions_each_in_name_order_with_applied_and_broken() {
        let mut saved = SavedScopes::new();
        saved.insert("rates".into(), Scope::one("desk", "RATES"));
        saved.insert("eq".into(), Scope::one("desk", "EQ"));
        let defs = named("[liq]\nexpression = \"npv > 0\"\n[bad]\nexpression = \"npv >\"\n");
        let current = Scope {
            named: vec!["liq".into()],
            ..Scope::default()
        };
        let rows = saved_rows(&saved, &defs, &current);
        let ids: Vec<SavedId> = rows.iter().map(|r| r.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                SavedId::Scope("eq".into()),
                SavedId::Scope("rates".into()),
                SavedId::Expression("bad".into()),
                SavedId::Expression("liq".into()),
            ]
        );
        assert_eq!(
            rows[0].kind,
            SavedKind::Scope {
                summary: "desk EQ".into()
            }
        );
        assert!(matches!(
            &rows[2].kind,
            SavedKind::Expression {
                applied: false,
                broken: Some(_),
                ..
            }
        ));
        assert_eq!(
            rows[3].kind,
            SavedKind::Expression {
                text: "npv > 0".into(),
                applied: true,
                broken: None
            }
        );
    }

    #[test]
    fn a_summary_names_each_ingredient_briefly() {
        let scope = Scope {
            dimensions: vec![
                DimensionSelection {
                    column: "underlying_ref".into(),
                    values: vec!["SPX".into()],
                },
                DimensionSelection {
                    column: "book".into(),
                    values: vec!["B1".into(), "B2".into()],
                },
                DimensionSelection {
                    column: "desk".into(),
                    values: vec![],
                },
            ],
            text: Some("dec".into()),
            expression: Some(
                parse_expr("npv > 0 and underlying_ref = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'").unwrap(),
            ),
            impossible: false,
            named: vec!["liq".into()],
        };
        assert_eq!(
            summary(&scope),
            "underlying_ref SPX · book 2 · ≡ liq · npv > 0 · underlying_ref = 'ABCDE… · \"dec\""
        );
        assert_eq!(summary(&Scope::default()), "everything");
    }
}
