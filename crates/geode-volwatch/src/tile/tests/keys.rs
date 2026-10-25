//! The grid's keys, pointer, sort, narrowing, yank, emission and paint,
//! each driven through the shell's doors over a landed board.

use std::cell::Cell;
use std::rc::Rc;

use geode_core::link::{CursorScope, underlying_scope};
use geode_core::volboard::{Lookback, Measure};
use geode_shell::module::FindEvent;
use gpui_component::ActiveTheme as _;
use gpui_component::table::TableEvent;

use super::harness::*;
use crate::content::VolwatchSettings;
use crate::core::lens::{Lens, Metric, Shown, Window};
use crate::tile::table::sized_key;

#[gpui::test]
fn every_key_reaches_its_action(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    for (keys, action) in [
        ("j", "motion::down"),
        ("k", "motion::up"),
        ("l", "motion::right"),
        ("h", "motion::left"),
        ("g 2", "volwatch::view_2"),
        ("g 4", "volwatch::view_4"),
        ("tab", "volwatch::view_next"),
        ("shift-tab", "volwatch::view_prev"),
        ("r", "volwatch::lens"),
        ("q", "volwatch::metric"),
        ("w", "volwatch::window"),
        ("m", "volwatch::shown"),
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
        ("space", "volwatch::toggle"),
        ("z o", "volwatch::expand"),
        ("z c", "volwatch::collapse"),
        ("z a", "volwatch::toggle"),
        ("z shift-r", "volwatch::expand_all"),
        ("z shift-m", "volwatch::collapse_all"),
        ("shift-l", "volwatch::col_right"),
        ("shift-h", "volwatch::col_left"),
        ("shift-s", "volwatch::sort_abs"),
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
fn digits_and_tab_walk_the_configured_tabs_and_requery_only_when_the_ask_grows(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    assert_eq!(h.view(&vcx), "vol");
    h.press(&mut vcx, "g 2"); // skew reads other measures
    assert_eq!(h.view(&vcx), "skew");
    assert_eq!(Harness::boards(&h.requests()).len(), 1);
    let asked = h.asked_of(&vcx);
    let tag = h.tag_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    h.press(&mut vcx, "g 1"); // the vol spark and vega are not in skew's ask → requery
    assert_eq!(Harness::boards(&h.requests()).len(), 1);
    h.press(&mut vcx, "shift-tab");
    assert_eq!(h.view(&vcx), "surface", "wraps backwards");
    h.press(&mut vcx, "tab");
    assert_eq!(h.view(&vcx), "vol");
    h.press(&mut vcx, "g 9");
    assert_eq!(
        h.view(&vcx),
        "vol",
        "a digit past the last tab does nothing"
    );
    assert!(h.notices(&vcx).is_empty());
}

/// A bare digit is a count for the motion that follows, never a tab: the
/// tabs moved to `g N` so the shell's count grammar could reach the grid.
#[gpui::test]
fn a_bare_digit_is_a_count_and_g_digit_picks_the_tab(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "2 j");
    assert_eq!(h.view(&vcx), "vol", "a bare 2 switches no tab");
    assert_eq!(h.cursor(&vcx), Some("NEW".into()), "2 j moves two rows");
    assert!(h.dispatched(&vcx).iter().all(|a| a != "volwatch::view_2"));
    h.press(&mut vcx, "g 2");
    assert_eq!(h.view(&vcx), "skew");
}

/// Twelve rows: the four the surface holds first, the eight it lacks at
/// the foot, so a page motion has room to move and clamp.
fn twelve_rows(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let names = [
        "DAX", "SX5E", "UKX", "SPX", "A5", "A6", "A7", "A8", "A9", "A10", "A11", "A12",
    ];
    let (h, mut vcx) = open_with(
        cx,
        list_of(names),
        Some(restored_on("vol")),
        VolwatchSettings::default(),
    );
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.asked_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&names, &asked));
    h.requests();
    assert_eq!(h.shown(&vcx).len(), 12);
    (h, vcx)
}

/// The shared grid rules, as the keymaps guide states them: a count
/// multiplies a step and clamps, the pages are 5 and 10 rows, a counted
/// `shift+g` is that 1-based row, `^`/`$` reach the first and last column.
#[gpui::test]
fn a_count_multiplies_a_row_motion_and_switches_no_tab(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = twelve_rows(cx);
    let shown = h.shown(&vcx);
    let row = |h: &Harness, vcx: &gpui::VisualTestContext| {
        let name = h.cursor(vcx).expect("a cursor");
        shown.iter().position(|n| *n == name).expect("a shown row")
    };
    h.press(&mut vcx, "3 j");
    assert_eq!(row(&h, &vcx), 3, "3 j moves three rows");
    assert_eq!(h.view(&vcx), "vol", "the 3 is a count, not a tab");
    h.press(&mut vcx, "ctrl-d");
    assert_eq!(row(&h, &vcx), 8, "ctrl+d is five rows");
    h.press(&mut vcx, "ctrl-f");
    assert_eq!(row(&h, &vcx), 11, "ctrl+f is ten rows, clamped to the last");
    h.press(&mut vcx, "1 j");
    assert_eq!(row(&h, &vcx), 11, "a counted step clamps rather than wraps");
    h.press(&mut vcx, "j");
    assert_eq!(row(&h, &vcx), 0, "a bare step wraps");
    h.press(&mut vcx, "5 shift-g");
    assert_eq!(row(&h, &vcx), 4, "5 shift+g is row 5");
    h.press(&mut vcx, "ctrl-u");
    assert_eq!(
        row(&h, &vcx),
        0,
        "ctrl+u is five rows, clamped to the first"
    );
    h.press(&mut vcx, "shift-g");
    assert_eq!(row(&h, &vcx), 11);
    h.press(&mut vcx, "g g");
    assert_eq!(row(&h, &vcx), 0);
    h.press(&mut vcx, "2 ctrl-d");
    assert_eq!(row(&h, &vcx), 10, "a count multiplies a page");
    let last = h.tile.read_with(&vcx, |t, _| t.columns.len() - 1);
    h.press(&mut vcx, "$");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor_col), last);
    h.press(&mut vcx, "^");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.cursor_col),
        1,
        "the first column after Underlying"
    );
    h.press(&mut vcx, "3 l");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor_col), 4);
}

