//! Edits over a delivered document, with labels for restoring and rebasing.
//!
//! Cell indices address the base document's grid; row and column labels
//! identify edits when another document changes those positions. Inserted
//! rows and attributes are keyed by label or column name directly.
//!
//! `base` identifies the document by [`DocumentBase`]: its per-document
//! source time and, when the read named one, the store generation behind it.
//! A different timestamp can mean a newer or a historical document; an equal
//! timestamp with a different generation is a corrected republish, which the
//! pair tells apart so it cannot re-point grid-keyed edits. Where no single
//! generation names the read, the timestamp alone decides.

use crate::core::matrix::{MatrixModel, RowState};
use crate::core::spec::{Columns, PanelSpec};
use geode_core::document::Value;
use geode_core::schema::ColumnType;
use gpui::SharedString;
use std::collections::{BTreeMap, HashMap};

/// Source time and optional store generation identifying the document a draft
/// was edited against. Both are needed to detect a corrected republish that
/// keeps its source time but changes the positions addressed by cell edits.
///
/// Production document reads report a generation for live and historical
/// results. A restored draft may omit `base_generation`; other snapshots may
/// also omit generation provenance. [`Self::differs_from`] then compares only
/// source time, which cannot detect a same-time republish. Exact equality of
/// the pair is stricter: it also distinguishes known from unknown generations.
/// The tile uses exact equality when retaining a base snapshot or reusing an
/// echo comparison.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentBase {
    /// RFC 3339.
    pub as_of: String,
    pub generation: Option<i64>,
}

impl DocumentBase {
    /// Whether `delivered` is a different document generation from this one.
    ///
    /// Differing source times always differ. Equal times differ only when
    /// both generations are known and disagree; see the type's own note on
    /// why an unknown generation cannot prove movement.
    pub fn differs_from(&self, delivered: &DocumentBase) -> bool {
        self.as_of != delivered.as_of
            || matches!(
                (self.generation, delivered.generation),
                (Some(mine), Some(theirs)) if mine != theirs
            )
    }
}

/// Where the draft stands against the document on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DraftState {
    /// No edits.
    #[default]
    Clean,
    /// Edits present, against the generation being painted.
    Editing,
    /// Edits present and a DIFFERENT generation has been delivered —
    /// usually a newer one, but an as-of step back delivers an older one
    /// and is the same situation. When available, the panel keeps painting
    /// the base generation under the edits; `:rebase` moves them onto the
    /// delivered one and `:revert` drops them. The edits' own base
    /// generation coming back (an as-of round trip, or a republish reverted
    /// upstream) returns the draft to `Editing` — see
    /// [`Draft::on_delivered`]. Identity is the pair, so a corrected
    /// republish at the same source time reaches this state too.
    Behind { newer: DocumentBase },
    /// The transport accepted the upload. Edits remain painted as sent
    /// while the tile checks subsequent deliveries for an echo; acceptance
    /// alone does not confirm upstream publication. `at` is the send time
    /// in RFC 3339, displayed as `sent HH:MM`.
    Sent { at: String },
}

/// Header state: a dot for `Dirty`, `update HH:MM` for `Behind`,
/// `sent HH:MM` for `Sent`, and nothing for `Clean`. Edit counts are
/// reported separately by [`Draft::count_phrase`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftBadge {
    Clean,
    Dirty,
    Behind { newer: String },
    Sent { at: String },
}

/// Per-tile handling of a different document delivered over edits.
/// [`Draft::on_delivered`] enters `Behind` without reading this policy;
/// the tile then holds, rebases, or replaces the draft. Clean drafts and
/// a return to the edits' own base bypass that choice. `Sent` drafts are
/// handled by the tile's echo comparison instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UpdatePolicy {
    /// Keep the available base document painted under the edits in `Behind`;
    /// `:rebase` and `:revert` resolve the pending delivery.
    #[default]
    Hold,
    /// Re-place the edits onto the new document at once, by label —
    /// exactly `:rebase`, dropped pairs named in the notice.
    Rebase,
    /// Drop the edits and paint the new document; the notice says how
    /// much unsent work went.
    Replace,
}

impl UpdatePolicy {
    /// Every policy, in the order the `:auto` completions and the menu
    /// section offer them.
    pub const ALL: [UpdatePolicy; 3] = [
        UpdatePolicy::Hold,
        UpdatePolicy::Rebase,
        UpdatePolicy::Replace,
    ];

    /// The typed and serialised spelling — one lowercase word.
    pub fn as_str(self) -> &'static str {
        match self {
            UpdatePolicy::Hold => "hold",
            UpdatePolicy::Rebase => "rebase",
            UpdatePolicy::Replace => "replace",
        }
    }

    /// The inverse of [`Self::as_str`]; `None` for anything else, so a
    /// session file carrying an unknown word restores as the default
    /// rather than refusing the tile.
    pub fn parse(word: &str) -> Option<UpdatePolicy> {
        Self::ALL.into_iter().find(|p| p.as_str() == word)
    }
}

/// A row insertion or deletion keyed by row label. Inserted cells use
/// column labels because their grid positions depend on model placement.
/// A deletion preserves the document row in the painted model as a marker.
#[derive(Debug, Clone, PartialEq)]
pub enum RowEdit {
    /// `after` names a document or inserted row; `None` means the top.
    /// Cells are keyed by column label so column reordering does not move
    /// an inserted value onto a different column.
    Inserted {
        after: Option<String>,
        cells: BTreeMap<String, Value>,
    },
    /// A document row marked for removal. The model retains it with a
    /// deleted marker; upload assembly excludes it from the sent document.
    Deleted,
}

/// What [`Draft::delete_row`] did, so a caller can say so: `Dropped` is an
/// `Inserted` row removed outright (there was nothing to send in the
/// first place), `Marked` is a document row now `Deleted`, `Already` is a
/// document row that already was.
#[derive(Debug, PartialEq, Eq)]
pub enum RowDelete {
    Dropped,
    Marked,
    Already,
}

/// Document-cell edits keyed by base-grid position, with labels for rebase;
/// attribute edits keyed by column name; row edits keyed by row label.
///
/// Rebase requires unique model row and column labels: duplicate labels
/// would merge distinct edit targets. The model rejects duplicate document
/// row identities, pivot pairs, and axis/slice label collisions. Compiled
/// panel specs must also supply distinct flat-column and slice labels.
/// Header attributes and inserted cells already carry their named identities.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    /// The generation the edits were made against. Edit operations set it
    /// when the draft is empty or has no base; restoration can omit the
    /// generation half.
    pub base: Option<DocumentBase>,
    pub edits: BTreeMap<(usize, usize), Value>,
    /// Document-level attribute edits, keyed by column name. Part of the
    /// same draft as `edits` (one base, one state) because both are unsent
    /// work against the same document generation.
    pub attrs: BTreeMap<String, Value>,
    /// Row insert/delete, keyed by row label. Part of the same draft as
    /// `edits`/`attrs` for the same reason: one base, one state, one
    /// `len()`, one `revert`.
    pub rows: BTreeMap<String, RowEdit>,
    /// Captured row counts for groups containing edited or deleted labels.
    /// Minted IDs such as `2026-09-18#2` are ordinals within a date: a changed
    /// nonzero count can redirect an edit to another dividend, so rebase drops
    /// that group's edits. Equal counts do not detect same-size reordering.
    ///
    /// [`Draft::capture_groups`] reads a clean base model supplied by the
    /// caller; [`Draft::set`] has no model from which to obtain the counts.
    /// An absent group count disables that group's guard.
    pub groups: BTreeMap<String, usize>,
    pub state: DraftState,
    /// (row label, column label) per edited cell. Private because it must
    /// never drift from `edits`: every door that writes one writes both.
    labels: BTreeMap<(usize, usize), (String, String)>,
}

/// Provisional column for restored label-based edits until rebase resolves
/// them against a model. Keeping them outside the grid prevents an unresolved
/// edit from painting on an arbitrary cell. The provisional row follows file
/// order to preserve serialization order.
const UNRESOLVED_COLUMN: usize = usize::MAX;

impl Draft {
    /// Edited cells alone — what a grid position is keyed by.
    pub fn cell_count(&self) -> usize {
        self.edits.len()
    }

    /// Edited header attributes alone.
    pub fn attr_count(&self) -> usize {
        self.attrs.len()
    }

    /// Cells, attributes and rows together — the one number that answers
    /// "is there unsent work".
    pub fn len(&self) -> usize {
        self.edits.len() + self.attrs.len() + self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.attrs.is_empty() && self.rows.is_empty()
    }

    pub fn is_sent(&self) -> bool {
        matches!(self.state, DraftState::Sent { .. })
    }

    pub fn is_behind(&self) -> bool {
        matches!(self.state, DraftState::Behind { .. })
    }

    /// Record an edit and its labels against the supplied document base. Capture
    /// the base when the draft is empty or has none; subsequent edits keep it so
    /// one draft cannot silently span multiple document generations.
    pub fn set(
        &mut self,
        cell: (usize, usize),
        labels: (String, String),
        value: Value,
        base: &DocumentBase,
    ) {
        if self.is_empty() || self.base.is_none() {
            self.base = Some(base.clone());
        }
        self.edits.insert(cell, value);
        self.labels.insert(cell, labels);
        // `Behind` survives an edit: the panel is still painting the base
        // generation, so a further edit is against the same document.
        // `Sent` does not — the draft no longer matches what was sent.
        if matches!(self.state, DraftState::Clean | DraftState::Sent { .. }) {
            self.state = DraftState::Editing;
        }
    }

    /// Borrow an existing `F64` or `I64` edit without converting its type.
    /// Other values and absent edits return `None`. Callers prefer this value
    /// over the underlying document so successive bumps compose.
    pub fn numeric_edit(&self, cell: (usize, usize)) -> Option<&Value> {
        match self.edits.get(&cell)? {
            value @ (Value::F64(_) | Value::I64(_)) => Some(value),
            Value::Utf8(_) | Value::Date(_) => None,
        }
    }

    /// Record one attribute edit — the same base rule as `set`.
    pub fn set_attr(&mut self, column: &str, value: Value, base: &DocumentBase) {
        if self.is_empty() || self.base.is_none() {
            self.base = Some(base.clone());
        }
        self.attrs.insert(column.to_string(), value);
        if matches!(self.state, DraftState::Clean | DraftState::Sent { .. }) {
            self.state = DraftState::Editing;
        }
    }

    /// Insert an empty row under `after` (`None` means the top), using the
    /// same base rule as [`Self::set`]. The caller supplies the identity,
    /// minting a temporary label or committing a typed row label.
    pub fn insert_row(&mut self, label: String, after: Option<String>, base: &DocumentBase) {
        if self.is_empty() || self.base.is_none() {
            self.base = Some(base.clone());
        }
        self.rows.insert(
            label,
            RowEdit::Inserted {
                after,
                cells: BTreeMap::new(),
            },
        );
        if matches!(self.state, DraftState::Clean | DraftState::Sent { .. }) {
            self.state = DraftState::Editing;
        }
    }

