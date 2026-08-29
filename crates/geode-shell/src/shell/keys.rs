//! Pure conversion from gpui's platform keystroke representation into the
//! shell's own [`crate::keymap::Keystroke`]. This is the only place the
//! shell touches `gpui::Keystroke`'s fields directly — everything downstream
//! (the matcher, the keymap) works in shell-native terms and stays testable
//! without a window.

use crate::keymap::{Keystroke, Modifiers};

/// Convert a `gpui::Keystroke` into the shell's keymap representation.
///
/// gpui's `Keystroke` already gives us what the keymap wants: `modifiers`
/// (control/alt/shift/platform) and a lowercase `key` string ("a", "enter",
/// "escape", digits, ...). We map platform's `control/alt/shift/platform`
/// onto the keymap's `ctrl/alt/shift/cmd` and pass `key` through unchanged.
///
/// Returns `None` for a bare-modifier press with no key of its own. gpui
/// delivers modifier-only presses as `ModifiersChangedEvent`, not
/// `KeyDownEvent`, so `on_key_down` should never see one in practice — but
/// the guard costs nothing and the keymap layer must never be handed a
/// keystroke with an empty (or modifier-named) key.
pub fn convert_keystroke(keystroke: &gpui::Keystroke) -> Option<Keystroke> {
    if keystroke.key.is_empty() || is_bare_modifier(&keystroke.key) {
        return None;
    }
    Some(Keystroke {
        mods: Modifiers {
            ctrl: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            shift: keystroke.modifiers.shift,
            cmd: keystroke.modifiers.platform,
        },
        key: keystroke.key.clone(),
    })
}

/// Key names a platform might (defensively) report for a bare modifier
/// press, rather than delivering it as a `ModifiersChangedEvent`.
fn is_bare_modifier(key: &str) -> bool {
    matches!(
        key,
        "control" | "ctrl" | "alt" | "shift" | "cmd" | "platform" | "super" | "win" | "fn"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `gpui::Keystroke` the way the platform layer would: public
    /// fields, no `key_char` needed for these mapping tests.
    fn gpui_keystroke(
        key: &str,
        control: bool,
        alt: bool,
        shift: bool,
        platform: bool,
    ) -> gpui::Keystroke {
        gpui::Keystroke {
            modifiers: gpui::Modifiers {
                control,
                alt,
                shift,
                platform,
                function: false,
            },
            key: key.to_string(),
            key_char: None,
        }
    }

    #[test]
    fn plain_key_passes_through_with_no_modifiers() {
        let converted =
            convert_keystroke(&gpui_keystroke("a", false, false, false, false)).unwrap();
        assert_eq!(converted.key, "a");
        assert_eq!(converted.mods, Modifiers::NONE);
    }

    #[test]
    fn control_maps_to_ctrl() {
        let converted = convert_keystroke(&gpui_keystroke("c", true, false, false, false)).unwrap();
        assert!(converted.mods.ctrl);
        assert!(!converted.mods.alt && !converted.mods.shift && !converted.mods.cmd);
    }

    #[test]
    fn alt_maps_to_alt() {
        let converted = convert_keystroke(&gpui_keystroke("s", false, true, false, false)).unwrap();
        assert!(converted.mods.alt);
    }

    #[test]
    fn shift_maps_to_shift() {
        let converted = convert_keystroke(&gpui_keystroke("g", false, false, true, false)).unwrap();
        assert!(converted.mods.shift);
    }

    #[test]
    fn platform_maps_to_cmd() {
        let converted = convert_keystroke(&gpui_keystroke("p", false, false, false, true)).unwrap();
        assert!(converted.mods.cmd);
    }

    #[test]
    fn combined_modifiers_all_map() {
        let converted = convert_keystroke(&gpui_keystroke("q", true, true, true, true)).unwrap();
        assert!(
            converted.mods.ctrl && converted.mods.alt && converted.mods.shift && converted.mods.cmd
        );
    }

    #[test]
    fn named_keys_and_digits_pass_through() {
        assert_eq!(
            convert_keystroke(&gpui_keystroke("enter", false, false, false, false))
                .unwrap()
                .key,
            "enter"
        );
        assert_eq!(
            convert_keystroke(&gpui_keystroke("escape", false, false, false, false))
                .unwrap()
                .key,
            "escape"
        );
        assert_eq!(
            convert_keystroke(&gpui_keystroke("1", false, false, false, false))
                .unwrap()
                .key,
            "1"
        );
    }

    #[test]
    fn empty_key_is_none() {
        assert!(convert_keystroke(&gpui_keystroke("", true, false, false, false)).is_none());
    }

    #[test]
    fn bare_modifier_key_names_are_none() {
        for key in ["control", "alt", "shift", "cmd", "fn"] {
            assert!(
                convert_keystroke(&gpui_keystroke(key, false, false, false, false)).is_none(),
                "expected None for bare modifier key '{key}'"
            );
        }
    }
}
