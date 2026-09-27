//! The panel's `V`/`v` grid selection through the production doors:
//! `dispatch`, `:` commands and deliveries.

use super::*;
use crate::core::bulk::step_delta;
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
    // A single `k` at row 0 is where normal mode enters the strip, and a
    // single `j` at the last row is where it wraps; both clamp here.
    h.dispatch(&mut vcx, "up", None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.cursor()),
        Cursor::Cell { row: 0, col: 4 }
    );
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Block, 0..1, 3..5)));
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "down", None);
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Block, 0..2, 3..5)));
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

/// A refused commit leaves the draft as it was, live steps included: the
/// editor stays open over the stepped block, and a later `escape` is what
/// takes the steps back.
#[gpui::test]
fn a_nothing_accepts_commit_after_steps_keeps_the_steps_and_the_editor(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    h.set_editor(&mut vcx, "abc");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&vcx), "insert");
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("abc"));
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4001", "0.5001", "0.6000"]);
    h.dispatch(&mut vcx, "cancel", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
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

/// A block over the first two nodes of both terms, the cursor ending on
/// row 1's second node.
fn select_two_nodes_by_two_terms(h: &Harness, vcx: &mut gpui::VisualTestContext) {
    h.dispatch(vcx, "right", Some(SLICE as u32));
    h.dispatch(vcx, "visual_block", None);
    h.dispatch(vcx, "down", None);
    h.dispatch(vcx, "right", None);
    // cursor ends on row 1, col 4 (0.5000)
}

/// With the editor untouched, arrows step every selected number in the
/// draft at once; `enter` keeps the steps and the selection.
#[gpui::test]
fn arrows_step_every_selected_number_live_and_enter_keeps_them(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", Some(2));
    h.dispatch(&mut vcx, "insert_up_big", None);
    assert_eq!(
        h.row_texts(&vcx, 0)[3..],
        ["0.1012", "0.2012", "0.3000"],
        "the grid paints the steps at once"
    );
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4012", "0.5012", "0.6000"]);
    assert_eq!(
        h.editor_value(&vcx).as_deref(),
        Some("0.5012"),
        "the editor follows the cursor cell"
    );
    assert!(
        h.header_texts(&vcx)
            .iter()
            .any(|t| t == "stepped 4 cells +12")
    );
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.mode(&vcx), "visual");
    assert_eq!(h.row_texts(&vcx, 1)[4], "0.5012");
    // Each cell keeps its own step: the editor's text is not written
    // across the block.
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.1012", "0.2012", "0.3000"]);
    assert!(h.editor_value(&vcx).is_none());
}

/// One step is one unit of each column's own places: `fwd` at two,
/// `atm` at four.
#[gpui::test]
fn a_block_steps_each_column_at_its_own_places(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None); // anchor on fwd (2 places)
    h.dispatch(&mut vcx, "right", None); // … through atm (4 places)
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_down", None);
    assert_eq!(h.row_texts(&vcx, 0)[..2], ["4499.99", "0.1799"]);
}

/// One `escape` puts the draft back exactly as `i` found it, an
/// earlier edit included, and leaves the selection.
#[gpui::test]
fn escape_after_steps_restores_the_draft_as_it_was_before_i(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.command(&mut vcx, "bump 1 col").unwrap(); // an earlier edit that must survive
    let before = h.tile.read_with(&vcx, |t, _| t.draft().clone());
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", Some(5));
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().clone()), before);
    assert_eq!(h.mode(&vcx), "visual");
    // The grid repaints the restore, not only the draft.
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.1000", "0.2000", "0.3000"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4000", "0.5000", "0.6000"]);
}

/// An F64 column steps by its places, an I64 column by one whole unit.
#[gpui::test]
fn a_mixed_block_steps_each_column_by_its_own_unit(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_mixed(cx);
    h.dispatch(&mut vcx, "visual_rows", None); // cursor on amount (F64)
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    let unit = step_delta(ColumnType::F64, ColumnFormat::MEASURE.precision, 1);
    let edits = h.tile.read_with(&vcx, |t, _| t.draft().edits.clone());
    assert_eq!(
        edits.get(&(0, 0)),
        Some(&Value::F64(1.25 + unit)),
        "amount by its places"
    );
    assert_eq!(
        edits.get(&(0, 1)),
        Some(&Value::I64(2)),
        "units by one whole unit"
    );
}

