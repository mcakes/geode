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

/// Hovering the status bar's AS OF segment (Task 4, spec §5.1) names the
/// same `frame::as_of` chord the toolbar's own AS OF badge does — the
/// keyboard twin to "click it to reopen the as-of selector".
#[gpui::test]
fn hovering_the_status_as_of_segment_names_the_selector_chord(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let _frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("alt-t");
    vcx.simulate_input("14:05");
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

/// Opens the shell, publishes `n` generations (oldest first, so
/// `note_published`'s newest-first `VecDeque` ends up with the newest at
/// preset index 0 — the same ordering `selecting_a_preset_sets_that_
/// instant_and_live_then_undo_returns` above relies on) and opens the
/// as-of dialog (`alt-t`), leaving the field empty so the dialog is
/// browsing presets.
fn open_as_of_with_presets(
    cx: &mut gpui::TestAppContext,
    n: usize,
) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let newest = chrono::Utc::now();
    frame.update(&mut vcx, |f, _| {
        for i in (0..n).rev() {
            f.note_published(Publish {
                dataset: "risk".into(),
                batch: "EOD".into(),
                books: 1,
                at: newest - chrono::Duration::seconds(30 * i as i64),
            });
        }
    });
    vcx.simulate_keystrokes("alt-t");
    (shell, vcx)
}

/// Spec §20.5: every list takes the whole nav set through one rule.
/// `ctrl+n`/`ctrl+p` used to be dead here and `up` at row 0 wrapped;
/// now `ctrl+n` moves, `up` at 0 still wraps (a bare ±1), and `ctrl+u`
/// at 0 clamps.
#[gpui::test]
fn the_as_of_list_takes_ctrl_n_and_clamps_a_page_step(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = open_as_of_with_presets(cx, 3); // ≥3 presets
    let selected = |cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |s, _| s.as_of_dialog.as_ref().unwrap().selected)
    };
    cx.simulate_keystrokes("ctrl-n");
    assert_eq!(selected(&cx), 1, "ctrl+n is down");
    cx.simulate_keystrokes("ctrl-p");
    assert_eq!(selected(&cx), 0);
    cx.simulate_keystrokes("up");
    assert_eq!(selected(&cx), 2, "a bare step wraps");
    cx.simulate_keystrokes("ctrl-u");
    assert_eq!(selected(&cx), 0, "a page step clamps");
}

/// Task 3 (tooltips): hovering the AS OF badge names its full text
/// (`ScopeBarModel::as_of_badge`) and `frame::as_of`'s chord — the
/// selector that reopens the very dialog that set it.
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

/// Task 3: the calendar pane paints beside the presets while the field
/// does not read `live`, and hides once it does.
#[gpui::test]
fn the_as_of_dialog_paints_a_calendar_that_live_hides(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let _shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    assert!(
        vcx.debug_bounds("as-of-calendar").is_some(),
        "calendar pane painted on open"
    );
    vcx.simulate_input("live");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("as-of-calendar").is_none(),
        "hidden under live"
    );
    vcx.simulate_keystrokes("backspace backspace backspace backspace");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("as-of-calendar").is_some(),
        "back once live is gone"
    );
}

