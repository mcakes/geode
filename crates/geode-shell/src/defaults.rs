//! Compiled-in shell actions and keymap defaults.
//! Desk and user bindings override these through the ordinary keymap compiler.

use crate::actions::{ActionDef, ActionId, ActionRegistry};
use crate::keymap::Modifiers;
use geode_core::config::{Config, Diagnostic, Severity};

/// Shell builtin bindings, followed by module fragments and desk/user layers.
/// Module-context bindings belong to module factories' default keymap fragments.
///
/// Focus uses `mod+h/j/k/l`; move-tile uses `ctrl+alt+arrows`; resize uses
/// `shift+arrows`. Resize moves the adjacent divider toward the arrow, including
/// [`crate::tiling::Tree::move_divider`]'s edge-flip behavior. Control-arrow focus
/// would conflict with macOS Mission Control/Spaces shortcuts.
///
/// Dock moves use `ctrl+{`, `ctrl+}`, and `ctrl+?`: the pinned macOS and Windows
/// backends deliver shifted punctuation as the shifted character with Shift
/// cleared. Letters and arrows retain an explicit Shift modifier instead.
///
/// The builtin document has no multi-key sequences; the engine supports them in
/// other layers. Exact ties use the last matching entry. Frame-slot bindings
/// precede workspace bindings, although the configured `mod` alias cannot be
/// Control and therefore cannot collide with the literal Control digit bindings.
pub const BUILTIN_KEYMAP: &str = r#"
[[bindings]]
[bindings.keys]
"ctrl+1" = "frame::slot_1"
"ctrl+2" = "frame::slot_2"
"ctrl+3" = "frame::slot_3"
"ctrl+4" = "frame::slot_4"
"ctrl+5" = "frame::slot_5"
"ctrl+6" = "frame::slot_6"
"ctrl+7" = "frame::slot_7"
"ctrl+8" = "frame::slot_8"
"ctrl+9" = "frame::slot_9"
"ctrl+0" = "frame::slot_clear"

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
"mod+e" = "workspace::toggle_split_orientation"
"mod+f" = "workspace::fullscreen_tile"
"ctrl+w" = "workspace::close_tile"
"shift+d" = "workspace::duplicate_horizontal"
"ctrl+shift+d" = "workspace::duplicate_vertical"
"ctrl+[" = "dock::toggle_left"
"ctrl+]" = "dock::toggle_right"
"ctrl+/" = "dock::toggle_bottom"
"mod+]" = "stack::next"
"mod+[" = "stack::prev"
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
"ctrl+," = "settings::open"
"ctrl+=" = "fontsize::increase"
"ctrl+-" = "fontsize::decrease"
"mod+shift+p" = "perf::toggle_overlay"
"mod+z" = "frame::scope_undo"
"mod+shift+z" = "frame::scope_redo"
"mod+/" = "frame::focus_text"
"mod+p" = "frame::pick"
"mod+t" = "frame::as_of"
"mod+g" = "frame::grouping"
"mod+n" = "tile::add"

[[bindings]]
context = "tile"
[bindings.keys]
":" = "tile::command_line"
"/" = "tile::find"

"#;

fn action(reg: &mut ActionRegistry, id: &str, title: &str, category: &str) {
    reg.register(ActionDef {
        id: ActionId(id.to_string()),
        title: title.to_string(),
        category: category.to_string(),
    })
    .expect("builtin action ids are unique by construction");
}

