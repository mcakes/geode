//! The dialog's layers: which screen or step shows, and where a commit or
//! escape leads. The bottom layer is what the door opened. A door that opens
//! a step directly (`mod+p`, `mod+x`, a chip) makes that step the bottom, so
//! its commit or escape empties the stack and the dialog closes: those doors
//! stay one-shot. A step pushed over a screen returns to that screen, so a
//! visit that started on Current can build several ingredients.
//!
//! Doors: `mod+o` and `+` open `Current`; the load glyph,
//! `frame::scope_saved`, `config::scopes` and `config::expressions` open
//! `Saved`; `mod+p`, `frame::pick_book` and a dimension chip open
//! `Step(Dimension)`; `mod+x` opens `Step(AddExpression)`; a term chip opens
//! `Step(Term)`; a `≡` chip opens `Step(Definition)`; the `save` chip and
//! `scope::save_current` open `Step(SaveScope)`.

use geode_core::scope::Expr;

#[allow(dead_code)] // Pushed by the step doors, which the view does not route yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// Columns, or a known column's values.
    Dimension {
        column: Option<String>,
    },
    AddExpression,
    /// Editing term `index`; `seed` guards the index against the scope
    /// moving under the dialog.
    Term {
        index: usize,
        seed: Expr,
    },
    Text,
    /// A saved expression's definition; `None` is a new one.
    Definition {
        name: Option<String>,
    },
    /// Naming a new expression whose text was just accepted.
    NameExpression {
        text: String,
    },
    SaveScope,
}

#[allow(dead_code)] // Pushed by the step doors, which the view does not route yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Layer {
    Current,
    Saved,
    Step(Step),
}

#[allow(dead_code)] // Returned by escape and commits, which the view does not route yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum After {
    Show,
    Close,
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

    #[allow(dead_code)] // Called by the step and Saved routes, which the view lacks yet.
    pub(crate) fn depth(&self) -> usize {
        self.layers.len()
    }

    #[allow(dead_code)] // Called by the step and Saved routes, which the view lacks yet.
    pub(crate) fn push(&mut self, layer: Layer) {
        self.layers.push(layer);
    }

    /// Swap the top layer without changing where its commit leads: a new
    /// expression's text step becomes its naming step and still returns to
    /// the screen beneath.
    #[allow(dead_code)] // Called by the step and Saved routes, which the view lacks yet.
    pub(crate) fn replace_top(&mut self, layer: Layer) {
        if let Some(top) = self.layers.last_mut() {
            *top = layer;
        }
    }

    #[allow(dead_code)] // Called by the step and Saved routes, which the view lacks yet.
    pub(crate) fn escape(&mut self) -> After {
        self.pop()
    }

    /// A step committed: it leaves, and whatever opened it shows again.
    #[allow(dead_code)] // Called by the step and Saved routes, which the view lacks yet.
    pub(crate) fn commit_step(&mut self) -> After {
        debug_assert!(matches!(self.top(), Layer::Step(_)), "{:?}", self.top());
        self.pop()
    }

    /// A Saved row committed (a scope loaded, an expression toggled): the
    /// Saved screen leaves with it, back to Current when Saved was entered
    /// from there, else the dialog closes.
    #[allow(dead_code)] // Called by the step and Saved routes, which the view lacks yet.
    pub(crate) fn commit_saved_row(&mut self) -> After {
        debug_assert_eq!(self.top(), &Layer::Saved);
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

    fn dim() -> Layer {
        Layer::Step(Step::Dimension { column: None })
    }

    #[test]
    fn a_step_entered_from_current_returns_there_on_commit_and_on_escape() {
        let mut l = Layers::open(Layer::Current);
        l.push(dim());
        assert_eq!(l.commit_step(), After::Show);
        assert_eq!(l.top(), &Layer::Current);
        l.push(Layer::Step(Step::Text));
        assert_eq!(l.escape(), After::Show);
        assert_eq!(l.top(), &Layer::Current);
        assert_eq!(l.escape(), After::Close);
    }

    #[test]
    fn a_one_shot_step_closes_on_commit_and_on_escape() {
        let mut l = Layers::open(dim());
        assert_eq!(l.commit_step(), After::Close);
        let mut l = Layers::open(Layer::Step(Step::AddExpression));
        assert_eq!(l.escape(), After::Close);
    }

    #[test]
    fn a_saved_row_returns_to_current_when_saved_was_entered_from_it() {
        let mut l = Layers::open(Layer::Current);
        l.push(Layer::Saved);
        assert_eq!(l.commit_saved_row(), After::Show);
        assert_eq!(l.top(), &Layer::Current);
        l.push(Layer::Saved);
        assert_eq!(l.escape(), After::Show);
        assert_eq!(l.top(), &Layer::Current);
    }

    #[test]
    fn a_saved_row_closes_when_saved_was_opened_directly() {
        let mut l = Layers::open(Layer::Saved);
        assert_eq!(l.commit_saved_row(), After::Close);
    }

    #[test]
    fn a_definition_from_saved_returns_to_saved() {
        let mut l = Layers::open(Layer::Current);
        l.push(Layer::Saved);
        l.push(Layer::Step(Step::Definition {
            name: Some("liq".into()),
        }));
        assert_eq!(l.commit_step(), After::Show);
        assert_eq!(l.top(), &Layer::Saved);
        assert_eq!(l.depth(), 2);
    }

    #[test]
    fn a_new_expression_names_itself_in_place_then_returns_to_saved() {
        let mut l = Layers::open(Layer::Saved);
        l.push(Layer::Step(Step::Definition { name: None }));
        l.replace_top(Layer::Step(Step::NameExpression {
            text: "npv > 0".into(),
        }));
        assert_eq!(l.depth(), 2);
        assert_eq!(l.commit_step(), After::Show);
        assert_eq!(l.top(), &Layer::Saved);
    }
}