    /// Delete by label, using the same base rule as [`Self::set`]. An inserted
    /// row is removed; a document row is marked `Deleted`; an existing deletion
    /// returns `Already`. Removing an inserted row reanchors its followers to
    /// its own anchor, preserving the chain's position.
    pub fn delete_row(&mut self, label: &str, base: &DocumentBase) -> RowDelete {
        if self.is_empty() || self.base.is_none() {
            self.base = Some(base.clone());
        }
        if matches!(self.state, DraftState::Clean | DraftState::Sent { .. }) {
            self.state = DraftState::Editing;
        }
        let result = match self.rows.get(label) {
            Some(RowEdit::Inserted { after, .. }) => {
                let after = after.clone();
                self.rows.remove(label);
                self.rehang_followers(Some(label), after);
                RowDelete::Dropped
            }
            Some(RowEdit::Deleted) => RowDelete::Already,
            None => {
                self.rows.insert(label.to_string(), RowEdit::Deleted);
                RowDelete::Marked
            }
        };
        // Deleting the only inserted row can empty the draft after the base
        // and state were set above. Clear both so an insert followed by delete
        // does not leave a dirty badge or become `Behind` on delivery.
        if self.is_empty() {
            self.base = None;
            self.state = DraftState::Clean;
        }
        result
    }

    /// Write one cell of an `Inserted` row, keyed by the column's own
    /// label. Answers `false` for anything else — a document row (even a
    /// `Deleted` one) or a label with no row edit at all — since a cell
    /// edit on a row the draft did not insert belongs in `Draft::set`
    /// against the model's own grid index, not here.
    ///
    /// The same `Sent`-clears-on-a-further-edit rule `set` follows: a cell
    /// written into an inserted row after an upload is unsent work the
    /// echo has not seen, so the draft must not still read as sent once it
    /// exists.
    pub fn set_row_cell(&mut self, label: &str, column_label: &str, value: Value) -> bool {
        match self.rows.get_mut(label) {
            Some(RowEdit::Inserted { cells, .. }) => {
                cells.insert(column_label.to_string(), value);
                if matches!(self.state, DraftState::Clean | DraftState::Sent { .. }) {
                    self.state = DraftState::Editing;
                }
                true
            }
            _ => false,
        }
    }

    /// Rename an inserted row and move its followers to the new label.
    /// Return `false` if `from` is not inserted or `to` already names a draft
    /// row. Moving followers prevents a renamed temporary label from leaving
    /// dangling anchors that would place those rows at the top.
    pub fn rename_row(&mut self, from: &str, to: &str) -> bool {
        if !matches!(self.rows.get(from), Some(RowEdit::Inserted { .. })) {
            return false;
        }
        if self.rows.contains_key(to) {
            return false;
        }
        let edit = self.rows.remove(from).expect("checked above");
        self.rows.insert(to.to_string(), edit);
        self.rehang_followers(Some(from), Some(to.to_string()));
        true
    }

    /// Change an inserted row's anchor; return `false` for other rows.
    /// The tile uses this for insertion above an inserted row: the new row
    /// takes the old anchor and the old row follows the new one.
    pub fn reanchor_row(&mut self, label: &str, after: Option<String>) -> bool {
        match self.rows.get_mut(label) {
            Some(RowEdit::Inserted { after: anchor, .. }) => {
                *anchor = after;
                true
            }
            _ => false,
        }
    }

    /// Move every inserted row anchored on `from` onto `to`. Inserting into
    /// a chain puts existing followers under the new row; deleting a chain
    /// member hands its followers to its anchor. Normal insertion uses chains,
    /// while restored drafts and rebases can also contain sibling followers.
    pub fn rehang_followers(&mut self, from: Option<&str>, to: Option<String>) {
        for edit in self.rows.values_mut() {
            if let RowEdit::Inserted { after, .. } = edit
                && after.as_deref() == from
            {
                *after = to.clone();
            }
        }
    }

    /// The row edit at `label`, if any.
    pub fn row_state(&self, label: &str) -> Option<&RowEdit> {
        self.rows.get(label)
    }

    /// How many rows this draft inserts.
    pub fn rows_added(&self) -> usize {
        self.rows
            .values()
            .filter(|e| matches!(e, RowEdit::Inserted { .. }))
            .count()
    }

    /// How many rows this draft marks deleted.
    pub fn rows_removed(&self) -> usize {
        self.rows
            .values()
            .filter(|e| matches!(e, RowEdit::Deleted))
            .count()
    }

    /// Count inserted rows missing a required cell among the model's columns.
    /// Flat columns use [`ValueColumn::required`](crate::core::spec::ValueColumn);
    /// every pivot column is required, including slice values. This counts
    /// presence only, without type validation. Upload assembly separately
    /// requires every value it serializes, including optional flat columns.
    pub fn incomplete_rows(&self, spec: &PanelSpec, columns: &[SharedString]) -> usize {
        let required = |label: &str| match &spec.columns {
            Columns::Axis(_) => true,
            Columns::Values(cols) => cols.iter().any(|c| c.label == label && c.required),
        };
        self.rows
            .values()
            .filter(|e| match e {
                RowEdit::Inserted { cells, .. } => columns
                    .iter()
                    .any(|c| required(c.as_ref()) && !cells.contains_key(c.as_ref())),
                RowEdit::Deleted => false,
            })
            .count()
    }