/// The column ids in painted order.
fn ids(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
    h.tile
        .read_with(vcx, |t, _| t.columns.iter().map(|c| c.id.clone()).collect())
}

/// The saved order of `view` in the serialized record.
fn saved_order(h: &Harness, vcx: &mut gpui::VisualTestContext, view: &str) -> Option<Vec<String>> {
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    let ids = saved
        .get("column_order")?
        .as_table()?
        .get(view)?
        .as_array()?;
    Some(
        ids.iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect(),
    )
}

#[gpui::test]
fn shift_l_moves_the_measure_and_its_companion_together_and_the_session_keeps_it(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    let before = ids(&h, &vcx);
    assert_eq!(
        &before[..5],
        [
            "underlying",
            "atfm-1W-level",
            "atfm-1W-companion",
            "atfm-1M-level",
            "atfm-1M-companion"
        ]
    );
    h.move_to_column(&mut vcx, "atfm-1W-level");
    h.press(&mut vcx, "shift-l");
    let after = ids(&h, &vcx);
    assert_eq!(
        &after[..5],
        [
            "underlying",
            "atfm-1M-level",
            "atfm-1M-companion",
            "atfm-1W-level",
            "atfm-1W-companion"
        ]
    );
    assert_eq!(after[5..], before[5..], "the rest keep their place");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.cursor_col),
        3,
        "the cursor follows the moved column"
    );
    assert!(h.requests().is_empty(), "a reorder never requeries");
    assert!(h.notices(&vcx).is_empty());
    // The session holds the order as measure stems, one per unit.
    let order = saved_order(&h, &mut vcx, "vol").expect("the order is saved");
    assert_eq!(
        order,
        [
            "atfm-1M",
            "atfm-1W",
            "atfm-3M",
            "atfm-6M",
            "atfm-1Y",
            "atfm-2Y",
            "atfm-3Y",
            "atfm-5Y",
            "spark-atfm-3M"
        ]
    );
    // A restore applies it before the first paint.
    let mut record = restored_on("vol");
    let mut orders = toml::Table::new();
    orders.insert(
        "vol".into(),
        toml::Value::Array(order.into_iter().map(toml::Value::String).collect()),
    );
    record.insert("column_order".into(), toml::Value::Table(orders));
    let (h2, vcx2) = open_with(cx, one_list(), Some(record), VolwatchSettings::default());
    assert_eq!(ids(&h2, &vcx2), after);
}

#[gpui::test]
fn shift_h_at_underlying_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    let before = ids(&h, &vcx);
    h.move_to_column(&mut vcx, "atfm-1W-companion");
    h.press(&mut vcx, "shift-h");
    assert_eq!(ids(&h, &vcx), before, "nothing moves past Underlying");
    assert_eq!(h.notices(&vcx), ["that is as far as this column goes"]);
    assert_eq!(
        saved_order(&h, &mut vcx, "vol"),
        None,
        "a refusal saves nothing"
    );
    // The far end refuses the same way.
    h.press(&mut vcx, "escape");
    h.press(&mut vcx, "$");
    h.press(&mut vcx, "shift-l");
    assert_eq!(ids(&h, &vcx), before);
    assert_eq!(h.notices(&vcx), ["that is as far as this column goes"]);
}

#[gpui::test]
fn a_spark_unit_moves_alone_and_a_count_moves_that_many(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.move_to_column(&mut vcx, "spark-atfm-3M");
    h.press(&mut vcx, "shift-h");
    let got = ids(&h, &vcx);
    let n = got.len();
    assert_eq!(
        &got[n - 3..],
        ["spark-atfm-3M", "atfm-5Y-level", "atfm-5Y-companion"],
        "the sparkline steps over the whole pair"
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.cursor_col),
        n - 3,
        "the cursor follows"
    );
    h.move_to_column(&mut vcx, "atfm-1W-level");
    h.press(&mut vcx, "2 shift-l");
    let got = ids(&h, &vcx);
    assert_eq!(
        &got[1..7],
        [
            "atfm-1M-level",
            "atfm-1M-companion",
            "atfm-3M-level",
            "atfm-3M-companion",
            "atfm-1W-level",
            "atfm-1W-companion"
        ],
        "2 shift+l moves two places"
    );
    assert_eq!(h.view(&vcx), "vol", "the 2 is a count");
    // A count past the end stops at the end; the next press refuses.
    h.press(&mut vcx, "9 9 shift-l");
    let got = ids(&h, &vcx);
    assert_eq!(
        &got[got.len() - 2..],
        ["atfm-1W-level", "atfm-1W-companion"]
    );
    assert!(h.notices(&vcx).is_empty());
    h.press(&mut vcx, "shift-l");
    assert_eq!(h.notices(&vcx), ["that is as far as this column goes"]);
}

