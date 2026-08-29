//! Full 1a stack: config directories → layered docs → keymap → matcher.
//! Mirrors the real wiring Phase 1b will do in the app.

use geode_core::config::{Config, ConfigSources, LayerDoc};
use geode_shell::actions::{ActionId, ActionRegistry};
use geode_shell::defaults;
use geode_shell::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};

#[test]
fn desk_overrides_user_unbinds_and_sequences_work() {
    let desk = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    // Desk rebinds mod+h; adds a sequence binding.
    std::fs::write(
        desk.path().join("keymap.toml"),
        "config_version = 1\n\n[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::split_down\"\n\"g g\" = \"workspace::focus_up\"\n",
    )
    .unwrap();
    // User unbinds fullscreen. (Mod remapping is covered by defaults' unit
    // tests; the default Alt alias is used here.)
    std::fs::write(
        user.path().join("keymap.toml"),
        "config_version = 1\n\n[[bindings]]\n[bindings.keys]\n\"mod+f\" = \"none\"\n",
    )
    .unwrap();

    let config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", defaults::BUILTIN_KEYMAP).unwrap()],
        desk: Some(desk.path().to_path_buf()),
        user: Some(user.path().to_path_buf()),
    });
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);

    let mut registry = ActionRegistry::default();
    defaults::register_builtin_actions(&mut registry);
    let mod_alias = defaults::mod_alias_from_config(&config);
    let (keymap, diags) = build_keymap(config.layered_docs("keymap"), mod_alias, &registry);
    assert!(diags.is_empty(), "{diags:?}");

    let stack = vec![KeyContext::new("workspace")];
    let mut matcher = Matcher::default();
    let ks = |s: &str| parse_keystroke(s, mod_alias).unwrap();

    // Desk override beats builtin.
    assert_eq!(
        matcher.press(&keymap, ks("mod+h"), &stack),
        MatchResult::Matched(ActionId("workspace::split_down".into()))
    );
    // User unbind swallows the builtin binding.
    assert_eq!(
        matcher.press(&keymap, ks("mod+f"), &stack),
        MatchResult::NoMatch
    );
    // Untouched builtin binding still works — focus_down is the direct
    // mod+j binding (replacing the Phase 1c "ctrl+w j" chord).
    assert_eq!(
        matcher.press(&keymap, ks("mod+j"), &stack),
        MatchResult::Matched(ActionId("workspace::focus_down".into()))
    );
    // Move-tile is the direct ctrl+shift+arrow binding.
    assert_eq!(
        matcher.press(&keymap, ks("ctrl+shift+down"), &stack),
        MatchResult::Matched(ActionId("workspace::move_down".into()))
    );
    // Desk-added sequence: pending, then match.
    assert_eq!(
        matcher.press(&keymap, ks("g"), &stack),
        MatchResult::Pending
    );
    assert_eq!(
        matcher.press(&keymap, ks("g"), &stack),
        MatchResult::Matched(ActionId("workspace::focus_up".into()))
    );
}
