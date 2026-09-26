//! Prepared pivot or flat grids for the market-data table.
//!
//! Build on delivery or structural draft changes; patch individual cell
//! edits. Cells carry formatted `SharedString`s so rendering clones text
//! without reformatting, and the table lays out only visible rows.
//!
//! `Columns::Axis` pivots one value onto a row-by-column grid with leading
//! slice columns. `Columns::Values` lays out the declared value columns.
//! Both expose the same cells to cursor, copy, and edit operations.

use crate::core::draft::{Draft, RowEdit, attr_text};
use crate::core::spec::{Columns, PanelSpec, ValueColumn};
use geode_core::attribution::Attribution;
use geode_core::document::Value;
use geode_core::format::format_number;
use geode_core::schema::ColumnType;
use geode_core::snapshot::Snapshot;
use geode_core::view::ColumnFormat;
use gpui::SharedString;
use std::collections::{BTreeMap, HashMap, HashSet};

/// A column's display and editor kind, parallel to [`MatrixModel::columns`].
/// Pivot ladder and slice columns are numeric. Flat columns use their
/// declared type and optional choice vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub enum CellKind {
    Number(ColumnFormat),
    Date,
    Text,
    Choice(&'static [&'static str]),
}

/// The one formatter a [`Cell`]'s text is ever built through — the
/// document's own value and an edited one alike, so a typed edit paints
/// in exactly the shape the document would have.
///
/// A number in a non-number column (and vice versa) is spelled plainly
/// rather than through a format the column does not have: the mismatch
/// itself is a defect elsewhere (a spec whose `ValueColumn::ty` disagrees
/// with what it reads), and this function's job is to paint something
/// honest, not to hide that.
pub fn cell_text(value: &Value, kind: &CellKind) -> String {
    match (kind, value) {
        (CellKind::Number(format), Value::F64(v)) => format_number(*v, format).text,
        (CellKind::Number(format), Value::I64(v)) => format_number(*v as f64, format).text,
        (_, Value::Date(d)) => d.format("%Y-%m-%d").to_string(),
        (_, Value::Utf8(s)) => s.clone(),
        (_, Value::F64(v)) => format!("{v}"),
        (_, Value::I64(v)) => v.to_string(),
    }
}

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
    /// Prepared display text. A missing document value paints blank;
    /// a missing inserted value paints [`UNFILLED`], distinct from zero.
    pub text: SharedString,
    pub value: Option<Value>,
    pub edited: bool,
    pub sent: bool,
    /// The edit key: base-document row and model column for a document cell,
    /// even when inserted rows shift its painted position. Inserted cells use
    /// their painted position here, but store values in `RowEdit.cells` by
    /// column label rather than in `Draft::edits`.
    pub cell_ref: (usize, usize),
}

/// Row origin and deletion state. Inserted rows follow their anchors;
/// deleted document rows remain visible but upload assembly excludes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Document,
    Inserted,
    Deleted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowModel {
    pub label: SharedString,
    pub cells: Vec<Cell>,
    pub state: RowState,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatrixModel {
    /// The document key, in `document_columns()` order.
    pub key: Vec<String>,
    /// Per-document source time from the first provenance dataset's `as_of`,
    /// in RFC 3339. Draft identity compares this timestamp, not `gen_id`.
    pub source_time: Option<String>,
    /// Header attributes the spec names, in spec order.
    pub header: Vec<HeaderCell>,
    /// The slice-value labels first (`slice_columns` of them), then the
    /// ladder.
    pub columns: Vec<SharedString>,
    /// Parallel to `columns`: each column's [`CellKind`], so a cell's own
    /// column says how it paints and how a typed edit to it is parsed —
    /// a pivot's are always `Number`, a flat panel's follow each
    /// [`ValueColumn::ty`].
    pub column_kinds: Vec<CellKind>,
    /// How many of `columns` (and of every row's leading `cells`) are the
    /// spec's per-slice values rather than the pivot's own ladder — what
    /// lets a row bump skip them and the delegate rule them off.
    pub slice_columns: usize,
    /// The column axis's own TYPED value per ladder column (parallel to
    /// `columns[slice_columns..]`), read off the column's first document
    /// row; empty for the flat shape. What an upload writes into the
    /// long form's column axis — `columns` is painted text, and parsing
    /// `-20` back into a number would make the label's spelling the
    /// wire's value.
    ///
    /// A `Snapshot` declares no type, so the value is typed by what the
    /// column arrived as: an integer column `I64`, any other number
    /// `F64`, anything else `Utf8`. A dataset declaring a date column
    /// axis would therefore read `Utf8` here, and the kind's own type
    /// check refuses the upload by name rather than sending a string.
    pub column_values: Vec<Value>,
    pub rows: Vec<RowModel>,
    /// Snapshot positions for pivot cells, retained so a cell patch does not
    /// re-index the document. Flat grids use the base row index and spec's
    /// column list directly.
    pub pivot_index: Option<PivotIndex>,
}

/// The pivot's (grid row, ladder column) → snapshot row map, plus the
/// snapshot columns the ladder and each slice column are read from —
/// everything [`pivot`] had in hand when it filled the grid, kept so a
/// patch reads the same cell of the same document the build did. Indices
/// into the snapshot the model was built from; meaningless against any
/// other.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PivotIndex {
    /// The one value column's snapshot index.
    value_idx: usize,
    /// Per slice column (in `columns` order), the snapshot column it is
    /// read from.
    slice_idx: Vec<usize>,
    /// Per grid row, the first snapshot row of its slice — where a slice
    /// value is read.
    first_row: Vec<usize>,
    /// The ladder's width — the stride of `at`.
    ladder: usize,
    /// Row-major `rows × ladder`: the snapshot row holding the value for
    /// (grid row, ladder column).
    at: Vec<usize>,
}

impl PivotIndex {
    /// The snapshot column the ladder's one value is read from — how an
    /// upload names the value column it writes the ladder into.
    pub(crate) fn value_idx(&self) -> usize {
        self.value_idx
    }
}

impl MatrixModel {
    /// The [`CellKind`] a column paints and edits through, if `col` is in
    /// range.
    pub fn kind_of(&self, col: usize) -> Option<&CellKind> {
        self.column_kinds.get(col)
    }

