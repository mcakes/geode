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

#[gpui::test]
fn bump_with_no_axis_moves_every_selected_number_and_keeps_the_selection(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", None);
    h.command(&mut vcx, "bump 0.01").unwrap();
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.1100", "0.2100", "0.3000"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4100", "0.5100", "0.6000"]);
    assert_eq!(h.mode(&vcx), "visual");
    // `row` keeps today's meaning even with a selection live.
    h.command(&mut vcx, "bump 1 row").unwrap();
    assert_eq!(
        h.row_texts(&vcx, 1)[0],
        "4510.00",
        "a row bump never moves the slice values"
    );
    // Column 5 is outside the block, so only a real row bump reaches it.
    assert_eq!(
        h.row_texts(&vcx, 1)[5],
        "1.6000",
        "an explicit axis bypasses the selection"
    );
}

#[gpui::test]
fn a_one_cell_selection_bump_says_cell(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    h.command(&mut vcx, "bump 0.01").unwrap();
    assert_eq!(notice_of(&h, &vcx).as_deref(), Some("bumped 1 cell"));
}

#[gpui::test]
fn a_rows_selection_bump_skips_the_slice_values(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.command(&mut vcx, "bump 0.01").unwrap();
    assert_eq!(
        h.row_texts(&vcx, 0),
        vec!["4500.00", "0.1800", "-1.0000", "0.1100", "0.2100", "0.3100"]
    );
}

/// A flat panel restored with one inserted row (`new-1`, after `D1`),
/// whose amount is then typed — `a_commit_on_an_inserted_row_writes_its_own_cells`'s
/// own setup. The inserted row is model row 1.
fn open_with_inserted_row(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let restored: toml::Table = format!(
        r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = []
[draft.rows.new-1]
after = "D1"
cells = {{ ex = {{ type = "date", value = "2027-01-15" }}, status = {{ type = "text", value = "declared" }} }}
"#
    )
    .parse()
    .unwrap();
    let (h, mut vcx) = open_spec(cx, &test_fixtures::SCHEDULE, Some(restored));
    h.visible(&mut vcx, true);
    let tag = h.document_request().unwrap().tag;
    h.deliver(
        &mut vcx,
        tag,
        Arc::new(test_fixtures::schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ])),
    );
    h.tile.update(&mut vcx, |t, cx| t.cursor_to(1, Some(1), cx));
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "2.5");
    h.dispatch(&mut vcx, "commit", None);
    (h, vcx)
}

/// Two numeric columns of different declared types — `amount` (F64) and
/// `units` (I64) — over one row, for the all-or-nothing and per-column
/// step rules. `cell_ref`s are `(0, 0)` amount and `(0, 1)` units.
const SCHEDULE_MIXED: PanelSpec = PanelSpec {
    kind: "sched_mixed",
    title: "Mixed",
    dataset: "div_schedule_mixed",
    document: "div_schedule_mixed",
    rows: RowAxis {
        column: "dividend_id",
        identity: RowIdentity::Minted,
        label: RowLabel::Shown,
    },
    columns: Columns::Values(&[
        ValueColumn {
            column: "amount",
            label: "amount",
            ty: ColumnType::F64,
            format: ColumnFormat::MEASURE,
            choices: None,
            required: true,
        },
        ValueColumn {
            column: "units",
            label: "units",
            ty: ColumnType::I64,
            format: ColumnFormat::MEASURE,
            choices: None,
            required: true,
        },
    ]),
    header: &[],
    slice_values: &[],
    value_type: ColumnType::F64,
    format: ColumnFormat::MEASURE,
    actions: &[],
};

