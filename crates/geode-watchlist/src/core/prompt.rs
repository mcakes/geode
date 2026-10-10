//! The tile's one-line prompt: what each step asks for, and what the typed
//! answer leads to. The add field asks a name to include by hand. Every
//! answer is checked here ahead of any write; a name already a member is
//! refused by the add verb itself, naming where the name comes from.

use geode_core::watchlist::members::Member;

/// What the prompt is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// A name to add by hand.
    AddName,
}

/// What an answer leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Add these names by hand.
    Add(Vec<String>),
    /// The answer is refused: the prompt stays open and says why.
    Refuse(String),
}

/// What the add field says of a blank answer.
pub const TYPE_A_NAME: &str = "type a name";

/// The step `text` (trimmed) leads to from `prompt`. `members` are the
/// shown list's, for the steps that read them.
pub fn submit(prompt: &Prompt, text: &str, _members: &[Member]) -> Step {
    let text = text.trim();
    match prompt {
        Prompt::AddName if text.is_empty() => Step::Refuse(TYPE_A_NAME.into()),
        Prompt::AddName => Step::Add(vec![text.to_string()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
