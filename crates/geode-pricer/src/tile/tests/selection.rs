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

#[gpui::test]
fn the_totals_follow_a_delivery_not_only_a_cursor_move(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    let price = |h: &Harness, vcx: &VisualTestContext| {
        h.tile.read_with(vcx, |t, _| {
            t.totals
                .iter()
                .find(|c| c.label.as_ref() == "price")
                .map(|c| c.text.to_string())
        })
    };
    assert_eq!(price(&h, &vcx).as_deref(), Some("—"), "nothing priced yet");
    answer_all(&h, &mut vcx, 1.0);
    let total = price(&h, &vcx).expect("a price total");
    assert!(
        total.parse::<f64>().is_ok(),
        "the delivery re-totals the live selection, got {total:?}"
    );
}

#[gpui::test]
fn a_footer_refusal_takes_the_footer_from_the_selection_strip(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("aggregate-extent").is_some());
    h.dispatch(&mut vcx, "put_below", None); // nothing yanked: refused
    h.draw(&mut vcx);
    assert_eq!(
        h.mode(&mut vcx),
        "visual",
        "the refusal keeps the selection"
    );
    assert_eq!(h.footer(&vcx).as_deref(), Some("nothing to put"));
    assert!(
        vcx.debug_bounds("aggregate-extent").is_none(),
        "the refusal yields no room to the strip"
    );
    assert!(vcx.debug_bounds("pricer-footer").is_some());
}

#[gpui::test]
fn y_over_rows_copies_top_most_shorthand_and_p_puts_them_all(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "yank", None);
    let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(
        clip.as_deref(),
        Some("SPX Z26 5000 C\n-5 SPX Z26 4800/5200 CS")
    );
    assert_eq!(h.mode(&mut vcx), "normal", "y ends the selection");
    h.dispatch(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "put_below", None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()),
        5,
        "both rows landed"
    );
    // The landed package opens (its two legs show); the cursor is on the
    // first landed row.
    assert_eq!(h.tree(&vcx).len(), 7);
    assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(3));
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()),
        3,
        "one undo takes both back"
    );
}

#[gpui::test]
fn y_over_a_block_copies_its_columns_as_tsv_and_keeps_the_register(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "yank", None);
    let clip = vcx
        .update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
        .unwrap();
    let label = h.tile.read_with(&vcx, |t, _| {
        let c = t.cursor.col;
        t.plan.columns[c].label.to_string()
    });
    assert_eq!(
        clip,
        format!(
            "{label}\n{}\n{}",
            h.cell(&vcx, 0, "strike"),
            h.cell(&vcx, 1, "strike")
        )
    );
    assert!(h.tile.read_with(&vcx, |t, _| t.register.is_none()));
}

#[gpui::test]
fn d_over_rows_deletes_them_in_one_undo_entry(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "delete", None);
    assert_eq!(h.tree(&vcx), vec!["SPX Z26 4000 P".to_string()]);
    assert_eq!(h.mode(&mut vcx), "normal");
    assert_eq!(h.notice(&vcx).as_deref(), Some("deleted 2 rows"));
    assert_eq!(
        h.footer(&vcx),
        None,
        "a deliberate delete is no lost anchor"
    );
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.register.as_ref().map(|r| r.len())),
        Some(2),
        "p puts back what d took"
    );
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(
        h.tree(&vcx).len(),
        3,
        "one undo restores both, the package with its legs"
    );
    h.dispatch(&mut vcx, "redo", None);
    assert_eq!(h.tree(&vcx).len(), 1);
}

#[gpui::test]
fn row_verbs_in_a_block_refuse_and_name_v(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_block", None);
    for (verb, text) in [
        ("delete", "d deletes rows — use V"),
        ("move_down", "shift+j/k move rows — use V"),
        ("group", "g p groups rows — use V"),
        ("ungroup", "g u ungroups rows — use V"),
    ] {
        h.dispatch(&mut vcx, verb, None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(text), "{verb}");
        assert_eq!(h.mode(&mut vcx), "visual", "a refusal keeps the selection");
    }
    assert_eq!(h.tree(&vcx).len(), 3);
}

#[gpui::test]
fn a_put_of_a_line_and_a_package_from_an_open_leg_lands_at_a_root_boundary(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // the line and the CS package
    h.dispatch(&mut vcx, "yank", None);
    h.dispatch(&mut vcx, "expand", None); // open the CS package
    h.dispatch(&mut vcx, "down", None); // its first leg
    assert!(
        h.tile.read_with(&vcx, |t, _| t
            .cursor_sheet_row()
            .is_some_and(|r| t.sheet.parent(r).is_some())),
        "the cursor sits on a leg"
    );
    h.dispatch(&mut vcx, "put_below", None);
    assert_eq!(h.footer(&vcx), None, "no refusal");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()),
        5,
        "both rows landed as roots"
    );
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()),
        3,
        "one undo takes both back"
    );
}