/// Register shell-owned actions. Factories register module-owned actions separately.
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
    // Reorient the pair around the focused tile through Tree::toggle_split_orientation.
    action(
        reg,
        "workspace::toggle_split_orientation",
        "Toggle split orientation",
        "Workspace",
    );
    // Duplicate the focused tile with its serialized state: horizontal places
    // it to the right, vertical places it below.
    action(
        reg,
        "workspace::duplicate_horizontal",
        "Duplicate tile horizontal",
        "Workspace",
    );
    action(
        reg,
        "workspace::duplicate_vertical",
        "Duplicate tile vertical",
        "Workspace",
    );
    action(
        reg,
        "workspace::fullscreen_tile",
        "Fullscreen tile",
        "Workspace",
    );
    action(reg, "workspace::close_tile", "Close tile", "Workspace");
    // Dock toggles retain hidden occupants. Move sends the focused tile to a
    // dock, or back to the tree when that dock is already focused.
    action(reg, "dock::toggle_left", "Toggle left dock", "Dock");
    action(reg, "dock::toggle_right", "Toggle right dock", "Dock");
    action(reg, "dock::toggle_bottom", "Toggle bottom dock", "Dock");
    action(reg, "dock::move_left", "Move tile to left dock", "Dock");
    action(reg, "dock::move_right", "Move tile to right dock", "Dock");
    action(reg, "dock::move_bottom", "Move tile to bottom dock", "Dock");
    // Cycle stack members, pick a member, or unstack it; pick/unstack are palette-only.
    action(reg, "stack::next", "Stack: Next", "Workspace");
    action(reg, "stack::prev", "Stack: Previous", "Workspace");
    action(reg, "stack::pick", "Stack: Pick…", "Workspace");
    action(reg, "stack::unstack", "Stack: Unstack", "Workspace");
    for i in 1..=9 {
        action(
            reg,
            &format!("workspace::switch_{i}"),
            &format!("Switch to workspace {i}"),
            "Workspace",
        );
    }
    action(reg, "palette::toggle", "Toggle command palette", "Palette");
    // Settings share the Appearance category with themes. A theme name
    // selects its light/dark presentation; there is no separate mode action.
    action(reg, "settings::open", "Open settings…", "Appearance");
    // Font size steps clamp to the supported range. `ctrl+=` uses the
    // unshifted key; literal `+` is the keystroke parser's separator.
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
    // Cycle `[ui] line_numbers` through off, on, and relative; palette-only.
    action(
        reg,
        "ui::line_numbers_cycle",
        "Cycle line numbers (off / on / relative)",
        "Appearance",
    );
    // Open the keybinding editor from the palette, under Keyboard.
    action(reg, "keybindings::open", "Keyboard shortcuts…", "Keyboard");
    // The palette groups named-object editors under Configuration.
    action(reg, "config::views", "Edit views…", "Configuration");
    // Edit the groupings assigned to the nine Control-digit slots.
    action(reg, "config::groupings", "Edit groupings…", "Configuration");
    // Manage saved scopes; their separate `scope::<name>` actions recall them.
    action(reg, "config::scopes", "Edit scopes…", "Configuration");
    // Inspect the read-only dataset schema used by configuration choices.
    action(reg, "config::schema", "Edit schema…", "Configuration");
    // Edit ingest sources, grouped by dataset.
    action(reg, "config::sources", "Edit sources…", "Configuration");
    // Edit named colors shared by column presentation and chart series.
    action(reg, "config::colors", "Edit colors…", "Configuration");
    reg.register_rename("config::colours", "config::colors")
        .expect("a retired id is never a registered one");
    // Edit named scope expressions, which saved scopes and the frame tick.
    action(
        reg,
        "config::expressions",
        "Edit expressions…",
        "Configuration",
    );
    // Open the user configuration directory in the OS file manager for edits
    // outside the dialogs. The title omits an ellipsis because this opens a folder.
    action(
        reg,
        "config::open_directory",
        "Open config directory",
        "Configuration",
    );
    // Toggle the frame-time overlay with `mod+shift+p`.
    // Counter reset is available through the palette.
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
    // Command and find prompts are shell actions in the occupied `tile`
    // context, so every module gets `:` and `/` without registering them.
    action(
        reg,
        "tile::command_line",
        "Open the tile command line",
        "Tile",
    );
    action(reg, "tile::find", "Find in tile", "Tile");
    // The tile picker lists roster kinds. Its id is outside the `tile::add_`
    // prefix so parse_add_action cannot mistake it for a specific module kind.
    action(reg, "tile::add", "Add a tile…", "Tiles");
    // Pulls the focused tile's launch context and lists the kinds that
    // accept it. Outside the `tile::add_` prefix, like `tile::add`.
    action(reg, "tile::open_with", "Open with context…", "Tiles");
    // Control-1 through Control-9 activate grouping slots; empty slots are
    // ignored. Control-0 restores each following tile's view-default grouping.
    for i in 1..=9 {
        action(
            reg,
            &format!("frame::slot_{i}"),
            &format!("Grouping slot {i}"),
            "Frame",
        );
    }
    action(
        reg,
        "frame::slot_clear",
        "Clear grouping slot (views' own grouping)",
        "Frame",
    );
    // Scope undo/redo traverse Frame's bounded history. Clearing the whole
    // scope is a separate palette action.
    action(reg, "frame::scope_undo", "Undo scope change", "Frame");
    action(reg, "frame::scope_redo", "Redo scope change", "Frame");
    action(reg, "frame::scope_clear", "Clear scope", "Frame");
    // Save the current frame scope through the Scopes dialog naming prompt.
    // Dispatch this id before matching the generic `scope::` prefix; otherwise
    // `save_current` would be interpreted as a scope name. That name is reserved
    // in scope configuration for the same reason.
    action(
        reg,
        "scope::save_current",
        "Scope: Save current as…",
        "Scope",
    );
    // Open the dimension picker at column selection. Per-column actions are
    // registered separately from the startup schema by register_pick_actions.
    action(reg, "frame::pick", "Pick a dimension…", "Frame");
    // The grouping picker lists filled slots and the view default with typeahead.
    action(reg, "frame::grouping", "Pick a grouping…", "Frame");
    // Focus the scope text field; its Input handles typing once focused.
    action(
        reg,
        "frame::focus_text",
        "Focus the scope text field",
        "Frame",
    );
    // Open the as-of selector with recent generation timestamps as presets.
    // Live mode and as-of undo are separate palette actions.
    action(reg, "frame::as_of", "Jump to a point in time…", "Frame");
    action(reg, "frame::live", "Return to live", "Frame");
    action(
        reg,
        "frame::as_of_undo",
        "Swap to the previous as of",
        "Frame",
    );
    // Open the frame-scope expression dialog through the palette.
    action(
        reg,
        "frame::scope_expression",
        "Set scope expression…",
        "Frame",
    );
    // Join a new expression to the current one with `and` (the toolbar's
    // add-a-filter menu reaches the same action), and drop the whole
    // expression layer undoably. Neither has a default chord.
    action(
        reg,
        "frame::add_expression",
        "Add scope expression…",
        "Frame",
    );
    action(
        reg,
        "frame::clear_expression",
        "Clear scope expression",
        "Frame",
    );
    // Choose a log target and level, then submit Diagnostics::request_level.
    action(reg, "log::level", "Set log level…", "Diagnostics");
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

