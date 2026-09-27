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
    // Typed, not the untouched `1` the field opened on (which writes
    // nothing): the same value in another spelling.
    set_editor(&h, &mut vcx, "01");
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
    assert_eq!(notice(&h, &vcx), None, "an untouched enter writes nothing");
    assert_eq!(editor_text(&h, &vcx), None, "and closes the editor");
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

// ---- the live step ----

/// Every line selected (`V`), the cursor on the 4000 P's strike, its
/// editor open and untouched.
fn step_setup(h: &Harness, vcx: &mut VisualTestContext) {
    goto_column(h, vcx, "strike");
    h.dispatch(vcx, "visual_rows", None);
    h.dispatch(vcx, "bottom", None);
    h.dispatch(vcx, "edit", None);
}

#[gpui::test]
fn arrows_step_every_target_line_live_and_enter_keeps_them_as_one_undo(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", Some(2));
    assert_eq!(
        h.cell(&vcx, 0, "strike"),
        "5002",
        "the grid paints the step at once"
    );
    assert_eq!(
        h.cell(&vcx, 1, "strike"),
        "4802/5202",
        "each leg by one unit per press"
    );
    assert_eq!(h.cell(&vcx, 2, "strike"), "4002");
    assert_eq!(
        editor_text(&h, &vcx).as_deref(),
        Some("4002"),
        "the editor follows its own cell"
    );
    assert_eq!(notice(&h, &vcx).as_deref(), Some("stepped 4 cells +2"));
    assert!(!can_undo(&h, &vcx), "nothing recorded mid-edit");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(editor_text(&h, &vcx), None, "enter closes the editor");
    assert_eq!(h.cell(&vcx, 0, "strike"), "5002", "enter keeps the steps");
    assert!(can_undo(&h, &vcx));
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(
        h.cell(&vcx, 0, "strike"),
        "5000",
        "one undo takes every step back"
    );
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert_eq!(h.cell(&vcx, 2, "strike"), "4000");
    assert!(!can_undo(&h, &vcx), "the steps were one entry");
}

#[gpui::test]
fn escape_rolls_every_step_back(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up_big", None);
    h.dispatch(&mut vcx, "insert_down", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5009");
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(editor_text(&h, &vcx), None, "the editor closed");
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000");
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert_eq!(h.cell(&vcx, 2, "strike"), "4000");
    assert!(!can_undo(&h, &vcx), "a rolled-back step leaves no history");
    assert_eq!(notice(&h, &vcx), None, "the step count no longer holds");
}

#[gpui::test]
fn escape_after_steps_rolls_back_even_after_prices_arrive(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    answer_all(&h, &mut vcx, 1.0);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    assert!(!h.prices().is_empty(), "the step repriced");
    answer_all(&h, &mut vcx, 2.0); // outcomes for the stepped lines
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000");
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert!(!can_undo(&h, &vcx));
}

#[gpui::test]
fn a_step_that_zeroes_a_qty_writes_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "2 SPX Z26 4000 P"]);
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    h.prices(); // drains the opening requests
    h.dispatch(&mut vcx, "insert_down", None); // 1 → 0 refuses; 2 → 1 must not land either
    assert_eq!(h.cell(&vcx, 1, "qty"), "2");
    assert_eq!(h.cell(&vcx, 0, "qty"), "1");
    assert_eq!(
        h.footer(&vcx).as_deref(),
        Some("quantity must not be zero"),
        "the refusal is shown"
    );
    assert_eq!(
        editor_text(&h, &vcx).as_deref(),
        Some("2"),
        "the editor stays"
    );
    assert!(h.prices().is_empty(), "nothing repriced");
}

#[gpui::test]
fn after_typing_arrows_nudge_only_the_text(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    set_editor(&h, &mut vcx, "4100");
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4101"));
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "typed: nothing live");
    assert_eq!(h.cell(&vcx, 2, "strike"), "4000");
}

#[gpui::test]
fn typing_after_steps_replaces_them_in_one_undo(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    set_editor(&h, &mut vcx, "4500");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "4500");
    assert_eq!(h.cell(&vcx, 1, "strike"), "4500");
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(
        h.cell(&vcx, 0, "strike"),
        "5000",
        "one undo: the steps were rolled back before the write"
    );
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert!(!can_undo(&h, &vcx));
}