/// Task 3: a day click (`CalendarState::activate_date`, the same call
/// the component's own day cell makes) writes the field's date part
/// through `compose_with_date`, keeping a typed time, and never steals
/// focus from the field — `enter` still commits afterward.
#[gpui::test]
fn clicking_a_day_writes_the_date_and_keeps_a_typed_time(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
    // A "click" is the same call the calendar's own day cell makes.
    let calendar = shell.read_with(&vcx, |s, _| s.as_of_calendar().clone());
    calendar.update(&mut vcx, |c, cx| {
        c.activate_date(day, cx);
    });
    vcx.run_until_parked();
    let text = shell.read_with(&vcx, |s, cx| s.dialog_input().read(cx).value().to_string());
    assert_eq!(text, "2026-09-08", "a blank field becomes the bare date");
    assert!(
        vcx.debug_bounds("as-of-resolved").is_some(),
        "the preview shows the end of that day"
    );
    // Now type a time, click another day: the time survives. `ctrl-a
    // backspace` does not clear the field here — the dialog reclaims
    // `ctrl+a` — so clear it the way `a_bad_time_...`'s sibling tests do:
    // one `backspace` per character of "2026-09-08" (10).
    vcx.simulate_keystrokes(&"backspace ".repeat(10));
    vcx.simulate_input("14:05");
    let other = chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
    calendar.update(&mut vcx, |c, cx| {
        c.activate_date(other, cx);
    });
    vcx.run_until_parked();
    let text = shell.read_with(&vcx, |s, cx| s.dialog_input().read(cx).value().to_string());
    assert_eq!(text, "2026-09-09 14:05");
    // The field still has the keyboard: enter commits.
    vcx.simulate_keystrokes("enter");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(matches!(
        frame.read_with(&vcx, |f, _| f.as_of().clone()),
        AsOf::At(_)
    ));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

/// Task 3: typing a date mirrors onto the calendar's own selection.
#[gpui::test]
fn typing_a_date_moves_the_calendars_selection(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    vcx.simulate_input("2026-09-08 14:05");
    vcx.run_until_parked();
    let calendar = shell.read_with(&vcx, |s, _| s.as_of_calendar().clone());
    let selected = calendar.read_with(&vcx, |c, _| c.date().start());
    assert_eq!(selected, chrono::NaiveDate::from_ymd_opt(2026, 9, 8));
}

/// Final review, finding 1: the calendar's chrome (‹/›, the month/year
/// toggles, the pane's own padding — anything that is not a day cell)
/// must not steal focus from the field on mouse-down. `asof_view::build`
/// guards the `as-of-calendar` wrapper with `capture_any_mouse_down` +
/// `window.prevent_default()` (gpui focuses a `track_focus`ed element's
/// handle on bubble-phase mouse-down unless the capture phase called
/// `prevent_default` first — the pending-click recorder ignores
/// `default_prevented`, so the click itself still reaches the header
/// button underneath). A click near the pane's top edge lands in the
/// header row (‹ › and the month/year toggles), never a day cell.
#[gpui::test]
fn clicking_the_calendars_chrome_leaves_the_field_focused(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("as-of-calendar")
        .expect("calendar pane painted");
    let header_point = gpui::point(
        bounds.origin.x + bounds.size.width / 2.0,
        bounds.origin.y + gpui::px(8.0),
    );
    vcx.simulate_mouse_down(header_point, MouseButton::Left, gpui::Modifiers::none());
    vcx.simulate_mouse_up(header_point, MouseButton::Left, gpui::Modifiers::none());
    vcx.run_until_parked();

    let focused = vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.read(cx).focus_handle(cx).is_focused(window)
    });
    assert!(
        focused,
        "the field must still hold focus after a click on the calendar's chrome"
    );
}

/// Display check 2026-09-19: with presets present, a click on the calendar
/// DISMISSED the dialog. `build_presets` painted the list at the dialog's
/// full `WIDTH` inside its `flex_1` slot, so every preset row ran on
/// under the calendar; gpui hit-tests a plain div behind another, so the
/// day click also fired the covered row's `on_mouse_down` → `commit_at`
/// → `close_modal` (and the selected row's highlight showed through).
/// The list must stay in its own column and the pane must occlude.
#[gpui::test]
fn a_calendar_click_over_the_preset_list_neither_commits_nor_closes(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of_with_presets(cx, 20);
    vcx.run_until_parked();
    let list = vcx.debug_bounds("as-of-presets").expect("presets painted");
    let pane = vcx
        .debug_bounds("as-of-calendar")
        .expect("calendar painted");
    assert!(
        list.right() <= pane.left(),
        "the preset list ({:?}) must not run under the calendar ({:?})",
        list,
        pane
    );
    // A click in the middle of the pane — the day grid, where the rows
    // used to be hit-tested through it.
    let centre = pane.center();
    vcx.simulate_mouse_down(centre, MouseButton::Left, gpui::Modifiers::none());
    vcx.simulate_mouse_up(centre, MouseButton::Left, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.modal.is_some()),
        "the dialog must stay open after a calendar click"
    );
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(
        matches!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::Live),
        "no preset row may commit through the calendar"
    );
}

