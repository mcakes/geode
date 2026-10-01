//! The market-data grid's whole-document index — labels, row states, row
//! sources, the label map — and the one cell formatter. Built on delivery,
//! structural edit and selection bulk step; formats nothing. The delegate's
//! window (`geode_tile::grid::WindowCache<MdCell>`) holds the formatted text
//! for the rows on screen; every reader of cell text (window fill, yank,
//! find, editors) goes through [`MatrixIndex::format_cell`].
//!
//! `Columns::Axis` pivots one value onto a row-by-column grid with leading
//! slice columns. `Columns::Values` lays out the declared value columns.
//! Both expose the same cells to cursor, copy, and edit operations.

use crate::core::draft::{DocumentBase, Draft, RowEdit, attr_text};
use crate::core::spec::{Columns, PanelSpec, ValueColumn};
use geode_core::attribution::Attribution;
use geode_core::document::Value;
use geode_core::format::format_number;
use geode_core::schema::ColumnType;
use geode_core::snapshot::Snapshot;
use geode_core::view::ColumnFormat;
use geode_tile::grid::WindowCache;
use gpui::SharedString;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A column's display and editor kind, parallel to [`MatrixIndex::columns`].
/// Pivot ladder and slice columns are numeric. Flat columns use their
/// declared type and optional choice vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub enum CellKind {
    Number(ColumnFormat),
    Date,
    Text,
    Choice(Arc<[String]>),
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

/// One cell as a reader sees it, prepared on demand by
/// [`MatrixIndex::cell`]; never stored.
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

/// Where a row's values come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowSource {
    /// A document row: its base-document row (the snapshot row when flat,
    /// the [`PivotIndex`] row under a pivot). Also its edits' key row.
    Document(usize),
    /// An inserted row: values live in the draft's `RowEdit::Inserted`
    /// under the row's label.
    Inserted,
}

/// One row as the index holds it, borrowed.
#[derive(Debug, Clone, Copy)]
pub struct RowView<'a> {
    pub index: usize,
    pub label: &'a SharedString,
    pub state: RowState,
}

/// One window cell as the delegate paints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdCell {
    pub text: SharedString,
    pub edited: bool,
    pub sent: bool,
    pub state: RowState,
}

/// A row while the build orders and splices it; unzipped into the index's
/// parallel arrays at the end.
struct IndexRow {
    label: SharedString,
    state: RowState,
    source: RowSource,
}

/// One cell's reading: its value, whether the draft wrote it, its edit key.
struct Reading<'a> {
    value: Option<Value>,
    edited: bool,
    sent: bool,
    inserted: bool,
    cell_ref: (usize, usize),
    kind: &'a CellKind,
}

