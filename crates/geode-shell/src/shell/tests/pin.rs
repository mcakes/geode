//! Workspace-pinned frame: which lane a shell surface reads and commits to.

use super::occupants::dispatch_and_draw;
use super::*;
use crate::frame::{FLIP_DEADLINE, Frame};
use crate::tiling::WorkspaceIx;
use std::time::Instant;

fn slots() -> geode_core::groupings::GroupingSlots {
    let mut s = geode_core::groupings::GroupingSlots::default();
    s.set(1, vec!["book".into()]);
    s.set(2, vec!["lhu".into()]);
    s
}

/// A shell whose frame has grouping slots 1 and 2 to activate.
fn open_pinnable(
    cx: &mut gpui::TestAppContext,
) -> (
    gpui::WindowHandle<Root>,
    gpui::VisualTestContext,
    Entity<ShellView>,
    Entity<Frame>,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.replace_slots(slots());
        cx.notify();
    });
    vcx.run_until_parked();
    (window, vcx, shell, frame)
}

fn ws(n: u8) -> WorkspaceIx {
    WorkspaceIx::new(n).unwrap()
}

/// A frame dialog commits into the workspace it was opened from, even if
/// the active workspace changed underneath it.
#[gpui::test]
fn a_frame_dialog_commits_into_the_workspace_it_opened_from(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let ws1 = WorkspaceIx::FIRST;
    let ws2 = WorkspaceIx::new(2).unwrap();
    frame.update(&mut vcx, |f, _| assert!(f.pin(ws1)));
    dispatch_and_draw(&shell, &mut vcx, "frame::scope_expression");
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
    // Test-only: move the active workspace underneath the open modal.
    shell.update(&mut vcx, |s, _| assert!(s.services.workspaces.switch(2)));
    vcx.simulate_input("book = 'BK000'");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let (pinned, shared) = frame.read_with(&vcx, |f, _| {
        (
            f.view(ws1).scope().expression.is_some(),
            f.view(ws2).scope().expression.is_some(),
        )
    });
    assert!(pinned, "the expression lands in workspace 1's pinned lane");
    assert!(!shared, "the shared lane is untouched");
}

#[gpui::test]
fn the_pin_action_toggles_the_active_workspace(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(ws(2))));
    assert!(!frame.read_with(&vcx, |f, _| f.is_pinned(ws(1))));
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    assert!(!frame.read_with(&vcx, |f, _| f.is_pinned(ws(2))));
}

#[gpui::test]
fn clicking_the_pin_glyph_pins_the_active_workspace(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, _shell, frame) = open_pinnable(cx);
    let bounds = vcx.debug_bounds("scope-pin").expect("the pin glyph paints");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(WorkspaceIx::FIRST)));
}

#[gpui::test]
fn keys_in_a_pinned_workspace_stay_there(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("ctrl-1");
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws(2)).active_slot()),
        Some(1)
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws(1)).active_slot()),
        None
    );
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_1");
    vcx.simulate_keystrokes("ctrl-2");
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws(2)).active_slot()),
        Some(1),
        "a shared-lane edit does not reach the pinned workspace"
    );
}

#[gpui::test]
fn switching_workspace_reseeds_the_barrier_without_opening_one(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("ctrl-1");
    vcx.run_until_parked();
    frame.update(&mut vcx, |f, _| {
        f.sweep(Instant::now() + FLIP_DEADLINE * 2);
    });
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_1");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.last_flip_versions),
        frame.read_with(&vcx, |f, _| f.view(ws(1)).versions()),
    );
    // The switch itself notifies nothing; a later non-flip notification is
    // what compares against the baseline. Unseeded, it would read workspace
    // 2's numbers against workspace 1's lane and open a barrier.
    frame.update(&mut vcx, |f, cx| {
        f.note_config_reloaded();
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_open()),
        "a switch shows another lane; it is not a frame change to flip"
    );
}