    /// Build a grid with draft values over the delivered document.
    ///
    /// Refuse missing axes or values, unreadable key/axis cells, repeated flat
    /// row labels, and malformed pivots: multiple value columns, repeated
    /// pairs, grid holes, inconsistent slice values, or axis/slice label
    /// collisions. A grid hole differs from an existing cell containing NULL;
    /// inventing a blank or zero would hide a missing document row.
    ///
    /// Compiled specs must provide unique flat and slice labels. Together with
    /// validated document identities, these let rebase resolve edits by label.
    /// Apply row edits last: deleted rows retain a marker, inserts follow their
    /// anchors, and document cells keep their original `cell_ref` positions.
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
            .column_index(spec.rows.column)
            .ok_or_else(|| format!("the document has no '{}' column", spec.rows.column))?;
        let key = key_of(snapshot, spec, rows_idx)?;
        let header = header_of(snapshot, spec, draft);
        let (columns, column_kinds, slice_columns, column_values, rows, pivot_index) =
            match &spec.columns {
                Columns::Axis(axis) => {
                    let (columns, column_kinds, slice_columns, column_values, rows, index) =
                        pivot(snapshot, spec, draft, rows_idx, axis)?;
                    (
                        columns,
                        column_kinds,
                        slice_columns,
                        column_values,
                        rows,
                        Some(index),
                    )
                }
                Columns::Values(_) => {
                    let (columns, column_kinds, rows) = flatten(snapshot, spec, draft, rows_idx)?;
                    (columns, column_kinds, 0, Vec::new(), rows, None)
                }
            };
        let rows = splice_rows(rows, draft, &columns, &column_kinds);
        Ok(MatrixModel {
            key,
            source_time,
            header,
            columns,
            column_kinds,
            slice_columns,
            column_values,
            rows,
            pivot_index,
        })
    }

    /// Reprepare one cell's text, value, edited flag, and sent flag using the
    /// same rules as a build. A cell commit then pays for one formatter call
    /// instead of reformatting the whole grid.
    ///
    /// `snapshot` must be the document used to build this model: pivot indices
    /// and document `cell_ref`s address that snapshot. `row` is a painted
    /// model position; its cell reference resolves any shift from inserted
    /// rows. Inserted values come from `RowEdit.cells` by label.
    ///
    /// Return `false` if the position or required mapping is absent, including
    /// an inserted row removed from the draft. The caller then rebuilds.
    pub fn patch_cell(
        &mut self,
        row: usize,
        col: usize,
        snapshot: &Snapshot,
        spec: &PanelSpec,
        draft: &Draft,
    ) -> bool {
        let Some(kind) = self.column_kinds.get(col) else {
            return false;
        };
        let Some(current) = self.rows.get(row) else {
            return false;
        };
        if current.state == RowState::Inserted {
            let Some(RowEdit::Inserted { cells, .. }) = draft.rows.get(current.label.as_ref())
            else {
                return false;
            };
            let Some(label) = self.columns.get(col) else {
                return false;
            };
            self.rows[row].cells[col] =
                inserted_cell(cells.get(label.as_ref()), (row, col), kind, draft);
            return true;
        }
        let Some(doc_row) = current.cells.get(col).map(|c| c.cell_ref.0) else {
            return false;
        };
        let value = match &self.pivot_index {
            Some(index) => match col.checked_sub(self.slice_columns) {
                // A ladder cell: the value column at the snapshot row the
                // build's own grid map recorded for this pair.
                Some(ci) => {
                    let Some(&srow) = index.at.get(doc_row * index.ladder + ci) else {
                        return false;
                    };
                    snapshot.f64_at(index.value_idx, srow).map(Value::F64)
                }
                // A slice cell: read off its slice's first row, as
                // `pivot` reads it (the whole slice was checked to
                // agree at build time, and nothing since has changed
                // the document).
                None => {
                    let (Some(&idx), Some(&first)) =
                        (index.slice_idx.get(col), index.first_row.get(doc_row))
                    else {
                        return false;
                    };
                    snapshot.f64_at(idx, first).map(Value::F64)
                }
            },
            // The flat shape: document row `doc_row` is snapshot row
            // `doc_row`, and grid column `col` is the spec's `col`th flat
            // column — the same resolution `flatten` made, redone for one
            // column.
            None => {
                let Some(vc) = spec.flat_columns().get(col) else {
                    return false;
                };
                let Some(idx) = snapshot.column_index(vc.column) else {
                    return false;
                };
                read_flat_value(snapshot, idx, doc_row, vc.ty)
            }
        };
        self.rows[row].cells[col] = cell_of(value, (doc_row, col), kind, draft);
        true
    }

    /// The "no document received" shape: the key the panel asked about
    /// and nothing else. Distinct from a built model only in having no
    /// rows — the tile's header says which.
    pub fn empty(spec: &PanelSpec, key: &[String]) -> MatrixModel {
        // Keep the same spec-bearing interface as `build`; an empty model
        // currently carries only the requested key.
        let _ = spec;
        MatrixModel {
            key: key.to_vec(),
            ..MatrixModel::default()
        }
    }

    /// The row and column labels used to identify an edit across generations.
    /// Each out-of-range coordinate returns an empty label independently; a
    /// cursor can outlive the model against which it was positioned.
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

/// Read key columns preceding the row axis, excluding spec-named columns
/// and attributed values. This relies on `document_columns()` emitting
/// keys first and the panel choosing the dataset's first axis as rows.
///
/// Reject unreadable key cells rather than shortening a composite key and
/// making the header identify a different document.
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

/// Read declared header attributes from row 0, relying on the document
/// contract that attributes are constant across rows. Omit absent columns
/// and unreadable values. A draft override paints even over a NULL value,
/// but still requires the attribute column to exist in the snapshot.
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

/// The document's CELL values: every value column the spec does not name
/// as a per-slice value. A slice value (CVI's `forward`) arrives
/// `DeterminedNonAdditive` exactly as `param` does — it is a value the
/// dataset declares — so without this filter a pivot would refuse the
/// document as one with four value columns, and a flat panel would lay a
/// per-slice value out as a cell column.
fn value_columns(snapshot: &Snapshot, spec: &PanelSpec) -> Vec<usize> {
    (0..snapshot.columns())
        .filter(|i| {
            is_value(snapshot, *i)
                && snapshot
                    .meta_at(*i)
                    .is_none_or(|m| spec.slice_value(&m.name).is_none())
        })
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

/// Read an axis or key label, refusing NULL or unreadable values instead
/// of inventing an identity that could merge distinct rows. A present empty
/// string is still a label here; this function does not validate its syntax.
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
    /// Per column label (parallel to `columns`), the first snapshot row
    /// carrying it — where the column axis's typed value is read.
    column_first: Vec<usize>,
    at: HashMap<String, HashMap<String, usize>>,
    /// Each snapshot row's index into `rows` — the slice it belongs to,
    /// so a second pass over the document (the slice values) allocates
    /// no label.
    row_of: Vec<usize>,
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
        column_first: Vec::new(),
        at: HashMap::new(),
        row_of: Vec::with_capacity(snapshot.rows()),
    };
    // Membership sets beside the order vectors: `Vec::contains` per
    // document row is quadratic in the label count, and a schedule-shaped
    // document has thousands.
    let mut seen_rows: HashMap<String, usize> = HashMap::new();
    let mut seen_cols: HashMap<String, ()> = HashMap::new();
    for row in 0..snapshot.rows() {
        let row_label = required_label(snapshot, rows_idx, row, spec.rows.column)?;
        let col_label = required_label(snapshot, col_idx, row, axis)?;
        let ri = match seen_rows.entry(row_label.clone()) {
            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
            std::collections::hash_map::Entry::Vacant(e) => {
                grid.rows.push(row_label.clone());
                *e.insert(grid.rows.len() - 1)
            }
        };
        grid.row_of.push(ri);
        if seen_cols.insert(col_label.clone(), ()).is_none() {
            grid.columns.push(col_label.clone());
            grid.column_first.push(row);
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
                spec.rows.column
            ));
        }
    }
    Ok(grid)
}

/// The pivot. The spec's slice values first, then the column labels
/// across the top in document order; row labels down the side in
/// document order, the one value column in the ladder's cells.
///
/// A slice value is read off its slice's first row and the rest of the
/// slice must agree: the long form repeats it per node row, so two
/// different forwards under one term is the document contradicting
/// itself, refused like a hole rather than averaged or first-wins — a
/// grid that painted either would look complete and be wrong. A slice
/// value the document does not carry is left out, as a header attribute
/// is: it identifies nothing, so absent display is absence.
///
/// The order is the document's own, first appearance first, and is NOT
/// sorted: a term ladder and a node ladder arrive in the order the desk
/// means them to be read (`compile_document` selects `order by` the axes),
/// and sorting the labels here would silently reshuffle a trader's grid —
/// lexically, at that, so `-20` would land between `-1` and `3.5`.
/// `Grid::at` is keyed by the labels themselves rather than by their
/// indices precisely so that this ordering decision is separable from
/// where the values come from.
/// `pivot`'s own result: the column headers (slice labels then the
/// ladder), each one's [`CellKind`] in the same order, how many of the
/// two leading vecs are slice columns, the built rows, and the
/// [`PivotIndex`] a later [`MatrixModel::patch_cell`] reads.
type PivotResult = Result<
    (
        Vec<SharedString>,
        Vec<CellKind>,
        usize,
        Vec<Value>,
        Vec<RowModel>,
        PivotIndex,
    ),
    String,
>;

