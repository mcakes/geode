//! Keyboard to layout, end to end: builtin keymap → Matcher → ActionId →
//! apply_workspace_action → Tree geometry. This is the exact pipeline
//! Phase 1b-ui wires into gpui's key handler.

use geode_core::config::LayerDoc;
use geode_shell::actions::ActionRegistry;
use geode_shell::defaults;
use geode_shell::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};
use geode_shell::tiling::{Rect, Workspaces, apply_workspace_action};

#[test]
fn keystrokes_drive_the_tiling_tree() {
    let mut registry = ActionRegistry::default();
    defaults::register_builtin_actions(&mut registry);
    let doc = LayerDoc::builtin("keymap", defaults::BUILTIN_KEYMAP).unwrap();
    let mod_alias = defaults::default_mod();
    let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
    assert!(diags.is_empty(), "{diags:?}");

    let stack = vec![KeyContext::new("workspace")];
    let mut matcher = Matcher::default();
    let mut ws = Workspaces::new();
    let press = |matcher: &mut Matcher, ws: &mut Workspaces, key: &str| {
        let ks = parse_keystroke(key, mod_alias).unwrap();
        match matcher.press(&keymap, ks, &stack) {
            MatchResult::Matched(action) => {
                assert!(
                    apply_workspace_action(ws, &action),
                    "unhandled action {action}"
                );
            }
            other => panic!("expected a match for {key}, got {other:?}"),
        }
    };

    // mod+s twice: two tiles side by side.
    press(&mut matcher, &mut ws, "mod+s");
    press(&mut matcher, &mut ws, "mod+s");
    assert_eq!(ws.active().tiles().len(), 2);

    // mod+h: focus left tile; mod+v: split it vertically.
    press(&mut matcher, &mut ws, "mod+h");
    press(&mut matcher, &mut ws, "mod+v");
    assert_eq!(ws.active().tiles().len(), 3);
    let rects = ws.active().layout(Rect::UNIT);
    assert_eq!(rects.len(), 3);

    // mod+f: fullscreen the focused tile — only one visible.
    press(&mut matcher, &mut ws, "mod+f");
    assert_eq!(ws.active().layout(Rect::UNIT).len(), 1);
    press(&mut matcher, &mut ws, "mod+f");

    // mod+2: switch to an empty workspace; mod+1: back with tiles intact.
    press(&mut matcher, &mut ws, "mod+2");
    assert!(ws.active().is_empty());
    press(&mut matcher, &mut ws, "mod+1");
    assert_eq!(ws.active().tiles().len(), 3);

    // Directional focus works through the same pipeline.
    let before = ws.active().focused();
    press(&mut matcher, &mut ws, "mod+l");
    assert_ne!(ws.active().focused(), before);
    assert!(
        ws.active()
            .neighbor(geode_shell::tiling::Direction::Left)
            .is_some()
    );
}