#[gpui::test]
fn a_saved_id_the_tab_no_longer_has_is_ignored_and_dropped_on_the_next_save(
    cx: &mut gpui::TestAppContext,
) {
    let mut record = restored_on("vol");
    let mut orders = toml::Table::new();
    orders.insert(
        "vol".into(),
        toml::Value::Array(vec![
            toml::Value::String("nope".into()),
            toml::Value::String("atfm-3M".into()),
        ]),
    );
    record.insert("column_order".into(), toml::Value::Table(orders));
    let (h, mut vcx) = landed_with_record(cx, record);
    let got = ids(&h, &vcx);
    assert_eq!(
        &got[..5],
        [
            "underlying",
            "atfm-3M-level",
            "atfm-3M-companion",
            "atfm-1W-level",
            "atfm-1W-companion"
        ]
    );
    assert_eq!(
        saved_order(&h, &mut vcx, "vol").unwrap(),
        ["nope", "atfm-3M"],
        "untouched, the record is written back as read"
    );
    h.move_to_column(&mut vcx, "atfm-1W-level");
    h.press(&mut vcx, "shift-l");
    let order = saved_order(&h, &mut vcx, "vol").unwrap();
    assert!(!order.iter().any(|s| s == "nope"), "{order:?}");
    assert_eq!(order.len(), 9, "every unit, once");
    // A board refresh keeps the order.
    let before = ids(&h, &vcx);
    h.press(&mut vcx, "shift-r");
    let tag = h.tag_of(&vcx);
    let asked = h.asked_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    assert_eq!(ids(&h, &vcx), before);
    // Another tab is untouched by vol's order.
    h.press(&mut vcx, "g 2");
    assert_eq!(ids(&h, &vcx)[1], "skew-1W-level");
}

#[gpui::test]
fn r_q_and_m_change_the_lens_without_a_requery_and_w_and_d_requery(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "r");
    assert_eq!(h.lens(&vcx).lens, Lens::Rich);
    h.press(&mut vcx, "q");
    assert_eq!(h.lens(&vcx).metric, Metric::Pctl);
    h.press(&mut vcx, "m");
    assert_eq!(h.lens(&vcx).shown, Shown::Companion);
    assert!(
        Harness::boards(&h.requests()).is_empty(),
        "no requery for a lens, metric or shown change"
    );
    let col = h.column_index(&vcx, "atfm-3M-companion");
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.prepared.rows[0].cells[col].text.to_string()),
        "91",
        "the rank paints from the board already held"
    );
    h.press(&mut vcx, "w");
    assert_eq!(h.lens(&vcx).window, Window::Y2);
    let reqs = h.requests();
    let boards = Harness::boards(&reqs);
    assert_eq!(boards.len(), 1);
    assert_eq!(boards[0].window, Some(Window::Y2.duration()));
    h.press(&mut vcx, "d");
    assert_eq!(h.lens(&vcx).lookback, Lookback::W1);
    assert_eq!(Harness::boards(&h.requests()).len(), 1);
}

#[gpui::test]
fn w_cycles_the_window_requeries_and_supersedes_the_board_in_flight(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "w");
    let first = h.tag_of(&vcx);
    h.press(&mut vcx, "w");
    let second = h.tag_of(&vcx);
    assert!(second > first);
    let asked = h.asked_of(&vcx);
    h.answer_board(&mut vcx, first, board_for(&["DAX"], &asked));
    assert!(
        h.tile.read_with(&vcx, |t, _| t.in_flight),
        "the superseded board is dropped"
    );
    h.answer_board(&mut vcx, second, board_for(&["DAX", "SX5E", "NEW"], &asked));
    assert!(h.tile.read_with(&vcx, |t, _| !t.in_flight));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.board_ask.clone().unwrap().window),
        Some(Window::M6.duration())
    );
}

#[gpui::test]
fn w_mutes_the_old_windows_companions_until_the_board_lands(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "r");
    let col = h.column_index(&vcx, "atfm-3M-companion");
    let muted = |vcx: &gpui::VisualTestContext| {
        h.tile
            .read_with(vcx, |t, _| t.prepared.rows[0].cells[col].muted)
    };
    assert!(!muted(&vcx), "the 1Y board's rank paints under z · 1Y");
    h.press(&mut vcx, "w");
    assert_eq!(h.lens(&vcx).window, Window::Y2);
    assert!(muted(&vcx), "the 1Y board's rank greys under z · 2Y");
    let tag = h.tag_of(&vcx);
    let asked = h.asked_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    assert!(!muted(&vcx), "the 2Y board's rank paints");
}

#[gpui::test]
fn colon_view_switches_by_name_and_refuses_an_unknown_one(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    assert!(h.command(&mut vcx, "view varswap").is_ok());
    assert_eq!(h.view(&vcx), "varswap");
    assert_eq!(
        h.command(&mut vcx, "view nope").unwrap_err(),
        "no view 'nope'"
    );
    assert_eq!(
        h.completions(&vcx, "view ", 5),
        ["skew", "surface", "varswap", "vol"]
    );
}