fn pivot(
    snapshot: &Snapshot,
    spec: &PanelSpec,
    draft: &Draft,
    rows_idx: usize,
    axis: &str,
) -> PivotResult {
    let col_idx = snapshot
        .column_index(axis)
        .ok_or_else(|| format!("the document has no '{axis}' column"))?;
    // Exactly one value column, never "the first of several": a pivot
    // spends both of its axes on the document's own axes, so a second
    // value has nowhere to go. Taking the first silently and dropping the
    // rest would paint a grid that looks complete and is missing a
    // column's worth of numbers — a panel over such a dataset wants
    // `Columns::Values`, or a spec that names which value it pivots.
    let values = value_columns(snapshot, spec);
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
    // The pivot's one cell column must be numeric: `display_at` reads
    // text, dictionaries, dates, timestamps and bools and deliberately
    // leaves numbers alone (`label_at`'s own rule, above), so `Some` here
    // means "this is one of the types `display_at` covers" — i.e. not a
    // number. Row 0 stands for the whole column: a document's columns are
    // uniformly typed, so one row settles it.
    if snapshot.display_at(value_idx, 0).is_some() {
        let name = snapshot
            .meta_at(value_idx)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        return Err(format!("a pivot's value column '{name}' must be numeric"));
    }

    let grid = index_grid(snapshot, spec, rows_idx, col_idx, axis)?;

    // The slice values the document carries, in spec order: the snapshot
    // column and, per slice, the first row that carries it — after the
    // whole slice has been checked to agree.
    let slices: Vec<(&crate::core::spec::SliceValue, usize)> = spec
        .slice_values
        .iter()
        .filter_map(|sv| snapshot.column_index(sv.column).map(|idx| (sv, idx)))
        .collect();
    // A slice label is a column label: `Draft` resolves edits by label and
    // indexes `columns` by it, so a node that reads `fwd` would make two
    // columns one name.
    for (sv, _) in &slices {
        if grid.columns.iter().any(|c| c == sv.label) {
            return Err(format!(
                "the slice value '{}' is labelled '{}', which is also a {axis} label; \
                 a column label must name one column",
                sv.column, sv.label
            ));
        }
    }
    let mut first_row: Vec<Option<usize>> = vec![None; grid.rows.len()];
    for (srow, &ri) in grid.row_of.iter().enumerate() {
        match first_row[ri] {
            None => first_row[ri] = Some(srow),
            Some(first) => {
                for (sv, idx) in &slices {
                    if snapshot.f64_at(*idx, first) != snapshot.f64_at(*idx, srow) {
                        return Err(format!(
                            "the document's {}='{}' rows disagree on '{}' ({} on row {first}, {} on row {srow}): a slice value is constant across its slice",
                            spec.rows.column,
                            grid.rows[ri],
                            sv.column,
                            label_at(snapshot, *idx, first).unwrap_or_default(),
                            label_at(snapshot, *idx, srow).unwrap_or_default(),
                        ));
                    }
                }
            }
        }
    }
    let slice_columns = slices.len();
    let column_values = grid
        .column_first
        .iter()
        .map(|&row| axis_value(snapshot, col_idx, row))
        .collect();

    // A slice column carries its own format (a forward at two places
    // beside `param`'s four); every ladder column shares the panel's
    // `spec.format`. Both are always `Number` — the refusal above is what
    // makes that true of the ladder, and a slice value is `f64` only by
    // the spec's own doc comment.
    let mut column_kinds: Vec<CellKind> = slices
        .iter()
        .map(|(sv, _)| CellKind::Number(sv.format.clone()))
        .collect();
    column_kinds.extend(
        std::iter::repeat_with(|| CellKind::Number(spec.format.clone())).take(grid.columns.len()),
    );

    // The index a patch reads back, filled as the grid is: one entry per
    // cell the loop below visits, in the same order.
    let mut index = PivotIndex {
        value_idx,
        slice_idx: slices.iter().map(|(_, idx)| *idx).collect(),
        first_row: Vec::with_capacity(grid.rows.len()),
        ladder: grid.columns.len(),
        at: Vec::with_capacity(grid.rows.len() * grid.columns.len()),
    };
    let mut rows = Vec::with_capacity(grid.rows.len());
    for (ri, row_label) in grid.rows.iter().enumerate() {
        let mut cells = Vec::with_capacity(slice_columns + grid.columns.len());
        // Every row label came out of `index_grid`'s pass over the
        // snapshot rows, so each has a first row.
        let first = first_row[ri].unwrap_or_default();
        index.first_row.push(first);
        for (ci, (_, idx)) in slices.iter().enumerate() {
            let value = snapshot.f64_at(*idx, first).map(Value::F64);
            cells.push(cell_of(value, (ri, ci), &column_kinds[ci], draft));
        }
        for (ci, col_label) in grid.columns.iter().enumerate() {
            match grid.at.get(row_label).and_then(|m| m.get(col_label)) {
                Some(&srow) => {
                    index.at.push(srow);
                    let value = snapshot.f64_at(value_idx, srow).map(Value::F64);
                    cells.push(cell_of(
                        value,
                        (ri, slice_columns + ci),
                        &column_kinds[slice_columns + ci],
                        draft,
                    ));
                }
                None => {
                    return Err(format!(
                        "the document has no cell for {}='{row_label}' {axis}='{col_label}'",
                        spec.rows.column
                    ));
                }
            }
        }
        rows.push(RowModel {
            label: SharedString::from(row_label.clone()),
            cells,
            state: RowState::Document,
        });
    }
    Ok((
        slices
            .iter()
            .map(|(sv, _)| SharedString::from(sv.label))
            .chain(grid.columns.into_iter().map(SharedString::from))
            .collect(),
        column_kinds,
        slice_columns,
        column_values,
        rows,
        index,
    ))
}

/// One column-axis value, typed by what the column arrived as (see
/// [`MatrixModel::column_values`]). `index_grid` has already refused a
/// blank axis cell, so the text arm always has something to read.
fn axis_value(snapshot: &Snapshot, idx: usize, row: usize) -> Value {
    if let Some(v) = snapshot.i64_at(idx, row) {
        return Value::I64(v);
    }
    if let Some(v) = snapshot.f64_at(idx, row) {
        return Value::F64(v);
    }
    Value::Utf8(snapshot.display_at(idx, row).unwrap_or_default())
}

/// The [`CellKind`] a flat [`ValueColumn`] paints and edits through: a
/// number for `F64`/`I64`, `Date` for a date, and for `Utf8` either
/// `Choice` (the column declares a fixed vocabulary) or plain `Text`.
/// `Timestamp`/`Bool` are not yet a shape this crate's typed cells cover
/// and fall back to `Text` — the same "paint something honest" rule
/// [`cell_text`] follows for a mismatch, rather than refuse a spec that
/// declares one.
fn flat_kind(vc: &ValueColumn) -> CellKind {
    match (vc.ty, vc.choices) {
        (ColumnType::F64 | ColumnType::I64, _) => CellKind::Number(vc.format.clone()),
        (ColumnType::Date, _) => CellKind::Date,
        (_, Some(choices)) => CellKind::Choice(choices),
        (ColumnType::Utf8 | ColumnType::Timestamp | ColumnType::Bool, None) => CellKind::Text,
    }
}

/// One flat cell's document value, read per its column's declared type —
/// `f64_at`/`i64_at` for a number, `display_at` for everything else
/// (a `Date32` displays as `%Y-%m-%d`, [`label_at`]'s own rule, so the
/// round trip back into a [`Value::Date`] is exact).
pub(crate) fn read_flat_value(
    snapshot: &Snapshot,
    idx: usize,
    row: usize,
    ty: ColumnType,
) -> Option<Value> {
    match ty {
        ColumnType::F64 => snapshot.f64_at(idx, row).map(Value::F64),
        ColumnType::I64 => snapshot.i64_at(idx, row).map(Value::I64),
        ColumnType::Date => snapshot
            .display_at(idx, row)
            .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
            .map(Value::Date),
        ColumnType::Utf8 | ColumnType::Timestamp | ColumnType::Bool => {
            snapshot.display_at(idx, row).map(Value::Utf8)
        }
    }
}

/// `flatten`'s own result: the column headers, each one's [`CellKind`] in
/// the same order, and the built rows.
type FlattenResult = Result<(Vec<SharedString>, Vec<CellKind>, Vec<RowModel>), String>;

