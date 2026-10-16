//! The request lifecycle: the tenor listing and the board at open, tags,
//! failures, refusals, membership changes and the refresh timer. Every
//! test drives the shell's doors.

use std::sync::Arc;
use std::time::Duration;

use geode_core::volboard::{Lookback, Measure};
use geode_core::watchlist::state::Status;
use geode_shell::watchlist::WatchlistGlobal;

use super::harness::*;
use crate::content::{MIN_REFRESH, VolwatchSettings};
use crate::core::views::SPARK_30D;
use crate::tile::{MenuKind, MenuState};

#[gpui::test]
fn opening_on_a_list_lists_the_tenors_and_asks_one_board(cx: &mut gpui::TestAppContext) {
    let (h, _vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let reqs = h.requests();
    let keys = Harness::keys_asks(&reqs);
    assert_eq!(
        (
            keys[0].dataset.as_str(),
            keys[0].column.as_str(),
            keys[0].key
        ),
        ("vol_surface", "tenor", KEY)
    );
    let boards = Harness::boards(&reqs);
    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].members, ["DAX", "SX5E", "NEW"]);
    assert_eq!(boards[0].lookbacks, [Lookback::D1]);
    assert_eq!(boards[0].window, None);
    assert_eq!(boards[0].sparklines, [(Measure::atfm("3M"), SPARK_30D)]);
    assert_eq!(boards[0].key, KEY);
}

#[gpui::test]
fn a_new_tile_with_no_list_opens_the_switcher_and_asks_nothing(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_with(cx, one_list(), None, VolwatchSettings::default());
    assert_eq!(h.title(&vcx), "Vol · New…");
    assert!(h.tile.read_with(&vcx, |t, _| matches!(
        t.menu,
        Some(MenuState {
            kind: MenuKind::Switch,
            ..
        })
    )));
    assert!(Harness::boards(&h.requests()).is_empty());
}

#[gpui::test]
fn a_board_lands_rows_in_member_order_with_the_missing_at_the_foot(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"]);
    assert!(h.tile.read_with(&vcx, |t, _| !t.prepared.rows[2].present));
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.header.not_in_surface), 1);
}

#[gpui::test]
fn a_stale_tag_board_is_dropped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let first = Harness::boards(&h.requests())[0].tag;
    h.press(&mut vcx, "d"); // W1: a new ask, a new tag
    let second = Harness::boards(&h.requests())[0].tag;
    assert!(second > first);
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    h.answer_board(&mut vcx, first, board_for(&["DAX"], &asked));
    assert!(
        h.tile.read_with(&vcx, |t, _| t.board.is_none()),
        "the first ask's board is stale"
    );
    h.answer_board(&mut vcx, second, board_for(&["DAX", "SX5E", "NEW"], &asked));
    assert!(h.tile.read_with(&vcx, |t, _| t.board.is_some()));
}

#[gpui::test]
fn a_surface_failure_keeps_the_last_board_and_stands_a_warning(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    h.press(&mut vcx, "shift-r");
    let tag = Harness::boards(&h.requests())[0].tag;
    h.fail_board(&mut vcx, tag, "connection refused");
    assert_eq!(
        h.shown(&vcx),
        ["DAX", "SX5E", "NEW"],
        "the last board stays"
    );
    assert_eq!(
        h.notices(&vcx),
        ["surface: connection refused — shift+r retries"]
    );
    assert!(h.tile.read_with(&vcx, |t, _| t.prepared.rows[0].stale));
    // The next good board clears the warning and the stale tone.
    h.press(&mut vcx, "shift-r");
    let tag = Harness::boards(&h.requests())[0].tag;
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    assert!(h.notices(&vcx).is_empty());
    assert!(h.tile.read_with(&vcx, |t, _| !t.prepared.rows[0].stale));
}

#[gpui::test]
fn members_changing_in_flight_reconcile_and_requery(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    vcx.update(|_, cx| cx.set_global(WatchlistGlobal(Arc::new(list_of(["SX5E", "UKX"])))));
    vcx.run_until_parked();
    let reqs = h.requests();
    let again = Harness::boards(&reqs);
    assert_eq!(
        again[0].members,
        ["SX5E", "UKX"],
        "a membership change asks again"
    );
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked)); // the old ask's board
    assert!(
        h.tile.read_with(&vcx, |t, _| t.board.is_none()),
        "superseded by the membership ask"
    );
    assert_eq!(
        h.shown(&vcx),
        ["SX5E", "UKX"],
        "blank rows for the new members meanwhile"
    );
}

