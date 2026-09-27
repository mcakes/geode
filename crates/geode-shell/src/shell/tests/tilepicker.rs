//! Tile-picker integration: double-clicking a placeholder opens the picker and fills it
//! in place; `tile::add` (`mod+n`, Alt-N in this fixture) opens it anywhere and splits
//! when launched from a real tile. Double-clicking a real tile or holding a modifier
//! does not open it.

use super::drag::{dock_tile_point, main_tile_point};
use super::occupants::dispatch_and_draw;
use super::*;
use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::shell::choicedialog::Target;
use crate::tiling::Orientation;

/// A shell whose one main tile is a placeholder (a bare `split_active`
/// carries no add request, so `ensure_occupants` gives it the
/// placeholder — `add_on_a_placeholder_tile_fills_it_in_place`'s own
/// fixture).
fn shell_with_a_placeholder(
    cx: &mut gpui::TestAppContext,
) -> (gpui::VisualTestContext, Entity<ShellView>, TileId) {
    let (mut cx, shell) = dock_test_shell(cx);
    shell.update(&mut cx, |s, cx| {
        s.services.workspaces.split_active(Orientation::Horizontal);
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some(PLACEHOLDER_KIND)
    );
    (cx, shell, tile)
}

fn is_tile_picker(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> bool {
    shell.read_with(cx, |s, _| {
        matches!(
            s.choice_dialog.as_ref().map(|d| &d.target),
            Some(Target::TileKind { .. })
        )
    })
}

/// Double-clicking a placeholder opens the picker with the field
/// focused (typing reaches it), and `enter` on the highlighted kind
/// fills THAT tile in place — no split.
#[gpui::test]
fn double_clicking_a_placeholder_opens_the_picker_and_a_pick_fills_it(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell, tile) = shell_with_a_placeholder(cx);
    let at = main_tile_point(&mut cx, &shell, tile, 0.5, 0.5);
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();

    assert!(is_tile_picker(&shell, &cx), "the tile picker opened");
    assert!(cx.debug_bounds("tile-choice-list").is_some());
    assert!(cx.debug_bounds("tile-choice-Rec").is_some());
    // Defensive: no roster registers the placeholder factory today, so
    // the unit test `tile_rows_are_the_roster_kinds_titled_minus_the_
    // placeholder` is the real pin of the filter.
    assert!(cx.debug_bounds("tile-choice-Placeholder").is_none());
    assert!(cx.debug_bounds("tile-hints").is_some());
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the field keeps focus through the rest of the double-click"
    );

    cx.simulate_input("re");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(shell.read_with(&cx, |s, _| !s.modal_open()));
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles, vec![tile], "no split: the placeholder was filled");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
}

/// `mod+n` from a real tile opens the same picker; the pick splits, as
/// the palette's `Rec: Split` row would.
#[gpui::test]
fn mod_n_opens_the_picker_and_a_pick_from_a_real_tile_splits(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let before = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(before.len(), 1);

    cx.simulate_keystrokes("alt-n");
    cx.run_until_parked();
    assert!(is_tile_picker(&shell, &cx));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let after = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(after.len(), 2, "a real focused tile is split");
    for t in &after {
        assert_eq!(
            shell.read_with(&cx, |s, _| s.occupant_kind(*t)),
            Some("rec")
        );
    }
}

/// A row click is a pick, like `enter`.
#[gpui::test]
fn a_row_click_adds_that_kind(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, tile) = shell_with_a_placeholder(cx);
    cx.simulate_keystrokes("alt-n");
    cx.run_until_parked();
    let row = cx.debug_bounds("tile-choice-Rec").expect("the Rec row");
    cx.simulate_click(row.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(shell.read_with(&cx, |s, _| !s.modal_open()));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
}

