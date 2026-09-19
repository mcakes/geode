//! Tile stacks (spec 2026-09-19): delivery of the stack position, the
//! visible set, the four verbs and their notice.

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
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(top)
    );
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
    // Same shape as `input.rs`'s I-3 test: focus a module input, then
    // move which tile has focus by a stack verb; the shell must re-arm.
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
