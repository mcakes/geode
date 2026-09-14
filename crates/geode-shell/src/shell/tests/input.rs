//! `handle_key_down`'s own seams. Dispatch's action tail recording
//! (Phase 4b Task 6): every dispatched action's FNV-1a hash lands in
//! `ShellServices::action_tail` before `dispatch` matches the action —
//! the crash hook's only view into "what was the user doing"
//! (`geode_app::crash::install_panic_hook`, `ActionTail`'s own doc
//! comment on why hashes rather than `ActionId`s). And insert mode
//! (market-data spec §8.6): while a focused handle the shell does not own
//! sits under a context reporting `mode == insert`, typing belongs to
//! that input and only single-keystroke bindings resolve.

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

/// Spec §8.6, the whole rule: a tile that owns a focused `Input` gets the
/// keystrokes. `j` is bound — in the module's own fragment — to
/// `rec::down` in `mode == normal`, and the panel's cursor must not move
/// while a trader types a value into a cell; the digits and the decimal
/// point must not feed the matcher's count state either, which is what
/// makes this test fail before the branch exists (a typed `5` left the
/// shell holding a count of 5, waiting to multiply the next motion the
/// trader made after leaving the cell).
///
/// Then `escape`: the module's own insert-mode binding claims it,
/// `rec::cancel` drops the tile's `InputState`, and the shell's existing
/// dropped-focus net (`render`'s `focused(cx).is_none()`) is what turns
/// that back into shell focus — no module touches the shell's own handle.
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

/// The other half of the branch (spec §8.6, the filter field's own rule
/// generalised to a tile): a chord still reaches the shell from inside a
/// tile's input — `ctrl+k` opens the palette — because it resolves as a
/// single keystroke against the live context stack. A binding that has
/// shipped is a promise, insert mode or not.
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

/// The branch keys on the tile actually HOLDING the keyboard, not on its
/// mode alone (`!holds_shell_focus`): with focus back on the shell's own
/// root and the tile still in insert mode, nothing is being typed into an
/// input, so the shell's matcher is in charge again — count prefix and
/// all. `3 enter` therefore commits with `Some(3)`, the ordinary Phase 3
/// §3.3 semantics, where the same two keys typed INTO the input commit
/// with no count at all (the test above).
///
/// This state is reachable in one click: every tile mouse-down re-arms
/// `pending_focus_restore`, so the next frame hands the keyboard back to
/// the shell root while the occupant's own editor is still open. Focus is
/// set on the handle directly here, as `escape_in_the_filter_input_
/// returns_focus_to_the_shell_root` does, rather than depending on the
/// tile's on-screen geometry.
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

/// A BARE keystroke in insert mode resolves only against the contexts that
/// themselves carry `mode == insert` — the tile's own (controller ruling,
/// market-data spec §8.6). The shell's own bare-key bindings are the
/// reason: `/` and `:` open the find and command lines from the `tile`
/// context and `shift+d` duplicates the tile from `workspace`, and every
/// one of them is an ordinary character a trader types into a cell.
/// Requiring each future text-entry module to reclaim them one by one in
/// its own fragment is the wrong side of the seam.
///
/// The fragment's `escape`/`enter` still resolve, because the context they
/// name is exactly the one that is kept — `chords_still_dispatch_from_
/// insert_mode` pins the other half, a chord against the full stack.
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
