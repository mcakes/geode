//! Keymap engine (spec §3.4): keystroke parsing, context predicates,
//! layered binding resolution, and the sequence-aware matcher.
//! Pure logic — no gpui.

mod build;
mod context;
pub mod fragments;
mod keystroke;
mod matcher;

pub use build::{Binding, Keymap, UNBOUND_ACTION, build_keymap};
pub use context::{COUNTS, KeyContext, Predicate, parse_predicate};
pub use keystroke::{Keystroke, Modifiers, parse_binding, parse_keystroke};
pub use matcher::{MAX_COUNT, MatchResult, Matcher};