/// A docked placeholder is the same door (the dock listener calls it
/// too): the pick fills the docked tile in place, the dock's tree
/// unchanged.
#[gpui::test]
fn a_docked_placeholder_double_click_opens_the_picker_and_fills_it(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, tile) = shell_with_a_placeholder(cx);
    cx.simulate_keystrokes("ctrl-{"); // tile → left dock, dock focused
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let docked = shell.read_with(&cx, |s, _| {
        s.services
            .workspaces
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .tiles()
    });
    assert_eq!(docked, vec![tile], "sanity: the placeholder is docked");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some(PLACEHOLDER_KIND)
    );

    let at = dock_tile_point(&mut cx, &shell, DockSide::Left, tile, 0.5, 0.5);
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        is_tile_picker(&shell, &cx),
        "the tile picker opened from the dock"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
    let docked = shell.read_with(&cx, |s, _| {
        s.services
            .workspaces
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .tiles()
    });
    assert_eq!(
        docked,
        vec![tile],
        "filled in place: the dock tree is unchanged"
    );
}

/// The door requires the placeholder to be the FOCUSED tile: a lone
/// second click (the OS stamps `click_count: 2` even when the first
/// click landed on an occluding neighbour — a divider strip, a modal's
/// backdrop) on an unfocused placeholder is the plain click it looks
/// like, never a picker whose pick would land on the tile that IS
/// focused.
#[gpui::test]
fn a_second_click_on_an_unfocused_placeholder_is_a_plain_click(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let real = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    shell.update(&mut cx, |s, cx| {
        s.services.workspaces.split_active(Orientation::Horizontal);
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let placeholder = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_ne!(real, placeholder);
    // Focus the real tile again, so the placeholder is unfocused.
    shell.update(&mut cx, |s, cx| {
        assert!(s.services.workspaces.active_mut().focus_main_tile(real));
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let at = main_tile_point(&mut cx, &shell, placeholder, 0.5, 0.5);
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::none(),
                click_count: 2,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: gpui::Modifiers::none(),
                click_count: 2,
            }),
            cx,
        );
    });
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| !s.modal_open()),
        "no picker: the placeholder was not the focused tile"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(placeholder),
        "the click was click-to-focus"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(placeholder)),
        Some(PLACEHOLDER_KIND)
    );
}

/// A single click on a placeholder is click-to-focus and nothing more;
/// only the pair's SECOND click (`click_count == 2`) is the door.
#[gpui::test]
fn a_single_click_on_a_placeholder_opens_nothing(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, tile) = shell_with_a_placeholder(cx);
    let at = main_tile_point(&mut cx, &shell, tile, 0.5, 0.5);
    cx.simulate_click(at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| !s.modal_open()));
    assert!(shell.read_with(&cx, |s, _| s.choice_dialog.is_none()));
}

/// A double-click on a REAL tile is two clicks and nothing more — a
/// module may own the gesture — and a modified double-click on a
/// placeholder is not this door's either.
#[gpui::test]
fn a_double_click_on_a_real_tile_or_with_a_modifier_opens_nothing(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, placeholder) = shell_with_a_placeholder(cx);
    let at = main_tile_point(&mut cx, &shell, placeholder, 0.5, 0.5);
    double_click(
        &mut cx,
        at,
        gpui::Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| !s.modal_open()),
        "shift+double-click is not the door"
    );

    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(placeholder)),
        Some("rec")
    );
    let at = main_tile_point(&mut cx, &shell, placeholder, 0.5, 0.5);
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| !s.modal_open()),
        "a real tile's double-click opens nothing"
    );
}

/// A fresh session has an empty main tree rather than a placeholder tile.
/// Double-clicking its empty-region hint opens the picker, and the selected kind
/// becomes the tree's root tile.
#[gpui::test]
fn double_clicking_the_empty_tree_hint_opens_the_picker_and_a_pick_fills_the_tree(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell) = dock_test_shell(cx);
    assert!(
        shell.read_with(&cx, |s, _| s
            .services
            .workspaces
            .active()
            .tree()
            .tiles()
            .is_empty()),
        "sanity: a fresh shell has no tile"
    );
    let hint = cx
        .debug_bounds("empty-hint")
        .expect("the empty hint painted");
    double_click(&mut cx, hint.center(), gpui::Modifiers::none());
    cx.run_until_parked();

    assert!(is_tile_picker(&shell, &cx), "the tile picker opened");
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the field holds focus after the pair"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(shell.read_with(&cx, |s, _| !s.modal_open()));
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 1, "the pick is the tree's root tile");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tiles[0])),
        Some("rec")
    );
    assert!(cx.debug_bounds("empty-hint").is_none());
}