/// Typed text is absolute: `enter` replaces the steps, and a cell that
/// refuses the value returns to its pre-`i` value rather than keeping a
/// half-step.
#[gpui::test]
fn typing_after_steps_replaces_them_and_a_refusing_cell_returns_to_before(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_mixed(cx);
    h.dispatch(&mut vcx, "visual_rows", None);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None); // amount and units both stepped
    h.set_editor(&mut vcx, "2.5"); // typed: absolute from here
    h.dispatch(&mut vcx, "commit", None);
    let edits = h.tile.read_with(&vcx, |t, _| t.draft().edits.clone());
    assert_eq!(
        edits.get(&(0, 0)),
        Some(&Value::F64(2.5)),
        "the typed value replaced the step"
    );
    assert_eq!(
        edits.get(&(0, 1)),
        None,
        "units refused 2.5 and is back to its pre-`i` value, no half-step"
    );
    assert!(
        h.header_texts(&vcx)
            .contains(&"set 1 cell, skipped 1 (1 wrong type)".to_string())
    );
}

/// Once typed, arrows nudge the editor text alone and write nothing.
#[gpui::test]
fn after_typing_arrows_nudge_only_the_text(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "0.3000");
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.3001"));
    assert!(
        h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
        "typed: nothing live"
    );
}

/// A newer generation held behind while the steps were live stays news
/// after the escape restores the draft.
#[gpui::test]
fn escape_after_steps_keeps_a_behind_that_arrived_meanwhile(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let tag = h.with_document_tagged(&mut vcx);
    // An earlier edit, so the restored draft is non-empty and can stay Behind.
    h.command(&mut vcx, "bump 1 col").unwrap(); // fwd, both terms
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    // A newer generation lands under Hold → Behind; the base stays painted.
    h.deliver(&mut vcx, tag, Arc::new(document_of(&TERMS, &NODES, NEWER)));
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.draft().len()),
        2,
        "only the fwd bump remains"
    );
    assert!(
        h.tile.read_with(&vcx, |t, _| t.draft().is_behind()),
        "and the delivery is still news"
    );
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.model().base.clone().map(|b| b.as_of)),
        Some(BASE.to_string()),
        "a behind draft still paints the generation its edits were made on"
    );
}

/// A behind draft refuses both the selection editor and a selection
/// bump, writing nothing.
#[gpui::test]
fn the_selection_edit_refuses_while_behind(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let tag = h.with_document_tagged(&mut vcx);
    h.command(&mut vcx, "bump 1 col").unwrap();
    h.deliver(
        &mut vcx,
        tag,
        Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
    );
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "edit", None);
    assert!(
        h.editor_value(&vcx).is_none(),
        "no editor opens while behind"
    );
    assert_eq!(h.mode(&vcx), "visual");
    assert_eq!(
        h.command(&mut vcx, "bump 1"),
        Err("the draft is behind — :rebase or :revert first".to_string())
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.draft().len()),
        2,
        "nothing further was written"
    );
}

/// An automatic rebase while the editor is open moves the painted
/// generation; the pre-`i` draft is keyed to a grid no longer on screen,
/// so an escape keeps the steps and says so rather than misplacing them.
#[gpui::test]
fn escape_after_an_auto_rebase_keeps_the_steps_and_says_so(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "auto rebase").unwrap();
    let tag = h.with_document_tagged(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    h.deliver(&mut vcx, tag, Arc::new(document_of(&TERMS, &NODES, NEWER)));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.model().base.clone().map(|b| b.as_of)),
        Some(NEWER.to_string()),
        "the rebase painted the newer generation"
    );
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.1001", "0.2001", "0.3000"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4001", "0.5001", "0.6000"]);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 4);
    assert_eq!(
        notice_of(&h, &vcx).as_deref(),
        Some("steps kept: the document moved")
    );
    assert_eq!(h.mode(&vcx), "visual");
}

