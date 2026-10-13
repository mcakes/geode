//! The tile's one-line prompt: what each step asks for, and what the typed
//! answer leads to. The add field asks a name to include by hand; a rule
//! is asked in three steps (its dataset, then its scope, then an
//! expression when the scope is one); New, Clone and Rename ask a list's
//! name. Every answer is checked here ahead of any write: a rule is folded
//! over a one-rule list against the factory's configuration, so what the
//! popup lists is what the data layer will run; a list's name goes through
//! `watchlist::validate_name` against every list defined, so a name the
//! reload would refuse never leaves the field. A name already a member is
//! refused by the add verb itself, naming where the name comes from.

use geode_core::dimensions::DerivedDimensions;
use geode_core::named::NamedExpressions;
use geode_core::schema::SchemaSpec;
use geode_core::scopes::SavedScopes;
use geode_core::watchlist::fold::{eligible_datasets, fold_rules};
use geode_core::watchlist::members::Member;
use geode_core::watchlist::{Rule, Watchlist, Watchlists, validate_name};

pub use super::rules::WHOLE_DATASET;

/// What the prompt is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// A name to add by hand.
    AddName,
    /// The dataset a new rule reads: one of the eligible ones.
    RuleDataset,
    /// The rule's scope: the whole dataset, a saved scope, or an
    /// expression to type next. `replace` is the rule being edited.
    RuleScope {
        dataset: String,
        replace: Option<usize>,
    },
    /// The rule's expression, over `dataset`'s columns.
    RuleExpression {
        dataset: String,
        replace: Option<usize>,
    },
    /// A new, empty list's name.
    NewName,
    /// The name a copy of `from` (as shown, pending edits included) is
    /// written under.
    CloneName { from: String },
    /// The new name of `from`.
    Rename { from: String },
}

impl Prompt {
    /// Whether this is one of the rule steps.
    pub fn is_rule(&self) -> bool {
        matches!(
            self,
            Prompt::RuleDataset | Prompt::RuleScope { .. } | Prompt::RuleExpression { .. }
        )
    }

    /// Whether this asks a list's name (New, Clone, Rename): answered
    /// through `submit_object`, with or without a list shown.
    pub fn is_object(&self) -> bool {
        matches!(
            self,
            Prompt::NewName | Prompt::CloneName { .. } | Prompt::Rename { .. }
        )
    }

    /// The rule a scope or expression step is editing in place, if any.
    pub fn replace(&self) -> Option<usize> {
        match self {
            Prompt::AddName
            | Prompt::RuleDataset
            | Prompt::NewName
            | Prompt::CloneName { .. }
            | Prompt::Rename { .. } => None,
            Prompt::RuleScope { replace, .. } | Prompt::RuleExpression { replace, .. } => *replace,
        }
    }
}

/// What an answer leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Add these names by hand.
    Add(Vec<String>),
    /// Ask the next thing.
    Next(Prompt),
    /// Write these as the list's rules, whole.
    Rules(Vec<Rule>),
    /// Write an empty list under `name` and show it.
    Create { name: String },
    /// Write `from` as it is shown under `to` and show it; `from` stays.
    Clone { from: String, to: String },
    /// Ask y/n, then write `from` under `to` and remove `from`, in one
    /// batch.
    Rename { from: String, to: String },
    /// The answer is refused: the prompt stays open and says why.
    Refuse(String),
}

/// What the add field says of a blank answer.
pub const TYPE_A_NAME: &str = "type a name";
/// What the dataset step says of a blank answer.
pub const PICK_A_DATASET: &str = "pick a dataset";
/// What the expression step says of a blank answer.
pub const TYPE_AN_EXPRESSION: &str = "type an expression";
/// The scope choice that leads to the expression step.
pub const EXPRESSION: &str = "expression\u{2026}";

/// The step `text` (trimmed) leads to from `prompt`. `members` are the
/// shown list's, for the steps that read them.
pub fn submit(prompt: &Prompt, text: &str, _members: &[Member]) -> Step {
    let text = text.trim();
    match prompt {
        Prompt::AddName if text.is_empty() => Step::Refuse(TYPE_A_NAME.into()),
        Prompt::AddName => Step::Add(vec![text.to_string()]),
        Prompt::RuleDataset | Prompt::RuleScope { .. } | Prompt::RuleExpression { .. } => {
            Step::Refuse("a rule step needs its context".into())
        }
        Prompt::NewName | Prompt::CloneName { .. } | Prompt::Rename { .. } => {
            Step::Refuse(NEEDS_THE_LISTS.into())
        }
    }
}

