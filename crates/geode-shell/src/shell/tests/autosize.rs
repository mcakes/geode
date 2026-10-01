//! `tile::autosize_columns`: the palette action reaches the focused
//! tile's occupant alone, and a tile without a table (or no tile at all)
//! answers with the refusal notice.

use super::*;
use crate::defaults::AddPlacement;
use crate::module::recording::Recorded;

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

fn autosized(log: &[Recorded]) -> Vec<(TileId, bool)> {
    log.iter()
        .filter_map(|r| match r {
            Recorded::Autosize(t, reset) => Some((*t, *reset)),
            _ => None,
        })
        .collect()
}

/// Two recorder tiles, the second focused: the action reaches the second
/// alone, as a fit (not a reset), and leaves no notice. Moving focus moves
/// the next fit with it.
#[gpui::test]
fn the_autosize_action_reaches_only_the_focused_tile(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    draw(&mut vcx);
    let (tiles, focused) = shell.read_with(&vcx, |s, _| {
        let ws = s.services.workspaces.active();
        (ws.tree().tiles(), ws.focused_tile().unwrap())
    });
    assert_eq!(tiles.len(), 2);
    dispatch_action(&shell, "tile::autosize_columns", &mut vcx);
    assert_eq!(autosized(&log.borrow()), vec![(focused, false)]);
    assert_eq!(shell.read_with(&vcx, |s, _| s.notice.clone()), None);

    let other = *tiles.iter().find(|t| **t != focused).unwrap();
    shell.update(&mut vcx, |s, _| {
        s.services.workspaces.active_mut().focus_main_tile(other);
    });
    dispatch_action(&shell, "tile::autosize_columns", &mut vcx);
    assert_eq!(
        autosized(&log.borrow()),
        vec![(focused, false), (other, false)]
    );
}

/// A placeholder occupant keeps the trait's default and refuses.
#[gpui::test]
fn autosize_on_a_tile_without_a_table_shows_the_refusal(cx: &mut gpui::TestAppContext) {
    let (services, log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.add_tile("no-such-kind", AddPlacement::Split(None), None, window, cx);
        })
    });
    draw(&mut vcx);
    let kind = shell.read_with(&vcx, |s, _| {
        let tile = s.services.workspaces.active().focused_tile().unwrap();
        s.occupant_kind(tile)
    });
    assert_eq!(kind, Some(crate::module::placeholder::PLACEHOLDER_KIND));
    dispatch_action(&shell, "tile::autosize_columns", &mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice.clone()).as_deref(),
        Some(crate::colfit::NO_TABLE)
    );
    assert!(autosized(&log.borrow()).is_empty());
}

/// With no tile at all, the same refusal.
#[gpui::test]
fn autosize_with_no_focused_tile_shows_the_refusal(cx: &mut gpui::TestAppContext) {
    let (services, _log) = test_services_with_log();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "tile::autosize_columns", &mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice.clone()).as_deref(),
        Some(crate::colfit::NO_TABLE)
    );
}

/// Registered for the palette with its title and category.
#[test]
fn the_autosize_action_is_registered_for_the_palette() {
    let mut registry = crate::actions::ActionRegistry::default();
    register_builtin_actions(&mut registry);
    let def = registry
        .get(&ActionId("tile::autosize_columns".into()))
        .expect("registered");
    assert_eq!(def.title, "Autosize columns");
    assert_eq!(def.category, "Tile");
}