fn open_mixed(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_spec(cx, &SCHEDULE_MIXED, None);
    h.command(&mut vcx, "key SPX.Z").unwrap();
    h.visible(&mut vcx, true);
    let tag = h.document_request().unwrap().tag;
    let snapshot = Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into())]),
            ),
            (
                meta("dividend_id", Attribution::Additive),
                TestColumn::Dict(vec![Some("D1".into())]),
            ),
            (
                meta("amount", Attribution::DeterminedNonAdditive),
                TestColumn::F64(vec![Some(1.25)]),
            ),
            (
                meta("units", Attribution::DeterminedNonAdditive),
                TestColumn::I64(vec![1]),
            ),
        ],
        0,
        provenance(BASE),
    );
    h.deliver(&mut vcx, tag, Arc::new(snapshot));
    (h, vcx)
}

#[gpui::test]
fn a_selection_bump_reaches_an_inserted_rows_cells(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with_inserted_row(cx);
    h.dispatch(&mut vcx, "up", None); // D1
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None); // D1 + new-1
    h.command(&mut vcx, "bump 1").unwrap();
    assert_eq!(h.cell(&vcx, 0, 1).0, "2.2500");
    assert_eq!(h.cell(&vcx, 1, 1).0, "3.5000");
    let inserted = h
        .tile
        .read_with(&vcx, |t, _| t.draft().row_state("new-1").cloned());
    assert!(
        matches!(&inserted, Some(RowEdit::Inserted { cells, .. }) if cells.get("amount") == Some(&Value::F64(3.5))),
        "the inserted row's cell is written by label, not by position: {inserted:?}"
    );
}

#[gpui::test]
fn a_fractional_bump_over_a_mixed_block_writes_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_mixed(cx);
    h.dispatch(&mut vcx, "visual_rows", None); // amount (F64) and units (I64)
    let refused = h.command(&mut vcx, "bump 0.5").unwrap_err();
    assert!(refused.contains("whole numbers"), "{refused}");
    assert!(
        h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
        "amount accepted 0.5, but units refused it, so nothing is written"
    );
}

/// A selection bump over document rows lands each column's declared type
/// (`amount` stays F64, `units` stays I64) and adds to the CURRENT value,
/// so a second bump composes with the first rather than reading through
/// to the document underneath. The selection stays for the next one.
#[gpui::test]
fn a_selection_bump_lands_each_declared_type_and_composes(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_mixed(cx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.command(&mut vcx, "bump 2").unwrap();
    let edits = |h: &Harness, vcx: &gpui::VisualTestContext| {
        h.tile.read_with(vcx, |t, _| {
            (
                t.draft().numeric_edit((0, 0)).cloned(),
                t.draft().numeric_edit((0, 1)).cloned(),
            )
        })
    };
    assert_eq!(
        edits(&h, &vcx),
        (Some(Value::F64(3.25)), Some(Value::I64(3)))
    );
    assert_eq!(notice_of(&h, &vcx).as_deref(), Some("bumped 2 cells"));
    assert_eq!(h.mode(&vcx), "visual");
    h.command(&mut vcx, "bump 2").unwrap();
    assert_eq!(
        edits(&h, &vcx),
        (Some(Value::F64(5.25)), Some(Value::I64(5)))
    );
}

/// Every selected cell a bump cannot move is counted by reason: a deleted
/// row's cells, and the date and status columns. The inserted row's
/// amount is bumped beside the document row's.
#[gpui::test]
fn a_selection_bump_counts_what_it_skipped_by_reason(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with_inserted_row(cx);
    h.dispatch(&mut vcx, "bottom", None); // D2
    h.dispatch(&mut vcx, "delete_row", None);
    h.dispatch(&mut vcx, "top", None);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "bottom", None);
    h.command(&mut vcx, "bump 1").unwrap();
    assert_eq!(
        notice_of(&h, &vcx).as_deref(),
        Some("bumped 2 cells, skipped 7 (3 deleted, 4 not numeric)")
    );
    assert_eq!(h.cell(&vcx, 0, 1).0, "2.2500");
    assert_eq!(h.cell(&vcx, 1, 1).0, "3.5000");
    assert_eq!(
        h.cell(&vcx, 2, 1).0,
        "0.5000",
        "a deleted row is never stepped"
    );
}