/// What a name step answered through another path says.
const NEEDS_THE_LISTS: &str = "a name step needs the lists defined";

/// The step `text` (trimmed) leads to from a name `prompt`, checked
/// against `existing`, every list defined: the core's name rules (an
/// identifier, no reserved word, no clash ignoring case). A rename's own
/// name is not a clash (so a case change is a rename) but is refused as
/// no change. The name is kept as typed, never re-cased.
pub fn submit_object(prompt: &Prompt, text: &str, existing: &Watchlists) -> Step {
    let text = text.trim();
    let from = match prompt {
        Prompt::NewName => None,
        Prompt::CloneName { from } => Some(from),
        Prompt::Rename { from } => {
            if text == from {
                return Step::Refuse(format!("{from} is already its name"));
            }
            Some(from)
        }
        Prompt::AddName
        | Prompt::RuleDataset
        | Prompt::RuleScope { .. }
        | Prompt::RuleExpression { .. } => {
            return Step::Refuse("not a name step".into());
        }
    };
    if text.is_empty() {
        return Step::Refuse(TYPE_A_NAME.into());
    }
    // On a rename the list's own name is left out of the clash check.
    let others;
    let existing = match prompt {
        Prompt::Rename { from } => {
            let mut kept = Watchlists::default();
            for (name, list) in existing.iter().filter(|(n, _)| *n != from) {
                kept.insert(name.clone(), list.clone());
            }
            others = kept;
            &others
        }
        _ => existing,
    };
    if let Err(why) = validate_name(text, existing) {
        return Step::Refuse(why);
    }
    let to = text.to_string();
    match (prompt, from) {
        (Prompt::NewName, _) => Step::Create { name: to },
        (Prompt::CloneName { .. }, Some(from)) => Step::Clone {
            from: from.clone(),
            to,
        },
        (Prompt::Rename { .. }, Some(from)) => Step::Rename {
            from: from.clone(),
            to,
        },
        _ => unreachable!("the other prompts returned above"),
    }
}

/// What a rule is validated against: the factory's configuration and the
/// shown list as it is now (the pending object while an edit awaits its
/// reload).
pub struct RuleContext<'a> {
    pub schema: &'a SchemaSpec,
    pub dims: &'a DerivedDimensions,
    pub saved: &'a SavedScopes,
    pub named: &'a NamedExpressions,
    pub current: &'a Watchlist,
}

impl RuleContext<'_> {
    /// The fold's word on `rule` alone: `Ok` when it folds clean.
    fn check(&self, rule: &Rule) -> Result<(), String> {
        let list = Watchlist {
            rules: vec![rule.clone()],
            ..Watchlist::default()
        };
        let (_, errors) = fold_rules(&list, self.schema, self.dims, self.saved, self.named);
        match errors.into_iter().next() {
            Some(e) => Err(e.reason),
            None => Ok(()),
        }
    }

    /// The rules with `rule` appended, or put at `replace` when the list
    /// still has that index (appended otherwise: the rules moved under
    /// the edit).
    fn with(&self, rule: Rule, replace: Option<usize>) -> Vec<Rule> {
        let mut rules = self.current.rules.clone();
        match replace {
            Some(i) if i < rules.len() => rules[i] = rule,
            _ => rules.push(rule),
        }
        rules
    }
}

fn rule(dataset: &str, scope: Option<&str>, expression: Option<&str>) -> Rule {
    Rule {
        dataset: dataset.to_string(),
        scope: scope.map(str::to_string),
        expression: expression.map(str::to_string),
    }
}

/// The scope step's choices for `dataset`: the whole dataset, each saved
/// scope that folds clean over it (one the dataset cannot honour, or that
/// selects nothing, is left out), then the expression step.
pub fn scope_choices(dataset: &str, ctx: &RuleContext) -> Vec<String> {
    let mut out = vec![WHOLE_DATASET.to_string()];
    out.extend(
        ctx.saved
            .keys()
            .filter(|name| ctx.check(&rule(dataset, Some(name), None)).is_ok())
            .cloned(),
    );
    out.push(EXPRESSION.to_string());
    out
}

