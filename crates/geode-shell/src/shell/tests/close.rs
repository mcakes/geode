//! A tile's own close button: the pointer route that stands in for
//! `workspace::close_tile`, aimed at the pressed tile.

use super::*;
use crate::module::CloseHandle;
use crate::module::recording::{Recorded, RecordingPageFactory};
use crate::shell::input::CLOSE_PAGE_FIRST;
use crate::tiling::{Orientation, TileId};

fn sel(tile: TileId) -> &'static str {
    Box::leak(format!("tile-close-{}", tile.0).into_boxed_str())
}

fn press(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).expect("× painted").center();
    cx.simulate_mouse_down(at, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Left, gpui::Modifiers::none());
}

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// Two placeholder tiles side by side, the second (`b`) focused.
fn two_placeholders() -> (ShellServices, TileId, TileId) {
    let mut services = test_services();
    let a = services.workspaces.split_active(Orientation::Horizontal);
    let b = services.workspaces.split_active(Orientation::Horizontal);
    (services, a, b)
}

/// Three placeholder tiles in a row, the last (`c`) focused: closing `a`
/// with focus moved to it first would land focus on `b`, its neighbour.
fn three_placeholders() -> (ShellServices, TileId, TileId, TileId) {
    let mut services = test_services();
    let a = services.workspaces.split_active(Orientation::Horizontal);
    let b = services.workspaces.split_active(Orientation::Horizontal);
    let c = services.workspaces.split_active(Orientation::Horizontal);
    (services, a, b, c)
}

/// The production close handle the shell delivered to `tile`'s occupant.
fn handle_of(shell: &Entity<ShellView>, tile: TileId, cx: &gpui::VisualTestContext) -> CloseHandle {
    shell
        .read_with(cx, |s, _| {
            s.occupants
                .get(&tile)
                .and_then(|o| o.content.close_handle_for_test())
        })
        .expect("the occupant received a close handle")
}

/// Two recorder tiles added through the fixture key; returns (shell, first, second).
fn two_recorders(
    window: &gpui::WindowHandle<Root>,
    cx: &mut gpui::VisualTestContext,
) -> (Entity<ShellView>, TileId, TileId) {
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    draw(cx);
    let shell = shell_of(window, cx);
    let tiles = shell.read_with(cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2, "{tiles:?}");
    (shell, tiles[0], tiles[1])
}

/// A press on an unfocused tile's × closes that tile and leaves focus on
/// the tile that held it; the press does not focus the pressed tile first.
#[gpui::test]
fn the_close_button_closes_its_own_tile_and_keeps_focus(cx: &mut gpui::TestAppContext) {
    let (services, a, _b, c) = three_placeholders();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile()),
        Some(c),
        "fixture: the split's new tile is focused"
    );
    shell.update(&mut cx, |s, _| s.session_dirty = false);
    press(&mut cx, sel(a));
    shell.read_with(&cx, |s, _| {
        let ws = s.services.workspaces.active();
        assert!(!ws.tree().contains(a), "the pressed tile closed");
        assert_eq!(ws.focused_tile(), Some(c), "focus stayed");
        assert!(s.session_dirty);
    });
}

/// The focused tile's × lands focus where `ctrl+w` would.
#[gpui::test]
fn the_focused_tiles_close_button_matches_the_key(cx: &mut gpui::TestAppContext) {
    let (services, _a, b) = two_placeholders();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    press(&mut cx, sel(b));
    shell.read_with(&cx, |s, _| {
        let ws = s.services.workspaces.active();
        assert!(!ws.tree().contains(b));
        assert_eq!(ws.tree().tiles().len(), 1);
        assert_eq!(ws.focused_tile(), Some(ws.tree().tiles()[0]));
    });
}

/// The press is recorded as the action it stands in for.
#[gpui::test]
fn the_close_button_records_the_close_action(cx: &mut gpui::TestAppContext) {
    let (services, _a, b) = two_placeholders();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    press(&mut cx, sel(b));
    let recent: Vec<u64> = shell.read_with(&cx, |s, _| {
        s.services.action_tail.lock().unwrap().recent().collect()
    });
    assert_eq!(
        recent.last(),
        Some(&crate::diagnostics::fnv1a("workspace::close_tile"))
    );
}