/// The flat shape: one row per document row, one column per
/// [`PanelSpec::flat_columns`] entry, in the spec's own order — the paint
/// order, not the document's column order.
fn flatten(snapshot: &Snapshot, spec: &PanelSpec, draft: &Draft, rows_idx: usize) -> FlattenResult {
    let flat_columns = spec.flat_columns();
    if flat_columns.is_empty() {
        return Err(format!(
            "the document '{}' has no value column",
            spec.dataset
        ));
    }
    // Resolve each spec-declared column against the document, in spec
    // order: this loop's order is what `columns`, `column_kinds` and
    // every row's `cells` inherit.
    let mut idxs = Vec::with_capacity(flat_columns.len());
    for vc in flat_columns {
        let idx = snapshot
            .column_index(vc.column)
            .ok_or_else(|| format!("the document has no '{}' column", vc.column))?;
        idxs.push(idx);
    }
    // The reverse direction: a value column the document carries that the
    // spec does not list is refused, never painted unlabelled — a schema
    // drifting under a spec is reported rather than silently shown or
    // silently dropped.
    for idx in 0..snapshot.columns() {
        let name = snapshot
            .meta_at(idx)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        if is_value(snapshot, idx) && spec.value_column(&name).is_none() {
            return Err(format!(
                "the document carries a value column '{name}' this panel does not declare"
            ));
        }
    }

    let columns = flat_columns
        .iter()
        .map(|vc| SharedString::from(vc.label))
        .collect();
    let column_kinds: Vec<CellKind> = flat_columns.iter().map(flat_kind).collect();

    // A row label must name exactly one row, the same rule `index_grid`
    // applies to a pivot's (row, column) pair and for the same reason: a
    // draft resolves its edits by label across generations, so a repeated
    // label makes two different rows one target. One defence, here at the
    // model boundary, is what lets `Draft::rebase` index the labels
    // without a collision check of its own.
    let mut seen: HashMap<String, usize> = HashMap::with_capacity(snapshot.rows());
    let mut rows = Vec::with_capacity(snapshot.rows());
    for row in 0..snapshot.rows() {
        let label = required_label(snapshot, rows_idx, row, spec.rows.column)?;
        if let Some(previous) = seen.insert(label.clone(), row) {
            return Err(format!(
                "the document repeats {}='{label}' (rows {previous} and {row}): a row \
                 label identifies an edit, so it must name one row",
                spec.rows.column
            ));
        }
        let cells = idxs
            .iter()
            .zip(flat_columns.iter())
            .zip(column_kinds.iter())
            .enumerate()
            .map(|(ci, ((&idx, vc), kind))| {
                let value = read_flat_value(snapshot, idx, row, vc.ty);
                cell_of(value, (row, ci), kind, draft)
            })
            .collect();
        rows.push(RowModel {
            label: SharedString::from(label),
            cells,
            state: RowState::Document,
        });
    }
    Ok((columns, column_kinds, rows))
}

/// Apply row edits after document cells and their `cell_ref`s are built.
/// Deleted rows remain marked in the grid. Inserted rows use their own
/// label-keyed cells and follow their anchor, including another insert.
/// Siblings follow draft label order; `None` or an unknown anchor places
/// a row at the top. Unreachable anchor cycles are appended at the end.
///
/// Document edit keys remain unchanged when rows shift. With no row edits,
/// return the original rows without allocating.
fn splice_rows(
    rows: Vec<RowModel>,
    draft: &Draft,
    columns: &[SharedString],
    column_kinds: &[CellKind],
) -> Vec<RowModel> {
    if draft.rows.is_empty() {
        return rows;
    }
    // Every label the model will carry — the document's rows and the
    // draft's inserted ones — so an anchor can be checked against the
    // whole set; then the inserted rows grouped by anchor (the top's own
    // group apart), each group in label order (the draft's own `BTreeMap`
    // order), and each row's cells by label for the fill below.
    let known: HashSet<&str> = rows
        .iter()
        .map(|r| r.label.as_ref())
        .chain(draft.rows.keys().map(String::as_str))
        .collect();
    let mut top: Vec<&str> = Vec::new();
    let mut splicer = Splicer {
        followers: HashMap::new(),
        stack: Vec::new(),
        cells_of: HashMap::new(),
        columns,
        column_kinds,
        draft,
    };
    for (label, edit) in &draft.rows {
        if let RowEdit::Inserted { after, cells } = edit {
            match after.as_deref() {
                Some(a) if known.contains(a) => {
                    splicer.followers.entry(a).or_default().push(label);
                }
                _ => top.push(label),
            }
            splicer.cells_of.insert(label, cells);
        }
    }

    let mut out: Vec<RowModel> = Vec::with_capacity(rows.len() + splicer.cells_of.len());
    splicer.emit(top, &mut out);
    for mut row in rows {
        if matches!(draft.rows.get(row.label.as_ref()), Some(RowEdit::Deleted)) {
            row.state = RowState::Deleted;
        }
        let group = splicer.followers.remove(row.label.as_ref());
        out.push(row);
        if let Some(group) = group {
            splicer.emit(group, &mut out);
        }
    }
    // An anchor that is an inserted row is reachable only through that
    // row, so a cycle among inserted rows (a after b, b after a — nothing
    // in this crate writes one, but a session file could) would leave
    // both unplaced. Appended at the end, in label order, rather than
    // lost: unsent work is never dropped in silence.
    if !splicer.followers.is_empty() {
        let mut rest: Vec<&str> = std::mem::take(&mut splicer.followers)
            .into_values()
            .flatten()
            .collect();
        rest.sort_unstable();
        splicer.emit(rest, &mut out);
    }
    out
}

/// [`splice_rows`]'s working state: the inserted rows still waiting by
/// anchor label, the depth-first worklist, each row's cells, and what a
/// row's cells are built from. A struct rather than a closure so the
/// document loop can pull a row's own group out of `followers` while the
/// emitter is alive, which one closure borrowing both could not allow.
struct Splicer<'a> {
    followers: HashMap<&'a str, Vec<&'a str>>,
    stack: Vec<&'a str>,
    cells_of: HashMap<&'a str, &'a BTreeMap<String, Value>>,
    columns: &'a [SharedString],
    column_kinds: &'a [CellKind],
    draft: &'a Draft,
}

impl<'a> Splicer<'a> {
    /// Emit `group` and, depth first, every row anchored on each of them:
    /// a row's own followers go on TOP of the stack, so a chain hangs off
    /// its anchor ahead of the anchor's next sibling. A worklist rather
    /// than recursion, so a long chain costs no stack frames. A row is
    /// removed from `followers` as it is placed, so nothing is emitted
    /// twice.
    fn emit(&mut self, group: Vec<&'a str>, out: &mut Vec<RowModel>) {
        self.stack.extend(group.into_iter().rev());
        while let Some(label) = self.stack.pop() {
            let at = out.len();
            out.push(inserted_row(
                label,
                self.cells_of[label],
                at,
                self.columns,
                self.column_kinds,
                self.draft,
            ));
            if let Some(next) = self.followers.remove(label) {
                self.stack.extend(next.into_iter().rev());
            }
        }
    }
}
/// One inserted row at model position `at`: a cell per column, each from
/// the row's own `cells` by column label through [`inserted_cell`].
fn inserted_row(
    label: &str,
    cells: &BTreeMap<String, Value>,
    at: usize,
    columns: &[SharedString],
    column_kinds: &[CellKind],
    draft: &Draft,
) -> RowModel {
    RowModel {
        label: SharedString::from(label.to_string()),
        cells: columns
            .iter()
            .zip(column_kinds)
            .enumerate()
            .map(|(ci, (column, kind))| {
                inserted_cell(cells.get(column.as_ref()), (at, ci), kind, draft)
            })
            .collect(),
        state: RowState::Inserted,
    }
}

/// An inserted cell uses the draft value formatted through its column
/// kind, or `·` when unfilled. That marker distinguishes pending input from
/// a document's blank NULL cell. Every inserted cell is marked edited.
fn inserted_cell(
    value: Option<&Value>,
    cell_ref: (usize, usize),
    kind: &CellKind,
    draft: &Draft,
) -> Cell {
    Cell {
        text: value.map_or_else(
            || SharedString::new_static(UNFILLED),
            |v| SharedString::from(cell_text(v, kind)),
        ),
        value: value.cloned(),
        edited: true,
        sent: draft.is_sent(),
        cell_ref,
    }
}

/// What an inserted row's unfilled cell paints.
pub const UNFILLED: &str = "·";