/// The step `text` (trimmed) leads to from a rule `prompt`.
pub fn submit_rule(prompt: &Prompt, text: &str, ctx: &RuleContext) -> Step {
    let text = text.trim();
    match prompt {
        Prompt::AddName | Prompt::NewName | Prompt::CloneName { .. } | Prompt::Rename { .. } => {
            submit(prompt, text, &[])
        }
        Prompt::RuleDataset => {
            if text.is_empty() {
                return Step::Refuse(PICK_A_DATASET.into());
            }
            if !eligible_datasets(ctx.schema).contains(&text) {
                return Step::Refuse(format!("'{text}' is not a dataset a rule may read"));
            }
            Step::Next(Prompt::RuleScope {
                dataset: text.to_string(),
                replace: None,
            })
        }
        Prompt::RuleScope { dataset, replace } => {
            if text == WHOLE_DATASET {
                return Step::Rules(ctx.with(rule(dataset, None, None), *replace));
            }
            if text == EXPRESSION {
                return Step::Next(Prompt::RuleExpression {
                    dataset: dataset.clone(),
                    replace: *replace,
                });
            }
            if !ctx.saved.contains_key(text) {
                return Step::Refuse(format!(
                    "'{text}' is not {WHOLE_DATASET}, a saved scope or {EXPRESSION}"
                ));
            }
            let r = rule(dataset, Some(text), None);
            match ctx.check(&r) {
                Err(why) => Step::Refuse(why),
                Ok(()) => Step::Rules(ctx.with(r, *replace)),
            }
        }
        Prompt::RuleExpression { dataset, replace } => {
            if text.is_empty() {
                return Step::Refuse(TYPE_AN_EXPRESSION.into());
            }
            let r = rule(dataset, None, Some(text));
            match ctx.check(&r) {
                Err(why) => Step::Refuse(why),
                Ok(()) => Step::Rules(ctx.with(r, *replace)),
            }
        }
    }
}