#[gpui::test]
fn an_unchanged_snapshot_asks_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let _ = h.requests();
    vcx.update(|_, cx| cx.set_global(WatchlistGlobal(Arc::new(one_list()))));
    vcx.run_until_parked();
    assert!(Harness::boards(&h.requests()).is_empty());
}

#[gpui::test]
fn a_refused_submission_is_a_notice_not_a_retry(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let _ = h.requests();
    h.data.fill_for_tests(); // the queue refuses Busy
    h.press(&mut vcx, "shift-r");
    assert_eq!(
        h.notices(&vcx),
        ["the data service is busy — shift+r retries"]
    );
}

#[gpui::test]
fn the_refresh_timer_asks_again_at_its_interval_and_never_below_ten_seconds(
    cx: &mut gpui::TestAppContext,
) {
    let settings = VolwatchSettings {
        refresh: Some(Duration::from_secs(10)),
        ..Default::default()
    };
    let (h, mut vcx) = open_with(cx, one_list(), Some(restored("europe_risk")), settings);
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    h.answer_board(&mut vcx, tag, board_for(&["DAX"], &asked));
    cx.executor().advance_clock(Duration::from_secs(10));
    vcx.run_until_parked();
    assert_eq!(
        Harness::boards(&h.requests()).len(),
        1,
        "one refresh after the interval"
    );
    assert_eq!(
        VolwatchSettings::clamp_refresh(Some(Duration::from_secs(3))),
        Some(MIN_REFRESH)
    );
}

#[gpui::test]
fn a_hidden_tile_does_not_refresh(cx: &mut gpui::TestAppContext) {
    let settings = VolwatchSettings {
        refresh: Some(Duration::from_secs(10)),
        ..Default::default()
    };
    let (h, mut vcx) = open_with(cx, one_list(), Some(restored("europe_risk")), settings);
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    h.answer_board(&mut vcx, tag, board_for(&["DAX"], &asked));
    vcx.update(|_, cx| h.content.set_visible(false, cx));
    cx.executor().advance_clock(Duration::from_secs(30));
    vcx.run_until_parked();
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "the timer went with the tile"
    );
}

#[gpui::test]
fn the_tenor_listing_rebuilds_term_columns_and_asks_again_for_a_tenor_it_did_not_have(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let reqs = h.requests();
    let ktag = Harness::keys_asks(&reqs)[0].tag;
    h.press(&mut vcx, "4"); // Term over ALL_TENORS meanwhile
    let _ = h.requests();
    h.answer_keys(&mut vcx, ktag, Some(vec!["1M", "3M", "9M"]));
    let cols = h.tile.read_with(&vcx, |t, _| {
        t.columns.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
    });
    assert!(cols.contains(&"atfm-9M-level".into()));
    // ALL_TENORS asked atfm for eight tenors; the listing names 9M, which
    // that ask lacks, so one board is asked again.
    assert_eq!(Harness::boards(&h.requests()).len(), 1);
}

#[gpui::test]
fn a_tenor_listing_inside_the_last_ask_asks_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let reqs = h.requests();
    let ktag = Harness::keys_asks(&reqs)[0].tag;
    h.press(&mut vcx, "4");
    let _ = h.requests();
    h.answer_keys(&mut vcx, ktag, Some(vec!["1M", "3M", "1Y"]));
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "a narrower ask never requeries"
    );
}

#[gpui::test]
fn removing_the_shown_list_opens_the_switcher_and_drops_the_board(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    let mut other = list_of(["SPX"]);
    let spx = other.lists.remove("europe_risk").unwrap();
    other.lists.insert("us".into(), spx);
    vcx.update(|_, cx| cx.set_global(WatchlistGlobal(Arc::new(other))));
    vcx.run_until_parked();
    assert!(h.tile.read_with(&vcx, |t, _| matches!(
        t.menu,
        Some(MenuState {
            kind: MenuKind::Switch,
            ..
        })
    )));
    assert!(Harness::boards(&h.requests()).is_empty());
    assert!(h.shown(&vcx).is_empty());
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.empty.as_ref().map(|s| s.to_string())),
        Some("europe_risk no longer exists — g w switches".into())
    );
    h.answer_board(&mut vcx, tag, board_for(&["DAX"], &asked));
    assert!(
        h.tile.read_with(&vcx, |t, _| t.board.is_none()),
        "an answer for the gone list"
    );
    // Enter on the switcher shows `us` and asks for it.
    h.press(&mut vcx, "enter");
    assert_eq!(h.title(&vcx), "Vol · us");
    assert_eq!(Harness::boards(&h.requests())[0].members, ["SPX"]);
}

