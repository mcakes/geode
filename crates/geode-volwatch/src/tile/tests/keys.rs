//! The grid's keys, pointer, sort, narrowing, yank, emission and paint,
//! each driven through the shell's doors over a landed board.

use std::cell::Cell;
use std::rc::Rc;

use geode_core::link::{CursorScope, underlying_scope};
use geode_core::volboard::{Lookback, Measure};
use geode_shell::module::FindEvent;
use gpui_component::ActiveTheme as _;

use super::harness::*;
use crate::content::VolwatchSettings;
use crate::core::views::View;

#[gpui::test]
fn every_key_reaches_its_action(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    for (keys, action) in [
        ("j", "volwatch::down"),
        ("k", "volwatch::up"),
        ("l", "volwatch::right"),
        ("h", "volwatch::left"),
        ("2", "volwatch::view_2"),
        ("tab", "volwatch::view_next"),
        ("d", "volwatch::lookback"),
        ("p", "volwatch::paint"),
        ("s", "volwatch::sort"),
        ("y", "volwatch::yank"),
        ("v", "volwatch::visual_rows"),
        ("escape", "volwatch::escape"),
        ("shift-r", "volwatch::refresh"),
        ("enter", "volwatch::inspect"),
        ("escape", "volwatch::escape"),
        ("t", "volwatch::timeseries"),
        (".", "volwatch::menu"),
        ("escape", "volwatch::escape"),
        ("g w", "volwatch::switch"),
        ("escape", "volwatch::escape"),
    ] {
        h.press(&mut vcx, keys);
        assert_eq!(
            h.dispatched(&vcx).last().map(String::as_str),
            Some(action),
            "{keys}"
        );
    }
}

#[gpui::test]
fn j_k_and_g_move_the_cursor_and_h_l_the_column(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    assert_eq!(h.cursor(&vcx), Some("DAX".into()));
    h.press(&mut vcx, "j");
    assert_eq!(h.cursor(&vcx), Some("SX5E".into()));
    h.press(&mut vcx, "shift-g");
    assert_eq!(h.cursor(&vcx), Some("NEW".into()));
    h.press(&mut vcx, "g g");
    assert_eq!(h.cursor(&vcx), Some("DAX".into()));
    h.press(&mut vcx, "h h");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.cursor_col),
        1,
        "never onto Underlying"
    );
    h.press(&mut vcx, "l");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor_col), 2);
}

#[gpui::test]
fn digits_switch_views_and_requery_only_when_the_ask_grows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "2"); // Levels reads realized + varswap: new measures
    assert_eq!(h.view(&vcx), View::Levels);
    assert_eq!(Harness::boards(&h.requests()).len(), 1);
    let asked = h.asked_of(&vcx);
    let tag = h.tag_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    h.press(&mut vcx, "1"); // the 30d sparkline is not in Levels' ask → requery
    assert_eq!(Harness::boards(&h.requests()).len(), 1);
    h.press(&mut vcx, "tab");
    assert_eq!(h.view(&vcx), View::Levels);
}

#[gpui::test]
fn d_cycles_the_lookback_requeries_and_mutes_the_old_changes_until_the_board_lands(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "d");
    assert_eq!(h.lookback(&vcx), Lookback::W1);
    assert_eq!(Harness::boards(&h.requests())[0].lookbacks, [Lookback::W1]);
    let col = h.column_index(&vcx, "atfm-3M-change");
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.prepared.rows[0].cells[col].muted)
    );
    h.press(&mut vcx, "d d");
    assert_eq!(h.lookback(&vcx), Lookback::D1, "wraps");
}

