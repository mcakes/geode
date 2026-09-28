//! The one door a key reaches the screen through: dialog footer hints,
//! keybinding rows, tooltips, the command palette's binding column, the
//! which-key overlay, the status bar's pending keys, empty-state and
//! section hints, and a module's menus and footers all paint
//! gpui-component's `Kbd` from here.
//!
//! `Kbd` owns the presentation: the label (platform glyphs on macOS,
//! `Ctrl+Shift+P` elsewhere, the key capitalised), the muted fill, the
//! radius and the type. The one restyle is a menu's trailing lane
//! ([`menu_binding`]), which paints keys the way gpui-component's own
//! `PopupMenu` does. Text that NAMES a key inside a sentence (a notice,
//! a confirmation, the keymap files) keeps the keymap's own lowercase
//! spelling, which is what the trader types into those files.

use gpui::prelude::*;
use gpui::{AnyElement, Div, Hsla, SharedString};
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

/// A binding in a menu row's trailing lane, painted as gpui-component's
/// `PopupMenu` paints its own: the chip's label without its fill, border
/// or padding, in the lane's `colour`, so the keys follow the row's
/// highlight instead of sitting in a muted box on the accent fill.
pub fn menu_binding(keystrokes: &[Keystroke], colour: Hsla) -> Div {
    h_flex()
        .gap_0p5()
        .items_center()
        .flex_shrink_0()
        .children(keystrokes.iter().map(|ks| {
            chip(ks)
                .p_0()
                .flex_nowrap()
                .border_0()
                .bg(gpui::transparent_white())
                .text_color(colour)
        }))
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
/// nothing. A spec naming `mod` stays text (the alias is the user's, so
/// no chip can name it honestly), and one that cannot parse falls back
/// to its text rather than hiding.
pub fn spec(spec: &str) -> AnyElement {
    match spec_keys(spec) {
        Some(keys) => binding(&keys).into_any_element(),
        None => SharedString::from(spec.to_string()).into_any_element(),
    }
}

/// A hint line that names keys inside prose: every backtick-quoted run
/// is a key spec painted through [`spec`], the rest is text, and the
/// pieces sit on one wrapping row. `"double-click or `ctrl+k` → Add a
/// tile"` paints the words, a `⌃K` chip, then the arrow and the rest.
/// Hint lines are short static strings, so splitting per paint costs a
/// handful of slices.
pub fn marked(text: &str) -> Div {
    h_flex()
        .gap_1()
        .items_center()
        .flex_wrap()
        .children(text.split('`').enumerate().filter_map(|(i, part)| {
            if i % 2 == 1 {
                return Some(self::spec(part));
            }
            let part = part.trim();
            (!part.is_empty()).then(|| SharedString::from(part.to_string()).into_any_element())
        }))
}

/// [`spec`]'s decision: the keys a hint names, or `None` for text.
fn spec_keys(spec: &str) -> Option<Vec<Keystroke>> {
    // `mod` is the user's configurable modifier; parsed against no alias
    // it would add nothing and paint a bare key, so it stays text.
    let names_mod = spec
        .split(|c: char| c == '+' || c.is_whitespace())
        .any(|part| part.eq_ignore_ascii_case("mod"));
    if spec.is_empty() || spec.starts_with(':') || names_mod {
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
        assert_eq!(
            spec_keys("mod+x"),
            None,
            "`mod` is not a key a chip can name"
        );
        let keys = spec_keys("g p").expect("a sequence is keys");
        assert_eq!(keys.len(), 2);
        assert_eq!(
            spec_keys("ctrl+r").expect("a chord is keys")[0].mods,
            Modifiers::CTRL
        );
    }
}
