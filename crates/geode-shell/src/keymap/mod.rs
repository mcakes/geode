//! Keymap engine: keystroke parsing, context predicates, layered binding
//! resolution, and the sequence-aware matcher.
//! Pure logic — no gpui.

mod build;
mod context;
pub mod fragments;
mod keystroke;
mod matcher;

pub use build::{
    Binding, Keymap, UNBOUND_ACTION, UserOverride, build_keymap, effective_binding,
    user_overrides_for,
};
pub use context::{COUNTS, GRID, KeyContext, Predicate, TILELIST, parse_predicate};
pub use keystroke::{Keystroke, Modifiers, parse_binding, parse_keystroke};
pub use matcher::{MAX_COUNT, MatchResult, Matcher};
