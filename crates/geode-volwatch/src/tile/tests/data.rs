//! The request lifecycle: the tenor listing and the board at open, tags,
//! failures, refusals, membership changes and the refresh timer. Every
//! test drives the shell's doors.

use std::sync::Arc;
use std::time::Duration;

use geode_core::query::QueryOutcome;
use geode_core::snapshot::Snapshot;
use geode_core::volboard::{Lookback, Measure};
use geode_core::watchlist::state::Status;
use geode_shell::module::Delivery;
use geode_shell::watchlist::WatchlistGlobal;
use geode_widgets::cell::widths;
use std::time::Instant;

use super::harness::*;
use crate::content::{MIN_REFRESH, VolwatchSettings};
use crate::core::views::{ALL_TENORS, ColumnKind, SPARK_30D, WINDOW_1Y};
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

#[gpui::test]
fn enter_opens_the_band_on_the_cursor_row_grows_the_ask_and_keeps_the_cursor(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "j enter");
    assert!(h.band_open(&vcx));
    assert_eq!(h.cursor(&vcx), Some("SX5E".into()));
    let reqs = h.requests();
    let b = Harness::boards(&reqs);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].window, Some(WINDOW_1Y));
    assert_eq!(b[0].sparklines.len(), 3);
    assert!(b[0].measures.contains(&Measure::Point {
        tenor: "1M".into(),
        delta: 0.9
    }));
    h.press(&mut vcx, "enter");
    assert!(!h.band_open(&vcx));
    assert_eq!(h.cursor(&vcx), Some("SX5E".into()));
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "closing shrinks the ask without a requery"
    );
}

#[gpui::test]
fn escape_closes_the_band_before_a_selection_and_before_notices(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "enter v escape");
    assert!(!h.band_open(&vcx), "the band is innermost");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.grid.selecting()),
        "the selection survives"
    );
    h.press(&mut vcx, "escape");
    assert!(!h.tile.read_with(&vcx, |t, _| t.grid.selecting()));
}

#[gpui::test]
fn the_band_shows_the_fixed_delta_surface_three_sparks_and_the_context(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "j enter");
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    let tag = tag_of(&h, &vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    let band = h.tile.read_with(&vcx, |t, _| t.band_model.clone());
    assert_eq!(band.surface.len(), ALL_TENORS.len());
    assert_eq!(band.surface[0].1.len(), 7, "five deltas, skew, Δ");
    assert!(band.sparks.iter().all(Option::is_some));
    assert_eq!(band.context[0].0.as_ref(), "Varswap 3M");
    assert!(band.context.last().unwrap().1.starts_with("surface "));
}

#[gpui::test]
fn enter_on_a_missing_member_is_a_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "shift-g enter");
    assert!(!h.band_open(&vcx));
    assert_eq!(h.notices(&vcx), ["NEW is not in the surface"]);
}

#[gpui::test]
fn the_band_follows_the_cursor_and_paints_between_the_grid_and_the_footer(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "enter");
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    let tag = tag_of(&h, &vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    let name =
        |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, _| t.band_model.name.to_string());
    assert_eq!(name(&vcx), "DAX");
    let band = vcx
        .debug_bounds("volwatch-band-7")
        .expect("the band is painted");
    let footer = vcx
        .debug_bounds("volwatch-footer-7")
        .expect("the footer is painted");
    assert!(
        band.bottom() <= footer.top(),
        "the band docks above the footer"
    );
    h.press(&mut vcx, "j");
    assert_eq!(name(&vcx), "SX5E", "the band follows the cursor");
    assert!(h.band_open(&vcx));
    h.press(&mut vcx, "escape");
    assert_eq!(
        h.cursor(&vcx),
        Some("SX5E".into()),
        "the cursor keeps its row"
    );
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        vcx.debug_bounds("volwatch-band-7").is_none(),
        "a closed band paints nothing"
    );
}

#[gpui::test]
fn a_restored_open_band_asks_for_its_measures_and_shows_once_the_row_lands(
    cx: &mut gpui::TestAppContext,
) {
    let record = crate::core::session::to_table(&crate::core::session::State {
        list: Some(LIST.into()),
        cursor: Some("SX5E".into()),
        band: true,
        ..Default::default()
    });
    let (h, mut vcx) = open_with(cx, one_list(), Some(record), VolwatchSettings::default());
    let reqs = h.requests();
    let b = Harness::boards(&reqs);
    assert_eq!(
        b[0].window,
        Some(WINDOW_1Y),
        "the first ask carries the band"
    );
    assert_eq!(b[0].sparklines.len(), 3);
    let asked = h.asked_of(&vcx);
    let tag = b[0].tag;
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.band_model.name.to_string()),
        "SX5E"
    );
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.serialize().get("band").cloned()),
        Some(toml::Value::Boolean(true))
    );
}

