//! Key dispatch records each action's FNV-1a hash in `ShellServices::action_tail`
//! before handling it, providing the crash hook's action history. Insert-mode tests
//! verify that a tile-owned focused input receives typing while shell bindings resolve
//! only as single keystrokes.

use super::*;
use crate::actions::ActionId;
use crate::diagnostics::fnv1a;

#[gpui::test]
fn dispatching_three_actions_leaves_their_hashes_in_the_tail_in_order(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);

    // Three harmless, side-effect-free actions (no window-state
    // assumptions: a single-tile workspace still accepts a focus-left/
    // right dispatch as a no-op, per `apply_workspace_action`'s own doc
    // comment) — the point here is the tail, not what each one does.
    let dispatched = [
        "workspace::focus_left",
        "workspace::focus_right",
        "perf::toggle_overlay",
    ];

    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            for id in dispatched {
                s.dispatch(&ActionId(id.to_string()), None, window, cx);
            }
        });
    });

    let recorded: Vec<u64> = shell.read_with(&vcx, |s, _| {
        s.services.action_tail.lock().unwrap().recent().collect()
    });
    let expected: Vec<u64> = dispatched.iter().map(|id| fnv1a(id)).collect();
    assert_eq!(
        recorded, expected,
        "the tail must hold the dispatched actions' hashes, oldest first"
    );
}

/// Enter insert mode for real: add a `rec` tile, press `i` (the module
/// fragment's own `rec::edit`), and draw — the draw matters, because gpui
/// installs a text-input handler only for a focused `Input` that has been
/// painted, so typing before it would reach nothing at all. Hands back the
/// shell and its focus handle; the tile's `InputState` is read through the
/// factory's cell (`rec_input_value`), never held.
fn enter_insert_mode(
    window: &gpui::WindowHandle<Root>,
    vcx: &mut gpui::VisualTestContext,
) -> (Entity<ShellView>, gpui::FocusHandle) {
    vcx.simulate_keystrokes("ctrl-v");
    let shell = shell_of(window, vcx);
    let shell_focus = shell.read_with(vcx, |s, _| s.focus_handle.clone());
    vcx.simulate_keystrokes("i");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        !vcx.update(|window, _cx| shell_focus.is_focused(window)),
        "fixture check: `i` must have moved focus off the shell root and \
         into the tile's own input"
    );
    (shell, shell_focus)
}

fn dispatched(
    log: &std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    id: &str,
) -> Vec<Option<u32>> {
    log.borrow()
        .iter()
        .filter_map(|r| match r {
            crate::module::recording::Recorded::Dispatch(_, a, count) if a.0 == id => Some(*count),
            _ => None,
        })
        .collect()
}

/// A tile-owned input receives typing in insert mode: normal-mode `j` must not move the
/// recorder, and digits must not enter the shell matcher's count state. Escape resolves
/// through the module fragment, drops the input, and lets the shell's dropped-focus
/// recovery restore root focus.
#[gpui::test]
fn typed_keys_reach_a_tiles_focused_input_in_insert_mode(cx: &mut gpui::TestAppContext) {
    let (services, log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let (shell, shell_focus) = enter_insert_mode(&window, &mut vcx);

    vcx.simulate_input("j1.5");
    assert_eq!(
        rec_input_value(&input, &vcx).as_deref(),
        Some("j1.5"),
        "every typed key must have reached the tile's own input"
    );
    assert!(
        dispatched(&log, "rec::down").is_empty(),
        "no motion may fire while the tile is typing: {:?}",
        log.borrow()
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| (s.matcher.count(), s.matcher.pending().len())),
        (None, 0),
        "the matcher's count and sequence state must never be fed in insert mode"
    );

    vcx.simulate_keystrokes("escape");
    assert_eq!(
        dispatched(&log, "rec::cancel"),
        vec![None],
        "escape is the module's own insert-mode binding: {:?}",
        log.borrow()
    );
    assert!(
        rec_input_value(&input, &vcx).is_none(),
        "cancel must have dropped the tile's input"
    );
    // One draw: the net runs at the top of `render`, so the very next
    // frame after the cancel is the one that takes focus back.
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vcx.update(|window, _cx| shell_focus.is_focused(window)),
        "the shell's own dropped-focus net must have taken focus back"
    );
}