/// The same moved generation under a typed commit: the steps are not
/// restored away (that would misplace the pre-`i` draft), the typed
/// value lands, and the notice carries both facts.
#[gpui::test]
fn a_typed_commit_after_an_auto_rebase_keeps_the_steps_and_says_so(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "auto rebase").unwrap();
    let tag = h.with_document_tagged(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    h.deliver(&mut vcx, tag, Arc::new(document_of(&TERMS, &NODES, NEWER)));
    h.set_editor(&mut vcx, "0.25");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.2500", "0.2500", "0.3000"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.2500", "0.2500", "0.6000"]);
    assert_eq!(
        notice_of(&h, &vcx).as_deref(),
        Some("set 4 cells; steps kept: the document moved")
    );
}

/// A key switch closes the editor before it parks the draft, so live
/// steps are undone and never parked under the outgoing underlying.
#[gpui::test]
fn a_key_switch_mid_step_parks_the_draft_as_it_was_before_i(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.command(&mut vcx, "bump 1 col").unwrap();
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", Some(3));
    h.command(&mut vcx, "key SPX.Y").unwrap();
    assert!(h.editor_value(&vcx).is_none());
    h.document_request().expect("SPX.Y's request");
    h.command(&mut vcx, "key SPX.Z").unwrap();
    // A parked draft places its edits against the next delivery.
    let tag = h.document_request().expect("the key's request").tag;
    h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.draft().len()),
        2,
        "only the earlier fwd bump was parked"
    );
    assert_eq!(
        h.row_texts(&vcx, 0)[..5],
        ["4501.00", "0.1800", "-1.0000", "0.1000", "0.2000"]
    );
    assert_eq!(h.row_texts(&vcx, 1)[3..5], ["0.4000", "0.5000"]);
}

/// A click on another cell is a cancel like `escape`: the steps go.
#[gpui::test]
fn a_click_elsewhere_undoes_the_steps(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    assert!(!h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    draw(&mut vcx);
    let at = vcx
        .debug_bounds("marketdata-cell-0-0")
        .expect("the fwd cell paints")
        .center();
    click_at(&mut vcx, at, 1);
    assert!(h.editor_value(&vcx).is_none());
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
}

/// The rebuild a step makes keeps the open editor painted in its own
/// cell, where gpui routes typed characters to it.
#[gpui::test]
fn the_editor_stays_painted_in_its_cell_across_steps(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", Some(2));
    draw(&mut vcx);
    // Table column = model column + the row-label column.
    assert!(vcx.debug_bounds("marketdata-editor-1-5").is_some());
    type_keys(&mut vcx, "9");
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.50029"));
}

/// A palette revert while the editor is open discards the draft; the
/// escape that follows must not bring the reverted edits back.
#[gpui::test]
fn escape_after_a_revert_mid_step_leaves_the_draft_reverted(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.command(&mut vcx, "bump 1 col").unwrap(); // reverted below
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    h.dispatch(&mut vcx, "revert", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    h.dispatch(&mut vcx, "cancel", None);
    assert!(
        h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
        "the reverted edits stay reverted"
    );
    assert_eq!(h.row_texts(&vcx, 0)[..1], ["4500.00"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4000", "0.5000", "0.6000"]);
}

/// The selection cleared while the editor was open — an automatic rebase
/// onto a generation without the anchor's term: `enter` on typed text is
/// a single-cell commit, and its close must not take the value back out.
#[gpui::test]
fn a_typed_commit_after_the_selection_cleared_mid_step_keeps_the_value(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "auto rebase").unwrap();
    let tag = h.with_document_tagged(&mut vcx);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32 + 1));
    h.dispatch(&mut vcx, "visual_block", None); // anchor on the second term
    h.dispatch(&mut vcx, "up", None); // cursor on the first term's 0.2000
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    h.deliver(
        &mut vcx,
        tag,
        Arc::new(document_of(&TERMS[..1], &NODES, NEWER)),
    );
    assert!(
        resolved(&h, &vcx).is_none(),
        "the premise: the anchor is gone"
    );
    h.set_editor(&mut vcx, "0.25");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.editor_value(&vcx).is_none());
    assert_eq!(
        h.row_texts(&vcx, 0)[4],
        "0.2500",
        "the typed value stays written"
    );
}