/// Register `frame::pick_<column>` actions from the startup pickable columns.
/// Registration precedes keymap compilation so these ids are bindable. The action
/// registry is fixed at startup; newly pickable columns need restart for new ids.
pub fn register_pick_actions(reg: &mut ActionRegistry, columns: &[crate::shell::Pickable]) {
    for c in columns {
        action(
            reg,
            &format!("frame::pick_{}", c.column),
            &format!("Pick: {}", c.column),
            "Frame",
        );
    }
}

/// Register palette and keymap actions `scope::<name>` for startup saved scopes.
/// They are unbound by default. Reload updates saved scope contents, but newly
/// added names require restart to enter the action registry.
pub fn register_scope_actions(reg: &mut ActionRegistry, saved: &geode_core::scopes::SavedScopes) {
    for name in saved.keys() {
        let id = format!("scope::{name}");
        // A hand-written scope name may collide with a shell action. Keep the
        // existing action and skip this palette row rather than panic at startup.
        if reg.contains(&ActionId(id.clone())) {
            tracing::warn!(
                target: "geode::config",
                "scopes: '{name}' collides with an existing action id ({id}); no palette row for it"
            );
            continue;
        }
        action(reg, &id, &format!("Scope: {name}"), "Scope");
    }
}

/// Register four Tiles actions per module kind: default-direction split,
/// explicit horizontal/right split, explicit vertical/below split, and stack.
/// Ids are `tile::add_<kind>` with optional `_horizontal`, `_vertical`, or
/// `_stacked` suffixes. Registration precedes keymap compilation.
pub fn register_add_actions(reg: &mut ActionRegistry, kinds: &[&str]) {
    for kind in kinds {
        let title = capitalize(kind);
        action(
            reg,
            &format!("tile::add_{kind}"),
            &format!("{title}: Split"),
            "Tiles",
        );
        action(
            reg,
            &format!("tile::add_{kind}_horizontal"),
            &format!("{title}: Split Horizontal"),
            "Tiles",
        );
        action(
            reg,
            &format!("tile::add_{kind}_vertical"),
            &format!("{title}: Split Vertical"),
            "Tiles",
        );
        action(
            reg,
            &format!("tile::add_{kind}_stacked"),
            &format!("{title}: Stack"),
            "Tiles",
        );
    }
}

