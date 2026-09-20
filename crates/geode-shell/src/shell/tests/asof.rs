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

/// Hovering the status bar's AS OF segment (Task 4, spec §5.1) names the
/// same `frame::as_of` chord the toolbar's own AS OF badge does — the
/// keyboard twin to "click it to reopen the as-of selector". As-of dialog
/// Part 3 (2026-09-20) replaced the free-text grammar this test used to
/// type through (`"14:05"` + `enter`) with the ranked-list model: `enter`
/// alone on an empty field commits the highlighted row, which is always
/// the first business-day preset on open — still a historical instant,
/// which is all this test needs to make the segment paint.
#[gpui::test]
fn hovering_the_status_as_of_segment_names_the_selector_chord(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let _frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("alt-t");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    let seg = vcx.debug_bounds("status-as-of").expect("segment painted");
    vcx.simulate_mouse_move(
        seg.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-status-as-of").is_some());
    assert!(
        vcx.debug_bounds("tip-status-as-of-chord-mod+t").is_some()
            || vcx.debug_bounds("tip-status-as-of-chord-alt+t").is_some()
    );
    // Final review, spec §5.1: the title is the full resolved timestamp
    // (`ScopeBarModel::as_of_full`), not the elided `"AS OF … · Return to
    // live in the palette"` segment text — the width comparison lives on
    // the scope-bar badge's own test below, since the segment's OWN text
    // is longer than the bare timestamp and so is not the shorter side
    // here.
    assert!(vcx.debug_bounds("tip-status-as-of-title").is_some());
}

/// Task 3 (tooltips): hovering the AS OF badge names its full text
/// (`ScopeBarModel::as_of_badge`) and `frame::as_of`'s chord — the
/// selector that reopens the very dialog that set it. Unlike the sibling
/// test above, this one never opens the dialog at all — it sets the
/// frame's as-of directly — so it is untouched by Part 3's rewrite.
#[gpui::test]
fn hovering_the_as_of_badge_names_the_selector_chord(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    // Pinned to today-on-the-clock NOON, not `now − 1h`: in the hour
    // after the clock's own midnight the latter falls on yesterday, the
    // badge stops eliding to `HH:MM`, and the width assertion below
    // fails by construction.
    let clock = shell.read_with(&vcx, |s, cx| s.clock(cx));
    let at = clock
        .resolve_local(
            clock.today(chrono::Utc::now()),
            chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        )
        .expect("noon exists in every zone");
    frame.update(&mut vcx, |f, cx| {
        if f.set_as_of(AsOf::At(at)) {
            cx.notify();
        }
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.run_until_parked();

    let badge = vcx.debug_bounds("scope-asof").expect("badge painted");
    vcx.simulate_mouse_move(
        badge.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-scope-asof").is_some());
    assert!(
        vcx.debug_bounds("tip-scope-asof-chord-mod+t").is_some()
            || vcx.debug_bounds("tip-scope-asof-chord-alt+t").is_some(),
        "the tooltip must name frame::as_of's chord"
    );
    // Final review, spec §5.1: the title is the FULL resolved timestamp
    // (`ScopeBarModel::as_of_full`), not the elided badge text — wider,
    // since it always carries the date and seconds the badge itself
    // elides away.
    let title = vcx
        .debug_bounds("tip-scope-asof-title")
        .expect("tooltip title painted");
    assert!(
        title.size.width > badge.size.width,
        "the full timestamp is wider than the elided badge"
    );
}

fn open_as_of(cx: &mut gpui::TestAppContext) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let newest = chrono::Utc::now();
    frame.update(&mut vcx, |f, _| {
        f.note_published(Publish {
            dataset: "risk".into(),
            batch: "EOD".into(),
            books: 12,
            at: newest,
        });
    });
    vcx.simulate_keystrokes("alt-t");
    (shell, vcx)
}

fn state_of(
    shell: &Entity<ShellView>,
    vcx: &gpui::VisualTestContext,
) -> crate::shell::asof_rows::AsOfState {
    shell.read_with(vcx, |s, _| s.as_of_dialog.clone().expect("dialog open"))
}

#[gpui::test]
fn typing_eod_and_enter_commits_eod_t_minus_one(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_input("eod");
    let s = state_of(&shell, &vcx);
    assert_eq!(s.painted()[0].label, "EOD T-1");
    let expected = match s.painted()[0].row {
        crate::shell::asof_rows::Row::Preset(_) => s.clone().commit().unwrap(),
        _ => panic!(),
    };
    vcx.simulate_keystrokes("enter");
    let crate::shell::asof_rows::Commit::At(t) = expected else {
        panic!()
    };
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(t));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-stripe").is_some());
}

#[gpui::test]
fn the_list_takes_nav_keys_and_a_digit_jumps_on_an_empty_field(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("ctrl-n");
    assert_eq!(state_of(&shell, &vcx).highlighted(), 1, "ctrl+n is down");
    vcx.simulate_keystrokes("up");
    assert_eq!(state_of(&shell, &vcx).highlighted(), 0);
    vcx.simulate_keystrokes("2");
    let s = shell.read_with(&vcx, |s, _| s.as_of_dialog.is_none());
    assert!(s, "a digit on an empty field committed and closed");
    assert!(matches!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::At(_)
    ));
}

#[gpui::test]
fn a_digit_after_typing_is_a_filter_character(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_input("t-");
    vcx.simulate_keystrokes("1");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()));
    assert_eq!(state_of(&shell, &vcx).query(), "t-1");
    assert_eq!(state_of(&shell, &vcx).painted()[0].label, "EOD T-1");
}