/// Chords still resolve against the live context stack while a tile input is focused:
/// `ctrl+k` opens the palette as a single-keystroke binding.
#[gpui::test]
fn chords_still_dispatch_from_insert_mode(cx: &mut gpui::TestAppContext) {
    let (services, _log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let (shell, _shell_focus) = enter_insert_mode(&window, &mut vcx);

    vcx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&vcx, |s, _| s.palette.is_some()),
        "ctrl+k must still open the palette from a tile's own input"
    );
    assert_eq!(
        rec_input_value(&input, &vcx).as_deref(),
        Some(""),
        "a dispatched chord must not also type itself into the input"
    );
}

/// A count prefix typed into a cell is text, not a count (the filter
/// field's own rule, `single_keystroke_binding`'s doc comment): the
/// matcher is never fed, so nothing accumulates and nothing leaks into
/// the next action the module DOES claim. Before the branch, the `7` below
/// became a count and `enter` committed the cell with `Some(7)` — an
/// argument the trader never typed.
#[gpui::test]
fn a_count_prefix_typed_in_insert_mode_is_text_not_a_count(cx: &mut gpui::TestAppContext) {
    let (services, log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let (shell, _shell_focus) = enter_insert_mode(&window, &mut vcx);

    vcx.simulate_input("3");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.matcher.count()),
        None,
        "a digit typed into a tile's input must not start a count"
    );
    vcx.simulate_input("j");
    assert_eq!(
        rec_input_value(&input, &vcx).as_deref(),
        Some("3j"),
        "both keys are text"
    );
    assert!(
        dispatched(&log, "rec::down").is_empty(),
        "{:?}",
        log.borrow()
    );

    // And the count cannot leak into the one key the module does claim:
    // `enter` commits the cell with no count, not with the digit typed
    // into it a moment earlier.
    vcx.simulate_input("7");
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        dispatched(&log, "rec::commit"),
        vec![None],
        "commit must carry no count: {:?}",
        log.borrow()
    );
}

/// Insert-mode input handling requires the occupant to hold keyboard focus. With focus
/// on the shell root, the normal matcher handles count prefixes even if the editor
/// remains open: `3 enter` dispatches with `Some(3)`. Set focus directly to isolate
/// dispatch from tile-click focus restoration.
#[gpui::test]
fn the_insert_branch_needs_the_tile_to_hold_focus_not_just_insert_mode(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let (_shell, shell_focus) = enter_insert_mode(&window, &mut vcx);

    vcx.update(|window, cx| shell_focus.focus(window, cx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        rec_input_value(&input, &vcx).is_some(),
        "fixture check: the tile's editor must still be open, so the tile \
         is still reporting insert mode"
    );

    vcx.simulate_keystrokes("3 enter");
    assert_eq!(
        dispatched(&log, "rec::commit"),
        vec![Some(3)],
        "with the shell holding the keyboard the matcher governs the count: {:?}",
        log.borrow()
    );
}

/// A mouse-down on the tile's own view, at a point away from its input
/// — what a real double-click's second press lands on — through the
/// tile cell's listener, which arms `pending_focus_restore`. The view's
/// `track_focus` handle takes window focus on the same press, exactly as
/// the market-data panel's own view does, so the editor focus is
/// re-taken afterwards by the caller when the flow under test does that
/// (a double-click opens the editor on the CLICK, after the mouse-down
/// has already moved focus).
fn mouse_down_on_the_rec_tile(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext) {
    let tile = shell.read_with(vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap().0
    });
    let selector: &'static str = Box::leak(format!("tile-content-{tile}").into_boxed_str());
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let bounds = vcx.debug_bounds(selector).expect("the rec tile is painted");
    let at = gpui::point(
        bounds.origin.x + bounds.size.width - px(8.),
        bounds.origin.y + bounds.size.height - px(8.),
    );
    vcx.simulate_mouse_down(at, MouseButton::Left, gpui::Modifiers::none());
    vcx.simulate_mouse_up(at, MouseButton::Left, gpui::Modifiers::none());
}