/// Under `:auto replace` a delivery reverts the draft and says so. The
/// escape after it has no steps to keep and must not claim any, nor
/// overwrite the replace disclosure.
#[gpui::test]
fn escape_after_an_auto_replace_says_nothing_about_steps(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "auto replace").unwrap();
    let tag = h.with_document_tagged(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    h.deliver(&mut vcx, tag, Arc::new(document_of(&TERMS, &NODES, NEWER)));
    let disclosure = notice_of(&h, &vcx).expect("the replace disclosure");
    assert!(disclosure.contains("replaced"), "{disclosure}");
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(notice_of(&h, &vcx), Some(disclosure));
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4000", "0.5000", "0.6000"]);
}

/// A flat panel with a free-text `note` beside a number `amount`, over
/// two rows: a text cursor cell for the absolute-commit rule.
const SCHEDULE_NOTE_AMOUNT: PanelSpec = PanelSpec {
    kind: "sched_note_amount",
    title: "Notes",
    dataset: "div_schedule_note_amount",
    document: "div_schedule_note_amount",
    rows: RowAxis {
        column: "dividend_id",
        identity: RowIdentity::Minted,
        label: RowLabel::Shown,
    },
    columns: Columns::Values(&[
        ValueColumn {
            column: "note",
            label: "note",
            ty: ColumnType::Utf8,
            format: ColumnFormat::MEASURE,
            choices: None,
            required: false,
        },
        ValueColumn {
            column: "amount",
            label: "amount",
            ty: ColumnType::F64,
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

fn open_note_amount(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_spec(cx, &SCHEDULE_NOTE_AMOUNT, None);
    h.command(&mut vcx, "key SPX.Z").unwrap();
    h.visible(&mut vcx, true);
    let tag = h.document_request().unwrap().tag;
    let snapshot = Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into()), Some("SPX.Z".into())]),
            ),
            (
                meta("dividend_id", Attribution::Additive),
                TestColumn::Dict(vec![Some("D1".into()), Some("D2".into())]),
            ),
            (
                meta("note", Attribution::DeterminedNonAdditive),
                TestColumn::Dict(vec![Some("special".into()), Some("plain".into())]),
            ),
            (
                meta("amount", Attribution::DeterminedNonAdditive),
                TestColumn::F64(vec![Some(1.25), Some(0.5)]),
            ),
        ],
        0,
        provenance(BASE),
    );
    h.deliver(&mut vcx, tag, Arc::new(snapshot));
    (h, vcx)
}

/// On a text cursor cell the selection edit is absolute from the start:
/// an untouched `enter` writes the seeded text to every accepting cell.
#[gpui::test]
fn an_untouched_commit_on_a_text_cell_writes_the_seed_to_every_accepting_cell(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_note_amount(cx);
    h.dispatch(&mut vcx, "visual_rows", None); // cursor on D1's note
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("plain"));
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.col_texts(&vcx, 0), vec!["plain", "plain"]);
    assert_eq!(
        h.col_texts(&vcx, 1),
        vec!["1.25", "0.50"],
        "amount refused the text"
    );
    assert_eq!(
        notice_of(&h, &vcx).as_deref(),
        Some("set 2 cells, skipped 2 (2 wrong type)")
    );
}

// Mouse: shift+click and drag, through the same doors the keys use.

fn drag(vcx: &mut gpui::VisualTestContext, from: &str, to: &str) {
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
    draw(vcx);
}

fn shift_press(vcx: &mut gpui::VisualTestContext, selector: &str) {
    let at = centre_of(vcx, selector);
    let shift = gpui::Modifiers {
        shift: true,
        ..Default::default()
    };
    vcx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: shift,
        button: gpui::MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers: shift,
        button: gpui::MouseButton::Left,
        click_count: 1,
    });
}

