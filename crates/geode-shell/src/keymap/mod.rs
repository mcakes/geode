//! Keymap engine (spec §3.4): keystroke parsing, context predicates,
//! layered binding resolution, and the sequence-aware matcher.
//! Pure logic — no gpui.

mod build;
mod context;
mod keystroke;

pub use build::{Binding, Keymap, UNBOUND_ACTION, build_keymap};
pub use context::{KeyContext, Predicate, parse_predicate};
pub use keystroke::{Keystroke, Modifiers, parse_binding, parse_keystroke};