/// Pending root-focus restoration is consumed without moving focus only when the
/// focused occupant owns the focused input in insert mode.
///
/// First exercise the recorder's press: its tracked view handle takes focus, which is
/// not its editor handle, so restoration runs. Then focus the editor and arm
/// restoration in the same event, matching a module that opens an editor on click.
/// Typing after the frame proves that an owned editor keeps the keyboard.
#[gpui::test]
fn a_tile_in_insert_mode_keeps_focus_through_the_mouse_down_restore(cx: &mut gpui::TestAppContext) {
    let (services, _log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let (shell, shell_focus) = enter_insert_mode(&window, &mut vcx);
    let editor = input.borrow().clone().expect("the editor is open");

    mouse_down_on_the_rec_tile(&shell, &mut vcx);
    assert!(
        !shell.read_with(&vcx, |s, _| s.pending_focus_restore),
        "the flag is consumed on the frame the press schedules"
    );
    assert!(
        vcx.update(|window, _cx| shell_focus.is_focused(window)),
        "the recorder's press moved focus onto its VIEW's handle, which is not an input the \
         occupant holds: ownership fails, so the restore runs even with an editor open"
    );

    vcx.update(|window, cx| editor.read(cx).focus_handle(cx).focus(window, cx));
    shell.update(&mut vcx, |s, _| s.pending_focus_restore = true);
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        !shell.read_with(&vcx, |s, _| s.pending_focus_restore),
        "the flag is consumed either way"
    );
    assert!(
        vcx.update(|window, cx| editor.read(cx).focus_handle(cx).is_focused(window)),
        "the editor keeps the keyboard: the restore was skipped, not deferred"
    );
    vcx.simulate_input("x");
    assert_eq!(
        rec_input_value(&input, &vcx).as_deref(),
        Some("x"),
        "and typing still reaches it"
    );
}

/// With insert mode off, pressing a tile's tracked handle restores focus to the shell
/// root on the next frame. A focused occupant view alone is insufficient to suppress
/// restoration; the occupant must own an input in insert mode.
#[gpui::test]
fn a_tile_out_of_insert_mode_still_hands_focus_back_on_a_mouse_down(cx: &mut gpui::TestAppContext) {
    let (services, _log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let (shell, shell_focus) = enter_insert_mode(&window, &mut vcx);
    // `escape` is the fragment's `rec::cancel`: blur, drop, insert off.
    vcx.simulate_keystrokes("escape");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        rec_input_value(&input, &vcx).is_none(),
        "fixture check: editor closed"
    );

    // The same press as the test above — the only difference is the mode.
    mouse_down_on_the_rec_tile(&shell, &mut vcx);
    assert!(
        vcx.update(|window, _cx| shell_focus.is_focused(window)),
        "out of insert mode the restore runs and the shell root has the keyboard"
    );
}

