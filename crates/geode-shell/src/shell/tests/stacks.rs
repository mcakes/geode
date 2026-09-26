//! Tile-stack position delivery, visible membership, navigation, and notices.

use super::*;
use crate::module::recording::Recorded;
use crate::tiling::TileId;

/// Two tiles side by side, then a "rec" stacked onto the right one:
/// [left | stack(right, top active)]. Returns (cx, shell, log, left, right, top).
#[allow(clippy::type_complexity)]
pub(super) fn stacked_shell(
    cx: &mut gpui::TestAppContext,
) -> (
    gpui::VisualTestContext,
    Entity<ShellView>,
    std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>,
    TileId,
    TileId,
    TileId,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(
                &ActionId("tile::add_rec_stacked".to_string()),
                None,
                window,
                cx,
            );
        });
        let _ = window.draw(cx);
    });
    let tiles: Vec<TileId> =
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 3, "{tiles:?}");
    (cx, shell, log, tiles[0], tiles[1], tiles[2])
}

fn stack_events(
    log: &std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>,
    tile: TileId,
) -> Vec<Option<(usize, usize)>> {
    log.borrow()
        .iter()
        .filter_map(|r| match r {
            Recorded::Stack(t, p) if *t == tile => Some(*p),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn a_stacked_add_tells_both_members_their_position_once_and_hides_the_old_one(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell, log, left, right, top) = stacked_shell(cx);
    assert_eq!(stack_events(&log, right), vec![None, Some((1, 2))]);
    assert_eq!(stack_events(&log, top), vec![Some((2, 2))]);
    assert_eq!(stack_events(&log, left), vec![None]);
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Visible(t, false) if *t == right)),
        "the hidden member is told it left the screen: {:?}",
        log.borrow()
    );
    // An unrelated render re-notifies nobody.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
        let _ = window.draw(cx);
    });
    assert_eq!(stack_events(&log, right), vec![None, Some((1, 2))]);
    let _ = shell;
}

#[gpui::test]
fn mod_bracket_cycles_and_the_ring_follows(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, log, _left, right, top) = stacked_shell(cx);
    cx.simulate_keystrokes("alt-]");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right)
    );
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Visible(t, true) if *t == right))
    );
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Visible(t, false) if *t == top))
    );
    assert!(shell.read_with(&cx, |s, _| s.session_dirty));
    cx.simulate_keystrokes("alt-[");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(top)
    );
    // A cycle changes which member is ACTIVE, never a member's own
    // `(index, len)` within the stack — so neither cycle re-delivered
    // `right`'s stack position.
    assert_eq!(stack_events(&log, right), vec![None, Some((1, 2))]);
}

#[gpui::test]
fn a_count_prefix_steps_n_members(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(
                &ActionId("tile::add_rec_stacked".to_string()),
                None,
                window,
                cx,
            );
        });
    });
    // [right, top, newest]; newest (index 2) focused. 2 × next is
    // (2 + 2) mod 3 = 1: `top`, wrapping past `right`.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::next".to_string()), Some(2), window, cx);
        });
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(top)
    );
    let _ = right;
}

#[gpui::test]
fn a_stack_verb_on_a_plain_tile_leaves_a_notice_the_next_action_clears(
    cx: &mut gpui::TestAppContext,
) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-]");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.notice),
        Some("not in a stack")
    );
    assert!(
        cx.debug_bounds("shell-notice").is_some(),
        "painted in the status bar"
    );
    cx.simulate_keystrokes("alt-l");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(shell.read_with(&cx, |s, _| s.notice), None);
}

#[gpui::test]
fn unstack_pops_the_focused_member_out_and_the_survivors_are_re_notified(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell, log, _left, right, top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::unstack".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.stack_position(top)),
        None
    );
    assert_eq!(stack_events(&log, top).last(), Some(&None));
    assert_eq!(
        stack_events(&log, right).last(),
        Some(&None),
        "a one-member stack collapsed"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s
            .services
            .workspaces
            .active()
            .tree()
            .visible_tiles()
            .len()),
        3
    );
}