/// A selection with nothing numeric in it refuses, names why, and
/// writes nothing.
#[gpui::test]
fn a_selection_bump_with_no_numbers_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with_inserted_row(cx);
    h.dispatch(&mut vcx, "top", None);
    h.dispatch(&mut vcx, "first_col", None); // ex_date
    h.dispatch(&mut vcx, "visual_block", None);
    let before = h.tile.read_with(&vcx, |t, _| t.draft().len());
    let refused = h.command(&mut vcx, "bump 1").unwrap_err();
    assert_eq!(
        refused,
        "no numeric cells to step, skipped 1 (1 not numeric)"
    );
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), before);
}

/// `i` then `enter` over a block writes the one typed value to every
/// member, keeps the selection, and one revert takes all of it back.
#[gpui::test]
fn i_over_a_block_writes_one_value_to_every_accepting_cell(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", None);
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.mode(&vcx), "insert");
    h.set_editor(&mut vcx, "0.25");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.2500", "0.2500", "0.3000"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.2500", "0.2500", "0.6000"]);
    assert!(h.header_texts(&vcx).iter().any(|t| t == "set 4 cells"));
    assert_eq!(h.mode(&vcx), "visual", "a block commit keeps the selection");
    h.command(&mut vcx, "revert").unwrap();
    assert!(
        h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
        "one revert restores all of it"
    );
}

/// Over a flat row selection a number lands only in the number column;
/// the date and choice cells refuse it and are counted by reason.
#[gpui::test]
fn a_flat_block_commit_skips_cells_that_refuse_and_counts_them(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_flat(cx);
    h.with_flat_document(&mut vcx);
    h.dispatch(&mut vcx, "right", None); // amount
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "2");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(
        h.row_texts(&vcx, 0),
        vec!["2026-12-18", "2.0000", "declared"]
    );
    assert_eq!(
        h.row_texts(&vcx, 1),
        vec!["2027-03-19", "2.0000", "estimated"]
    );
    assert!(
        h.header_texts(&vcx)
            .contains(&"set 2 cells, skipped 4 (2 wrong type, 2 not an option)".to_string())
    );
}

/// When no member accepts the typed value, nothing is written and the
/// editor stays open with the text for the trader to fix.
#[gpui::test]
fn a_block_commit_nothing_accepts_is_refused_with_the_editor_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "abc");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&vcx), "insert");
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    assert!(
        h.header_texts(&vcx)
            .contains(&"no selected cell accepts 'abc', skipped 1 (1 wrong type)".to_string()),
        "{:?}",
        h.header_texts(&vcx)
    );
}

/// A choice picked from the popup lands in every selected choice cell;
/// the number and date cells refuse the option's text.
#[gpui::test]
fn a_choice_pick_over_a_selection_writes_the_option_to_every_choice_cell(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_flat(cx);
    h.with_flat_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(2)); // status
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
    h.set_choice_text(&mut vcx, "paid");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.col_texts(&vcx, 2), vec!["paid", "paid"]);
    assert_eq!(
        h.col_texts(&vcx, 1),
        vec!["1.2500", "0.5000"],
        "amount refused 'paid'"
    );
    assert!(
        h.header_texts(&vcx)
            .contains(&"set 2 cells, skipped 4 (4 wrong type)".to_string())
    );
    assert!(!h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
    assert_eq!(h.mode(&vcx), "visual", "a pick keeps the selection");
}

/// The date field's value commits to every selected date cell.
#[gpui::test]
fn a_date_commit_over_a_selection_writes_every_date_cell(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_flat(cx);
    h.with_flat_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None); // ex column
    h.dispatch(&mut vcx, "down", None); // cursor on D2's 2027-03-19
    h.dispatch(&mut vcx, "edit", None);
    draw(&mut vcx);
    type_keys(&mut vcx, "2"); // the day segment: 2027-03-02
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.col_texts(&vcx, 0), vec!["2027-03-02", "2027-03-02"]);
    assert!(h.header_texts(&vcx).contains(&"set 2 cells".to_string()));
    assert_eq!(h.mode(&vcx), "visual", "a date commit keeps the selection");
}
