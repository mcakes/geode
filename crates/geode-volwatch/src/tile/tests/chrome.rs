//! The header's chips, the view strip, the footer's readout and the
//! per-row stale times, each driven through the shell's doors.

use std::time::Duration;

use chrono::Utc;
use geode_core::volboard::Role;
use geode_widgets::cell::widths;

use super::harness::*;
use crate::content::VolwatchSettings;
use crate::core::views::View;

#[gpui::test]
fn the_header_names_the_list_count_lookback_sort_and_the_missing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    assert_eq!(
        chip_texts(&h, &vcx),
        [
            "▤",
            "Vol · europe_risk ▾",
            "3",
            "Δ 1d",
            "1w",
            "1m",
            "1 not in surface"
        ]
    );
    h.press(&mut vcx, "d");
    assert!(chip_texts(&h, &vcx).contains(&"Δ 1w".into()));
    assert!(h.tile.read_with(&vcx, |t, _| t.header.lookbacks[1].1));
}

#[gpui::test]
fn a_failed_dataset_is_a_danger_chip_and_blanks_its_columns(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_on(cx, View::Levels);
    let mut b = board_for(&["DAX", "SX5E", "NEW"], &asked_of(&h, &vcx));
    b.dataset_errors
        .push(("realized_vol".into(), "drifted".into()));
    blank_role(&mut b, Role::Realized);
    let tag = tag_of(&h, &vcx);
    h.answer_board(&mut vcx, tag, b);
    assert!(chip_texts(&h, &vcx).contains(&"realized_vol".into()));
    let col = column_index(&h, &vcx, "rlz-5D-level");
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.prepared.rows[0].cells[col].text.to_string()),
        widths::DASH
    );
}

#[gpui::test]
fn the_readout_follows_the_cursor_cell(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "j");
    h.move_to_column(&mut vcx, "atfm-3M-level");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.readout.to_string()),
        "SX5E · ATFM 3M 15.9 · Δ1d +1.1 from 14.8 on 2026-10-09"
    );
    h.press(&mut vcx, "j");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.readout.to_string()),
        "NEW · not in surface",
        "a row move re-reads the readout"
    );
}

#[gpui::test]
fn the_stalest_row_times_the_header_and_each_row_arms_its_own_stale(cx: &mut gpui::TestAppContext) {
    let settings = VolwatchSettings {
        stale_after: Duration::from_secs(60),
        ..Default::default()
    };
    let (h, mut vcx) = open_with(cx, one_list(), Some(restored("europe_risk")), settings);
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    let mut b = board_for(&["DAX", "SX5E", "NEW"], &asked);
    b.rows[0].as_of = Some(Utc::now() - chrono::Duration::seconds(120)); // DAX is dead
    h.answer_board(&mut vcx, tag, b);
    assert!(h.tile.read_with(&vcx, |t, _| t.prepared.rows[0].stale
        && !t.prepared.rows[1].stale));
    assert!(
        h.tile.read_with(&vcx, |t, _| t.times[0].stale),
        "the header's surface run is the stalest row"
    );
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.times[0].label.starts_with("surface ")),
        "the run names its dataset"
    );
}

#[gpui::test]
fn a_stale_after_reload_rearms_the_row_wake_ups(cx: &mut gpui::TestAppContext) {
    let settings = VolwatchSettings {
        stale_after: Duration::from_secs(600),
        ..Default::default()
    };
    let (h, mut vcx) = open_with(cx, one_list(), Some(restored("europe_risk")), settings);
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.tile.read_with(&vcx, |t, _| t.asked.clone().unwrap());
    let mut b = board_for(&["DAX"], &asked);
    b.rows[0].as_of = Some(Utc::now() - chrono::Duration::seconds(120));
    h.answer_board(&mut vcx, tag, b);
    assert!(h.tile.read_with(&vcx, |t, _| !t.prepared.rows[0].stale));
    // 120 s old: fresh under 600 s, stale 60 s from now under 180 s.
    vcx.update(|_, cx| {
        h.factory.set_settings(
            VolwatchSettings {
                stale_after: Duration::from_secs(180),
                ..Default::default()
            },
            cx,
        )
    });
    assert!(h.tile.read_with(&vcx, |t, _| !t.prepared.rows[0].stale));
    cx.executor().advance_clock(Duration::from_secs(70));
    vcx.run_until_parked();
    assert!(
        h.tile.read_with(&vcx, |t, _| t.prepared.rows[0].stale),
        "the row turns stale at the reloaded threshold"
    );
}

#[gpui::test]
fn the_view_strip_lights_the_active_view_and_a_press_switches(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    assert!(vcx.debug_bounds("volwatch-view-lit-changes").is_some());
    assert!(vcx.debug_bounds("volwatch-view-lit-term").is_none());
    let bounds = vcx
        .debug_bounds("volwatch-view-term")
        .expect("the strip paints every view");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(h.view(&vcx), View::Term);
    assert!(vcx.debug_bounds("volwatch-view-lit-term").is_some());
    assert!(vcx.debug_bounds("volwatch-view-lit-changes").is_none());
}

#[gpui::test]
fn a_lookback_chip_press_sets_that_lookback(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    let bounds = vcx
        .debug_bounds("volwatch-lookback-1m")
        .expect("the header paints every lookback");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(h.lookback(&vcx), geode_core::volboard::Lookback::M1);
    assert_eq!(
        Harness::boards(&h.requests())[0].lookbacks,
        [geode_core::volboard::Lookback::M1],
        "the press asks as `d` does"
    );
}

#[gpui::test]
fn group_heads_and_the_band_paint_at_the_grids_text_size(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "enter"); // the band opens
    h.draw(&mut vcx);
    let head = vcx
        .debug_bounds("volwatch-th-text-atfm-1M-level")
        .expect("the head's text is painted")
        .size
        .height;
    let group = vcx
        .debug_bounds("volwatch-group-ATFM 1M")
        .expect("the group label is painted")
        .size
        .height;
    // The band's name is keyed by the tile (`TILE`, 7).
    let band_name = vcx
        .debug_bounds("volwatch-band-name-7")
        .expect("the band's name is painted")
        .size
        .height;
    // Same line height => same text size (the elements are single-line
    // text in one font).
    assert!(
        (head - group).abs() <= gpui::px(1.0),
        "group {group:?} vs head {head:?}"
    );
    assert!(
        (head - band_name).abs() <= gpui::px(1.0),
        "band {band_name:?} vs head {head:?}"
    );
}