#[gpui::test]
fn the_session_restores_the_view_name_and_the_lens(cx: &mut gpui::TestAppContext) {
    let mut record = restored(LIST);
    record.insert("view".into(), toml::Value::String("skew".into()));
    record.insert("lens".into(), toml::Value::String("rich".into()));
    record.insert("window".into(), toml::Value::String("6M".into()));
    let (h, mut vcx) = open_with(cx, one_list(), Some(record), VolwatchSettings::default());
    assert_eq!(h.view(&vcx), "skew");
    assert_eq!(h.lens(&vcx).lens, Lens::Rich);
    assert_eq!(h.lens(&vcx).window, Window::M6);
    let reqs = h.requests();
    let boards = Harness::boards(&reqs);
    assert_eq!(
        boards[0].window,
        Some(Window::M6.duration()),
        "the first ask carries the restored window"
    );
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved.get("view").and_then(|v| v.as_str()), Some("skew"));
    assert_eq!(saved.get("lens").and_then(|v| v.as_str()), Some("rich"));
}

#[gpui::test]
fn an_old_view_name_opens_the_default_with_a_notice(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_on(cx, "richcheap");
    assert_eq!(h.view(&vcx), "vol");
    assert_eq!(h.notices(&vcx), ["view richcheap is gone"]);
}

#[gpui::test]
fn d_cycles_the_lookback_requeries_and_mutes_the_old_changes_until_the_board_lands(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "d");
    assert_eq!(h.lens(&vcx).lookback, Lookback::W1);
    assert_eq!(Harness::boards(&h.requests())[0].lookbacks, [Lookback::W1]);
    let col = h.column_index(&vcx, "atfm-3M-companion");
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.prepared.rows[0].cells[col].muted)
    );
    h.press(&mut vcx, "d d");
    assert_eq!(h.lens(&vcx).lookback, Lookback::D1, "wraps");
}

#[gpui::test]
fn p_toggles_the_glyphs_without_asking(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    let col = h.column_index(&vcx, "atfm-3M-companion");
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
        Some("vol 3M ↑".into()),
        "the chip names the direction"
    );
    h.press(&mut vcx, "s");
    assert_eq!(
        h.shown(&vcx),
        ["SX5E", "DAX", "NEW"],
        "desc, the missing member last"
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header.sort.clone()),
        Some("vol 3M ↓".into()),
        "desc reads apart from asc"
    );
    h.press(&mut vcx, "s");
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"], "off: member order");
    assert!(h.tile.read_with(&vcx, |t, _| t.state.sort.is_none()));
    h.command(&mut vcx, "sort atfm-3M-companion abs").unwrap();
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.header.sort.as_deref() == Some("|Δ 3M| ↓"))
    );
    h.command(&mut vcx, "sort").unwrap();
    assert!(h.tile.read_with(&vcx, |t, _| t.header.sort.is_none()));
}

/// `shift+s` walks the absolute cycle on the cursor column, as the
/// blotter's does: |desc| → |asc| → member order, and `s` over it starts
/// the signed cycle afresh.
#[gpui::test]
fn shift_s_cycles_an_absolute_sort_on_the_cursor_column(cx: &mut gpui::TestAppContext) {
    // SX5E moved −2.0 and DAX +1.0: by magnitude SX5E leads, signed DAX does.
    let (h, mut vcx) = landed_board(cx, |board| {
        for row in &mut board.rows {
            let value = match row.underlying.as_str() {
                "SX5E" => -2.0,
                "DAX" => 1.0,
                _ => continue,
            };
            for c in &mut row.cells {
                if c.measure == Measure::atfm("3M")
                    && let Some(Some(change)) = c.changes.first_mut()
                {
                    change.value = value;
                }
            }
        }
    });
    h.move_to_column(&mut vcx, "atfm-3M-companion");
    h.press(&mut vcx, "shift-s");
    assert_eq!(
        h.shown(&vcx),
        ["SX5E", "DAX", "NEW"],
        "|desc|: the larger move first"
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header.sort.clone()),
        Some("|Δ 3M| ↓".into()),
        "the chip names the magnitude and the direction"
    );
    h.press(&mut vcx, "shift-s");
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"], "|asc|");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header.sort.clone()),
        Some("|Δ 3M| ↑".into())
    );
    h.press(&mut vcx, "shift-s");
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E", "NEW"], "off: member order");
    assert!(h.tile.read_with(&vcx, |t, _| t.state.sort.is_none()));
    // `s` over an absolute order starts the signed cycle at asc.
    h.press(&mut vcx, "shift-s");
    h.press(&mut vcx, "s");
    assert_eq!(
        h.shown(&vcx),
        ["SX5E", "DAX", "NEW"],
        "asc: −2.0 before 1.0"
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header.sort.clone()),
        Some("Δ 3M ↑".into())
    );
}

