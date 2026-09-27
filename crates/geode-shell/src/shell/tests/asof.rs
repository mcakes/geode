//! The as-of selector and historical indicators through real key dispatch:
//! `frame::as_of` (`mod+t`), ranked presets, the Custom field, and
//! `frame::live`/`frame::as_of_undo`. The window warning stripe and status segment must
//! appear exactly while the frame is historical.
//!
//! `test_services()` uses `default_mod()` (Alt), so these tests dispatch `mod+t` as
//! literal `alt-t`.

use super::*;
use crate::frame::Publish;
use geode_core::query::AsOf;

/// Hovering the historical status segment shows `frame::as_of`'s chord. Enter on the
/// selector's empty query commits its highlighted business-day preset, giving this test
/// a historical frame whose segment can be hovered.
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
    // The tooltip uses the full resolved timestamp (`ScopeBarModel::as_of_full`). The
    // badge test below compares widths; this status segment's own text can be longer
    // than the timestamp.
    assert!(vcx.debug_bounds("tip-status-as-of-title").is_some());
}

/// Hovering the AS OF badge shows the full timestamp and the chord for reopening the
/// selector. This fixture sets the frame's as-of directly.
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
    // The full timestamp includes the date and seconds elided from the badge, so its
    // tooltip title is wider.
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
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("as-of-stripe").is_none(),
        "the frame is still live before committing — the stripe must not paint"
    );
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
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
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
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
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
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

#[gpui::test]
fn escape_closes_the_field_first_and_the_dialog_second(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_keystrokes("tab");
    assert!(state_of(&shell, &vcx).field().is_some());
    vcx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&vcx, |s, _| s.modal_open()),
        "first escape: field closed, dialog up"
    );
    assert!(state_of(&shell, &vcx).field().is_none());
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

#[gpui::test]
fn a_chord_inside_the_open_field_is_not_the_fields(cx: &mut gpui::TestAppContext) {
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
    // Assert the clicked row's exact instant: merely reaching `At(_)` would also pass
    // if the initially highlighted row were committed.
    let mut clicked = state_of(&shell, &vcx);
    assert!(clicked.set_highlighted(1), "row 1 must exist to click it");
    let expected = clicked.commit().unwrap();
    let row = vcx.debug_bounds("as-of-row-1").expect("second row painted");
    vcx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    let crate::shell::asof_rows::Commit::At(t) = expected else {
        panic!("row 1 expected to be an At commit")
    };
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(t));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
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

/// Tab clears the query in both `AsOfState` and the shared `Input`. Subsequent
/// navigation must traverse the unfiltered list, and Enter must commit its highlighted
/// row without reapplying stale input text.
#[gpui::test]
fn tab_then_escape_then_down_then_enter_commits_the_highlighted_row(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_input("eod");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "",
        "tab clears the model's query and the shared field must follow it"
    );
    vcx.simulate_keystrokes("escape");
    vcx.simulate_keystrokes("down");
    let s = state_of(&shell, &vcx);
    let highlighted = s.highlighted();
    let expected = s.clone().commit().unwrap();
    vcx.simulate_keystrokes("enter");
    let crate::shell::asof_rows::Commit::At(t) = expected else {
        panic!("row {highlighted} expected to be an At commit")
    };
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(t));
}

/// An unrecognized bare key while Custom is open must be claimed without reaching the
/// shared input. `route` also returns `None` for chords, so the handler must
/// distinguish chords that can fall through from bare keys that would silently refilter
/// the list.
#[gpui::test]
fn a_bare_key_the_field_does_not_own_is_swallowed_while_it_is_open(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_keystrokes("tab");
    vcx.simulate_keystrokes("x");
    vcx.run_until_parked();
    let s = state_of(&shell, &vcx);
    assert_eq!(
        s.query(),
        "",
        "the key must not have reached the shared field"
    );
    assert!(s.field().is_some(), "the field stays open");
    assert!(
        vcx.debug_bounds("as-of-custom-seg-2").is_some(),
        "the Custom row's field is still painted, not hidden by a re-filter"
    );
}

