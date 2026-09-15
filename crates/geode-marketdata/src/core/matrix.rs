//! The prepared grid a frame paints (market-data spec §8.2).
//!
//! Built ONCE per delivery or draft change, never in `render`: every cell
//! is already formatted text in a `SharedString`, so a frame clones
//! refcounts and formats nothing (PHILOSOPHY §6, spec §7.1's 8 ms pure-UI
//! budget). gpui-component's table (`crate::delegate::MatrixDelegate`,
//! user ruling 2026-09-14) then lays out only the visible rows.
//!
//! Two shapes, one model. `Columns::Axis` pivots: the grid is (row axis ×
//! column axis) and the single value column fills it. `Columns::Values`
//! flattens: one row per document row, one column per value column. Both
//! answer the same `Cell`, so the tile's cursor, yank and edit paths know
//! only about a grid.

use crate::core::draft::{Draft, attr_text};
use crate::core::spec::{Columns, PanelSpec};
use geode_core::attribution::Attribution;
use geode_core::format::format_number;
use geode_core::snapshot::Snapshot;
use gpui::SharedString;
use std::collections::HashMap;

/// One header attribute as painted: prepared text, and whether the draft
/// has overridden it — `true` exactly when [`header_of`] found a
/// [`Draft`] entry for this column's own attribute edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderCell {
    pub column: SharedString,
    pub label: SharedString,
    pub text: SharedString,
    pub edited: bool,
}

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
    /// Header attributes the spec names, in spec order.
    pub header: Vec<HeaderCell>,
    pub columns: Vec<SharedString>,
    pub rows: Vec<RowModel>,
}