/// The anchor is the cursor from before the press: the pointer's press
/// reaches the tile ahead of the table's own `SelectCell`, which only
/// comes on the release.
#[gpui::test]
fn shift_click_anchors_at_the_cursor_and_extends_a_block_to_the_click(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32)); // cursor on (0, 3)
    let clicks = h.host_clicks();
    shift_press(&mut vcx, "marketdata-cell-1-6"); // model (1, 5)
    assert_eq!(resolved(&h, &vcx).map(|r| (r.1, r.2)), Some((0..2, 3..6)));
    // Focus stays on the tile: the shell's own tile-level press (the
    // host's stand-in, which bubbles after the table's) still arrives,
    // and it is what puts the keyboard back on the tile.
    assert_eq!(h.host_clicks(), clicks + 1, "the press still propagates");
    h.dispatch(&mut vcx, "yank", None);
    assert!(clipboard(&mut vcx).is_some_and(|t| t.lines().count() == 3));
}

#[gpui::test]
fn a_plain_click_clears_the_selection_and_moves_the_cursor(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_block", None);
    let at = centre_of(&mut vcx, "marketdata-cell-1-6");
    click_at(&mut vcx, at, 1);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.cursor()),
        Cursor::Cell { row: 1, col: 5 }
    );
}

#[gpui::test]
fn a_drag_across_cells_selects_a_block_from_the_press(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    // Table column 4 is model column 3 (the label column leads).
    drag(&mut vcx, "marketdata-cell-0-4", "marketdata-cell-1-6");
    assert_eq!(resolved(&h, &vcx), Some((SelectKind::Block, 0..2, 3..6)));
    let at = centre_of(&mut vcx, "marketdata-cell-0-1");
    click_at(&mut vcx, at, 1); // clears
    drag(&mut vcx, "marketdata-cell-0-0", "marketdata-cell-1-0");
    assert_eq!(
        resolved(&h, &vcx).map(|r| (r.0, r.1)),
        Some((SelectKind::Rows, 0..2)),
        "a drag that starts on a row label selects rows"
    );
}

#[gpui::test]
fn a_shift_click_on_a_row_label_selects_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    shift_press(&mut vcx, "marketdata-cell-1-0");
    assert_eq!(
        resolved(&h, &vcx).map(|r| (r.0, r.1)),
        Some((SelectKind::Rows, 0..2))
    );
    assert_eq!(h.mode(&vcx), "visual");
}

/// A press that came down somewhere else (here the header) and only
/// passes over the cells with the button held never selects.
#[gpui::test]
fn a_drag_that_started_off_the_cells_selects_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    drag(&mut vcx, "marketdata-th-4", "marketdata-cell-1-6");
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(h.mode(&vcx), "normal");
}

/// A plain press on a row beside its last cell (the table's trailing
/// filler) is still a plain click: it clears the selection and moves the
/// cursor's row, keeping its column.
#[gpui::test]
fn a_plain_press_beside_the_cells_clears_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(2));
    h.dispatch(&mut vcx, "visual_block", None);
    draw(&mut vcx);
    let last = vcx
        .debug_bounds("marketdata-cell-1-6")
        .expect("the last cell is painted");
    let beside = gpui::point(last.right() + gpui::px(20.), last.center().y);
    click_at(&mut vcx, beside, 1);
    assert_eq!(resolved(&h, &vcx), None);
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.cursor()),
        Cursor::Cell { row: 1, col: 2 }
    );
}

/// Under a hidden row label the line-number gutter is the row's handle:
/// a shift press there selects rows, one on the value cell beside it a
/// block.
#[gpui::test]
fn on_a_hidden_label_panel_the_gutter_selects_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spec(cx, &test_fixtures::HIDDEN_SCHEDULE, None);
    h.with_flat_document(&mut vcx);
    vcx.update(|_, cx| {
        cx.set_global(UiSettings {
            line_numbers: LineNumbers::On,
        })
    });
    shift_press(&mut vcx, "marketdata-gutter-1");
    assert_eq!(
        resolved(&h, &vcx).map(|r| (r.0, r.1)),
        Some((SelectKind::Rows, 0..2))
    );
    let at = centre_of(&mut vcx, "marketdata-cell-0-0");
    click_at(&mut vcx, at, 1); // clears
    assert_eq!(resolved(&h, &vcx), None);
    shift_press(&mut vcx, "marketdata-cell-1-0");
    assert_eq!(
        resolved(&h, &vcx).map(|r| (r.0, r.1, r.2)),
        Some((SelectKind::Block, 0..2, 0..1))
    );
}

