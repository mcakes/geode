//! Compiled-in defaults: the builtin action set and keymap (spec §3.1).
//! These form the Builtin config layer; desk and user files override them.

use crate::actions::{ActionDef, ActionId, ActionRegistry};
use crate::keymap::Modifiers;
use geode_core::config::Config;

/// The builtin keymap document, layered under desk/user keymaps.
///
/// Directional focus is `mod+h/j/k/l` (user direction — vim letters on
/// the primary modifier, replacing the Phase 1c `ctrl+w h/j/k/l` chords).
/// Not `ctrl+arrows`: macOS binds those to Mission Control/Spaces
/// system-wide and swallows the events before any app sees them.
/// Move-tile keeps the vim window
/// prefix `ctrl+w` as a two-keystroke sequence (`ctrl+w shift+h` etc.),
/// mirroring `<C-w>H`. Splits follow vim's own mnemonics —
/// `ctrl+v` is `:vsplit` (side by side), `ctrl+h` is `:split` (stacked) —
/// which is why the actions they bind to are named by resulting geometry
/// (`split_right`/`split_down`) rather than by vim verb: naming them
/// `split_vertical`/`split_horizontal` would read backwards against these
/// keys. Resize is a direct binding, not a mode: `shift+h/j/k/l` move the
/// divider adjacent to the focused tile toward that letter's direction by
/// `RESIZE_STEP` (vim's model — the key names the divider's direction, not
/// "grow"; see [`crate::tiling::Tree::move_divider`] for the edge-flip
/// consequence when the focused tile has no divider on that side).
pub const BUILTIN_KEYMAP: &str = r#"
[[bindings]]
context = "workspace"
[bindings.keys]
"mod+h" = "workspace::focus_left"
"mod+j" = "workspace::focus_down"
"mod+k" = "workspace::focus_up"
"mod+l" = "workspace::focus_right"
"ctrl+w shift+h" = "workspace::move_left"
"ctrl+w shift+j" = "workspace::move_down"
"ctrl+w shift+k" = "workspace::move_up"
"ctrl+w shift+l" = "workspace::move_right"
"shift+h" = "workspace::resize_left"
"shift+j" = "workspace::resize_down"
"shift+k" = "workspace::resize_up"
"shift+l" = "workspace::resize_right"
"ctrl+v" = "workspace::split_right"
"ctrl+h" = "workspace::split_down"
"mod+f" = "workspace::fullscreen_tile"
"ctrl+shift+w" = "workspace::close_tile"
"mod+1" = "workspace::switch_1"
"mod+2" = "workspace::switch_2"
"mod+3" = "workspace::switch_3"
"mod+4" = "workspace::switch_4"
"mod+5" = "workspace::switch_5"
"mod+6" = "workspace::switch_6"
"mod+7" = "workspace::switch_7"
"mod+8" = "workspace::switch_8"
"mod+9" = "workspace::switch_9"

[[bindings]]
[bindings.keys]
"ctrl+k" = "palette::toggle"
"ctrl+shift+p" = "palette::toggle"
"mod+shift+t" = "theme::toggle_mode"
"ctrl+," = "settings::open"
"#;

fn action(reg: &mut ActionRegistry, id: &str, title: &str, category: &str) {
    reg.register(ActionDef {
        id: ActionId(id.to_string()),
        title: title.to_string(),
        category: category.to_string(),
    })
    .expect("builtin action ids are unique by construction");
}

