//! The dialog's layers: which screen or step shows, and where a commit or
//! escape leads. The bottom layer is what the door opened. A door that opens
//! a step directly (`mod+p`, `mod+x`, a chip) makes that step the bottom, so
//! its commit or escape empties the stack and the dialog closes: those doors
//! stay one-shot. A step pushed over a screen returns to that screen, so a
//! visit that started on the screen can build several ingredients.
//!
//! Doors: `mod+o` and `+` open `Current`. The saved door
//! (`frame::scope_saved`, `config::scopes`, `config::expressions`) opens the
//! same screen with the cursor in the saved group and records
//! `Opener::Saved`. A `≡` chip opens `Step(Definition)`; the `save` chip and
//! `scope::save_current` open `Step(SaveScope)`. The dimension and expression
//! steps are their own dialogs (the picker and the frame expression dialog),
//! pushed over the Scope dialog rather than layered inside it.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    Text,
    /// A saved expression's definition; `None` is a new one.
    Definition {
        name: Option<String>,
    },
    /// Naming a new expression whose text was just accepted.
    NameExpression {
        text: String,
    },
    /// Naming the lane's scope to save it.
    SaveScope,
    /// Naming a copy of a saved definition.
    CopyName {
        from: super::saved::SavedId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Layer {
    /// The one screen: the current scope's sections, then the saved ones.
    Current,
    Step(Step),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum After {
    Show,
    Close,
}

/// Which door opened the dialog. A load or toggle on a saved row closes the
/// dialog when the saved door opened it
/// (`frame::scope_saved`, `config::scopes`, `config::expressions`): that door
/// is one-shot. Opened any other way, the commit stays on the screen, so a
/// visit from `mod+o` sees what it changed in the current sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opener {
    Scope,
    Saved,
}

impl Opener {
    pub(crate) fn closes_on_saved_commit(self) -> bool {
        self == Opener::Saved
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Layers {
    /// Never empty while the dialog is open.
    layers: Vec<Layer>,
}

impl Layers {
    pub(crate) fn open(first: Layer) -> Layers {
        Layers {
            layers: vec![first],
        }
    }

    pub(crate) fn top(&self) -> &Layer {
        self.layers.last().expect("an open dialog has a layer")
    }

    pub(crate) fn depth(&self) -> usize {
        self.layers.len()
    }

    pub(crate) fn push(&mut self, layer: Layer) {
        self.layers.push(layer);
    }

    /// Swap the top layer without changing where its commit leads: a new
    /// expression's text step becomes its naming step and still returns to
    /// the screen beneath.
    pub(crate) fn replace_top(&mut self, layer: Layer) {
        if let Some(top) = self.layers.last_mut() {
            *top = layer;
        }
    }

    pub(crate) fn escape(&mut self) -> After {
        self.pop()
    }

    /// A step committed: it leaves, and whatever opened it shows again.
    pub(crate) fn commit_step(&mut self) -> After {
        debug_assert!(matches!(self.top(), Layer::Step(_)), "{:?}", self.top());
        self.pop()
    }

    fn pop(&mut self) -> After {
        self.layers.pop();
        if self.layers.is_empty() {
            After::Close
        } else {
            After::Show
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> Layer {
        Layer::Step(Step::Text)
    }

    #[test]
    fn only_the_saved_door_closes_on_a_saved_commit() {
        assert!(Opener::Saved.closes_on_saved_commit());
        assert!(!Opener::Scope.closes_on_saved_commit());
    }

    #[test]
    fn a_step_entered_from_current_returns_there_on_commit_and_on_escape() {
        let mut l = Layers::open(Layer::Current);
        l.push(text());
        assert_eq!(l.commit_step(), After::Show);
        assert_eq!(l.top(), &Layer::Current);
        l.push(Layer::Step(Step::Text));
        assert_eq!(l.escape(), After::Show);
        assert_eq!(l.top(), &Layer::Current);
        assert_eq!(l.escape(), After::Close);
    }

    #[test]
    fn a_one_shot_step_closes_on_commit_and_on_escape() {
        let mut l = Layers::open(text());
        assert_eq!(l.commit_step(), After::Close);
        let mut l = Layers::open(Layer::Step(Step::SaveScope));
        assert_eq!(l.escape(), After::Close);
    }

    #[test]
    fn a_definition_from_the_screen_returns_to_it() {
        let mut l = Layers::open(Layer::Current);
        l.push(Layer::Step(Step::Definition {
            name: Some("liq".into()),
        }));
        assert_eq!(l.commit_step(), After::Show);
        assert_eq!(l.top(), &Layer::Current);
        assert_eq!(l.depth(), 1);
    }

    #[test]
    fn a_new_expression_names_itself_in_place_then_returns_to_the_screen() {
        let mut l = Layers::open(Layer::Current);
        l.push(Layer::Step(Step::Definition { name: None }));
        l.replace_top(Layer::Step(Step::NameExpression {
            text: "npv > 0".into(),
        }));
        assert_eq!(l.depth(), 2);
        assert_eq!(l.commit_step(), After::Show);
        assert_eq!(l.top(), &Layer::Current);
    }
}
