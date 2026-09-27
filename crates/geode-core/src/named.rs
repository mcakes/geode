//! Named scope expressions, `expressions.toml`: an expression text saved
//! under a name so saved scopes and the frame can refer to it rather than
//! copy it. A reference is resolved by `Scope::resolve` before any query. An
//! entry that does not parse is kept as `Invalid` with its reason, so a
//! reference to it reports "invalid" rather than "missing". Dropping it would
//! make a broken definition indistinguishable from a deleted one.

use std::collections::BTreeMap;

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::scope::complete::{ExprVocab, check};
use crate::scope::{Expr, parse_expr};

pub use crate::config::EXPRESSIONS_DOC;

#[derive(Debug, Clone, PartialEq)]
pub enum NamedExpr {
    Valid { text: String, expr: Expr },
    Invalid { text: String, reason: String },
}

impl NamedExpr {
    pub fn text(&self) -> &str {
        match self {
            NamedExpr::Valid { text, .. } | NamedExpr::Invalid { text, .. } => text,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamedExpressions {
    map: BTreeMap<String, NamedExpr>,
}

impl NamedExpressions {
    /// Read the merged document. Unknown columns in a valid expression are a
    /// warning, and the entry stays valid; the query reports the column error
    /// exactly as it does for any expression.
    pub fn from_doc(doc: &MergedDoc, vocab: &ExprVocab) -> (Self, Vec<Diagnostic>) {
        let mut map = BTreeMap::new();
        let mut diags = Vec::new();
        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let path = format!("expressions.{name}.expression");
            let warn = |message: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("expression '{name}': {message}"),
                path: Some(path.clone()),
            };
            let text = value
                .as_table()
                .and_then(|t| t.get("expression"))
                .and_then(|v| v.as_str());
            let entry = match text {
                None => {
                    let reason = "missing 'expression' text".to_string();
                    diags.push(warn(reason.clone()));
                    NamedExpr::Invalid {
                        text: String::new(),
                        reason,
                    }
                }
                Some(text) => match parse_expr(text.trim()) {
                    Ok(expr) => {
                        for w in check(text, vocab, None) {
                            diags.push(warn(w.message));
                        }
                        NamedExpr::Valid {
                            text: text.to_string(),
                            expr,
                        }
                    }
                    Err(e) => {
                        let reason = format!("{} at column {}", e.message, e.caret + 1);
                        diags.push(warn(reason.clone()));
                        NamedExpr::Invalid {
                            text: text.to_string(),
                            reason,
                        }
                    }
                },
            };
            map.insert(name.clone(), entry);
        }
        (Self { map }, diags)
    }

    pub fn get(&self, name: &str) -> Option<&NamedExpr> {
        self.map.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Layer, LayerDoc, merge_docs};
    use crate::scope::complete::ExprVocab;

    fn doc(layers: &[(&str, Layer)]) -> MergedDoc {
        let docs: Vec<LayerDoc> = layers
            .iter()
            .map(|(text, layer)| {
                let mut d = LayerDoc::builtin(EXPRESSIONS_DOC, text).unwrap();
                d.layer = *layer;
                d
            })
            .collect();
        merge_docs(EXPRESSIONS_DOC, &docs)
    }

    #[test]
    fn valid_and_invalid_entries_are_both_kept() {
        let (n, diags) = NamedExpressions::from_doc(
            &doc(&[(
                "[good]\nexpression = \"npv > 0\"\n[bad]\nexpression = \"npv >\"\n",
                Layer::Builtin,
            )]),
            &ExprVocab::default(),
        );
        assert!(matches!(n.get("good"), Some(NamedExpr::Valid { .. })));
        match n.get("bad") {
            Some(NamedExpr::Invalid { reason, text }) => {
                assert_eq!(text, "npv >");
                assert!(reason.starts_with("expected a value"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(n.names().collect::<Vec<_>>(), ["bad", "good"]);
        assert!(
            diags
                .iter()
                .any(|d| d.path.as_deref() == Some("expressions.bad.expression"))
        );
    }

    #[test]
    fn a_non_table_or_missing_expression_is_invalid_not_dropped() {
        let (n, _) = NamedExpressions::from_doc(
            &doc(&[("x = 3\n[empty]\n", Layer::Builtin)]),
            &ExprVocab::default(),
        );
        assert!(matches!(n.get("x"), Some(NamedExpr::Invalid { .. })));
        assert!(matches!(n.get("empty"), Some(NamedExpr::Invalid { .. })));
    }

    #[test]
    fn the_user_layer_replaces_a_desk_object_whole() {
        // The desk's `liq` carries a second key the user's override omits.
        // `NamedExpr` only ever reads `expression`, so proving replacement
        // (rather than a leaf-only merge that would leave `note` behind)
        // needs the merged table itself, not just the two entries' text.
        let merged = doc(&[
            (
                "[liq]\nexpression = \"npv > 0\"\nnote = \"legacy\"\n[keep]\nexpression = \"npv < 0\"\n",
                Layer::Desk,
            ),
            ("[liq]\nexpression = \"npv > 5\"\n", Layer::User),
        ]);
        let liq = merged.value.get("liq").and_then(|v| v.as_table()).unwrap();
        assert!(
            !liq.contains_key("note"),
            "atomic merge should drop the desk-only key"
        );

        let (n, _) = NamedExpressions::from_doc(&merged, &ExprVocab::default());
        assert_eq!(n.get("liq").unwrap().text(), "npv > 5");
        assert_eq!(n.get("keep").unwrap().text(), "npv < 0");
    }

    #[test]
    fn unknown_columns_warn_but_stay_valid() {
        use crate::dimensions::DerivedDimensions;
        use crate::schema::SchemaSpec;
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        )
        .unwrap();
        let (schema, _) = SchemaSpec::from_doc(&merge_docs("datasets", &[datasets]));
        let vocab = ExprVocab::new(&schema, &DerivedDimensions::default());
        let (n, diags) = NamedExpressions::from_doc(
            &doc(&[("[liq]\nexpression = \"bokk = 'A'\"\n", Layer::Builtin)]),
            &vocab,
        );
        assert!(matches!(n.get("liq"), Some(NamedExpr::Valid { .. })));
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("expressions.liq.expression"))
            .unwrap();
        assert_eq!(d.severity, crate::config::Severity::Warning);
        assert!(d.message.contains("unknown column 'bokk'"), "{}", d.message);
    }
}
