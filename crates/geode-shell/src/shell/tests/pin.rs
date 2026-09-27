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

/// Add a recorder tile to the active workspace. A flip barrier opens only
/// over visible occupied tiles, so a barrier assertion without one can
/// never fail.
fn add_tile(vcx: &mut gpui::VisualTestContext) {
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// A frame dialog opened in a pinned workspace commits into that
/// workspace's lane, not the shared one. Switching and pinning are refused
/// while it is open, so the lane active when it opened is the lane it
/// commits to.
#[gpui::test]
fn a_frame_dialog_commits_into_the_lane_active_when_it_opened(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let ws1 = WorkspaceIx::FIRST;
    frame.update(&mut vcx, |f, _| assert!(f.pin(ws1)));
    dispatch_and_draw(&shell, &mut vcx, "frame::scope_expression");
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
    vcx.simulate_input("book = 'BK000'");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let (pinned, shared) = frame.read_with(&vcx, |f, _| {
        (
            f.view(ws1).scope().expression.is_some(),
            f.shared().scope().expression.is_some(),
        )
    });
    assert!(pinned, "the expression lands in workspace 1's pinned lane");
    assert!(!shared, "the shared lane is untouched");
}

/// The palette runs non-dialog actions behind an open dialog, but a
/// workspace switch or a pin toggle there would move the lane the toolbar
/// and the dialog read out from under the dialog. Both are refused with the
/// close-the-dialog notice, leaving the workspace and the pin unchanged.
#[gpui::test]
fn switching_and_pinning_are_refused_behind_a_dialog(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    for (query, id) in [
        ("Switch to workspace 2", "workspace::switch_2"),
        (
            "Toggle the frame pin for this workspace",
            "frame::pin_workspace",
        ),
    ] {
        dispatch_and_draw(&shell, &mut vcx, "frame::scope_expression");
        assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
        vcx.simulate_keystrokes("ctrl-k");
        vcx.simulate_input(query);
        let selected = shell.read_with(&vcx, |s, _| s.palette.as_ref().unwrap().selected_item());
        assert!(
            matches!(&selected, Some(crate::palette::PaletteItem::Action(a, ..)) if a.0 == id),
            "{id}: {selected:?}"
        );
        vcx.simulate_keystrokes("enter");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.notice),
            Some(crate::shell::input::CLOSE_DIALOG_FIRST),
            "{id} must be refused with the notice"
        );
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.active_ix()),
            WorkspaceIx::FIRST,
            "{id} must not switch the workspace"
        );
        assert!(
            !frame.read_with(&vcx, |f, _| f.is_pinned(WorkspaceIx::FIRST)),
            "{id} must not pin"
        );
        assert!(
            shell.read_with(&vcx, |s, _| s.modal_open()),
            "{id}: the dialog stays open"
        );
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                while s.modal_open() {
                    s.close_modal(window, cx);
                }
            });
        });
    }
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
    add_tile(&mut vcx); // workspace 1, the one switched back to
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

/// After an unpin drops a pinned typing session, `mod+z` through the keymap
/// undoes the shared lane's own earlier edit; the pinned text never
/// surfaces in the shared lane, neither on Enter nor as an undo target.
#[gpui::test]
fn undo_after_an_unpin_mid_session_undoes_the_shared_edit(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx, shell, frame) = open_pinnable(cx);
    vcx.update(|window, _| window.activate_window());
    let _ = window;
    vcx.run_until_parked();
    // A shared-lane edit through the field, committed.
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("shared");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("shared")
    );
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
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("shared"),
        "Enter after the unpin commits no pinned text"
    );
    vcx.simulate_keystrokes("alt-z"); // mod+z under the default mod
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None,
        "the chord undoes the shared lane's own edit"
    );
    assert!(
        !frame.update(&mut vcx, |f, _| f.shared_mut().undo_scope()),
        "and nothing pinned is left to undo"
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
    add_tile(&mut vcx); // workspace 2, the hidden lane's
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
    let (window, mut vcx, shell, frame) = open_pinnable(cx);
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
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.active_ix()),
        ws(3),
        "the chord switched workspaces, so the kept session is not vacuous"
    );
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