/// Navigation beyond the visible rows must scroll the highlight into view. Assert
/// `ScrollHandle::offset()`: row intersection with an unbounded parent would pass even
/// without a height cap or tracked scrolling, while a changed offset requires both.
#[gpui::test]
fn nav_past_the_visible_rows_scrolls_the_highlight_into_view(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let newest = chrono::Utc::now();
    frame.update(&mut vcx, |f, _| {
        for i in 0..20 {
            f.note_published(Publish {
                dataset: "risk".into(),
                batch: "EOD".into(),
                books: 1,
                at: newest - chrono::Duration::seconds(i as i64),
            });
        }
    });
    vcx.simulate_keystrokes("alt-t");
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let before = shell.read_with(&vcx, |s, _| s.as_of_scroll.offset());

    // 15 `ctrl-d` (+5 each, clamping at the end — vimnav's own contract
    // for a multi-step `Move`) lands well past any reasonable visible
    // window.
    vcx.simulate_keystrokes(&vec!["ctrl-d"; 15].join(" "));
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });

    let s = state_of(&shell, &vcx);
    let last = s.painted().len() - 1;
    assert_eq!(s.highlighted(), last, "clamped at the bottom of the list");

    let after = shell.read_with(&vcx, |s, _| s.as_of_scroll.offset());
    assert_ne!(
        after, before,
        "the list's own scroll offset must have moved to follow the \
         highlight past the visible window"
    );

    let last_selector: &'static str = Box::leak(format!("as-of-row-{last}").into_boxed_str());
    let list_bounds = vcx.debug_bounds("as-of-rows").expect("list painted");
    let row_bounds = vcx
        .debug_bounds(last_selector)
        .expect("the last row should still be part of the layout tree (no virtualization)");
    assert!(
        list_bounds.intersects(&row_bounds),
        "row {last} {row_bounds:?} should be scrolled into the visible list \
         viewport {list_bounds:?}, not left below it with only its index \
         having changed"
    );

    // The scroll handle survives closing the dialog; reopening must reset its offset to
    // the top.
    vcx.simulate_keystrokes("escape");
    vcx.simulate_keystrokes("alt-t");
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let reopened = shell.read_with(&vcx, |s, _| s.as_of_scroll.offset());
    assert_eq!(
        reopened,
        gpui::Point::default(),
        "reopening the dialog must reset the scroll to the top, not keep \
         the previous session's offset"
    );

    // Filtering reranks the rows and resets the highlight to zero. Scrolling must
    // follow that reset.
    vcx.simulate_keystrokes(&vec!["ctrl-d"; 15].join(" "));
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let scrolled_again = shell.read_with(&vcx, |s, _| s.as_of_scroll.offset());
    assert_ne!(
        scrolled_again,
        gpui::Point::default(),
        "sanity: scrolled down again before typing"
    );

    vcx.simulate_input("eod");
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let after_filter = shell.read_with(&vcx, |s, _| s.as_of_scroll.offset());
    // Not necessarily exactly `(0, 0)`: `scroll_to_item`'s default
    // strategy brings the target minimally into view rather than
    // top-aligning it, and the re-ranked highlight (painted row 0) sits
    // just after its section's own eyebrow — but it must have moved
    // substantially back up from where 15 `ctrl-d`s left it, and row 0
    // must now actually be visible.
    assert!(
        f32::from(after_filter.y.abs()) < f32::from(scrolled_again.y.abs()),
        "typing a filter after scrolling down must scroll back up toward \
         the re-ranked highlight, not stay near the bottom: before \
         {scrolled_again:?}, after {after_filter:?}"
    );
    let list_bounds_now = vcx.debug_bounds("as-of-rows").expect("list painted");
    let first_row_bounds = vcx
        .debug_bounds("as-of-row-0")
        .expect("the re-ranked first row is painted");
    assert!(
        list_bounds_now.intersects(&first_row_bounds),
        "the re-ranked highlight (row 0) must be scrolled into view: \
         row {first_row_bounds:?}, list {list_bounds_now:?}"
    );
}

/// A publish arriving while the dialog is open must appear in its rows without
/// reopening the dialog.
#[gpui::test]
fn a_publish_while_open_adds_a_new_row(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.run_until_parked();
    let before = state_of(&shell, &vcx).painted().len();

    frame.update(&mut vcx, |f, cx| {
        f.note_published(Publish {
            dataset: "greeks".into(),
            batch: "INTRADAY".into(),
            books: 4,
            at: chrono::Utc::now(),
        });
        cx.notify();
    });
    vcx.run_until_parked();

    let after = state_of(&shell, &vcx).painted().len();
    assert_eq!(
        after,
        before + 1,
        "the new publish must add exactly one row"
    );
    let new_row_selector: &'static str =
        Box::leak(format!("as-of-row-{}", after - 1).into_boxed_str());
    assert!(
        vcx.debug_bounds(new_row_selector).is_some(),
        "the new row must actually paint"
    );
}