#[gpui::test]
fn typing_across_a_workspace_switch_lands_in_each_lane(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx, shell, frame) = open_pinnable(cx);
    vcx.update(|window, _| window.activate_window());
    let _ = window;
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("sp");
    vcx.simulate_keystrokes("alt-1"); // switch from inside the field
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("nd");
    let (two, one) = frame.read_with(&vcx, |f, _| {
        (
            f.view(ws(2)).scope().text.clone(),
            f.view(ws(1)).scope().text.clone(),
        )
    });
    assert_eq!(two.as_deref(), Some("sp"));
    assert_eq!(
        one.as_deref(),
        Some("nd"),
        "the field shows and edits the new lane"
    );
}

/// A switch from inside the field rebinds its session to the new lane, so
/// Escape restores the new lane's own text rather than writing the old
/// lane's entry text over it.
#[gpui::test]
fn escape_after_a_switch_restores_the_new_lanes_text(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx, shell, frame) = open_pinnable(cx);
    vcx.update(|window, _| window.activate_window());
    let _ = window;
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_text(Some("one".into()));
        cx.notify();
    });
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("sp");
    vcx.simulate_keystrokes("alt-1"); // switch from inside the field
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    let (two, one) = frame.read_with(&vcx, |f, _| {
        (
            f.view(ws(2)).scope().text.clone(),
            f.view(ws(1)).scope().text.clone(),
        )
    });
    assert_eq!(two.as_deref(), Some("sp"), "the old lane keeps its typing");
    assert_eq!(
        one.as_deref(),
        Some("one"),
        "Escape restores the new lane's text, not the old lane's entry text"
    );
}

#[gpui::test]
fn unpinning_mid_session_leaves_the_shared_history_clean(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx, shell, frame) = open_pinnable(cx);
    vcx.update(|window, _| window.activate_window());
    let _ = window;
    frame.update(&mut vcx, |f, _| {
        f.shared_mut().set_active_slot(Some(2));
    });
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("pinned");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace"); // unpin
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None,
        "pinned text never reached the shared lane"
    );
    assert!(
        !frame.update(&mut vcx, |f, _| f.shared_mut().undo_scope()),
        "and left no undo entry there"
    );
}

/// Pinning ends the shared lane's open text session before the field moves
/// to the pinned lane. A session whose edits returned to their base has
/// pushed that base; only ending it pops the no-op entry again.
#[gpui::test]
fn a_pin_mid_session_ends_the_shared_session(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx, shell, frame) = open_pinnable(cx);
    vcx.update(|window, _| window.activate_window());
    let _ = window;
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("ab");
    vcx.simulate_keystrokes("backspace backspace");
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None,
        "the edits returned to the base"
    );
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(WorkspaceIx::FIRST)));
    assert!(
        !frame.update(&mut vcx, |f, _| f.shared_mut().undo_scope()),
        "the shared session ended with no undo entry left behind"
    );
}

#[gpui::test]
fn a_lane_changed_while_hidden_promotes_without_waiting(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_1");
    // Workspace 2's lane changes while hidden (a hot reload bumps it).
    frame.update(&mut vcx, |f, cx| {
        f.view_mut(ws(2)).set_text(Some("hidden".into()));
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_open()),
        "no barrier for a lane nobody can see"
    );
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    // A non-flip notification after the switch compares the shown lane
    // against the baseline the switch re-seeded.
    frame.update(&mut vcx, |f, cx| {
        f.note_config_reloaded();
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_open()),
        "showing it opens none either; its tiles requery and promote directly"
    );
}

/// Between two unpinned workspaces the field edits the shared lane
/// throughout, so a switch from inside the field leaves its session whole:
/// Escape restores the pre-focus text and leaves no undo entry behind.
#[gpui::test]
fn a_switch_between_unpinned_workspaces_keeps_the_field_session(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx, _shell, frame) = open_pinnable(cx);
    vcx.update(|window, _| window.activate_window());
    let _ = window;
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("ab");
    vcx.simulate_keystrokes("alt-3"); // switch from inside the field
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("c");
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("abc"),
        "the field kept focus and kept editing the shared lane"
    );
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None,
        "escape must restore the pre-focus text"
    );
    assert!(
        !frame.update(&mut vcx, |f, _| f.shared_mut().undo_scope()),
        "the whole session left no undo entry"
    );
}
