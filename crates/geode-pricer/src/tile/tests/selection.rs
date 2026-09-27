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

fn line_texts(h: &Harness, vcx: &VisualTestContext) -> Vec<String> {
    h.tile.read_with(vcx, |t, _| {
        (0..t.sheet.len())
            .filter(|&r| t.sheet.is_line(r))
            .map(|r| t.sheet.shorthand(r))
            .collect()
    })
}

fn notice(h: &Harness, vcx: &VisualTestContext) -> Option<String> {
    h.tile
        .read_with(vcx, |t, _| t.notice.clone())
        .map(|s| s.to_string())
}

#[gpui::test]
fn i_over_rows_writes_the_cursor_column_on_every_target_line_in_one_undo(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4500");
    h.dispatch(&mut vcx, "commit", None);
    // 5000 C, both CS legs, 4000 P → all 4500; qty untouched
    let strikes = line_texts(&h, &vcx);
    assert_eq!(strikes.len(), 4);
    assert!(strikes.iter().all(|s| s.contains("4500")), "{strikes:?}");
    assert_eq!(h.mode(&mut vcx), "visual", "a commit keeps the selection");
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(
        h.cell(&vcx, 0, "strike"),
        "5000",
        "one undo takes every write back"
    );
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert_eq!(h.cell(&vcx, 2, "strike"), "4000");
}

#[gpui::test]
fn a_typed_strike_over_a_package_and_its_leg_writes_each_leg_once(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "expand", None);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // the package + its first leg
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4900");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.footer(&vcx).is_none() || !h.footer(&vcx).unwrap().contains("refused"));
    assert_eq!(
        notice(&h, &vcx).as_deref(),
        Some("set 2 cells"),
        "each leg counted once"
    );
    assert!(h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
    // exactly the two legs changed, once each: undo once restores both
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
}

#[gpui::test]
fn a_block_commit_writes_the_cursor_column_and_counts_an_inapplicable_line(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C DO 4000", "SPX Z26 4000 P"]);
    h.command(&mut vcx, "view barrier").unwrap();
    let columns = h.columns(&vcx);
    let at = |name: &str| columns.iter().position(|c| c == name).unwrap();
    // A block strike … barrier over both lines, the cursor on the barrier
    // line's barrier cell.
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "up", None);
    h.dispatch(
        &mut vcx,
        "right",
        Some((at("barrier") - at("strike")) as u32),
    );
    assert_eq!(
        resolved(&h, &vcx).map(|r| r.2),
        Some(at("strike")..at("barrier") + 1)
    );
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "3900");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(
        notice(&h, &vcx).as_deref(),
        Some("set 1 cell, skipped 1 (1 n/a)"),
        "the vanilla line has no barrier; the block's other columns are not targets"
    );
    assert_eq!(h.cell(&vcx, 0, "barrier"), "3900");
    assert_eq!(
        h.cell(&vcx, 0, "strike"),
        "5000",
        "3900 is a valid strike too, and still not written"
    );
    assert_eq!(h.cell(&vcx, 1, "strike"), "4000");
}

#[gpui::test]
fn a_typed_commit_in_a_block_leaves_its_other_columns_untouched(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "2 SPX Z26 4000 P"]);
    // A block qty … strike, the cursor on strike: "5" parses as a qty too.
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    let (qty, strike) = {
        let c = h.columns(&vcx);
        let at = |n: &str| c.iter().position(|x| x == n).unwrap();
        (at("qty"), at("strike"))
    };
    h.dispatch(&mut vcx, "right", Some((strike - qty) as u32));
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "5");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(notice(&h, &vcx).as_deref(), Some("set 2 cells"));
    assert_eq!(
        line_texts(&h, &vcx),
        vec!["SPX Z26 5 C", "2 SPX Z26 5 P"],
        "the strikes took 5, the quantities kept theirs"
    );
}

#[gpui::test]
fn a_barrier_column_over_a_vanilla_line_is_counted_not_applicable(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C DO 4000", "SPX Z26 4000 P"]);
    h.command(&mut vcx, "view barrier").unwrap();
    goto_column(&h, &mut vcx, "barrier");
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "up", None); // the cursor on the barrier line
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "3900");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(
        notice(&h, &vcx).as_deref(),
        Some("set 1 cell, skipped 1 (1 n/a)"),
        "a vanilla line has no barrier: not applicable, not read-only"
    );
    assert_eq!(h.cell(&vcx, 0, "barrier"), "3900");
}

#[gpui::test]
fn nothing_accepting_refuses_with_the_editor_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "abc");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&mut vcx), "insert");
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
    assert_eq!(
        h.footer(&vcx).as_deref(),
        Some("no selected cell accepts 'abc', skipped 1 (1 refused)")
    );
}

