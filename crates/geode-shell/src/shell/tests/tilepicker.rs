//! The tile picker (2026-09-19): a bare double-click on a placeholder
//! tile opens it and a pick fills that placeholder in place; `tile::add`
//! (`mod+n`, dispatched here as the literal `alt-n`) opens it from
//! anywhere and a pick from a real tile splits; a double-click on a real
//! tile, or one with a modifier held, opens nothing.

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

    assert!(shell.read_with(&cx, |s, _| s.modal.is_none()));
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
    assert!(shell.read_with(&cx, |s, _| s.modal.is_none()));
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
        shell.read_with(&cx, |s, _| s.modal.is_none()),
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
    assert!(shell.read_with(&cx, |s, _| s.modal.is_none()));
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
        shell.read_with(&cx, |s, _| s.modal.is_none()),
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
        shell.read_with(&cx, |s, _| s.modal.is_none()),
        "a real tile's double-click opens nothing"
    );
}