#[gpui::test]
fn a_spark_column_refuses_to_sort_with_a_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.move_to_column(&mut vcx, "spark-atfm-3M");
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
fn a_column_narrower_than_its_number_keeps_its_text_inside_the_cell(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    // Force the level column to 20 px, narrower than DAX's `15.9`.
    h.tile.update(&mut vcx, |t, cx| {
        t.table.update(cx, |tb, cx| {
            let mut sized = tb.delegate().fitted.clone();
            sized.insert(sized_key("vol", "atfm-1M-level"), 20.0);
            tb.delegate_mut().set_fitted(sized);
            tb.refresh(cx);
        });
    });
    h.draw(&mut vcx);
    // The prepared cell id is `{tile}-{name}-{column}`.
    let cell = vcx
        .debug_bounds("volwatch-td-7-DAX-atfm-1M-level")
        .expect("the cell is painted");
    let text = vcx
        .debug_bounds("7-DAX-atfm-1M-level-text")
        .expect("the cell's text is painted");
    assert!(
        text.left() >= cell.left() && text.right() <= cell.right(),
        "a number's leading digits must never be cut: text {text:?} in cell {cell:?}"
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
    // The cursor starts on the first level.
    h.press(&mut vcx, "y");
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()),
        Some("15.9".into())
    );
    h.press(&mut vcx, "l");
    h.press(&mut vcx, "y");
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()),
        Some("+1.1".into()),
        "its companion"
    );
}

#[gpui::test]
fn y_over_a_selection_yanks_the_column_one_row_per_line_and_ends_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
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

#[gpui::test]
fn the_menu_lists_the_verbs_with_their_chords_and_a_pick_dispatches(cx: &mut gpui::TestAppContext) {
    use geode_tile::menu::Trailing;
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, ".");
    assert_eq!(
        h.menu_titles(&vcx),
        [
            "Switch list\u{2026}",
            "vol view",
            "skew view",
            "varswap view",
            "surface view",
            "Toggle moves / rich-cheap",
            "Cycle lookback",
            "Toggle z-score / percentile",
            "Cycle window",
            "Swap level / companion",
            "Paint",
            "Sort by column",
            "Sort by |column|",
            "Toggle group",
            "Expand all groups",
            "Collapse all groups",
            "Inspect row",
            "Open timeseries",
            "Yank cell",
            "Refresh now",
        ]
    );
    let lens_verbs = [
        "Toggle moves / rich-cheap",
        "Toggle z-score / percentile",
        "Cycle window",
    ];
    let row = |vcx: &gpui::VisualTestContext, title: &str| {
        h.tile.read_with(vcx, |t, _| {
            t.menu
                .as_ref()
                .unwrap()
                .menu
                .rows()
                .iter()
                .filter_map(geode_tile::menu::Row::action)
                .find(|a| a.title().as_ref() == title)
                .unwrap_or_else(|| panic!("no row {title}"))
                .clone()
        })
    };
    for title in lens_verbs {
        assert!(
            matches!(row(&vcx, title).trailing(), Trailing::Keys(_)),
            "{title} shows its chord"
        );
    }
    // The vol tab pairs a level with its companion, so there is nothing to
    // swap: the row says so in place of its chord.
    let swap = row(&vcx, "Swap level / companion");
    assert!(!swap.is_enabled());
    assert_eq!(swap.reason().unwrap().as_ref(), "a pair view shows both");
    assert!(matches!(swap.trailing(), Trailing::Text(_)));
    let titles = h.menu_titles(&vcx);
    let at = titles
        .iter()
        .position(|t| t == "Toggle moves / rich-cheap")
        .unwrap();
    for _ in 0..at {
        h.press(&mut vcx, "j");
    }
    h.press(&mut vcx, "enter");
    assert_eq!(h.lens(&vcx).lens, Lens::Rich, "the pick flips the lens");
    assert!(h.tile.read_with(&vcx, |t, _| t.menu.is_none()));
    h.press(&mut vcx, ".");
    let titles = h.menu_titles(&vcx);
    let at = titles.iter().position(|t| t == "surface view").unwrap();
    for _ in 0..at {
        h.press(&mut vcx, "j");
    }
    h.press(&mut vcx, "enter");
    assert_eq!(h.view(&vcx), "surface");
    assert!(h.tile.read_with(&vcx, |t, _| t.menu.is_none()));
    // A single view swaps, and the row shows its chord.
    h.press(&mut vcx, ".");
    let swap = row(&vcx, "Swap level / companion");
    assert!(swap.is_enabled());
    assert!(matches!(swap.trailing(), Trailing::Keys(_)));
    h.press(&mut vcx, "escape");
}

#[gpui::test]
fn the_menu_rows_carry_their_chords_ticks_and_reasons(cx: &mut gpui::TestAppContext) {
    use geode_tile::menu::Trailing;
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "l");
    h.press(&mut vcx, ".");
    h.tile.read_with(&vcx, |t, _| {
        let m = t.menu.as_ref().unwrap();
        let row = |title: &str| {
            m.menu
                .rows()
                .iter()
                .filter_map(geode_tile::menu::Row::action)
                .find(|a| a.title().as_ref() == title)
                .unwrap_or_else(|| panic!("no row {title}"))
                .clone()
        };
        assert_eq!(row("vol view").tick(), Some(true));
        assert_eq!(row("surface view").tick(), Some(false));
        assert_eq!(row("Paint").tick(), Some(t.paint));
        assert!(row("Sort by column").is_enabled(), "a numeric column sorts");
        assert!(matches!(row("Yank cell").trailing(), Trailing::Keys(_)));
        assert!(matches!(
            row("Switch list\u{2026}").trailing(),
            Trailing::Keys(_)
        ));
    });
    // On the sparkline, the sort row says why it is off.
    h.press(&mut vcx, "escape");
    h.move_to_column(&mut vcx, "spark-atfm-3M");
    h.press(&mut vcx, ".");
    h.tile.read_with(&vcx, |t, _| {
        let sort = t
            .menu
            .as_ref()
            .unwrap()
            .menu
            .rows()
            .iter()
            .filter_map(geode_tile::menu::Row::action)
            .find(|a| a.title().as_ref() == "Sort by column")
            .unwrap()
            .clone();
        assert!(!sort.is_enabled());
        assert!(sort.reason().unwrap().contains("is not sortable"));
    });
    // A pointer press on the disabled row gives its reason as a notice
    // (the keys step over it).
    let index = h.tile.read_with(&vcx, |t, _| {
        t.menu
            .as_ref()
            .unwrap()
            .menu
            .rows()
            .iter()
            .position(|r| {
                r.action()
                    .is_some_and(|a| a.title().as_ref() == "Sort by column")
            })
            .unwrap()
    });
    let tile = h.tile.clone();
    vcx.update(|window, cx| {
        use geode_tile::menu::MenuHost as _;
        tile.update(cx, |t, cx| t.menu_pick(index, window, cx))
    });
    assert!(
        h.notices(&vcx)
            .iter()
            .any(|n| n.contains("is not sortable"))
    );
    assert!(h.tile.read_with(&vcx, |t, _| t.menu.is_none()));
}