/// Focus ownership is checked against the currently focused tile. Move from an editor
/// in A to one in B and back to A: A still reports insert mode, but B owns the focused
/// input. Restoration must return the keyboard to the shell root, where `3` becomes a
/// count and Enter reaches A without typing into B.
#[gpui::test]
fn an_abandoned_editor_in_the_focused_tile_does_not_keep_another_tiles_field_focused(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let shell_focus = shell.read_with(&vcx, |s, _| s.focus_handle.clone());
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    };

    vcx.simulate_keystrokes("ctrl-v");
    let a = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    vcx.simulate_keystrokes("ctrl-v");
    let b = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_ne!(a, b, "fixture check: two tiles");

    // `i` in A, `mod+l` (`alt+l` is `workspace::focus_right`), `i` in B.
    vcx.simulate_keystrokes("alt-h");
    vcx.simulate_keystrokes("i");
    draw(&mut vcx);
    vcx.simulate_keystrokes("alt-l");
    draw(&mut vcx);
    assert!(
        vcx.update(|window, _cx| shell_focus.is_focused(window)),
        "fixture check: I-3 — the move out of A handed the keyboard back"
    );
    vcx.simulate_keystrokes("i");
    draw(&mut vcx);
    let b_editor = input.borrow().clone().expect("B's editor is open");
    assert!(
        vcx.update(|window, cx| b_editor.read(cx).focus_handle(cx).is_focused(window)),
        "fixture check: B's field holds the keyboard"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.services.workspaces.active().focused_tile()),
        Some(b)
    );

    // `mod+h`: the ring goes to A, whose abandoned editor still claims
    // insert mode. The restore must run regardless.
    vcx.simulate_keystrokes("alt-h");
    draw(&mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.services.workspaces.active().focused_tile()),
        Some(a),
        "fixture check: the ring moved to A"
    );
    assert!(
        shell.read_with(&vcx, |s, cx| {
            s.occupants
                .get(&a)
                .unwrap()
                .content
                .key_context(cx)
                .get("mode")
                == Some("insert")
        }),
        "fixture check: A's abandoned editor still reports insert mode"
    );
    assert!(
        vcx.update(|window, _cx| shell_focus.is_focused(window)),
        "the restore ran: A does not hold B's field, so B's field does not keep the keyboard"
    );

    // The matcher governs: a bare key is a count, typed into nobody's
    // cell, and the next action is the FOCUSED tile's own.
    vcx.simulate_input("3");
    assert_eq!(
        rec_input_value(&input, &vcx).as_deref(),
        Some(""),
        "nothing may type itself into B's abandoned-by-focus field"
    );
    assert_eq!(shell.read_with(&vcx, |s, _| s.matcher.count()), Some(3));
    vcx.simulate_keystrokes("enter");
    let commits: Vec<(TileId, Option<u32>)> = log
        .borrow()
        .iter()
        .filter_map(|r| match r {
            crate::module::recording::Recorded::Dispatch(tile, action, count)
                if action.0 == "rec::commit" =>
            {
                Some((*tile, *count))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        commits,
        vec![(a, Some(3))],
        "the count reaches the focused tile's own action, and only its: {:?}",
        log.borrow()
    );
}

/// Keyboard tile navigation restores root focus when leaving a focused editor.
/// Otherwise a count or command for the newly focused tile could also type into the
/// previous editor. The editor remains open until commit or cancel; returning to its
/// tile and pressing Escape still cancels it.
#[gpui::test]
fn a_keyboard_focus_move_hands_the_keyboard_back_to_the_shell(cx: &mut gpui::TestAppContext) {
    let (services, log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    // A second tile to move to — `enter_insert_mode` adds the first and
    // opens its editor.
    vcx.simulate_keystrokes("ctrl-v");
    let (shell, shell_focus) = enter_insert_mode(&window, &mut vcx);

    // `mod` is alt (`defaults::default_mod`): `alt+h` is
    // `workspace::focus_left`, a chord, so it resolves against the whole
    // stack even from inside the editor. It moves focus to the FIRST
    // tile, which is in normal mode — the second one keeps its editor.
    let editing_tile =
        shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().focused());
    vcx.simulate_keystrokes("alt-h");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_ne!(
        shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().focused()),
        editing_tile,
        "fixture check: the focused tile must really have moved"
    );
    assert!(
        vcx.update(|window, _cx| shell_focus.is_focused(window)),
        "the keyboard belongs to the shell once the focused tile moved"
    );
    assert!(
        rec_input_value(&input, &vcx).is_some(),
        "the editor is orphaned, not closed: commit or cancel owns that"
    );

    // With root focus restored, the matcher owns the keyboard: a digit becomes a count
    // without typing into the abandoned editor.
    vcx.simulate_input("3");
    assert_eq!(
        rec_input_value(&input, &vcx).as_deref(),
        Some(""),
        "nothing may type itself into the abandoned cell"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.matcher.count()),
        Some(3),
        "the count belongs to the matcher once the shell holds the keyboard"
    );
    // The newly focused tile is in NORMAL mode (insert is per tile —
    // the editor belongs to the one left behind), so its own normal-mode
    // `j` is the action the count reaches, and the abandoned editor's
    // `enter` is out of reach until focus goes back there.
    vcx.simulate_keystrokes("j");
    assert_eq!(
        dispatched(&log, "rec::down"),
        vec![Some(3)],
        "and it reaches the action, exactly as it would after a click: {:?}",
        log.borrow()
    );
    assert!(dispatched(&log, "rec::commit").is_empty());
}

