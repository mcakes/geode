//! Rule folding: a definition's rules checked against the schema and
//! turned into the scopes the data layer runs. Every refusal is a
//! `RuleError` by index; the list is never dropped for a bad rule.

use super::{Rule, UNDERLYING, Watchlist};
use crate::dimensions::DerivedDimensions;
use crate::named::NamedExpressions;
use crate::query::ResolvedRule;
use crate::schema::{Family, SchemaSpec};
use crate::scope::{Scope, parse_expr};
use crate::scopes::SavedScopes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleError {
    pub index: usize,
    pub reason: String,
}

/// Datasets a rule may name: stored (measures or document family), not
/// computed, not local, carrying `underlying_ref`. Reference and series
/// datasets have no grain for a distinct read.
pub fn eligible_datasets(schema: &SchemaSpec) -> Vec<&str> {
    schema
        .datasets
        .iter()
        .filter(|ds| matches!(ds.family, Family::Measures | Family::Document))
        .filter(|ds| !ds.computed && !ds.local)
        .filter(|ds| ds.column(UNDERLYING).is_some())
        .map(|ds| ds.name.as_str())
        .collect()
}

fn dataset_error(schema: &SchemaSpec, name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("no dataset named".into());
    }
    let Some(ds) = schema.dataset(name) else {
        return Some(format!(
            "no dataset '{name}' (a dataset added since launch needs a restart)"
        ));
    };
    if ds.computed {
        return Some(format!(
            "'{name}' is computed by a module and holds no rows"
        ));
    }
    if ds.local {
        return Some(format!("'{name}' is a local dataset the app writes itself"));
    }
    match ds.family {
        Family::Reference => return Some(format!("'{name}' is a reference dataset")),
        Family::Series => return Some(format!("'{name}' is a series dataset")),
        Family::Measures | Family::Document => {}
    }
    if ds.column(UNDERLYING).is_none() {
        return Some(format!("'{name}' has no {UNDERLYING} column"));
    }
    None
}

fn fold_one(
    index: usize,
    rule: &Rule,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
    saved: &SavedScopes,
    named: &NamedExpressions,
) -> Result<ResolvedRule, String> {
    if let Some(e) = dataset_error(schema, &rule.dataset) {
        return Err(e);
    }
    let ds = schema.dataset(&rule.dataset).expect("checked above");
    let scope = match (&rule.scope, &rule.expression) {
        (Some(_), Some(_)) => {
            return Err("a rule names both a scope and an expression; keep one".into());
        }
        (None, None) => Scope::default(),
        (Some(name), None) => saved
            .get(name)
            .cloned()
            .ok_or_else(|| format!("saved scope '{name}' is not defined"))?,
        (None, Some(text)) => {
            let expr = parse_expr(text.trim())
                .map_err(|e| format!("expression: {} at column {}", e.message, e.caret + 1))?;
            Scope {
                expression: Some(expr),
                ..Scope::default()
            }
        }
    };
    let scope = scope.resolve(named)?;
    if scope.impossible {
        return Err(format!(
            "saved scope '{}' selects nothing",
            rule.scope.as_deref().unwrap_or("")
        ));
    }
    let diags = scope.validate(ds, dims);
    if let Some(d) = diags.first() {
        return Err(d.message.clone());
    }
    Ok(ResolvedRule {
        index,
        dataset: rule.dataset.clone(),
        scope,
    })
}