#[cfg(test)]
thread_local! {
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Index builds on this thread, for tests that prove a route does not build.
#[cfg(test)]
pub(crate) fn builds() -> usize {
    BUILDS.with(|b| b.get())
}

/// The whole document as the grid lays it out, without any cell text.
#[derive(Debug, Clone, Default)]
pub struct MatrixIndex {
    /// The document key, in `document_columns()` order.
    pub key: Vec<String>,
    /// The document generation this index was built from, from the first
    /// provenance dataset. Draft identity compares the whole pair: a
    /// corrected republish keeps its source time and changes only the
    /// generation.
    pub base: Option<DocumentBase>,
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
    /// How many of `columns` are the spec's per-slice values rather than
    /// the pivot's own ladder — what lets a row bump skip them and the
    /// delegate rule them off.
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
    /// Snapshot positions for pivot cells, so a cell is read on demand
    /// without re-indexing the document. Flat grids use the base row index
    /// and `flat` directly.
    pub pivot_index: Option<PivotIndex>,
    /// Parallel per painted row.
    labels: Vec<SharedString>,
    states: Vec<RowState>,
    sources: Vec<RowSource>,
    /// Label → painted row, built with the arrays. The first row wins if a
    /// label ever repeated, as `position` did.
    label_index: HashMap<SharedString, usize>,
    /// Flat panels: per column, its snapshot column and declared type.
    flat: Vec<(usize, ColumnType)>,
    /// The generation this index was built from: values are read from it on demand.
    snapshot: Option<Arc<Snapshot>>,
}

/// The pivot's (grid row, ladder column) → snapshot row map, plus the
/// snapshot columns the ladder and each slice column are read from —
/// everything [`pivot`] had in hand when it indexed the grid, kept so every
/// on-demand read finds the cell the build checked. Indices into the
/// snapshot the index was built from; meaningless against any other.
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

/// Read the first provenance dataset's source time and generation as a
/// [`DocumentBase`]. Return `None` if the dataset or its source time is absent;
/// retain an unknown generation as `None` within a dated base.
///
/// The tile and prepared model share this extraction so they agree on the
/// identity of a delivered document.
pub(crate) fn base_of(snapshot: &Snapshot) -> Option<DocumentBase> {
    let freshness = snapshot.provenance().datasets.first()?;
    Some(DocumentBase {
        as_of: freshness.as_of.clone()?,
        generation: freshness.generation,
    })
}

impl MatrixIndex {
    /// The [`CellKind`] a column paints and edits through, if `col` is in
    /// range.
    pub fn kind_of(&self, col: usize) -> Option<&CellKind> {
        self.column_kinds.get(col)
    }

    /// Index a grid over the delivered document, with the draft's row edits
    /// spliced in. No cell is read or formatted: values are read on demand.
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
    /// anchors, and document rows keep their document row as their source.
    pub fn build(
        snapshot: &Arc<Snapshot>,
        spec: &PanelSpec,
        draft: &Draft,
    ) -> Result<MatrixIndex, String> {
        #[cfg(test)]
        BUILDS.with(|b| b.set(b.get() + 1));
        let base = base_of(snapshot);
        // An empty result is not a defect: the key may simply have no
        // document yet, or an as-of before its first publish (the
        // compiler's `and false` arm). The tile paints "no document
        // received for <key>" off `is_empty()`.
        if snapshot.rows() == 0 {
            return Ok(MatrixIndex {
                base,
                snapshot: Some(Arc::clone(snapshot)),
                ..MatrixIndex::default()
            });
        }

        let rows_idx = snapshot
            .column_index(&spec.rows.column)
            .ok_or_else(|| format!("the document has no '{}' column", spec.rows.column))?;
        let key = key_of(snapshot, spec, rows_idx)?;
        let header = header_of(snapshot, spec, draft);
        let (columns, column_kinds, slice_columns, column_values, flat, rows, pivot_index) =
            match &spec.columns {
                Columns::Axis(axis) => {
                    let (columns, column_kinds, slice_columns, column_values, rows, index) =
                        pivot(snapshot, spec, rows_idx, axis)?;
                    (
                        columns,
                        column_kinds,
                        slice_columns,
                        column_values,
                        Vec::new(),
                        rows,
                        Some(index),
                    )
                }
                Columns::Values(_) => {
                    let (columns, column_kinds, flat, rows) = flatten(snapshot, spec, rows_idx)?;
                    (columns, column_kinds, 0, Vec::new(), flat, rows, None)
                }
            };
        let rows = splice_rows(rows, draft);
        let mut index = MatrixIndex {
            key,
            base,
            header,
            columns,
            column_kinds,
            slice_columns,
            column_values,
            pivot_index,
            flat,
            snapshot: Some(Arc::clone(snapshot)),
            ..MatrixIndex::default()
        };
        index.set_rows(rows);
        Ok(index)
    }

    /// The "no document received" shape: the key the panel asked about
    /// and nothing else. Distinct from a built index only in having no
    /// rows — the tile's header says which.
    pub fn empty(spec: &PanelSpec, key: &[String]) -> MatrixIndex {
        // Keep the same spec-bearing interface as `build`; an empty index
        // currently carries only the requested key.
        let _ = spec;
        MatrixIndex {
            key: key.to_vec(),
            ..MatrixIndex::default()
        }
    }

    /// An index with the given row and column labels, document rows only,
    /// no snapshot: what draft and header tests need.
    #[cfg(test)]
    pub(crate) fn for_tests(rows: &[&str], columns: &[&str]) -> MatrixIndex {
        let mut index = MatrixIndex {
            columns: columns
                .iter()
                .map(|c| SharedString::from(c.to_string()))
                .collect(),
            ..MatrixIndex::default()
        };
        index.set_rows(
            rows.iter()
                .enumerate()
                .map(|(i, l)| IndexRow {
                    label: SharedString::from(l.to_string()),
                    state: RowState::Document,
                    source: RowSource::Document(i),
                })
                .collect(),
        );
        index
    }

    /// The row and column labels used to identify an edit across generations.
    /// Each out-of-range coordinate returns an empty label independently; a
    /// cursor can outlive the index against which it was positioned.
    pub fn label_of(&self, cell: (usize, usize)) -> (SharedString, SharedString) {
        (
            self.labels.get(cell.0).cloned().unwrap_or_default(),
            self.columns.get(cell.1).cloned().unwrap_or_default(),
        )
    }

    fn set_rows(&mut self, rows: Vec<IndexRow>) {
        let n = rows.len();
        self.labels.reserve_exact(n);
        self.states.reserve_exact(n);
        self.sources.reserve_exact(n);
        self.label_index.reserve(n);
        for (i, r) in rows.into_iter().enumerate() {
            self.label_index.entry(r.label.clone()).or_insert(i);
            self.labels.push(r.label);
            self.states.push(r.state);
            self.sources.push(r.source);
        }
    }

    /// Painted rows, inserts and deleted rows included.
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    pub fn row(&self, r: usize) -> Option<RowView<'_>> {
        Some(RowView {
            index: r,
            label: self.labels.get(r)?,
            state: self.states[r],
        })
    }

    pub fn rows(&self) -> impl Iterator<Item = RowView<'_>> + '_ {
        (0..self.len()).filter_map(|r| self.row(r))
    }

    pub fn label(&self, r: usize) -> Option<&SharedString> {
        self.labels.get(r)
    }

    pub fn state(&self, r: usize) -> Option<RowState> {
        self.states.get(r).copied()
    }

    /// The painted row carrying `label`.
    pub fn row_of(&self, label: &str) -> Option<usize> {
        self.label_index.get(label).copied()
    }

    pub fn label_index(&self) -> &HashMap<SharedString, usize> {
        &self.label_index
    }

    /// The generation this index was built from, `None` for
    /// [`MatrixIndex::empty`].
    pub fn snapshot(&self) -> Option<&Arc<Snapshot>> {
        self.snapshot.as_ref()
    }

    /// A document cell's edit key, `None` on an inserted row (its values are
    /// keyed by label) or out of range.
    pub fn cell_ref(&self, cell: (usize, usize)) -> Option<(usize, usize)> {
        match *self.sources.get(cell.0)? {
            RowSource::Document(doc_row) => Some((doc_row, cell.1)),
            RowSource::Inserted => None,
        }
    }

    /// The document's own value at a document row, read on demand.
    fn document_value(&self, doc_row: usize, col: usize) -> Option<Value> {
        let snapshot = self.snapshot.as_deref()?;
        match &self.pivot_index {
            Some(index) => match col.checked_sub(self.slice_columns) {
                // A ladder cell: the value column at the snapshot row the
                // build's own grid map recorded for this pair.
                Some(ci) => {
                    let &srow = index.at.get(doc_row * index.ladder + ci)?;
                    snapshot.f64_at(index.value_idx, srow).map(Value::F64)
                }
                // A slice cell: read off its slice's first row (the build
                // checked the whole slice agrees).
                None => {
                    let (&idx, &first) = (index.slice_idx.get(col)?, index.first_row.get(doc_row)?);
                    snapshot.f64_at(idx, first).map(Value::F64)
                }
            },
            // The flat shape: document row `doc_row` is snapshot row
            // `doc_row`, and grid column `col` is the spec's `col`th flat
            // column.
            None => {
                let &(idx, ty) = self.flat.get(col)?;
                read_flat_value(snapshot, idx, doc_row, ty)
            }
        }
    }

    fn read(&self, draft: &Draft, r: usize, col: usize) -> Option<Reading<'_>> {
        let kind = self.column_kinds.get(col)?;
        match *self.sources.get(r)? {
            RowSource::Inserted => {
                let label = self.columns.get(col)?;
                let cells = match draft.rows.get(self.labels[r].as_ref()) {
                    Some(RowEdit::Inserted { cells, .. }) => Some(cells),
                    _ => None,
                };
                let value = cells.and_then(|c| c.get(label.as_ref())).cloned();
                // Every inserted cell is edited; its edit key is its painted
                // position, since its values are keyed by label.
                Some(Reading {
                    value,
                    edited: true,
                    sent: draft.is_sent(),
                    inserted: true,
                    cell_ref: (r, col),
                    kind,
                })
            }
            RowSource::Document(doc_row) => {
                let (value, edited) =
                    draft_or(self.document_value(doc_row, col), (doc_row, col), draft);
                Some(Reading {
                    value,
                    edited,
                    sent: edited && draft.is_sent(),
                    inserted: false,
                    cell_ref: (doc_row, col),
                    kind,
                })
            }
        }
    }

    /// The cell's current value: the draft's where it wrote one, else the
    /// document's. `None` for NULL, an unfilled insert, or out of range.
    pub fn value_at(&self, draft: &Draft, r: usize, col: usize) -> Option<Value> {
        self.read(draft, r, col)?.value
    }

    pub fn edited_at(&self, draft: &Draft, r: usize, col: usize) -> bool {
        self.read(draft, r, col).is_some_and(|x| x.edited)
    }

    pub fn sent_at(&self, draft: &Draft, r: usize, col: usize) -> bool {
        self.read(draft, r, col).is_some_and(|x| x.sent)
    }

    /// The one formatter: the window, yank, find and the editors all read
    /// cell text through here. Blank out of range or for a NULL document
    /// value; [`UNFILLED`] for an unfilled inserted cell.
    pub fn format_cell(&self, draft: &Draft, r: usize, col: usize) -> SharedString {
        self.read(draft, r, col)
            .map(|x| text_of(&x))
            .unwrap_or_default()
    }

    /// One window cell, through [`MatrixIndex::format_cell`]'s rule.
    pub fn md_cell(&self, draft: &Draft, r: usize, col: usize) -> Option<MdCell> {
        let x = self.read(draft, r, col)?;
        Some(MdCell {
            text: text_of(&x),
            edited: x.edited,
            sent: x.sent,
            state: self.states[r],
        })
    }

    /// One cell as a reader sees it, on demand.
    pub fn cell(&self, draft: &Draft, r: usize, col: usize) -> Option<Cell> {
        let x = self.read(draft, r, col)?;
        Some(Cell {
            text: text_of(&x),
            edited: x.edited,
            sent: x.sent,
            cell_ref: x.cell_ref,
            value: x.value,
        })
    }
}

