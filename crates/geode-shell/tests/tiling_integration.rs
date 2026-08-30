//! Keyboard to layout, end to end: builtin keymap → Matcher → ActionId →
//! apply_workspace_action → Tree geometry. This is the exact pipeline
//! Phase 1b-ui wires into gpui's key handler.

use geode_core::config::LayerDoc;
use geode_shell::actions::ActionRegistry;
use geode_shell::defaults;
use geode_shell::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};
use geode_shell::tiling::{DockSide, FocusRegion, Rect, Workspaces, apply_workspace_action};

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
    // Presses a single (non-sequence) keystroke and expects it to resolve
    // to a matched action immediately.
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

    // ctrl+v twice: two tiles side by side (workspace::split_right).
    press(&mut matcher, &mut ws, "ctrl+v");
    press(&mut matcher, &mut ws, "ctrl+v");
    assert_eq!(ws.active().tree().tiles().len(), 2);

    // mod+h: focus left tile (direct binding);
    // ctrl+h: split it stacked (workspace::split_down).
    press(&mut matcher, &mut ws, "mod+h");
    press(&mut matcher, &mut ws, "ctrl+h");
    assert_eq!(ws.active().tree().tiles().len(), 3);
    let rects = ws.active().tree().layout(Rect::UNIT);
    assert_eq!(rects.len(), 3);

    // mod+f: fullscreen the focused tile — only one visible.
    press(&mut matcher, &mut ws, "mod+f");
    assert_eq!(ws.active().tree().layout(Rect::UNIT).len(), 1);
    press(&mut matcher, &mut ws, "mod+f");

    // mod+2: switch to an empty workspace; mod+1: back with tiles intact.
    press(&mut matcher, &mut ws, "mod+2");
    assert!(ws.active().is_empty());
    press(&mut matcher, &mut ws, "mod+1");
    assert_eq!(ws.active().tree().tiles().len(), 3);

    // Directional focus works through the same pipeline (mod+l).
    let before = ws.active().tree().focused();
    press(&mut matcher, &mut ws, "mod+l");
    assert_ne!(ws.active().tree().focused(), before);
    assert!(
        ws.active()
            .tree()
            .neighbor(geode_shell::tiling::Direction::Left)
            .is_some()
    );

    // Move-tile is the direct ctrl+shift+arrow binding
    // (ctrl+shift+left = workspace::move_left), same pipeline. (The builtin
    // keymap has no sequence bindings anymore; sequence matching itself is
    // covered by keymap_integration's desk-layer "g g" binding.)
    let layout_before = ws.active().tree().layout(Rect::UNIT);
    press(&mut matcher, &mut ws, "ctrl+shift+left");
    assert_ne!(ws.active().tree().layout(Rect::UNIT), layout_before);

    // mod+e: toggle the focused tile's parent split orientation — geometry
    // changes, tile count doesn't.
    let layout_before = ws.active().tree().layout(Rect::UNIT);
    press(&mut matcher, &mut ws, "mod+e");
    assert_ne!(ws.active().tree().layout(Rect::UNIT), layout_before);
    assert_eq!(
        ws.active().tree().layout(Rect::UNIT).len(),
        layout_before.len()
    );
}

/// Dock-regions task: the dock verbs run through the exact same pipeline.
/// The keystroke specs here are the ones BUILTIN_KEYMAP really binds —
/// `ctrl+{`, not `ctrl+shift+[`: real platform events arrive as the
/// shifted character with shift cleared (see BUILTIN_KEYMAP's doc
/// comment), and `parse_keystroke("ctrl+{")` produces that same shape.
#[test]
fn keystrokes_drive_the_docks() {
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

    // Two tiles; ctrl+{ parks the focused one in the left dock.
    press(&mut matcher, &mut ws, "ctrl+v");
    press(&mut matcher, &mut ws, "ctrl+v");
    press(&mut matcher, &mut ws, "ctrl+{");
    assert!(!ws.active().docks().get(DockSide::Left).tree().is_empty());
    assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    assert_eq!(ws.active().tree().tiles().len(), 1);

    // ctrl+h splits *inside* the focused dock (dock-trees task): the
    // dock's tree gains a second, stacked tile through the same pipeline.
    press(&mut matcher, &mut ws, "ctrl+h");
    assert_eq!(
        ws.active().docks().get(DockSide::Left).tree().tiles().len(),
        2
    );
    assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));

    // ctrl+? relocates the dock's focused tile dock-to-dock (left →
    // bottom); the other tile stays in the still-visible left dock.
    press(&mut matcher, &mut ws, "ctrl+?");
    assert_eq!(
        ws.active().docks().get(DockSide::Left).tree().tiles().len(),
        1
    );
    assert!(ws.active().docks().get(DockSide::Left).visible());
    assert!(!ws.active().docks().get(DockSide::Bottom).tree().is_empty());
    assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Bottom));

    // shift+up grows the focused bottom dock (lone tile inside: no
    // divider to move, so the press falls through to the frame).
    let before = ws.active().docks().get(DockSide::Bottom).size();
    press(&mut matcher, &mut ws, "shift+up");
    assert!(ws.active().docks().get(DockSide::Bottom).size() > before);

    // ctrl+/ hides it (tree kept; focus falls back to Main — the main
    // tree is non-empty, the only end-to-end pin of the hide-fallback
    // path); ctrl+/ shows it again.
    press(&mut matcher, &mut ws, "ctrl+/");
    assert!(!ws.active().docks().get(DockSide::Bottom).visible());
    assert!(!ws.active().docks().get(DockSide::Bottom).tree().is_empty());
    assert_eq!(ws.active().region(), FocusRegion::Main);
    press(&mut matcher, &mut ws, "ctrl+/");
    assert!(ws.active().docks().get(DockSide::Bottom).visible());

    // ctrl+] shows the (empty) right dock; ctrl+] hides it again.
    press(&mut matcher, &mut ws, "ctrl+]");
    assert!(ws.active().docks().get(DockSide::Right).visible());
    press(&mut matcher, &mut ws, "ctrl+]");
    assert!(!ws.active().docks().get(DockSide::Right).visible());
}