#[gpui::test]
fn a_block_step_steps_each_block_column_and_counts_what_does_not_step(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "right", Some(2)); // strike, type, spot_shift
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(
        editor_text(&h, &vcx).as_deref(),
        Some(""),
        "no own spot shift"
    );
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(
        h.cell(&vcx, 0, "strike"),
        "5001",
        "the block's strike steps"
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.shift(0).spot_pct),
        Some(1.0)
    );
    assert_eq!(editor_text(&h, &vcx).as_deref(), Some("1"));
    assert_eq!(
        notice(&h, &vcx).as_deref(),
        Some("stepped 2 cells +1, skipped 1 (1 not numeric)")
    );
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200", "outside the block");
}

#[gpui::test]
fn a_rows_step_moves_only_the_cursor_column(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    let qtys = h.tile.read_with(&vcx, |t, _| {
        (0..t.sheet.len())
            .map(|r| t.sheet.qty(r))
            .collect::<Vec<_>>()
    });
    let shifts = h.tile.read_with(&vcx, |t, _| {
        (0..t.sheet.len())
            .filter(|&r| t.sheet.is_line(r))
            .all(|r| t.sheet.shift(r) == Default::default())
    });
    assert_eq!(&qtys[..1], [1], "V steps the strike alone");
    assert_eq!(&qtys[2..], [-5, 5, 1]);
    assert!(shifts, "no shift stepped");
}

#[gpui::test]
fn a_package_qty_step_moves_its_legs_by_the_template_weights(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let weights = h.tile.read_with(&vcx, |t, _| {
        crate::core::package::package_qty(&t.sheet, 1).unwrap().1
    });
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // the 5000 C and the CS package
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(editor_text(&h, &vcx).as_deref(), Some("-5"));
    h.dispatch(&mut vcx, "insert_up", None);
    let want: Vec<i64> = weights.iter().map(|w| -4 * w).collect();
    assert_eq!(leg_qtys(&h, &vcx, 1), want, "the package qty -5 → -4");
    assert_ne!(
        leg_qtys(&h, &vcx, 1),
        [-4, 6],
        "not each leg stepped on its own"
    );
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.qty(0)), 2, "the line");
    assert_eq!(editor_text(&h, &vcx).as_deref(), Some("-4"));
    assert_eq!(notice(&h, &vcx).as_deref(), Some("stepped 3 cells +1"));
    h.dispatch(&mut vcx, "commit", None);
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(leg_qtys(&h, &vcx, 1), [-5, 5], "one undo entry");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.qty(0)), 1);
    assert!(!can_undo(&h, &vcx));
}

#[gpui::test]
fn a_package_in_list_form_is_skipped_by_a_qty_step(cx: &mut gpui::TestAppContext) {
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
    // The package, then the 5000 C above it, the cursor on the line.
    h.dispatch(&mut vcx, "top", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "up", None);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(leg_qtys(&h, &vcx, 1), [-5, 3], "no leg stepped");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.qty(0)), 2);
    assert_eq!(
        notice(&h, &vcx).as_deref(),
        Some("stepped 1 cell +1, skipped 1 (1 refused)")
    );
}

/// No verb reaches the sheet while the editor is open (each closes it
/// first), so another writer is simulated here; the guard is what keeps
/// a rollback from undoing that writer's edit.
#[gpui::test]
fn escape_after_another_recorded_edit_keeps_the_steps_undoable(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    // The sheet-wide shift leaves every stepped cell as the step left it:
    // only the counter can tell this edit happened.
    let shift = crate::core::sheet::OwnShifts {
        spot_pct: Some(2.0),
        ..Default::default()
    };
    edit(&h, &mut vcx, Edit::SetSheetShift(shift));
    let sheet_shift = |h: &Harness, vcx: &VisualTestContext| {
        h.tile.read_with(vcx, |t, _| t.sheet.sheet_shift().spot_pct)
    };
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5001", "the steps stay");
    assert_eq!(sheet_shift(&h, &vcx), Some(2.0));
    h.dispatch(&mut vcx, "escape", None);
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "the steps are one entry");
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert_eq!(sheet_shift(&h, &vcx), Some(2.0));
    h.dispatch(&mut vcx, "undo", None);
    assert_eq!(sheet_shift(&h, &vcx), None);
}

/// The counter alone misses a sheet write that bypassed the recorded
/// path; the stepped cells' own values are checked too.
#[gpui::test]
fn escape_after_a_stepped_cell_changed_underneath_keeps_the_steps(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    let other = new_strike(&h, &vcx, 0, 4321.0);
    h.tile
        .update(&mut vcx, |t, _| t.sheet.apply(other).map(|_| ()))
        .unwrap();
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(4)),
        "SPX Z26 4001 P",
        "the steps stay"
    );
    assert!(can_undo(&h, &vcx), "recorded as one entry");
}

