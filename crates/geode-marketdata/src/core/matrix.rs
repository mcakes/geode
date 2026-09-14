//! The prepared grid a frame paints (market-data spec §8.2).
//!
//! Built ONCE per delivery or draft change, never in `render`: every cell
//! is already formatted text in a `SharedString`, so a frame clones
//! refcounts and formats nothing (PHILOSOPHY §6, spec §7.1's 8 ms pure-UI
//! budget). A `uniform_list` then lays out only the visible rows.
//!
//! Two shapes, one model. `Columns::Axis` pivots: the grid is (row axis ×
//! column axis) and the single value column fills it. `Columns::Values`
//! flattens: one row per document row, one column per value column. Both
//! answer the same `Cell`, so the tile's cursor, yank and edit paths know
//! only about a grid.

use crate::core::draft::Draft;
use crate::core::spec::{Columns, PanelSpec};
use geode_core::attribution::Attribution;
use geode_core::format::format_number;
use geode_core::snapshot::Snapshot;
use gpui::SharedString;
use std::collections::HashMap;

/// One prepared cell.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// Formatted with the panel's `ColumnFormat`, or empty for a NULL.
    /// Empty is the honest rendering of "no value here" (§6.3): a blank
    /// cell and a `0.0000` are different claims.
    pub text: SharedString,
    pub value: Option<f64>,
    pub edited: bool,
    pub sent: bool,
    /// The grid index this cell sits at, which is also the key a
    /// [`Draft`] edit is stored under. Carried on the cell so a render
    /// closure that already has the cell never has to reconstruct it —
    /// for `Columns::Values` it reads as (document row, value column).
    pub cell_ref: (usize, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowModel {
    pub label: SharedString,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatrixModel {
    /// The document key, in `document_columns()` order.
    pub key: Vec<String>,
    /// The generation's source time, RFC 3339 — `Provenance.datasets[0]
    /// .as_of`, which for a live document read is that document's own
    /// source time (`Catalog::live_source_time`, Part 1 §4.5). This is
    /// the identity a [`Draft`] compares, not a `gen_id`.
    pub source_time: Option<String>,
    /// (label, value) per header attribute the spec names, in spec order.
    pub header: Vec<(SharedString, SharedString)>,
    pub columns: Vec<SharedString>,
    pub rows: Vec<RowModel>,
}

impl MatrixModel {
    /// Pivot or flatten `snapshot` per `spec`, with `draft`'s edits
    /// painted over the document's own values.
    ///
    /// `Err` is a document that cannot be laid out as a grid at all: a
    /// missing axis column, no value column, or — for a pivot — a hole or
    /// a repeated (row, column) pair. A hole is deliberately an error
    /// rather than a blank cell: the axes say the document claims a value
    /// there, so a blank would be this model inventing the §6.3 claim
    /// "this number does not belong to this row" on the document's
    /// behalf, and a zero would be worse.
    pub fn build(
        snapshot: &Snapshot,
        spec: &PanelSpec,
        draft: &Draft,
    ) -> Result<MatrixModel, String> {
        let source_time = snapshot
            .provenance()
            .datasets
            .first()
            .and_then(|f| f.as_of.clone());
        // An empty result is not a defect: the key may simply have no
        // document yet, or an as-of before its first publish (the
        // compiler's `and false` arm). The tile paints "no document
        // received for <key>" off `rows.is_empty()`.
        if snapshot.rows() == 0 {
            return Ok(MatrixModel {
                source_time,
                ..MatrixModel::default()
            });
        }

        let rows_idx = snapshot
            .column_index(spec.rows)
            .ok_or_else(|| format!("the document has no '{}' column", spec.rows))?;
        let key = key_of(snapshot, spec, rows_idx);
        let header = header_of(snapshot, spec);
        let (columns, rows) = match spec.columns {
            Columns::Axis(axis) => pivot(snapshot, spec, draft, rows_idx, axis)?,
            Columns::Values => flatten(snapshot, spec, draft, rows_idx)?,
        };
        Ok(MatrixModel {
            key,
            source_time,
            header,
            columns,
            rows,
        })
    }

    /// The "no document received" shape: the key the panel asked about
    /// and nothing else. Distinct from a built model only in having no
    /// rows — the tile's header says which.
    pub fn empty(spec: &PanelSpec, key: &[String]) -> MatrixModel {
        // `spec` is taken for the call site's sake: every other model in
        // the tile is built from one, and a panel that later wants its
        // column strip painted while waiting has it here.
        let _ = spec;
        MatrixModel {
            key: key.to_vec(),
            ..MatrixModel::default()
        }
    }

    /// The (row label, column label) pair a cell sits at — the identity a
    /// [`Draft`] records so an edit survives a new generation.
    ///
    /// Out of range answers a pair of empty strings rather than panicking:
    /// a cursor can outlive the model it was set against by one delivery.
    pub fn label_of(&self, cell: (usize, usize)) -> (SharedString, SharedString) {
        (
            self.rows
                .get(cell.0)
                .map(|r| r.label.clone())
                .unwrap_or_default(),
            self.columns.get(cell.1).cloned().unwrap_or_default(),
        )
    }
}

/// The document key: the columns ahead of the row axis that the spec does
/// not itself paint.
///
/// `DatasetSpec::document_columns()` emits the key columns first (spec
/// §3.3), and a panel's row axis is its dataset's first axis, so the
/// prefix ahead of it is the key. The `spec.names` filter is the belt: a
/// spec whose rows were a *later* axis would otherwise show the earlier
/// axis as part of the key.
fn key_of(snapshot: &Snapshot, spec: &PanelSpec, rows_idx: usize) -> Vec<String> {
    (0..rows_idx)
        .filter(|i| {
            snapshot
                .meta_at(*i)
                .is_some_and(|m| !spec.names(&m.name) && !is_value(snapshot, *i))
        })
        .filter_map(|i| label_at(snapshot, i, 0))
        .collect()
}

/// The header attributes the spec names, read off row 0 — a document-level
/// attribute is constant within one document (spec §3.1), so any row would
/// do. An attribute the document does not carry is left out rather than
/// shown blank: the panel says what it has.
fn header_of(snapshot: &Snapshot, spec: &PanelSpec) -> Vec<(SharedString, SharedString)> {
    spec.header
        .iter()
        .filter_map(|name| {
            let idx = snapshot.column_index(name)?;
            let value = label_at(snapshot, idx, 0)?;
            Some((
                SharedString::from(name.to_string()),
                SharedString::from(value),
            ))
        })
        .collect()
}

/// Whether a column is one of the document's values.
///
/// `compile_document` marks a document value `DeterminedNonAdditive` — it
/// is real on its row and must never be totalled — and every label column
/// (the key, the axes, the document-level attributes) `Additive`. That is
/// the only signal in a delivered snapshot that says which columns hold
/// the numbers, so it is what decides both what a pivot fills its grid
/// from and which columns `Columns::Values` lays flat.
fn is_value(snapshot: &Snapshot, idx: usize) -> bool {
    snapshot.meta_at(idx).is_some_and(|m| {
        m.attribution_by_depth.first() == Some(&Attribution::DeterminedNonAdditive)
    })
}

fn value_columns(snapshot: &Snapshot) -> Vec<usize> {
    (0..snapshot.columns())
        .filter(|i| is_value(snapshot, *i))
        .collect()
}

/// One cell's label, whatever type the column arrived in.
///
/// `display_at` covers text, dictionaries, dates, timestamps and bools;
/// numbers it deliberately leaves alone (precision is the renderer's
/// decision), so an f64 axis like CVI's `node` falls through to `{}` —
/// the shortest round-tripping spelling, which is what makes a node read
/// `-20` and `3.5` rather than `-20.0000`. An axis value is a label, not
/// a measure, so the panel's `ColumnFormat` has no business here.
fn label_at(snapshot: &Snapshot, idx: usize, row: usize) -> Option<String> {
    if let Some(text) = snapshot.display_at(idx, row) {
        return Some(text);
    }
    snapshot.f64_at(idx, row).map(|v| format!("{v}"))
}

/// Where every (row label, column label) pair lives in the snapshot,
/// nested so a lookup in the fill loop borrows both labels and allocates
/// nothing. Label order is the document's own, first appearance first.
struct Grid {
    rows: Vec<String>,
    columns: Vec<String>,
    at: HashMap<String, HashMap<String, usize>>,
}

/// One pass over the document: the two label orders and the pair → row
/// index map. A repeated pair is refused here, because a pivot that
/// silently kept one of two rows for a cell would drop a real value with
/// nothing to show it had.
fn index_grid(
    snapshot: &Snapshot,
    spec: &PanelSpec,
    rows_idx: usize,
    col_idx: usize,
    axis: &str,
) -> Result<Grid, String> {
    let mut grid = Grid {
        rows: Vec::new(),
        columns: Vec::new(),
        at: HashMap::new(),
    };
    // Membership sets beside the order vectors: `Vec::contains` per
    // document row is quadratic in the label count, and a schedule-shaped
    // document has thousands.
    let mut seen_rows: HashMap<String, ()> = HashMap::new();
    let mut seen_cols: HashMap<String, ()> = HashMap::new();
    for row in 0..snapshot.rows() {
        let row_label = label_at(snapshot, rows_idx, row).unwrap_or_default();
        let col_label = label_at(snapshot, col_idx, row).unwrap_or_default();
        if seen_rows.insert(row_label.clone(), ()).is_none() {
            grid.rows.push(row_label.clone());
        }
        if seen_cols.insert(col_label.clone(), ()).is_none() {
            grid.columns.push(col_label.clone());
        }
        if let Some(previous) = grid
            .at
            .entry(row_label.clone())
            .or_default()
            .insert(col_label.clone(), row)
        {
            return Err(format!(
                "the document repeats {}='{row_label}' {axis}='{col_label}' \
                 (rows {previous} and {row}): one of the two values would vanish",
                spec.rows
            ));
        }
    }
    Ok(grid)
}

/// The pivot. Row labels down the side in document order, column labels
/// across the top in document order, the one value column in the cells.
///
/// The order is the document's own, first appearance first, and is NOT
/// sorted: a term ladder and a node ladder arrive in the order the desk
/// means them to be read (`compile_document` selects `order by` the axes),
/// and sorting the labels here would silently reshuffle a trader's grid —
/// lexically, at that, so `-20` would land between `-1` and `3.5`.
/// `Grid::at` is keyed by the labels themselves rather than by their
/// indices precisely so that this ordering decision is separable from
/// where the values come from.
fn pivot(
    snapshot: &Snapshot,
    spec: &PanelSpec,
    draft: &Draft,
    rows_idx: usize,
    axis: &str,
) -> Result<(Vec<SharedString>, Vec<RowModel>), String> {
    let col_idx = snapshot
        .column_index(axis)
        .ok_or_else(|| format!("the document has no '{axis}' column"))?;
    let value_idx = *value_columns(snapshot)
        .first()
        .ok_or_else(|| format!("the document '{}' has no value column", spec.dataset))?;

    let grid = index_grid(snapshot, spec, rows_idx, col_idx, axis)?;

    let mut rows = Vec::with_capacity(grid.rows.len());
    for (ri, row_label) in grid.rows.iter().enumerate() {
        let mut cells = Vec::with_capacity(grid.columns.len());
        for (ci, col_label) in grid.columns.iter().enumerate() {
            match grid.at.get(row_label).and_then(|m| m.get(col_label)) {
                Some(&srow) => {
                    cells.push(cell_of(snapshot, value_idx, srow, (ri, ci), spec, draft))
                }
                None => {
                    return Err(format!(
                        "the document has no cell for {}='{row_label}' {axis}='{col_label}'",
                        spec.rows
                    ));
                }
            }
        }
        rows.push(RowModel {
            label: SharedString::from(row_label.clone()),
            cells,
        });
    }
    Ok((
        grid.columns.into_iter().map(SharedString::from).collect(),
        rows,
    ))
}

/// The flat shape: one row per document row, one column per value column.
fn flatten(
    snapshot: &Snapshot,
    spec: &PanelSpec,
    draft: &Draft,
    rows_idx: usize,
) -> Result<(Vec<SharedString>, Vec<RowModel>), String> {
    let value_idxs = value_columns(snapshot);
    if value_idxs.is_empty() {
        return Err(format!(
            "the document '{}' has no value column",
            spec.dataset
        ));
    }
    let columns = value_idxs
        .iter()
        .filter_map(|i| snapshot.meta_at(*i))
        .map(|m| SharedString::from(m.name.clone()))
        .collect();
    let rows = (0..snapshot.rows())
        .map(|row| RowModel {
            label: SharedString::from(label_at(snapshot, rows_idx, row).unwrap_or_default()),
            cells: value_idxs
                .iter()
                .enumerate()
                .map(|(ci, &idx)| cell_of(snapshot, idx, row, (row, ci), spec, draft))
                .collect(),
        })
        .collect();
    Ok((columns, rows))
}

/// One cell: the draft's value where there is an edit, the document's
/// otherwise.
fn cell_of(
    snapshot: &Snapshot,
    value_idx: usize,
    srow: usize,
    cell_ref: (usize, usize),
    spec: &PanelSpec,
    draft: &Draft,
) -> Cell {
    if let Some(&edited) = draft.edits.get(&cell_ref) {
        return Cell {
            text: SharedString::from(format_number(edited, &spec.format).text),
            value: Some(edited),
            edited: true,
            sent: draft.is_sent(),
            cell_ref,
        };
    }
    // `f64_at`, never a raw values slice: a NULL there is a deliberate
    // "this number does not belong to this row" and reads back as 0.0
    // out of the storage buffer.
    let value = snapshot.f64_at(value_idx, srow);
    Cell {
        text: value
            .map(|v| SharedString::from(format_number(v, &spec.format).text))
            .unwrap_or_default(),
        value,
        edited: false,
        sent: false,
        cell_ref,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::draft::{Draft, DraftState};
    use crate::core::spec::{CVI, Columns, PanelSpec};
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
    use geode_core::view::ColumnFormat;
    use proptest::prelude::*;

    const TERMS: [&str; 2] = ["2026-10-16", "2026-11-20"];
    const NODES: [f64; 3] = [-20.0, -1.0, 3.5];
    const BASE: &str = "2026-09-12T14:00:00Z";

    fn meta(name: &str, attribution: Attribution) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            // A document snapshot is depth 0 only (spec §7), which is why
            // `compile_document` emits exactly one attribution per column.
            attribution_by_depth: vec![attribution],
            scope_semantics: ScopeSemantics::Direct,
        }
    }

    fn provenance(as_of: &str) -> Provenance {
        Provenance {
            datasets: vec![Freshness {
                dataset: "cvi_params".into(),
                as_of: Some(as_of.into()),
                generation: 7,
            }],
            as_of_request: None,
        }
    }

    /// A CVI document in the shape `query::document::compile_document`
    /// delivers one: `document_columns()` order (key, axes, value,
    /// document-level attributes), the value column marked
    /// `DeterminedNonAdditive` and every label column `Additive`, no
    /// grouping.
    ///
    /// `term` is a dictionary column of date text rather than a real
    /// `Date32`: `TestColumn` has no date arm, and what the panel reads
    /// off an axis is its label either way.
    fn document(cells: &[(String, f64, Option<f64>)]) -> Snapshot {
        let n = cells.len();
        Snapshot::for_tests_with_provenance(
            vec![
                (
                    meta("underlying_ref", Attribution::Additive),
                    TestColumn::Dict(vec![Some("SPX.Z".into()); n]),
                ),
                (
                    meta("term", Attribution::Additive),
                    TestColumn::Dict(cells.iter().map(|c| Some(c.0.clone())).collect()),
                ),
                (
                    meta("node", Attribution::Additive),
                    TestColumn::F64(cells.iter().map(|c| Some(c.1)).collect()),
                ),
                (
                    meta("param", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(cells.iter().map(|c| c.2).collect()),
                ),
                (
                    meta("anchor_date", Attribution::Additive),
                    TestColumn::Dict(vec![Some("2026-09-12".into()); n]),
                ),
                (
                    meta("spot_ref", Attribution::Additive),
                    TestColumn::F64(vec![Some(5000.0); n]),
                ),
            ],
            0,
            provenance(BASE),
        )
    }

    /// The six-row CVI document: two terms × three nodes, `param` running
    /// 0.1 … 0.6 in axis order.
    fn full_grid() -> Snapshot {
        let mut cells = Vec::new();
        for (t, term) in TERMS.iter().enumerate() {
            for (n, node) in NODES.iter().enumerate() {
                let i = t * NODES.len() + n + 1;
                cells.push(((*term).to_string(), *node, Some(i as f64 / 10.0)));
            }
        }
        document(&cells)
    }

    fn labels(model: &MatrixModel, row: usize) -> Vec<String> {
        model.rows[row]
            .cells
            .iter()
            .map(|c| c.text.to_string())
            .collect()
    }

    fn columns_of(model: &MatrixModel) -> Vec<String> {
        model.columns.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn a_pivot_puts_the_row_axis_down_the_side_and_the_column_axis_across() {
        let snap = full_grid();
        let model = MatrixModel::build(&snap, &CVI, &Draft::default()).expect("a complete grid");

        assert_eq!(model.key, vec!["SPX.Z".to_string()]);
        assert_eq!(model.source_time.as_deref(), Some(BASE));
        assert_eq!(model.rows.len(), 2);
        assert_eq!(columns_of(&model), vec!["-20", "-1", "3.5"]);
        assert_eq!(model.rows[0].label.to_string(), "2026-10-16");
        assert_eq!(model.rows[1].label.to_string(), "2026-11-20");

        let cell = &model.rows[0].cells[1];
        assert_eq!(
            cell.text.to_string(),
            "0.2000",
            "CVI formats to four places"
        );
        assert_eq!(cell.value, Some(0.2));
        assert!(!cell.edited);
        assert!(!cell.sent);
        assert_eq!(
            cell.cell_ref,
            (0, 1),
            "a cell carries the grid index the draft is keyed by"
        );
        assert_eq!(labels(&model, 1), vec!["0.4000", "0.5000", "0.6000"]);

        assert_eq!(
            model
                .header
                .iter()
                .map(|(l, v)| (l.to_string(), v.to_string()))
                .collect::<Vec<_>>(),
            vec![
                ("anchor_date".to_string(), "2026-09-12".to_string()),
                ("spot_ref".to_string(), "5000".to_string()),
            ],
            "the header reads the document-level attributes off row 0"
        );
    }

    #[test]
    fn the_pivot_keeps_the_documents_own_axis_order() {
        // Delivered newest term first and with the nodes unsorted: the
        // panel shows a document as the document lists it, because the
        // order is the desk's (an expiry ladder, a node ladder), and
        // sorting the labels would silently reorder a trader's grid.
        let cells = vec![
            ("2026-11-20".to_string(), 3.5, Some(0.1)),
            ("2026-11-20".to_string(), -20.0, Some(0.2)),
            ("2026-10-16".to_string(), 3.5, Some(0.3)),
            ("2026-10-16".to_string(), -20.0, Some(0.4)),
        ];
        let model =
            MatrixModel::build(&document(&cells), &CVI, &Draft::default()).expect("a full grid");
        assert_eq!(
            model
                .rows
                .iter()
                .map(|r| r.label.to_string())
                .collect::<Vec<_>>(),
            vec!["2026-11-20", "2026-10-16"]
        );
        assert_eq!(columns_of(&model), vec!["3.5", "-20"]);
        assert_eq!(labels(&model, 0), vec!["0.1000", "0.2000"]);
        assert_eq!(labels(&model, 1), vec!["0.3000", "0.4000"]);
    }

    #[test]
    fn a_missing_cell_is_a_hole_and_the_error_names_the_pair() {
        let mut cells = Vec::new();
        for (t, term) in TERMS.iter().enumerate() {
            for (n, node) in NODES.iter().enumerate() {
                if t == 1 && n == 2 {
                    continue;
                }
                cells.push((
                    (*term).to_string(),
                    *node,
                    Some((t * NODES.len() + n) as f64),
                ));
            }
        }
        let err = MatrixModel::build(&document(&cells), &CVI, &Draft::default())
            .expect_err("a hole is refused, never painted as a zero");
        assert!(err.contains("2026-11-20"), "{err}");
        assert!(err.contains("3.5"), "{err}");
        assert!(err.contains("term") && err.contains("node"), "{err}");
    }

    #[test]
    fn a_repeated_pair_is_refused_and_the_error_names_it() {
        let cells = vec![
            ("2026-10-16".to_string(), -20.0, Some(1.0)),
            ("2026-10-16".to_string(), -20.0, Some(2.0)),
        ];
        let err = MatrixModel::build(&document(&cells), &CVI, &Draft::default())
            .expect_err("two rows for one cell: one of the two values would vanish");
        assert!(err.contains("2026-10-16") && err.contains("-20"), "{err}");
    }

    #[test]
    fn an_edited_cell_paints_the_drafts_value_not_the_documents() {
        let snap = full_grid();
        let mut draft = Draft::default();
        draft.set(
            (0, 1),
            ("2026-10-16".into(), "-1".into()),
            0.9,
            "2026-09-12T14:00:00Z",
        );
        let model = MatrixModel::build(&snap, &CVI, &draft).expect("a complete grid");

        let cell = &model.rows[0].cells[1];
        assert_eq!(cell.text.to_string(), "0.9000");
        assert_eq!(cell.value, Some(0.9));
        assert!(cell.edited);
        assert!(!cell.sent, "an edit is sent only once an upload said so");
        assert_eq!(
            model.rows[0].cells[0].text.to_string(),
            "0.1000",
            "a neighbouring cell still paints the document"
        );
        assert!(!model.rows[0].cells[0].edited);
    }

    #[test]
    fn a_sent_draft_marks_its_own_cells_sent() {
        let mut draft = Draft::default();
        draft.set((1, 0), ("2026-11-20".into(), "-20".into()), 0.5, BASE);
        draft.state = DraftState::Sent;
        let model = MatrixModel::build(&full_grid(), &CVI, &draft).expect("a complete grid");
        assert!(model.rows[1].cells[0].sent);
        assert!(model.rows[1].cells[0].edited);
        assert!(!model.rows[1].cells[1].sent);
    }

    #[test]
    fn a_null_value_paints_blank_not_zero() {
        let mut cells = Vec::new();
        for term in TERMS {
            for node in NODES {
                cells.push((term.to_string(), node, None));
            }
        }
        let model =
            MatrixModel::build(&document(&cells), &CVI, &Draft::default()).expect("a full grid");
        assert_eq!(model.rows[0].cells[0].text.to_string(), "");
        assert_eq!(
            model.rows[0].cells[0].value, None,
            "NULL and 0.0 are different answers (§6.3)"
        );
    }

    /// A schedule-shaped panel: one row per document row, the value
    /// columns laid flat.
    const SCHEDULE: PanelSpec = PanelSpec {
        kind: "sched",
        title: "Dividends",
        dataset: "div_schedule",
        document: "div_schedule",
        rows: "ex_date",
        columns: Columns::Values,
        header: &["currency"],
        format: ColumnFormat::MEASURE,
    };

    fn schedule() -> Snapshot {
        let dates = ["2026-10-16", "2026-11-20", "2026-12-18"];
        Snapshot::for_tests_with_provenance(
            vec![
                (
                    meta("underlying_ref", Attribution::Additive),
                    TestColumn::Dict(vec![Some("SPX.Z".into()); 3]),
                ),
                (
                    meta("ex_date", Attribution::Additive),
                    TestColumn::Dict(dates.iter().map(|d| Some((*d).to_string())).collect()),
                ),
                (
                    meta("gross", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(vec![Some(1.5), Some(2.5), Some(3.5)]),
                ),
                (
                    meta("net", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(vec![Some(1.0), Some(2.0), Some(3.0)]),
                ),
                (
                    meta("currency", Attribution::Additive),
                    TestColumn::Dict(vec![Some("USD".into()); 3]),
                ),
            ],
            0,
            provenance(BASE),
        )
    }

    #[test]
    fn values_columns_lay_the_documents_value_columns_flat() {
        let model =
            MatrixModel::build(&schedule(), &SCHEDULE, &Draft::default()).expect("a flat document");
        assert_eq!(model.rows.len(), 3, "one row per document row");
        assert_eq!(columns_of(&model), vec!["gross", "net"]);
        assert_eq!(model.key, vec!["SPX.Z".to_string()]);
        assert_eq!(model.rows[1].label.to_string(), "2026-11-20");
        assert_eq!(labels(&model, 1), vec!["2.50", "2.00"]);
        assert_eq!(model.rows[1].cells[0].cell_ref, (1, 0));
        assert_eq!(
            model
                .header
                .iter()
                .map(|(l, v)| (l.to_string(), v.to_string()))
                .collect::<Vec<_>>(),
            vec![("currency".to_string(), "USD".to_string())]
        );
    }

    #[test]
    fn an_edit_lands_on_the_right_value_column_when_the_columns_are_flat() {
        let mut draft = Draft::default();
        draft.set((2, 1), ("2026-12-18".into(), "net".into()), 9.0, BASE);
        let model = MatrixModel::build(&schedule(), &SCHEDULE, &draft).expect("a flat document");
        assert_eq!(labels(&model, 2), vec!["3.50", "9.00"]);
        assert!(model.rows[2].cells[1].edited);
        assert!(!model.rows[2].cells[0].edited);
    }

    #[test]
    fn empty_has_no_rows_and_carries_the_key() {
        let model = MatrixModel::empty(&CVI, &["SPX.Z".to_string()]);
        assert!(model.rows.is_empty());
        assert!(model.columns.is_empty());
        assert!(model.header.is_empty());
        assert_eq!(model.source_time, None);
        assert_eq!(model.key, vec!["SPX.Z".to_string()]);
    }

    #[test]
    fn a_document_with_no_rows_builds_the_no_document_shape() {
        let model = MatrixModel::build(&document(&[]), &CVI, &Draft::default())
            .expect("an empty result is not an error — the key may simply be unknown");
        assert!(model.rows.is_empty());
        assert!(model.columns.is_empty());
    }

    #[test]
    fn label_of_answers_the_row_and_column_labels_and_never_panics() {
        let model = MatrixModel::build(&full_grid(), &CVI, &Draft::default()).expect("a full grid");
        let (row, col) = model.label_of((1, 2));
        assert_eq!(row.to_string(), "2026-11-20");
        assert_eq!(col.to_string(), "3.5");
        let (row, col) = model.label_of((9, 9));
        assert_eq!(row.to_string(), "");
        assert_eq!(col.to_string(), "");
    }

    proptest! {
        /// The pivot is a bijection between the document's rows and the
        /// grid's cells: T×N cells out, every value in exactly once.
        /// A pivot that lost a value would paint a blank where a real
        /// number is, and one that duplicated a value would show a
        /// number in a cell it does not belong to — the two failures
        /// §6.3 exists to forbid, and neither is visible in a spot check
        /// of one shape.
        #[test]
        fn a_pivot_neither_loses_nor_duplicates_a_cell(t in 1usize..7, n in 1usize..7) {
            let terms: Vec<String> = (0..t).map(|i| format!("2026-{:02}-01", i + 1)).collect();
            let nodes: Vec<f64> = (0..n).map(|j| j as f64 / 4.0 - 5.0).collect();
            let mut cells = Vec::new();
            let mut source = Vec::new();
            for (i, term) in terms.iter().enumerate() {
                for (j, node) in nodes.iter().enumerate() {
                    let value = (i * n + j + 1) as f64 / 8.0;
                    source.push(value);
                    cells.push((term.clone(), *node, Some(value)));
                }
            }
            let model = MatrixModel::build(&document(&cells), &CVI, &Draft::default())
                .expect("a complete grid pivots");
            prop_assert_eq!(model.rows.len(), t);
            prop_assert_eq!(model.columns.len(), n);
            let painted: Vec<f64> = model
                .rows
                .iter()
                .flat_map(|r| r.cells.iter().map(|c| c.value.expect("every cell has a value")))
                .collect();
            prop_assert_eq!(painted.len(), t * n);
            let mut painted_sorted = painted;
            painted_sorted.sort_by(f64::total_cmp);
            source.sort_by(f64::total_cmp);
            prop_assert_eq!(painted_sorted, source);
        }
    }
}