#[gpui::test]
fn tab_opens_the_custom_field_up_steps_the_day_and_enter_commits(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("tab");
    let s = state_of(&shell, &vcx);
    let field = s.field().expect("tab opened the field");
    let before = field.value();
    assert_eq!(field.segment(), geode_widgets::datefield::Segment::Day);
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("as-of-custom-seg-2").is_some(),
        "the day segment is painted"
    );
    assert!(
        vcx.debug_bounds("as-of-custom-seg-suffix").is_some(),
        "the zone suffix is painted"
    );
    vcx.simulate_keystrokes("up");
    let after = state_of(&shell, &vcx).field().unwrap().value();
    assert_eq!(after, before + chrono::Duration::days(1));
    vcx.simulate_keystrokes("enter");
    let clock = shell.read_with(&vcx, |s, cx| s.clock(cx));
    let expected = clock.resolve_local(after.date(), after.time()).unwrap();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::At(expected)
    );
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn escape_closes_the_field_first_and_the_dialog_second(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_keystrokes("tab");
    assert!(state_of(&shell, &vcx).field().is_some());
    vcx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&vcx, |s, _| s.modal.is_some()),
        "first escape: field closed, dialog up"
    );
    assert!(state_of(&shell, &vcx).field().is_none());
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn a_chord_inside_the_open_field_still_reaches_the_shell(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_keystrokes("tab");
    // `ctrl+n` is a nav chord, not the field's: the field stays open and
    // the highlight does not move off Custom (nav is refused while the
    // field is open — see handle_key), which is the observable "not typed
    // into the field" outcome.
    let before = state_of(&shell, &vcx).field().unwrap().clone();
    vcx.simulate_keystrokes("ctrl-n");
    assert_eq!(state_of(&shell, &vcx).field().unwrap(), &before);
}

#[gpui::test]
fn a_row_click_commits_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.run_until_parked();
    let row = vcx.debug_bounds("as-of-row-1").expect("second row painted");
    vcx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(matches!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::At(_)
    ));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn while_pinned_current_and_live_lead_and_live_returns_to_live(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("enter"); // EOD T-1
    assert!(matches!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::At(_)
    ));
    vcx.simulate_keystrokes("alt-t");
    let s = state_of(&shell, &vcx);
    assert_eq!(s.painted()[0].label, "current");
    assert_eq!(s.painted()[1].label, "live");
    vcx.simulate_keystrokes("down enter");
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::Live);
}

#[gpui::test]
fn the_footer_swaps_to_the_fields_keys_while_it_is_open(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-hint-tab").is_some());
    assert!(vcx.debug_bounds("as-of-hint-step").is_none());
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-hint-step").is_some());
    assert!(vcx.debug_bounds("as-of-hint-tab").is_none());
    let _ = shell;
}

/// Toolbar restyle (2026-09-19, option A): the AS OF chip leads the bar
/// as its own segment — the warning tint is on the chip alone, not
/// across the whole readout — with a hairline after it, and clicking it
/// opens the as-of selector, the mouse form of `frame::as_of`.
#[gpui::test]
fn the_as_of_chip_leads_the_bar_and_opens_the_selector(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let clock = shell.read_with(&vcx, |s, cx| s.clock(cx));
    let at = clock
        .resolve_local(
            clock.today(chrono::Utc::now()),
            chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        )
        .expect("noon exists in every zone");
    frame.update(&mut vcx, |f, cx| {
        if f.set_as_of(AsOf::At(at)) {
            cx.notify();
        }
    });
    vcx.run_until_parked();

    let badge = vcx.debug_bounds("scope-asof").expect("badge painted");
    let divider = vcx
        .debug_bounds("scope-divider-asof")
        .expect("the as-of segment's divider is painted");
    let readout = vcx.debug_bounds("scope-grouping").expect("readout painted");
    assert!(
        badge.right() <= divider.left() && divider.right() <= readout.left(),
        "AS OF {badge:?} | divider {divider:?} | readout {readout:?}"
    );

    vcx.simulate_click(badge.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.as_of_dialog.is_some()),
        "the chip opens the as-of selector"
    );
    // A mouse-opened dialog's test types after the click (CLAUDE.md's
    // `open_shell_dialog` gotcha): the `+` chip's dialog once opened
    // deaf while its test asserted only `Some`.
    vcx.simulate_input("eod");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .as_of_dialog
            .as_ref()
            .unwrap()
            .query()
            .to_string()),
        "eod"
    );
}
