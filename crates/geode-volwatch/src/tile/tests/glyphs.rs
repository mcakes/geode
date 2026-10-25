//! The per-column cell glyphs a `vol_views.toml` names (`level = "heat"`,
//! `companion = "bar" | "slider"`) painted through the real delegate, and
//! the empty tab a view with no columns paints.

use geode_widgets::cell::widths;

use super::harness::*;

// Selectors spell the tile id `7` ([`TILE`]) literally: `debug_bounds`
// takes a static string.

/// `vol`: ATFM 3M with a heat level and a bar companion, 25Δ skew 1Y
/// with a slider companion.
const GLYPH_VIEWS: &str = r#"config_version = 1
default = "vol"

[vol]
[[vol.columns]]
measure = "atfm 3M"
level = "heat"
companion = "bar"
[[vol.columns]]
measure = "skew 1Y"
companion = "slider"
"#;

/// `vol` with one column, and `blank` with none.
const EMPTY_VIEW: &str = r#"config_version = 1
default = "vol"

[vol]
[[vol.columns]]
measure = "atfm 3M"

[blank]
columns = []
"#;

/// A user view with a heat level, a bar companion and a slider companion
/// paints each glyph through the real delegate, with its text, and the
/// widths grew to hold them.
#[gpui::test]
fn heat_bar_and_slider_paint_through_the_grid(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored(LIST)),
        settings_with_views(GLYPH_VIEWS),
    );
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.asked_of(&vcx);
    let mut board = board_for(&["DAX", "SX5E", "NEW"], &asked);
    // DAX above SX5E on every measure: the column has a range to heat.
    for c in &mut board.rows[0].cells {
        c.current = Some(20.0);
    }
    h.answer_board(&mut vcx, tag, board);
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("7-DAX-atfm-3M-level-text").is_some());
    assert!(
        vcx.debug_bounds("7-DAX-atfm-3M-level-heat").is_some(),
        "the column's top is tinted"
    );
    assert!(
        vcx.debug_bounds("7-SX5E-atfm-3M-level-heat").is_none(),
        "the column's bottom is not"
    );
    assert!(vcx.debug_bounds("7-DAX-atfm-3M-companion-text").is_some());
    assert!(
        vcx.debug_bounds("7-DAX-atfm-3M-companion-bar").is_some(),
        "a move of +1.1 draws a bar"
    );
    assert!(
        h.column_width(&vcx, "atfm-3M-companion") >= widths::bar(4.0 * 7.0),
        "the companion column holds a bar beside `+1.1`"
    );
    // Under moves the slider column shows a chip; after r q (rich/cheap,
    // pctl) the rank gauge, its text still painted.
    assert!(vcx.debug_bounds("7-DAX-skew-1Y-companion-track").is_none());
    h.press(&mut vcx, "r q");
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("7-DAX-skew-1Y-companion-text").is_some());
    assert!(
        vcx.debug_bounds("7-DAX-skew-1Y-companion-track").is_some(),
        "the slider's track under the percentile metric"
    );
    assert!(
        h.column_width(&vcx, "skew-1Y-companion") >= widths::slider(2.0 * 7.0),
        "the companion column holds a slider beside `91`"
    );
}

/// An empty tab (a view with no columns) paints the strip and an empty
/// state, no grid, and asks the service for nothing.
#[gpui::test]
fn an_empty_tab_paints_its_strip_and_no_grid(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        one_list(),
        Some(restored(LIST)),
        settings_with_views(EMPTY_VIEW),
    );
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.asked_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    h.requests();
    h.press(&mut vcx, "g 2");
    h.draw(&mut vcx);
    assert_eq!(h.view(&vcx), "blank");
    assert!(vcx.debug_bounds("volwatch-strip-7").is_some());
    assert!(vcx.debug_bounds("7-DAX-underlying-text").is_none());
    assert!(h.notices(&vcx).is_empty(), "{:?}", h.notices(&vcx));
    assert!(
        h.requests().is_empty(),
        "a tab with no measure asks nothing of the board"
    );
    // The hint names the key the fragment binds to `config::vol_views`;
    // the palette label is its fallback for a keymap that unbinds it.
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.empty.clone()).as_deref(),
        Some("blank has no columns \u{2014} e adds one")
    );
    // shift+r on the empty tab asks nothing either, and says nothing.
    h.press(&mut vcx, "shift-r");
    assert!(h.requests().is_empty());
    assert!(h.notices(&vcx).is_empty());
    // Back on `vol` the grid returns.
    h.press(&mut vcx, "g 1");
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("7-DAX-underlying-text").is_some());
    assert!(h.tile.read_with(&vcx, |t, _| t.empty.is_none()));
}