/// Every rule folded, good ones in definition order; the rest as errors by
/// index. Nothing here drops the list.
pub fn fold_rules(
    list: &Watchlist,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
    saved: &SavedScopes,
    named: &NamedExpressions,
) -> (Vec<ResolvedRule>, Vec<RuleError>) {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for (index, rule) in list.rules.iter().enumerate() {
        match fold_one(index, rule, schema, dims, saved, named) {
            Ok(r) => rules.push(r),
            Err(reason) => errors.push(RuleError { index, reason }),
        }
    }
    (rules, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn schema() -> SchemaSpec {
        // risk: measures with underlying_ref; cvi: document keyed on it;
        // pricer: computed; sheets: local document; underlyings: reference;
        // fx: measures without the column.
        let text = r#"
[risk]
[risk.columns.book]
role = "dimension"
type = "utf8"
grain = "position"
[risk.columns.underlying_ref]
role = "dimension"
type = "utf8"
grain = "position"
[risk.columns.npv]
role = "measure"
type = "f64"
grain = "position"
[cvi]
family = "document"
key = ["underlying_ref"]
axes = ["term"]
[cvi.columns.underlying_ref]
role = "dimension"
type = "utf8"
[cvi.columns.term]
role = "axis"
type = "f64"
[cvi.columns.atm]
role = "value"
type = "f64"
[pricer]
computed = true
[pricer.columns.underlying_ref]
role = "dimension"
type = "utf8"
grain = "position"
[sheets]
family = "document"
local = true
key = ["underlying_ref"]
axes = ["line"]
[sheets.columns.underlying_ref]
role = "dimension"
type = "utf8"
[sheets.columns.line]
role = "axis"
type = "utf8"
[sheets.columns.notional]
role = "value"
type = "f64"
[underlyings]
family = "reference"
key = ["underlying_ref"]
[underlyings.columns.underlying_ref]
role = "dimension"
type = "utf8"
[underlyings.columns.name]
role = "attribute"
type = "utf8"
[fx]
[fx.columns.pair]
role = "dimension"
type = "utf8"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == crate::config::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "fixture must parse clean: {errors:?}");
        schema
    }

    fn rule(dataset: &str, scope: Option<&str>, expression: Option<&str>) -> Rule {
        Rule {
            dataset: dataset.into(),
            scope: scope.map(str::to_string),
            expression: expression.map(str::to_string),
        }
    }

    #[test]
    fn eligible_datasets_are_stored_non_local_and_carry_the_column() {
        assert_eq!(eligible_datasets(&schema()), vec!["risk", "cvi"]);
    }

    #[test]
    fn a_whole_dataset_rule_folds_to_an_empty_scope() {
        let list = Watchlist {
            rules: vec![rule("risk", None, None)],
            ..Default::default()
        };
        let (rules, errors) = fold_rules(
            &list,
            &schema(),
            &DerivedDimensions::default(),
            &SavedScopes::new(),
            &NamedExpressions::default(),
        );
        assert!(errors.is_empty());
        assert_eq!(
            rules,
            vec![ResolvedRule {
                index: 0,
                dataset: "risk".into(),
                scope: Scope::default()
            }]
        );
    }

    #[test]
    fn a_saved_scope_is_folded_with_its_named_expressions() {
        let mut saved = SavedScopes::new();
        saved.insert(
            "eu".into(),
            Scope {
                named: vec!["big".into()],
                ..Scope::one("book", "BK000")
            },
        );
        let named = NamedExpressions::from_entries([("big", "npv > 1")]);
        let list = Watchlist {
            rules: vec![rule("risk", Some("eu"), None)],
            ..Default::default()
        };
        let (rules, errors) = fold_rules(
            &list,
            &schema(),
            &DerivedDimensions::default(),
            &saved,
            &named,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert!(rules[0].scope.named.is_empty());
        assert!(rules[0].scope.expression.is_some());
        assert_eq!(rules[0].scope.dimensions[0].values, vec!["BK000"]);
    }

    #[test]
    fn an_expression_rule_parses_and_validates_against_its_dataset() {
        let list = Watchlist {
            rules: vec![
                rule("risk", None, Some("book = 'BK000'")),
                rule("risk", None, Some("pair = 'EURUSD'")),
                rule("risk", None, Some("book = ")),
            ],
            ..Default::default()
        };
        let (rules, errors) = fold_rules(
            &list,
            &schema(),
            &DerivedDimensions::default(),
            &SavedScopes::new(),
            &NamedExpressions::default(),
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].index, 0);
        assert_eq!(
            errors.iter().map(|e| e.index).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(errors[0].reason.contains("pair"), "{}", errors[0].reason);
        assert!(errors[1].reason.contains("column"), "{}", errors[1].reason);
    }

    #[test]
    fn ineligible_datasets_and_double_scopes_are_rule_errors_by_index() {
        let list = Watchlist {
            rules: vec![
                rule("pricer", None, None),
                rule("sheets", None, None),
                rule("underlyings", None, None),
                rule("fx", None, None),
                rule("nope", None, None),
                rule("risk", Some("eu"), Some("book = 'x'")),
                rule("risk", Some("missing"), None),
                rule("", None, None),
            ],
            ..Default::default()
        };
        let (rules, errors) = fold_rules(
            &list,
            &schema(),
            &DerivedDimensions::default(),
            &SavedScopes::new(),
            &NamedExpressions::default(),
        );
        assert!(rules.is_empty());
        assert_eq!(
            errors.iter().map(|e| e.index).collect::<Vec<_>>(),
            (0..8).collect::<Vec<_>>()
        );
        assert!(errors[0].reason.contains("computed"));
        assert!(errors[1].reason.contains("local"));
        assert!(errors[2].reason.contains("reference"));
        assert!(errors[3].reason.contains("underlying_ref"));
        assert!(errors[4].reason.contains("no dataset 'nope'"));
        assert!(errors[5].reason.contains("both"));
        assert!(errors[6].reason.contains("saved scope 'missing'"));
        assert!(errors[7].reason.contains("no dataset"));
    }

    #[test]
    fn a_saved_scope_the_dataset_cannot_honour_is_a_rule_error() {
        let mut saved = SavedScopes::new();
        saved.insert("fx_only".into(), Scope::one("pair", "EURUSD"));
        saved.insert(
            "nothing".into(),
            Scope {
                impossible: true,
                ..Scope::one("book", "BK000")
            },
        );
        let list = Watchlist {
            rules: vec![
                rule("risk", Some("fx_only"), None),
                rule("risk", Some("nothing"), None),
            ],
            ..Default::default()
        };
        let (rules, errors) = fold_rules(
            &list,
            &schema(),
            &DerivedDimensions::default(),
            &saved,
            &NamedExpressions::default(),
        );
        assert!(rules.is_empty());
        assert!(errors[0].reason.contains("pair"));
        assert!(errors[1].reason.contains("selects nothing"));
    }
}
