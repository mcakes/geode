//! `V`/`v` selections: kind switching, escape, a lost anchor, the
//! footer's extent and position totals, and a sheet replace.

use super::*;
use geode_core::grid::selection::SelectKind;

type Shape = (SelectKind, std::ops::Range<usize>, std::ops::Range<usize>);

fn resolved(h: &Harness, vcx: &VisualTestContext) -> Option<Shape> {
    h.tile.read_with(vcx, |t, _| {
        t.resolved()
            .map(|r| (r.kind, r.rows.clone(), r.cols.clone()))
    })
}

#[gpui::test]
fn v_starts_a_block_and_motions_extend_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    let strike = h.cursor(&vcx).unwrap().1;
    h.dispatch(&mut vcx, "visual_block", None);
    assert_eq!(h.mode(&mut vcx), "visual");
    h.dispatch(&mut vcx, "down", Some(9)); // clamps
    h.dispatch(&mut vcx, "right", None);
    assert_eq!(
        resolved(&h, &vcx),
        Some((SelectKind::Block, 0..3, strike..strike + 2))
    );
}

#[gpui::test]
fn shift_v_switches_kind_keeping_the_anchor_and_the_same_key_clears(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    let cols = h.columns(&vcx).len();
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Rows, 0..2, 0..cols)));
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&mut vcx), "normal");
}

#[gpui::test]
fn escape_clears_only_the_selection_first(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "escape", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&mut vcx), "normal");
}

#[gpui::test]
fn collapsing_the_anchors_package_clears_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "expand", None); // open the CS package
    h.dispatch(&mut vcx, "down", None); // its first leg
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "up", None);
    // collapse through the palette-reachable verb: the leg anchor is hidden
    h.dispatch(&mut vcx, "collapse", None);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(
        h.footer(&vcx).as_deref(),
        Some("selection cleared: anchor row no longer shown")
    );
}

#[gpui::test]
fn the_footer_totals_position_risk_over_top_most_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    answer_all(&h, &mut vcx, 1.0);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    let totals = h.tile.read_with(&vcx, |t, _| t.totals.clone());
    let price = totals
        .iter()
        .find(|c| c.label.as_ref() == "price")
        .expect("a price total");
    // Two 1-lot lines at 1.00, plus the package's own folded sum (read it off its cell).
    let package: f64 = h.cell(&vcx, 1, "price").parse().unwrap();
    let expected = 1.0 + 1.0 + package;
    assert_eq!(price.text.as_ref(), format!("{expected:.2}"));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.selection_extent.clone())
            .as_deref(),
        Some(format!("3 rows × {} cols", h.columns(&vcx).len()).as_str())
    );
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("aggregate-extent").is_some(),
        "the strip paints in the footer"
    );
}

#[gpui::test]
fn an_unpriced_row_turns_its_total_into_a_dash(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK); // no answers: nothing priced
    h.dispatch(&mut vcx, "visual_rows", None);
    let totals = h.tile.read_with(&vcx, |t, _| t.totals.clone());
    assert!(
        !totals.is_empty(),
        "the vanilla view shows every risk column"
    );
    assert!(totals.iter().all(|c| c.text.as_ref() == "—" && c.refused));
}

#[gpui::test]
fn a_new_sheet_clears_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.command(&mut vcx, "new").unwrap();
    assert_eq!(resolved(&h, &vcx), None);
}
