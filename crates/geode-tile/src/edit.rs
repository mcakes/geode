//! Initial caret placement for a tile's text cell editor.

use gpui::{Context, SharedString, Window};
use gpui_component::input::InputState;

/// Where typing begins when a text cell opens for editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditCaret {
    Start,
    End,
}

impl EditCaret {
    /// Seed an editor without selecting or changing the cell's text.
    /// The tile retains ownership of focus and the editor's lifetime.
    pub fn seed(
        self,
        input: &mut InputState,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<InputState>,
    ) {
        input.set_value(text, window, cx);
        if self == Self::Start {
            input.set_selected_range(0..0, cx);
        }
    }
}