    /// The smallest `new-<n>` (`n` starting at 1) that `taken` (the
    /// model's own rows, or whatever else a caller wants to avoid) does
    /// not name and that is not CURRENTLY a key of this draft's own
    /// `rows` — so a mint never collides with a row already on screen or
    /// already pending in this very draft. A number freed by renaming or
    /// dropping the row that first took it CAN be re-minted: that is
    /// harmless, since by then it is minting a fresh row with no history
    /// of its own, not reusing one still live.
    pub fn mint_label(&self, taken: impl Fn(&str) -> bool) -> String {
        let mut n = 1usize;
        loop {
            let candidate = format!("new-{n}");
            if !taken(&candidate) && !self.rows.contains_key(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Drop every edit, attribute and row, answering how many there were
    /// in total. The draft is `Clean` afterwards and carries no base,
    /// since a base describes a set of edits.
    pub fn revert(&mut self) -> usize {
        let n = self.len();
        self.edits.clear();
        self.labels.clear();
        self.attrs.clear();
        self.rows.clear();
        self.groups.clear();
        self.base = None;
        self.state = DraftState::Clean;
        n
    }

    /// Add `delta` to the supplied current values, returning the number of
    /// writes. Validate all results before changing the draft so a fractional
    /// bump refused by an integer column cannot leave a partially bumped row.
    ///
    /// The caller supplies numeric candidates and their declared types, with
    /// existing edits already included in each current value. [`bumped`] chooses
    /// the result's type and refuses fractional deltas for integer columns.
    pub fn bump(
        &mut self,
        cells: impl Iterator<Item = ((usize, usize), (String, String), Value, ColumnType)>,
        delta: f64,
        base: &DocumentBase,
    ) -> Result<usize, String> {
        // Compute every result before writing any edit: a typing refusal
        // partway through must leave the whole draft unchanged.
        let mut writes = Vec::new();
        for (cell, labels, current, ty) in cells {
            let value = bumped(&current, delta, ty, &labels.1)?;
            writes.push((cell, labels, value));
        }
        let n = writes.len();
        for (cell, labels, value) in writes {
            self.set(cell, labels, value, base);
        }
        Ok(n)
    }

    /// Process delivery and report whether the draft state changed. Editing
    /// becomes Behind when the delivery differs from the base; Behind returns
    /// to Editing when the base is delivered again. Further differing deliveries
    /// update the Behind target. Clean and Sent states are unchanged here.
    ///
    /// [`DocumentBase::differs_from`] compares source times and, when both are
    /// known, store generations. Missing generations fall back to source time,
    /// so a same-time republish is detectable only when both generations are known.
    /// The tile handles Sent echoes separately.
    pub fn on_delivered(&mut self, delivered: &DocumentBase) -> bool {
        match &self.state {
            DraftState::Editing
                if self
                    .base
                    .as_ref()
                    .is_none_or(|base| base.differs_from(delivered)) =>
            {
                self.state = DraftState::Behind {
                    newer: delivered.clone(),
                };
                true
            }
            // Returning to the base generation restores editing without moving
            // edits. Check this before updating the pending delivery.
            DraftState::Behind { .. }
                if self
                    .base
                    .as_ref()
                    .is_some_and(|base| !base.differs_from(delivered)) =>
            {
                self.state = DraftState::Editing;
                true
            }
            // Keep the badge on the latest delivered generation, which may be
            // historical when the user changes as-of.
            DraftState::Behind { newer } if newer != delivered => {
                self.state = DraftState::Behind {
                    newer: delivered.clone(),
                };
                true
            }
            _ => false,
        }
    }

    /// Capture counts for groups touched by cell edits or deleted rows.
    /// `base` must be a clean model of the document these edits refer to;
    /// that association is the caller's responsibility. Replace the counts
    /// rather than merging them so rebase cannot retain stale base counts.
    pub fn capture_groups(&mut self, base: &MatrixModel) {
        let now = group_sizes(base);
        let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (row_label, _col_label) in self.labels.values() {
            touched.insert(group_of(row_label).to_string());
        }
        for (label, edit) in &self.rows {
            if matches!(edit, RowEdit::Deleted) {
                touched.insert(group_of(label).to_string());
            }
        }
        self.groups = now
            .into_iter()
            .filter(|(g, _)| touched.contains(g))
            .collect();
    }

    /// Reapply edits to a delivered document by label, returning the number
    /// kept and descriptions of dropped edits or changed anchors. Matching
    /// labels moves an edit with its row or column when grid positions change.
    ///
    /// `model_of_newer` must be built with [`Draft::default()`]. A model with
    /// this draft would mistake inserted rows for upstream conflicts and would
    /// resolve document cells to painted positions shifted by inserts.
    pub fn rebase(&mut self, model_of_newer: &MatrixModel) -> (usize, Vec<(String, String)>) {
        // The two axes are indexed separately — O(R + C), not the R × C
        // every cell pair would cost, which at a 10,000-row schedule is
        // 50,000 entries built to resolve a few hundred edits. A model's
        // grid is rectangular, so a row that exists and a column that
        // exists are a cell that exists; both maps are unique by the
        // invariant on this struct.
        let rows: HashMap<&str, usize> = model_of_newer
            .rows
            .iter()
            .enumerate()
            .map(|(ri, row)| (row.label.as_ref(), ri))
            .collect();
        let columns: HashMap<&str, usize> = model_of_newer
            .columns
            .iter()
            .enumerate()
            .map(|(ci, column)| (column.as_ref(), ci))
            .collect();

        // Compare captured group counts before resolving labels. A surviving
        // group with a different size may assign the same ordinal to a different
        // dividend even when the edited label still exists.
        let now = group_sizes(model_of_newer);

        let mut edits = BTreeMap::new();
        let mut labels = BTreeMap::new();
        let mut dropped = Vec::new();
        for (cell, value) in &self.edits {
            // `set` writes both maps, so a missing label is unreachable;
            // if it ever happened, the edit has no identity to re-place
            // and is reported dropped rather than guessed at.
            let Some((row_label, col_label)) = self.labels.get(cell) else {
                dropped.push((format!("row {}", cell.0), format!("column {}", cell.1)));
                continue;
            };
            // A same-date group this draft captured a size for, whose
            // size in the newer document differs — but only while the
            // group still has SOME row in the newer document: a group
            // gone to zero means `row_label` cannot resolve either way,
            // and the ordinary "no target" branch below already reports
            // that (by column, the more specific of the two, e.g. a lone
            // node dropped from a term that otherwise survives — see
            // `rebase_moves_an_edit_to_its_new_index_by_label_and_reports_a_dropped_one`).
            // A NONZERO size that differs is the real hazard: the
            // group's ordinals shifted, so `row_label` may now name a
            // DIFFERENT row than the one this edit was made against.
            // Checked before the label even looks for a target cell, and
            // named rather than silently carried onto whatever
            // `row_label` resolves to.
            if let Some(&was) = self.groups.get(group_of(row_label)) {
                let size_now = now.get(group_of(row_label)).copied().unwrap_or(0);
                if size_now != 0 && was != size_now {
                    dropped.push((
                        row_label.clone(),
                        format!("row (same-day rows changed: {was} → {size_now})"),
                    ));
                    continue;
                }
            }
            let target = rows
                .get(row_label.as_str())
                .zip(columns.get(col_label.as_str()))
                .map(|(&ri, &ci)| (ri, ci));
            match target {
                Some(new_cell) => {
                    edits.insert(new_cell, value.clone());
                    labels.insert(new_cell, (row_label.clone(), col_label.clone()));
                }
                None => dropped.push((row_label.clone(), col_label.clone())),
            }
        }

        self.edits = edits;
        self.labels = labels;

        // A header attribute's identity is its column NAME — the same
        // name every generation of one document carries it under — so
        // there is no label to resolve, only a declared/not-declared
        // check against the newer model's own header.
        let declared: std::collections::HashSet<&str> = model_of_newer
            .header
            .iter()
            .map(|h| h.column.as_ref())
            .collect();
        let mut attrs = BTreeMap::new();
        for (column, value) in std::mem::take(&mut self.attrs) {
            if declared.contains(column.as_str()) {
                attrs.insert(column, value);
            } else {
                dropped.push((column, "attribute".to_string()));
            }
        }
        self.attrs = attrs;

        // Resolve row edits against document labels and surviving inserts.
        // Precompute the latter because a chain's anchor may sort after its
        // follower. If an insert conflicts with an upstream row, followers can
        // still anchor on that label through the real document row. Own these
        // labels because the loop consumes `self.rows`.
        let surviving: std::collections::HashSet<String> = self
            .rows
            .iter()
            .filter(|(label, edit)| {
                matches!(edit, RowEdit::Inserted { .. }) && !rows.contains_key(label.as_str())
            })
            .map(|(label, _)| label.clone())
            .collect();
        let anchor_survives =
            |anchor: &str| rows.contains_key(anchor) || surviving.contains(anchor);
        let mut rows_out = BTreeMap::new();
        for (label, edit) in std::mem::take(&mut self.rows) {
            match edit {
                // Deleted, and the newer document still has it: keep
                // marking it deleted. Deleted, and the newer document
                // dropped the row itself: there is nothing left to
                // delete, so the edit is dropped and named.
                RowEdit::Deleted => {
                    // Same guard as a cell edit's, above, and for the
                    // same reason and the same zero exception: `label` is
                    // what the newer document is checked against just
                    // below, and a NONZERO same-date group size that
                    // moved means this label may no longer name the row
                    // the trader marked deleted; a group gone to zero
                    // leaves that check below to report it plainly.
                    if let Some(&was) = self.groups.get(group_of(&label)) {
                        let size_now = now.get(group_of(&label)).copied().unwrap_or(0);
                        if size_now != 0 && was != size_now {
                            dropped.push((
                                label,
                                format!("row (same-day rows changed: {was} → {size_now})"),
                            ));
                            continue;
                        }
                    }
                    if rows.contains_key(label.as_str()) {
                        rows_out.insert(label, RowEdit::Deleted);
                    } else {
                        dropped.push((label, "row".to_string()));
                    }
                }
                // Inserted, and the newer document now carries a row
                // under this very label: upstream got there first, so
                // this draft's own insert is dropped and named as a
                // conflict rather than silently shadowing the real row.
                // Otherwise it survives, but its anchor might not have:
                // an `after` label that is neither a row of the newer
                // document nor a surviving inserted row re-anchors to the
                // top and is named too.
                RowEdit::Inserted { after, cells } => {
                    if rows.contains_key(label.as_str()) {
                        dropped.push((label, "row (the document now carries it)".to_string()));
                    } else {
                        let after = match after {
                            Some(anchor) if !anchor_survives(&anchor) => {
                                dropped.push((label.clone(), format!("anchor '{anchor}'")));
                                None
                            }
                            other => other,
                        };
                        rows_out.insert(label, RowEdit::Inserted { after, cells });
                    }
                }
            }
        }
        self.rows = rows_out;

        // The newer document is the new base: its own group sizes,
        // restricted to whatever survived above, are what the NEXT
        // rebase must compare against — not the sizes this one started
        // with, which describe a document no longer on screen.
        self.capture_groups(model_of_newer);

        self.base = model_of_newer.base.clone();
        self.state = if self.is_empty() {
            DraftState::Clean
        } else {
            DraftState::Editing
        };
        (self.len(), dropped)
    }

    /// The header state without counts. [`Self::count_phrase`] supplies the
    /// edit summary used by notices and confirmations.
    pub fn badge(&self) -> DraftBadge {
        match &self.state {
            DraftState::Clean => DraftBadge::Clean,
            DraftState::Editing => DraftBadge::Dirty,
            DraftState::Behind { newer } => DraftBadge::Behind {
                newer: newer.as_of.clone(),
            },
            DraftState::Sent { at } => DraftBadge::Sent { at: at.clone() },
        }
    }

    /// "3 cells, 1 row added, 1 row removed, spot_ref" — the unsent work
    /// named for a notice or a confirm, in that fixed order: cells (a
    /// count, since a cell has no name worth showing), rows added, rows
    /// removed, then every edited attribute's own column name. Each part
    /// appears only when non-zero.
    pub fn count_phrase(&self) -> String {
        let mut parts = Vec::new();
        match self.edits.len() {
            0 => {}
            1 => parts.push("1 cell".to_string()),
            n => parts.push(format!("{n} cells")),
        }
        match self.rows_added() {
            0 => {}
            1 => parts.push("1 row added".to_string()),
            n => parts.push(format!("{n} rows added")),
        }
        match self.rows_removed() {
            0 => {}
            1 => parts.push("1 row removed".to_string()),
            n => parts.push(format!("{n} rows removed")),
        }
        parts.extend(self.attrs.keys().cloned());
        parts.join(", ")
    }

    /// Serialize the base, edits, rows, and group guards. Cell positions are
    /// label pairs, never grid indices; restoration resolves them by rebase.
    /// Draft state and transport/echo status are not serialized.
    pub fn to_toml(&self) -> toml::Table {
        let mut table = toml::Table::new();
        if let Some(base) = &self.base {
            table.insert("base".into(), toml::Value::String(base.as_of.clone()));
            // Preserve known generations in both saved sessions and parked drafts.
            // Omit unknown generations rather than inventing an ID. Losing a known
            // ID would prevent detection of same-time republishes after restoration.
            if let Some(generation) = base.generation {
                table.insert("base_generation".into(), toml::Value::Integer(generation));
            }
        }
        let edits = self
            .edits
            .iter()
            .filter_map(|(cell, value)| {
                let (row_label, col_label) = self.labels.get(cell)?;
                Some(toml::Value::Array(vec![
                    toml::Value::String(row_label.clone()),
                    toml::Value::String(col_label.clone()),
                    value_to_toml(value),
                ]))
            })
            .collect();
        table.insert("edits".into(), toml::Value::Array(edits));
        if !self.attrs.is_empty() {
            let mut attrs = toml::Table::new();
            for (column, value) in &self.attrs {
                attrs.insert(
                    column.clone(),
                    match value {
                        Value::F64(f) => toml::Value::Float(*f),
                        Value::I64(i) => toml::Value::Integer(*i),
                        Value::Utf8(s) => toml::Value::String(s.clone()),
                        Value::Date(d) => toml::Value::String(d.format("%Y-%m-%d").to_string()),
                    },
                );
            }
            table.insert("attrs".into(), toml::Value::Table(attrs));
        }
        if !self.rows.is_empty() {
            let mut rows = toml::Table::new();
            for (label, edit) in &self.rows {
                let mut row = toml::Table::new();
                match edit {
                    RowEdit::Inserted { after, cells } => {
                        if let Some(after) = after {
                            row.insert("after".into(), toml::Value::String(after.clone()));
                        }
                        let mut cells_table = toml::Table::new();
                        for (column_label, value) in cells {
                            cells_table.insert(column_label.clone(), value_to_toml(value));
                        }
                        row.insert("cells".into(), toml::Value::Table(cells_table));
                    }
                    RowEdit::Deleted => {
                        row.insert("deleted".into(), toml::Value::Boolean(true));
                    }
                }
                rows.insert(label.clone(), toml::Value::Table(row));
            }
            table.insert("rows".into(), toml::Value::Table(rows));
        }
        if !self.groups.is_empty() {
            let mut groups = toml::Table::new();
            for (group, size) in &self.groups {
                groups.insert(group.clone(), toml::Value::Integer(*size as i64));
            }
            table.insert("groups".into(), toml::Value::Table(groups));
        }
        table
    }

    /// Restore a session draft, skipping malformed entries individually.
    /// Cell edits wait at [`UNRESOLVED_COLUMN`] until rebase resolves their
    /// labels. Restored content determines `Clean` versus `Editing`; sent
    /// status is not restored.
    ///
    /// Cell dates and text use explicit tags. Attribute strings instead parse
    /// as dates when they match `%Y-%m-%d`, and as text otherwise. This is
    /// ambiguous for a text attribute containing a date-shaped string: it
    /// restores as [`Value::Date`], regardless of its original type.
    pub fn from_toml(t: &toml::Table) -> Draft {
        let base = t
            .get("base")
            .and_then(|v| v.as_str())
            .map(|as_of| DocumentBase {
                as_of: as_of.to_string(),
                generation: t.get("base_generation").and_then(|v| v.as_integer()),
            });
        let mut edits = BTreeMap::new();
        let mut labels = BTreeMap::new();
        let rows = t.get("edits").and_then(|v| v.as_array());
        for (i, entry) in rows
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let Some(triple) = entry.as_array() else {
                continue;
            };
            if triple.len() != 3 {
                continue;
            }
            let (Some(row_label), Some(col_label), Some(value)) = (
                triple[0].as_str(),
                triple[1].as_str(),
                value_from_toml(&triple[2]),
            ) else {
                continue;
            };
            let cell = (i, UNRESOLVED_COLUMN);
            edits.insert(cell, value);
            labels.insert(cell, (row_label.to_string(), col_label.to_string()));
        }
        let mut attrs = BTreeMap::new();
        if let Some(toml::Value::Table(attr_table)) = t.get("attrs") {
            for (column, value) in attr_table {
                let value = match value {
                    toml::Value::Float(f) => Value::F64(*f),
                    toml::Value::Integer(i) => Value::I64(*i),
                    toml::Value::String(s) => {
                        match chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                            Ok(d) => Value::Date(d),
                            Err(_) => Value::Utf8(s.clone()),
                        }
                    }
                    // A malformed attribute value is skipped, the same
                    // rule a malformed cell edit above follows.
                    _ => continue,
                };
                attrs.insert(column.clone(), value);
            }
        }
        // Row edits are separate from the top-level cell-edit array.
        let mut row_edits = BTreeMap::new();
        if let Some(toml::Value::Table(rows_table)) = t.get("rows") {
            for (label, entry) in rows_table {
                let Some(row) = entry.as_table() else {
                    continue;
                };
                if row.get("deleted").and_then(|v| v.as_bool()) == Some(true) {
                    row_edits.insert(label.clone(), RowEdit::Deleted);
                    continue;
                }
                let after = row
                    .get("after")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let mut cells = BTreeMap::new();
                if let Some(toml::Value::Table(cells_table)) = row.get("cells") {
                    for (column_label, value) in cells_table {
                        // A malformed cell is skipped, the same rule the
                        // top-level `edits` loop above follows.
                        if let Some(value) = value_from_toml(value) {
                            cells.insert(column_label.clone(), value);
                        }
                    }
                }
                row_edits.insert(label.clone(), RowEdit::Inserted { after, cells });
            }
        }
        // An absent group table restores no guards; rebase then has no base
        // count against which to check a label's group.
        let mut groups = BTreeMap::new();
        if let Some(toml::Value::Table(groups_table)) = t.get("groups") {
            for (group, size) in groups_table {
                if let Some(size) = size.as_integer() {
                    groups.insert(group.clone(), size as usize);
                }
            }
        }
        let state = if edits.is_empty() && attrs.is_empty() && row_edits.is_empty() {
            DraftState::Clean
        } else {
            DraftState::Editing
        };
        Draft {
            base,
            edits,
            attrs,
            rows: row_edits,
            groups,
            state,
            labels,
        }
    }
}

/// A cell edit's value on the wire: a number bare (`toml::Value::Float`/
/// `Integer`, matching whichever `to_toml` wrote), a date or text edit
/// TAGGED — see [`tagged`] — so a restored `2026-12-18` cannot be
/// confused with the text edit `"2026-12-18"` a trader might just as
/// well have typed into a `Utf8` cell.
fn value_to_toml(value: &Value) -> toml::Value {
    match value {
        Value::F64(f) => toml::Value::Float(*f),
        Value::I64(i) => toml::Value::Integer(*i),
        Value::Date(d) => tagged("date", d.format("%Y-%m-%d").to_string()),
        Value::Utf8(s) => tagged("text", s.clone()),
    }
}

/// `{ type = "<ty>", value = "<value>" }` — a date and a text edit are
/// both strings on the wire, and only a tag keeps a restored
/// `2026-12-18` from reading back as text.
fn tagged(ty: &str, value: String) -> toml::Value {
    let mut t = toml::Table::new();
    t.insert("type".into(), toml::Value::String(ty.to_string()));
    t.insert("value".into(), toml::Value::String(value));
    toml::Value::Table(t)
}

/// The inverse of [`value_to_toml`]: a bare `Float`/`Integer` reads back
/// as the matching numeric variant, a `{ type, value }` table as the
/// date or text it tags. A bare `String` — the shape [`attr_text`]'s
/// sibling below writes for an attribute, never a cell edit — is refused
/// rather than guessed: a cell edit's date or text is only ever spelled
/// tagged, so an untagged string here is malformed, skipped the same as
/// any other unreadable entry.
fn value_from_toml(value: &toml::Value) -> Option<Value> {
    match value {
        toml::Value::Float(f) => Some(Value::F64(*f)),
        toml::Value::Integer(i) => Some(Value::I64(*i)),
        toml::Value::Table(t) => {
            let ty = t.get("type")?.as_str()?;
            let text = t.get("value")?.as_str()?;
            match ty {
                "date" => chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                    .ok()
                    .map(Value::Date),
                "text" => Some(Value::Utf8(text.to_string())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Display an RFC 3339 time on the configured clock. Preserve unreadable
/// input verbatim so an invalid timestamp is visible in the badge.
pub(crate) fn local_hhmm(rfc3339: &str, clock: geode_core::clock::Clock) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(t) => clock.hm(t.to_utc()),
        Err(_) => rfc3339.to_string(),
    }
}

/// Add `delta` using the column's declared numeric type.
///
/// For an `I64` current value, checked integer addition preserves values above
/// 2^53 without conversion to `f64`. Fractional deltas, fractional current
/// values, nonnumeric values, and integer addition overflow are refused.
/// Whole finite `F64` current values are also accepted for integer columns.
/// The delta arrives as `f64`, so precision already lost while parsing it
/// cannot be recovered here. Floating columns use floating-point addition.
pub fn bumped(current: &Value, delta: f64, ty: ColumnType, column: &str) -> Result<Value, String> {
    match ty {
        ColumnType::F64 => match current {
            Value::F64(v) => Ok(Value::F64(v + delta)),
            Value::I64(v) => Ok(Value::F64(*v as f64 + delta)),
            other => Err(format!("bump: {column} holds {other:?}, not a number")),
        },
        ColumnType::I64 if delta.fract() != 0.0 => {
            Err(format!("bump: {column} takes whole numbers"))
        }
        ColumnType::I64 => {
            let current = match current {
                Value::I64(v) => *v,
                // A whole-valued double in an integer column is the same
                // number; anything else would have to be truncated, and
                // `:bump` does not silently change a holding.
                Value::F64(v) if v.fract() == 0.0 && v.is_finite() => *v as i64,
                Value::F64(v) => {
                    return Err(format!("bump: {column} holds a fractional value ({v})"));
                }
                other => return Err(format!("bump: {column} holds {other:?}, not a number")),
            };
            // Convert the already-parsed whole delta to `i64`. Precision lost in
            // its `f64` representation cannot be recovered by this conversion.
            current
                .checked_add(delta as i64)
                .map(Value::I64)
                .ok_or_else(|| format!("bump: {column} would be too large"))
        }
        other => Err(format!("bump: {column} is not numeric ({other:?})")),
    }
}

/// The group prefix before the first `#`, or the full label without one.
/// For minted dividend IDs (`<date>`, `<date>#2`, ...), this is the ex-date;
/// the function itself does not validate date syntax.
pub fn group_of(label: &str) -> &str {
    match label.split_once('#') {
        Some((group, _)) => group,
        None => label,
    }
}

/// Count document and deleted rows per label group. A deleted row still
/// belongs to the upstream document; an inserted row has no upstream
/// ordinal. Guard capture normally supplies a clean model.
pub fn group_sizes(model: &MatrixModel) -> BTreeMap<String, usize> {
    let mut sizes = BTreeMap::new();
    for row in &model.rows {
        if matches!(row.state, RowState::Document | RowState::Deleted) {
            *sizes.entry(group_of(&row.label).to_string()).or_insert(0) += 1;
        }
    }
    sizes
}

/// Parse trimmed text as the declared numeric type. Integer columns parse
/// directly to `I64`, preserving values that `f64` cannot represent exactly.
/// Floating columns require a finite `F64`. Refuse nonnumeric types; callers
/// route text, choice, and date cells separately. Errors retain the input.
pub fn parse_cell(text: &str, ty: ColumnType) -> Result<Value, String> {
    let trimmed = text.trim();
    match ty {
        ColumnType::F64 => {
            let value: f64 = trimmed
                .parse()
                .map_err(|_| format!("'{text}' is not a number"))?;
            if !value.is_finite() {
                return Err(format!("'{text}' is not a finite number"));
            }
            Ok(Value::F64(value))
        }
        ColumnType::I64 => trimmed
            .parse::<i64>()
            .map(Value::I64)
            .map_err(|_| format!("'{text}' is not a whole number")),
        ColumnType::Utf8 | ColumnType::Date | ColumnType::Timestamp | ColumnType::Bool => Err(
            format!("'{text}' cannot be entered here — not a numeric cell"),
        ),
    }
}

/// Parse an attribute at its declared type: finite number, integer, date,
/// or text. Bool and timestamp attributes are unsupported and refused.
pub fn parse_attr(text: &str, ty: ColumnType) -> Result<Value, String> {
    let trimmed = text.trim();
    match ty {
        ColumnType::Date => chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
            .map(Value::Date)
            .map_err(|_| format!("'{text}' is not a date (YYYY-MM-DD)")),
        ColumnType::F64 | ColumnType::I64 => parse_cell(text, ty),
        ColumnType::Utf8 if trimmed.is_empty() => Err("a value is required".to_string()),
        ColumnType::Utf8 => Ok(Value::Utf8(trimmed.to_string())),
        other => Err(format!("a {other:?} attribute is not editable")),
    }
}

/// The document's own spelling of an attribute (what `label_at` yields for
/// the delivered value), so an edited value paints in the same shape.
pub fn attr_text(value: &Value) -> String {
    match value {
        Value::F64(f) => format!("{f}"),
        Value::I64(i) => i.to_string(),
        Value::Utf8(s) => s.clone(),
        Value::Date(d) => d.format("%Y-%m-%d").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::matrix::{Cell, HeaderCell, MatrixModel, RowModel, RowState};
    use chrono::NaiveDate;
    use geode_core::document::Value;
    use geode_core::schema::ColumnType;
    use gpui::SharedString;
    use proptest::prelude::*;

    const BASE: &str = "2026-09-12T14:02:00Z";
    const NEWER: &str = "2026-09-12T14:07:00Z";

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// Same as `d`, spelled out — the row tests below bind `d` to a
    /// `Draft` and need a date constructor `d` would shadow.
    fn date(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// A flat model naming only the given row labels, no columns and no
    /// header — `rebase`'s row handling reads a newer model's row labels
    /// alone (see [`Draft::rebase`]'s doc comment).
    fn flat_model_with_rows(labels: &[&str]) -> MatrixModel {
        MatrixModel {
            rows: labels
                .iter()
                .map(|label| RowModel {
                    label: SharedString::from(*label),
                    cells: Vec::new(),
                    state: RowState::Document,
                })
                .collect(),
            ..MatrixModel::default()
        }
    }

    /// A model whose header names the given `(column, label)` pairs and
    /// nothing else — `rebase` only ever reads a model's `header`, `rows`
    /// and `columns`.
    fn model_with_header(attrs: &[(&str, &str)]) -> MatrixModel {
        MatrixModel {
            header: attrs
                .iter()
                .map(|(c, l)| HeaderCell {
                    column: (*c).into(),
                    label: (*l).into(),
                    text: "".into(),
                    edited: false,
                })
                .collect(),
            ..MatrixModel::default()
        }
    }

    fn pair(row: &str, col: &str) -> (String, String) {
        (row.to_string(), col.to_string())
    }

    fn at(as_of: &str) -> DocumentBase {
        DocumentBase {
            as_of: as_of.to_string(),
            generation: None,
        }
    }

    fn at_gen(as_of: &str, generation: i64) -> DocumentBase {
        DocumentBase {
            as_of: as_of.to_string(),
            generation: Some(generation),
        }
    }

    #[test]
    fn a_same_time_republish_is_a_different_generation_when_both_ids_are_known() {
        // Equal source times with different known generations identify a
        // republish. Position-keyed edits must not follow the changed grid.
        assert!(at_gen(BASE, 7).differs_from(&at_gen(BASE, 8)));
        assert!(!at_gen(BASE, 7).differs_from(&at_gen(BASE, 7)));
        // A different time always differs, generations or not.
        assert!(at_gen(BASE, 7).differs_from(&at_gen(NEWER, 7)));
        assert!(at(BASE).differs_from(&at(NEWER)));
        // When either generation is unknown, only a source-time difference
        // can establish a change. Equal times cannot rule out a republish.
        assert!(!at(BASE).differs_from(&at_gen(BASE, 9)));
        assert!(!at_gen(BASE, 9).differs_from(&at(BASE)));
    }

    #[test]
    fn on_delivered_goes_behind_on_a_same_time_republish() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), &at_gen(BASE, 7));
        assert!(
            !draft.on_delivered(&at_gen(BASE, 7)),
            "the same generation redelivered changes nothing"
        );
        assert!(
            draft.on_delivered(&at_gen(BASE, 8)),
            "a republish at the same source time moved the document"
        );
        assert_eq!(
            draft.state,
            DraftState::Behind {
                newer: at_gen(BASE, 8)
            }
        );
        assert!(
            draft.on_delivered(&at_gen(BASE, 7)),
            "the edits' own generation coming back restores Editing"
        );
        assert_eq!(draft.state, DraftState::Editing);
    }

    /// A model with the given row and column labels and no values — the
    /// draft only ever reads a model's labels and its base.
    fn model(rows: &[&str], cols: &[&str], base: &DocumentBase) -> MatrixModel {
        MatrixModel {
            key: vec!["SPX.Z".to_string()],
            base: Some(base.clone()),
            header: Vec::new(),
            slice_columns: 0,
            column_values: Vec::new(),
            // `rebase` never reads a column's `CellKind` — only its label
            // — so an empty vec here is honest, not a shortcut.
            column_kinds: Vec::new(),
            columns: cols
                .iter()
                .map(|c| SharedString::from(c.to_string()))
                .collect(),
            rows: rows
                .iter()
                .enumerate()
                .map(|(r, label)| RowModel {
                    label: SharedString::from(label.to_string()),
                    cells: (0..cols.len())
                        .map(|c| Cell {
                            text: SharedString::default(),
                            value: None,
                            edited: false,
                            sent: false,
                            cell_ref: (r, c),
                        })
                        .collect(),
                    state: RowState::Document,
                })
                .collect(),
            pivot_index: None,
        }
    }

    #[test]
    fn the_first_edit_records_the_base_and_a_second_edit_on_one_cell_keeps_the_latest() {
        let mut draft = Draft::default();
        assert_eq!(draft.state, DraftState::Clean);
        draft.set((0, 1), pair("T1", "-1"), Value::F64(0.5), &at(BASE));
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.base.as_ref(), Some(&at(BASE)));
        draft.set((0, 1), pair("T1", "-1"), Value::F64(0.7), &at(BASE));
        assert_eq!(draft.edits.len(), 1);
        assert_eq!(draft.edits.get(&(0, 1)), Some(&Value::F64(0.7)));
        assert_eq!(
            draft.base.as_ref(),
            Some(&at(BASE)),
            "every edit in one draft is against one generation"
        );
    }

    #[test]
    fn on_delivered_stays_editing_on_the_same_generation_and_goes_behind_on_a_newer_one() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), &at(BASE));