#[gpui::test]
fn p_toggles_the_glyphs_without_asking(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    let col = h.column_index(&vcx, "atfm-3M-change");
    let glyph = |vcx: &gpui::VisualTestContext| {
        h.tile
            .read_with(vcx, |t, _| t.prepared.rows[0].cells[col].glyph.clone())
    };
    assert!(matches!(glyph(&vcx), crate::core::model::Glyph::Chip(_)));
    h.press(&mut vcx, "p");
    assert_eq!(glyph(&vcx), crate::core::model::Glyph::None);
    assert!(h.requests().is_empty());
}

/// SX5E at 17.0 where DAX reads 15.9; NEW is not in the surface.
fn sx5e_higher(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    landed_board(cx, |board| {
        let row = board
            .rows
            .iter_mut()
            .find(|r| r.underlying == "SX5E")
            .unwrap();
        for c in &mut row.cells {
            if c.measure == Measure::atfm("3M") {
                c.current = Some(17.0);
            }
        }
    })
}

/// `s` walks the shared `SortOrder::cycle`: asc → desc → member order.
#[gpui::test]
fn s_cycles_the_sort_on_the_cursor_column_and_blanks_sort_last(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = sx5e_higher(cx);
    h.move_to_column(&mut vcx, "atfm-3M-level");
    h.press(&mut vcx, "s");
    assert_eq!(
        h.shown(&vcx),
        ["DAX", "SX5E", "NEW"],
        "asc, the missing member last"
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header.sort.clone()),
        Some("vol 3M".into())
    );
    h.press(&mut vcx, "s");
    assert_eq!(
        h.shown(&vcx),
        ["SX5E", "DAX", "NEW"],
        "desc, the missing member last"
    );
    h.press(&mut vcx, "s");
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"], "off: member order");
    assert!(h.tile.read_with(&vcx, |t, _| t.state.sort.is_none()));
    h.command(&mut vcx, "sort atfm-3M-change abs").unwrap();
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.header.sort.as_deref() == Some("|Δ 3M|"))
    );
    h.command(&mut vcx, "sort").unwrap();
    assert!(h.tile.read_with(&vcx, |t, _| t.header.sort.is_none()));
}

#[gpui::test]
fn a_spark_column_refuses_to_sort_with_a_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.move_to_column(&mut vcx, "spark-0");
    h.press(&mut vcx, "s");
    assert_eq!(h.notices(&vcx), ["ATFM 3M · 30d is not sortable"]);
    assert!(h.tile.read_with(&vcx, |t, _| t.state.sort.is_none()));
    h.press(&mut vcx, "j");
    assert!(
        h.notices(&vcx).is_empty(),
        "the refusal goes with the next key"
    );
}

#[gpui::test]
fn a_header_click_walks_the_click_cycle(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = sx5e_higher(cx);
    let click = |vcx: &mut gpui::VisualTestContext| {
        let bounds = vcx
            .debug_bounds("volwatch-th-atfm-3M-level")
            .expect("the head is painted");
        vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
        vcx.run_until_parked();
    };
    click(&mut vcx);
    assert_eq!(h.shown(&vcx), ["SX5E", "DAX", "NEW"], "desc first");
    click(&mut vcx);
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"], "asc");
}

#[gpui::test]
fn a_row_press_takes_the_cursor_and_shift_extends(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    let press = |vcx: &mut gpui::VisualTestContext, row: &'static str, mods: gpui::Modifiers| {
        let bounds = vcx.debug_bounds(row).expect("the row is painted");
        vcx.simulate_mouse_down(bounds.center(), gpui::MouseButton::Left, mods);
        vcx.simulate_mouse_up(bounds.center(), gpui::MouseButton::Left, mods);
        vcx.run_until_parked();
    };
    press(&mut vcx, "volwatch-row-SX5E", gpui::Modifiers::none());
    assert_eq!(h.cursor(&vcx), Some("SX5E".into()));
    press(&mut vcx, "volwatch-row-NEW", gpui::Modifiers::shift());
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.grid.selected()), Some(1..3));
}

