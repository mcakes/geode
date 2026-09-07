//! The as-of selector and the historical indicator (Phase 4a §3.6,
//! §3.11): the real key-dispatch pipeline through `frame::as_of`
//! (`mod+t`), the field's live parse preview/error, preset selection, and
//! `frame::live`/`frame::as_of_undo` — plus the window-wide warning
//! stripe and the status-bar segment that must paint if and only if the
//! frame is historical (spec §4.5: nothing on screen may look live when
//! it is not).
//!
//! `mod+t` is dispatched here as the literal `alt-t` — `test_services()`
//! builds its keymap with `default_mod()` (spec §3.1: Alt), the same
//! convention every other `mod+`-bound e2e test in this crate follows
//! (see `scopebar.rs`'s `typing_in_the_field_...` test's own comment).

use super::*;
use crate::frame::Publish;
use geode_core::query::AsOf;

#[gpui::test]
fn typing_a_time_and_enter_sets_as_of_and_paints_the_stripe_and_segment(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(vcx.debug_bounds("as-of-stripe").is_none());
    vcx.simulate_keystrokes("alt-t");
    vcx.simulate_input("14:05");
    vcx.simulate_keystrokes("enter");
    assert!(matches!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::At(_)
    ));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-stripe").is_some());
    assert!(vcx.debug_bounds("status-as-of").is_some());
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

/// A bad time shows inline (`as-of-error`) and `enter` does nothing: the
/// modal stays open and the frame stays live — the mutation entry "as-of:
/// a bad time never sets the frame" pins the failure arm never calling
/// `set_as_of` on any input.
#[gpui::test]
fn a_bad_time_shows_inline_and_enter_does_nothing(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    vcx.simulate_keystrokes("alt-t");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()));
    vcx.simulate_input("nope");
    vcx.simulate_keystrokes("enter");

    assert!(
        shell.read_with(&vcx, |s, _| s.modal.is_some()),
        "enter on a bad time must not close the modal"
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::Live,
        "a bad time must never set the frame"
    );
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("as-of-error").is_some(),
        "the parse failure must be painted inline"
    );
    assert!(
        vcx.debug_bounds("as-of-stripe").is_none(),
        "the frame is still live, so the stripe must not paint"
    );
}

/// Selecting a preset by keyboard sets that instant; `frame::live`
/// returns to live, remembering the previous as-of; `frame::as_of_undo`
/// swaps back to it (spec §3.6 — a toggle, not a stack).
#[gpui::test]
fn selecting_a_preset_sets_that_instant_and_live_then_undo_returns(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    let newest = chrono::Utc::now();
    let second_newest = newest - chrono::Duration::seconds(30);
    frame.update(&mut vcx, |f, _| {
        // Oldest first, so `note_published` (newest-first `VecDeque`)
        // ends up with `newest` at index 0 and `second_newest` at index 1
        // — the row `down` should land on.
        f.note_published(Publish {
            dataset: "risk".into(),
            batch: "EOD".into(),
            books: 3,
            at: second_newest,
        });
        f.note_published(Publish {
            dataset: "risk".into(),
            batch: "EOD".into(),
            books: 1,
            at: newest,
        });
    });

    vcx.simulate_keystrokes("alt-t");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()));

    // The field is empty; `down` moves off the newest preset (index 0)
    // onto the second-newest (index 1), and a bare `enter` commits it.
    vcx.simulate_keystrokes("down enter");

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::At(second_newest),
        "'down enter' should have picked the second-newest preset"
    );
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));

    // `frame::live` (palette-only — dispatched directly, the same route
    // `shell::tests::picker`'s own `dispatch_action` helper uses for
    // other palette-only actions) returns to live and remembers the
    // as-of it just left.
    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("frame::live".to_string()), None, window, cx);
        });
    });
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::Live);

    // `frame::as_of_undo` swaps back to the instant `frame::live` just
    // left.
    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("frame::as_of_undo".to_string()), None, window, cx);
        });
    });
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::At(second_newest)
    );
}
