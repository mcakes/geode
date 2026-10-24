//! The header's chips, the view strip, the footer's readout and the
//! per-row stale times, each driven through the shell's doors.

use std::time::Duration;

use chrono::Utc;
use geode_core::volboard::{Lookback, Role};
use geode_widgets::cell::widths;

use super::harness::*;
use crate::content::VolwatchSettings;
use crate::core::lens::Window;
use crate::tile::table::td_muted;

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
    assert!(h.tile.read_with(&vcx, |t, _| t.header.chips[1].1));
}

#[gpui::test]
fn a_failed_dataset_is_a_danger_chip_and_blanks_its_columns(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_on(cx, "varswap");
    let mut b = board_for(&["DAX", "SX5E", "NEW"], &asked_of(&h, &vcx));
    b.dataset_errors.push(("varswap".into(), "drifted".into()));
    blank_role(&mut b, Role::Varswap);
    let tag = tag_of(&h, &vcx);
    h.answer_board(&mut vcx, tag, b);
    assert!(chip_texts(&h, &vcx).contains(&"varswap".into()));
    let col = column_index(&h, &vcx, "varswap-3M-level");
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
        "SX5E · ATFM 3M 15.9 · Δ1d +1.1 from 14.8 on 2026-10-09 · z 1Y +1.2 · pctl 1Y 91 of 251 · mean 14.5 · sd 1.2"
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
    h.press(&mut vcx, "enter"); // the band opens on DAX
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.state.band && !t.band_model.stale)
    );
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
    // A stale row keeps its numbers' tone and colors: only its name paints
    // muted, beside the header's stale surface run.
    h.tile.read_with(&vcx, |t, _| {
        let row = &t.prepared.rows[0];
        assert!(
            row.cells.iter().all(|c| !c.muted),
            "the wake-up mutes no cell"
        );
        let muted: Vec<bool> = t
            .columns
            .iter()
            .zip(&row.cells)
            .map(|(column, cell)| td_muted(row, cell, column))
            .collect();
        assert!(muted[0], "the Underlying cell paints muted");
        assert!(
            muted[1..].iter().all(|m| !m),
            "every other cell paints as it is: {muted:?}"
        );
        assert!(
            t.band_model.stale,
            "the band's name follows the grid's Underlying cell"
        );
    });
}

#[gpui::test]
fn the_strip_lists_the_tabs_with_digits_the_lens_readout_and_the_hints(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.draw(&mut vcx);
    for name in ["vol", "skew", "varswap", "surface"] {
        assert!(
            vcx.debug_bounds(format!("volwatch-view-{name}-7").leak())
                .is_some(),
            "{name}"
        );
    }
    assert!(vcx.debug_bounds("volwatch-view-lit-vol").is_some());
    assert!(vcx.debug_bounds("volwatch-view-lit-skew").is_none());
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.state.lens.readout(false)),
        "moves \u{b7} \u{394} 1d"
    );
    assert!(vcx.debug_bounds("volwatch-lens-readout-7").is_some());
    assert!(vcx.debug_bounds("volwatch-strip-hint-lens-7").is_some());
    assert!(vcx.debug_bounds("volwatch-strip-hint-lookback-7").is_some());
    assert!(vcx.debug_bounds("volwatch-strip-hint-metric-7").is_none());
    assert!(
        vcx.debug_bounds("volwatch-strip-hint-shown-7").is_none(),
        "a pair view has no swap"
    );
    assert!(
        vcx.debug_bounds("volwatch-strip-hint-view_next-7")
            .is_some()
    );
    h.press(&mut vcx, "r");
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("volwatch-strip-hint-metric-7").is_some());
    assert!(vcx.debug_bounds("volwatch-strip-hint-window-7").is_some());
    assert!(vcx.debug_bounds("volwatch-strip-hint-lookback-7").is_none());
    h.press(&mut vcx, "4");
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("volwatch-view-lit-surface").is_some());
    assert!(
        vcx.debug_bounds("volwatch-strip-hint-shown-7").is_some(),
        "a single view hints the swap"
    );
    // Unbind the step, as a user keymap removal would: only its hint goes.
    vcx.update(|_, cx| {
        let kept: Vec<_> = cx
            .global::<geode_shell::tips::Chords>()
            .0
            .iter()
            .filter(|b| b.action.0 != crate::tile::VIEW_NEXT_ACTION)
            .cloned()
            .collect();
        cx.set_global(geode_shell::tips::Chords(std::sync::Arc::new(kept)));
    });
    vcx.run_until_parked();
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("volwatch-strip-hint-view_next-7")
            .is_none(),
        "the unbound action hints nothing"
    );
    assert!(
        vcx.debug_bounds("volwatch-strip-hint-lens-7").is_some(),
        "the bound ones still hint"
    );
}