/// The closed tile's occupant is told so, exactly once.
#[gpui::test]
fn the_closed_tiles_occupant_hears_closed_once(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let (shell, a, _b) = two_recorders(&window, &mut cx);
    let handle = handle_of(&shell, a, &cx);
    cx.update(|window, cx| handle.close(window, cx));
    draw(&mut cx);
    let closed = log
        .borrow()
        .iter()
        .filter(|r| matches!(r, Recorded::Closed(t) if *t == a))
        .count();
    assert_eq!(closed, 1, "{:?}", log.borrow());
    assert!(shell.read_with(&cx, |s, _| !s.occupants.contains_key(&a)));
}

/// A page covers the tiles: no layout edit may reach them under it.
#[gpui::test]
fn the_close_button_is_refused_over_a_page(cx: &mut gpui::TestAppContext) {
    let services = services_with_page(RecordingPageFactory::new("diagnostics"));
    let (window, mut cx) = open_shell(cx, services);
    let (shell, a, _b) = two_recorders(&window, &mut cx);
    dispatch_action(&shell, "page::toggle_diagnostics", &mut cx);
    assert!(shell.read_with(&cx, |s, _| s.page_open()));
    let handle = handle_of(&shell, a, &cx);
    cx.update(|window, cx| handle.close(window, cx));
    draw(&mut cx);
    shell.read_with(&cx, |s, _| {
        assert!(s.services.workspaces.active().tree().contains(a));
        assert_eq!(s.notice.as_deref(), Some(CLOSE_PAGE_FIRST));
    });
}

/// Pressing another tile's × leaves an open `:` line, as any press on
/// another tile does.
#[gpui::test]
fn the_close_button_leaves_an_open_command_line(cx: &mut gpui::TestAppContext) {
    let (services, a, _b) = two_placeholders();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes(":");
    draw(&mut cx);
    assert!(shell.read_with(&cx, |s, _| s.command_line.is_some()));
    press(&mut cx, sel(a));
    draw(&mut cx);
    shell.read_with(&cx, |s, _| {
        assert!(s.command_line.is_none());
        assert!(!s.services.workspaces.active().tree().contains(a));
    });
}

/// Closing the tile whose input holds the keyboard hands focus back to
/// the shell, so the next key still reaches it.
#[gpui::test]
fn closing_a_tile_whose_input_holds_focus_keeps_keys_working(cx: &mut gpui::TestAppContext) {
    let (services, _log, _input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut cx) = open_shell(cx, services);
    let (shell, _a, b) = two_recorders(&window, &mut cx);
    let shell_focus = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    cx.simulate_keystrokes("i");
    draw(&mut cx);
    assert!(
        !cx.update(|window, _| shell_focus.is_focused(window)),
        "fixture: the tile's input holds focus"
    );
    let handle = handle_of(&shell, b, &cx);
    cx.update(|window, cx| handle.close(window, cx));
    draw(&mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, _| s
            .services
            .workspaces
            .active()
            .tree()
            .tiles()
            .len()),
        1
    );
    cx.simulate_keystrokes("ctrl-v");
    draw(&mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, _| s
            .services
            .workspaces
            .active()
            .tree()
            .tiles()
            .len()),
        2,
        "a workspace key still reaches the shell"
    );
}

/// A handle outliving its tile closes nothing.
#[gpui::test]
fn a_stale_close_handle_closes_nothing(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let (shell, a, _b) = two_recorders(&window, &mut cx);
    let handle = handle_of(&shell, a, &cx);
    cx.update(|window, cx| handle.close(window, cx));
    draw(&mut cx);
    let count = |cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |s, _| {
            s.services.workspaces.active().tree().tiles().len()
        })
    };
    assert_eq!(count(&cx), 1);
    cx.update(|window, cx| handle.close(window, cx));
    draw(&mut cx);
    assert_eq!(count(&cx), 1);
}

/// Every visible placeholder paints its × inside its own tile.
#[gpui::test]
fn the_placeholder_paints_the_close_button(cx: &mut gpui::TestAppContext) {
    let (services, a, b) = two_placeholders();
    let (_window, mut cx) = open_shell(cx, services);
    for tile in [a, b] {
        let button = cx.debug_bounds(sel(tile)).expect("× painted");
        let content: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
        let tile_bounds = cx.debug_bounds(content).expect("tile painted");
        assert!(
            tile_bounds.contains(&button.origin)
                && tile_bounds.contains(&button.bottom_right().map(|v| v - px(0.5))),
            "{tile:?}: {button:?} inside {tile_bounds:?}"
        );
    }
}