#[gpui::test]
fn the_header_trigger_opens_the_same_menu_and_shows_it_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.draw(&mut vcx);
    let button = vcx
        .debug_bounds("volwatch-menu-button")
        .expect("the trigger");
    vcx.simulate_click(button.center(), gpui::Modifiers::none());
    assert_eq!(h.menu_titles(&vcx)[0], "Switch list\u{2026}");
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("volwatch-actions").is_some());
    let menu = vcx.debug_bounds("volwatch-actions").unwrap();
    assert!(menu.origin.y >= button.origin.y, "hung under the header");
    assert!(h.tile.read_with(&vcx, |t, _| t.menu_at.is_none()));
}

#[gpui::test]
fn a_right_press_moves_the_cursor_and_opens_the_menu_at_the_pointer(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.draw(&mut vcx);
    let row = vcx.debug_bounds("volwatch-row-SX5E").unwrap();
    vcx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::none(),
    );
    assert_eq!(h.cursor(&vcx), Some("SX5E".into()));
    assert!(h.tile.read_with(&vcx, |t, _| t.is_actions()));
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.menu_at), Some(row.center()));
    h.draw(&mut vcx);
    let menu = vcx.debug_bounds("volwatch-actions").expect("painted");
    let at = row.center();
    assert!(
        (menu.origin.x - at.x).abs() <= gpui::px(1.0)
            && (menu.origin.y - at.y).abs() <= gpui::px(1.0),
        "{:?} is at the pointer {at:?}",
        menu.origin
    );
    h.press(&mut vcx, "escape");
    assert!(
        h.tile
            .read_with(&vcx, |t, _| t.menu.is_none() && t.menu_at.is_none())
    );
    release_the_table_menu(&mut vcx, row.center());
}

/// gpui-component's table builds an empty context menu on every right
/// press, held in a cycle only the next right press breaks, so a test
/// ending after a right press leaks it. One more right press, whose
/// deferred rebuild never runs because the window closes in the same
/// update, breaks it (the watchlist's tests have the same helper).
fn release_the_table_menu(vcx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    vcx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                button: gpui::MouseButton::Right,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.remove_window();
    });
    vcx.run_until_parked();
}

#[gpui::test]
fn t_is_refused_with_a_notice_naming_the_gap(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "j");
    h.move_to_column(&mut vcx, "atfm-3M-level");
    h.press(&mut vcx, "t");
    assert_eq!(
        h.notices(&vcx),
        ["timeseries cannot open a history series yet: vol_surface [SX5E, 3M, 0.5] vol"]
    );
    // A column with no series says so.
    h.move_to_column(&mut vcx, "spark-atfm-3M");
    h.press(&mut vcx, "t");
    let n = h.notices(&vcx);
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].ends_with("has no series"), "{n:?}");
}

#[gpui::test]
fn escape_peels_menu_band_selection_narrowing_then_notices(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.find(&mut vcx, FindEvent::Committed("S".into()));
    let shown = h.shown(&vcx).len();
    h.press(&mut vcx, "enter");
    assert!(h.band_open(&vcx));
    h.move_to_column(&mut vcx, "atfm-3M-level");
    h.press(&mut vcx, "t");
    assert_eq!(h.notices(&vcx).len(), 1);
    // The pointer routes touch no verb, so the notice stands: a shift
    // press starts a selection, the header's trigger opens the menu.
    h.draw(&mut vcx);
    let row = vcx.debug_bounds("volwatch-row-SX5E").unwrap();
    vcx.simulate_click(row.center(), gpui::Modifiers::shift());
    assert!(h.tile.read_with(&vcx, |t, _| t.grid.selecting()));
    h.draw(&mut vcx);
    let button = vcx.debug_bounds("volwatch-menu-button").unwrap();
    vcx.simulate_click(button.center(), gpui::Modifiers::none());
    assert!(h.tile.read_with(&vcx, |t, _| t.is_actions()));
    h.press(&mut vcx, "escape");
    assert!(h.tile.read_with(&vcx, |t, _| t.menu.is_none()));
    assert!(h.band_open(&vcx), "the band outlives the menu");
    h.press(&mut vcx, "escape");
    assert!(!h.band_open(&vcx));
    assert!(h.tile.read_with(&vcx, |t, _| t.grid.selecting()));
    h.press(&mut vcx, "escape");
    assert!(!h.tile.read_with(&vcx, |t, _| t.grid.selecting()));
    assert_eq!(h.shown(&vcx).len(), shown);
    h.press(&mut vcx, "escape");
    assert!(h.shown(&vcx).len() > shown, "narrowing cleared");
    assert_eq!(h.notices(&vcx).len(), 1);
    h.press(&mut vcx, "escape");
    assert!(h.notices(&vcx).is_empty());
}