/// Bare keys in a tile input resolve only against contexts that themselves declare
/// insert mode. Shell commands such as `/`, `:`, and `shift+d` are ordinary input
/// characters there. The module's Escape and Enter bindings still resolve, while the
/// separate chord test checks resolution against the full context stack.
#[gpui::test]
fn bare_shell_keys_are_text_in_insert_mode(cx: &mut gpui::TestAppContext) {
    let (services, _log, input) = services_with_an_insert_recorder(REC_INSERT_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let (shell, _shell_focus) = enter_insert_mode(&window, &mut vcx);
    let tiles = |vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| {
            s.services.workspaces.active().tree().tiles().len()
        })
    };
    let before = tiles(&vcx);

    // `/` is `tile::find` and `:` is `tile::command_line` in the shipped
    // keymap; `shift+d` is `workspace::duplicate_horizontal` (a bare key
    // by `Modifiers::is_chord`'s rule — shift alone is typing).
    vcx.simulate_input("1/2:x");
    vcx.simulate_keystrokes("shift-d");

    let value = rec_input_value(&input, &vcx).expect("the editor must still be open");
    assert_eq!(
        value, "1/2:xD",
        "every bare key must have been typed into the tile's input — `/`, `:` \
         and the shifted `D` included"
    );
    assert!(
        shell.read_with(&vcx, |s, _| s.command_line.is_none()),
        "neither the find nor the command line may open behind a trader's typing"
    );
    assert_eq!(
        tiles(&vcx),
        before,
        "a typed `D` must not duplicate the tile"
    );
}

/// GPUI dispatches matched framework bindings before `on_key_down`. The shell's
/// `GeodeShell` context reclaims Tab and Shift-Tab from the component root's focus
/// cycling so module bindings can receive them. Exercise a recorder Tab binding inside
/// a real `Root` and assert it reaches the focused occupant.
#[gpui::test]
fn tab_reaches_a_focused_tiles_own_binding_rather_than_roots_focus_cycling(
    cx: &mut gpui::TestAppContext,
) {
    let (mut services, log) = test_services_with_log();
    // `rec::down` is one of the five verbs `RecordingFactory` registers,
    // so the binding really resolves (`build_keymap` drops a binding
    // whose action nothing registered).
    let tab_layer = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:tab>".into(),
        table: "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"tab\" = \"rec::down\"\n"
            .parse()
            .unwrap(),
    };
    services.keymap = test_keymap(&services.registry, &[tab_layer]);

    let (_window, mut vcx) = open_shell(cx, services);
    vcx.simulate_keystrokes("ctrl-v"); // a recorder tile, focused
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    vcx.simulate_keystrokes("tab");

    assert!(
        log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(_, a, _) if a.0 == "rec::down"
        )),
        "a bare `tab` must reach the focused tile's own binding, not `Root`'s \
         focus cycling: {:?}",
        log.borrow()
    );
}