/// Press at `at` with `click_count`, as the platform reports each press
/// of a double-click.
fn press_counted(cx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>, count: usize) {
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::none(),
                click_count: count,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::none(),
                click_count: count,
            }),
            cx,
        );
    });
    draw(cx);
}

/// A double-click on the right tile's × closes that tile only: its
/// neighbour grows under the pointer, and the second press neither
/// closes it nor reaches its double-click gestures (fullscreen, picker).
#[gpui::test]
fn a_double_click_on_the_close_button_closes_one_tile(cx: &mut gpui::TestAppContext) {
    let (services, a, b) = two_placeholders();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let at = cx.debug_bounds(sel(b)).expect("× painted").center();
    press_counted(&mut cx, at, 1);
    press_counted(&mut cx, at, 2);
    shell.read_with(&cx, |s, _| {
        let tree = s.services.workspaces.active().tree();
        assert_eq!(tree.tiles(), vec![a], "exactly one tile closed");
        assert_eq!(tree.fullscreen(), None, "the survivor is not fullscreen");
        assert!(!picker_open(s), "no tile picker opened");
    });
}

/// Whether the tile picker is the open choice dialog.
fn picker_open(s: &ShellView) -> bool {
    matches!(
        s.choice_dialog.as_ref().map(|d| &d.target),
        Some(crate::shell::choicedialog::Target::TileKind { .. })
    )
}

/// A double-click on the only tile's × closes it; the second press lands
/// on the empty-tree hint, whose double-click would open the tile picker.
#[gpui::test]
fn a_double_click_on_the_last_tiles_close_button_opens_nothing(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let a = services.workspaces.split_active(Orientation::Horizontal);
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let at = cx.debug_bounds(sel(a)).expect("× painted").center();
    press_counted(&mut cx, at, 1);
    press_counted(&mut cx, at, 2);
    shell.read_with(&cx, |s, _| {
        assert!(s.services.workspaces.active().tree().tiles().is_empty());
        assert!(!picker_open(s), "no tile picker opened");
    });
}

/// A double-click on the left tile's × closes that tile only: the
/// focused placeholder beside it grows under the pointer, and the second
/// press reaches neither its picker nor its fullscreen gesture.
#[gpui::test]
fn a_double_click_on_the_left_close_button_reaches_no_neighbour(cx: &mut gpui::TestAppContext) {
    let (services, a, b) = two_placeholders();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let at = cx.debug_bounds(sel(a)).expect("× painted").center();
    press_counted(&mut cx, at, 1);
    press_counted(&mut cx, at, 2);
    shell.read_with(&cx, |s, _| {
        let tree = s.services.workspaces.active().tree();
        assert_eq!(tree.tiles(), vec![b], "exactly one tile closed");
        assert_eq!(tree.fullscreen(), None, "the survivor is not fullscreen");
        assert!(!picker_open(s), "no tile picker opened");
    });
}

/// Only the closing double-click is swallowed: the next first press ends
/// it, so a fresh double-click on the empty tree still opens the picker.
#[gpui::test]
fn a_fresh_double_click_after_a_close_still_reaches_its_gesture(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let a = services.workspaces.split_active(Orientation::Horizontal);
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let at = cx.debug_bounds(sel(a)).expect("× painted").center();
    press_counted(&mut cx, at, 1);
    press_counted(&mut cx, at, 2);
    press_counted(&mut cx, at, 1);
    press_counted(&mut cx, at, 2);
    shell.read_with(&cx, |s, _| assert!(picker_open(s), "the picker opened"));
}

/// A mod+press on the × is the tile's drag gesture, not a close: the
/// press passes through to the tile cell, which arms a drag.
#[gpui::test]
fn a_mod_press_on_the_close_button_arms_a_drag(cx: &mut gpui::TestAppContext) {
    let (services, a, b) = two_placeholders();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let at = cx.debug_bounds(sel(a)).expect("× painted").center();
    let alt = gpui::Modifiers {
        alt: true,
        ..gpui::Modifiers::none()
    };
    cx.simulate_mouse_down(at, MouseButton::Left, alt);
    shell.read_with(&cx, |s, _| {
        assert_eq!(s.services.workspaces.active().tree().tiles(), vec![a, b]);
        assert_eq!(
            s.tile_drag.as_ref().map(|d| d.tile),
            Some(a),
            "a drag armed"
        );
    });
    cx.simulate_mouse_up(at, MouseButton::Left, gpui::Modifiers::none());
    draw(&mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles()),
        vec![a, b],
        "the release closes nothing"
    );
}
