//! The grouping picker (2026-09-19): the toolbar readout's click, the
//! `frame::grouping` chord (`mod+g`, dispatched here as the literal
//! `alt-g` — `test_services()` builds its keymap with `default_mod()`,
//! the same convention `asof.rs` follows), typeahead + `enter`, the
//! digit jump, a row click, and the two "nothing happens" arms: an
//! empty slot's digit, and a query that matches nothing.

use super::*;
use geode_core::groupings::GroupingSlots;

fn slots() -> GroupingSlots {
    let mut s = GroupingSlots::default();
    s.set(1, vec!["book".into(), "lhu".into()]);
    s.set(3, vec!["underlying_ref".into()]);
    s
}

/// A shell whose frame has slots 1 and 3 filled (`replace_slots`, the
/// hot-reload door — the fixture's config carries no `groupings` doc).
fn open_with_slots(
    cx: &mut gpui::TestAppContext,
) -> (
    gpui::WindowHandle<Root>,
    gpui::VisualTestContext,
    Entity<ShellView>,
    Entity<crate::frame::Frame>,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        assert!(f.replace_slots(slots()));
        cx.notify();
    });
    vcx.run_until_parked();
    (window, vcx, shell, frame)
}

/// Clicking the toolbar's grouping readout opens the picker; typing
/// narrows it to one slot and `enter` activates that slot, the modal
/// closing and the readout following.
#[gpui::test]
fn clicking_the_readout_opens_the_picker_and_enter_activates_the_typed_slot(
    cx: &mut gpui::TestAppContext,
) {
    let (_window, mut vcx, shell, frame) = open_with_slots(cx);
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), None);

    let readout = vcx
        .debug_bounds("scope-grouping")
        .expect("the grouping readout is painted");
    vcx.simulate_click(readout.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.grouping_picker.is_some()));
    assert!(vcx.debug_bounds("grouping-choice-list").is_some());
    assert!(vcx.debug_bounds("grouping-choice-view default").is_some());
    assert!(vcx.debug_bounds("grouping-choice-1 · book / lhu").is_some());
    assert!(
        vcx.debug_bounds("grouping-choice-3 · underlying_ref")
            .is_some()
    );
    assert!(
        vcx.debug_bounds("grouping-choice-2 · ").is_none(),
        "an empty slot is not a row"
    );
    assert!(vcx.debug_bounds("grouping-hints").is_some());
    // The field keeps the focus the open gave it through the rest of
    // the mouse-down (`open_shell_dialog_with_key`'s `prevent_default`),
    // which is what lets the typing below reach it.
    let focused = vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.read(cx).focus_handle(cx).is_focused(window)
    });
    assert!(focused, "the field holds focus after the click");

    vcx.simulate_input("under");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("grouping-choice-view default").is_none());
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), Some(3));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    assert!(shell.read_with(&vcx, |s, _| s.grouping_picker.is_none()));
}

/// `mod+g` opens the same picker the readout does, on the frame's
/// current slot; a bare `enter` there changes nothing.
#[gpui::test]
fn mod_g_opens_the_picker_on_the_active_slot(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = open_with_slots(cx);
    frame.update(&mut vcx, |f, cx| {
        assert!(f.set_active_slot(Some(3)));
        cx.notify();
    });
    vcx.run_until_parked();
    let before = frame.read_with(&vcx, |f, _| f.versions().grouping);

    vcx.simulate_keystrokes("alt-g");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .grouping_picker
            .as_ref()
            .and_then(|p| p.highlighted_slot())),
        Some(Some(3))
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), Some(3));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.versions().grouping),
        before,
        "re-picking the active slot bumps nothing"
    );
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

/// A digit on the empty field is the chord's twin: `1` activates slot 1
/// and closes; `2` (empty) is refused and the picker stays open; `0`
/// returns to the view default.
#[gpui::test]
fn a_digit_jumps_to_a_filled_slot_and_zero_to_the_view_default(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = open_with_slots(cx);

    vcx.simulate_keystrokes("alt-g");
    vcx.simulate_keystrokes("2");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.grouping_picker.is_some()),
        "an empty slot's digit does nothing"
    );
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), None);

    vcx.simulate_keystrokes("1");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), Some(1));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));

    vcx.simulate_keystrokes("alt-g");
    vcx.simulate_keystrokes("0");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), None);
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

