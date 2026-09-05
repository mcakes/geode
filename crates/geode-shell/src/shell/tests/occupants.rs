//! Occupant lifecycle: restoring, filling, ensuring, and the module
//! hosting contract's visibility handshake.

use super::*;
// Explicit: `tests`'s own `mod session;` declaration shadows the
// glob-imported `crate::session`, so the bare `session::` paths below need
// this to resolve to the real module rather than to the sibling test file.
use crate::session;

#[gpui::test]
fn a_restored_tile_of_an_unknown_kind_falls_back_without_its_state(cx: &mut gpui::TestAppContext) {
    // The record names a kind nothing in the roster registers, so
    // `ensure_occupants` falls back to the roster's default ("rec")
    // — but the fallback factory did not produce that state and must
    // not be handed it (fix-round finding).
    let (mut services, log) = services_with_recorder();
    let mut state = toml::Table::new();
    state.insert(
        "last_command".into(),
        toml::Value::String("state for a different module".into()),
    );
    services.restored_tiles.insert(
        1,
        crate::session::TileRecord {
            kind: "unregistered-kind".into(),
            state,
        },
    );
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(tile, TileId(1), "the first split always allocates tile 1");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec"),
        "the default factory still hosts the tile"
    );
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)
        ),
        "the fallback factory got no state: {:?}",
        log.borrow()
    );
}

/// I2, final review: `fill_all_tiles` walks every workspace, so the
/// FIRST render creates an occupant for a tile in an inactive
/// workspace too — restored here via `session::from_toml`, the same
/// real path `current_tiles_reflects_live_occupants_and_restored_
/// state_reaches_the_factory` above builds. Before the fix, only
/// tiles in the *active* set ever got a `set_visible` call at all;
/// an occupant created outside it heard nothing, ever. `RecordingFactory`
/// does not default a fresh occupant to anything — the assertion
/// below is only meaningful because `set_visible` is required to be
/// called at creation time, per its own doc comment's contract.
#[gpui::test]
fn an_occupant_created_outside_the_active_workspace_is_told_it_is_hidden(
    cx: &mut gpui::TestAppContext,
) {
    let mut table = session::to_toml(&Workspaces::new(), &session::TileRecords::new());
    // Workspace 1 (the default active one) stays empty. Workspace 2
    // gets one tile, restored with the recorder's own kind — this is
    // the occupant that is created on the very first render while
    // workspace 1, not 2, is active.
    let ws2: toml::Table = r#"
        focused = 1
        [node]
        kind = "leaf"
        id = 1
        [tiles.1]
        module = "rec"
    "#
    .parse()
    .unwrap();
    if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
        ws_table.insert("2".to_string(), toml::Value::Table(ws2));
    }
    let restored = session::from_toml(&table).unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    assert_eq!(
        restored.workspaces.active_index(),
        1,
        "sanity: workspace 1, not 2, is active"
    );

    let (mut services, log) = services_with_recorder();
    services.workspaces = restored.workspaces;
    services.restored_tiles = restored.tiles;
    let (_window, mut cx) = open_shell(cx, services);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Visible(TileId(1), false)
        )),
        "an occupant created outside the active workspace must be told \
         it is hidden on its first render: {:?}",
        log.borrow()
    );
    assert!(
        log.borrow().iter().all(|r| !matches!(
            r,
            crate::module::recording::Recorded::Visible(TileId(1), true)
        )),
        "it must never have been told the opposite: {:?}",
        log.borrow()
    );
}

#[gpui::test]
fn a_split_creates_an_occupant_of_the_default_kind_and_paints_it(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)
        ),
        "{:?}",
        log.borrow()
    );
    // `debug_bounds` takes `&'static str`; leak the dynamic selector
    // (test-only, a few bytes).
    let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
    let bounds = cx.debug_bounds(selector);
    assert!(
        bounds.is_some_and(|b| b.size.width > px(0.0)),
        "the occupant's view painted: {bounds:?}"
    );
}

#[gpui::test]
fn a_key_in_the_occupants_context_reaches_its_dispatch_with_the_count(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("4 j");
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert!(
        log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(t, a, Some(4)) if *t == tile && a.0 == "rec::noop"
        )),
        "{:?}",
        log.borrow()
    );
}

#[gpui::test]
fn closing_a_tile_drops_its_occupant_and_switching_workspaces_toggles_visibility(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });

    cx.simulate_keystrokes("alt-2");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Visible(t, false) if *t == tile)
        ),
        "hidden on switch: {:?}",
        log.borrow()
    );
    cx.simulate_keystrokes("alt-1");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Visible(t, true) if *t == tile)
        ),
        "shown on return: {:?}",
        log.borrow()
    );

    cx.simulate_keystrokes("ctrl-w");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        None,
        "occupant dropped with its tile"
    );
}

#[gpui::test]
fn a_click_on_a_tile_leaves_the_shell_focused_on_the_next_frame(cx: &mut gpui::TestAppContext) {
    // gpui focuses a tracked element on mouse down; an occupant that
    // tracks its own handle (DataTable does) would take focus with it
    // and every shell chord would go dead. The tile's click handler
    // arms the same restore `apply_reload` uses (§3.3).
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    // `debug_bounds` takes `&'static str`; leak the dynamic selector
    // (test-only, a few bytes).
    let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
    let bounds = cx.debug_bounds(selector).unwrap();
    cx.simulate_mouse_down(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| focused.is_focused(window)),
        "the shell root has focus again"
    );
}

#[gpui::test]
fn a_click_on_a_docked_tile_leaves_the_shell_focused_on_the_next_frame(
    cx: &mut gpui::TestAppContext,
) {
    // Same hazard as the tree-tile test above, but for a tile parked
    // in a dock (fix-round finding: the dock-tile `on_mouse_down`
    // listener did not re-arm `pending_focus_restore`, so a
    // focus-tracking occupant docked instead of tiled would leave
    // shell chords dead after a click).
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-{");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    // `debug_bounds` takes `&'static str`; leak the dynamic selector
    // (test-only, a few bytes).
    let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
    let bounds = cx.debug_bounds(selector).unwrap();
    cx.simulate_mouse_down(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| focused.is_focused(window)),
        "the shell root has focus again"
    );
}