#[gpui::test]
fn slash_narrows_over_name_and_reference_keeping_order(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed_with_reference(cx, &[("SX5E", "Euro Stoxx 50")]);
    h.find(&mut vcx, FindEvent::Changed("stoxx".into()));
    assert_eq!(h.shown(&vcx), ["SX5E"]);
    h.find(&mut vcx, FindEvent::Cancelled);
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"]);
}

#[gpui::test]
fn escape_ends_a_selection_then_a_narrowing_then_answers_false(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.find(&mut vcx, FindEvent::Committed("dax".into()));
    assert_eq!(h.shown(&vcx), ["DAX"]);
    h.press(&mut vcx, "v");
    assert!(h.tile.read_with(&vcx, |t, _| t.grid.selecting()));
    assert!(h.act(&mut vcx, "volwatch::escape"));
    assert!(!h.tile.read_with(&vcx, |t, _| t.grid.selecting()));
    assert_eq!(
        h.shown(&vcx),
        ["DAX"],
        "the narrowing outlives the selection"
    );
    assert!(h.act(&mut vcx, "volwatch::escape"));
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"]);
    assert!(!h.act(&mut vcx, "volwatch::escape"), "nothing left");
}

#[gpui::test]
fn y_yanks_the_cursor_cells_text(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    // The cursor starts on fwd Δ%: 1.1 from 14.8 is +7.4%.
    h.press(&mut vcx, "y");
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()),
        Some("+7.4".into())
    );
    h.press(&mut vcx, "l");
    h.press(&mut vcx, "y");
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()),
        Some("15.9".into())
    );
}

#[gpui::test]
fn y_over_a_selection_yanks_the_column_one_row_per_line_and_ends_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "l");
    h.press(&mut vcx, "v j j y");
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()),
        Some("15.9\n15.9\n\u{2014}".into())
    );
    assert!(!h.tile.read_with(&vcx, |t, _| t.grid.selecting()));
}

#[gpui::test]
fn the_cursor_row_is_emitted_as_a_one_value_underlying_path(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    let changed = Rc::new(Cell::new(0));
    let _sub = vcx.update(|_, cx| {
        h.content.watch_emission(
            Rc::new({
                let c = changed.clone();
                move |_| c.set(c.get() + 1)
            }),
            cx,
        )
    });
    h.press(&mut vcx, "j");
    assert!(changed.get() >= 1);
    let e = vcx.update(|_, cx| h.content.emission(cx));
    assert_eq!(e.cursor, CursorScope::Path(underlying_scope("SX5E")));
    assert!(h.content.emits());
}

#[gpui::test]
fn a_theme_change_rebuilds_the_paint(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    let before = h
        .tile
        .read_with(&vcx, |t, cx| t.table.read(cx).delegate().paints);
    vcx.update(|window, cx| {
        let other = if cx.theme().mode.is_dark() {
            gpui_component::ThemeMode::Light
        } else {
            gpui_component::ThemeMode::Dark
        };
        gpui_component::Theme::change(other, Some(window), cx);
    });
    vcx.run_until_parked();
    let (after, fresh) = h.tile.read_with(&vcx, |t, cx| {
        (
            t.table.read(cx).delegate().paints,
            crate::tile::table::Paints::from_theme(cx.theme()),
        )
    });
    assert_ne!(before, after);
    assert_eq!(after, fresh, "the paints are the new theme's");
}

#[gpui::test]
fn the_cursor_waits_for_its_restored_row(cx: &mut gpui::TestAppContext) {
    let mut t = restored(LIST);
    t.insert("cursor".into(), toml::Value::String("SX5E".into()));
    let (h, mut vcx) = open_with(cx, one_list(), Some(t), VolwatchSettings::default());
    // The members are rows before any board (all blank): the cursor rests
    // on its restored name at once, never on the first row.
    assert_eq!(h.cursor(&vcx), Some("SX5E".into()));
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.asked_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    assert_eq!(h.cursor(&vcx), Some("SX5E".into()));
}