impl MatrixModel {
    /// Pivot or flatten `snapshot` per `spec`, with `draft`'s edits
    /// painted over the document's own values.
    ///
    /// `Err` is a document that cannot be laid out as a grid at all: a
    /// missing axis column, no value column (or more than one under
    /// `Columns::Axis`), a blank axis or key cell, a repeated row label,
    /// or — for a pivot — a hole or a repeated (row, column) pair. A hole
    /// is deliberately an error rather than a blank cell: the axes say the
    /// document claims a value there, so a blank would be this model
    /// inventing the §6.3 claim "this number does not belong to this row"
    /// on the document's behalf, and a zero would be worse.
    ///
    /// Between them those refusals are what make the model's row labels
    /// unique and its column labels unique — the invariant
    /// [`Draft`]'s own doc states and [`Draft::rebase`] resolves edits
    /// against.
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
        let key = key_of(snapshot, spec, rows_idx)?;
        let header = header_of(snapshot, spec, draft);
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
///
/// A NULL key cell is an error, not a shorter key: the key is what the
/// panel asked for and what the header shows it is displaying, so
/// dropping a part of it would leave a two-part document reading as a
/// one-part one — `["SPX.Z"]` where the truth is `["SPX.Z", <nothing>]`.
fn key_of(snapshot: &Snapshot, spec: &PanelSpec, rows_idx: usize) -> Result<Vec<String>, String> {
    (0..rows_idx)
        .filter(|i| {
            snapshot
                .meta_at(*i)
                .is_some_and(|m| !spec.names(&m.name) && !is_value(snapshot, *i))
        })
        .map(|i| {
            let name = snapshot
                .meta_at(i)
                .map_or(String::new(), |m| m.name.clone());
            required_label(snapshot, i, 0, &name)
        })
        .collect()
}

/// The header attributes the spec names, read off row 0 — a document-level
/// attribute is constant within one document (spec §3.1), so any row would
/// do. An attribute the document does not carry is left out rather than
/// shown blank: the panel says what it has. Unlike an axis or a key cell,
/// a missing attribute identifies nothing, so there is nothing for it to
/// corrupt — it is display, and absent display is absence.
///
/// The draft's own value paints over a NULL or a real one alike, marked
/// `edited` — the same rule [`cell_of`] applies to a grid cell — so a
/// document that carries the column but a NULL row 0 no longer forces
/// `label_at` to succeed before an edit can be seen at all.
fn header_of(snapshot: &Snapshot, spec: &PanelSpec, draft: &Draft) -> Vec<HeaderCell> {
    spec.header
        .iter()
        .filter_map(|attr| {
            let idx = snapshot.column_index(attr.column)?;
            let (text, edited) = match draft.attrs.get(attr.column) {
                Some(value) => (attr_text(value), true),
                None => (label_at(snapshot, idx, 0)?, false),
            };
            Some(HeaderCell {
                column: attr.column.into(),
                label: attr.label.into(),
                text: text.into(),
                edited,
            })
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

/// One label that identifies a row, refusing a blank.
///
/// A NULL axis or key cell has no honest rendering here. `""` is not one:
/// two different rows whose axis value is missing would fold into a
/// single label, so a pivot would report them as a repeat (or, for the
/// flat shape, collapse two schedule rows into one) and a draft keyed by
/// that label could not tell them apart. The document is malformed —
/// `publish_document` requires every axis value — so the panel says so
/// rather than inventing a row identity.
fn required_label(
    snapshot: &Snapshot,
    idx: usize,
    row: usize,
    column: &str,
) -> Result<String, String> {
    label_at(snapshot, idx, row).ok_or_else(|| {
        format!(
            "the document has no '{column}' value on row {row}: an axis or key cell \
             identifies a row and cannot be blank"
        )
    })
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
        let row_label = required_label(snapshot, rows_idx, row, spec.rows)?;
        let col_label = required_label(snapshot, col_idx, row, axis)?;
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
    // Exactly one value column, never "the first of several": a pivot
    // spends both of its axes on the document's own axes, so a second
    // value has nowhere to go. Taking the first silently and dropping the
    // rest would paint a grid that looks complete and is missing a
    // column's worth of numbers — a panel over such a dataset wants
    // `Columns::Values`, or a spec that names which value it pivots.
    let values = value_columns(snapshot);
    let value_idx = match values.as_slice() {
        [] => {
            return Err(format!(
                "the document '{}' has no value column",
                spec.dataset
            ));
        }
        [one] => *one,
        many => {
            let names = many
                .iter()
                .filter_map(|i| snapshot.meta_at(*i))
                .map(|m| format!("'{}'", m.name))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "a pivot on '{axis}' fills one value per cell, but the document \
                 '{}' declares {} value columns: {names}",
                spec.dataset,
                many.len()
            ));
        }
    };

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
    // A row label must name exactly one row, the same rule `index_grid`
    // applies to a pivot's (row, column) pair and for the same reason: a
    // draft resolves its edits by label across generations, so a repeated
    // label makes two different rows one target. One defence, here at the
    // model boundary, is what lets `Draft::rebase` index the labels
    // without a collision check of its own.
    let mut seen: HashMap<String, usize> = HashMap::with_capacity(snapshot.rows());
    let mut rows = Vec::with_capacity(snapshot.rows());
    for row in 0..snapshot.rows() {
        let label = required_label(snapshot, rows_idx, row, spec.rows)?;
        if let Some(previous) = seen.insert(label.clone(), row) {
            return Err(format!(
                "the document repeats {}='{label}' (rows {previous} and {row}): a row \
                 label identifies an edit, so it must name one row",
                spec.rows
            ));
        }
        rows.push(RowModel {
            label: SharedString::from(label),
            cells: value_idxs
                .iter()
                .enumerate()
                .map(|(ci, &idx)| cell_of(snapshot, idx, row, (row, ci), spec, draft))
                .collect(),
        });
    }
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
    use crate::core::spec::{CVI, Columns, HeaderAttr, PanelSpec};
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::document::Value;
    use geode_core::schema::ColumnType;
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

        let header: Vec<(String, String, String)> = model
            .header
            .iter()
            .map(|h| {
                (
                    h.column.to_string(),
                    h.label.to_string(),
                    h.text.to_string(),
                )
            })
            .collect();
        assert_eq!(
            header,
            vec![
                ("anchor_date".into(), "anchor".into(), "2026-09-12".into()),
                ("spot_ref".into(), "spot".into(), "5000".into()),
            ],
            "the header reads the document-level attributes off row 0"
        );
        assert!(model.header.iter().all(|h| !h.edited));
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
    fn an_edited_attribute_paints_the_drafts_value_marked_edited() {
        let snapshot = full_grid();
        let mut draft = Draft::default();
        draft.set_attr("spot_ref", Value::F64(4520.0), "t0");
        let model = MatrixModel::build(&snapshot, &CVI, &draft).unwrap();
        let spot = model
            .header
            .iter()
            .find(|h| h.column == "spot_ref")
            .unwrap();
        assert_eq!((spot.text.as_ref(), spot.edited), ("4520", true));
        let anchor = model
            .header
            .iter()
            .find(|h| h.column == "anchor_date")
            .unwrap();
        assert!(!anchor.edited);
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
        header: &[HeaderAttr {
            column: "currency",
            label: "currency",
            ty: ColumnType::Utf8,
        }],
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
    };

    fn schedule() -> Snapshot {
        schedule_dated(&["2026-10-16", "2026-11-20", "2026-12-18"])
    }

    /// The same three-row schedule with the row axis spelled by the caller,
    /// so a repeat can be delivered.
    fn schedule_dated(dates: &[&str; 3]) -> Snapshot {
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
        let header: Vec<(String, String, String)> = model
            .header
            .iter()
            .map(|h| {
                (
                    h.column.to_string(),
                    h.label.to_string(),
                    h.text.to_string(),
                )
            })
            .collect();
        assert_eq!(
            header,
            vec![("currency".into(), "currency".into(), "USD".into())]
        );
        assert!(model.header.iter().all(|h| !h.edited));
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

    #[test]
    fn a_repeated_row_label_is_refused_when_the_columns_are_flat() {
        // Two schedule rows on one date: a draft resolves an edit by label,
        // so one label naming two rows would make two edits one — the
        // reviewer's case, where `rebase` silently kept one and reported
        // nothing dropped.
        let err = MatrixModel::build(
            &schedule_dated(&["2026-10-16", "2026-10-16", "2026-12-18"]),
            &SCHEDULE,
            &Draft::default(),
        )
        .expect_err("a row label must name one row");
        assert!(err.contains("2026-10-16"), "{err}");
        assert!(err.contains("ex_date"), "{err}");
    }

    #[test]
    fn a_pivot_refuses_a_document_with_more_than_one_value_column() {
        // The schedule's two value columns pivoted on its own row axis:
        // both cannot fit one cell, and filling from the first would hide
        // a whole column of numbers.
        const PIVOTED: PanelSpec = PanelSpec {
            kind: "sched",
            title: "Dividends",
            dataset: "div_schedule",
            document: "div_schedule",
            rows: "ex_date",
            columns: Columns::Axis("currency"),
            header: &[],
            value_type: ColumnType::F64,
            format: ColumnFormat::MEASURE,
        };
        let err = MatrixModel::build(&schedule(), &PIVOTED, &Draft::default())
            .expect_err("one value per cell");
        assert!(err.contains("gross") && err.contains("net"), "{err}");
        assert!(err.contains("currency"), "{err}");
    }

    #[test]
    fn a_blank_axis_cell_is_refused_rather_than_labelled_with_nothing() {
        let mut cells = Vec::new();
        for (t, term) in TERMS.iter().enumerate() {
            for node in NODES {
                cells.push(((*term).to_string(), node, Some(t as f64)));
            }
        }
        let snap = document(&cells);
        // Rebuild the same document with one NULL in the row axis.
        let mut terms: Vec<Option<String>> = cells.iter().map(|c| Some(c.0.clone())).collect();
        terms[3] = None;
        let holed = Snapshot::for_tests_with_provenance(
            vec![
                (
                    meta("underlying_ref", Attribution::Additive),
                    TestColumn::Dict(vec![Some("SPX.Z".into()); cells.len()]),
                ),
                (meta("term", Attribution::Additive), TestColumn::Dict(terms)),
                (
                    meta("node", Attribution::Additive),
                    TestColumn::F64(cells.iter().map(|c| Some(c.1)).collect()),
                ),
                (
                    meta("param", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(cells.iter().map(|c| c.2).collect()),
                ),
            ],
            0,
            provenance(BASE),
        );
        assert!(
            MatrixModel::build(&snap, &CVI, &Draft::default()).is_ok(),
            "the same document without the NULL builds"
        );
        let err = MatrixModel::build(&holed, &CVI, &Draft::default())
            .expect_err("a blank axis cell identifies no row");
        assert!(err.contains("term"), "{err}");
    }

    #[test]
    fn a_blank_key_cell_is_refused_rather_than_shortening_the_key() {
        let mut keys: Vec<Option<String>> = vec![Some("SPX.Z".into()); 6];
        keys[0] = None;
        let mut cells = Vec::new();
        for (t, term) in TERMS.iter().enumerate() {
            for node in NODES {
                cells.push(((*term).to_string(), node, Some(t as f64)));
            }
        }
        let snap = Snapshot::for_tests_with_provenance(
            vec![
                (
                    meta("underlying_ref", Attribution::Additive),
                    TestColumn::Dict(keys),
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
            ],
            0,
            provenance(BASE),
        );
        let err = MatrixModel::build(&snap, &CVI, &Draft::default())
            .expect_err("a one-part key that reads as no parts is a lie about the document");
        assert!(err.contains("underlying_ref"), "{err}");
    }

    proptest! {
        /// The pivot is a bijection between the document's rows and the
        /// grid's cells, and it is POSITIONAL: cell (i, j) holds the value
        /// the document sent for (row label i, column label j).
        ///
        /// Asserted positionally rather than as a multiset, because a
        /// transposed or rotated grid has exactly the same multiset of
        /// values as a correct one — every value present exactly once, and
        /// every one of them in the wrong cell. The document is shuffled
        /// first (a real one arrives in axis order, so an implementation
        /// that leaned on that would pass every ordered fixture), which
        /// also makes first-appearance order something the test has to
        /// derive rather than assume.
        #[test]
        fn a_pivot_is_a_positional_bijection(t in 1usize..7, n in 1usize..7, seed in any::<u64>()) {
            let terms: Vec<String> = (0..t).map(|i| format!("2026-{:02}-01", i + 1)).collect();
            let nodes: Vec<f64> = (0..n).map(|j| j as f64 / 4.0 - 5.0).collect();
            let mut cells = Vec::new();
            for (i, term) in terms.iter().enumerate() {
                for (j, node) in nodes.iter().enumerate() {
                    cells.push((term.clone(), *node, Some((i * n + j + 1) as f64 / 8.0)));
                }
            }
            // A seeded Fisher-Yates: deterministic per case, and proptest
            // shrinks the seed like any other input.
            let mut state = seed | 1;
            for i in (1..cells.len()).rev() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let j = (state >> 33) as usize % (i + 1);
                cells.swap(i, j);
            }

            // What the document now says, read independently of the model:
            // first-appearance label order per axis, and the value at each pair.
            let mut want_rows: Vec<String> = Vec::new();
            let mut want_columns: Vec<String> = Vec::new();
            let mut want_value: HashMap<(String, String), f64> = HashMap::new();
            for (term, node, value) in &cells {
                let column = format!("{node}");
                if !want_rows.contains(term) {
                    want_rows.push(term.clone());
                }
                if !want_columns.contains(&column) {
                    want_columns.push(column.clone());
                }
                want_value.insert((term.clone(), column), value.expect("a value"));
            }

            let model = MatrixModel::build(&document(&cells), &CVI, &Draft::default())
                .expect("a complete grid pivots");
            prop_assert_eq!(
                model.rows.iter().map(|r| r.label.to_string()).collect::<Vec<_>>(),
                want_rows.clone()
            );
            prop_assert_eq!(columns_of(&model), want_columns.clone());
            for (i, row) in model.rows.iter().enumerate() {
                prop_assert_eq!(row.cells.len(), n);
                for (j, cell) in row.cells.iter().enumerate() {
                    let want = want_value[&(want_rows[i].clone(), want_columns[j].clone())];
                    prop_assert_eq!(cell.value, Some(want));
                    prop_assert_eq!(cell.cell_ref, (i, j));
                }
            }
        }
    }
}