#[gpui::test]
fn escape_rearms_the_save_a_mid_step_save_took(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let seeded = stored(&h).shorthand(0);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    settle(&mut vcx, SAVE_IDLE);
    assert_ne!(
        stored(&h).shorthand(0),
        seeded,
        "a save mid-step took the step"
    );
    h.dispatch(&mut vcx, "cancel", None);
    settle(&mut vcx, SAVE_IDLE);
    assert_eq!(stored(&h).shorthand(0), seeded, "the rollback saves again");
}

#[gpui::test]
fn a_flush_mid_step_saves_the_sheet_without_the_steps(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let seeded = stored(&h).shorthand(0);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    vcx.update(|_, cx| h.factory.flush_all(cx));
    assert_eq!(
        stored(&h).shorthand(0),
        seeded,
        "a close or quit never persists steps escape would have taken back"
    );
}

#[gpui::test]
fn a_package_qty_stepping_to_zero_refuses_the_whole_press(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "-1 SPX Z26 4800/5200 CS"]);
    goto_column(&h, &mut vcx, "qty");
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // the line and the package
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(editor_text(&h, &vcx).as_deref(), Some("-1"));
    let legs = leg_qtys(&h, &vcx, 1);
    h.dispatch(&mut vcx, "insert_up", None); // the package -1 → 0 refuses
    assert_eq!(leg_qtys(&h, &vcx, 1), legs);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.sheet.qty(0)),
        1,
        "the line's 1 → 2 does not land either"
    );
    assert_eq!(h.footer(&vcx).as_deref(), Some("quantity must not be zero"));
}

#[gpui::test]
fn an_untouched_enter_over_a_selection_writes_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx); // the cursor on the 4000 P
    h.prices(); // drains the opening requests
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(editor_text(&h, &vcx), None, "enter closes the editor");
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "not the cursor's 4000");
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert_eq!(h.cell(&vcx, 2, "strike"), "4000");
    assert!(!can_undo(&h, &vcx), "no undo entry");
    assert!(h.prices().is_empty(), "nothing repriced");
}

#[gpui::test]
fn steps_that_net_to_nothing_keep_no_undo_entry(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    h.dispatch(&mut vcx, "insert_down", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(editor_text(&h, &vcx), None);
    assert!(
        !can_undo(&h, &vcx),
        "an undo that changes nothing is no entry"
    );
}

#[gpui::test]
fn a_flush_mid_step_closes_the_editor_on_the_rolled_back_sheet(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    vcx.update(|_, cx| h.factory.flush_all(cx));
    assert_eq!(
        editor_text(&h, &vcx),
        None,
        "no field left holding stepped text for a later enter"
    );
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "the grid is rebuilt");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5000");
    assert!(!can_undo(&h, &vcx));
}

#[gpui::test]
fn a_flush_before_any_step_keeps_the_open_editors_live_step(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx); // the cursor on the 4000 P
    h.prices(); // drains the opening requests
    vcx.update(|_, cx| h.factory.flush_all(cx));
    assert_eq!(
        editor_text(&h, &vcx).as_deref(),
        Some("4000"),
        "an unstepped editor stays open"
    );
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(editor_text(&h, &vcx), None, "enter closes the editor");
    assert_eq!(
        h.cell(&vcx, 0, "strike"),
        "5000",
        "an untouched enter still writes nothing, not the cursor's 4000"
    );
    assert_eq!(h.cell(&vcx, 1, "strike"), "4800/5200");
    assert!(!can_undo(&h, &vcx), "no undo entry");
}

// ---- the mouse: shift+click and drag, through the doors the keys use ----

/// The table column of plan column `name` (the tree column leads).
fn table_col(h: &Harness, vcx: &VisualTestContext, name: &str) -> usize {
    1 + h
        .columns(vcx)
        .iter()
        .position(|c| c == name)
        .unwrap_or_else(|| panic!("no {name} column"))
}

fn plan_col(h: &Harness, vcx: &VisualTestContext, name: &str) -> usize {
    table_col(h, vcx, name) - 1
}