#[gpui::test]
fn a_failed_resolution_keeps_the_members_and_warns(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    let _ = h.requests();
    let mut snap = one_list();
    snap.lists.get_mut("europe_risk").unwrap().status = Status::Failed("timeout".into());
    vcx.update(|_, cx| cx.set_global(WatchlistGlobal(Arc::new(snap))));
    vcx.run_until_parked();
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"]);
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "the same members ask nothing"
    );
    assert_eq!(h.notices(&vcx), ["europe_risk not resolved: timeout"]);
}

#[gpui::test]
fn a_late_snapshot_asks_once_it_holds_the_restored_list(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        Default::default(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    assert!(
        h.requests().is_empty(),
        "nothing to ask before the lists arrive"
    );
    vcx.update(|_, cx| cx.set_global(WatchlistGlobal(Arc::new(one_list()))));
    vcx.run_until_parked();
    let reqs = h.requests();
    assert_eq!(Harness::keys_asks(&reqs).len(), 1);
    assert_eq!(Harness::boards(&reqs)[0].members, ["DAX", "SX5E", "NEW"]);
    assert!(
        h.tile.read_with(&vcx, |t, _| t.menu.is_none()),
        "a shown list opens no switcher"
    );
}

#[gpui::test]
fn the_title_names_the_list_before_its_members_arrive_and_after_a_switch(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_with(
        cx,
        Default::default(),
        Some(restored("europe_risk")),
        VolwatchSettings::default(),
    );
    assert_eq!(
        h.title(&vcx),
        "Vol · europe_risk",
        "a restore names its list before the snapshot"
    );
    let mut snap = one_list();
    let mut empty = snap.lists["europe_risk"].clone();
    empty.members.clear();
    snap.lists.insert("bare".into(), empty);
    vcx.update(|_, cx| cx.set_global(WatchlistGlobal(Arc::new(snap))));
    vcx.run_until_parked();
    assert_eq!(h.title(&vcx), "Vol · europe_risk");
    // The switcher opens on the shown list; `bare` sorts above it and has
    // no live members.
    h.press(&mut vcx, "g w");
    h.press(&mut vcx, "k");
    h.press(&mut vcx, "enter");
    assert_eq!(h.title(&vcx), "Vol · bare");
    assert!(h.shown(&vcx).is_empty());
}

#[gpui::test]
fn a_refused_timer_refresh_asks_again_at_the_next_interval(cx: &mut gpui::TestAppContext) {
    let every = Duration::from_secs(10);
    let settings = VolwatchSettings {
        refresh: Some(every),
        ..Default::default()
    };
    let (h, mut vcx) = open_with(cx, one_list(), Some(restored("europe_risk")), settings);
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    h.answer_board(&mut vcx, tag, board_for(&["DAX"], &asked));
    h.data.fill_for_tests();
    cx.executor().advance_clock(every);
    vcx.run_until_parked();
    assert_eq!(
        h.notices(&vcx),
        ["the data service is busy — shift+r retries"]
    );
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "the refused ask never reached the queue"
    );
    cx.executor().advance_clock(every);
    vcx.run_until_parked();
    assert_eq!(
        Harness::boards(&h.requests()).len(),
        1,
        "the timer kept running"
    );
}

#[gpui::test]
fn a_refused_shift_r_does_not_arm_a_retry(cx: &mut gpui::TestAppContext) {
    let every = Duration::from_secs(10);
    let settings = VolwatchSettings {
        refresh: Some(every),
        ..Default::default()
    };
    let (h, mut vcx) = open_with(cx, one_list(), Some(restored("europe_risk")), settings);
    let _ = h.requests();
    // No board landed, so no refresh is armed past the one at show; let
    // that one find the ask in flight and lapse.
    cx.executor().advance_clock(every);
    vcx.run_until_parked();
    h.data.fill_for_tests();
    h.press(&mut vcx, "shift-r");
    let _ = h.requests();
    cx.executor().advance_clock(every * 3);
    vcx.run_until_parked();
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "a refused key only says so"
    );
}