#[gpui::test]
fn a_verbs_notice_lasts_until_the_next_verb(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "j");
    h.move_to_column(&mut vcx, "atfm-3M-level");
    h.press(&mut vcx, "t");
    assert_eq!(h.notices(&vcx).len(), 1);
    h.press(&mut vcx, "p");
    assert!(h.notices(&vcx).is_empty());
}

#[gpui::test]
fn autosize_refuses_before_a_board_lands(cx: &mut gpui::TestAppContext) {
    use geode_shell::colfit::NOTHING_TO_FIT;
    let (h, mut vcx) = open_on(cx, "vol");
    let tile = h.content.clone();
    assert_eq!(
        vcx.update(|window, cx| tile.autosize_columns(false, window, cx)),
        Err(NOTHING_TO_FIT)
    );
    // A reset never refuses.
    assert_eq!(
        vcx.update(|window, cx| tile.autosize_columns(true, window, cx)),
        Ok(())
    );
}

/// The delegate's sized widths, keyed `view:id`.
fn widths(h: &Harness, vcx: &gpui::VisualTestContext) -> geode_shell::colfit::FittedWidths {
    h.tile
        .read_with(vcx, |t, cx| t.table.read(cx).delegate().fitted.clone())
}

#[gpui::test]
fn autosize_fits_resets_and_persists(cx: &mut gpui::TestAppContext) {
    use geode_shell::colfit::SESSION_KEY;
    let (h, mut vcx) = landed(cx);
    let tile = h.content.clone();
    assert!(widths(&h, &vcx).is_empty());
    assert_eq!(
        vcx.update(|window, cx| tile.autosize_columns(false, window, cx)),
        Ok(())
    );
    let fitted = widths(&h, &vcx);
    assert_eq!(fitted.len(), h.tile.read_with(&vcx, |t, _| t.columns.len()));
    // Persisted in the session record.
    let saved = vcx.update(|_, cx| tile.serialize(cx));
    assert!(saved.contains_key(SESSION_KEY));
    // A reset returns to the prepared widths and drops the key.
    assert_eq!(
        vcx.update(|window, cx| tile.autosize_columns(true, window, cx)),
        Ok(())
    );
    assert!(widths(&h, &vcx).is_empty());
    assert!(
        !vcx.update(|_, cx| tile.serialize(cx))
            .contains_key(SESSION_KEY)
    );
}

#[gpui::test]
fn fitted_widths_restore_from_the_session_record(cx: &mut gpui::TestAppContext) {
    use geode_shell::colfit::SESSION_KEY;
    let mut record = restored(LIST);
    let mut widths = toml::Table::new();
    widths.insert(sized_key("vol", "atfm-3M-level"), toml::Value::Float(123.0));
    record.insert(SESSION_KEY.into(), toml::Value::Table(widths));
    let (h, vcx) = open_with(cx, one_list(), Some(record), VolwatchSettings::default());
    let got = h.tile.read_with(&vcx, |t, cx| {
        t.table
            .read(cx)
            .delegate()
            .fitted
            .get(&sized_key("vol", "atfm-3M-level"))
            .copied()
    });
    assert_eq!(got, Some(123.0));
}

#[gpui::test]
fn autosize_sizes_only_the_current_view_and_a_view_switch_keeps_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    let tile = h.content.clone();
    assert_eq!(
        vcx.update(|window, cx| tile.autosize_columns(false, window, cx)),
        Ok(())
    );
    let sized = widths(&h, &vcx);
    assert!(sized.keys().all(|k| k.starts_with("vol:")), "{sized:?}");
    let w = sized[&sized_key("vol", "underlying")];
    h.press(&mut vcx, "g 2");
    assert_eq!(h.view(&vcx), "skew");
    // skew's underlying is unfitted: the prepared width, not vol's.
    assert_ne!(h.column_width(&vcx, "underlying"), w);
    h.press(&mut vcx, "g 1");
    assert_eq!(
        h.column_width(&vcx, "underlying"),
        w,
        "the fit survives a round trip"
    );
    // Reset clears only this view.
    h.press(&mut vcx, "g 2");
    assert_eq!(
        vcx.update(|window, cx| tile.autosize_columns(false, window, cx)),
        Ok(())
    );
    assert!(widths(&h, &vcx).keys().any(|k| k.starts_with("skew:")));
    assert_eq!(
        vcx.update(|window, cx| tile.autosize_columns(true, window, cx)),
        Ok(())
    );
    assert!(widths(&h, &vcx).keys().all(|k| k.starts_with("vol:")));
}

