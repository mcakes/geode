//! The unsent work (market-data spec §8.4).
//!
//! A draft is edits over the generation that was painted when they were
//! made. It is keyed by grid cell, because that is what a cursor points
//! at, and it records each cell's row and column *label* beside the
//! value, because that is the only identity that survives a new document:
//! a term can move index, and a node the desk stopped publishing takes
//! its column with it. `base` is the document's own source time (never a
//! `gen_id` — a live query's provenance carries the dataset-wide latest
//! generation while its `as_of` is per document, Part 1 §4.5), so a
//! delivery whose `as_of` differs from `base` is a newer generation and
//! the draft goes `Behind` rather than being clobbered (roadmap ruling 9).

use crate::core::matrix::MatrixModel;
use geode_core::document::Value;
use geode_core::schema::ColumnType;
use std::collections::{BTreeMap, HashMap};

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
    /// and is the same situation. The panel keeps painting the base
    /// generation under the edits; `:rebase` moves them onto the
    /// delivered one and `:revert` drops them. The edits' own base
    /// generation coming back (an as-of round trip) returns the draft to
    /// `Editing` — see [`Draft::on_delivered`].
    Behind { newer: String },
    /// An upload succeeded; the edits are kept and painted as sent until
    /// the echo clears them (§9.4, Part 4).
    Sent,
}

/// What the header says about the draft at a glance (spec 2026-09-14 §4):
/// a dot for `Dirty`, `update HH:MM` for `Behind`, `sent HH:MM` for `Sent`
/// (Part 4), nothing for `Clean`. Counts live in `count_phrase`, for the
/// places a number matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftBadge {
    Clean,
    Dirty,
    Behind { newer: String },
    Sent,
}

/// What a panel does when a DIFFERENT generation is delivered while its
/// draft has edits — a per-tile choice (user ruling 2026-09-19), applied
/// by the tile at the one point today's code enters `Behind`
/// ([`Draft::on_delivered`] IS the `hold` decision and reads no policy;
/// the tile branches after it). A clean panel follows every document
/// regardless, and the edits' own base coming back (an as-of round trip)
/// is not a different document, so neither is touched by this.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UpdatePolicy {
    /// Today's behaviour: the draft goes `Behind`, the base generation
    /// stays painted, `:rebase`/`:revert` are the ways out.
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

/// Edits keyed by grid cell, with the labels that make them portable, plus
/// document-level attribute edits keyed by column name.
///
/// **The invariant this leans on:** a [`MatrixModel`]'s row labels are
/// unique and so are its column labels. A label is how an edit is
/// identified across generations, so a repeated one would make two
/// different rows a single target — and [`MatrixModel::build`] refuses
/// every way that could happen (a repeated pivot pair, a repeated flat row
/// label, a blank axis cell). That one defence at the model boundary is
/// why [`Draft::rebase`] indexes labels without a collision check of its
/// own: a second check here would be a defence the first one hides, and
/// neither would then be isolated enough for the mutation harness to say
/// which is load-bearing.
///
/// A header attribute needs no such indexing — its column NAME is its
/// identity, the same name every generation of one document carries it
/// under — so `attrs` is keyed directly, with no `labels`-style side map.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    /// The source time of the generation every edit was made against,
    /// RFC 3339. `None` exactly when there are no edits.
    pub base: Option<String>,
    pub edits: BTreeMap<(usize, usize), Value>,
    /// Document-level attribute edits, keyed by column name. Part of the
    /// same draft as `edits` (one base, one state) because both are unsent
    /// work against the same document generation.
    pub attrs: BTreeMap<String, Value>,
    pub state: DraftState,
    /// (row label, column label) per edited cell. Private because it must
    /// never drift from `edits`: every door that writes one writes both.
    labels: BTreeMap<(usize, usize), (String, String)>,
}