        assert!(
            !draft.on_delivered(&at(BASE)),
            "the same document redelivered changes nothing"
        );
        assert_eq!(draft.state, DraftState::Editing);

        assert!(draft.on_delivered(&at(NEWER)));
        assert_eq!(draft.state, DraftState::Behind { newer: at(NEWER) });
        assert_eq!(draft.edits.len(), 1, "a newer document never clobbers work");
        assert_eq!(draft.edits.get(&(0, 0)), Some(&Value::F64(1.0)));
        assert_eq!(
            draft.base.as_ref(),
            Some(&at(BASE)),
            "the base still names what is painted"
        );
        assert!(
            !draft.on_delivered(&at(NEWER)),
            "the same newer document again is not a fresh transition"
        );
    }

    /// With unknown generation IDs, returning to the base source time restores
    /// Editing without moving or dropping edits.
    #[test]
    fn the_base_generation_redelivered_brings_a_behind_draft_back_to_editing() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), &at(BASE));
        let older = "2026-09-12T09:00:00Z";

        assert!(
            draft.on_delivered(&at(older)),
            "an as-of step back is Behind"
        );
        assert_eq!(draft.state, DraftState::Behind { newer: at(older) });

        assert!(
            draft.on_delivered(&at(BASE)),
            "the base coming back is a real transition"
        );
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.edits.len(), 1, "the edits are untouched");
        assert_eq!(draft.edits.get(&(0, 0)), Some(&Value::F64(1.0)));
        assert_eq!(draft.base.as_ref(), Some(&at(BASE)));
        assert!(
            !draft.on_delivered(&at(BASE)),
            "and the base again is no transition at all"
        );
    }

    /// Delivery leaves a sent draft in `Sent` for the tile's echo comparison;
    /// the update policy acts only on `Behind`. Explicit rebase returns it
    /// to `Editing` when edits survive.
    #[test]
    fn a_sent_draft_stays_sent_on_delivery_and_rebases_to_editing() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), &at(BASE));
        draft.state = DraftState::Sent {
            at: "2026-09-24T09:00:00Z".into(),
        };
        assert!(!draft.on_delivered(&at(NEWER)));
        assert!(draft.is_sent());
        let (kept, dropped) = draft.rebase(&model(&["T1"], &["-20"], &at(NEWER)));
        assert_eq!((kept, dropped.len()), (1, 0));
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.base.as_ref(), Some(&at(NEWER)));
    }

    #[test]
    fn on_delivered_does_nothing_to_a_clean_draft() {
        let mut draft = Draft::default();
        assert!(!draft.on_delivered(&at(NEWER)));
        assert_eq!(draft.state, DraftState::Clean);
    }

    #[test]
    fn rebase_moves_an_edit_to_its_new_index_by_label_and_reports_a_dropped_one() {
        let mut draft = Draft::default();
        // Two edits on a document whose rows were [T_b, T_a].
        draft.set((1, 1), pair("T_a", "-1"), Value::F64(0.5), &at(BASE));
        draft.set((0, 0), pair("T_b", "-20"), Value::F64(0.25), &at(BASE));
        draft.on_delivered(&at(NEWER));

        // The new document dropped T_b and so lists T_a first: the kept
        // edit's *index* moves even though its labels did not.
        let newer = model(&["T_a"], &["-20", "-1"], &at(NEWER));
        let (kept, dropped) = draft.rebase(&newer);

        assert_eq!(kept, 1);
        assert_eq!(dropped, vec![pair("T_b", "-20")]);
        assert_eq!(draft.edits.len(), 1);
        assert_eq!(
            draft.edits.get(&(0, 1)),
            Some(&Value::F64(0.5)),
            "T_a × -1 is cell (0,1) in the new document"
        );
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(
            draft.base.as_ref(),
            Some(&at(NEWER)),
            "a rebased draft is against the document it was rebased onto"
        );
    }

    #[test]
    fn rebase_onto_a_document_that_lost_every_label_is_clean_again() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T_a", "-20"), Value::F64(1.0), &at(BASE));
        draft.on_delivered(&at(NEWER));
        let (kept, dropped) = draft.rebase(&model(&["T_z"], &["-20"], &at(NEWER)));
        assert_eq!(kept, 0);
        assert_eq!(dropped, vec![pair("T_a", "-20")]);
        assert_eq!(draft.state, DraftState::Clean);
        assert!(draft.edits.is_empty());
    }

    #[test]
    fn rebase_refuses_edits_in_a_same_day_group_that_changed_size() {
        let base =
            crate::core::test_fixtures::flat_model(&["2026-09-18", "2026-09-18#2", "2026-12-18"]);
        let newer = crate::core::test_fixtures::flat_model(&[
            "2026-09-18",
            "2026-09-18#2",
            "2026-09-18#3",
            "2026-12-18",
        ]);
        let mut draft = Draft::default();
        draft.set(
            (1, 3),
            ("2026-09-18#2".into(), "amount".into()),
            Value::F64(1.0),
            &at("t0"),
        );
        draft.set(
            (2, 3),
            ("2026-12-18".into(), "amount".into()),
            Value::F64(2.0),
            &at("t0"),
        );
        draft.capture_groups(&base);
        let (_, dropped) = draft.rebase(&newer);
        assert!(
            dropped
                .iter()
                .any(|(l, why)| l == "2026-09-18#2" && why.contains("2 → 3")),
            "{dropped:?}"
        );
        assert_eq!(draft.edits.len(), 1, "the 2026-12-18 edit survives");
    }

    /// A changed same-day group count drops a deletion just as it drops a
    /// cell edit: the old ordinal must not delete a different dividend.
    #[test]
    fn rebase_refuses_a_deleted_row_in_a_same_day_group_that_changed_size() {
        let base =
            crate::core::test_fixtures::flat_model(&["2026-09-18", "2026-09-18#2", "2026-12-18"]);
        let newer = crate::core::test_fixtures::flat_model(&[
            "2026-09-18",
            "2026-09-18#2",
            "2026-09-18#3",
            "2026-12-18",
        ]);
        let mut draft = Draft::default();
        draft.delete_row("2026-09-18#2", &at("t0"));
        draft.capture_groups(&base);
        let (_, dropped) = draft.rebase(&newer);
        assert!(
            dropped
                .iter()
                .any(|(l, why)| l == "2026-09-18#2" && why.contains("2 → 3")),
            "{dropped:?}"
        );
        assert!(
            draft.rows.is_empty(),
            "the deleted mark did not survive the rebase: {:?}",
            draft.rows
        );
    }

    #[test]
    fn rebase_without_captured_groups_applies_no_guard() {
        let newer =
            crate::core::test_fixtures::flat_model(&["2026-09-18", "2026-09-18#2", "2026-09-18#3"]);
        let mut draft = Draft::default();
        draft.set(
            (1, 3),
            ("2026-09-18#2".into(), "amount".into()),
            Value::F64(1.0),
            &at("t0"),
        );
        let (_, dropped) = draft.rebase(&newer);
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    #[test]
    fn captured_groups_round_trip_through_the_session() {
        let base = crate::core::test_fixtures::flat_model(&["2026-09-18", "2026-09-18#2"]);
        let mut draft = Draft::default();
        draft.set(
            (1, 3),
            ("2026-09-18#2".into(), "amount".into()),
            Value::F64(1.0),
            &at("t0"),
        );
        draft.capture_groups(&base);
        let back = Draft::from_toml(&draft.to_toml());
        assert_eq!(back.groups, draft.groups);
    }

    #[test]
    fn revert_clears_the_edits_and_counts_them() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), &at(BASE));
        draft.set((0, 1), pair("T1", "-1"), Value::F64(2.0), &at(BASE));
        assert_eq!(draft.revert(), 2);
        assert_eq!(draft.state, DraftState::Clean);
        assert!(draft.edits.is_empty());
        assert_eq!(draft.base, None);
        assert_eq!(draft.revert(), 0);
    }

    #[test]
    fn revert_clears_everything_including_the_behind_state() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), &at(BASE));
        draft.on_delivered(&at(NEWER));
        draft.revert();
        assert!(draft.edits.is_empty());
        assert_eq!(draft.state, DraftState::Clean);
        assert_eq!(draft.base, None);
        assert_eq!(draft.badge(), DraftBadge::Clean);
    }

    #[test]
    fn bump_adds_the_delta_to_each_cells_current_value() {
        let mut draft = Draft::default();
        let cells = vec![
            ((0, 0), pair("T1", "-20"), Value::F64(1.0), ColumnType::F64),
            ((0, 1), pair("T1", "-1"), Value::F64(2.5), ColumnType::F64),
        ];
        assert_eq!(draft.bump(cells.into_iter(), 0.5, &at(BASE)), Ok(2));
        assert_eq!(draft.edits.get(&(0, 0)), Some(&Value::F64(1.5)));
        assert_eq!(draft.edits.get(&(0, 1)), Some(&Value::F64(3.0)));
        assert_eq!(draft.state, DraftState::Editing);
        // Bumping again reads the caller's *current* value, which is the
        // draft's own by then — the tile passes what the model paints.
        let again = vec![((0, 0), pair("T1", "-20"), Value::F64(1.5), ColumnType::F64)];
        assert_eq!(draft.bump(again.into_iter(), 0.5, &at(BASE)), Ok(1));
        assert_eq!(draft.edits.get(&(0, 0)), Some(&Value::F64(2.0)));
    }

    #[test]
    fn bump_lands_the_declared_type() {
        let mut draft = Draft::default();
        let n = draft
            .bump(
                [
                    (
                        (0, 0),
                        ("a".into(), "x".into()),
                        Value::F64(1.5),
                        ColumnType::F64,
                    ),
                    (
                        (0, 1),
                        ("a".into(), "y".into()),
                        Value::I64(3),
                        ColumnType::I64,
                    ),
                ]
                .into_iter(),
                2.0,
                &at("t0"),
            )
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(draft.edits[&(0, 0)], Value::F64(3.5));
        assert_eq!(draft.edits[&(0, 1)], Value::I64(5));
    }

    #[test]
    fn bump_refuses_a_fractional_delta_on_an_integer_column_before_writing() {
        let mut draft = Draft::default();
        let err = draft
            .bump(
                [
                    (
                        (0, 0),
                        ("a".into(), "x".into()),
                        Value::F64(1.5),
                        ColumnType::F64,
                    ),
                    (
                        (0, 1),
                        ("a".into(), "y".into()),
                        Value::I64(3),
                        ColumnType::I64,
                    ),
                ]
                .into_iter(),
                0.5,
                &at("t0"),
            )
            .unwrap_err();
        assert!(err.contains("whole numbers") && err.contains("y"), "{err}");
        assert!(draft.is_empty(), "no cell written on a refusal");
    }

    #[test]
    fn set_row_cell_moves_a_sent_draft_back_to_editing() {
        let mut draft = Draft::default();
        draft.insert_row("new-1".into(), None, &at("t0"));
        draft.state = DraftState::Sent {
            at: "2026-09-24T09:00:00Z".into(),
        };
        assert!(draft.set_row_cell("new-1", "amount", Value::F64(1.0)));
        assert_eq!(draft.state, DraftState::Editing);
    }

    /// `:bump` only ever reaches a `Number` cell — the tile decides that
    /// through `MatrixModel::kind_of`, before it ever builds the iterator
    /// `bump` takes — and `numeric_edit` is the door `MarketDataTile::bump`
    /// reads an existing edit's CURRENT value through: `F64`/`I64` keep
    /// their own type, a `Date`/`Utf8` edit (or no edit at all) answers
    /// `None` rather than being coerced.
    #[test]
    fn numeric_edit_reads_f64_and_i64_and_ignores_other_kinds() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.5), &at(BASE));
        draft.set((0, 1), pair("T1", "-1"), Value::I64(7), &at(BASE));
        draft.set((0, 2), pair("T1", "0"), Value::Utf8("x".into()), &at(BASE));
        assert_eq!(draft.numeric_edit((0, 0)), Some(&Value::F64(1.5)));
        assert_eq!(draft.numeric_edit((0, 1)), Some(&Value::I64(7)));
        assert_eq!(
            draft.numeric_edit((0, 2)),
            None,
            "a text edit is not numeric"
        );
        assert_eq!(draft.numeric_edit((9, 9)), None, "no edit at all");
    }

    #[test]
    fn badge_and_count_phrase_track_the_drafts_state() {
        let mut draft = Draft::default();
        assert_eq!(draft.badge(), DraftBadge::Clean);
        assert_eq!(draft.count_phrase(), "");

        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), &at(BASE));
        assert_eq!(draft.badge(), DraftBadge::Dirty);
        assert_eq!(draft.count_phrase(), "1 cell");

        draft.set((0, 1), pair("T1", "-1"), Value::F64(1.0), &at(BASE));
        draft.set((1, 1), pair("T2", "-1"), Value::F64(1.0), &at(BASE));
        assert_eq!(draft.count_phrase(), "3 cells");

        draft.state = DraftState::Sent {
            at: BASE.to_string(),
        };
        assert_eq!(
            draft.badge(),
            DraftBadge::Sent {
                at: BASE.to_string()
            }
        );

        draft.state = DraftState::Behind { newer: at(NEWER) };
        assert_eq!(
            draft.badge(),
            DraftBadge::Behind {
                newer: NEWER.to_string()
            },
            "M-4: an as-of step back delivers an OLDER document, so the \
             badge can only say the delivered one is DIFFERENT, never newer"
        );
    }

    #[test]
    fn update_policy_round_trips_its_three_names_and_refuses_the_rest() {
        for p in UpdatePolicy::ALL {
            assert_eq!(UpdatePolicy::parse(p.as_str()), Some(p));
        }
        assert_eq!(UpdatePolicy::parse("hold"), Some(UpdatePolicy::Hold));
        assert_eq!(UpdatePolicy::parse("rebase"), Some(UpdatePolicy::Rebase));
        assert_eq!(UpdatePolicy::parse("replace"), Some(UpdatePolicy::Replace));
        assert_eq!(UpdatePolicy::parse("Hold"), None, "lowercase only");
        assert_eq!(UpdatePolicy::parse("discard"), None);
        assert_eq!(UpdatePolicy::default(), UpdatePolicy::Hold);
    }

    #[test]
    fn local_hhmm_reads_on_the_clock() {
        use geode_core::clock::Clock;
        assert_eq!(local_hhmm("2026-09-18T22:00:00Z", Clock::utc()), "22:00");
        assert_eq!(
            local_hhmm("2026-09-18T22:00:00Z", Clock::in_zone_named("Asia/Tokyo")),
            "07:00"
        );
        assert_eq!(local_hhmm("not a time", Clock::utc()), "not a time");
    }

    #[test]
    fn to_toml_and_from_toml_round_trip_the_edits_by_label_and_the_base() {
        let mut draft = Draft::default();
        draft.set((0, 1), pair("T1", "-1"), Value::F64(0.5), &at(BASE));
        draft.set((1, 0), pair("T2", "-20"), Value::F64(0.25), &at(BASE));

        let table = draft.to_toml();
        assert_eq!(table.get("base").and_then(|v| v.as_str()), Some(BASE));
        let edits = table
            .get("edits")
            .and_then(|v| v.as_array())
            .expect("edits");
        assert_eq!(edits.len(), 2);
        let first = edits[0].as_array().expect("a triple");
        assert_eq!(first[0].as_str(), Some("T1"));
        assert_eq!(first[1].as_str(), Some("-1"));
        assert_eq!(first[2].as_float(), Some(0.5));

        let restored = Draft::from_toml(&table);
        assert_eq!(
            restored.base.as_ref(),
            Some(&at(BASE)),
            "the base is what makes the first delivery Behind rather than aligned"
        );
        assert_eq!(restored.state, DraftState::Editing);
        assert_eq!(restored.edits.len(), 2);
        assert_eq!(
            restored.to_toml(),
            table,
            "a restored draft writes back what it read"
        );

        // The restored indices are provisional: labels are the truth, and
        // `rebase` against the first model resolves them.
        let (kept, dropped) = {
            let mut restored = restored;
            let resolved = restored.rebase(&model(&["T2", "T1"], &["-20", "-1"], &at(BASE)));
            assert_eq!(
                restored.edits.get(&(1, 1)),
                Some(&Value::F64(0.5)),
                "T1 × -1"
            );
            assert_eq!(
                restored.edits.get(&(0, 0)),
                Some(&Value::F64(0.25)),
                "T2 × -20"
            );
            resolved
        };
        assert_eq!(kept, 2);
        assert!(dropped.is_empty());
    }

    /// The generation half survives the session file, and its absence stays
    /// absent. The parked-draft route round-trips a live draft through this
    /// table on every underlying switch, so a dropped `base_generation`
    /// write would silently return parked edits to time-only identity and the
    /// next same-time republish would re-point them.
    #[test]
    fn to_toml_and_from_toml_round_trip_the_base_generation_and_tolerate_its_absence() {
        let mut known = Draft::default();
        known.set((0, 1), pair("T1", "-1"), Value::F64(0.5), &at_gen(BASE, 7));
        let table = known.to_toml();
        assert_eq!(table.get("base").and_then(|v| v.as_str()), Some(BASE));
        assert_eq!(
            table.get("base_generation").and_then(|v| v.as_integer()),
            Some(7),
            "a known generation is written under its own key"
        );
        assert_eq!(
            Draft::from_toml(&table).base.as_ref(),
            Some(&at_gen(BASE, 7)),
            "and comes back as the same pair, not the time alone"
        );

        // An unknown generation writes no key at all, so a reader cannot
        // mistake a placeholder for a generation the store never named.
        let mut unknown = Draft::default();
        unknown.set((0, 1), pair("T1", "-1"), Value::F64(0.5), &at(BASE));
        let bare = unknown.to_toml();
        assert!(
            !bare.contains_key("base_generation"),
            "an unknown generation must not be spelled at all: {bare:?}"
        );

        // A session file predating the key: `None`, never `Some(0)` — a zero
        // would compare unequal to every real generation and put an aligned
        // draft Behind on its own base.
        let mut older = toml::Table::new();
        older.insert("base".into(), toml::Value::String(BASE.to_string()));
        older.insert(
            "edits".into(),
            toml::Value::Array(vec![toml::Value::Array(vec![
                toml::Value::String("T1".into()),
                toml::Value::String("-1".into()),
                toml::Value::Float(0.5),
            ])]),
        );
        let restored = Draft::from_toml(&older);
        assert_eq!(
            restored.base,
            Some(at(BASE)),
            "no key restores as an unknown generation"
        );
        assert_eq!(
            restored.base.as_ref().and_then(|b| b.generation),
            None,
            "specifically not Some(0)"
        );
        assert!(
            !restored
                .base
                .as_ref()
                .unwrap()
                .differs_from(&at_gen(BASE, 9)),
            "and an unknown generation cannot prove the document moved"
        );
    }

    #[test]
    fn an_empty_table_is_a_clean_draft_and_a_malformed_edit_is_skipped() {
        let empty = Draft::from_toml(&toml::Table::new());
        assert_eq!(empty.state, DraftState::Clean);
        assert!(empty.edits.is_empty());
        assert_eq!(empty.base, None);

        let mut table = toml::Table::new();
        table.insert(
            "edits".into(),
            toml::Value::Array(vec![
                toml::Value::String("not a triple".into()),
                toml::Value::Array(vec![
                    toml::Value::String("T1".into()),
                    toml::Value::String("-1".into()),
                    toml::Value::Integer(3),
                ]),
            ]),
        );
        let draft = Draft::from_toml(&table);
        assert_eq!(draft.edits.len(), 1, "the readable edit survives");
        assert_eq!(draft.edits.values().next(), Some(&Value::I64(3)));
    }

    /// Date and text cell edits require explicit tags in the session.
    /// Untagged strings are skipped rather than guessed as either type.
    #[test]
    fn typed_edits_round_trip_through_toml_with_a_type_tag() {
        let mut draft = Draft::default();
        draft.set(
            (0, 0),
            pair("D1", "ex"),
            Value::Date(d(2026, 12, 20)),
            &at(BASE),
        );
        draft.set((0, 1), pair("D1", "amount"), Value::F64(1.5), &at(BASE));
        draft.set(
            (0, 2),
            pair("D1", "status"),
            Value::Utf8("paid".into()),
            &at(BASE),
        );

        let table = draft.to_toml();
        let back = Draft::from_toml(&table);
        let values: Vec<_> = back.edits.values().cloned().collect();
        assert!(values.contains(&Value::Date(d(2026, 12, 20))));
        assert!(values.contains(&Value::F64(1.5)));
        assert!(values.contains(&Value::Utf8("paid".into())));

        // A date is a tagged table, never a bare string a text edit
        // could be confused with.
        let text = toml::to_string(&table).unwrap();
        assert!(text.contains("type = \"date\""), "{text}");
        assert!(text.contains("type = \"text\""), "{text}");
    }

    /// A bare, untagged string in a cell edit's value slot — the shape a
    /// hand-edited file, or a session written by a future mistake, might
    /// carry — is refused rather than guessed as text or a date.
    #[test]
    fn an_untagged_string_cell_edit_is_skipped_not_guessed() {
        let mut table = toml::Table::new();
        table.insert(
            "edits".into(),
            toml::Value::Array(vec![toml::Value::Array(vec![
                toml::Value::String("T1".into()),
                toml::Value::String("-1".into()),
                toml::Value::String("paid".into()),
            ])]),
        );
        let draft = Draft::from_toml(&table);
        assert!(draft.edits.is_empty(), "an untagged string is malformed");
    }

    #[test]
    fn parse_cell_reads_f64_and_i64_and_names_the_text_it_refused() {
        assert_eq!(parse_cell(" 0.25 ", ColumnType::F64), Ok(Value::F64(0.25)));
        assert_eq!(parse_cell("-3", ColumnType::F64), Ok(Value::F64(-3.0)));
        assert_eq!(parse_cell("7", ColumnType::I64), Ok(Value::I64(7)));
        // 2^53 + 1. Through an f64 this is 9007199254740992 — the whole
        // reason `parse_attr` parses integers directly.
        assert_eq!(
            parse_cell("9007199254740993", ColumnType::I64),
            Ok(Value::I64(9007199254740993))
        );

        let err = parse_cell("0.5", ColumnType::I64).expect_err("a whole number only");
        assert!(
            err.contains("0.5"),
            "the refusal must name what it refused: {err}"
        );
        assert!(err.contains("whole number"), "{err}");
        let err = parse_cell("abc", ColumnType::F64).expect_err("not a number");
        assert!(err.contains("abc"), "{err}");
        let err = parse_cell("", ColumnType::F64).expect_err("nothing is not a number");
        assert!(!err.is_empty());
        let err = parse_cell("1e400", ColumnType::F64).expect_err("infinity is not a value");
        assert!(err.contains("1e400"), "{err}");
        let err = parse_cell("2026-10-16", ColumnType::Date)
            .expect_err("the belt behind the kind dispatch refuses a non-numeric type");
        assert!(err.contains("2026-10-16"), "{err}");
    }

    #[test]
    fn a_bump_of_an_integer_column_stays_an_integer_above_2_pow_53() {
        // An integer above 2^53 must retain its value through the bump; routing
        // it through `f64` would round before the addition.
        assert_eq!(
            bumped(&Value::I64(9007199254740993), 1.0, ColumnType::I64, "lots"),
            Ok(Value::I64(9007199254740994))
        );
        assert_eq!(
            bumped(&Value::F64(0.25), 0.5, ColumnType::F64, "vol"),
            Ok(Value::F64(0.75))
        );
        // A fractional delta on an integer column is still refused.
        let err =
            bumped(&Value::I64(3), 0.5, ColumnType::I64, "lots").expect_err("a fractional delta");
        assert!(err.contains("whole numbers"), "{err}");
        // A fractional value already sitting in an integer column is
        // refused rather than truncated into one.
        let err = bumped(&Value::F64(1.5), 1.0, ColumnType::I64, "lots")
            .expect_err("a fractional current value");
        assert!(err.contains("fractional"), "{err}");
        // Overflow refuses rather than wrapping or saturating.
        let err = bumped(&Value::I64(i64::MAX), 1.0, ColumnType::I64, "lots")
            .expect_err("an overflowing bump");
        assert!(err.contains("too large"), "{err}");
        // A text value in a numeric column is not bumpable.
        let err = bumped(&Value::Utf8("x".into()), 1.0, ColumnType::F64, "note")
            .expect_err("not a number");
        assert!(!err.is_empty());
    }

    #[test]
    fn an_attribute_edit_is_part_of_the_same_draft() {
        let mut draft = Draft::default();
        assert_eq!(draft.badge(), DraftBadge::Clean);
        draft.set_attr("spot_ref", Value::F64(4520.0), &at("2026-09-14T14:00:00Z"));
        assert_eq!(draft.len(), 1);
        assert_eq!(draft.attr_count(), 1);
        assert_eq!(draft.base.as_ref(), Some(&at("2026-09-14T14:00:00Z")));
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.badge(), DraftBadge::Dirty);
        // No cell touched at all — an `is_empty` keyed on `edits` alone
        // would call this draft empty and let a trader navigate away with
        // the attribute edit uncounted.
        assert!(!draft.is_empty(), "an attribute alone is still unsent work");
        assert_eq!(draft.revert(), 1);
        assert!(draft.is_empty() && draft.base.is_none());
    }

    #[test]
    fn an_attribute_edit_survives_rebase_when_the_newer_document_declares_it() {
        let mut draft = Draft::default();
        draft.set_attr("spot_ref", Value::F64(1.0), &at("t0"));
        draft.set_attr("gone", Value::I64(2), &at("t0"));
        let newer = model_with_header(&[("spot_ref", "spot")]);
        let (kept, dropped) = draft.rebase(&newer);
        assert_eq!(kept, 1);
        assert_eq!(dropped, vec![("gone".to_string(), "attribute".to_string())]);
        assert_eq!(draft.attrs.get("spot_ref"), Some(&Value::F64(1.0)));
    }

    #[test]
    fn parse_attr_per_type() {
        assert_eq!(
            parse_attr("2026-09-14", ColumnType::Date),
            Ok(Value::Date(d(2026, 9, 14)))
        );
        assert_eq!(
            parse_attr("2026-13-45", ColumnType::Date),
            Err("'2026-13-45' is not a date (YYYY-MM-DD)".into())
        );
        assert_eq!(
            parse_attr(" 4520.5 ", ColumnType::F64),
            Ok(Value::F64(4520.5))
        );
        assert_eq!(parse_attr("7", ColumnType::I64), Ok(Value::I64(7)));
        assert_eq!(
            parse_attr("7.5", ColumnType::I64),
            Err("'7.5' is not a whole number".into())
        );
        // Above 2^53: exact, because the parse never passes through f64.
        assert_eq!(
            parse_attr("9007199254740993", ColumnType::I64),
            Ok(Value::I64(9_007_199_254_740_993))
        );
        assert_eq!(
            attr_text(&parse_attr("9007199254740993", ColumnType::I64).unwrap()),
            "9007199254740993"
        );
        assert_eq!(
            parse_attr("  ", ColumnType::Utf8),
            Err("a value is required".into())
        );
        assert_eq!(
            parse_attr(" abc ", ColumnType::Utf8),
            Ok(Value::Utf8("abc".into()))
        );
        assert!(parse_attr("x", ColumnType::Bool).is_err());
    }

    #[test]
    fn attr_text_is_the_documents_own_spelling() {
        assert_eq!(attr_text(&Value::Date(d(2026, 9, 14))), "2026-09-14");
        assert_eq!(attr_text(&Value::F64(5000.0)), "5000");
        assert_eq!(attr_text(&Value::F64(4520.25)), "4520.25");
        assert_eq!(attr_text(&Value::I64(3)), "3");
    }

    #[test]
    fn toml_round_trips_attribute_edits() {
        let mut draft = Draft::default();
        draft.set_attr("anchor_date", Value::Date(d(2026, 9, 14)), &at("t0"));
        draft.set_attr("spot_ref", Value::F64(4520.0), &at("t0"));
        let back = Draft::from_toml(&draft.to_toml());
        assert_eq!(back.attrs, draft.attrs);
        assert_eq!(back.base, draft.base);
        assert_eq!(back.state, DraftState::Editing);
    }

    /// The `attrs` analog of
    /// `an_empty_table_is_a_clean_draft_and_a_malformed_edit_is_skipped`:
    /// a value `to_toml` never writes (a `Boolean`, a `Datetime`) is
    /// skipped rather than taking the whole draft with it — unsent work
    /// is worth more than tidiness, the same rule the cell-edit loop
    /// follows.
    #[test]
    fn a_malformed_attribute_entry_is_skipped_and_the_others_survive() {
        let mut attrs = toml::Table::new();
        attrs.insert("spot_ref".into(), toml::Value::Float(4520.0));
        attrs.insert(
            "anchor_date".into(),
            toml::Value::String("2026-09-14".into()),
        );
        attrs.insert("flag".into(), toml::Value::Boolean(true));
        attrs.insert(
            "stamp".into(),
            toml::Value::Datetime("2026-09-14T00:00:00Z".parse().unwrap()),
        );
        let mut table = toml::Table::new();
        table.insert("base".into(), toml::Value::String("t0".into()));
        table.insert("attrs".into(), toml::Value::Table(attrs));

        let draft = Draft::from_toml(&table);
        assert_eq!(
            draft.attrs.len(),
            2,
            "the boolean and the datetime are skipped, not the whole draft"
        );
        assert_eq!(draft.attrs.get("spot_ref"), Some(&Value::F64(4520.0)));
        assert_eq!(
            draft.attrs.get("anchor_date"),
            Some(&Value::Date(d(2026, 9, 14)))
        );
        assert!(!draft.attrs.contains_key("flag"));
        assert!(!draft.attrs.contains_key("stamp"));
        assert_eq!(draft.state, DraftState::Editing, "unsent work survived");
    }

    #[test]
    fn count_phrase_names_cells_and_attributes() {
        let mut draft = Draft::default();
        draft.set(
            (0, 0),
            ("1M".into(), "-20".into()),
            Value::F64(0.1),
            &at("t0"),
        );
        draft.set(
            (0, 1),
            ("1M".into(), "-10".into()),
            Value::F64(0.1),
            &at("t0"),
        );
        draft.set_attr("spot_ref", Value::F64(1.0), &at("t0"));
        assert_eq!(draft.count_phrase(), "2 cells, spot_ref");
        let mut one = Draft::default();
        one.set(
            (0, 0),
            ("1M".into(), "-20".into()),
            Value::F64(0.1),
            &at("t0"),
        );
        assert_eq!(one.count_phrase(), "1 cell");
    }

    proptest! {
        /// The session form round-trips an attribute map through TOML
        /// exactly as it round-trips a cell edit: `to_toml`/`from_toml`
        /// must answer the same `attrs` it was given, over arbitrary
        /// column names and finite `F64` values plus one fixed `Date` and
        /// one `Utf8`, so a restart never quietly drops or reshapes a
        /// header edit sitting in `session.toml`.
        #[test]
        fn to_toml_and_from_toml_round_trip_an_arbitrary_attrs_map(
            floats in prop::collection::btree_map(
                "[a-z_]{1,8}",
                any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Value::F64),
                0..4,
            )
        ) {
            let mut draft = Draft::default();
            for (column, value) in &floats {
                draft.set_attr(column, value.clone(), &at("t0"));
            }
            draft.set_attr("anchor_date", Value::Date(d(2026, 9, 14)), &at("t0"));
            draft.set_attr("free_text", Value::Utf8("a note".into()), &at("t0"));

            let back = Draft::from_toml(&draft.to_toml());
            prop_assert_eq!(back.attrs, draft.attrs);
        }
    }

    #[test]
    fn inserting_and_deleting_rows_is_counted_and_phrased() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("D1".into()), &at("t0"));
        assert!(d.set_row_cell("new-1", "amount", Value::F64(1.0)));
        assert!(
            !d.set_row_cell("D1", "amount", Value::F64(1.0)),
            "not an inserted row"
        );
        assert_eq!(d.delete_row("D2", &at("t0")), RowDelete::Marked);
        assert_eq!(d.delete_row("D2", &at("t0")), RowDelete::Already);
        assert_eq!(d.rows_added(), 1);
        assert_eq!(d.rows_removed(), 1);
        assert_eq!(d.count_phrase(), "1 row added, 1 row removed");
        assert_eq!(d.delete_row("new-1", &at("t0")), RowDelete::Dropped);
        assert_eq!(d.rows_added(), 0);
        assert_eq!(d.revert(), 1);
        assert!(d.rows.is_empty());
    }

    /// Inserting and then deleting the only pending row leaves a clean draft
    /// with no base, so later deliveries cannot mark it behind.
    #[test]
    fn dropping_the_only_inserted_row_leaves_a_clean_draft() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), None, &at("t0"));
        assert_eq!(d.delete_row("new-1", &at("t0")), RowDelete::Dropped);
        assert!(d.is_empty());
        assert!(d.base.is_none());
        assert_eq!(d.badge(), DraftBadge::Clean);
        assert!(
            !d.on_delivered(&at("t9")),
            "a clean draft with nothing pending never goes Behind"
        );
    }

    #[test]
    fn mint_label_takes_the_smallest_unused_number() {
        let mut d = Draft::default();
        assert_eq!(d.mint_label(|_| false), "new-1");
        d.insert_row("new-1".into(), None, &at("t0"));
        assert_eq!(d.mint_label(|_| false), "new-2");
        assert_eq!(
            d.mint_label(|l| l == "new-2"),
            "new-3",
            "a label the model already has is skipped"
        );
    }

    #[test]
    fn rename_row_moves_an_inserted_row_and_refuses_a_collision() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), None, &at("t0"));
        d.insert_row("new-2".into(), None, &at("t0"));
        assert!(d.rename_row("new-1", "2027-01-15"));
        assert!(d.row_state("2027-01-15").is_some());
        assert!(!d.rename_row("new-2", "2027-01-15"));
        assert!(!d.rename_row("D1", "x"), "only an inserted row renames");
    }

    /// Renaming an inserted chain member moves its followers to the new
    /// label, preserving their placement and persisted anchors.
    #[test]
    fn rename_row_rehangs_its_followers() {
        let mut d = Draft::default();
        d.insert_row("2027-01-15".into(), Some("new-2".into()), &at("t0"));
        d.insert_row("new-2".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-3".into(), Some("D2".into()), &at("t0"));
        assert!(d.rename_row("new-2", "2027-02-15"));
        assert!(
            matches!(
                d.row_state("2027-01-15"),
                Some(RowEdit::Inserted { after: Some(a), .. }) if a == "2027-02-15"
            ),
            "the follower hangs off the renamed label"
        );
        assert!(
            matches!(
                d.row_state("new-3"),
                Some(RowEdit::Inserted { after: Some(a), .. }) if a == "D2"
            ),
            "a row anchored elsewhere is untouched"
        );
    }

    /// Dropping the middle of a chain hands its followers to its own
    /// anchor: `D1 → new-2 → new-1`, drop `new-2`, and `new-1` sits
    /// under `D1` — never at the top as a vanished anchor would land it.
    #[test]
    fn dropping_an_inserted_row_hands_its_followers_to_its_anchor() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("new-2".into()), &at("t0"));
        d.insert_row("new-2".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-3".into(), Some("D2".into()), &at("t0"));
        assert_eq!(d.delete_row("new-2", &at("t0")), RowDelete::Dropped);
        assert!(matches!(
            d.row_state("new-1"),
            Some(RowEdit::Inserted { after: Some(a), .. }) if a == "D1"
        ));
        assert!(
            matches!(
                d.row_state("new-3"),
                Some(RowEdit::Inserted { after: Some(a), .. }) if a == "D2"
            ),
            "a row anchored elsewhere is untouched"
        );
    }

    /// `rehang_followers` moves exactly the rows anchored on `from` —
    /// several at once, a `None` anchor included — and nothing anchored
    /// elsewhere.
    #[test]
    fn rehang_followers_moves_every_row_anchored_on_from() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-2".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-3".into(), None, &at("t0"));
        d.insert_row("new-4".into(), Some("D2".into()), &at("t0"));
        d.delete_row("D1", &at("t0"));
        d.rehang_followers(Some("D1"), Some("new-9".into()));
        fn after(d: &Draft, l: &str) -> Option<String> {
            match d.row_state(l) {
                Some(RowEdit::Inserted { after, .. }) => after.clone(),
                _ => None,
            }
        }
        assert_eq!(after(&d, "new-1").as_deref(), Some("new-9"));
        assert_eq!(after(&d, "new-2").as_deref(), Some("new-9"));
        assert_eq!(after(&d, "new-3"), None, "a top row is not anchored on D1");
        assert_eq!(after(&d, "new-4").as_deref(), Some("D2"));
        assert!(
            matches!(d.row_state("D1"), Some(RowEdit::Deleted)),
            "a deleted row is not a follower"
        );
        d.rehang_followers(None, Some("D2".into()));
        assert_eq!(
            after(&d, "new-3").as_deref(),
            Some("D2"),
            "the top group moves too"
        );
    }

    /// `shift+o` on an inserted row: the new row takes the old anchor and
    /// the old row hangs off the new one, so the model paints
    /// `D1, new-2, new-1` — a chain the splice resolves transitively.
    #[test]
    fn reanchor_row_moves_an_inserted_row_under_a_new_anchor() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-2".into(), Some("D1".into()), &at("t0"));
        assert!(d.reanchor_row("new-1", Some("new-2".into())));
        assert!(matches!(
            d.row_state("new-1"),
            Some(RowEdit::Inserted { after: Some(a), .. }) if a == "new-2"
        ));
        assert!(
            !d.reanchor_row("D1", None),
            "only an inserted row re-anchors"
        );
        d.delete_row("D3", &at("t0"));
        assert!(!d.reanchor_row("D3", None), "a deleted row has no anchor");
    }

    /// Rebase drops and names vanished deletions and inserts that now collide
    /// with document rows. A vanished anchor moves its insert to the top
    /// and is reported without dropping the inserted row.
    #[test]
    fn rebase_carries_rows_by_label() {
        let mut d = Draft::default();
        d.delete_row("GONE", &at("t0"));
        d.delete_row("D1", &at("t0"));
        d.insert_row("D9".into(), Some("D1".into()), &at("t0")); // upstream will carry D9
        d.insert_row("new-1".into(), Some("GONE".into()), &at("t0")); // anchor vanishes
        let newer = flat_model_with_rows(&["D1", "D2", "D9"]);
        let (_, dropped) = d.rebase(&newer);
        assert!(matches!(d.row_state("D1"), Some(RowEdit::Deleted)));
        assert!(d.row_state("GONE").is_none());
        assert!(d.row_state("D9").is_none(), "upstream got there first");
        assert!(matches!(
            d.row_state("new-1"),
            Some(RowEdit::Inserted { after: None, .. })
        ));
        assert!(dropped.contains(&("GONE".into(), "row".into())));
        assert!(dropped.contains(&("D9".into(), "row (the document now carries it)".into())));
        assert!(dropped.contains(&("new-1".into(), "anchor 'GONE'".into())));
    }

    /// A chain — `D1 → new-2 → new-1`, the shape `shift+o` on an inserted
    /// row builds — survives a rebase whole: `new-1`'s anchor is an
    /// INSERTED row the newer model (the document's own grid) never
    /// carries, and that is not a vanished anchor. Both rows kept, the
    /// anchor untouched, nothing named dropped. The anchor's survival
    /// must be decided from a precomputed set, since label order visits
    /// `new-1` before the `new-2` it hangs off. An anchor on an inserted
    /// row that the rebase DROPS as a conflict still resolves — the row is
    /// dropped precisely because the document now carries that label, so
    /// the chained row lands under the real row instead.
    #[test]
    fn rebase_keeps_a_chain_anchored_on_a_surviving_inserted_row() {
        let mut d = Draft::default();
        d.insert_row("new-2".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-1".into(), Some("new-2".into()), &at("t0"));
        let (kept, dropped) = d.rebase(&flat_model_with_rows(&["D1"]));
        assert_eq!(kept, 2);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert!(matches!(
            d.row_state("new-2"),
            Some(RowEdit::Inserted { after: Some(a), .. }) if a == "D1"
        ));
        assert!(matches!(
            d.row_state("new-1"),
            Some(RowEdit::Inserted { after: Some(a), .. }) if a == "new-2"
        ));

        // The anchor itself dropped as a conflict: the document now
        // carries `D7`, so the chained row hangs off the real one and
        // only the conflict is named.
        let mut d = Draft::default();
        d.insert_row("D7".into(), Some("D1".into()), &at("t0"));
        d.insert_row("new-1".into(), Some("D7".into()), &at("t0"));
        let (_, dropped) = d.rebase(&flat_model_with_rows(&["D1", "D7"]));
        assert!(d.row_state("D7").is_none());
        assert!(matches!(
            d.row_state("new-1"),
            Some(RowEdit::Inserted { after: Some(a), .. }) if a == "D7"
        ));
        assert_eq!(
            dropped,
            [(
                "D7".to_string(),
                "row (the document now carries it)".to_string()
            )]
        );
    }

    #[test]
    fn rows_round_trip_through_toml() {
        let mut d = Draft::default();
        d.insert_row("new-1".into(), Some("D1".into()), &at("t0"));
        d.set_row_cell("new-1", "ex", Value::Date(date(2027, 1, 15)));
        d.set_row_cell("new-1", "status", Value::Utf8("estimated".into()));
        d.delete_row("D2", &at("t0"));
        let back = Draft::from_toml(&d.to_toml());
        assert_eq!(back.rows, d.rows);
        assert_eq!(back.base, d.base);
    }
}