/// Refreshing preserves the highlighted publish's identity. Newer publishes can move it
/// to a different index, so the viewport must follow the restored highlight.
#[gpui::test]
fn a_publish_below_the_fold_scrolls_the_highlight_into_view(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let newest = chrono::Utc::now();
    // 20 publishes, oldest last — well under the 32-deep cap
    // (`recent_publishes_keep_the_last_thirty_two_newest_first`), so
    // none of them are evicted by the 10 fresher ones landed below.
    frame.update(&mut vcx, |f, _| {
        for i in 0..20 {
            f.note_published(Publish {
                dataset: "risk".into(),
                batch: "EOD".into(),
                books: 1,
                at: newest - chrono::Duration::seconds(i as i64),
            });
        }
    });
    vcx.simulate_keystrokes("alt-t");
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    // Scroll all the way down, the same 15×ctrl-d clamp the sibling
    // scroll-follow test uses, landing the highlight on one of the
    // OLDEST publishes at the tail of the list.
    vcx.simulate_keystrokes(&vec!["ctrl-d"; 15].join(" "));
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let before_offset = shell.read_with(&vcx, |s, _| s.as_of_scroll.offset());
    let before_state = state_of(&shell, &vcx);
    let highlighted_label = before_state.painted()[before_state.highlighted()]
        .label
        .clone();

    // 10 MORE, NEWER publishes land while the dialog is open — ranking
    // above every one of the first 20, so the previously highlighted
    // row's identity survives `refresh` but its painted INDEX shifts
    // ten rows further from the top without moving relative to the
    // bottom of the (now longer) list.
    frame.update(&mut vcx, |f, cx| {
        for i in 0..10 {
            f.note_published(Publish {
                dataset: "risk".into(),
                batch: "EOD".into(),
                books: 1,
                at: newest + chrono::Duration::seconds(i as i64 + 1),
            });
        }
        cx.notify();
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    vcx.run_until_parked();

    let after_state = state_of(&shell, &vcx);
    assert_eq!(
        after_state.painted()[after_state.highlighted()].label,
        highlighted_label,
        "sanity: refresh's identity restore kept the same row highlighted"
    );
    let after_offset = shell.read_with(&vcx, |s, _| s.as_of_scroll.offset());
    assert_ne!(
        after_offset, before_offset,
        "the highlighted row's position shifted well below the fold — \
         the scroll must follow it there, not stay where it was \
         (before {before_offset:?}, after {after_offset:?})"
    );
}

/// Exercise the palette-only `frame::live` and `frame::as_of_undo` actions through real
/// key dispatch, independently of the selector's preset and Custom field paths.
#[gpui::test]
fn frame_live_and_as_of_undo_still_dispatch(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("enter"); // EOD T-1
    let pinned = frame.read_with(&vcx, |f, _| f.as_of().clone());
    assert!(matches!(pinned, AsOf::At(_)));

    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("frame::live".to_string()), None, window, cx);
        });
    });
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::Live);

    vcx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("frame::as_of_undo".to_string()), None, window, cx);
        });
    });
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), pinned);
}

/// Clicking a Custom-field segment selects it, preserves input focus, and allows
/// subsequent typing. This establishes the visible contract but does not isolate
/// `sync_dialog_text`: the modal panel also stops mouse-down propagation and already
/// prevents focus theft.
#[gpui::test]
fn clicking_a_segment_selects_it_and_the_field_still_hears_the_keyboard(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    let before = state_of(&shell, &vcx).field().unwrap().value();

    let seg = vcx
        .debug_bounds("as-of-custom-seg-1")
        .expect("the month segment is painted");
    vcx.simulate_mouse_down(
        seg.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
    assert_eq!(
        state_of(&shell, &vcx).field().unwrap().segment(),
        geode_widgets::datefield::Segment::Month,
        "the click selected the month segment"
    );

    vcx.simulate_keystrokes("up");
    let after = state_of(&shell, &vcx).field().unwrap().value();
    assert_ne!(
        after, before,
        "the keyboard must still reach the field after the click stepped \
         the month, not have gone deaf behind a stolen focus"
    );

    let input_focus = shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).focus_handle(cx));
    assert!(
        vcx.update(|window, _cx| input_focus.is_focused(window)),
        "the shared Input must still hold window focus after the click"
    );
}

/// Clicking the Custom row while its field is already open preserves focus and typing.
/// As in the segment-click test, modal mouse-down handling also protects focus, so this
/// test cannot establish whether a redundant sync call ran.
#[gpui::test]
fn clicking_the_custom_rows_body_while_open_keeps_the_field_focused(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    let custom_row = state_of(&shell, &vcx)
        .painted()
        .iter()
        .position(|p| matches!(p.row, crate::shell::asof_rows::Row::Custom))
        .expect("the custom row is painted");
    let row_selector: &'static str = Box::leak(format!("as-of-row-{custom_row}").into_boxed_str());
    let row = vcx
        .debug_bounds(row_selector)
        .expect("the custom row is painted");
    // Land the click on the row's body, not a segment — the far right,
    // past where the segments themselves paint.
    let body = gpui::Point::new(row.right() - gpui::px(4.), row.center().y);
    vcx.simulate_mouse_down(body, gpui::MouseButton::Left, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(
        state_of(&shell, &vcx).field().is_some(),
        "the field stays open"
    );

    let input_focus = shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).focus_handle(cx));
    assert!(
        vcx.update(|window, _cx| input_focus.is_focused(window)),
        "a body click on an already-open Custom row must resync focus too"
    );
}

/// The AS OF chip forms a separate leading segment with its own warning tint and
/// trailing separator. Clicking it opens the selector, matching `frame::as_of`.
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
