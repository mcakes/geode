//! The one door a key reaches the screen through: dialog footer hints,
//! keybinding rows, tooltips, the command palette's binding column, the
//! which-key overlay, the status bar's pending keys and a module's hint
//! rows all paint gpui-component's `Kbd` from here.
//!
//! `Kbd` owns the whole presentation: the label (platform glyphs on
//! macOS, `Ctrl+Shift+P` elsewhere, the key capitalised), the muted fill,
//! the radius and the type. Nothing here restyles it, so every key in the
//! app reads the same. Text that NAMES a key inside a sentence (a notice,
//! a help line, the keymap files) keeps the keymap's own lowercase
//! spelling, which is what the trader types into those files.

use gpui::prelude::*;
use gpui::{AnyElement, Div, SharedString};
use gpui_component::h_flex;
use gpui_component::kbd::Kbd;

use super::keys::to_gpui_keystroke;
use crate::keymap::{Keystroke, Modifiers, parse_binding};

/// One keystroke as a `Kbd` chip.
pub fn chip(keystroke: &Keystroke) -> Kbd {
    Kbd::new(to_gpui_keystroke(keystroke))
}

/// A binding (one keystroke or a sequence such as `g g`) as a row of
/// chips, one per keystroke.
pub fn binding(keystrokes: &[Keystroke]) -> Div {
    h_flex()
        .gap_0p5()
        .items_center()
        .flex_shrink_0()
        .children(keystrokes.iter().map(chip))
}

/// A hint verb beside its binding: the chips, then the word.
pub fn hint(keystrokes: &[Keystroke], word: impl Into<SharedString>) -> Div {
    h_flex()
        .gap_1()
        .items_center()
        .flex_shrink_0()
        .child(binding(keystrokes))
        .child(word.into())
}

/// A hint written in the keymap's own spelling, as a module menu stores
/// it: `u`, `g p` or `ctrl+r` paint as chips; a command-line verb such
/// as `:upload` is not a key and stays text. An empty spec paints
/// nothing. `mod` is not expanded — a hint names the shipped key, and a
/// hint that cannot parse falls back to its text rather than hiding.
pub fn spec(spec: &str) -> AnyElement {
    match spec_keys(spec) {
        Some(keys) => binding(&keys).into_any_element(),
        None => SharedString::from(spec.to_string()).into_any_element(),
    }
}

/// [`spec`]'s decision: the keys a hint names, or `None` for text.
fn spec_keys(spec: &str) -> Option<Vec<Keystroke>> {
    if spec.is_empty() || spec.starts_with(':') {
        return None;
    }
    parse_binding(spec, Modifiers::NONE).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_names_keys_unless_it_is_a_command_verb() {
        assert_eq!(spec_keys(""), None);
        assert_eq!(spec_keys(":upload"), None);
        assert_eq!(spec_keys("ctrl+"), None, "an unparsable spec stays text");
        let keys = spec_keys("g p").expect("a sequence is keys");
        assert_eq!(keys.len(), 2);
        assert_eq!(
            spec_keys("ctrl+r").expect("a chord is keys")[0].mods,
            Modifiers::CTRL
        );
    }
}
