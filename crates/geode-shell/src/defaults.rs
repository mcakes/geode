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
/// Move-tile is `ctrl+alt+arrows` (user direction, superseding the earlier
/// `ctrl+shift+arrows`) and resize is `shift+arrows` (user direction,
/// retiring the Phase 1c `ctrl+w shift+h/j/k/l` vim window prefix and
/// `shift+h/j/k/l`) — the builtin keymap now has no sequence
/// bindings at all; sequences remain a first-class engine feature for
/// desk/user layers. Splits follow vim's own mnemonics —
/// `ctrl+v` is `:vsplit` (side by side), `ctrl+h` is `:split` (stacked) —
/// which is why the actions they bind to are named by resulting geometry
/// (`split_right`/`split_down`) rather than by vim verb: naming them
/// `split_vertical`/`split_horizontal` would read backwards against these
/// keys. Resize is a direct binding, not a mode: `shift+arrows` move the
/// divider adjacent to the focused tile toward the arrow's direction by
/// `RESIZE_STEP` (the key names the divider's direction, not
/// "grow"; see [`crate::tiling::Tree::move_divider`] for the edge-flip
/// consequence when the focused tile has no divider on that side).
///
/// Docks (dock-regions task): `ctrl+[` / `ctrl+]` / `ctrl+/` toggle the
/// left/right/bottom dock; the *move*-to-dock verbs are the same physical
/// keys with shift held — which the user thinks of as `ctrl+shift+[` etc.,
/// but which are deliberately bound as `ctrl+{` / `ctrl+}` / `ctrl+?`
/// (shifted character, NO shift modifier). Verified against the pinned
/// platform sources, not assumed: both macOS (`gpui_macos/src/events.rs`,
/// the `else if shift { shift = false; chars_with_shift }` arm) and Windows
/// (`gpui_windows/src/keyboard.rs`, `get_keystroke_key`'s
/// `need_to_convert_to_shifted_key` OEM-key list) deliver shift+punctuation
/// as the shifted character with the shift modifier *cleared* — a real
/// `KeyDownEvent` for shift+[ arrives as key `{`, `shift: false`, so a
/// `"ctrl+shift+["` binding would never match anything. (Letters are the
/// opposite: a shifted letter stays the letter + shift modifier, so a
/// shifted-letter chord would be bound with the modifier spelled out —
/// as would a shifted *arrow*, which is why `shift+arrows` resize binds
/// shift as a modifier.) The e2e dock tests dispatch `ctrl-{`
/// through gpui's real pipeline to pin this shape.
///
/// Close-tile is `ctrl+w` (user direction — the browser/vim close idiom;
/// free since the vim window prefix retired, and unclaimed by macOS).
pub const BUILTIN_KEYMAP: &str = r#"
[[bindings]]
context = "workspace"
[bindings.keys]
"mod+h" = "workspace::focus_left"
"mod+j" = "workspace::focus_down"
"mod+k" = "workspace::focus_up"
"mod+l" = "workspace::focus_right"
"ctrl+alt+left" = "workspace::move_left"
"ctrl+alt+down" = "workspace::move_down"
"ctrl+alt+up" = "workspace::move_up"
"ctrl+alt+right" = "workspace::move_right"
"shift+left" = "workspace::resize_left"
"shift+down" = "workspace::resize_down"
"shift+up" = "workspace::resize_up"
"shift+right" = "workspace::resize_right"
"ctrl+v" = "workspace::split_right"
"ctrl+h" = "workspace::split_down"
"mod+e" = "workspace::toggle_split_orientation"
"mod+f" = "workspace::fullscreen_tile"
"ctrl+w" = "workspace::close_tile"
"ctrl+[" = "dock::toggle_left"
"ctrl+]" = "dock::toggle_right"
"ctrl+/" = "dock::toggle_bottom"
"ctrl+{" = "dock::move_left"
"ctrl+}" = "dock::move_right"
"ctrl+?" = "dock::move_bottom"
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
"ctrl+=" = "fontsize::increase"
"ctrl+-" = "fontsize::decrease"
"mod+shift+p" = "perf::toggle_overlay"
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
    // Pairwise reorient around the focused tile (see
    // `Tree::toggle_split_orientation` — deliberately not i3's
    // whole-container toggle), bound mod+e (i3's layout-toggle key).
    action(
        reg,
        "workspace::toggle_split_orientation",
        "Toggle split orientation",
        "Workspace",
    );
    action(
        reg,
        "workspace::fullscreen_tile",
        "Fullscreen tile",
        "Workspace",
    );
    action(reg, "workspace::close_tile", "Close tile", "Workspace");
    // Dock regions (dock-regions task): toggle shows/hides a dock (a
    // hidden dock keeps its tile); move sends the focused tile there —
    // or back into the tree when it's already the focused dock. See
    // `tiling::Workspace` for the full verb semantics and BUILTIN_KEYMAP's
    // doc comment for why the move bindings are spelled `ctrl+{` etc.
    action(reg, "dock::toggle_left", "Toggle left dock", "Dock");
    action(reg, "dock::toggle_right", "Toggle right dock", "Dock");
    action(reg, "dock::toggle_bottom", "Toggle bottom dock", "Dock");
    action(reg, "dock::move_left", "Move tile to left dock", "Dock");
    action(reg, "dock::move_right", "Move tile to right dock", "Dock");
    action(reg, "dock::move_bottom", "Move tile to bottom dock", "Dock");
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
    // Step the UI font size (crate::fontsize, clamped small..=large).
    // ctrl+= / ctrl+- — the browser-zoom idiom: the unshifted key next to
    // backspace is `=`, and platforms deliver ctrl+that-key as key "="
    // (see the shift+punctuation note on BUILTIN_KEYMAP's doc comment for
    // why "ctrl++" would be both unpressable-without-shift and unparseable
    // — parse_keystroke splits on '+'). Same "Appearance" category as the
    // settings dialog that owns the equivalent toggle group.
    action(
        reg,
        "fontsize::increase",
        "Increase font size",
        "Appearance",
    );
    action(
        reg,
        "fontsize::decrease",
        "Decrease font size",
        "Appearance",
    );
    // The keybinding dialog (Part B). Palette-only by design: no key
    // binding of its own in BUILTIN_KEYMAP — bootstrapping a dialog whose
    // whole purpose is showing/editing keybindings out of a keybinding
    // would be a little too cute, and the palette is always reachable
    // regardless. Category "Keyboard", not "Appearance" — this edits
    // behavior (bindings), not how the app looks.
    action(reg, "keybindings::open", "Keyboard shortcuts", "Keyboard");
    // Frame-time instrumentation (spec §7.4). The overlay toggle is bound
    // `mod+shift+p` ("performance" — a shifted letter keeps its modifier,
    // unlike the punctuation story above, so this spelling is real, and no
    // builtin binding claims it under the default mod). Known remap hazard,
    // shared with every mod+letter chord (e.g. mod+h vs ctrl+h): under
    // `keymap.mod = "ctrl"` this aliases onto `ctrl+shift+p`, and the
    // matcher's last-exact-match rule lets it shadow the palette's second
    // binding (ctrl+k still opens the palette). `perf::reset` is palette-only —
    // resetting counters is an occasional deliberate act, not muscle
    // memory worth a chord.
    action(
        reg,
        "perf::toggle_overlay",
        "Toggle performance overlay",
        "Diagnostics",
    );
    action(
        reg,
        "perf::reset",
        "Reset performance counters",
        "Diagnostics",
    );
    // Profiler-feature actions (the `profiling` feature — gpui's own
    // `profiler` histograms/overlay): registered only when compiled in,
    // so the palette never advertises a no-op.
    #[cfg(feature = "profiling")]
    {
        action(
            reg,
            "perf::gpui_overlay",
            "Cycle gpui frame overlay",
            "Diagnostics",
        );
        action(
            reg,
            "perf::dump",
            "Dump frame-time stats to stderr",
            "Diagnostics",
        );
    }
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
        // 4 focus + 4 move + 4 resize + 2 splits + orientation toggle +
        // fullscreen + close + 3 dock toggles + 3 dock moves + 9 workspace
        // switches + 2 palette::toggle bindings + theme toggle +
        // settings::open (Task 5) + 2 font size steps + perf overlay
        // toggle (spec §7.4).
        assert!(keymap.bindings().len() >= 39);
    }

    #[test]
    fn dock_actions_are_registered_with_the_dock_category() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        for id in [
            "dock::toggle_left",
            "dock::toggle_right",
            "dock::toggle_bottom",
            "dock::move_left",
            "dock::move_right",
            "dock::move_bottom",
        ] {
            let def = reg
                .get(&ActionId(id.to_string()))
                .unwrap_or_else(|| panic!("{id} not registered"));
            assert_eq!(def.category, "Dock", "{id}");
        }
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