#[gpui::test]
fn a_cycle_re_arms_the_focus_restore_while_an_abandoned_editor_holds_the_keyboard(
    cx: &mut gpui::TestAppContext,
) {
    // Focus a module input, then change the active tile through a stack command. The
    // shell must arm root-focus restoration.
    let (services, focus) = services_with_recorder_focus();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(
                &ActionId("tile::add_rec_stacked".to_string()),
                None,
                window,
                cx,
            );
        });
        let _ = window.draw(cx);
    });
    cx.update(|window, cx| {
        if let Some(handle) = focus.borrow().clone() {
            window.focus(&handle, cx);
        }
    });
    // Dispatched directly, and the flag read inside the SAME update — a
    // full `simulate_keystrokes` round trip flushes gpui's test-mode
    // auto-draw before control returns to the test, which is exactly the
    // render that consumes `pending_focus_restore` at its top; reading it
    // afterwards would only ever see it already cleared.
    let pending = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::next".to_string()), None, window, cx);
            shell.pending_focus_restore
        })
    });
    assert!(pending);
}

#[gpui::test]
fn stack_pick_opens_the_list_highlighting_the_active_member(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let list = shell
        .read_with(&cx, |s, _| s.stack_list.clone())
        .expect("open");
    assert_eq!(list.members, vec![right, top]);
    assert_eq!(list.highlighted, 1, "the showing member");
    assert!(cx.debug_bounds("stack-list").is_some());
    assert!(cx.debug_bounds("stack-list-row-0").is_some());
}

#[gpui::test]
fn a_digit_enter_and_escape_do_what_the_spec_says(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, top) = stacked_shell(cx);
    let focused = |cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |s, _| s.services.workspaces.active().tree().focused())
    };
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    cx.simulate_keystrokes("1");
    assert_eq!(focused(&cx), Some(right), "a digit activates at once");
    assert!(
        shell.read_with(&cx, |s, _| s.stack_list.is_none()),
        "and closes the list"
    );

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        focused(&cx),
        Some(top),
        "j then enter activates the highlighted row"
    );

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    cx.simulate_keystrokes("k");
    cx.simulate_keystrokes("escape");
    assert_eq!(focused(&cx), Some(top), "escape changes nothing");
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));
}

#[gpui::test]
fn a_row_click_activates_and_a_click_outside_closes(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, _top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let row = cx.debug_bounds("stack-list-row-0").unwrap();
    cx.simulate_click(row.center(), gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right)
    );
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let catcher = cx.debug_bounds("stack-list-click-catcher").unwrap();
    cx.simulate_click(
        gpui::point(catcher.right() - px(4.0), catcher.bottom() - px(4.0)),
        gpui::Modifiers::none(),
    );
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right),
        "an outside click changes nothing"
    );
}

#[gpui::test]
fn the_handle_opens_the_list_on_its_own_tile_and_ctrl_k_closes_it(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, _top) = stacked_shell(cx);
    // Focus `left`, then open through `right`'s handle: the list must be
    // about `right`'s stack and `right`'s tile must take focus first.
    cx.simulate_keystrokes("alt-h");
    let handle = shell.read_with(&cx, |s, _| {
        s.occupants
            .get(&right)
            .and_then(|o| o.content.stack_handle_for_test())
    });
    let handle = handle.expect("right holds a handle");
    cx.update(|window, cx| handle.open_list(window, cx));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right)
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.stack_list.as_ref().map(|l| l.tile)),
        Some(right)
    );
    cx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&cx, |s, _| s.stack_list.is_none()),
        "ctrl+k closes it for the palette"
    );
}

#[gpui::test]
fn pick_on_a_plain_tile_refuses_with_the_notice(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.notice),
        Some("not in a stack")
    );
}

/// Opening a stack list takes shell-root focus even when the scope field owns the
/// keyboard. General tile-focus restoration accepts that field as a shell surface and
/// therefore cannot perform this overlay-specific transfer.
#[gpui::test]
fn the_list_takes_the_keyboard_from_the_scope_bar(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, _top) = stacked_shell(cx);
    cx.simulate_keystrokes("alt-/");
    assert!(
        filter_is_focused(&shell, &mut cx),
        "mod+/ focused the scope bar's field"
    );
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    assert!(
        !filter_is_focused(&shell, &mut cx),
        "the list took the keyboard back from the field"
    );
    cx.simulate_keystrokes("1");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right)
    );
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));
}

