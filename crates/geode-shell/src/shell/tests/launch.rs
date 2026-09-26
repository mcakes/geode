//! Launch context: `TileContent::launched` reaches a tile `add_tile`
//! created (add, duplicate) once it is focused, never a restored one;
//! `tile::open_with` lists the kinds accepting the focused tile's
//! context and creates the pick with the factory's translated state.

use super::*;
use crate::defaults::AddPlacement;
use crate::module::recording::{Recorded, RecordingFactory};
use geode_core::launch::{ContextField, LaunchContext};

type Log = std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>;

fn launched(log: &Log, tile: TileId) -> usize {
    log.borrow()
        .iter()
        .filter(|r| matches!(r, Recorded::Launched(t) if *t == tile))
        .count()
}

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

fn focused(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> TileId {
    shell.read_with(cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    })
}

/// An add through the real key route: the new tile is focused on its first
/// render, and hears `launched` exactly once, even across further renders.
#[gpui::test]
fn an_added_tile_hears_launched_once(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let tile = focused(&shell, &vcx);
    assert_eq!(launched(&log, tile), 1, "{:?}", log.borrow());
    draw(&mut vcx);
    assert_eq!(launched(&log, tile), 1, "a later render does not repeat it");
}

/// Duplicate goes through `add_tile`, so the copy hears it too.
#[gpui::test]
fn a_duplicated_tile_hears_launched(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let first = focused(&shell, &vcx);
    dispatch_action(&shell, "workspace::duplicate_horizontal", &mut vcx);
    draw(&mut vcx);
    let copy = focused(&shell, &vcx);
    assert_ne!(copy, first);
    assert_eq!(launched(&log, copy), 1, "{:?}", log.borrow());
}

/// An add whose tile is no longer focused on its first render (focus moved
/// back before the frame) is not prompted: `launched` may take the keyboard,
/// and only the focused tile may do that.
#[gpui::test]
fn an_add_that_is_not_focused_on_its_first_render_is_not_launched(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let first = focused(&shell, &vcx);
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.add_tile("rec", AddPlacement::Split(None), None, window, cx);
            s.services.workspaces.active_mut().focus_main_tile(first);
        })
    });
    draw(&mut vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2);
    let second = *tiles.iter().find(|t| **t != first).unwrap();
    assert_eq!(launched(&log, second), 0, "{:?}", log.borrow());
}

/// A restored session never hears `launched`: startup takes focus from
/// nothing, however many tiles were saved.
#[gpui::test]
fn a_restored_tile_is_not_launched(cx: &mut gpui::TestAppContext) {
    let mut table = crate::session::to_toml(
        &Workspaces::new(),
        &crate::session::TileRecords::new(),
        None,
        &crate::palette_usage::PaletteUsage::new(),
    );
    let ws1: toml::Table = r#"
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
        ws_table.insert("1".to_string(), toml::Value::Table(ws1));
    }
    let restored = crate::session::from_toml(&table).unwrap();
    let (mut services, log) = test_services_with_log();
    services.workspaces = restored.workspaces;
    services.restored_tiles = restored.tiles;
    let (_window, mut vcx) = open_shell(cx, services);
    draw(&mut vcx);
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(TileId(1), _))),
        "fixture: the tile was restored: {:?}",
        log.borrow()
    );
    assert_eq!(launched(&log, TileId(1)), 0, "{:?}", log.borrow());
}

/// Picking a kind in the tile picker: a tile that takes the keyboard in
/// `launched` (as the market-data panel's picker does) still holds it after
/// the modal's focus return and two more frames.
#[gpui::test]
fn a_launched_tile_keeps_the_keyboard_it_takes(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.edit_on_launch = true;
    let input = rec.input.clone();
    let services = services_with_recorders(vec![rec]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    // The first tile took the keyboard in its own `launched`; give it back
    // (the fixture ships no cancel binding) so the picker opens from the
    // shell, as a trader's `mod+n` would.
    vcx.update(|window, cx| window.blur(cx));
    draw(&mut vcx);
    dispatch_action(&shell, "tile::add", &mut vcx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    draw(&mut vcx);
    let new = focused(&shell, &vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2, "the pick split");
    let held = vcx.update(|window, cx| {
        input
            .borrow()
            .as_ref()
            .is_some_and(|i| i.read(cx).focus_handle(cx).is_focused(window))
    });
    assert!(held, "tile {new:?}'s launched input holds the keyboard");
}