#[gpui::test]
fn a_header_drag_records_the_width_for_this_view_and_persists(cx: &mut gpui::TestAppContext) {
    use geode_shell::colfit::SESSION_KEY;
    let (h, mut vcx) = landed(cx);
    let col = h.column_index(&vcx, "atfm-1M-level");
    // The production route: the table's own event, as a drag emits it.
    let mut all: Vec<gpui::Pixels> = h.tile.read_with(&vcx, |t, cx| {
        (0..t.columns.len())
            .map(|i| t.table.read(cx).delegate().width(i))
            .collect()
    });
    all[col] = gpui::px(97.0);
    h.tile.update(&mut vcx, |t, cx| {
        t.table.update(cx, |_, cx| {
            cx.emit(TableEvent::ColumnWidthsChanged(all.clone()));
        });
    });
    vcx.run_until_parked();
    assert_eq!(h.column_width(&vcx, "atfm-1M-level"), 97.0);
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    let t = saved[SESSION_KEY].as_table().unwrap();
    assert_eq!(t[&sized_key("vol", "atfm-1M-level")].as_float(), Some(97.0));
    assert_eq!(t.len(), 1, "an undragged column is not recorded");
}

#[gpui::test]
fn a_part_4_record_without_view_prefixes_is_ignored(cx: &mut gpui::TestAppContext) {
    use geode_shell::colfit::SESSION_KEY;
    let mut record = restored(LIST);
    let mut widths = toml::Table::new();
    widths.insert("atfm-3M-level".into(), toml::Value::Float(30.0));
    record.insert(SESSION_KEY.into(), toml::Value::Table(widths));
    let (h, vcx) = open_with(cx, one_list(), Some(record), VolwatchSettings::default());
    assert!(
        h.tile
            .read_with(&vcx, |t, cx| t.table.read(cx).delegate().fitted.is_empty())
    );
}

#[gpui::test]
fn a_single_view_paints_the_level_over_a_tint_and_m_swaps_the_number(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "g 4");
    let asked = h.asked_of(&vcx);
    let tag = h.tag_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    h.draw(&mut vcx);
    let id = h.tile.read_with(&vcx, |t, _| {
        let col = t
            .columns
            .iter()
            .position(|c| c.id == "atfm-3M-single")
            .expect("the 0.5 point is the ATFM column");
        t.prepared.rows[0].cells[col].id.to_string()
    });
    assert!(vcx.debug_bounds(format!("{id}-tint").leak()).is_some());
    let text = vcx.debug_bounds(format!("{id}-text").leak()).unwrap();
    let tint = vcx.debug_bounds(format!("{id}-tint").leak()).unwrap();
    // Edge to edge: the text may end flush with the tint's right edge,
    // which `Bounds::contains` (half-open) would call outside.
    let (t0, t1, x0, x1) = (
        tint.origin,
        tint.bottom_right(),
        text.origin,
        text.bottom_right(),
    );
    assert!(
        x0.x >= t0.x && x0.y >= t0.y && x1.x <= t1.x && x1.y <= t1.y,
        "the text sits inside the tint: {tint:?} vs {text:?}"
    );
    h.press(&mut vcx, "m");
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds(format!("{id}-chip").leak()).is_some(),
        "a companion chip after m"
    );
    assert!(vcx.debug_bounds(format!("{id}-tint").leak()).is_none());
}

/// A tint is a per-row condition (a missing companion, a stale lookback),
/// so a tinted and an untinted level in one column must share the inset:
/// otherwise row A's number sits a few px left of row B's.
#[gpui::test]
fn a_single_column_keeps_one_right_edge_with_and_without_a_tint(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "g 4");
    let asked = h.asked_of(&vcx);
    let tag = h.tag_of(&vcx);
    let mut b = board_for(&["DAX", "SX5E", "NEW"], &asked);
    for c in &mut b.rows[0].cells {
        if c.measure == Measure::atfm("3M") {
            c.changes = vec![None];
        }
    }
    h.answer_board(&mut vcx, tag, b);
    h.draw(&mut vcx);
    let (dax, sx5e) = h.tile.read_with(&vcx, |t, _| {
        let col = t
            .columns
            .iter()
            .position(|c| c.id == "atfm-3M-single")
            .expect("the 0.5 point is the ATFM column");
        (
            t.prepared.rows[0].cells[col].id.to_string(),
            t.prepared.rows[1].cells[col].id.to_string(),
        )
    });
    assert!(
        vcx.debug_bounds(format!("{dax}-tint").leak()).is_none(),
        "no change, no tint"
    );
    assert!(vcx.debug_bounds(format!("{sx5e}-tint").leak()).is_some());
    let bare = vcx.debug_bounds(format!("{dax}-text").leak()).unwrap();
    let tinted = vcx.debug_bounds(format!("{sx5e}-text").leak()).unwrap();
    assert_eq!(
        bare.bottom_right().x,
        tinted.bottom_right().x,
        "one right edge: {bare:?} vs {tinted:?}"
    );
}

/// `e` is a shell action in the tile's own fragment: the stand-in resolves
/// and records `config::vol_views` with no tile code, and the tile names the
/// tab the editor should open on.
#[gpui::test]
fn e_opens_the_vol_views_dialog_through_the_shells_action(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.press(&mut vcx, "e");
    assert_eq!(
        h.dispatched(&vcx).last().map(String::as_str),
        Some("config::vol_views")
    );
    let object = vcx
        .update(|_, cx| h.content.config_object(cx))
        .expect("a vol tile names the tab it shows");
    assert_eq!((object.doc, object.name.as_str()), ("vol_views", "vol"));
}