/// The draft's edit at `cell_ref` where there is one, else the document's value.
fn draft_or(
    document: Option<Value>,
    cell_ref: (usize, usize),
    draft: &Draft,
) -> (Option<Value>, bool) {
    if let Some(edited) = draft.edits.get(&cell_ref) {
        return (Some(edited.clone()), true);
    }
    // Missing or unreadable document values paint blank, never zero.
    // Typed readers can also return `None` for incompatible input.
    (document, false)
}

/// A reading's text through [`cell_text`] and its column's kind: blank for a
/// missing document value, [`UNFILLED`] for a missing inserted one, so
/// pending input is distinct from a document's NULL.
fn text_of(x: &Reading<'_>) -> SharedString {
    match &x.value {
        Some(v) => SharedString::from(cell_text(v, x.kind)),
        None if x.inserted => SharedString::new_static(UNFILLED),
        None => SharedString::default(),
    }
}

/// Re-prepare one window cell after an edit to it, through the one formatter.
pub fn refill_cell(
    window: &mut WindowCache<MdCell>,
    index: &MatrixIndex,
    draft: &Draft,
    cell: (usize, usize),
) {
    window.refill_cell(cell.0, cell.1, || index.md_cell(draft, cell.0, cell.1));
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
            let idx = snapshot.column_index(&attr.column)?;
            let (text, edited) = match draft.attrs.get(&attr.column) {
                Some(value) => (attr_text(value), true),
                None => (label_at(snapshot, idx, 0)?, false),
            };
            Some(HeaderCell {
                column: attr.column.clone().into(),
                label: attr.label.clone().into(),
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
        let row_label = required_label(snapshot, rows_idx, row, &spec.rows.column)?;
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
/// two leading vecs are slice columns, the indexed rows, and the
/// [`PivotIndex`] every on-demand read goes through.
type PivotResult = Result<
    (
        Vec<SharedString>,
        Vec<CellKind>,
        usize,
        Vec<Value>,
        Vec<IndexRow>,
        PivotIndex,
    ),
    String,
>;

fn pivot(snapshot: &Snapshot, spec: &PanelSpec, rows_idx: usize, axis: &str) -> PivotResult {
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
        .filter_map(|sv| snapshot.column_index(&sv.column).map(|idx| (sv, idx)))
        .collect();
    // A slice label is a column label: `Draft` resolves edits by label and
    // indexes `columns` by it, so a node that reads `fwd` would make two
    // columns one name.
    for (sv, _) in &slices {
        if grid.columns.contains(&sv.label) {
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

    // The index every on-demand read goes through, filled as the grid is:
    // one entry per cell the loop below visits, in the same order.
    let mut index = PivotIndex {
        value_idx,
        slice_idx: slices.iter().map(|(_, idx)| *idx).collect(),
        first_row: Vec::with_capacity(grid.rows.len()),
        ladder: grid.columns.len(),
        at: Vec::with_capacity(grid.rows.len() * grid.columns.len()),
    };
    let mut rows = Vec::with_capacity(grid.rows.len());
    for (ri, row_label) in grid.rows.iter().enumerate() {
        // Every row label came out of `index_grid`'s pass over the
        // snapshot rows, so each has a first row.
        let first = first_row[ri].unwrap_or_default();
        index.first_row.push(first);
        for col_label in grid.columns.iter() {
            match grid.at.get(row_label).and_then(|m| m.get(col_label)) {
                Some(&srow) => {
                    index.at.push(srow);
                }
                None => {
                    return Err(format!(
                        "the document has no cell for {}='{row_label}' {axis}='{col_label}'",
                        spec.rows.column
                    ));
                }
            }
        }
        rows.push(IndexRow {
            label: SharedString::from(row_label.clone()),
            state: RowState::Document,
            source: RowSource::Document(ri),
        });
    }
    Ok((
        slices
            .iter()
            .map(|(sv, _)| SharedString::from(sv.label.clone()))
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
/// [`MatrixIndex::column_values`]). `index_grid` has already refused a
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
    match (vc.ty, &vc.choices) {
        (ColumnType::F64 | ColumnType::I64, _) => CellKind::Number(vc.format.clone()),
        (ColumnType::Date, _) => CellKind::Date,
        (_, Some(choices)) => CellKind::Choice(Arc::clone(choices)),
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
/// the same order, each column's snapshot column and declared type, and
/// the indexed rows.
type FlattenResult = Result<
    (
        Vec<SharedString>,
        Vec<CellKind>,
        Vec<(usize, ColumnType)>,
        Vec<IndexRow>,
    ),
    String,
>;

/// The flat shape: one row per document row, one column per
/// [`PanelSpec::flat_columns`] entry, in the spec's own order — the paint
/// order, not the document's column order.
fn flatten(snapshot: &Snapshot, spec: &PanelSpec, rows_idx: usize) -> FlattenResult {
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
            .column_index(&vc.column)
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
        .map(|vc| SharedString::from(vc.label.clone()))
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
        let label = required_label(snapshot, rows_idx, row, &spec.rows.column)?;
        if let Some(previous) = seen.insert(label.clone(), row) {
            return Err(format!(
                "the document repeats {}='{label}' (rows {previous} and {row}): a row \
                 label identifies an edit, so it must name one row",
                spec.rows.column
            ));
        }
        rows.push(IndexRow {
            label: SharedString::from(label),
            state: RowState::Document,
            source: RowSource::Document(row),
        });
    }
    let flat = idxs
        .into_iter()
        .zip(flat_columns.iter())
        .map(|(idx, vc)| (idx, vc.ty))
        .collect();
    Ok((columns, column_kinds, flat, rows))
}

/// Apply row edits after the document rows are indexed. Deleted rows remain
/// marked in the grid. Inserted rows follow their anchor, including another
/// insert. Siblings follow draft label order; `None` or an unknown anchor
/// places a row at the top. Unreachable anchor cycles are appended at the end.
///
/// Document rows keep their document row as their source when rows shift.
/// With no row edits, return the original rows without allocating.
fn splice_rows(rows: Vec<IndexRow>, draft: &Draft) -> Vec<IndexRow> {
    if draft.rows.is_empty() {
        return rows;
    }
    // Every label the index will carry — the document's rows and the
    // draft's inserted ones — so an anchor can be checked against the
    // whole set; then the inserted rows grouped by anchor (the top's own
    // group apart), each group in label order (the draft's own `BTreeMap`
    // order).
    let known: HashSet<&str> = rows
        .iter()
        .map(|r| r.label.as_ref())
        .chain(draft.rows.keys().map(String::as_str))
        .collect();
    let mut top: Vec<&str> = Vec::new();
    let mut splicer = Splicer {
        followers: HashMap::new(),
        stack: Vec::new(),
    };
    for (label, edit) in &draft.rows {
        if let RowEdit::Inserted { after, .. } = edit {
            match after.as_deref() {
                Some(a) if known.contains(a) => {
                    splicer.followers.entry(a).or_default().push(label);
                }
                _ => top.push(label),
            }
        }
    }

    let inserted = draft
        .rows
        .values()
        .filter(|e| matches!(e, RowEdit::Inserted { .. }))
        .count();
    let mut out: Vec<IndexRow> = Vec::with_capacity(rows.len() + inserted);
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
/// anchor label and the depth-first worklist. A struct rather than a
/// closure so the document loop can pull a row's own group out of
/// `followers` while the emitter is alive, which one closure borrowing
/// both could not allow.
struct Splicer<'a> {
    followers: HashMap<&'a str, Vec<&'a str>>,
    stack: Vec<&'a str>,
}

impl<'a> Splicer<'a> {
    /// Emit `group` and, depth first, every row anchored on each of them:
    /// a row's own followers go on TOP of the stack, so a chain hangs off
    /// its anchor ahead of the anchor's next sibling. A worklist rather
    /// than recursion, so a long chain costs no stack frames. A row is
    /// removed from `followers` as it is placed, so nothing is emitted
    /// twice.
    fn emit(&mut self, group: Vec<&'a str>, out: &mut Vec<IndexRow>) {
        self.stack.extend(group.into_iter().rev());
        while let Some(label) = self.stack.pop() {
            out.push(IndexRow {
                label: SharedString::from(label.to_string()),
                state: RowState::Inserted,
                source: RowSource::Inserted,
            });
            if let Some(next) = self.followers.remove(label) {
                self.stack.extend(next.into_iter().rev());
            }
        }
    }
}

/// What an inserted row's unfilled cell paints.
pub const UNFILLED: &str = "·";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::draft::{Draft, DraftState};
    use crate::core::spec::{
        Columns, HeaderAttr, PanelSpec, RowAxis, RowIdentity, RowLabel, SliceValue, ValueColumn,
    };
    use crate::core::test_fixtures::{
        CVI, SCHEDULE, at, date, schedule_snapshot, schedule_snapshot_with_extra_value,
    };
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::document::Value;
    use geode_core::schema::ColumnType;
    use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
    use geode_core::view::ColumnFormat;
    use proptest::prelude::*;
    use std::sync::LazyLock;

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
            summable: false,
            mixed_flag: None,
        }
    }

    fn provenance(as_of: &str) -> Provenance {
        Provenance {
            datasets: vec![Freshness {
                dataset: "cvi_params".into(),
                as_of: Some(as_of.into()),
                generation: Some(7),
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

    fn labels(model: &MatrixIndex, draft: &Draft, row: usize) -> Vec<String> {
        (0..model.columns.len())
            .map(|c| model.format_cell(draft, row, c).to_string())
            .collect()
    }

    fn provenance_base() -> DocumentBase {
        base_of(&full_grid()).expect("the fixture carries provenance")
    }

    fn columns_of(model: &MatrixIndex) -> Vec<String> {
        model.columns.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn a_pivot_puts_the_row_axis_down_the_side_and_the_column_axis_across() {
        let snap = full_grid();
        let model =
            MatrixIndex::build(&Arc::new(snap), &CVI, &Draft::default()).expect("a complete grid");

        assert_eq!(model.key, vec!["SPX.Z".to_string()]);
        assert_eq!(
            model.base,
            Some(DocumentBase {
                as_of: BASE.to_string(),
                generation: Some(7),
            })
        );
        assert_eq!(model.len(), 2);
        // The slice values are the FIRST grid columns, ahead of the
        // ladder, each with its own format: a forward at two places, a
        // vol and a skew at four.
        assert_eq!(
            columns_of(&model),
            vec!["fwd", "atm", "skew", "-20", "-1", "3.5"]
        );
        assert_eq!(model.slice_columns, 3);
        assert_eq!(model.label(0).unwrap().to_string(), "2026-10-16");
        assert_eq!(model.label(1).unwrap().to_string(), "2026-11-20");
        assert_eq!(
            labels(&model, &Draft::default(), 0),
            vec!["4512.30", "0.1820", "-1.1000", "0.1000", "0.2000", "0.3000"]
        );
        assert_eq!(
            model.cell(&Draft::default(), 0, 0).unwrap().value,
            Some(Value::F64(4512.3))
        );
        assert_eq!(
            model.cell(&Draft::default(), 0, 0).unwrap().cell_ref,
            (0, 0)
        );

        let cell = &model.cell(&Draft::default(), 0, 4).unwrap();
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
            labels(&model, &Draft::default(), 1),
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
        let model = MatrixIndex::build(&Arc::new(document(&cells)), &CVI, &Draft::default())
            .expect("a full grid");
        assert_eq!(
            model
                .rows()
                .map(|r| r.label.to_string())
                .collect::<Vec<_>>(),
            vec!["2026-11-20", "2026-10-16"]
        );
        assert_eq!(columns_of(&model), vec!["fwd", "atm", "skew", "3.5", "-20"]);
        assert_eq!(
            labels(&model, &Draft::default(), 0),
            vec!["4530.75", "0.1900", "-0.9500", "0.1000", "0.2000"]
        );
        assert_eq!(
            labels(&model, &Draft::default(), 1),
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
        let err = MatrixIndex::build(&Arc::new(snap), &CVI, &Draft::default())
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
        let model =
            MatrixIndex::build(&Arc::new(snap), &CVI, &Draft::default()).expect("the ladder alone");
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
        let err = MatrixIndex::build(&Arc::new(mixed), &CVI, &Draft::default())
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
        let model = MatrixIndex::build(&Arc::new(all_null), &CVI, &Draft::default())
            .expect("an all-NULL slice agrees with itself");
        assert_eq!(
            model
                .cell(&Draft::default(), 1, 2)
                .unwrap()
                .text
                .to_string(),
            ""
        );
        assert_eq!(model.cell(&Draft::default(), 1, 2).unwrap().value, None);
        assert_eq!(
            model
                .cell(&Draft::default(), 0, 2)
                .unwrap()
                .text
                .to_string(),
            "-1.1000"
        );
    }

    /// A slice label is a column label, and `Draft` indexes columns by
    /// label: a spec whose slice label the column axis also produces is
    /// refused rather than painted as two columns with one name.
    #[test]
    fn a_slice_label_colliding_with_an_axis_label_is_refused() {
        // CVI's three slice values, the first relabelled as a node.
        static COLLIDING: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
            Arc::new(PanelSpec {
                slice_values: vec![
                    SliceValue {
                        column: "forward".into(),
                        label: "-20".into(),
                        format: ColumnFormat::MEASURE,
                    },
                    SliceValue {
                        column: "atm".into(),
                        label: "atm".into(),
                        format: ColumnFormat::MEASURE,
                    },
                    SliceValue {
                        column: "skew".into(),
                        label: "skew".into(),
                        format: ColumnFormat::MEASURE,
                    },
                ],
                ..(**CVI).clone()
            })
        });
        let err = MatrixIndex::build(&Arc::new(full_grid()), &COLLIDING, &Draft::default())
            .expect_err("a node labelled -20 and a slice value labelled -20");
        assert!(err.contains("'-20'"), "{err}");
        assert!(err.contains("forward"), "{err}");
        assert!(
            MatrixIndex::build(&Arc::new(full_grid()), &CVI, &Draft::default()).is_ok(),
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
            &at(BASE),
        );
        let model =
            MatrixIndex::build(&Arc::new(full_grid()), &CVI, &draft).expect("a complete grid");
        let cell = &model.cell(&draft, 1, 0).unwrap();
        assert_eq!(cell.text.to_string(), "4600.00");
        assert!(cell.edited);
        assert_eq!(model.label_of((1, 0)), ("2026-11-20".into(), "fwd".into()));
        assert!(
            !model.cell(&draft, 0, 0).unwrap().edited,
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
        let err = MatrixIndex::build(&Arc::new(document(&cells)), &CVI, &Draft::default())
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
        let err = MatrixIndex::build(&Arc::new(document(&cells)), &CVI, &Draft::default())
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
            &at("2026-09-12T14:00:00Z"),
        );
        let model = MatrixIndex::build(&Arc::new(snap), &CVI, &draft).expect("a complete grid");

        let cell = &model.cell(&draft, 0, 4).unwrap();
        assert_eq!(cell.text.to_string(), "0.9000");
        assert_eq!(cell.value, Some(Value::F64(0.9)));
        assert!(cell.edited);
        assert!(!cell.sent, "an edit is sent only once an upload said so");
        assert_eq!(
            model.cell(&draft, 0, 3).unwrap().text.to_string(),
            "0.1000",
            "a neighbouring cell still paints the document"
        );
        assert!(!model.cell(&draft, 0, 3).unwrap().edited);
    }

    #[test]
    fn an_edited_attribute_paints_the_drafts_value_marked_edited() {
        let snapshot = full_grid();
        let mut draft = Draft::default();
        draft.set_attr("spot_ref", Value::F64(4520.0), &at("t0"));
        let model = MatrixIndex::build(&Arc::new(snapshot), &CVI, &draft).unwrap();
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
            &at(BASE),
        );
        draft.state = DraftState::Sent {
            at: "2026-09-12T14:05:00Z".into(),
        };
        let model =
            MatrixIndex::build(&Arc::new(full_grid()), &CVI, &draft).expect("a complete grid");
        assert!(model.cell(&draft, 1, 3).unwrap().sent);
        assert!(model.cell(&draft, 1, 3).unwrap().edited);
        assert!(!model.cell(&draft, 1, 4).unwrap().sent);
    }

    #[test]
    fn a_null_value_paints_blank_not_zero() {
        let mut cells = Vec::new();
        for term in TERMS {
            for node in NODES {
                cells.push((term.to_string(), node, None));
            }
        }
        let model = MatrixIndex::build(&Arc::new(document(&cells)), &CVI, &Draft::default())
            .expect("a full grid");
        assert_eq!(
            model
                .cell(&Draft::default(), 0, 3)
                .unwrap()
                .text
                .to_string(),
            ""
        );
        assert_eq!(
            model.cell(&Draft::default(), 0, 3).unwrap().value,
            None,
            "NULL and 0.0 are different answers"
        );
    }

    /// A flat-shaped panel over two plain `F64` value columns — the
    /// generic "columns are flat" fixture the tests below use for
    /// behaviour that has nothing to do with a column's own type (row
    /// routing, repeated labels, the pivot's one-value-column rule).
    /// [`SCHEDULE`] (below) is the typed-cell fixture proper.
    static FLAT_SPEC: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
        Arc::new(PanelSpec {
            kind: "sched".into(),
            title: "Dividends".into(),
            dataset: "div_schedule".into(),
            document: "div_schedule".into(),
            rows: RowAxis {
                column: "ex_date".into(),
                identity: RowIdentity::Typed(ColumnType::Date),
                label: RowLabel::Shown,
            },
            columns: Columns::Values(vec![
                ValueColumn {
                    column: "gross".into(),
                    label: "gross".into(),
                    ty: ColumnType::F64,
                    format: ColumnFormat::MEASURE,
                    choices: None,
                    required: true,
                },
                ValueColumn {
                    column: "net".into(),
                    label: "net".into(),
                    ty: ColumnType::F64,
                    format: ColumnFormat::MEASURE,
                    choices: None,
                    required: true,
                },
            ]),
            header: vec![HeaderAttr {
                column: "currency".into(),
                label: "currency".into(),
                ty: ColumnType::Utf8,
            }],
            slice_values: Vec::new(),
            value_type: ColumnType::F64,
            format: ColumnFormat::MEASURE,
            actions: Vec::new(),
        })
    });

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
        let model = MatrixIndex::build(&Arc::new(flat_snapshot()), &FLAT_SPEC, &Draft::default())
            .expect("a flat document");
        assert_eq!(model.len(), 3, "one row per document row");
        assert_eq!(columns_of(&model), vec!["gross", "net"]);
        assert_eq!(model.key, vec!["SPX.Z".to_string()]);
        assert_eq!(model.label(1).unwrap().to_string(), "2026-11-20");
        assert_eq!(labels(&model, &Draft::default(), 1), vec!["2.50", "2.00"]);
        assert_eq!(
            model.cell(&Draft::default(), 1, 0).unwrap().cell_ref,
            (1, 0)
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
            &at(BASE),
        );
        let model = MatrixIndex::build(&Arc::new(flat_snapshot()), &FLAT_SPEC, &draft)
            .expect("a flat document");
        assert_eq!(labels(&model, &draft, 2), vec!["3.50", "9.00"]);
        assert!(model.cell(&draft, 2, 1).unwrap().edited);
        assert!(!model.cell(&draft, 2, 0).unwrap().edited);
    }

    #[test]
    fn empty_has_no_rows_and_carries_the_key() {
        let model = MatrixIndex::empty(&CVI, &["SPX.Z".to_string()]);
        assert!(model.is_empty());
        assert!(model.columns.is_empty());
        assert!(model.header.is_empty());
        assert_eq!(model.base, None);
        assert_eq!(model.key, vec!["SPX.Z".to_string()]);
    }

    #[test]
    fn a_document_with_no_rows_builds_the_no_document_shape() {
        let model = MatrixIndex::build(&Arc::new(document(&[])), &CVI, &Draft::default())
            .expect("an empty result is not an error — the key may simply be unknown");
        assert!(model.is_empty());
        assert!(model.columns.is_empty());
    }

    #[test]
    fn label_of_answers_the_row_and_column_labels_and_never_panics() {
        let model = MatrixIndex::build(&Arc::new(full_grid()), &CVI, &Draft::default())
            .expect("a full grid");
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
        let err = MatrixIndex::build(
            &Arc::new(flat_snapshot_dated(&[
                "2026-10-16",
                "2026-10-16",
                "2026-12-18",
            ])),
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
        static PIVOTED: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
            Arc::new(PanelSpec {
                kind: "sched".into(),
                title: "Dividends".into(),
                dataset: "div_schedule".into(),
                document: "div_schedule".into(),
                rows: RowAxis {
                    column: "ex_date".into(),
                    identity: RowIdentity::Typed(ColumnType::Date),
                    label: RowLabel::Shown,
                },
                columns: Columns::Axis("currency".into()),
                header: Vec::new(),
                slice_values: Vec::new(),
                value_type: ColumnType::F64,
                format: ColumnFormat::MEASURE,
                actions: Vec::new(),
            })
        });
        let err = MatrixIndex::build(&Arc::new(flat_snapshot()), &PIVOTED, &Draft::default())
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
            MatrixIndex::build(&Arc::new(snap), &CVI, &Draft::default()).is_ok(),
            "the same document without the NULL builds"
        );
        let err = MatrixIndex::build(&Arc::new(holed), &CVI, &Draft::default())
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
        let err = MatrixIndex::build(&Arc::new(snap), &CVI, &Draft::default())
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
        let model = MatrixIndex::build(&Arc::new(snapshot), &SCHEDULE, &Draft::default()).unwrap();
        assert_eq!(model.columns, ["ex", "amount", "status"]);
        assert!(matches!(model.column_kinds[0], CellKind::Date));
        assert!(matches!(model.column_kinds[1], CellKind::Number(_)));
        assert!(matches!(model.column_kinds[2], CellKind::Choice(_)));
        assert_eq!(
            model.cell(&Draft::default(), 0, 0).unwrap().text.as_ref(),
            "2026-12-18"
        );
        assert_eq!(
            model.cell(&Draft::default(), 0, 1).unwrap().text.as_ref(),
            "1.2500"
        );
        assert_eq!(
            model.cell(&Draft::default(), 0, 2).unwrap().text.as_ref(),
            "declared"
        );
        assert_eq!(
            model.cell(&Draft::default(), 1, 0).unwrap().value,
            Some(Value::Date(date(2027, 3, 19)))
        );
    }

    /// A value column the spec does not list is refused, never painted
    /// unlabelled: a schema drifting under a spec is reported.
    #[test]
    fn a_flat_model_refuses_a_value_column_the_spec_does_not_list() {
        let snapshot = schedule_snapshot_with_extra_value("bonus");
        let err =
            MatrixIndex::build(&Arc::new(snapshot), &SCHEDULE, &Draft::default()).unwrap_err();
        assert!(
            err.contains("'bonus'") && err.contains("does not declare"),
            "{err}"
        );
    }

    /// The pivot's one cell column must be numeric.
    #[test]
    fn a_pivot_refuses_a_non_numeric_value_column() {
        let snapshot = cvi_with_text_param();
        let err = MatrixIndex::build(&Arc::new(snapshot), &CVI, &Draft::default()).unwrap_err();
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
            &at("t0"),
        );
        draft.set(
            (0, 0),
            ("D1".into(), "ex".into()),
            Value::Date(date(2026, 12, 20)),
            &at("t0"),
        );
        let model = MatrixIndex::build(&Arc::new(snapshot), &SCHEDULE, &draft).unwrap();
        assert_eq!(model.cell(&draft, 0, 2).unwrap().text.as_ref(), "paid");
        assert!(model.cell(&draft, 0, 2).unwrap().edited);
        assert_eq!(
            model.cell(&draft, 0, 0).unwrap().text.as_ref(),
            "2026-12-20"
        );
    }

    /// Every window cell is exactly what `format_cell` returns, over a pivot
    /// and a flat panel with an edit, a deleted row and an unfilled insert.
    #[test]
    fn the_window_paints_the_same_text_as_format_cell() {
        use geode_tile::grid::WindowCache;
        let mut draft = Draft::default();
        let base = provenance_base();
        draft.set(
            (0, 4),
            ("2026-10-16".into(), "-1".into()),
            Value::F64(0.25),
            &base,
        );
        draft.delete_row("2026-11-20", &base);
        draft.insert_row("2026-12-18".into(), Some("2026-10-16".into()), &base);
        let snapshot = Arc::new(full_grid());
        let index = MatrixIndex::build(&snapshot, &CVI, &draft).unwrap();
        let mut window = WindowCache::default();
        window.set_window(0..index.len(), index.columns.len(), |r, c| {
            index.md_cell(&draft, r, c)
        });
        for r in 0..index.len() {
            for c in 0..index.columns.len() {
                assert_eq!(
                    window.get(r, c).map(|m| m.text.clone()),
                    Some(index.format_cell(&draft, r, c)),
                    "({r}, {c})"
                );
            }
        }
        assert_eq!(
            index.format_cell(&draft, 1, 3).as_ref(),
            UNFILLED,
            "the insert is unfilled"
        );
        assert_eq!(index.format_cell(&draft, 0, 4).as_ref(), "0.2500");
        assert_eq!(index.state(2), Some(RowState::Deleted));

        // The flat shape: a typed edit and an unfilled insert.
        let flat = Arc::new(schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ]));
        let mut draft = Draft::default();
        draft.set(
            (1, 2),
            ("D2".into(), "status".into()),
            Value::Utf8("paid".into()),
            &at("t0"),
        );
        draft.insert_row("new-1".into(), Some("D1".into()), &at("t0"));
        let index = MatrixIndex::build(&flat, &SCHEDULE, &draft).unwrap();
        let mut window = WindowCache::default();
        window.set_window(0..index.len(), index.columns.len(), |r, c| {
            index.md_cell(&draft, r, c)
        });
        for r in 0..index.len() {
            for c in 0..index.columns.len() {
                assert_eq!(
                    window.get(r, c).map(|m| m.text.clone()),
                    Some(index.format_cell(&draft, r, c)),
                    "flat ({r}, {c})"
                );
            }
        }
        assert_eq!(index.format_cell(&draft, 2, 2).as_ref(), "paid");
        assert_eq!(index.format_cell(&draft, 1, 0).as_ref(), UNFILLED);
    }

    /// A label resolves through the index to its painted row, inserts
    /// included, and a label not in the grid resolves to nothing.
    #[test]
    fn label_lookup_and_uniqueness_use_the_index() {
        let base = provenance_base();
        let mut draft = Draft::default();
        draft.insert_row("2026-12-18".into(), Some("2026-10-16".into()), &base);
        let index = MatrixIndex::build(&Arc::new(full_grid()), &CVI, &draft).unwrap();
        assert_eq!(index.row_of("2026-10-16"), Some(0));
        assert_eq!(
            index.row_of("2026-12-18"),
            Some(1),
            "the insert follows its anchor"
        );
        assert_eq!(index.row_of("2026-11-20"), Some(2));
        assert_eq!(index.row_of("2027-01-15"), None);
        assert_eq!(index.label(1).map(|l| l.as_ref()), Some("2026-12-18"));
    }

    /// A one-cell edit re-prepares that window cell to exactly what a
    /// rebuilt index paints, and touches nothing else.
    #[test]
    fn patch_cell_matches_a_rebuild() {
        use geode_tile::grid::WindowCache;
        let snapshot = Arc::new(full_grid());
        let spec = &CVI;
        let index = MatrixIndex::build(&snapshot, spec, &Draft::default()).unwrap();
        let mut draft = Draft::default();
        let mut window = WindowCache::default();
        window.set_window(0..index.len(), index.columns.len(), |r, c| {
            index.md_cell(&draft, r, c)
        });
        let cell = (1, 4);
        let labels = index.label_of(cell);
        draft.set(
            cell,
            (labels.0.to_string(), labels.1.to_string()),
            Value::F64(9.5),
            &provenance_base(),
        );
        refill_cell(&mut window, &index, &draft, cell);
        let rebuilt = MatrixIndex::build(&snapshot, spec, &draft).unwrap();
        for r in 0..rebuilt.len() {
            for c in 0..rebuilt.columns.len() {
                assert_eq!(
                    window.get(r, c).cloned(),
                    rebuilt.md_cell(&draft, r, c),
                    "({r}, {c})"
                );
            }
        }
        assert!(window.get(1, 4).is_some_and(|m| m.edited));
        // A reverted edit refills back to the document's own value: the
        // refill reads the draft as it stands, not the cell as it was.
        draft.revert();
        refill_cell(&mut window, &index, &draft, cell);
        assert_eq!(
            window.get(1, 4).cloned(),
            index.md_cell(&draft, 1, 4),
            "back to the document"
        );
        assert!(!window.get(1, 4).is_some_and(|m| m.edited));
    }

    /// The pivot's own refill: a ladder cell and a slice cell each find
    /// their snapshot row through the grid the build indexed — the
    /// (row, column) → snapshot row map a flat layout does not need,
    /// since there a grid row IS a snapshot row.
    #[test]
    fn patch_cell_matches_a_rebuild_under_a_pivot() {
        use geode_tile::grid::WindowCache;
        let snapshot = Arc::new(full_grid());
        let mut draft = Draft::default();
        let index = MatrixIndex::build(&snapshot, &CVI, &draft).unwrap();
        let mut window = WindowCache::default();
        window.set_window(0..index.len(), index.columns.len(), |r, c| {
            index.md_cell(&draft, r, c)
        });
        // Node 3.5 of the second term: column 3 + 2 in the grid, snapshot
        // row 5 — the last of six, so a read that took "grid row" as
        // "snapshot row" would land on the wrong term.
        draft.set(
            (1, 5),
            ("2026-11-20".into(), "3.5".into()),
            Value::F64(9.0),
            &at("t0"),
        );
        // And a slice cell: `fwd` of the first term, read off that
        // slice's first row.
        draft.set(
            (0, 0),
            ("2026-10-16".into(), "fwd".into()),
            Value::F64(4600.5),
            &at("t0"),
        );
        refill_cell(&mut window, &index, &draft, (1, 5));
        refill_cell(&mut window, &index, &draft, (0, 0));
        let rebuilt = MatrixIndex::build(&snapshot, &CVI, &draft).unwrap();
        for r in 0..rebuilt.len() {
            for c in 0..rebuilt.columns.len() {
                assert_eq!(
                    window.get(r, c).cloned(),
                    rebuilt.md_cell(&draft, r, c),
                    "({r}, {c})"
                );
            }
        }
        assert_eq!(window.get(1, 5).unwrap().text.as_ref(), "9.0000");
        assert_eq!(window.get(0, 0).unwrap().text.as_ref(), "4600.50");
        draft.revert();
        refill_cell(&mut window, &index, &draft, (1, 5));
        refill_cell(&mut window, &index, &draft, (0, 0));
        assert_eq!(window.get(1, 5).unwrap().text.as_ref(), "0.6000");
        assert_eq!(window.get(0, 0).unwrap().text.as_ref(), "4512.30");
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
        d.insert_row("new-1".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-2".into(), None, &at("t0"));
        d.set_row_cell("new-1", "amount", Value::F64(2.0));
        d.delete_row("D3", &at("t0"));
        let m = MatrixIndex::build(&Arc::new(snapshot), &SCHEDULE, &d).unwrap();
        let labels: Vec<_> = m.rows().map(|r| r.label.to_string()).collect();
        assert_eq!(labels, ["new-2", "D1", "new-1", "D2", "D3"]);
        assert_eq!(m.state(0).unwrap(), RowState::Inserted);
        assert_eq!(m.state(1).unwrap(), RowState::Document);
        assert_eq!(m.state(2).unwrap(), RowState::Inserted);
        assert_eq!(m.cell(&d, 2, 1).unwrap().text.as_ref(), "2.0000");
        assert_eq!(m.cell(&d, 2, 1).unwrap().value, Some(Value::F64(2.0)));
        assert!(m.cell(&d, 2, 1).unwrap().edited);
        assert_eq!(
            m.cell(&d, 2, 0).unwrap().text.as_ref(),
            "·",
            "an unfilled cell paints a dot"
        );
        assert_eq!(m.cell(&d, 2, 0).unwrap().value, None);
        assert!(m.cell(&d, 2, 0).unwrap().edited);
        assert_eq!(m.state(4).unwrap(), RowState::Deleted);
        assert_eq!(
            m.cell(&d, 4, 1).unwrap().text.as_ref(),
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
        assert_eq!(m.cell(&d, 2, 1).unwrap().cell_ref, (2, 1));
        assert_eq!(m.cell(&d, 3, 0).unwrap().cell_ref, (1, 0));
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
        d.insert_row("new-3".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-1".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-2".into(), Some("new-3".into()), &at("t0"));
        d.insert_row("new-4".into(), Some("gone".into()), &at("t0"));
        let m = MatrixIndex::build(&Arc::new(snapshot), &SCHEDULE, &d).unwrap();
        let labels: Vec<_> = m.rows().map(|r| r.label.to_string()).collect();
        assert_eq!(labels, ["new-4", "D1", "new-1", "new-3", "new-2", "D2"]);
        for row in m.rows() {
            if row.state == RowState::Inserted {
                assert_eq!(m.cell_ref((row.index, 0)), None, "keyed by label");
                for j in 0..m.columns.len() {
                    assert_eq!(m.cell(&d, row.index, j).unwrap().cell_ref, (row.index, j));
                }
            }
        }
    }

    /// A pivot's inserted row is incomplete until EVERY column — the
    /// ladder and the slice values alike — is filled: a CVI term with a
    /// node missing is not a term the desk can price.
    #[test]
    fn a_pivot_inserted_row_is_incomplete_until_every_cell_is_filled() {
        let snapshot = Arc::new(full_grid());
        let mut d = Draft::default();
        d.insert_row("2027-01-15".into(), Some("2026-11-20".into()), &at(BASE));
        let m = MatrixIndex::build(&snapshot, &CVI, &d).unwrap();
        let labels: Vec<_> = m.rows().map(|r| r.label.to_string()).collect();
        assert_eq!(labels, ["2026-10-16", "2026-11-20", "2027-01-15"]);
        assert_eq!(m.state(2).unwrap(), RowState::Inserted);
        assert_eq!(m.columns.len(), 6, "three slice values and three nodes");
        assert!((0..6).all(|c| m.format_cell(&d, 2, c).as_ref() == "·"));
        assert!(m.cell(&d, 2, 6).is_none(), "nothing past the last column");
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
        let m = MatrixIndex::build(&snapshot, &CVI, &d).unwrap();
        assert_eq!(
            m.cell(&d, 2, 0).unwrap().text.as_ref(),
            "1.00",
            "fwd at its own format"
        );
        assert_eq!(
            m.cell(&d, 2, 3).unwrap().text.as_ref(),
            "0.2000",
            "a node at the panel's"
        );
    }

    /// Under `Columns::Values` only the `required` columns count: a row
    /// missing an optional column is complete.
    #[test]
    fn a_flat_inserted_row_is_complete_once_its_required_columns_are_filled() {
        let spec = PanelSpec {
            columns: Columns::Values(vec![
                ValueColumn {
                    column: "gross".into(),
                    label: "gross".into(),
                    ty: ColumnType::F64,
                    format: ColumnFormat::MEASURE,
                    choices: None,
                    required: true,
                },
                ValueColumn {
                    column: "net".into(),
                    label: "net".into(),
                    ty: ColumnType::F64,
                    format: ColumnFormat::MEASURE,
                    choices: None,
                    required: false,
                },
            ]),
            ..(**FLAT_SPEC).clone()
        };
        let m = MatrixIndex::build(&Arc::new(flat_snapshot()), &spec, &Draft::default()).unwrap();
        let mut d = Draft::default();
        d.insert_row("new-1".into(), None, &at(BASE));
        assert_eq!(d.incomplete_rows(&spec, &m.columns), 1);
        d.set_row_cell("new-1", "gross", Value::F64(1.0));
        assert_eq!(d.incomplete_rows(&spec, &m.columns), 0, "net is optional");
        assert_eq!(
            Draft::default().incomplete_rows(&spec, &m.columns),
            0,
            "no inserted rows, nothing incomplete"
        );
    }

    /// Refill inserted values from their draft rows and shifted document
    /// values through their document row, matching a full rebuild in both
    /// cases.
    #[test]
    fn patch_cell_matches_a_rebuild_with_rows_spliced() {
        use geode_tile::grid::WindowCache;
        let snapshot = Arc::new(schedule_snapshot(&[
            ("D1", "2026-12-18", 1.25, "declared"),
            ("D2", "2027-03-19", 0.5, "estimated"),
        ]));
        let mut draft = Draft::default();
        draft.insert_row("new-1".into(), None, &at("t0"));
        let index = MatrixIndex::build(&snapshot, &SCHEDULE, &draft).unwrap();
        assert_eq!(index.label(2).unwrap().as_ref(), "D2");
        let mut window = WindowCache::default();
        window.set_window(0..index.len(), index.columns.len(), |r, c| {
            index.md_cell(&draft, r, c)
        });
        // D2 is painted row 2 but document row 1 — its cell_ref says so, and
        // that is the key its edit lives under.
        let d2_ref = index.cell_ref((2, 1)).unwrap();
        assert_eq!(d2_ref, (1, 1));
        draft.set(
            d2_ref,
            ("D2".into(), "amount".into()),
            Value::F64(0.75),
            &at("t0"),
        );
        draft.set_row_cell("new-1", "amount", Value::F64(3.0));
        refill_cell(&mut window, &index, &draft, (2, 1));
        refill_cell(&mut window, &index, &draft, (0, 1));
        let rebuilt = MatrixIndex::build(&snapshot, &SCHEDULE, &draft).unwrap();
        for r in 0..rebuilt.len() {
            for c in 0..rebuilt.columns.len() {
                assert_eq!(
                    window.get(r, c).cloned(),
                    rebuilt.md_cell(&draft, r, c),
                    "({r}, {c})"
                );
            }
        }
        assert_eq!(window.get(2, 1).unwrap().text.as_ref(), "0.7500");
        assert_eq!(window.get(0, 1).unwrap().text.as_ref(), "3.0000");
        assert_eq!(
            window.get(1, 1).unwrap().text.as_ref(),
            "1.2500",
            "D1, at the inserted row's old index, is untouched"
        );
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

            let model = MatrixIndex::build(&Arc::new(document(&cells)), &CVI, &Draft::default())
                .expect("a complete grid pivots");
            prop_assert_eq!(
                model.rows().map(|r| r.label.to_string()).collect::<Vec<_>>(),
                want_rows.clone()
            );
            // The three slice columns lead; the ladder follows them.
            let s = model.slice_columns;
            prop_assert_eq!(s, 3);
            prop_assert_eq!(columns_of(&model)[s..].to_vec(), want_columns.clone());
            let clean = Draft::default();
            prop_assert_eq!(model.columns.len(), s + n);
            for i in 0..model.len() {
                for j in s..s + n {
                    let cell = model.cell(&clean, i, j).expect("a cell in range");
                    let want = want_value[&(want_rows[i].clone(), want_columns[j - s].clone())];
                    prop_assert_eq!(cell.value.clone(), Some(Value::F64(want)));
                    prop_assert_eq!(cell.cell_ref, (i, j));
                }
            }
        }
    }
}