fn press(vcx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>, shift: bool) {
    let modifiers = gpui::Modifiers {
        shift,
        ..Default::default()
    };
    vcx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers,
        button: gpui::MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers,
        button: gpui::MouseButton::Left,
        click_count: 1,
    });
}

fn shift_press(vcx: &mut VisualTestContext, selector: &str) {
    let at = centre_of(vcx, selector);
    press(vcx, at, true);
}

fn drag(vcx: &mut VisualTestContext, from: &str, to: &str) {
    let (a, b) = (centre_of(vcx, from), centre_of(vcx, to));
    vcx.simulate_event(gpui::MouseDownEvent {
        position: a,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseMoveEvent {
        position: b,
        pressed_button: Some(gpui::MouseButton::Left),
        modifiers: gpui::Modifiers::default(),
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        position: b,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 1,
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// The anchor is the cursor from before the press: the pointer's press
/// reaches the tile on mouse-down, ahead of the table's own
/// `SelectCell`, which only comes with the release.
#[gpui::test]
fn a_shift_click_anchors_at_the_cursor_and_extends_a_block_to_the_click(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    let s = plan_col(&h, &vcx, "strike");
    let presses = h.host_presses.get();
    shift_press(&mut vcx, &format!("pricer-cell-2-{}", s + 2));
    assert_eq!(
        resolved(&h, &vcx),
        Some((SelectKind::Block, 0..3, s..s + 2))
    );
    assert_eq!(h.mode(&mut vcx), "visual");
    // The keyboard stays with the tile: the press still bubbles to the
    // shell's tile-level press (the host's stand-in), and no field took
    // focus from it.
    assert_eq!(h.host_presses.get(), presses + 1, "the press propagates");
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    h.dispatch(&mut vcx, "yank", None);
    let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(
        clip.map(|t| t.lines().count()),
        Some(4),
        "the verb acts on the block: its header and three rows"
    );
    assert_eq!(resolved(&h, &vcx), None, "y ends the selection");
}

#[gpui::test]
fn a_plain_click_clears_the_selection_and_moves_the_cursor(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    let s = plan_col(&h, &vcx, "strike");
    h.dispatch(&mut vcx, "visual_block", None);
    let at = centre_of(&mut vcx, &format!("pricer-cell-2-{}", s + 2));
    click_at(&mut vcx, at, 1);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&mut vcx), "normal");
    assert_eq!(h.cursor(&vcx), Some((2, s + 1)));
}

#[gpui::test]
fn a_drag_across_cells_selects_a_block_and_from_the_tree_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let t = table_col(&h, &vcx, "strike");
    drag(
        &mut vcx,
        &format!("pricer-cell-0-{t}"),
        &format!("pricer-cell-2-{}", t + 1),
    );
    assert_eq!(
        resolved(&h, &vcx),
        Some((SelectKind::Block, 0..3, t - 1..t + 1)),
        "anchored at the press cell"
    );
    let at = centre_of(&mut vcx, &format!("pricer-cell-1-{t}"));
    click_at(&mut vcx, at, 1); // clears
    assert_eq!(resolved(&h, &vcx), None);
    drag(&mut vcx, "pricer-cell-0-0", "pricer-cell-2-0");
    assert_eq!(
        resolved(&h, &vcx).map(|r| (r.0, r.1)),
        Some((SelectKind::Rows, 0..3)),
        "a drag that starts on the tree cell selects rows"
    );
    assert_eq!(
        h.cursor(&vcx),
        Some((2, t - 1)),
        "the tree cell keeps the cursor's column"
    );
}

#[gpui::test]
fn a_shift_click_on_the_tree_cell_selects_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    shift_press(&mut vcx, "pricer-cell-2-0");
    assert_eq!(
        resolved(&h, &vcx).map(|r| (r.0, r.1)),
        Some((SelectKind::Rows, 0..3))
    );
    assert_eq!(h.mode(&mut vcx), "visual");
}

/// The line-number gutter beside the tree cell is the row's handle too.
#[gpui::test]
fn a_shift_click_on_the_gutter_selects_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    vcx.update(|_, cx| {
        cx.set_global(UiSettings {
            line_numbers: LineNumbers::On,
        })
    });
    shift_press(&mut vcx, "pricer-gutter-2");
    assert_eq!(
        resolved(&h, &vcx).map(|r| (r.0, r.1)),
        Some((SelectKind::Rows, 0..3))
    );
}

