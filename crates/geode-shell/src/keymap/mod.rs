//! Keymap engine (spec §3.4): keystroke parsing, context predicates,
//! layered binding resolution, and the sequence-aware matcher.
//! Pure logic — no gpui.

mod keystroke;

pub use keystroke::{Keystroke, Modifiers, parse_binding, parse_keystroke};