// A press inside the cell holding the open editor is the editor's own.

/// Caret placement: the click neither cancels the edit nor writes the
/// draft, and typing still reaches the field.
#[gpui::test]
fn a_click_inside_the_open_editor_keeps_it_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(4)); // (0, 4): 0.2000
    h.dispatch(&mut vcx, "edit", None);
    draw(&mut vcx);
    let at = centre_of(&mut vcx, "marketdata-editor-0-5");
    click_at(&mut vcx, at, 1);
    draw(&mut vcx);
    assert_eq!(h.mode(&vcx), "insert", "the click did not cancel the edit");
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    type_keys(&mut vcx, "9");
    let text = h.editor_value(&vcx).expect("still open");
    assert!(
        text.contains('9') && text.len() == "0.2000".len() + 1,
        "{text}"
    );
}

/// A double-click inside the open editor (a word selection there) does
/// not reopen it: the typed text is not reseeded away.
#[gpui::test]
fn a_double_click_inside_the_open_editor_keeps_the_typed_text(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", Some(4));
    h.dispatch(&mut vcx, "edit", None);
    h.set_editor(&mut vcx, "0.75");
    draw(&mut vcx);
    let at = centre_of(&mut vcx, "marketdata-editor-0-5");
    click_at(&mut vcx, at, 1);
    click_at(&mut vcx, at, 2);
    assert_eq!(h.mode(&vcx), "insert");
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.75"));
}

/// The bulk editor: a click inside it is not a cancel, so the live steps
/// are not undone and the selection stays.
#[gpui::test]
fn a_click_inside_the_stepped_bulk_editor_keeps_the_steps(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    draw(&mut vcx);
    let at = centre_of(&mut vcx, "marketdata-editor-1-5");
    click_at(&mut vcx, at, 1);
    draw(&mut vcx);
    assert_eq!(h.mode(&vcx), "insert");
    assert!(resolved(&h, &vcx).is_some(), "the selection stays");
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.1001", "0.2001", "0.3000"]);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4001", "0.5001", "0.6000"]);
}

/// A date cell's field: a segment click and a click on a separator
/// between segments both leave the field open.
#[gpui::test]
fn a_click_inside_a_date_cells_field_keeps_it_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_flat(cx);
    h.with_flat_document(&mut vcx);
    h.dispatch(&mut vcx, "edit", None); // ex date, a date cell
    assert!(h.tile.read_with(&vcx, |t, _| t.date_field().is_some()));
    let year = centre_of(&mut vcx, &format!("marketdata-date-seg-{TILE}-0"));
    click_at(&mut vcx, year, 1);
    assert_eq!(h.mode(&vcx), "insert", "a segment click keeps the field");
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.date_field().map(|f| f.segment())),
        Some(Segment::Year)
    );
    draw(&mut vcx);
    let seg = vcx
        .debug_bounds(Box::leak(
            format!("marketdata-date-seg-{TILE}-0").into_boxed_str(),
        ))
        .expect("painted");
    // Just past the year: the `-` separator, which has no listener of
    // its own.
    let separator = gpui::point(seg.right() + gpui::px(2.), seg.center().y);
    click_at(&mut vcx, separator, 1);
    assert_eq!(h.mode(&vcx), "insert", "a separator click keeps the field");
    assert!(h.tile.read_with(&vcx, |t, _| t.date_field().is_some()));
}

