//! Compiled-in defaults: the builtin action set and keymap (spec §3.1).
//! These form the Builtin config layer; desk and user files override them.

use crate::actions::{ActionDef, ActionId, ActionRegistry};
use crate::keymap::Modifiers;
use geode_core::config::Config;

/// The builtin keymap document, layered under desk/user keymaps.
pub const BUILTIN_KEYMAP: &str = r#"
[[bindings]]
context = "workspace"
[bindings.keys]
"mod+h" = "workspace::focus_left"
"mod+j" = "workspace::focus_down"
"mod+k" = "workspace::focus_up"
"mod+l" = "workspace::focus_right"
"mod+v" = "workspace::split_vertical"
"mod+s" = "workspace::split_horizontal"
"mod+f" = "workspace::fullscreen_tile"
"mod+shift+q" = "workspace::close_tile"
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
"mod+p" = "palette::toggle"
"ctrl+shift+p" = "palette::toggle"
"mod+shift+t" = "theme::toggle_mode"
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
    action(
        reg,
        "workspace::split_vertical",
        "Split vertical",
        "Workspace",
    );
    action(
        reg,
        "workspace::split_horizontal",
        "Split horizontal",
        "Workspace",
    );
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
        assert!(keymap.bindings().len() >= 18);
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