/// The column index a restored edit is parked at until a model resolves
/// it by label.
///
/// A session stores edits as label pairs, not indices (spec §8.5), so a
/// restored draft has no real grid position for anything. Parking them
/// out of every possible grid's range means an unresolved edit paints
/// NOWHERE rather than in some arbitrary cell: an edit a trader cannot
/// see is recoverable (`rebase` puts it back), while an edit painted
/// against the wrong cell is a wrong number on a screen, which is the one
/// failure this codebase refuses to risk. The row index stays the file's
/// own order so the round trip is stable.
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

    /// Cells and attributes together — the one number that answers
    /// "is there unsent work".
    pub fn len(&self) -> usize {
        self.edits.len() + self.attrs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.attrs.is_empty()
    }

    pub fn is_sent(&self) -> bool {
        self.state == DraftState::Sent
    }

    pub fn is_behind(&self) -> bool {
        matches!(self.state, DraftState::Behind { .. })
    }

    /// Record one edit. `base` is the source time of the generation on
    /// screen; it is stored only while the draft is empty, because every
    /// edit in one draft is against one generation and a later keystroke
    /// must never quietly restamp the set.
    pub fn set(
        &mut self,
        cell: (usize, usize),
        labels: (String, String),
        value: Value,
        base: &str,
    ) {
        if self.is_empty() || self.base.is_none() {
            self.base = Some(base.to_string());
        }
        self.edits.insert(cell, value);
        self.labels.insert(cell, labels);
        // `Behind` survives an edit: the panel is still painting the base
        // generation, so a further edit is against the same document.
        // `Sent` does not — the draft no longer matches what was sent.
        if matches!(self.state, DraftState::Clean | DraftState::Sent) {
            self.state = DraftState::Editing;
        }
    }

    /// A cell edit's numeric reading — `F64`/`I64` widened to `f64`,
    /// `None` for a `Date`/`Utf8` edit or no edit at all. `:bump`'s own
    /// door onto an EXISTING edit — `MarketDataTile::bump`'s
    /// `numeric_value` reads a cell's current value through here first,
    /// falling back to the model's own painted one when there is no edit
    /// yet — so a second bump composes with the first rather than reading
    /// through to the document underneath it. Which COLUMNS are ever
    /// bumped at all is `CellKind::Number`'s decision, made by the
    /// caller (this crate has no `MatrixModel` to consult here); this is
    /// only the "what number is already there" half of that.
    pub fn numeric_edit(&self, cell: (usize, usize)) -> Option<f64> {
        match self.edits.get(&cell)? {
            Value::F64(v) => Some(*v),
            Value::I64(v) => Some(*v as f64),
            Value::Utf8(_) | Value::Date(_) => None,
        }
    }

    /// Record one attribute edit — the same base rule as `set`.
    pub fn set_attr(&mut self, column: &str, value: Value, base: &str) {
        if self.is_empty() || self.base.is_none() {
            self.base = Some(base.to_string());
        }
        self.attrs.insert(column.to_string(), value);
        if matches!(self.state, DraftState::Clean | DraftState::Sent) {
            self.state = DraftState::Editing;
        }
    }

    /// Drop every edit and attribute, answering how many there were in
    /// total. The draft is `Clean` afterwards and carries no base, since a
    /// base describes a set of edits.
    pub fn revert(&mut self) -> usize {
        let n = self.len();
        self.edits.clear();
        self.labels.clear();
        self.attrs.clear();
        self.base = None;
        self.state = DraftState::Clean;
        n
    }

    /// Add `delta` to each given cell's current value, answering how many
    /// cells were written.
    ///
    /// The caller passes each cell's *current* value — what the model is
    /// painting, which is the draft's own value where one exists — so that
    /// `:bump` composes with an edit already made rather than reading
    /// through to the document underneath it.
    ///
    /// **Number cells only, by construction of the caller, not this
    /// function**: `:bump` is a per-cell arithmetic op, so a caller (the
    /// tile) reads each candidate cell's [`CellKind`] through
    /// [`MatrixModel::kind_of`] and passes only the `Number` ones —
    /// exactly the same door `f64_at`-vs-`display_at` reading in
    /// `matrix::cell_of` decides by. A bumped cell always lands as
    /// [`Value::F64`]: `delta` is itself an `f64`, and preserving an
    /// `I64` cell's own type through a bump is not this slice's problem.
    pub fn bump(
        &mut self,
        cells: impl Iterator<Item = ((usize, usize), (String, String), f64)>,
        delta: f64,
        base: &str,
    ) -> usize {
        let mut n = 0;
        for (cell, labels, current) in cells {
            self.set(cell, labels, Value::F64(current + delta), base);
            n += 1;
        }
        n
    }

    /// A generation was delivered. Answers whether the state changed, so
    /// the caller knows whether anything needs repainting.
    ///
    /// **A generation's identity here is its source time alone** (M-3,
    /// final whole-branch review): a republish that keeps its source
    /// time — `source_time = "document"` stamping every republish of one
    /// date at that date's midnight, or a corrected file republish,
    /// which ties its predecessor's `source_time` — reads as the same
    /// generation and swaps the grid under index-keyed edits. Harmless
    /// while the row/column set is unchanged (CVI's ladder is fixed per
    /// date) and not reachable under `--demo` (`source_time = "receive"`);
    /// a `gen_id` in the provenance is the fix, when Part 4 touches
    /// `compile_document`.
    ///
    /// Only a draft with edits can go `Behind`, and only when the
    /// delivered source time differs from the one the edits were made
    /// against — the same document redelivered (a requery on any
    /// publish of any dataset bumps the frame's `data` version, so this
    /// happens routinely) must not read as a newer one.
    pub fn on_delivered(&mut self, as_of: &str) -> bool {
        match &self.state {
            DraftState::Editing if self.base.as_deref() != Some(as_of) => {
                self.state = DraftState::Behind {
                    newer: as_of.to_string(),
                };
                true
            }
            // I-4 (final whole-branch review): the edits' OWN generation
            // came back — an as-of step back and `:asof undo` is an
            // ordinary round trip, and the panel is now painting exactly
            // what the edits were made on. Nothing is behind anything, so
            // the draft is `Editing` again with every edit untouched
            // (they are index-keyed against this very generation) and
            // `edit`/`:bump` open again. Placed ABOVE the "a further
            // generation arrived" arm, whose guard would otherwise fall
            // through to `_ => false` and leave the header claiming a
            // document had been received that the panel is not showing.
            DraftState::Behind { .. } if self.base.as_deref() == Some(as_of) => {
                self.state = DraftState::Editing;
                true
            }
            // Already behind, and a *further* generation arrived: the
            // header must name the newest one, not the first one missed.
            DraftState::Behind { newer } if newer != as_of => {
                self.state = DraftState::Behind {
                    newer: as_of.to_string(),
                };
                true
            }
            _ => false,
        }
    }

    /// Re-apply the edits onto a newer generation's model, by label.
    ///
    /// Answers how many were kept and the labels of those dropped —
    /// a row or column the new document no longer has. Matching by label
    /// rather than by index is the whole point: a term that moved from row
    /// 3 to row 2 is the same term, and an edit left at index 3 would be
    /// silently reassigned to a different expiry.
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

        self.base = model_of_newer.source_time.clone();
        self.state = if self.is_empty() {
            DraftState::Clean
        } else {
            DraftState::Editing
        };
        (self.len(), dropped)
    }

    /// The header's badge, at a glance (Task 4 paints it): [`DraftBadge`]
    /// carries no count, since a badge is a shape and a count is a
    /// sentence — [`Draft::count_phrase`] is where the count lives, for
    /// the notices and confirms that need one.
    pub fn badge(&self) -> DraftBadge {
        match &self.state {
            DraftState::Clean => DraftBadge::Clean,
            DraftState::Editing => DraftBadge::Dirty,
            DraftState::Behind { newer } => DraftBadge::Behind {
                newer: newer.clone(),
            },
            DraftState::Sent => DraftBadge::Sent,
        }
    }

    /// "3 cells, spot_ref" / "1 cell" / "anchor_date, spot_ref" — the
    /// unsent work named for a notice or a confirm, cells first (a count,
    /// since a cell has no name worth showing) and then every edited
    /// attribute's own column name.
    pub fn count_phrase(&self) -> String {
        let mut parts = Vec::new();
        match self.edits.len() {
            0 => {}
            1 => parts.push("1 cell".to_string()),
            n => parts.push(format!("{n} cells")),
        }
        parts.extend(self.attrs.keys().cloned());
        parts.join(", ")
    }

    /// The session form (spec §8.5): the base and the edits as label
    /// pairs, never indices — a restart onto a newer generation must land
    /// in `Behind`, not against misaligned cells.
    pub fn to_toml(&self) -> toml::Table {
        let mut table = toml::Table::new();
        if let Some(base) = &self.base {
            table.insert("base".into(), toml::Value::String(base.clone()));
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
        table
    }

    /// Read a session's draft back. Every edit lands at
    /// [`UNRESOLVED_COLUMN`] until [`Draft::rebase`] against the first
    /// model resolves it by label; a malformed entry is skipped rather
    /// than taking the whole draft with it (unsent work is worth more than
    /// tidiness).
    ///
    /// A cell edit's value is read through [`value_from_toml`] — a bare
    /// number or a tagged date/text, never a guess. An attribute's
    /// `String` is read back as a [`Value::Date`] when it parses
    /// `%Y-%m-%d` and a [`Value::Utf8`] otherwise: a date string is
    /// unambiguous (`to_toml` writes no other string in that exact shape),
    /// and a free-text attribute never happens to look like one — so the
    /// direction of the guess costs nothing either way. The two spellings
    /// differ on purpose (§4.3's ruling): `attrs` predates typed cells and
    /// nothing forces its shape to change to match.
    pub fn from_toml(t: &toml::Table) -> Draft {
        let base = t.get("base").and_then(|v| v.as_str()).map(str::to_string);
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
        let state = if edits.is_empty() && attrs.is_empty() {
            DraftState::Clean
        } else {
            DraftState::Editing
        };
        Draft {
            base,
            edits,
            attrs,
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

/// Local, like every other displayed time in this codebase (Phase 4a's
/// ruling) — `pub(crate)` because Task 4's header paints `DraftBadge`'s
/// `Behind`/`Sent` times through it directly.
pub(crate) fn local_hhmm(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(t) => t.with_timezone(&chrono::Local).format("%H:%M").to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

/// Parse a typed cell to the number a document holds.
///
/// Only `f64` and `i64` columns are editable — a document's values are
/// declared one of those two (spec §3.2) and an axis is read-only in
/// slice 1 (a header attribute is editable too, but through
/// [`parse_attr`], which parses its own broader vocabulary of types) — so
/// anything else is refused by type rather than coerced. Every message
/// names the text it refused, because the inline notice appears beside a
/// field the trader can no longer see the whole of.
pub fn parse_cell(text: &str, ty: ColumnType) -> Result<f64, String> {
    let trimmed = text.trim();
    match ty {
        ColumnType::F64 => {
            let value: f64 = trimmed
                .parse()
                .map_err(|_| format!("'{text}' is not a number"))?;
            if !value.is_finite() {
                return Err(format!("'{text}' is not a finite number"));
            }
            Ok(value)
        }
        ColumnType::I64 => trimmed
            .parse::<i64>()
            .map(|v| v as f64)
            .map_err(|_| format!("'{text}' is not a whole number")),
        ColumnType::Utf8 | ColumnType::Date | ColumnType::Timestamp | ColumnType::Bool => Err(
            format!("'{text}' cannot be entered here — only numeric cells are editable"),
        ),
    }
}

/// Parse a typed header attribute to the value a document holds.
///
/// A wider vocabulary than [`parse_cell`]'s: a header attribute can be a
/// date or free text as well as a number (spec 2026-09-14 §4), each
/// parsed per its own declared [`ColumnType`] rather than coerced —
/// `Bool`/`Timestamp` fall to the catch-all, since neither header
/// attribute type this slice ships is either.
pub fn parse_attr(text: &str, ty: ColumnType) -> Result<Value, String> {
    let trimmed = text.trim();
    match ty {
        ColumnType::Date => chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
            .map(Value::Date)
            .map_err(|_| format!("'{text}' is not a date (YYYY-MM-DD)")),
        ColumnType::F64 => parse_cell(text, ColumnType::F64).map(Value::F64),
        // Parsed as `i64` DIRECTLY, never through `parse_cell`'s `f64`
        // (final review, A4): a round trip through a double loses every
        // integer above 2^53, silently. Same error text as `parse_cell`'s
        // own whole-number refusal, so `:set` and a cell read alike.
        ColumnType::I64 => trimmed
            .parse::<i64>()
            .map(Value::I64)
            .map_err(|_| format!("'{text}' is not a whole number")),
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
    use crate::core::matrix::{Cell, HeaderCell, MatrixModel, RowModel};
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

    /// A model with the given row and column labels and no values — the
    /// draft only ever reads a model's labels and its source time.
    fn model(rows: &[&str], cols: &[&str], source_time: &str) -> MatrixModel {
        MatrixModel {
            key: vec!["SPX.Z".to_string()],
            source_time: Some(source_time.to_string()),
            header: Vec::new(),
            slice_columns: 0,
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
                })
                .collect(),
            pivot_index: None,
        }
    }

    #[test]
    fn the_first_edit_records_the_base_and_a_second_edit_on_one_cell_keeps_the_latest() {
        let mut draft = Draft::default();
        assert_eq!(draft.state, DraftState::Clean);
        draft.set((0, 1), pair("T1", "-1"), Value::F64(0.5), BASE);
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.base.as_deref(), Some(BASE));
        draft.set((0, 1), pair("T1", "-1"), Value::F64(0.7), BASE);
        assert_eq!(draft.edits.len(), 1);
        assert_eq!(draft.edits.get(&(0, 1)), Some(&Value::F64(0.7)));
        assert_eq!(
            draft.base.as_deref(),
            Some(BASE),
            "every edit in one draft is against one generation"
        );
    }

    #[test]
    fn on_delivered_stays_editing_on_the_same_generation_and_goes_behind_on_a_newer_one() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), BASE);

        assert!(
            !draft.on_delivered(BASE),
            "the same document redelivered changes nothing"
        );
        assert_eq!(draft.state, DraftState::Editing);

        assert!(draft.on_delivered(NEWER));
        assert_eq!(
            draft.state,
            DraftState::Behind {
                newer: NEWER.to_string()
            }
        );
        assert_eq!(draft.edits.len(), 1, "a newer document never clobbers work");
        assert_eq!(draft.edits.get(&(0, 0)), Some(&Value::F64(1.0)));
        assert_eq!(
            draft.base.as_deref(),
            Some(BASE),
            "the base still names what is painted"
        );
        assert!(
            !draft.on_delivered(NEWER),
            "the same newer document again is not a fresh transition"
        );
    }

    /// I-4 (final whole-branch review): an as-of round trip — step back
    /// to a historical generation and then return to live — redelivers
    /// the very generation the edits were made on, and the draft must
    /// come back to `Editing`. Without the arm the header kept claiming
    /// a different document had been received and `edit`/`:bump` stayed
    /// refused while the panel was painting the edits' own base.
    #[test]
    fn the_base_generation_redelivered_brings_a_behind_draft_back_to_editing() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), BASE);
        let older = "2026-09-12T09:00:00Z";

        assert!(draft.on_delivered(older), "an as-of step back is Behind");
        assert_eq!(
            draft.state,
            DraftState::Behind {
                newer: older.to_string()
            }
        );

        assert!(
            draft.on_delivered(BASE),
            "the base coming back is a real transition"
        );
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.edits.len(), 1, "the edits are untouched");
        assert_eq!(draft.edits.get(&(0, 0)), Some(&Value::F64(1.0)));
        assert_eq!(draft.base.as_deref(), Some(BASE));
        assert!(
            !draft.on_delivered(BASE),
            "and the base again is no transition at all"
        );
    }

    #[test]
    fn on_delivered_does_nothing_to_a_clean_draft() {
        let mut draft = Draft::default();
        assert!(!draft.on_delivered(NEWER));
        assert_eq!(draft.state, DraftState::Clean);
    }

    #[test]
    fn rebase_moves_an_edit_to_its_new_index_by_label_and_reports_a_dropped_one() {
        let mut draft = Draft::default();
        // Two edits on a document whose rows were [T_b, T_a].
        draft.set((1, 1), pair("T_a", "-1"), Value::F64(0.5), BASE);
        draft.set((0, 0), pair("T_b", "-20"), Value::F64(0.25), BASE);
        draft.on_delivered(NEWER);

        // The new document dropped T_b and so lists T_a first: the kept
        // edit's *index* moves even though its labels did not.
        let newer = model(&["T_a"], &["-20", "-1"], NEWER);
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
            draft.base.as_deref(),
            Some(NEWER),
            "a rebased draft is against the document it was rebased onto"
        );
    }

    #[test]
    fn rebase_onto_a_document_that_lost_every_label_is_clean_again() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T_a", "-20"), Value::F64(1.0), BASE);
        draft.on_delivered(NEWER);
        let (kept, dropped) = draft.rebase(&model(&["T_z"], &["-20"], NEWER));
        assert_eq!(kept, 0);
        assert_eq!(dropped, vec![pair("T_a", "-20")]);
        assert_eq!(draft.state, DraftState::Clean);
        assert!(draft.edits.is_empty());
    }

    #[test]
    fn revert_clears_the_edits_and_counts_them() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), BASE);
        draft.set((0, 1), pair("T1", "-1"), Value::F64(2.0), BASE);
        assert_eq!(draft.revert(), 2);
        assert_eq!(draft.state, DraftState::Clean);
        assert!(draft.edits.is_empty());
        assert_eq!(draft.base, None);
        assert_eq!(draft.revert(), 0);
    }

    #[test]
    fn revert_clears_everything_including_the_behind_state() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), BASE);
        draft.on_delivered(NEWER);
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
            ((0, 0), pair("T1", "-20"), 1.0),
            ((0, 1), pair("T1", "-1"), 2.5),
        ];
        assert_eq!(draft.bump(cells.into_iter(), 0.5, BASE), 2);
        assert_eq!(draft.edits.get(&(0, 0)), Some(&Value::F64(1.5)));
        assert_eq!(draft.edits.get(&(0, 1)), Some(&Value::F64(3.0)));
        assert_eq!(draft.state, DraftState::Editing);
        // Bumping again reads the caller's *current* value, which is the
        // draft's own by then — the tile passes what the model paints.
        let again = vec![((0, 0), pair("T1", "-20"), 1.5)];
        assert_eq!(draft.bump(again.into_iter(), 0.5, BASE), 1);
        assert_eq!(draft.edits.get(&(0, 0)), Some(&Value::F64(2.0)));
    }

    /// `:bump` only ever reaches a `Number` cell — the tile decides that
    /// through `MatrixModel::kind_of`, before it ever builds the iterator
    /// `bump` takes — and `numeric_edit` is the door `MarketDataTile::bump`
    /// reads an existing edit's CURRENT value through: `F64`/`I64` widen
    /// to `f64`, a `Date`/`Utf8` edit (or no edit at all) answers `None`
    /// rather than being coerced.
    #[test]
    fn numeric_edit_reads_f64_and_i64_and_ignores_other_kinds() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.5), BASE);
        draft.set((0, 1), pair("T1", "-1"), Value::I64(7), BASE);
        draft.set((0, 2), pair("T1", "0"), Value::Utf8("x".into()), BASE);
        assert_eq!(draft.numeric_edit((0, 0)), Some(1.5));
        assert_eq!(draft.numeric_edit((0, 1)), Some(7.0));
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

        draft.set((0, 0), pair("T1", "-20"), Value::F64(1.0), BASE);
        assert_eq!(draft.badge(), DraftBadge::Dirty);
        assert_eq!(draft.count_phrase(), "1 cell");

        draft.set((0, 1), pair("T1", "-1"), Value::F64(1.0), BASE);
        draft.set((1, 1), pair("T2", "-1"), Value::F64(1.0), BASE);
        assert_eq!(draft.count_phrase(), "3 cells");

        draft.state = DraftState::Sent;
        assert_eq!(draft.badge(), DraftBadge::Sent);

        draft.state = DraftState::Behind {
            newer: NEWER.to_string(),
        };
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
    fn local_hhmm_shows_an_unparseable_time_verbatim_rather_than_swallowing_it() {
        assert_eq!(local_hhmm("not a time"), "not a time");
    }

    #[test]
    fn to_toml_and_from_toml_round_trip_the_edits_by_label_and_the_base() {
        let mut draft = Draft::default();
        draft.set((0, 1), pair("T1", "-1"), Value::F64(0.5), BASE);
        draft.set((1, 0), pair("T2", "-20"), Value::F64(0.25), BASE);

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
            restored.base.as_deref(),
            Some(BASE),
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
            let resolved = restored.rebase(&model(&["T2", "T1"], &["-20", "-1"], BASE));
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

    /// §4.3: a date or text cell edit is spelled `{ type, value }` on the
    /// wire, never a bare string — the same tag `to_toml` writes and the
    /// only shape `from_toml` accepts for either, so a stray untagged
    /// string (however it got into the file) is refused like any other
    /// malformed entry rather than guessed at.
    #[test]
    fn typed_edits_round_trip_through_toml_with_a_type_tag() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("D1", "ex"), Value::Date(d(2026, 12, 20)), BASE);
        draft.set((0, 1), pair("D1", "amount"), Value::F64(1.5), BASE);
        draft.set(
            (0, 2),
            pair("D1", "status"),
            Value::Utf8("paid".into()),
            BASE,
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
        assert_eq!(parse_cell(" 0.25 ", ColumnType::F64), Ok(0.25));
        assert_eq!(parse_cell("-3", ColumnType::F64), Ok(-3.0));
        assert_eq!(parse_cell("7", ColumnType::I64), Ok(7.0));

        let err = parse_cell("0.5", ColumnType::I64).expect_err("a whole number only");
        assert!(err.contains("0.5"), "{err}");
        let err = parse_cell("abc", ColumnType::F64).expect_err("not a number");
        assert!(err.contains("abc"), "{err}");
        let err = parse_cell("", ColumnType::F64).expect_err("nothing is not a number");
        assert!(!err.is_empty());
        let err = parse_cell("1e400", ColumnType::F64).expect_err("infinity is not a value");
        assert!(err.contains("1e400"), "{err}");
        let err = parse_cell("2026-10-16", ColumnType::Date).expect_err("dates are read-only");
        assert!(err.contains("2026-10-16"), "{err}");
    }

    #[test]
    fn an_attribute_edit_is_part_of_the_same_draft() {
        let mut draft = Draft::default();
        assert_eq!(draft.badge(), DraftBadge::Clean);
        draft.set_attr("spot_ref", Value::F64(4520.0), "2026-09-14T14:00:00Z");
        assert_eq!(draft.len(), 1);
        assert_eq!(draft.attr_count(), 1);
        assert_eq!(draft.base.as_deref(), Some("2026-09-14T14:00:00Z"));
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
        draft.set_attr("spot_ref", Value::F64(1.0), "t0");
        draft.set_attr("gone", Value::I64(2), "t0");
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
        draft.set_attr("anchor_date", Value::Date(d(2026, 9, 14)), "t0");
        draft.set_attr("spot_ref", Value::F64(4520.0), "t0");
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
        draft.set((0, 0), ("1M".into(), "-20".into()), Value::F64(0.1), "t0");
        draft.set((0, 1), ("1M".into(), "-10".into()), Value::F64(0.1), "t0");
        draft.set_attr("spot_ref", Value::F64(1.0), "t0");
        assert_eq!(draft.count_phrase(), "2 cells, spot_ref");
        let mut one = Draft::default();
        one.set((0, 0), ("1M".into(), "-20".into()), Value::F64(0.1), "t0");
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
                draft.set_attr(column, value.clone(), "t0");
            }
            draft.set_attr("anchor_date", Value::Date(d(2026, 9, 14)), "t0");
            draft.set_attr("free_text", Value::Utf8("a note".into()), "t0");

            let back = Draft::from_toml(&draft.to_toml());
            prop_assert_eq!(back.attrs, draft.attrs);
        }
    }
}