/// A typed axis's `o` opens a provisional row-label editor in the label
/// column. A click inside it, on the field's centre or on the gap past
/// its last segment, is the editor's: it stays open and the provisional
/// row is not dropped (closing would drop it, since it has no cells yet).
#[gpui::test]
fn a_click_inside_a_row_label_editor_keeps_it_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "insert_below", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
    let rows = h.tile.read_with(&vcx, |t, _| t.model().rows.len());
    assert_eq!(rows, 3, "two terms and the provisional row");
    let editor = format!("marketdata-editor-1-{LABEL_COL}");
    let at = centre_of(&mut vcx, &editor);
    click_at(&mut vcx, at, 1);
    draw(&mut vcx);
    assert!(
        h.tile.read_with(&vcx, |t, _| t.label_editor_open()),
        "a click inside the label editor keeps it"
    );
    assert_eq!(h.mode(&vcx), "insert");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.model().rows.len()),
        rows,
        "the provisional row is still there"
    );
    let bounds = vcx
        .debug_bounds(Box::leak(editor.into_boxed_str()))
        .expect("still painted");
    let edge = gpui::point(bounds.right() - gpui::px(2.), bounds.center().y);
    click_at(&mut vcx, edge, 1);
    draw(&mut vcx);
    assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.model().rows.len()), rows);
}

/// A double-click with a selection live: the first press clears it, so
/// the editor that opens is a single-cell one and a commit writes only
/// its cell.
#[gpui::test]
fn a_double_click_with_a_selection_opens_a_single_cell_editor(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    let at = centre_of(&mut vcx, "marketdata-cell-1-5");
    click_at(&mut vcx, at, 1);
    click_at(&mut vcx, at, 2);
    assert_eq!(h.mode(&vcx), "insert");
    assert_eq!(resolved(&h, &vcx), None, "no selection is left");
    h.set_editor(&mut vcx, "0.25");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.row_texts(&vcx, 1)[3..], ["0.4000", "0.2500", "0.6000"]);
    assert_eq!(h.row_texts(&vcx, 0)[3..], ["0.1000", "0.2000", "0.3000"]);
}

/// A row selection's members skip the slice values, so `i` on one of them
/// has no selection edit to open: it refuses and names `v`, rather than
/// writing the typed value (or the arrows' steps) into the ladder.
#[gpui::test]
fn i_on_a_slice_value_in_a_rows_selection_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "visual_rows", None); // cursor on fwd
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "edit", None);
    assert!(h.editor_value(&vcx).is_none(), "no editor opens");
    assert_eq!(h.mode(&vcx), "visual");
    assert_eq!(
        notice_of(&h, &vcx).as_deref(),
        Some("slice values are not in a row selection — use v")
    );
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
}

/// A block includes the slice columns it covers, so `i` on `fwd` inside
/// one opens and its typed value lands in the block's cells.
#[gpui::test]
fn i_on_a_slice_value_in_a_block_edits_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    h.dispatch(&mut vcx, "right", None); // atm
    h.dispatch(&mut vcx, "visual_block", None);
    h.dispatch(&mut vcx, "left", None); // cursor on fwd
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("4500.00"));
    h.set_editor(&mut vcx, "4600");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.row_texts(&vcx, 0)[..2], ["4600.00", "4600.0000"]);
    assert_eq!(notice_of(&h, &vcx).as_deref(), Some("set 2 cells"));
}

/// `:upload` with a stepping editor open closes it first: the steps are
/// undone before anything is assembled, so what goes upstream is the
/// draft as `i` found it, never the live steps the close takes back.
#[gpui::test]
fn an_upload_armed_mid_step_sends_the_draft_as_it_was_before_i(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_upload(cx);
    h.with_document(&mut vcx);
    h.edit_one_cell(&mut vcx); // fwd on the first term: the one edit to send
    let before = h.tile.read_with(&vcx, |t, _| t.draft().clone());
    let expected = h.tile.read_with(&vcx, |t, _| {
        crate::core::upload::assemble(&t.painted_snapshot().unwrap(), t.spec, t.model(), t.draft())
            .unwrap()
    });
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    h.dispatch(&mut vcx, "upload", None);
    assert!(h.editor_value(&vcx).is_none(), "the editor closed");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().clone()), before);
    draw(&mut vcx);
    type_keys(&mut vcx, "y");
    let sent = h.upload_request().expect("y submits");
    assert_eq!(sent.rows, expected, "no stepped value went upstream");
}