/// A press that came down somewhere else (here the header) and only
/// passes over the cells with the button held never selects.
#[gpui::test]
fn a_drag_that_started_off_the_cells_selects_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let t = table_col(&h, &vcx, "strike");
    drag(
        &mut vcx,
        &format!("pricer-th-{t}"),
        &format!("pricer-cell-2-{t}"),
    );
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&mut vcx), "normal");
}

/// A plain press on a row beside its last cell (the table's trailing
/// filler) is still a plain click: it clears the selection and moves the
/// cursor's row, keeping its column. A two-column view leaves room.
#[gpui::test]
fn a_plain_press_beside_the_cells_clears_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    let views = slim_views("\"qty\", \"strike\"");
    vcx.update(|_, cx| {
        h.factory.reload(
            views,
            TemplateSet::builtin(),
            None,
            std::time::Duration::from_secs(60),
            cx,
        )
    });
    vcx.run_until_parked();
    goto_column(&h, &mut vcx, "strike");
    h.dispatch(&mut vcx, "visual_block", None);
    let _ = centre_of(&mut vcx, "pricer-cell-2-2");
    let bounds = vcx
        .debug_bounds("pricer-cell-2-2")
        .expect("the last cell is painted");
    let beside = gpui::point(bounds.right() + gpui::px(20.), bounds.center().y);
    click_at(&mut vcx, beside, 1);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.cursor(&vcx), Some((2, 1)));
}

/// Caret placement: a press inside the open editor's own cell is the
/// editor's. It neither cancels the edit nor writes anything.
#[gpui::test]
fn a_click_inside_the_open_editor_keeps_it_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    goto_column(&h, &mut vcx, "strike");
    let t = table_col(&h, &vcx, "strike");
    h.dispatch(&mut vcx, "edit", None);
    let at = centre_of(&mut vcx, &format!("pricer-editor-0-{t}"));
    click_at(&mut vcx, at, 1);
    h.draw(&mut vcx);
    assert_eq!(
        h.mode(&mut vcx),
        "insert",
        "the click did not cancel the edit"
    );
    assert_eq!(editor_text(&h, &vcx).as_deref(), Some("5000"));
    assert!(!can_undo(&h, &vcx));
}

/// The live step's editor: a press inside it is not a cancel, so the
/// steps are not rolled back and the selection stays.
#[gpui::test]
fn a_click_or_double_click_inside_the_stepped_editor_keeps_the_steps(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    step_setup(&h, &mut vcx); // the editor on the 4000 P's strike
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5001");
    let t = table_col(&h, &vcx, "strike");
    let at = centre_of(&mut vcx, &format!("pricer-editor-2-{t}"));
    click_at(&mut vcx, at, 1);
    h.draw(&mut vcx);
    assert_eq!(h.mode(&mut vcx), "insert");
    assert!(resolved(&h, &vcx).is_some(), "the selection stays");
    assert_eq!(h.cell(&vcx, 0, "strike"), "5001", "the steps stay");
    assert_eq!(h.cell(&vcx, 1, "strike"), "4801/5201");
    // A double-click there (a word selection) reopens nothing either.
    click_at(&mut vcx, at, 1);
    click_at(&mut vcx, at, 2);
    h.draw(&mut vcx);
    assert_eq!(h.mode(&mut vcx), "insert");
    assert_eq!(h.cell(&vcx, 0, "strike"), "5001", "still stepped");
    assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4001"));
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.cell(&vcx, 0, "strike"), "5001");
    assert!(can_undo(&h, &vcx), "enter still keeps them as one entry");
}

/// A chevron press is a plain press on its row: it toggles the package
/// and clears a live selection, as any plain click does (the blotter's
/// chevron does the same). It never starts one, even held with shift.
#[gpui::test]
fn a_chevron_click_toggles_the_package_and_never_starts_a_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_seeded(cx, &BOOK);
    h.dispatch(&mut vcx, "visual_block", None);
    let at = centre_of(&mut vcx, "pricer-chevron-1");
    click_at(&mut vcx, at, 1);
    h.draw(&mut vcx);
    assert_eq!(h.tree(&vcx).len(), 5, "the package opened");
    assert_eq!(resolved(&h, &vcx), None, "a plain press clears");
    shift_press(&mut vcx, "pricer-chevron-1");
    h.draw(&mut vcx);
    assert_eq!(h.tree(&vcx).len(), 3, "the package closed");
    assert_eq!(
        resolved(&h, &vcx),
        None,
        "shift on a chevron starts nothing"
    );
    assert_eq!(h.mode(&mut vcx), "normal");
}
