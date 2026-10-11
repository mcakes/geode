//! The tile's one-line prompt: what each step asks for, and what the typed
//! answer leads to. A new classification asks its name, then its source
//! column; a rename asks the new name. Every answer is validated here, ahead
//! of any write: the config door writes even when the reload then rejects
//! the result, so an invalid name must never reach it.

use geode_core::classification::validate::{validate_name, validate_source};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;

/// What the prompt is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// A new classification's name.
    NewName,
    /// The source column the new classification `name` maps.
    NewColumn { name: String },
    /// The new name for `from`.
    Rename { from: String },
    /// A value to add to the shown classification, over column `from`.
    Add { from: String },
}

/// What an answer leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Ask the next question.
    Next(Prompt),
    /// Create an empty classification `name` over column `from`.
    Create { name: String, from: String },
    /// Rename `from` to `to`.
    Rename { from: String, to: String },
    /// The added value already has a row: go there and edit its label.
    Existing(String),
    /// A value no row holds: add it as a draft and edit its label.
    Draft(String),
    /// The answer is refused: the prompt stays open and says why.
    Refuse(String),
}

/// What the rename prompt says when the typed name is the current one.
pub const SAME_NAME: &str = "that is already its name";

/// What the add field says to a blank answer.
pub const ADD_BLANK: &str = "type a value";

/// The step an added value leads to. Source values are matched exactly:
/// `dax` is not `DAX` in the data, so it is a new value.
pub fn submit_add(text: &str, shown: &[&str]) -> Step {
    let text = text.trim();
    if text.is_empty() {
        return Step::Refuse(ADD_BLANK.into());
    }
    if shown.contains(&text) {
        Step::Existing(text.to_string())
    } else {
        Step::Draft(text.to_string())
    }
}

/// The step `text` (trimmed) leads to from `prompt`.
pub fn submit(prompt: &Prompt, text: &str, schema: &SchemaSpec, dims: &DerivedDimensions) -> Step {
    let text = text.trim();
    match prompt {
        Prompt::NewName => match validate_name(text, schema, dims) {
            Ok(()) => Step::Next(Prompt::NewColumn {
                name: text.to_string(),
            }),
            Err(why) => Step::Refuse(why),
        },
        Prompt::NewColumn { name } => match validate_source(text, schema, dims) {
            Ok(()) => Step::Create {
                name: name.clone(),
                from: text.to_string(),
            },
            Err(why) => Step::Refuse(why),
        },
        Prompt::Rename { from } if text == from => Step::Refuse(SAME_NAME.into()),
        Prompt::Rename { from } => match validate_name(text, schema, dims) {
            Ok(()) => Step::Rename {
                from: from.clone(),
                to: text.to_string(),
            },
            Err(why) => Step::Refuse(why),
        },
        // The tile routes the add field to `submit_add`; refusing here keeps
        // a slipped route an on-screen refusal rather than a crash.
        Prompt::Add { .. } => Step::Refuse("the add field answers through submit_add".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};

    fn schema() -> SchemaSpec {
        let text = r#"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.book]
type = "utf8"
role = "dimension"
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
        SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", text).unwrap()],
        ))
        .0
    }

    fn dims() -> DerivedDimensions {
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", "[region]\nfrom = \"underlying_ref\"\n").unwrap()],
        );
        DerivedDimensions::from_doc(&doc).0
    }

    fn submit_(prompt: &Prompt, text: &str) -> Step {
        submit(prompt, text, &schema(), &dims())
    }

    #[test]
    fn a_valid_new_name_asks_for_the_column() {
        assert_eq!(
            submit_(&Prompt::NewName, "sector"),
            Step::Next(Prompt::NewColumn {
                name: "sector".into()
            })
        );
    }

    #[test]
    fn a_shadowing_name_is_refused_with_the_rule_s_words() {
        let why = validate_name("book", &schema(), &dims()).unwrap_err();
        assert_eq!(submit_(&Prompt::NewName, "book"), Step::Refuse(why));
        assert!(matches!(
            submit_(&Prompt::NewName, "region"),
            Step::Refuse(_)
        ));
    }

    #[test]
    fn a_valid_column_creates() {
        let prompt = Prompt::NewColumn {
            name: "sector".into(),
        };
        assert_eq!(
            submit_(&prompt, "underlying_ref"),
            Step::Create {
                name: "sector".into(),
                from: "underlying_ref".into()
            }
        );
    }

    #[test]
    fn a_derived_column_is_refused_classifications_do_not_chain() {
        let prompt = Prompt::NewColumn {
            name: "sector".into(),
        };
        match submit_(&prompt, "region") {
            Step::Refuse(why) => assert!(why.contains("do not chain"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(submit_(&prompt, "delta"), Step::Refuse(_)));
    }

    #[test]
    fn a_rename_to_the_same_name_is_refused() {
        let prompt = Prompt::Rename {
            from: "region".into(),
        };
        assert_eq!(submit_(&prompt, " region "), Step::Refuse(SAME_NAME.into()));
    }

    #[test]
    fn a_rename_to_a_valid_name_renames() {
        let prompt = Prompt::Rename {
            from: "region".into(),
        };
        assert_eq!(
            submit_(&prompt, "zone"),
            Step::Rename {
                from: "region".into(),
                to: "zone".into()
            }
        );
        assert!(matches!(submit_(&prompt, "book"), Step::Refuse(_)));
    }

    #[test]
    fn an_added_value_is_trimmed_and_goes_to_its_row_or_a_draft() {
        assert_eq!(submit_add("  ", &["DAX"]), Step::Refuse(ADD_BLANK.into()));
        assert_eq!(submit_add(" DAX ", &["DAX"]), Step::Existing("DAX".into()));
        assert_eq!(
            submit_add("dax", &["DAX"]),
            Step::Draft("dax".into()),
            "values are case-sensitive"
        );
        assert_eq!(submit_add("KOSPI2", &["DAX"]), Step::Draft("KOSPI2".into()));
    }

    #[test]
    fn the_text_is_trimmed() {
        assert_eq!(
            submit_(&Prompt::NewName, "  sector\t"),
            Step::Next(Prompt::NewColumn {
                name: "sector".into()
            })
        );
        assert_eq!(
            submit_(
                &Prompt::NewColumn {
                    name: "sector".into()
                },
                " book "
            ),
            Step::Create {
                name: "sector".into(),
                from: "book".into()
            }
        );
    }
}