/// With nothing but live steps in the draft, the close leaves nothing to
/// upload.
#[gpui::test]
fn an_upload_of_live_steps_alone_is_refused_as_empty(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_upload(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    h.dispatch(&mut vcx, "upload", None);
    assert_eq!(notice_of(&h, &vcx).as_deref(), Some("nothing to upload"));
    assert_eq!(h.upload_prompt(&vcx), None, "nothing armed");
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
}

/// Steps on a `Sent` draft make it `Editing`, which lets go of the rows
/// that were sent. The `escape` that takes the steps back restores them
/// with the `Sent` state, so the upstream's echo still confirms.
#[gpui::test]
fn escape_after_steps_on_a_sent_draft_keeps_its_echo_check(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_upload(cx);
    h.with_document(&mut vcx);
    h.edit_one_cell(&mut vcx);
    let sent = h.upload_ok(&mut vcx);
    let at = h.sent_at(&vcx);
    h.dispatch(&mut vcx, "visual_block", None); // on the sent fwd
    h.dispatch(&mut vcx, "edit", None);
    h.dispatch(&mut vcx, "insert_up", None);
    assert!(
        !h.tile.read_with(&vcx, |t, _| t.draft().is_sent()),
        "the premise: the step left Sent"
    );
    h.dispatch(&mut vcx, "cancel", None);
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_sent()));
    assert_eq!(h.sent_rows(&vcx), Some(sent.clone()));
    h.echo(&mut vcx, test_fixtures::snapshot_of_at(&CVI, &sent, NEWER));
    let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
    assert_eq!(draft.state, DraftState::Clean, "{draft:?}");
    let confirmed = format!("sent {at}, confirmed ");
    assert!(
        h.header_texts(&vcx)
            .iter()
            .any(|t| t.starts_with(&confirmed)),
        "{:?}",
        h.header_texts(&vcx)
    );
}

/// While a selection editor is open its members are the edit's operand:
/// a palette motion, `V`/`v`, `escape` or `y` would change them under
/// the open editor, so each refuses and the selection stays as it was.
#[gpui::test]
fn selection_changing_verbs_refuse_while_a_selection_editor_is_open(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.with_document(&mut vcx);
    select_two_nodes_by_two_terms(&h, &mut vcx);
    h.dispatch(&mut vcx, "edit", None);
    let shape = resolved(&h, &vcx);
    let cursor = h.tile.read_with(&vcx, |t, _| t.cursor());
    for verb in ["down", "visual_rows", "visual_block", "escape", "yank"] {
        h.dispatch(&mut vcx, verb, None);
        assert_eq!(resolved(&h, &vcx), shape, "{verb}");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), cursor, "{verb}");
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.5000"), "{verb}");
        assert_eq!(
            notice_of(&h, &vcx).as_deref(),
            Some("finish the edit first — enter or escape"),
            "{verb}"
        );
    }
}

/// A delivery that loses the anchor clears the selection under an open
/// selection editor; the arrows then nudge the editor's text as they do
/// for any single cell, rather than refusing a step over no members.
#[gpui::test]
fn arrows_nudge_the_text_once_a_delivery_drops_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let tag = h.with_document_tagged(&mut vcx);
    h.dispatch(&mut vcx, "down", None);
    h.dispatch(&mut vcx, "right", Some(SLICE as u32 + 1));
    h.dispatch(&mut vcx, "visual_block", None); // anchor on the second term
    h.dispatch(&mut vcx, "up", None); // cursor on the first term's 0.2000
    h.dispatch(&mut vcx, "edit", None);
    h.deliver(
        &mut vcx,
        tag,
        Arc::new(document_of(&TERMS[..1], &NODES, NEWER)),
    );
    assert_eq!(resolved(&h, &vcx), None, "the premise: the anchor is gone");
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.2001"));
    assert_eq!(notice_of(&h, &vcx), None, "nothing refused");
    assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
}