/// A workspace restored with its own `workspaces.N.frame` record starts
/// pinned with that lane, leaves the shared lane untouched, and offers no
/// undo back to the empty scope.
#[gpui::test]
fn a_restored_pinned_workspace_is_pinned_with_its_record(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let mut record = crate::session::FrameRecord {
        scope: geode_core::scope::Scope::default(),
        active_slot: None,
        as_of: geode_core::query::AsOf::Live,
    };
    record.scope.text = Some("spx".into());
    services.restored_pinned.insert(ws(1), record);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(ws(1))));
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.view(ws(1)).scope().text.clone())
            .as_deref(),
        Some("spx")
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None
    );
    assert!(
        !frame.update(&mut vcx, |f, _| f.view_mut(ws(1)).undo_scope()),
        "restore leaves no undo entry"
    );
}

/// A restored pinned lane whose recorded slot is empty in the current
/// slots starts with no active slot. It must not keep the shared lane's
/// slot, which pinning copied before the record applied.
#[gpui::test]
fn a_restored_pin_with_an_empty_slot_drops_the_slot(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let groupings = LayerDoc::builtin("groupings", "1 = [\"book\"]\n2 = [\"lhu\"]\n").unwrap();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![groupings, datasets],
        ..ConfigSources::default()
    });
    let record = |slot| crate::session::FrameRecord {
        scope: geode_core::scope::Scope::default(),
        active_slot: slot,
        as_of: geode_core::query::AsOf::Live,
    };
    services.restored_frame = Some(record(Some(1)));
    services.restored_pinned.insert(ws(1), record(Some(5)));
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().active_slot()),
        Some(1),
        "the shared record's slot applied, so the pinned check is not vacuous"
    );
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(ws(1))));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.view(ws(1)).active_slot()),
        None,
        "slot 5 is empty, so the pinned lane has no active slot"
    );
}

/// A restored pin for a workspace the layout does not hold is skipped. A
/// session read cannot produce one; kept, it would surface as an unexpected
/// pin if that workspace were created later.
#[gpui::test]
fn a_restored_pin_without_its_workspace_is_skipped(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    assert!(
        !services.workspaces.spaces().any(|(ix, _)| ix == 7),
        "fixture must not hold workspace 7"
    );
    let record = crate::session::FrameRecord {
        scope: geode_core::scope::Scope::default(),
        active_slot: None,
        as_of: geode_core::query::AsOf::Live,
    };
    services.restored_pinned.insert(ws(7), record);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(!frame.read_with(&vcx, |f, _| f.is_pinned(ws(7))));
}

/// Pinning, a pinned lane's edits, and unpinning each reach the next session
/// snapshot: the periodic write and the quit save both carry
/// `workspaces.N.frame` while pinned, and an unpin drops it so the workspace
/// restores shared.
#[gpui::test]
fn a_pinned_lane_reaches_both_session_writes_and_an_unpin_drops_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");
    let services = super::session::test_services_with_session(session_path.clone());
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.replace_slots(slots());
        cx.notify();
    });
    vcx.run_until_parked();
    // Baseline: whatever startup left dirty is written first.
    let _ = shell.update(&mut vcx, |s, cx| s.take_dirty_session_write(cx));

    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("ctrl-1");
    vcx.run_until_parked();
    frame.update(&mut vcx, |f, _| {
        let mut s = f.view(ws(2)).scope().clone();
        s.text = Some("spx".into());
        f.view_mut(ws(2)).set_scope(s);
    });

    let (_, text) = shell
        .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
        .expect("a pin and a pinned-lane edit dirty the session");
    let restored = crate::session::from_toml(&text.parse().unwrap()).unwrap();
    let record = restored
        .pinned
        .get(&ws(2))
        .expect("the periodic write carries workspace 2's lane");
    assert_eq!(record.scope.text.as_deref(), Some("spx"));
    assert_eq!(record.active_slot, Some(1));
    assert!(!restored.pinned.contains_key(&ws(1)));
    assert_eq!(
        restored.frame.as_ref().and_then(|r| r.scope.text.clone()),
        None,
        "the pinned lane's scope is not the shared record's"
    );

    shell.read_with(&vcx, |s, cx| s.save_session(cx));
    let saved = crate::session::load(&session_path);
    assert_eq!(saved.pinned.get(&ws(2)), Some(record), "the quit save too");

    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    let (_, text) = shell
        .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
        .expect("an unpin dirties the session");
    let restored = crate::session::from_toml(&text.parse().unwrap()).unwrap();
    assert!(
        restored.pinned.is_empty(),
        "an unpinned workspace writes no frame"
    );
}