#[gpui::test]
fn vega_is_one_grouped_query_under_the_tile_key_scoped_to_the_members(
    cx: &mut gpui::TestAppContext,
) {
    let (h, _vcx) = open_with(
        cx,
        one_list(),
        Some(restored("europe_risk")),
        vega_settings(),
    );
    let reqs = h.requests();
    let q = queries(&reqs);
    assert_eq!(q.len(), 1);
    assert_eq!(
        (q[0].key, q[0].view.as_str(), q[0].max_depth),
        (KEY, "wide", 1)
    );
    assert_eq!(q[0].grouping, Some(vec!["underlying_ref".into()]));
    assert_eq!(q[0].scope.dimensions[0].values, ["DAX", "SX5E", "NEW"]);
    assert_eq!(Harness::boards(&reqs).len(), 1, "the board is its own ask");
}

fn vega_outcome(tag: u64, snapshot: Result<Arc<Snapshot>, String>) -> Delivery {
    Delivery::Query(QueryOutcome {
        key: KEY,
        tag,
        submitted: Instant::now(),
        snapshot,
    })
}

#[gpui::test]
fn vega_joins_by_name_into_the_column(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed_with_vega(cx);
    let tag = queries(&h.requests())[0].tag;
    let snap = vega_snapshot(&[("SX5E", -312_000.0), ("DAX", 40_000.0)]);
    h.deliver(&mut vcx, vega_outcome(tag, Ok(Arc::new(snap))));
    let col = h.column_index(&vcx, "vega");
    let text = |row: usize| {
        h.tile.read_with(&vcx, |t, _| {
            t.prepared.rows[row].cells[col].text.to_string()
        })
    };
    assert_eq!(text(0), "40", "DAX");
    assert_eq!(text(1), "-312", "SX5E, not the depth-0 total");
    assert_eq!(text(2), widths::DASH, "NEW has no vega");
}

#[gpui::test]
fn a_refused_vega_view_drops_the_column_and_warns(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed_with_vega(cx);
    let tag = queries(&h.requests())[0].tag;
    assert!(h.tile.read_with(&vcx, |t, _| {
        t.columns.iter().any(|c| c.kind == ColumnKind::Vega)
    }));
    h.deliver(
        &mut vcx,
        vega_outcome(tag, Err("'vega01_usd' is not a column".into())),
    );
    assert!(h.tile.read_with(&vcx, |t, _| {
        t.columns.iter().all(|c| c.kind != ColumnKind::Vega)
    }));
    assert_eq!(
        h.notices(&vcx),
        ["[volwatch] vega: 'vega01_usd' is not a column"]
    );
    assert_eq!(
        h.shown(&vcx),
        ["DAX", "SX5E", "NEW"],
        "the board keeps painting"
    );
}

#[gpui::test]
fn a_later_vega_answer_restores_the_column_and_clears_the_warning(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed_with_vega(cx);
    let tag = queries(&h.requests())[0].tag;
    h.deliver(&mut vcx, vega_outcome(tag, Err("gone".into())));
    h.press(&mut vcx, "shift-r");
    let reqs = h.requests();
    let tag = queries(&reqs)[0].tag;
    assert!(tag > 1);
    h.deliver(
        &mut vcx,
        vega_outcome(tag, Ok(Arc::new(vega_snapshot(&[("DAX", 1_000.0)])))),
    );
    assert!(h.tile.read_with(&vcx, |t, _| {
        t.columns.iter().any(|c| c.kind == ColumnKind::Vega)
    }));
    assert!(h.notices(&vcx).is_empty());
}

#[gpui::test]
fn a_stale_vega_tag_is_dropped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed_with_vega(cx);
    let first = queries(&h.requests())[0].tag;
    h.press(&mut vcx, "shift-r");
    h.deliver(&mut vcx, vega_outcome(first, Err("old".into())));
    assert!(
        h.notices(&vcx).is_empty(),
        "the superseded answer paints nothing"
    );
}

#[gpui::test]
fn a_frame_scope_change_asks_vega_alone(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed_with_vega(cx);
    h.requests();
    let sel = geode_core::scope::DimensionSelection {
        column: "book".into(),
        values: vec!["b1".into()],
    };
    h.frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(geode_core::scope::Scope {
            dimensions: vec![sel],
            ..Default::default()
        });
        cx.notify();
    });
    let reqs = h.requests();
    assert_eq!(queries(&reqs).len(), 1);
    assert!(Harness::boards(&reqs).is_empty());
}

#[gpui::test]
fn no_vega_setting_means_no_vega_column_and_no_query(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = landed(cx);
    assert!(queries(&h.requests()).is_empty());
    assert!(h.tile.read_with(&vcx, |t, _| {
        t.columns.iter().all(|c| c.kind != ColumnKind::Vega)
    }));
}