/// Chords fall through the stack list to the matcher, whose dispatch closes the list.
/// `ctrl+3` must select a grouping slot rather than activate stack member three.
#[gpui::test]
fn a_chord_falls_through_the_list_to_the_matcher(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, _right, top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    cx.simulate_keystrokes("ctrl-3");
    assert!(
        shell.read_with(&cx, |s, _| s.stack_list.is_none()),
        "the chord fell through to the matcher, whose dispatch closed the list"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(top),
        "nothing was activated"
    );
}

/// Adding a stack member to an empty region or placeholder follows the split path,
/// creating its first real tile rather than a one-member stack.
#[gpui::test]
fn a_stacked_add_on_a_placeholder_or_empty_region_fills_or_roots_like_a_split(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(
                &ActionId("tile::add_rec_stacked".to_string()),
                None,
                window,
                cx,
            );
        });
        let _ = window.draw(cx);
    });
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 1, "empty region: the tile is the root");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.stack_position(tiles[0])),
        None
    );
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Created(t, _) if *t == tiles[0]))
    );
}

/// Adding to a leaf or stack inserts after the focused tile and makes the new member
/// active and focused.
#[gpui::test]
fn a_stacked_add_on_a_member_lands_after_it(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, top) = stacked_shell(cx);
    cx.simulate_keystrokes("alt-]"); // right active
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(
                &ActionId("tile::add_rec_stacked".to_string()),
                None,
                window,
                cx,
            );
        });
        let _ = window.draw(cx);
    });
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 4);
    let new = tiles[2];
    assert_eq!(&tiles[1..], &[right, new, top]);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(new)
    );
}

/// A center drop across regions stacks into the target's stack, with region and focus
/// following the dragged tile.
#[gpui::test]
fn a_centre_drop_across_regions_stacks_into_the_targets_dock(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    let (a, b) = (tiles[0], tiles[1]);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(b),
        "the second add focused the new tile"
    );
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("dock::move_left".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    // b is in the left dock and focused; drop a onto b's centre.
    let grab = super::drag::main_tile_point(&mut cx, &shell, a, 0.5, 0.5);
    let drop = super::drag::dock_tile_point(&mut cx, &shell, DockSide::Left, b, 0.5, 0.5);
    cx.simulate_mouse_down(grab, gpui::MouseButton::Left, super::drag::alt_held());
    cx.simulate_mouse_move(drop, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(drop, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.stack_position(a)),
        Some((2, 2))
    );
    assert!(shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().is_empty()));
}

/// Placeholder occupants paint the stack marker on the active member only. Build both
/// members through `Workspaces` directly so no recorder factory fills them.
#[gpui::test]
fn a_placeholder_paints_the_marker_for_the_active_member_only(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let a = services
        .workspaces
        .split_active(crate::tiling::Orientation::Horizontal);
    let b = services
        .workspaces
        .stack_active()
        .expect("a focused tile to stack onto");
    let (_window, mut cx) = open_shell(cx, services);

    let a_sel: &'static str = Box::leak(format!("stack-marker-{}", a.0).into_boxed_str());
    let b_sel: &'static str = Box::leak(format!("stack-marker-{}", b.0).into_boxed_str());
    assert!(
        cx.debug_bounds(b_sel).is_some(),
        "the active, visible member paints the marker"
    );
    assert!(
        cx.debug_bounds(a_sel).is_none(),
        "the hidden member paints nothing (it isn't even rendered)"
    );
}

/// Filling a stacked placeholder recreates its occupant under the same tile ID. Clear
/// cached stack delivery for that ID so the new occupant receives its initial stack
/// position.
#[gpui::test]
fn a_replaced_occupant_is_re_told_its_position(cx: &mut gpui::TestAppContext) {
    let (mut services, log) = services_with_recorder();
    // Both members are placeholders because the fixture uses `Workspaces` directly.
    services
        .workspaces
        .split_active(crate::tiling::Orientation::Horizontal);
    let b = services
        .workspaces
        .stack_active()
        .expect("a focused tile to stack onto");
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    // `b` is the focused placeholder; `tile::add_rec` fills it in place.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("tile::add_rec".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    assert!(
        stack_events(&log, b).contains(&Some((2, 2))),
        "the newly-filled occupant should have been told its position: {:?}",
        log.borrow()
    );
}