/// One cell: the draft's value where there is an edit, `value` (already
/// read off the document, per the caller's own rule — `f64_at` for a
/// pivot cell, [`read_flat_value`] for a flat one) otherwise. Both are
/// formatted through [`cell_text`] and `kind`, the column's own.
fn cell_of(value: Option<Value>, cell_ref: (usize, usize), kind: &CellKind, draft: &Draft) -> Cell {
    if let Some(edited) = draft.edits.get(&cell_ref) {
        return Cell {
            text: SharedString::from(cell_text(edited, kind)),
            value: Some(edited.clone()),
            edited: true,
            sent: draft.is_sent(),
            cell_ref,
        };
    }
    // Missing or unreadable document values paint blank, never zero.
    // Typed readers can also return `None` for incompatible input.
    Cell {
        text: value
            .as_ref()
            .map(|v| SharedString::from(cell_text(v, kind)))
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
    use crate::core::spec::{
        CVI, Columns, HeaderAttr, PanelSpec, RowAxis, RowIdentity, RowLabel, SliceValue,
        ValueColumn,
    };
    use crate::core::test_fixtures::{
        SCHEDULE, date, schedule_snapshot, schedule_snapshot_with_extra_value,
    };
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
            // Document snapshots have depth 0 only, so each column has one
            // attribution entry.
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
    /// `Date32`: an axis's own value is read as a label either way
    /// ([`label_at`]'s rule), so nothing here needs `TestColumn::Date` —
    /// [`schedule_snapshot`] (below) is what exercises that arm, since a
    /// flat panel's `ex_date` is a typed `Value::Date` cell, not a label.
    ///
    /// The per-slice values (`forward`/`atm`/`skew`) ride on every node
    /// row of their term, from [`slice_values_for`] — the long form the
    /// kind stores them in.
    fn document(cells: &[(String, f64, Option<f64>)]) -> Snapshot {
        document_with(cells, |_, term| slice_values_for(term))
    }

    /// The slice values a term carries in these fixtures: the first two
    /// terms have their own, anything else a third set.
    fn slice_values_for(term: &str) -> (Option<f64>, Option<f64>, Option<f64>) {
        match term {
            "2026-10-16" => (Some(4512.3), Some(0.182), Some(-1.1)),
            "2026-11-20" => (Some(4530.75), Some(0.19), Some(-0.95)),
            _ => (Some(4600.0), Some(0.2), Some(-1.0)),
        }
    }

    /// [`document`] with the slice values chosen per (row, term) — how a
    /// test builds a slice whose rows disagree.
    fn document_with(
        cells: &[(String, f64, Option<f64>)],
        slice: impl Fn(usize, &str) -> (Option<f64>, Option<f64>, Option<f64>),
    ) -> Snapshot {
        let n = cells.len();
        let slices: Vec<_> = cells
            .iter()
            .enumerate()
            .map(|(row, c)| slice(row, &c.0))
            .collect();
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
                    meta("forward", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(slices.iter().map(|s| s.0).collect()),
                ),
                (
                    meta("atm", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(slices.iter().map(|s| s.1).collect()),
                ),
                (
                    meta("skew", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(slices.iter().map(|s| s.2).collect()),
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
    /// 0.1 … 0.6 in axis order, each term's slice values from
    /// [`slice_values_for`]. Its grid is three slice columns then three
    /// nodes, so a node cell sits at column `3 + n`.
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
        // The slice values are the FIRST grid columns, ahead of the
        // ladder, each with its own format: a forward at two places, a
        // vol and a skew at four.
        assert_eq!(
            columns_of(&model),
            vec!["fwd", "atm", "skew", "-20", "-1", "3.5"]
        );
        assert_eq!(model.slice_columns, 3);
        assert_eq!(model.rows[0].label.to_string(), "2026-10-16");
        assert_eq!(model.rows[1].label.to_string(), "2026-11-20");
        assert_eq!(
            labels(&model, 0),
            vec!["4512.30", "0.1820", "-1.1000", "0.1000", "0.2000", "0.3000"]
        );
        assert_eq!(model.rows[0].cells[0].value, Some(Value::F64(4512.3)));
        assert_eq!(model.rows[0].cells[0].cell_ref, (0, 0));

        let cell = &model.rows[0].cells[4];
        assert_eq!(
            cell.text.to_string(),
            "0.2000",
            "CVI formats to four places"
        );
        assert_eq!(cell.value, Some(Value::F64(0.2)));
        assert!(!cell.edited);
        assert!(!cell.sent);
        assert_eq!(
            cell.cell_ref,
            (0, 4),
            "a cell carries the grid index the draft is keyed by"
        );
        assert_eq!(
            labels(&model, 1),
            vec!["4530.75", "0.1900", "-0.9500", "0.4000", "0.5000", "0.6000"]
        );

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
        assert_eq!(columns_of(&model), vec!["fwd", "atm", "skew", "3.5", "-20"]);
        assert_eq!(
            labels(&model, 0),
            vec!["4530.75", "0.1900", "-0.9500", "0.1000", "0.2000"]
        );
        assert_eq!(
            labels(&model, 1),
            vec!["4512.30", "0.1820", "-1.1000", "0.3000", "0.4000"]
        );
    }

    /// A slice value the long form repeats per node row must agree across
    /// the slice; two forwards under one term is the document
    /// contradicting itself, and the pivot refuses it like a hole rather
    /// than averaging or keeping the first — either would paint a grid
    /// that looks complete and is wrong.
    #[test]
    fn a_within_slice_disagreement_is_refused_naming_the_term_and_column() {
        let mut cells = Vec::new();
        for term in TERMS {
            for node in NODES {
                cells.push((term.to_string(), node, Some(0.1)));
            }
        }
        // Row 4 is the second term's middle node: its forward disagrees.
        let snap = document_with(&cells, |row, term| {
            let (fwd, atm, skew) = slice_values_for(term);
            if row == 4 {
                (fwd.map(|f| f + 1.0), atm, skew)
            } else {
                (fwd, atm, skew)
            }
        });
        let err = MatrixModel::build(&snap, &CVI, &Draft::default())
            .expect_err("a slice whose rows disagree is refused, never averaged");
        assert!(err.contains("2026-11-20"), "{err}");
        assert!(err.contains("forward"), "{err}");
        assert!(
            !err.contains("2026-10-16"),
            "the agreeing term is not blamed: {err}"
        );
    }

    /// A slice value the document does not carry is left out, as a
    /// header attribute is: the grid is then the ladder alone, with
    /// `slice_columns` saying so.
    #[test]
    fn a_document_without_the_slice_columns_builds_the_ladder_alone() {
        let n = 6;
        let mut cells = Vec::new();
        for term in TERMS {
            for node in NODES {
                cells.push((term.to_string(), node, Some(0.1)));
            }
        }
        let snap = Snapshot::for_tests_with_provenance(
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
            ],
            0,
            provenance(BASE),
        );
        let model = MatrixModel::build(&snap, &CVI, &Draft::default()).expect("the ladder alone");
        assert_eq!(model.slice_columns, 0);
        assert_eq!(columns_of(&model), vec!["-20", "-1", "3.5"]);
    }

    /// The pivot compares slice values as `Option<f64>` through `f64_at`:
    /// a NULL beside a value in one slice is a disagreement (refused,
    /// naming the term and the column), while a slice that is NULL
    /// throughout agrees with itself and paints a blank cell — the same
    /// "NULL is not 0.0" rule a ladder cell has.
    #[test]
    fn a_null_beside_a_value_in_a_slice_is_a_disagreement_and_an_all_null_slice_is_blank() {
        let mut cells = Vec::new();
        for term in TERMS {
            for node in NODES {
                cells.push((term.to_string(), node, Some(0.1)));
            }
        }
        // Row 4 (the second term's middle node) has no atm where the rest
        // of the slice does.
        let mixed = document_with(&cells, |row, term| {
            let (fwd, atm, skew) = slice_values_for(term);
            if row == 4 {
                (fwd, None, skew)
            } else {
                (fwd, atm, skew)
            }
        });
        let err = MatrixModel::build(&mixed, &CVI, &Draft::default())
            .expect_err("a NULL beside a value is a disagreement");
        assert!(err.contains("2026-11-20"), "{err}");
        assert!(err.contains("atm"), "{err}");

        // The whole second term has no skew: consistent, so it builds and
        // that slice cell is blank, never 0.0000.
        let all_null = document_with(&cells, |_, term| {
            let (fwd, atm, skew) = slice_values_for(term);
            if term == "2026-11-20" {
                (fwd, atm, None)
            } else {
                (fwd, atm, skew)
            }
        });
        let model = MatrixModel::build(&all_null, &CVI, &Draft::default())
            .expect("an all-NULL slice agrees with itself");
        assert_eq!(model.rows[1].cells[2].text.to_string(), "");
        assert_eq!(model.rows[1].cells[2].value, None);
        assert_eq!(model.rows[0].cells[2].text.to_string(), "-1.1000");
    }

    /// A slice label is a column label, and `Draft` indexes columns by
    /// label: a spec whose slice label the column axis also produces is
    /// refused rather than painted as two columns with one name.
    #[test]
    fn a_slice_label_colliding_with_an_axis_label_is_refused() {
        // CVI's three slice values, the first relabelled as a node.
        const COLLIDING: PanelSpec = PanelSpec {
            slice_values: &[
                SliceValue {
                    column: "forward",
                    label: "-20",
                    format: ColumnFormat::MEASURE,
                },
                SliceValue {
                    column: "atm",
                    label: "atm",
                    format: ColumnFormat::MEASURE,
                },
                SliceValue {
                    column: "skew",
                    label: "skew",
                    format: ColumnFormat::MEASURE,
                },
            ],
            ..CVI
        };
        let err = MatrixModel::build(&full_grid(), &COLLIDING, &Draft::default())
            .expect_err("a node labelled -20 and a slice value labelled -20");
        assert!(err.contains("'-20'"), "{err}");
        assert!(err.contains("forward"), "{err}");
        assert!(
            MatrixModel::build(&full_grid(), &CVI, &Draft::default()).is_ok(),
            "the shipped spec's labels do not collide with its ladder"
        );
    }

    /// An edit to a slice cell is keyed like any other — `(row, col)` in
    /// the grid, resolved by `(term, label)` across generations — so a
    /// draft on `fwd` paints over the slice value with the slice's own
    /// format and rebases by `(term, "fwd")`.
    #[test]
    fn an_edited_slice_cell_paints_the_draft_in_the_slice_values_own_format() {
        let mut draft = Draft::default();
        draft.set(
            (1, 0),
            ("2026-11-20".into(), "fwd".into()),
            Value::F64(4600.0),
            BASE,
        );
        let model = MatrixModel::build(&full_grid(), &CVI, &draft).expect("a complete grid");
        let cell = &model.rows[1].cells[0];
        assert_eq!(cell.text.to_string(), "4600.00");
        assert!(cell.edited);
        assert_eq!(model.label_of((1, 0)), ("2026-11-20".into(), "fwd".into()));
        assert!(
            !model.rows[0].cells[0].edited,
            "the other term's forward is untouched"
        );
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
            (0, 4),
            ("2026-10-16".into(), "-1".into()),
            Value::F64(0.9),
            "2026-09-12T14:00:00Z",
        );
        let model = MatrixModel::build(&snap, &CVI, &draft).expect("a complete grid");

        let cell = &model.rows[0].cells[4];
        assert_eq!(cell.text.to_string(), "0.9000");
        assert_eq!(cell.value, Some(Value::F64(0.9)));
        assert!(cell.edited);
        assert!(!cell.sent, "an edit is sent only once an upload said so");
        assert_eq!(
            model.rows[0].cells[3].text.to_string(),
            "0.1000",
            "a neighbouring cell still paints the document"
        );
        assert!(!model.rows[0].cells[3].edited);
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
        draft.set(
            (1, 3),
            ("2026-11-20".into(), "-20".into()),
            Value::F64(0.5),
            BASE,
        );
        draft.state = DraftState::Sent {
            at: "2026-09-12T14:05:00Z".into(),
        };
        let model = MatrixModel::build(&full_grid(), &CVI, &draft).expect("a complete grid");
        assert!(model.rows[1].cells[3].sent);
        assert!(model.rows[1].cells[3].edited);
        assert!(!model.rows[1].cells[4].sent);
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
        assert_eq!(model.rows[0].cells[3].text.to_string(), "");
        assert_eq!(
            model.rows[0].cells[3].value, None,
            "NULL and 0.0 are different answers (§6.3)"
        );
    }

    /// A flat-shaped panel over two plain `F64` value columns — the
    /// generic "columns are flat" fixture the tests below use for
    /// behaviour that has nothing to do with a column's own type (row
    /// routing, repeated labels, the pivot's one-value-column rule).
    /// [`SCHEDULE`] (below) is the typed-cell fixture proper.
    const FLAT_SPEC: PanelSpec = PanelSpec {
        kind: "sched",
        title: "Dividends",
        dataset: "div_schedule",
        document: "div_schedule",
        rows: RowAxis {
            column: "ex_date",
            identity: RowIdentity::Typed(ColumnType::Date),
            label: RowLabel::Shown,
        },
        columns: Columns::Values(&[
            ValueColumn {
                column: "gross",
                label: "gross",
                ty: ColumnType::F64,
                format: ColumnFormat::MEASURE,
                choices: None,
                required: true,
            },
            ValueColumn {
                column: "net",
                label: "net",
                ty: ColumnType::F64,
                format: ColumnFormat::MEASURE,
                choices: None,
                required: true,
            },
        ]),
        header: &[HeaderAttr {
            column: "currency",
            label: "currency",
            ty: ColumnType::Utf8,
        }],
        slice_values: &[],
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        actions: &[],
    };

    fn flat_snapshot() -> Snapshot {
        flat_snapshot_dated(&["2026-10-16", "2026-11-20", "2026-12-18"])
    }

    /// The same three-row schedule with the row axis spelled by the caller,
    /// so a repeat can be delivered.
    fn flat_snapshot_dated(dates: &[&str; 3]) -> Snapshot {
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
        let model = MatrixModel::build(&flat_snapshot(), &FLAT_SPEC, &Draft::default())
            .expect("a flat document");
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
        draft.set(
            (2, 1),
            ("2026-12-18".into(), "net".into()),
            Value::F64(9.0),
            BASE,
        );
        let model =
            MatrixModel::build(&flat_snapshot(), &FLAT_SPEC, &draft).expect("a flat document");
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
        let (row, col) = model.label_of((1, 5));
        assert_eq!(row.to_string(), "2026-11-20");
        assert_eq!(col.to_string(), "3.5");
        let (_, col) = model.label_of((1, 1));
        assert_eq!(col.to_string(), "atm");
        let (row, col) = model.label_of((9, 9));
        assert_eq!(row.to_string(), "");
        assert_eq!(col.to_string(), "");
    }

    #[test]
    fn a_repeated_row_label_is_refused_when_the_columns_are_flat() {
        // Repeated labels would merge distinct edit targets during rebase,
        // so the model must reject the document.
        let err = MatrixModel::build(
            &flat_snapshot_dated(&["2026-10-16", "2026-10-16", "2026-12-18"]),
            &FLAT_SPEC,
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
            rows: RowAxis {
                column: "ex_date",
                identity: RowIdentity::Typed(ColumnType::Date),
                label: RowLabel::Shown,
            },
            columns: Columns::Axis("currency"),
            header: &[],
            slice_values: &[],
            value_type: ColumnType::F64,
            format: ColumnFormat::MEASURE,
            actions: &[],
        };
        let err = MatrixModel::build(&flat_snapshot(), &PIVOTED, &Draft::default())
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

    /// A CVI document whose `param` column carries TEXT instead of a
    /// number — the pivot's one value column must be numeric, and this
    /// is what a misdeclared spec (or a genuinely non-numeric dataset)
    /// looks like. No slice values: CVI's own are optional (see
    /// `a_document_without_the_slice_columns_builds_the_ladder_alone`
    /// above), so this fixture can leave them out.
    fn cvi_with_text_param() -> Snapshot {
        let n = 2;
        Snapshot::for_tests_with_provenance(
            vec![
                (
                    meta("underlying_ref", Attribution::Additive),
                    TestColumn::Dict(vec![Some("SPX.Z".into()); n]),
                ),
                (
                    meta("term", Attribution::Additive),
                    TestColumn::Dict(vec![Some("2026-10-16".into()); n]),
                ),
                (
                    meta("node", Attribution::Additive),
                    TestColumn::F64(vec![Some(-20.0), Some(-1.0)]),
                ),
                (
                    meta("param", Attribution::DeterminedNonAdditive),
                    TestColumn::Dict(vec![Some("n/a".into()); n]),
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

    /// Flat cells use their column kinds: ISO dates, literal text, and
    /// column-formatted numbers. Kinds remain parallel to column labels.
    #[test]
    fn a_flat_model_types_each_column_by_its_spec() {
        let snapshot = schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ]);
        let model = MatrixModel::build(&snapshot, &SCHEDULE, &Draft::default()).unwrap();
        assert_eq!(model.columns, ["ex", "amount", "status"]);
        assert!(matches!(model.column_kinds[0], CellKind::Date));
        assert!(matches!(model.column_kinds[1], CellKind::Number(_)));
        assert!(matches!(model.column_kinds[2], CellKind::Choice(_)));
        assert_eq!(model.rows[0].cells[0].text.as_ref(), "2026-12-18");
        assert_eq!(model.rows[0].cells[1].text.as_ref(), "1.2500");
        assert_eq!(model.rows[0].cells[2].text.as_ref(), "declared");
        assert_eq!(
            model.rows[1].cells[0].value,
            Some(Value::Date(date(2027, 3, 19)))
        );
    }

    /// A value column the spec does not list is refused, never painted
    /// unlabelled: a schema drifting under a spec is reported.
    #[test]
    fn a_flat_model_refuses_a_value_column_the_spec_does_not_list() {
        let snapshot = schedule_snapshot_with_extra_value("bonus");
        let err = MatrixModel::build(&snapshot, &SCHEDULE, &Draft::default()).unwrap_err();
        assert!(
            err.contains("'bonus'") && err.contains("does not declare"),
            "{err}"
        );
    }

    /// The pivot's one cell column must be numeric.
    #[test]
    fn a_pivot_refuses_a_non_numeric_value_column() {
        let snapshot = cvi_with_text_param();
        let err = MatrixModel::build(&snapshot, &CVI, &Draft::default()).unwrap_err();
        assert!(err.contains("numeric"), "{err}");
    }

    /// A typed edit paints in its column's own kind.
    #[test]
    fn a_typed_edit_paints_by_its_columns_kind() {
        let snapshot = schedule_snapshot(&[("D1", "2026-12-18", 1.25, "declared")]);
        let mut draft = Draft::default();
        draft.set(
            (0, 2),
            ("D1".into(), "status".into()),
            Value::Utf8("paid".into()),
            "t0",
        );
        draft.set(
            (0, 0),
            ("D1".into(), "ex".into()),
            Value::Date(date(2026, 12, 20)),
            "t0",
        );
        let model = MatrixModel::build(&snapshot, &SCHEDULE, &draft).unwrap();
        assert_eq!(model.rows[0].cells[2].text.as_ref(), "paid");
        assert!(model.rows[0].cells[2].edited);
        assert_eq!(model.rows[0].cells[0].text.as_ref(), "2026-12-20");
    }

    /// A cell patch paints the same value and flags as a full rebuild.
    #[test]
    fn patch_cell_matches_a_rebuild() {
        let snapshot = schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ]);
        let mut draft = Draft::default();
        let mut patched = MatrixModel::build(&snapshot, &SCHEDULE, &draft).unwrap();
        draft.set(
            (1, 1),
            ("D2".into(), "amount".into()),
            Value::F64(0.75),
            "t0",
        );
        assert!(patched.patch_cell(1, 1, &snapshot, &SCHEDULE, &draft));
        let rebuilt = MatrixModel::build(&snapshot, &SCHEDULE, &draft).unwrap();
        assert_eq!(patched.rows[1].cells[1], rebuilt.rows[1].cells[1]);
        assert_eq!(patched.rows[1].cells[1].text.as_ref(), "0.7500");
        assert_eq!(
            patched.rows[0].cells[1], rebuilt.rows[0].cells[1],
            "untouched cells untouched"
        );
        assert_eq!(patched, rebuilt, "and nothing else about the model moved");
        // A reverted edit patches back to the document's own value: the
        // patch reads the draft as it stands, not the cell as it was.
        draft.revert();
        assert!(patched.patch_cell(1, 1, &snapshot, &SCHEDULE, &draft));
        assert_eq!(patched.rows[1].cells[1].text.as_ref(), "0.5000");
        assert!(!patched.rows[1].cells[1].edited);
        assert!(
            !patched.patch_cell(9, 0, &snapshot, &SCHEDULE, &draft),
            "out of range answers false"
        );
        assert!(
            !patched.patch_cell(0, 9, &snapshot, &SCHEDULE, &draft),
            "on either axis"
        );
    }

    /// The pivot's own patch: a ladder cell and a slice cell each find
    /// their snapshot row through the grid the build indexed — the
    /// (row, column) → snapshot row map a flat layout does not need,
    /// since there a grid row IS a snapshot row.
    #[test]
    fn patch_cell_matches_a_rebuild_under_a_pivot() {
        let snapshot = full_grid();
        let mut draft = Draft::default();
        let mut patched = MatrixModel::build(&snapshot, &CVI, &draft).unwrap();
        // Node 3.5 of the second term: column 3 + 2 in the grid, snapshot
        // row 5 — the last of six, so a patch that read "grid row" as
        // "snapshot row" would land on the wrong term.
        draft.set(
            (1, 5),
            ("2026-11-20".into(), "3.5".into()),
            Value::F64(9.0),
            "t0",
        );
        // And a slice cell: `fwd` of the first term, read off that
        // slice's first row.
        draft.set(
            (0, 0),
            ("2026-10-16".into(), "fwd".into()),
            Value::F64(4600.5),
            "t0",
        );
        assert!(patched.patch_cell(1, 5, &snapshot, &CVI, &draft));
        assert!(patched.patch_cell(0, 0, &snapshot, &CVI, &draft));
        let rebuilt = MatrixModel::build(&snapshot, &CVI, &draft).unwrap();
        assert_eq!(patched, rebuilt);
        assert_eq!(patched.rows[1].cells[5].text.as_ref(), "9.0000");
        assert_eq!(patched.rows[0].cells[0].text.as_ref(), "4600.50");
        draft.revert();
        assert!(patched.patch_cell(1, 5, &snapshot, &CVI, &draft));
        assert!(patched.patch_cell(0, 0, &snapshot, &CVI, &draft));
        assert_eq!(patched.rows[1].cells[5].text.as_ref(), "0.6000");
        assert_eq!(patched.rows[0].cells[0].text.as_ref(), "4512.30");
        assert!(!patched.patch_cell(2, 0, &snapshot, &CVI, &draft));
    }

    /// Inserted rows follow their anchors and use label-keyed draft cells;
    /// deleted rows remain marked. Missing inserted cells paint `·`, and
    /// incomplete counts measure rows rather than individual missing cells.
    #[test]
    fn inserted_rows_splice_after_their_anchor_and_deleted_rows_stay_marked() {
        let snapshot = schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
            ("D3", "2027-06-18", 0.75, "estimated"),
        ]);
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("D1".into()), "t0");
        d.insert_row("new-2".into(), None, "t0");
        d.set_row_cell("new-1", "amount", Value::F64(2.0));
        d.delete_row("D3", "t0");
        let m = MatrixModel::build(&snapshot, &SCHEDULE, &d).unwrap();
        let labels: Vec<_> = m.rows.iter().map(|r| r.label.to_string()).collect();
        assert_eq!(labels, ["new-2", "D1", "new-1", "D2", "D3"]);
        assert_eq!(m.rows[0].state, RowState::Inserted);
        assert_eq!(m.rows[1].state, RowState::Document);
        assert_eq!(m.rows[2].state, RowState::Inserted);
        assert_eq!(m.rows[2].cells[1].text.as_ref(), "2.0000");
        assert_eq!(m.rows[2].cells[1].value, Some(Value::F64(2.0)));
        assert!(m.rows[2].cells[1].edited);
        assert_eq!(
            m.rows[2].cells[0].text.as_ref(),
            "·",
            "an unfilled cell paints a dot"
        );
        assert_eq!(m.rows[2].cells[0].value, None);
        assert!(m.rows[2].cells[0].edited);
        assert_eq!(m.rows[4].state, RowState::Deleted);
        assert_eq!(
            m.rows[4].cells[1].text.as_ref(),
            "0.7500",
            "a deleted row still paints the document's own cells"
        );
        assert_eq!(
            d.incomplete_rows(&SCHEDULE, &m.columns),
            2,
            "new-1 lacks ex and status; new-2 lacks all three"
        );
        // An inserted row's cell_ref is its MODEL position (there is no
        // document position for it); a document row keeps the document's
        // own, which is what `Draft::edits` is keyed by — so D2, now at
        // model row 3, still says it is document row 1.
        assert_eq!(m.rows[2].cells[1].cell_ref, (2, 1));
        assert_eq!(m.rows[3].cells[0].cell_ref, (1, 0));
    }

    /// Several rows under one anchor land in label order; a row anchored
    /// on an inserted row follows it; an anchor that names no row at all
    /// lands at the top rather than losing the row.
    #[test]
    fn inserted_rows_chain_and_an_unknown_anchor_lands_at_the_top() {
        let snapshot = schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ]);
        let mut d = Draft::default();
        d.insert_row("new-3".into(), Some("D1".into()), "t0");
        d.insert_row("new-1".into(), Some("D1".into()), "t0");
        d.insert_row("new-2".into(), Some("new-3".into()), "t0");
        d.insert_row("new-4".into(), Some("gone".into()), "t0");
        let m = MatrixModel::build(&snapshot, &SCHEDULE, &d).unwrap();
        let labels: Vec<_> = m.rows.iter().map(|r| r.label.to_string()).collect();
        assert_eq!(labels, ["new-4", "D1", "new-1", "new-3", "new-2", "D2"]);
        for (i, row) in m.rows.iter().enumerate() {
            if row.state == RowState::Inserted {
                for (j, cell) in row.cells.iter().enumerate() {
                    assert_eq!(cell.cell_ref, (i, j));
                }
            }
        }
    }

    /// A pivot's inserted row is incomplete until EVERY column — the
    /// ladder and the slice values alike — is filled: a CVI term with a
    /// node missing is not a term the desk can price.
    #[test]
    fn a_pivot_inserted_row_is_incomplete_until_every_cell_is_filled() {
        let snapshot = full_grid();
        let mut d = Draft::default();
        d.insert_row("2027-01-15".into(), Some("2026-11-20".into()), BASE);
        let m = MatrixModel::build(&snapshot, &CVI, &d).unwrap();
        let labels: Vec<_> = m.rows.iter().map(|r| r.label.to_string()).collect();
        assert_eq!(labels, ["2026-10-16", "2026-11-20", "2027-01-15"]);
        assert_eq!(m.rows[2].state, RowState::Inserted);
        assert_eq!(
            m.rows[2].cells.len(),
            6,
            "three slice values and three nodes"
        );
        assert!(m.rows[2].cells.iter().all(|c| c.text.as_ref() == "·"));
        assert_eq!(d.incomplete_rows(&CVI, &m.columns), 1);
        for node in ["-20", "-1", "3.5"] {
            d.set_row_cell("2027-01-15", node, Value::F64(0.2));
        }
        assert_eq!(
            d.incomplete_rows(&CVI, &m.columns),
            1,
            "the ladder is filled but the slice values are not"
        );
        for slice in ["fwd", "atm", "skew"] {
            d.set_row_cell("2027-01-15", slice, Value::F64(1.0));
        }
        assert_eq!(d.incomplete_rows(&CVI, &m.columns), 0);
        let m = MatrixModel::build(&snapshot, &CVI, &d).unwrap();
        assert_eq!(
            m.rows[2].cells[0].text.as_ref(),
            "1.00",
            "fwd at its own format"
        );
        assert_eq!(
            m.rows[2].cells[3].text.as_ref(),
            "0.2000",
            "a node at the panel's"
        );
    }

    /// Under `Columns::Values` only the `required` columns count: a row
    /// missing an optional column is complete.
    #[test]
    fn a_flat_inserted_row_is_complete_once_its_required_columns_are_filled() {
        let spec = PanelSpec {
            columns: Columns::Values(&[
                ValueColumn {
                    column: "gross",
                    label: "gross",
                    ty: ColumnType::F64,
                    format: ColumnFormat::MEASURE,
                    choices: None,
                    required: true,
                },
                ValueColumn {
                    column: "net",
                    label: "net",
                    ty: ColumnType::F64,
                    format: ColumnFormat::MEASURE,
                    choices: None,
                    required: false,
                },
            ]),
            ..FLAT_SPEC.clone()
        };
        let m = MatrixModel::build(&flat_snapshot(), &spec, &Draft::default()).unwrap();
        let mut d = Draft::default();
        d.insert_row("new-1".into(), None, BASE);
        assert_eq!(d.incomplete_rows(&spec, &m.columns), 1);
        d.set_row_cell("new-1", "gross", Value::F64(1.0));
        assert_eq!(d.incomplete_rows(&spec, &m.columns), 0, "net is optional");
        assert_eq!(
            Draft::default().incomplete_rows(&spec, &m.columns),
            0,
            "no inserted rows, nothing incomplete"
        );
    }

    /// Patch inserted values from their draft rows and shifted document
    /// values through `cell_ref`, matching a full rebuild in both cases.
    #[test]
    fn patch_cell_matches_a_rebuild_with_rows_spliced() {
        let snapshot = schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ]);
        let mut draft = Draft::default();
        draft.insert_row("new-1".into(), None, "t0");
        let mut patched = MatrixModel::build(&snapshot, &SCHEDULE, &draft).unwrap();
        assert_eq!(patched.rows[2].label.as_ref(), "D2");
        // D2 is model row 2 but document row 1 — its cell_ref says so, and
        // that is the key its edit lives under.
        let d2_ref = patched.rows[2].cells[1].cell_ref;
        assert_eq!(d2_ref, (1, 1));
        draft.set(
            d2_ref,
            ("D2".into(), "amount".into()),
            Value::F64(0.75),
            "t0",
        );
        draft.set_row_cell("new-1", "amount", Value::F64(3.0));
        assert!(patched.patch_cell(2, 1, &snapshot, &SCHEDULE, &draft));
        assert!(patched.patch_cell(0, 1, &snapshot, &SCHEDULE, &draft));
        let rebuilt = MatrixModel::build(&snapshot, &SCHEDULE, &draft).unwrap();
        assert_eq!(patched, rebuilt);
        assert_eq!(patched.rows[2].cells[1].text.as_ref(), "0.7500");
        assert_eq!(patched.rows[0].cells[1].text.as_ref(), "3.0000");
        assert_eq!(
            patched.rows[1].cells[1].text.as_ref(),
            "1.2500",
            "D1, at the inserted row's old index, is untouched"
        );
        // An inserted row the draft no longer holds cannot be patched:
        // the caller rebuilds.
        draft.delete_row("new-1", "t0");
        assert!(!patched.patch_cell(0, 1, &snapshot, &SCHEDULE, &draft));
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
            // The three slice columns lead; the ladder follows them.
            let s = model.slice_columns;
            prop_assert_eq!(s, 3);
            prop_assert_eq!(columns_of(&model)[s..].to_vec(), want_columns.clone());
            for (i, row) in model.rows.iter().enumerate() {
                prop_assert_eq!(row.cells.len(), s + n);
                for (j, cell) in row.cells.iter().enumerate().skip(s) {
                    let want = want_value[&(want_rows[i].clone(), want_columns[j - s].clone())];
                    prop_assert_eq!(cell.value.clone(), Some(Value::F64(want)));
                    prop_assert_eq!(cell.cell_ref, (i, j));
                }
            }
        }
    }
}