/// The fixtures (`schema`, `saved`) are the tile tests' too.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::scope::Scope;

    #[test]
    fn the_add_step_trims_the_name_and_refuses_a_blank() {
        assert_eq!(
            submit(&Prompt::AddName, "  HSI ", &[]),
            Step::Add(vec!["HSI".into()])
        );
        assert_eq!(
            submit(&Prompt::AddName, "   ", &[]),
            Step::Refuse(TYPE_A_NAME.into())
        );
        assert_eq!(
            submit(&Prompt::AddName, "", &[]),
            Step::Refuse(TYPE_A_NAME.into())
        );
    }

    /// `risk` (measures, `book` and `underlying_ref`), `cvi` (a document
    /// keyed on `underlying_ref` with `term`), `fx` (measures without the
    /// column) and `underlyings` (reference).
    pub(crate) fn schema() -> SchemaSpec {
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
[fx]
[fx.columns.pair]
role = "dimension"
type = "utf8"
grain = "position"
[underlyings]
family = "reference"
key = ["underlying_ref"]
[underlyings.columns.underlying_ref]
role = "dimension"
type = "utf8"
[underlyings.columns.name]
role = "attribute"
type = "utf8"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == geode_core::config::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "fixture must parse clean: {errors:?}");
        schema
    }

    /// `eu` over `book` (fine on `risk`), `fx_only` over `pair` (which
    /// `risk` lacks) and `nothing`, impossible.
    pub(crate) fn saved() -> SavedScopes {
        let mut saved = SavedScopes::new();
        saved.insert("eu".into(), Scope::one("book", "BK000"));
        saved.insert("fx_only".into(), Scope::one("pair", "EURUSD"));
        saved.insert(
            "nothing".into(),
            Scope {
                impossible: true,
                ..Scope::one("book", "BK000")
            },
        );
        saved
    }

    struct Fixture {
        schema: SchemaSpec,
        dims: DerivedDimensions,
        saved: SavedScopes,
        named: NamedExpressions,
        current: Watchlist,
    }

    impl Fixture {
        fn new(current: Watchlist) -> Self {
            Fixture {
                schema: schema(),
                dims: DerivedDimensions::default(),
                saved: saved(),
                named: NamedExpressions::default(),
                current,
            }
        }

        fn ctx(&self) -> RuleContext<'_> {
            RuleContext {
                schema: &self.schema,
                dims: &self.dims,
                saved: &self.saved,
                named: &self.named,
                current: &self.current,
            }
        }
    }

    fn one(dataset: &str, scope: Option<&str>, expression: Option<&str>) -> Watchlist {
        Watchlist {
            rules: vec![rule(dataset, scope, expression)],
            ..Watchlist::default()
        }
    }

    #[test]
    fn scope_choices_keep_the_saved_scopes_the_dataset_can_honour() {
        let f = Fixture::new(Watchlist::default());
        assert_eq!(
            scope_choices("risk", &f.ctx()),
            [WHOLE_DATASET, "eu", EXPRESSION],
            "fx_only names a column risk lacks; nothing selects nothing"
        );
        assert_eq!(
            scope_choices("cvi", &f.ctx()),
            [WHOLE_DATASET, EXPRESSION],
            "cvi has no book column"
        );
    }

    #[test]
    fn the_dataset_step_takes_an_eligible_dataset_and_refuses_the_rest() {
        let f = Fixture::new(Watchlist::default());
        assert_eq!(
            submit_rule(&Prompt::RuleDataset, " risk ", &f.ctx()),
            Step::Next(Prompt::RuleScope {
                dataset: "risk".into(),
                replace: None
            })
        );
        for bad in ["fx", "underlyings", "nope", "Risk"] {
            assert_eq!(
                submit_rule(&Prompt::RuleDataset, bad, &f.ctx()),
                Step::Refuse(format!("'{bad}' is not a dataset a rule may read")),
                "{bad}"
            );
        }
        assert_eq!(
            submit_rule(&Prompt::RuleDataset, "  ", &f.ctx()),
            Step::Refuse(PICK_A_DATASET.into())
        );
    }

    #[test]
    fn the_scope_step_writes_whole_dataset_a_saved_scope_or_asks_an_expression() {
        let existing = one("cvi", None, None);
        let f = Fixture::new(existing.clone());
        let scope = |replace| Prompt::RuleScope {
            dataset: "risk".into(),
            replace,
        };
        // Whole dataset: appended after the existing rule.
        assert_eq!(
            submit_rule(&scope(None), WHOLE_DATASET, &f.ctx()),
            Step::Rules(vec![rule("cvi", None, None), rule("risk", None, None)])
        );
        // A saved scope that folds clean.
        assert_eq!(
            submit_rule(&scope(None), "eu", &f.ctx()),
            Step::Rules(vec![
                rule("cvi", None, None),
                rule("risk", Some("eu"), None)
            ])
        );
        // The expression step.
        assert_eq!(
            submit_rule(&scope(Some(0)), EXPRESSION, &f.ctx()),
            Step::Next(Prompt::RuleExpression {
                dataset: "risk".into(),
                replace: Some(0)
            })
        );
        // A saved scope the dataset cannot honour is refused with the
        // fold's reason; an unknown choice with the step's.
        let Step::Refuse(why) = submit_rule(&scope(None), "fx_only", &f.ctx()) else {
            panic!("refused");
        };
        assert!(why.contains("pair"), "{why}");
        let Step::Refuse(why) = submit_rule(&scope(None), "nothing", &f.ctx()) else {
            panic!("refused");
        };
        assert!(why.contains("selects nothing"), "{why}");
        assert_eq!(
            submit_rule(&scope(None), "whatever", &f.ctx()),
            Step::Refuse(format!(
                "'whatever' is not {WHOLE_DATASET}, a saved scope or {EXPRESSION}"
            ))
        );
        assert_eq!(
            submit_rule(&scope(None), "", &f.ctx()),
            Step::Refuse(format!(
                "'' is not {WHOLE_DATASET}, a saved scope or {EXPRESSION}"
            ))
        );
    }

    #[test]
    fn the_expression_step_folds_against_the_dataset_and_replaces_in_place() {
        let f = Fixture::new(Watchlist {
            rules: vec![rule("cvi", None, None), rule("risk", Some("eu"), None)],
            ..Watchlist::default()
        });
        let expr = |replace| Prompt::RuleExpression {
            dataset: "risk".into(),
            replace,
        };
        assert_eq!(
            submit_rule(&expr(Some(1)), " book = 'BK000' ", &f.ctx()),
            Step::Rules(vec![
                rule("cvi", None, None),
                rule("risk", None, Some("book = 'BK000'")),
            ]),
            "replaced at its index, trimmed"
        );
        // An index the rules no longer have appends instead.
        assert_eq!(
            submit_rule(&expr(Some(7)), "npv > 1", &f.ctx()),
            Step::Rules(vec![
                rule("cvi", None, None),
                rule("risk", Some("eu"), None),
                rule("risk", None, Some("npv > 1")),
            ])
        );
        // A column the dataset lacks, a syntax error, and a blank.
        let Step::Refuse(why) = submit_rule(&expr(None), "pair = 'EURUSD'", &f.ctx()) else {
            panic!("refused");
        };
        assert!(why.contains("pair"), "{why}");
        let Step::Refuse(why) = submit_rule(&expr(None), "book = ", &f.ctx()) else {
            panic!("refused");
        };
        assert!(why.starts_with("expression:"), "{why}");
        assert_eq!(
            submit_rule(&expr(None), "  ", &f.ctx()),
            Step::Refuse(TYPE_AN_EXPRESSION.into())
        );
    }

    #[test]
    fn a_rule_step_through_the_add_path_is_refused_not_added() {
        assert!(matches!(
            submit(&Prompt::RuleDataset, "risk", &[]),
            Step::Refuse(_)
        ));
        assert!(Prompt::RuleDataset.is_rule() && !Prompt::AddName.is_rule());
        assert!(!Prompt::NewName.is_rule());
    }

    /// `europe` and `asia` defined.
    fn existing() -> Watchlists {
        let mut lists = Watchlists::default();
        lists.insert("europe".into(), Watchlist::default());
        lists.insert("asia".into(), Watchlist::default());
        lists
    }

    #[test]
    fn a_new_name_is_trimmed_validated_and_clashes_ignoring_case() {
        let lists = existing();
        assert_eq!(
            submit_object(&Prompt::NewName, "  us_tech ", &lists),
            Step::Create {
                name: "us_tech".into()
            }
        );
        assert_eq!(
            submit_object(&Prompt::NewName, "   ", &lists),
            Step::Refuse(TYPE_A_NAME.into())
        );
        // A clash names the list it clashes with, whatever the case.
        assert_eq!(
            submit_object(&Prompt::NewName, "Europe", &lists),
            Step::Refuse("'Europe' already exists ('europe')".into())
        );
        // A reserved word and a bad identifier are the core's refusals.
        let Step::Refuse(why) = submit_object(&Prompt::NewName, "and", &lists) else {
            panic!("refused");
        };
        assert!(why.contains("reserved word"), "{why}");
        let Step::Refuse(why) = submit_object(&Prompt::NewName, "2fast", &lists) else {
            panic!("refused");
        };
        assert!(why.contains("not a valid name"), "{why}");
    }

    #[test]
    fn a_clone_name_is_validated_the_same_and_carries_the_source() {
        let lists = existing();
        let clone = Prompt::CloneName {
            from: "europe".into(),
        };
        assert_eq!(
            submit_object(&clone, "europe_copy", &lists),
            Step::Clone {
                from: "europe".into(),
                to: "europe_copy".into()
            }
        );
        assert_eq!(
            submit_object(&clone, "ASIA", &lists),
            Step::Refuse("'ASIA' already exists ('asia')".into())
        );
        assert_eq!(
            submit_object(&clone, "", &lists),
            Step::Refuse(TYPE_A_NAME.into())
        );
    }

    #[test]
    fn a_rename_refuses_its_own_name_and_clashes_with_the_others_only() {
        let lists = existing();
        let rename = Prompt::Rename {
            from: "europe".into(),
        };
        assert_eq!(
            submit_object(&rename, "emea", &lists),
            Step::Rename {
                from: "europe".into(),
                to: "emea".into()
            }
        );
        assert_eq!(
            submit_object(&rename, " europe ", &lists),
            Step::Refuse("europe is already its name".into())
        );
        // Its own name is not a clash, so a case change is a rename.
        assert_eq!(
            submit_object(&rename, "Europe", &lists),
            Step::Rename {
                from: "europe".into(),
                to: "Europe".into()
            }
        );
        assert_eq!(
            submit_object(&rename, "Asia", &lists),
            Step::Refuse("'Asia' already exists ('asia')".into())
        );
        assert_eq!(
            submit_object(&rename, "", &lists),
            Step::Refuse(TYPE_A_NAME.into())
        );
    }

    #[test]
    fn an_object_step_through_the_other_paths_is_refused() {
        assert!(matches!(
            submit(&Prompt::NewName, "x", &[]),
            Step::Refuse(_)
        ));
        let f = Fixture::new(Watchlist::default());
        assert!(matches!(
            submit_rule(&Prompt::NewName, "x", &f.ctx()),
            Step::Refuse(_)
        ));
        assert!(matches!(
            submit_object(&Prompt::AddName, "x", &existing()),
            Step::Refuse(_)
        ));
        assert!(matches!(
            submit_object(&Prompt::RuleDataset, "risk", &existing()),
            Step::Refuse(_)
        ));
    }
}