/// Final review, finding 3: `on_calendar_selected`'s own refocus, in
/// isolation from any backstop — the calendar's own focus handle is
/// given focus first (a click on the pane's DAY grid does briefly
/// carry keyboard focus at the pinned gpui rev; `chrome`'s guard above
/// is what stops it for the header, but a day click still resolves
/// through this function's own hand-back), then a day is activated the
/// same way the component's own day cell does, and the field must have
/// focus again.
#[gpui::test]
fn a_day_click_returns_focus_to_the_field(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    let calendar = shell.read_with(&vcx, |s, _| s.as_of_calendar().clone());

    vcx.update(|window, cx| {
        let handle = calendar.read(cx).focus_handle.clone();
        handle.focus(window, cx);
    });
    assert!(
        vcx.update(|window, cx| {
            let handle = calendar.read(cx).focus_handle.clone();
            handle.is_focused(window)
        }),
        "sanity: the calendar itself holds focus before the click"
    );

    let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
    calendar.update(&mut vcx, |c, cx| {
        c.activate_date(day, cx);
    });
    vcx.run_until_parked();

    let focused = vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.read(cx).focus_handle(cx).is_focused(window)
    });
    assert!(
        focused,
        "a day click must hand focus back to the field even when the \
         calendar itself held it going in"
    );
}

/// Final review, finding 4: an invalid INTERMEDIATE keystroke (a parse
/// failure mid-edit, `resolved: None`) must not snap the calendar back
/// to today — only a successful parse moves it. Typing a full date
/// moves the calendar there; one `backspace` breaks the parse
/// (`"2026-09-0"`, an `Err`) and the calendar must stay exactly where
/// it was.
#[gpui::test]
fn an_invalid_intermediate_keystroke_leaves_the_calendar_where_it_was(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    vcx.simulate_input("2026-10-03");
    vcx.run_until_parked();
    let calendar = shell.read_with(&vcx, |s, _| s.as_of_calendar().clone());
    let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
    assert_eq!(calendar.read_with(&vcx, |c, _| c.date().start()), Some(day));

    vcx.simulate_keystrokes("backspace");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.as_of_dialog.as_ref().unwrap().resolved),
        None,
        "sanity: '2026-10-0' does not parse"
    );
    assert_eq!(
        calendar.read_with(&vcx, |c, _| c.date().start()),
        Some(day),
        "a failed intermediate parse must not move the calendar off the day it showed"
    );
}

/// Slice-2 final review, finding 8 (built 2026-09-19): closing the dialog
/// from the month or year picker must not leave the next open showing
/// that picker — `open` puts the calendar back on the day grid.
#[gpui::test]
fn reopening_the_dialog_returns_the_calendar_to_the_day_grid(cx: &mut gpui::TestAppContext) {
    use gpui_base::CalendarView;
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("alt-t");
    let calendar = shell.read_with(&vcx, |s, _| s.as_of_calendar().clone());
    // What the header's month button does: switch to the month picker.
    calendar.update(&mut vcx, |c, _| c.set_view(CalendarView::Month));
    assert!(calendar.read_with(&vcx, |c, _| c.view().is_month()));
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()), "closed");
    vcx.simulate_keystrokes("alt-t");
    vcx.run_until_parked();
    assert!(
        calendar.read_with(&vcx, |c, _| c.view().is_day()),
        "a reopen must paint the day grid, not the picker it was closed from"
    );
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
    vcx.simulate_input("12");
    let typed = shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(
        typed, "12",
        "the selector's field takes the keys after the click"
    );
}
