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

/// `y` over a `Rows` selection copies a header line then each row as
/// `y y` would, and consumes the selection.
#[gpui::test]
fn y_over_rows_copies_a_header_and_every_painted_column_then_ends_the_selection(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "yank", None);
    let text = clipboard(&mut vcx).expect("copied");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "a header and two rows");
    let (axis, columns, labels) = h.tile.read_with(&vcx, |t, _| {
        let m = t.model();
        (
            t.spec.rows.column,
            m.columns.join("\t"),
            [m.rows[0].label.to_string(), m.rows[1].label.to_string()],
        )
    });
    assert_eq!(columns.split('\t').count(), 6, "every painted column");
    assert_eq!(lines[0], format!("{axis}\t{columns}"));
    assert_eq!(
        lines[1],
        format!(
            "{}\t4500.00\t0.1800\t-1.0000\t0.1000\t0.2000\t0.3000",
            labels[0]
        )
    );
    assert_eq!(
        lines[2],
        format!(
            "{}\t4510.00\t0.1900\t-1.1000\t0.4000\t0.5000\t0.6000",
            labels[1]
        )
    );
    assert_eq!(h.mode(&vcx), "normal", "y consumes the selection");
}

/// `y` over a `Block` copies only its own columns' header and cells.
#[gpui::test]
fn y_over_a_block_copies_its_columns_header_and_cells_only(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32 + 1));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", None);
    h.dispatch(&mut vcx, "yank", None);
    let columns = h
        .tile
        .read_with(&vcx, |t, _| t.model().columns[4..6].join("\t"));
    assert_eq!(
        clipboard(&mut vcx).as_deref(),
        Some(format!("{columns}\n0.2000\t0.3000\n0.5000\t0.6000").as_str())
    );
}

/// `d` over a `Rows` selection deletes every selected row as one draft
/// change (a single `:revert` restores them all) and ends the selection.
#[gpui::test]
fn d_over_rows_deletes_them_in_one_draft_change_and_ends_the_selection(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "delete_row", None);
    let states = h.tile.read_with(&vcx, |t, _| {
        t.model().rows.iter().map(|r| r.state).collect::<Vec<_>>()
    });
    assert_eq!(states, vec![RowState::Deleted, RowState::Deleted]);
    assert_eq!(h.mode(&vcx), "normal");
    h.command(&mut vcx, "revert").unwrap();
    let states = h.tile.read_with(&vcx, |t, _| {
        t.model().rows.iter().map(|r| r.state).collect::<Vec<_>>()
    });
    assert_eq!(
        states,
        vec![RowState::Document, RowState::Document],
        "one revert restores both"
    );
}

/// `d` over a `Block` refuses rather than deleting the rows it happens
/// to touch, names `V`, and keeps the selection.
#[gpui::test]
fn d_over_a_block_refuses_and_names_v(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "delete_row", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    assert!(
        h.header_texts(&vcx)
            .iter()
            .any(|t| t == "d deletes rows — use V")
    );
    assert_eq!(h.mode(&vcx), "visual", "a refusal keeps the selection");
}

/// Closing a provisional row-label editor drops its row and shifts every
/// later index, so a bulk delete reads its labels through a range
/// resolved after the drop. Here the provisional row sits above the
/// anchor: the pre-drop range `0..2` would read the anchor AND the row
/// below it.
#[gpui::test]
fn d_over_rows_after_closing_a_label_editor_deletes_only_the_selected_row(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "insert_above", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
    assert_eq!(
        resolved(&h, &vcx).map(|s| s.1),
        Some(0..2),
        "provisional + anchor"
    );
    h.dispatch(&mut vcx, "delete_row", None);
    let states = h.tile.read_with(&vcx, |t, _| {
        t.model()
            .rows
            .iter()
            .map(|r| (r.label.to_string(), r.state))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        states,
        vec![
            (TERMS[0].to_string(), RowState::Deleted),
            (TERMS[1].to_string(), RowState::Document),
        ]
    );
}

/// A selection anchored on a provisional row loses its anchor when the
/// delete closes that row's label editor. The verb refuses with the
/// lost-anchor notice rather than claiming the rows were already deleted,
/// and deletes nothing.
#[gpui::test]
fn d_over_rows_anchored_on_a_dropped_provisional_row_refuses_with_the_lost_anchor(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "insert_below", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(resolved(&h, &vcx).map(|s| s.1), Some(1..2));
    h.dispatch(&mut vcx, "delete_row", None);
    assert_eq!(
        notice_of(&h, &vcx).as_deref(),
        Some("selection cleared: anchor row no longer shown")
    );
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    assert_eq!(resolved(&h, &vcx), None);
}