/// A kind's palette title (`blotter` → `Blotter`) — also the tile
/// picker's row text (`shell::choicedialog`), so the two spell a kind
/// the same way.
pub fn capitalize(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Place a tile in an explicit/default-direction split or the focused stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddPlacement {
    Split(Option<crate::tiling::Orientation>),
    Stacked,
}

/// Decode a `tile::add_*` action into kind and placement; reject empty kinds.
/// Placement suffixes are stripped first, so kind names ending in a reserved
/// suffix are ambiguous and cannot be represented as a default-direction add.
pub fn parse_add_action(id: &str) -> Option<(&str, AddPlacement)> {
    use crate::tiling::Orientation;
    let rest = id.strip_prefix("tile::add_")?;
    let (kind, placement) = if let Some(k) = rest.strip_suffix("_horizontal") {
        (k, AddPlacement::Split(Some(Orientation::Horizontal)))
    } else if let Some(k) = rest.strip_suffix("_vertical") {
        (k, AddPlacement::Split(Some(Orientation::Vertical)))
    } else if let Some(k) = rest.strip_suffix("_stacked") {
        (k, AddPlacement::Stacked)
    } else {
        (rest, AddPlacement::Split(None))
    };
    (!kind.is_empty()).then_some((kind, placement))
}

/// Default primary modifier: Alt.
pub fn default_mod() -> Modifiers {
    Modifiers::ALT
}

/// Resolve `app.toml`'s `[keymap] mod`: `alt` or `cmd`.
/// `ctrl` returns an error and the Alt fallback because it collides with shipped
/// literal Control bindings. Missing, non-string, and other string values silently
/// fall back to Alt. Values are case-sensitive.
pub fn mod_alias_from_config(config: &Config) -> (Modifiers, Vec<Diagnostic>) {
    match config.get("app", "keymap.mod").and_then(|v| v.as_str()) {
        Some("ctrl") => (
            default_mod(),
            vec![Diagnostic {
                severity: Severity::Error,
                layer: config.explain("app", "keymap.mod"),
                file: None,
                message: "app: keymap.mod = \"ctrl\" is not allowed — ctrl is reserved for the \
                          shipped literal bindings (ctrl+1..9, ctrl+0, ctrl+k, ctrl+/ …); \
                          use \"alt\" or \"cmd\""
                    .into(),
                path: None,
            }],
        ),
        Some("cmd") => (Modifiers::CMD, Vec::new()),
        Some("alt") => (Modifiers::ALT, Vec::new()),
        _ => (default_mod(), Vec::new()),
    }
}

/// Warn when the ignored `app.toml` key `modules.default` is present.
/// Tiles are created by explicit module kind.
pub fn modules_default_diagnostic(config: &Config) -> Option<Diagnostic> {
    config.get("app", "modules.default").map(|_| Diagnostic {
        severity: Severity::Warning,
        layer: config.explain("app", "modules.default"),
        file: None,
        message: "app: modules.default is no longer read — tiles are added by kind \
                  (ctrl+k → Add …); remove the key"
            .into(),
        path: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};
    use geode_core::config::{ConfigSources, LayerDoc};

    /// Resolve a single keystroke spec (e.g. `"ctrl+1"`) against
    /// `BUILTIN_KEYMAP` built with `mod_alias`, in the always-active
    /// `workspace` context — a pure, no-window rerun of exactly what
    /// `Matcher::press` does on a real keypress.
    fn resolve(mod_alias: Modifiers, spec: &str) -> MatchResult {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], mod_alias, &reg);
        assert!(diags.is_empty(), "{diags:?}");
        let keystroke = parse_keystroke(spec, mod_alias).unwrap();
        Matcher::default().press(&keymap, keystroke, &[KeyContext::new("workspace")])
    }

    /// A `Config` with only a builtin `app` doc holding `text` — the
    /// minimal fixture `mod_alias_from_config`'s own tests need.
    fn config_from_app(text: &str) -> Config {
        Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", text).unwrap()],
            desk: None,
            user: None,
        })
    }

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
        // The builtin document contains the complete shell binding set.
        assert!(keymap.bindings().len() >= 39);
    }

    /// A user binding to the old `config::colours` id still opens the Colors
    /// dialog: it binds `config::colors`, with a warning naming both ids.
    #[test]
    fn a_user_binding_to_the_old_colours_action_binds_config_colors() {
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let user = LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".into(),
            file: "user/keymap.toml".into(),
            table: "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+shift+k\" = \"config::colours\"\n"
                .parse()
                .unwrap(),
        };
        let (keymap, diags) = build_keymap(&[user], default_mod(), &reg);
        assert_eq!(keymap.bindings().len(), 1, "{diags:?}");
        assert_eq!(keymap.bindings()[0].action.0, "config::colors");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("config::colours"), "{diags:?}");
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

    /// Opening the configuration folder is a Configuration action without an ellipsis.
    #[test]
    fn open_config_directory_is_registered_under_configuration_without_ellipsis() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let def = reg
            .get(&ActionId("config::open_directory".to_string()))
            .expect("config::open_directory registered");
        assert_eq!(def.category, "Configuration");
        assert_eq!(def.title, "Open config directory");
    }

    /// A configured scope cannot replace a shell action with the same id.
    /// Other scopes still register after a collision is skipped.
    #[test]
    fn register_scope_actions_skips_a_collision_and_does_not_panic() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        let mut saved = geode_core::scopes::SavedScopes::new();
        saved.insert(
            "save_current".to_string(),
            geode_core::scope::Scope::default(),
        );
        saved.insert("eu".to_string(), geode_core::scope::Scope::default());

        register_scope_actions(&mut reg, &saved); // must not panic

        let builtin = reg
            .get(&ActionId("scope::save_current".to_string()))
            .expect("the builtin action must still be registered");
        assert_eq!(
            builtin.title, "Scope: Save current as…",
            "the collision must not overwrite the builtin action's own title"
        );
        assert!(
            reg.contains(&ActionId("scope::eu".to_string())),
            "a saved scope with no colliding id still gets its own row"
        );
    }

    #[test]
    fn mod_alias_read_from_config_with_fallback() {
        let empty = Config::load(&ConfigSources::default());
        let (alias, diags) = mod_alias_from_config(&empty);
        assert_eq!(alias, default_mod());
        assert!(diags.is_empty(), "{diags:?}");

        let bogus = config_from_app("[keymap]\nmod = \"hyper\"\n");
        let (alias, diags) = mod_alias_from_config(&bogus);
        assert_eq!(alias, default_mod());
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// Control is refused as the primary modifier to protect literal Control bindings.
    #[test]
    fn mod_alias_ctrl_is_refused_with_an_error_and_the_default_stands() {
        let config = config_from_app("[keymap]\nmod = \"ctrl\"\n");
        let (alias, diags) = mod_alias_from_config(&config);
        assert_eq!(alias, default_mod());
        let d = diags
            .iter()
            .find(|d| d.message.contains("keymap.mod"))
            .unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("ctrl"), "{}", d.message);
    }

    #[test]
    fn mod_alias_alt_and_cmd_are_accepted_without_diagnostics() {
        for (value, expect_ctrl) in [("alt", false), ("cmd", false)] {
            let (alias, diags) =
                mod_alias_from_config(&config_from_app(&format!("[keymap]\nmod = \"{value}\"\n")));
            assert!(diags.is_empty(), "{value}: {diags:?}");
            assert_eq!(alias.ctrl, expect_ctrl);
        }
    }

    #[test]
    fn modules_default_in_config_is_a_warning_and_absent_is_silent() {
        let none = Config::load(&ConfigSources::default());
        assert!(modules_default_diagnostic(&none).is_none());
        let set = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[modules]\ndefault = \"blotter\"\n").unwrap()],
            desk: None,
            user: None,
        });
        let diag = modules_default_diagnostic(&set).expect("a diagnostic");
        assert_eq!(diag.severity, Severity::Warning);
        assert!(diag.message.contains("modules.default"), "{}", diag.message);
        assert!(diag.message.contains("Add"), "{}", diag.message);
    }

    /// A raw Control alias bypasses config validation. Workspace switching
    /// still wins the digit tie because its entry follows the frame-slot entry.
    #[test]
    fn ctrl_1_resolves_to_the_shipped_workspace_switch_under_a_ctrl_mod_alias() {
        match resolve(Modifiers::CTRL, "ctrl+1") {
            MatchResult::Matched { action, .. } => {
                assert_eq!(action.0, "workspace::switch_1");
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// With the default Alt alias, Control-digit frame slots have no alias collision.
    #[test]
    fn ctrl_1_resolves_to_the_frame_slot_under_the_default_mod_alias() {
        match resolve(default_mod(), "ctrl+1") {
            MatchResult::Matched { action, .. } => {
                assert_eq!(action.0, "frame::slot_1");
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// Generic split actions are absent; tile additions identify the module kind.
    #[test]
    fn the_split_actions_are_gone() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        assert!(!reg.contains(&ActionId("workspace::split_right".into())));
        assert!(!reg.contains(&ActionId("workspace::split_down".into())));
        assert!(!BUILTIN_KEYMAP.contains("\"ctrl+v\""));
        assert!(!BUILTIN_KEYMAP.contains("\"ctrl+h\""));
    }

    #[test]
    fn register_add_actions_registers_four_rows_per_kind_in_the_tiles_category() {
        let mut reg = ActionRegistry::default();
        register_add_actions(&mut reg, &["blotter", "diagnostics"]);
        let expect = |id: &str, title: &str| {
            let def = reg
                .iter()
                .find(|d| d.id.0 == id)
                .unwrap_or_else(|| panic!("{id} not registered"));
            assert_eq!(def.title, title);
            assert_eq!(def.category, "Tiles");
        };
        expect("tile::add_blotter", "Blotter: Split");
        expect("tile::add_blotter_horizontal", "Blotter: Split Horizontal");
        expect("tile::add_blotter_vertical", "Blotter: Split Vertical");
        expect("tile::add_blotter_stacked", "Blotter: Stack");
        expect("tile::add_diagnostics", "Diagnostics: Split");
        assert_eq!(reg.iter().count(), 8);
        let mut empty = ActionRegistry::default();
        register_add_actions(&mut empty, &[]);
        assert_eq!(empty.iter().count(), 0);
    }

    #[test]
    fn parse_add_action_peels_the_direction_suffix_before_the_kind() {
        use crate::tiling::Orientation;
        assert_eq!(
            parse_add_action("tile::add_blotter"),
            Some(("blotter", AddPlacement::Split(None)))
        );
        assert_eq!(
            parse_add_action("tile::add_blotter_horizontal"),
            Some((
                "blotter",
                AddPlacement::Split(Some(Orientation::Horizontal))
            ))
        );
        assert_eq!(
            parse_add_action("tile::add_blotter_vertical"),
            Some(("blotter", AddPlacement::Split(Some(Orientation::Vertical))))
        );
        assert_eq!(
            parse_add_action("tile::add_blotter_stacked"),
            Some(("blotter", AddPlacement::Stacked))
        );
        assert_eq!(parse_add_action("tile::add_"), None);
        assert_eq!(parse_add_action("tile::add__vertical"), None);
        assert_eq!(parse_add_action("tile::command_line"), None);
        assert_eq!(parse_add_action("workspace::close_tile"), None);
    }

    #[test]
    fn duplicate_actions_are_registered_and_bound() {
        use crate::actions::ActionId;
        let mut reg = ActionRegistry::default();
        register_builtin_actions(&mut reg);
        assert!(reg.contains(&ActionId("workspace::duplicate_horizontal".into())));
        assert!(reg.contains(&ActionId("workspace::duplicate_vertical".into())));
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &reg);
        assert!(diags.is_empty(), "{diags:?}");
        let bound: Vec<String> = keymap
            .bindings()
            .iter()
            .map(|b| b.action.0.clone())
            .collect();
        assert!(bound.iter().any(|a| a == "workspace::duplicate_horizontal"));
        assert!(bound.iter().any(|a| a == "workspace::duplicate_vertical"));
    }
}