/// A single click on the empty hint is nothing, and so is a modified
/// double-click — the same gesture table as the placeholder door.
#[gpui::test]
fn a_single_or_modified_click_on_the_empty_tree_hint_opens_nothing(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);
    let hint = cx
        .debug_bounds("empty-hint")
        .expect("the empty hint painted");
    cx.simulate_click(hint.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| !s.modal_open()));
    double_click(
        &mut cx,
        hint.center(),
        gpui::Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| !s.modal_open()));
    assert!(shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().tree().tiles().is_empty()
    }));
}

/// Showing an empty dock focuses that region, so `mod+n` adds into the dock even when a
/// real tile already exists in the main tree.
#[gpui::test]
fn showing_an_empty_dock_focuses_it_and_mod_n_adds_into_it(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("ctrl-[");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let region = shell.read_with(&cx, |s, _| s.services.workspaces.active().region());
    assert_eq!(
        region,
        crate::tiling::FocusRegion::Dock(DockSide::Left),
        "region after show"
    );
    cx.simulate_keystrokes("alt-n");
    cx.run_until_parked();
    assert!(is_tile_picker(&shell, &cx), "picker opened");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let docked = shell.read_with(&cx, |s, _| {
        s.services
            .workspaces
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .tiles()
    });
    assert_eq!(docked.len(), 1, "the pick landed in the dock");
    let main = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(main.len(), 1, "main untouched");
}

/// Clicking an empty dock focuses it; double-clicking opens a picker whose selection
/// adds into that dock.
#[gpui::test]
fn clicking_an_empty_dock_focuses_it_and_double_clicking_adds_into_it(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let main_tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.simulate_keystrokes("ctrl-["); // show the (empty) left dock
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    // Put the region back on the main tile by clicking it, so the dock
    // click below has something to change.
    let at = main_tile_point(&mut cx, &shell, main_tile, 0.5, 0.5);
    cx.simulate_click(at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().region()),
        crate::tiling::FocusRegion::Main
    );
    shell.update(&mut cx, |s, _| s.session_dirty = false);

    let hint = cx
        .debug_bounds("dock-empty-hint-left")
        .expect("the empty dock hint painted");
    cx.simulate_click(hint.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().region()),
        crate::tiling::FocusRegion::Dock(DockSide::Left),
        "a click on an empty dock focuses it"
    );
    assert!(
        shell.read_with(&cx, |s, _| !s.modal_open()),
        "a single click opens nothing"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.session_dirty),
        "a region change persists"
    );

    double_click(&mut cx, hint.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        is_tile_picker(&shell, &cx),
        "the picker opened from the dock"
    );
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let docked = shell.read_with(&cx, |s, _| {
        s.services
            .workspaces
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .tiles()
    });
    assert_eq!(docked.len(), 1, "the pick landed in the dock");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(docked[0])),
        Some("rec")
    );
    let main = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(main, vec![main_tile], "main untouched");
    assert!(cx.debug_bounds("dock-empty-hint-left").is_none());
}

/// The empty tree's hint names its key inside prose (`kbd::marked`): the
/// `ctrl+k` paints as a `Kbd` chip, not as text. A fresh shell paints
/// nothing else bound to `ctrl+k`, so the chip is the hint's.
#[gpui::test]
fn an_empty_workspace_hint_paints_its_keys_as_kbd(cx: &mut gpui::TestAppContext) {
    let (mut cx, _shell) = dock_test_shell(cx);
    assert!(
        cx.debug_bounds("empty-hint").is_some(),
        "sanity: the hint painted"
    );
    assert!(
        cx.debug_bounds("kbd:ctrl-k").is_some(),
        "the hint's `ctrl+k` paints as a Kbd chip"
    );
}