#[gpui::test]
fn a_tenth_tab_paints_without_a_digit_and_a_press_still_switches(cx: &mut gpui::TestAppContext) {
    use geode_tile::menu::Trailing;
    let (h, mut vcx) = landed(cx);
    let mut settings = VolwatchSettings::default();
    let vol = settings.views.views[0].clone();
    for i in 5..=10 {
        let mut v = vol.clone();
        v.name = format!("extra{i}");
        settings.views.views.push(v);
    }
    vcx.update(|_, cx| h.factory.set_settings(settings, cx));
    h.draw(&mut vcx);
    let tenth = vcx
        .debug_bounds("volwatch-view-extra10-7")
        .expect("every tab paints");
    vcx.simulate_click(tenth.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(h.view(&vcx), "extra10");
    h.press(&mut vcx, ".");
    h.tile.read_with(&vcx, |t, _| {
        let rows: Vec<_> = t
            .menu
            .as_ref()
            .unwrap()
            .menu
            .rows()
            .iter()
            .filter_map(geode_tile::menu::Row::action)
            .cloned()
            .collect();
        let ninth = rows.iter().find(|a| a.title().as_ref() == "extra9 view");
        let tenth = rows.iter().find(|a| a.title().as_ref() == "extra10 view");
        assert!(
            matches!(ninth.unwrap().trailing(), Trailing::Keys(_)),
            "the ninth tab carries its digit"
        );
        assert!(
            matches!(tenth.unwrap().trailing(), Trailing::None),
            "the tenth has no digit"
        );
        assert_eq!(tenth.unwrap().tick(), Some(true));
    });
}

#[gpui::test]
fn the_view_strip_lights_the_active_view_and_a_press_switches(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    assert!(vcx.debug_bounds("volwatch-view-lit-vol").is_some());
    assert!(vcx.debug_bounds("volwatch-view-lit-surface").is_none());
    let bounds = vcx
        .debug_bounds("volwatch-view-surface-7")
        .expect("the strip paints every tab");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(h.view(&vcx), "surface");
    assert!(vcx.debug_bounds("volwatch-view-lit-surface").is_some());
    assert!(vcx.debug_bounds("volwatch-view-lit-vol").is_none());
}

#[gpui::test]
fn a_reload_that_drops_the_shown_view_opens_the_default_with_a_notice(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "3");
    assert_eq!(h.view(&vcx), "varswap");
    h.press(&mut vcx, "enter");
    assert!(h.band_open(&vcx));
    let mut settings = VolwatchSettings::default();
    settings.views.views.retain(|v| v.name != "varswap");
    vcx.update(|_, cx| h.factory.set_settings(settings, cx));
    assert_eq!(h.view(&vcx), "vol");
    assert_eq!(h.notices(&vcx), ["view varswap is gone"]);
    assert!(h.band_open(&vcx), "the band stays on the cursor row");
    assert_eq!(h.cursor(&vcx), Some("DAX".into()));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.tabs()),
        ["vol", "skew", "surface"]
    );
}

#[gpui::test]
fn a_reload_that_relabels_a_column_reaches_the_grid_without_a_board(cx: &mut gpui::TestAppContext) {
    use geode_core::volboard::Measure;
    use geode_core::volviews::VolColumn;
    let (h, mut vcx) = landed(cx);
    let mut settings = VolwatchSettings::default();
    let vol = settings
        .views
        .views
        .iter_mut()
        .find(|v| v.name == "vol")
        .unwrap();
    for c in &mut vol.columns {
        if let VolColumn::Measure { measure, label, .. } = c
            && *measure == Measure::atfm("3M")
        {
            *label = Some("3m vol".into());
        }
    }
    vcx.update(|_, cx| h.factory.set_settings(settings, cx));
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "a label asks nothing of the board"
    );
    h.tile.read_with(&vcx, |t, cx| {
        let d = t.table.read(cx).delegate();
        let head = d
            .columns
            .iter()
            .find(|c| c.id == "atfm-3M-level")
            .map(|c| c.head.clone());
        assert_eq!(
            head.as_deref(),
            Some("3m vol"),
            "the grid holds the new head"
        );
        assert!(
            std::rc::Rc::ptr_eq(&d.prepared, &t.prepared),
            "the grid holds the re-prepared model"
        );
        assert_eq!(t.prepared.rows.len(), 3);
        assert!(
            t.prepared
                .rows
                .iter()
                .all(|r| r.cells.len() == t.columns.len()),
            "every row is prepared over the new columns"
        );
    });
}