#[gpui::test]
fn a_leg_selected_without_its_package_is_deleted_as_a_leg(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "expand", None); // open the CS package
    h.dispatch(&mut vcx, "down", None); // its first leg
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "delete", None);
    let legs = |h: &Harness, vcx: &VisualTestContext| {
        h.tile.read_with(vcx, |t, _| {
            let p = (0..t.sheet.len())
                .find(|&r| t.sheet.is_package(r))
                .expect("the package stays");
            t.sheet.children(p).len()
        })
    };
    assert_eq!(legs(&h, &vcx), 1, "the package keeps its other leg");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()), 3);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(legs(&h, &vcx), 2, "one undo restores the leg");
}

#[gpui::test]
fn shift_j_moves_the_selected_block_as_a_unit_and_keeps_the_selection(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"]);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(
        h.tree(&vcx),
        vec!["SPX Z26 3000 P", "SPX Z26 5000 C", "SPX Z26 4000 P"]
    );
    assert_eq!(
        resolved(&h, &vcx).map(|r| r.1),
        Some(1..3),
        "the selection followed its lines"
    );
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some("cannot move past the end"));
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tree(&vcx)[0], "SPX Z26 5000 C");
}

#[gpui::test]
fn shift_j_moves_selected_legs_inside_an_open_package(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 4800/5000/5200 FLY"]);
    h.dispatch(&mut vcx, "expand", None);
    h.dispatch(&mut vcx, "down", None); // the first leg
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // the first two legs
    assert_eq!(
        h.tree(&vcx)[1..],
        ["SPX Z26 4800 C", "-2 SPX Z26 5000 C", "SPX Z26 5200 C"]
    );
    h.dispatch(&mut vcx, "move_down", None);
    assert_eq!(
        h.tree(&vcx)[1..],
        ["SPX Z26 5200 C", "SPX Z26 4800 C", "-2 SPX Z26 5000 C"],
        "the two legs moved down one step, in their order"
    );
    assert_eq!(
        resolved(&h, &vcx).map(|r| r.1),
        Some(2..4),
        "the selection still covers the same two legs"
    );
}

#[gpui::test]
fn g_p_over_root_lines_groups_them_and_u_restores(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"]);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "group", None);
    assert_eq!(h.tree(&vcx)[0], "CUSTOM SPX Z26");
    assert_eq!(h.mode(&mut vcx), "normal", "g p ends the selection");
    assert_eq!(h.tree(&vcx).len(), 4, "the new package is open on its legs");
    assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(0), "the cursor is on it");
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()), 3);
}

#[gpui::test]
fn g_p_names_why_it_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // includes the CS package
    h.dispatch(&mut vcx, "group", None);
    assert_eq!(
        h.footer(&vcx).as_deref(),
        Some("can't group: selection includes a package")
    );
}

#[gpui::test]
fn g_u_ungroups_every_selected_package_in_one_undo_entry(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(
        cx,
        &[
            "-5 SPX Z26 4800/5200 CS",
            "SPX Z26 5000 C",
            "2 SPX Z26 4000/3800 PS",
        ],
    );
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "ungroup", None);
    assert!(h.tile.read_with(&vcx, |t, _| {
        (0..t.sheet.len()).all(|r| !t.sheet.is_package(r))
    }));
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| (0..t.sheet.len())
            .filter(|&r| t.sheet.is_package(r))
            .count()),
        2
    );
    assert_eq!(
        h.tree(&vcx),
        vec![
            "-5 SPX Z26 4800/5200 CS",
            "SPX Z26 5000 C",
            "2 SPX Z26 4000/3800 PS"
        ],
        "undo brings the templates back, not custom packages"
    );
}

#[gpui::test]
fn g_u_with_no_package_selected_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P"]);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "ungroup", None);
    assert_eq!(h.footer(&vcx).as_deref(), Some("no package selected"));
}

#[gpui::test]
fn a_count_is_ignored_while_a_selection_is_live(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(
        cx,
        &[
            "SPX Z26 5000 C",
            "SPX Z26 4000 P",
            "SPX Z26 3000 P",
            "SPX Z26 2000 P",
        ],
    );
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "move_down", Some(2));
    assert_eq!(
        h.tree(&vcx),
        vec![
            "SPX Z26 4000 P",
            "SPX Z26 5000 C",
            "SPX Z26 3000 P",
            "SPX Z26 2000 P"
        ],
        "one step, not two"
    );
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "group", Some(3));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()),
        3,
        "the package holds the two selected lines, not three"
    );
}
