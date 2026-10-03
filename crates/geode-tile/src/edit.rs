//! Initial caret placement for a tile's text cell editor.

use gpui::{Context, SharedString, Window};
use gpui_component::input::InputState;

/// How a text cell's editor opens: caret after the text, or the whole
/// text selected so typing replaces it and one backspace clears it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditCaret {
    Select,
    End,
}

impl EditCaret {
    /// Seed an editor with the cell's text, unchanged.
    /// The tile retains ownership of focus and the editor's lifetime.
    pub fn seed(
        self,
        input: &mut InputState,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<InputState>,
    ) {
        input.set_value(text, window, cx);
        if self == Self::Select {
            let len = input.value().len();
            input.set_selected_range(0..len, cx);
        }
    }
}