#[gpui::test]
fn a_reload_that_relabels_the_sorted_column_renames_the_sort_chip(cx: &mut gpui::TestAppContext) {
    use geode_core::volboard::Measure;
    use geode_core::volviews::VolColumn;
    let (h, mut vcx) = landed(cx);
    h.command(&mut vcx, "sort atfm-3M-level").unwrap();
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header.sort.clone()),
        Some("vol 3M \u{2191}".into())
    );
    let mut settings = VolwatchSettings::default();
    let vol = settings
        .views
        .views
        .iter_mut()
        .find(|v| v.name == "vol")
        .unwrap();
    for c in &mut vol.columns {
        if let VolColumn::Measure { measure, label, .. } = c
            && *measure == Measure::atfm("3M")
        {
            *label = Some("3m vol".into());
        }
    }
    vcx.update(|_, cx| h.factory.set_settings(settings, cx));
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "a label asks nothing of the board"
    );
    // The chip is rebuilt from the new columns, not the ones it was set on.
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header.sort.clone()),
        Some("3m vol 3M \u{2191}".into())
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.state.sort.clone()),
        Some(("atfm-3M-level".into(), geode_core::sort::SortOrder::Asc)),
        "the sort stays on the column by id"
    );
}

#[gpui::test]
fn a_reload_that_keeps_the_shown_view_keeps_it_and_requeries_only_for_new_measures(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    let mut settings = VolwatchSettings::default();
    // Reorder: vol stays, now second.
    let vol = settings.views.views.remove(0);
    settings.views.views.push(vol);
    settings.views.default = 0;
    vcx.update(|_, cx| h.factory.set_settings(settings, cx));
    assert_eq!(h.view(&vcx), "vol");
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "the same columns ask nothing"
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.tabs()),
        ["skew", "varswap", "surface", "vol"]
    );
}

#[gpui::test]
fn the_header_chips_follow_the_lens_and_a_press_sets_the_lookback_or_the_window(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("volwatch-lookback-1w").is_some());
    assert!(vcx.debug_bounds("volwatch-window-6M").is_none());
    let at = vcx.debug_bounds("volwatch-lookback-1w").unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(h.lens(&vcx).lookback, Lookback::W1);
    let boards = h.requests();
    let boards = Harness::boards(&boards);
    assert_eq!(boards.len(), 1);
    assert_eq!(
        boards[0].lookbacks,
        [Lookback::W1],
        "the press asks as `d` does"
    );
    h.press(&mut vcx, "r");
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("volwatch-lookback-1w").is_none());
    assert!(vcx.debug_bounds("volwatch-window-2Y").is_some());
    let at = vcx.debug_bounds("volwatch-window-2Y").unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(h.lens(&vcx).window, Window::Y2);
    assert_eq!(Harness::boards(&h.requests()).len(), 1);
    assert_eq!(
        chip_texts(&h, &vcx)[3..6],
        ["6M".to_string(), "1Y".into(), "z \u{b7} 2Y".into()]
    );
}

#[gpui::test]
fn the_menu_lists_every_tab_with_its_digit_and_ticks_the_shown_one(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "2");
    h.press(&mut vcx, ".");
    let titles = h.menu_titles(&vcx);
    for name in ["vol", "skew", "varswap", "surface"] {
        assert!(
            titles.iter().any(|t| t == &format!("{name} view")),
            "{name} in {titles:?}"
        );
    }
    // The tick is the row's own slot (`\u{2713}` in paint), as the paint
    // verb's is; the title never carries it.
    h.tile.read_with(&vcx, |t, _| {
        let ticked: Vec<(String, Option<bool>)> = t
            .menu
            .as_ref()
            .unwrap()
            .menu
            .rows()
            .iter()
            .filter_map(geode_tile::menu::Row::action)
            .filter(|a| a.title().ends_with(" view"))
            .map(|a| (a.title().to_string(), a.tick()))
            .collect();
        assert_eq!(
            ticked,
            [
                ("vol view".to_string(), Some(false)),
                ("skew view".into(), Some(true)),
                ("varswap view".into(), Some(false)),
                ("surface view".into(), Some(false)),
            ]
        );
    });
    // A pick switches through the view door, as the digit does.
    let at = titles.iter().position(|t| t == "surface view").unwrap();
    for _ in 0..at {
        h.press(&mut vcx, "j");
    }
    h.press(&mut vcx, "enter");
    assert_eq!(h.view(&vcx), "surface");
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
