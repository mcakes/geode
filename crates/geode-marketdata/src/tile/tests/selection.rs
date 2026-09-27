//! The panel's `V`/`v` grid selection through the production doors:
//! `dispatch`, `:` commands and deliveries.

use super::*;
use geode_core::grid::selection::SelectKind;

type Shape = (SelectKind, std::ops::Range<usize>, std::ops::Range<usize>);

fn resolved(h: &Harness, vcx: &gpui::VisualTestContext) -> Option<Shape> {
    h.tile.read_with(vcx, |t, _| {
        t.resolved()
            .map(|r| (r.kind, r.rows.clone(), r.cols.clone()))
    })
}

/// Motions extend a live block and clamp at the grid's edges: `j` past
/// the last row stops there, `k` past row 0 never enters the strip.
#[gpui::test]
fn v_starts_a_block_and_motions_extend_it_clamped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    assert_eq!(h.mode(&vcx), "visual");
    h.dispatch(&mut vcx, "right", None);
    h.dispatch(&mut vcx, "down", Some(5)); // clamps at the last row, no wrap
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Block, 0..2, 3..5)));
    h.dispatch(&mut vcx, "up", Some(9)); // clamps at row 0, never the strip
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.cursor()),
        Cursor::Cell { row: 0, col: 4 }
    );
}

/// The other key switches kind keeping the anchor; the same key again
/// clears and returns to normal mode.
#[gpui::test]
fn shift_v_switches_kind_keeping_the_anchor_and_the_same_key_clears(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(
        resolved(&h, &vcx),
        Some((SelectKind::Rows, 0..2, 0..6)),
        "every column, anchor kept"
    );
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
}

/// With a selection live, `escape` clears only it; the header's notice
/// waits for the second `escape`.
#[gpui::test]
fn escape_clears_only_the_selection_first(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "step", None); // fwd is not a choice cell: leaves a notice
    let notice = || "not a choice cell".to_string();
    assert!(h.header_texts(&vcx).contains(&notice()));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "escape", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert!(
        h.header_texts(&vcx).contains(&notice()),
        "the first escape cleared only the selection"
    );
    h.dispatch(&mut vcx, "escape", None);
    assert!(
        !h.header_texts(&vcx).contains(&notice()),
        "the second does today's escape"
    );
}

/// The attribute strip is never a selection member, so `v` there starts
/// nothing and says why.
#[gpui::test]
fn v_in_the_attribute_strip_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "up", None); // row 0 → the strip
    h.dispatch(&mut vcx, "visual_block", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert!(
        h.header_texts(&vcx)
            .iter()
            .any(|t| t == "select from a grid cell")
    );
}

/// The next underlying's terms can carry the same labels; a selection
/// must never carry over to another document.
#[gpui::test]
fn a_key_switch_clears_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.command(&mut vcx, "key SPX.Y").unwrap();
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
    // Cleared by the switch itself, not lost to the emptied model: a lost
    // anchor would say so in the header.
    assert!(
        !h.header_texts(&vcx)
            .iter()
            .any(|t| t.starts_with("selection cleared")),
        "{:?}",
        h.header_texts(&vcx)
    );
    // The next underlying's document carries the same term labels, and
    // still nothing is selected on it.
    let tag = h.document_request().expect("the new key's request").tag;
    h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
}

/// An anchor row a new generation no longer paints clears the selection
/// with a notice rather than guessing a neighbour.
#[gpui::test]
fn an_anchor_row_that_disappears_clears_the_selection_with_a_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let tag = h.with_document_tagged(&mut vcx);
    h.dispatch(&mut vcx, "down", None); // anchor on row 1
    h.dispatch(&mut vcx, "visual_rows", None);
    // A newer one-term generation: the anchor's term (TERMS[1]) is gone. The
    // draft is clean, so it simply paints.
    h.deliver(
        &mut vcx,
        tag,
        Arc::new(document_of(&TERMS[..1], &NODES, NEWER)),
    );
    assert_eq!(resolved(&h, &vcx), None);
    assert!(
        h.header_texts(&vcx)
            .iter()
            .any(|t| t == "selection cleared: anchor row no longer shown")
    );
}

/// The delegate paints from the mirrored `Resolved`, and the footer strip
/// shows the prepared extent while the selection is live.
#[gpui::test]
fn the_block_tints_its_cells_and_the_footer_shows_the_extent(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", None);
    let selected = h
        .tile
        .read_with(&vcx, |t, cx| t.table.read(cx).delegate().selected.clone());
    assert_eq!(selected.map(|r| (r.rows, r.cols)), Some((0..2, 3..5)));
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("aggregate-extent").is_some(),
        "the footer strip is painted"
    );
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.selection_extent.clone())
            .as_deref(),
        Some("2 rows × 2 cols")
    );
    // Escape takes both away: the delegate's tint and the footer.
    h.dispatch(&mut vcx, "escape", None);
    let selected = h
        .tile
        .read_with(&vcx, |t, cx| t.table.read(cx).delegate().selected.clone());
    assert_eq!(selected, None, "the delegate paints no tint");
    draw(&mut vcx);
    assert!(
        vcx.debug_bounds("aggregate-extent").is_none(),
        "the footer strip is gone"
    );
}

/// A click on a header attribute leaves the grid, and the strip is never
/// a member: the selection ends there, without the lost-anchor notice (the
/// anchor is still painted).
#[gpui::test]
fn an_attribute_click_clears_the_selection_without_a_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    let at = centre_of(&mut vcx, &format!("marketdata-attr-{TILE}-1"));
    click_at(&mut vcx, at, 1);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(1));
    assert_eq!(h.mode(&vcx), "normal");
    let texts = h.header_texts(&vcx);
    assert!(
        !texts.iter().any(|t| t.starts_with("selection cleared")),
        "{texts:?}"
    );
}
