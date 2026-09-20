//! The as-of selector and the historical indicator (Phase 4a §3.6,
//! §3.11; rewritten as-of dialog spec 2026-09-20 §5): the real
//! key-dispatch pipeline through `frame::as_of` (`mod+t`), the ranked
//! row list and the Custom field, `frame::live`/`frame::as_of_undo` —
//! plus the window-wide warning stripe and the status-bar segment that
//! must paint if and only if the frame is historical (spec §4.5:
//! nothing on screen may look live when it is not).
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
    // The CLICKED row's own instant, not just "some" `At(_)` — proves the
    // click committed the row it landed on, not merely the highlighted
    // one at open (minor, review round 2).
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

/// Review round 2, finding 1 (Critical): before the fix, `tab` cleared
/// `AsOfState::query` but left the shared `Input` showing the stale
/// typed text — nothing wrote it back, since `dialog::sync_dialog_text`
/// had no as-of arm. `escape` then closed the field without touching
/// the query either way, so `enter`'s own `set_query(&live)` re-fed the
/// STALE "eod" text from the field, re-filtering the list back down to
/// the EOD presets and committing "EOD T-1" regardless of where `down`
/// had actually moved the highlight. With the fix, `tab` reconciles the
/// field to empty immediately (the key-path seam every modal's handler
/// already runs through, `input.rs`), so `down` moves a genuinely
/// unfiltered list and `enter` commits whatever row is under it.
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

/// Review round 2, finding 2 (Important): `route` answers `None` both
/// for a chord AND for any other key it does not recognize, so a bare
/// unrecognized key (here, `x`) used to fall through exactly like a
/// chord does — reaching the shared, still-focused `Input` as typing,
/// re-filtering the list and hiding the Custom row out from under its
/// own open field. The fix checks the chord case first and swallows
/// every other unrouted key instead of leaving it unclaimed.
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

/// Review round 2, finding 3 (Important): keyboard navigation past the
/// visible window must scroll the highlight into view, not just move an
/// off-screen index. Checked directly on `ScrollHandle::offset()` — a
/// `list_bounds.intersects(&row_bounds)` check (the palette's own
/// scroll-follow proof, `palette.rs`'s `arrow_down_past_visible_rows_...`)
/// is trivially true here on an UNBOUNDED list (every child's bounds sit
/// inside its own unbounded parent's by construction, scrolled or not),
/// so it would pass even with no `max_h`/`track_scroll` at all — proven
/// by temporarily removing both during this fix's own RED pass, which
/// left that assertion green. The offset is the one signal that can
/// actually fail: it moves only if the list is both height-capped (so
/// there is a `max_offset` to move within) and tracked (so
/// `scroll_to_item` has a handle to act on).
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

    // Review round 2 re-review, finding 3a: `as_of_scroll` lives on
    // `ShellView` and keeps its offset across close/reopen — a fresh
    // open must reset it to the top, not silently keep whatever the
    // last session scrolled to.
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

    // Review round 2 re-review, finding 3b: typing a filter re-ranks and
    // resets the highlight to 0 (`AsOfState::rerank`) — the scroll must
    // follow it back to the top too, the same seam the sibling dialogs'
    // query-change arms already drive their own scroll handles from.
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

/// Review round 2, finding 5 (ruled in): a publish landing while the
/// dialog is open must show up in its row list — parity with the old
/// `cached_presets` (spec §5.1) — without a close/reopen.
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

/// Final whole-branch review, finding M-10: `refresh`'s identity restore
/// (review round 2 re-review, finding 3) can land the SAME highlighted
/// row at a very different painted index — new, newer publishes rank
/// ABOVE it — so the scroll must follow it there the same way the other
/// three seams that move the highlight already do (`input.rs`'s
/// query-change arm, `handle_key`'s `tab` and nav arms), or a trader
/// scrolled down to a publish loses sight of it the moment a fresher one
/// lands.
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

/// The dialog's own tests deleted `frame::live`/`frame::as_of_undo`'s one
/// window test along with the free-text grammar it used to type through
/// (Part 3's rewrite) — these two palette-only actions are otherwise
/// untouched by this dialog and still need a real key-dispatch proof.
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

/// Final whole-branch review, finding I-1: the Custom row's segment
/// click callback now calls `dialog::sync_dialog_text` after selecting
/// the segment, the same reconciliation seam every other `AsOfState`
/// mutation goes through. Proves the click selects the right segment,
/// the keyboard still reaches the field afterward, and the shared
/// `Input` still holds window focus. NOTE: this does not reproduce a
/// live regression — verified by deliberately reverting the fix and
/// re-running this test, which stayed green, because `dialog::
/// render_modal`'s panel already calls `cx.stop_propagation()` on every
/// mouse-down anywhere inside an already-open modal, which blocks gpui's
/// default track-focus grab (the "+" chip's mechanism, CLAUDE.md's
/// `open_shell_dialog` precedent) before it can ever reach the shell
/// root — that mechanism only bites the mouse-down that OPENS a dialog,
/// before a modal panel exists to intercept it. The fix is still correct
/// hygiene (every other mutation site in this file reconciles on its own
/// seam; a segment select had none), so it stays; this test locks in the
/// resulting behaviour rather than catching a regression that does not
/// reproduce here.
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

/// Final whole-branch review, finding I-1's second half: the
/// `sync_dialog_text` hoist out of the `field().is_none()` arm — a body
/// click on the Custom row while its field is ALREADY open resyncs too,
/// not just the click that opens it. Same note as the segment-click test
/// above: this does not reproduce a live regression in this codebase
/// (the modal panel's own `stop_propagation()` already prevents a click
/// anywhere inside it from stealing window focus, verified by reverting
/// the hoist and re-running this test, which stayed green) — kept for
/// the same "every mutation seam reconciles the same way" consistency,
/// not because a bug reproduces without it.
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