/// A digit typed into a non-empty field is text, never a jump.
#[gpui::test]
fn a_digit_after_text_filters_rather_than_jumps(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = open_with_slots(cx);
    vcx.simulate_keystrokes("alt-g");
    vcx.simulate_input("book");
    vcx.simulate_keystrokes("1");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.grouping_picker.is_some()));
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), None);
    let query = shell.read_with(&vcx, |s, _| {
        s.grouping_picker
            .as_ref()
            .map(|p| p.list.query().to_string())
    });
    assert_eq!(query.as_deref(), Some("book1"));
}

/// A row click activates that row's slot (a pick, not the dialogs'
/// `tab`), resolved through the RANKED order after a filter.
#[gpui::test]
fn a_row_click_activates_that_slot(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = open_with_slots(cx);
    vcx.simulate_keystrokes("alt-g");
    vcx.simulate_input("under");
    vcx.run_until_parked();
    let row = vcx
        .debug_bounds("grouping-choice-3 · underlying_ref")
        .expect("the filtered row");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), Some(3));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

/// A query that matches nothing leaves nothing lit: `enter` keeps the
/// picker open and the frame unchanged; `escape` closes it.
#[gpui::test]
fn enter_with_no_match_does_nothing_and_escape_closes(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = open_with_slots(cx);
    vcx.simulate_keystrokes("alt-g");
    vcx.simulate_input("zzz");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.grouping_picker.is_some()));
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), None);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

/// `enter` picks from the field's LIVE text, not the last `Change` the
/// list saw: a write through `set_value` emits no `Change` (the trap
/// `sync_dialog_text` documents), so without the re-feed the list would
/// still be ranked against an empty query and commit the view default.
#[gpui::test]
fn enter_re_feeds_the_fields_live_text_before_picking(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = open_with_slots(cx);
    vcx.simulate_keystrokes("alt-g");
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.update(cx, |i, cx| i.set_value("under", window, cx));
    });
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .grouping_picker
            .as_ref()
            .and_then(|p| p.highlighted_slot())),
        Some(None),
        "the list has not seen the write yet"
    );
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), Some(3));
}

/// A slot emptied under the open picker (a `groupings.toml` reload —
/// `replace_slots`) commits nothing and says so on the status bar: the
/// rows claimed the slot existed, so silence would read as a pick.
#[gpui::test]
fn picking_a_slot_emptied_under_the_picker_says_so(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = open_with_slots(cx);
    vcx.simulate_keystrokes("alt-g");
    vcx.run_until_parked();
    frame.update(&mut vcx, |f, cx| {
        let mut only_one = GroupingSlots::default();
        only_one.set(1, vec!["book".into()]);
        assert!(f.replace_slots(only_one));
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("3");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.active_slot()), None);
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.notice),
        Some(crate::shell::groupingpicker::SLOT_GONE)
    );
}

/// `tab` completes the field to the highlighted row and keeps typing
/// there — the choice core's rule, with the filter-only dialog writing
/// the field itself.
#[gpui::test]
fn tab_completes_the_field_to_the_highlighted_row(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, _frame) = open_with_slots(cx);
    vcx.simulate_keystrokes("alt-g");
    vcx.simulate_input("under");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    let field = shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(field, "3 · underlying_ref");
    assert!(shell.read_with(&vcx, |s, _| s.grouping_picker.is_some()));
}

/// Hovering the readout names the chord, like the AS OF badge does.
#[gpui::test]
fn hovering_the_readout_names_the_chord(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, _shell, _frame) = open_with_slots(cx);
    let readout = vcx.debug_bounds("scope-grouping").expect("readout painted");
    vcx.simulate_mouse_move(
        readout.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-scope-grouping").is_some());
    assert!(
        vcx.debug_bounds("tip-scope-grouping-chord-mod+g").is_some()
            || vcx.debug_bounds("tip-scope-grouping-chord-alt+g").is_some()
    );
}