/// Register the shell's own actions. Modules register theirs at module
/// registration time (spec §9.1); these are the shell's.
pub fn register_builtin_actions(reg: &mut ActionRegistry) {
    action(reg, "workspace::focus_left", "Focus left", "Workspace");
    action(reg, "workspace::focus_down", "Focus down", "Workspace");
    action(reg, "workspace::focus_up", "Focus up", "Workspace");
    action(reg, "workspace::focus_right", "Focus right", "Workspace");
    action(reg, "workspace::move_left", "Move left", "Workspace");
    action(reg, "workspace::move_down", "Move down", "Workspace");
    action(reg, "workspace::move_up", "Move up", "Workspace");
    action(reg, "workspace::move_right", "Move right", "Workspace");
    action(
        reg,
        "workspace::resize_left",
        "Move split left",
        "Workspace",
    );
    action(
        reg,
        "workspace::resize_down",
        "Move split down",
        "Workspace",
    );
    action(reg, "workspace::resize_up", "Move split up", "Workspace");
    action(
        reg,
        "workspace::resize_right",
        "Move split right",
        "Workspace",
    );
    // Vim naming: split_right = Orientation::Horizontal (side by side, vim
    // :vsplit, bound ctrl+v); split_down = Orientation::Vertical (stacked,
    // vim :split, bound ctrl+h). See BUILTIN_KEYMAP's doc comment.
    action(reg, "workspace::split_right", "Split right", "Workspace");
    action(reg, "workspace::split_down", "Split down", "Workspace");
    action(
        reg,
        "workspace::fullscreen_tile",
        "Fullscreen tile",
        "Workspace",
    );
    action(reg, "workspace::close_tile", "Close tile", "Workspace");
    for i in 1..=9 {
        action(
            reg,
            &format!("workspace::switch_{i}"),
            &format!("Switch to workspace {i}"),
            "Workspace",
        );
    }
    action(reg, "palette::toggle", "Toggle command palette", "Palette");
    action(
        reg,
        "theme::toggle_mode",
        "Toggle light/dark theme",
        "Theme",
    );
    // The sidebar's bottom profile icon, ctrl+,, and the palette all
    // dispatch this (Task 5: the real settings dialog). Category
    // "Appearance" — not a standalone "Settings" category — groups it in
    // the palette alongside the theme rows, which share the same category
    // (`palette::THEME_CATEGORY`).
    action(reg, "settings::open", "Open settings", "Appearance");
    // The keybinding dialog (Part B). Palette-only by design: no key
    // binding of its own in BUILTIN_KEYMAP — bootstrapping a dialog whose
    // whole purpose is showing/editing keybindings out of a keybinding
    // would be a little too cute, and the palette is always reachable
    // regardless. Category "Keyboard", not "Appearance" — this edits
    // behavior (bindings), not how the app looks.
    action(reg, "keybindings::open", "Keyboard shortcuts", "Keyboard");
}

/// The default primary modifier (spec §3.1: Alt, remappable).
pub fn default_mod() -> Modifiers {
    Modifiers::ALT
}

/// Resolve the `mod` alias from config: doc `app`, key `keymap.mod`.
pub fn mod_alias_from_config(config: &Config) -> Modifiers {
    match config.get("app", "keymap.mod").and_then(|v| v.as_str()) {
        Some("ctrl") => Modifiers::CTRL,
        Some("cmd") => Modifiers::CMD,
        Some("alt") => Modifiers::ALT,
        _ => default_mod(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::build_keymap;
    use geode_core::config::{ConfigSources, LayerDoc};

    #[test]
    fn builtin_keymap_builds_clean_against_builtin_actions() {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &reg);
        assert!(
            diags.is_empty(),
            "builtin keymap must be diagnostic-free: {diags:?}"
        );
        // 4 focus + 4 move + 4 resize + 2 splits + fullscreen + close + 9
        // workspace switches + 2 palette::toggle bindings + theme toggle +
        // settings::open (Task 5).
        assert!(keymap.bindings().len() >= 29);
    }

    #[test]
    fn mod_alias_read_from_config_with_fallback() {
        let config = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[keymap]\nmod = \"ctrl\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(mod_alias_from_config(&config), Modifiers::CTRL);
        let empty = Config::load(&ConfigSources::default());
        assert_eq!(mod_alias_from_config(&empty), default_mod());
        let bogus = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[keymap]\nmod = \"hyper\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(mod_alias_from_config(&bogus), default_mod());
    }
}