#[gpui::test]
fn a_zero_qty_over_a_selection_writes_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    let before = line_texts(&h, &vcx);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "0");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&mut vcx), "insert", "the editor stays open");
    assert_eq!(line_texts(&h, &vcx), before, "no line changed");
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
    assert!(
        h.footer(&vcx)
            .is_some_and(|f| f.starts_with("no selected cell accepts '0'")),
        "{:?}",
        h.footer(&vcx)
    );
}

#[gpui::test]
fn an_unchanged_value_over_a_selection_is_no_undo_entry(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P"]);
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "1");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&mut vcx), "visual", "the editor closed");
    assert!(!h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
    assert_eq!(notice(&h, &vcx).as_deref(), Some("set 2 cells"));
}

#[gpui::test]
fn a_picked_type_commits_to_every_selected_line(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "type");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "p");
    h.dispatch(&mut vcx, "commit", None);
    let lines = line_texts(&h, &vcx);
    assert!(lines.iter().all(|s| s.ends_with(" P")), "{lines:?}");
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.cell(&vcx, 0, "type"), "C");
}

#[gpui::test]
fn a_date_commits_to_every_selected_line(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "NDX 3m 100% C"]);
    goto_column(&h, &mut vcx, "expiry");
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "up", None); // the dated line opens the field
    h.dispatch(&mut vcx, "edit", None);
    h.draw(&mut vcx);
    keys(&h, &mut vcx, "up");
    keys(&h, &mut vcx, "enter");
    assert_eq!(h.mode(&mut vcx), "visual", "the field closed");
    let want = Expiry::Date(ymd(2026, 12, 19));
    assert_eq!(expiry_of(&h, &vcx, 0), want);
    assert_eq!(expiry_of(&h, &vcx, 1), want, "the tenor line took the date");
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(expiry_of(&h, &vcx, 0), Expiry::Date(ymd(2026, 12, 18)));
    assert_ne!(expiry_of(&h, &vcx, 1), want, "one undo took both back");
}

#[gpui::test]
fn a_read_only_cursor_cell_refuses_to_open_under_a_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "last_col", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.mode(&mut vcx), "visual", "no editor opened");
    assert_eq!(h.footer(&vcx).as_deref(), Some("read-only"));
}

fn leg_qtys(h: &Harness, vcx: &VisualTestContext, pkg: usize) -> Vec<i64> {
    h.tile.read_with(vcx, |t, _| {
        t.sheet.children(pkg).map(|l| t.sheet.qty(l)).collect()
    })
}

fn can_undo(h: &Harness, vcx: &VisualTestContext) -> bool {
    h.tile.read_with(vcx, |t, _| t.undo.can_undo())
}

#[gpui::test]
fn an_unchanged_package_qty_over_a_selection_keeps_its_legs_weighted(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "down", None); // the CS package
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    assert_eq!(leg_qtys(&h, &vcx, 1), [-5, 5]);
    h.dispatch(&mut vcx, "edit", None);
    let opened = h.tile.read_with(&vcx, |t, cx| match &t.editor {
        Some(Editor::Text { input, .. }) => input.read(cx).value().to_string(),
        _ => panic!("a text editor is open"),
    });
    assert_eq!(opened, "-5");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(leg_qtys(&h, &vcx, 1), [-5, 5], "not -5/-5");
    assert_eq!(notice(&h, &vcx).as_deref(), Some("set 2 cells"));
    assert!(!can_undo(&h, &vcx), "no edit, no undo entry");
}

#[gpui::test]
fn a_typed_package_qty_over_a_selection_scales_its_legs_by_the_weights(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let weights = h.tile.read_with(&vcx, |t, _| {
        crate::core::package::package_qty(&t.sheet, 1).unwrap().1
    });
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "bottom", None); // the 4000 P
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "up", None); // the cursor on the CS package
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "3");
    h.dispatch(&mut vcx, "commit", None);
    let want: Vec<i64> = weights.iter().map(|w| 3 * w).collect();
    assert_eq!(leg_qtys(&h, &vcx, 1), want);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.qty(4)),
        3,
        "the line itself"
    );
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(leg_qtys(&h, &vcx, 1), [-5, 5], "one undo entry");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.qty(4)), 1);
    assert!(!can_undo(&h, &vcx));
}

#[gpui::test]
fn a_package_in_list_form_is_refused_a_selection_qty(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    // Put the CS in list form: its second leg's qty no longer fits.
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "expand", None);
    h.dispatch(&mut vcx, "down", Some(2));
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "3");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(leg_qtys(&h, &vcx, 1), [-5, 3]);
    // The package, then the 5000 C above it.
    h.dispatch(&mut vcx, "top", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "up", None);
    h.dispatch(&mut vcx, "edit", None);
    set_editor(&h, &mut vcx, "4");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(
        notice(&h, &vcx).as_deref(),
        Some("set 1 cell, skipped 1 (1 refused)")
    );
    assert_eq!(leg_qtys(&h, &vcx, 1), [-5, 3], "no leg written");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.qty(0)), 4);
}
