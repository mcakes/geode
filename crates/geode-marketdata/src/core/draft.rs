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
    /// Edits present and a newer generation has been delivered. The panel
    /// keeps painting the base generation under the edits; `:rebase` moves
    /// them onto the newer one and `:discard` drops them.
    Behind { newer: String },
    /// An upload succeeded; the edits are kept and painted as sent until
    /// the echo clears them (§9.4, Part 4).
    Sent,
}

/// Edits keyed by grid cell, with the labels that make them portable.
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    /// The source time of the generation every edit was made against,
    /// RFC 3339. `None` exactly when there are no edits.
    pub base: Option<String>,
    pub edits: BTreeMap<(usize, usize), f64>,
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
    pub fn len(&self) -> usize {
        self.edits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
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
    pub fn set(&mut self, cell: (usize, usize), labels: (String, String), value: f64, base: &str) {
        if self.edits.is_empty() || self.base.is_none() {
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

    /// Drop every edit, answering how many there were. The draft is
    /// `Clean` afterwards and carries no base, since a base describes a
    /// set of edits.
    pub fn revert(&mut self) -> usize {
        let n = self.edits.len();
        self.edits.clear();
        self.labels.clear();
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
    pub fn bump(
        &mut self,
        cells: impl Iterator<Item = ((usize, usize), (String, String), f64)>,
        delta: f64,
        base: &str,
    ) -> usize {
        let mut n = 0;
        for (cell, labels, current) in cells {
            self.set(cell, labels, current + delta, base);
            n += 1;
        }
        n
    }

    /// A generation was delivered. Answers whether the state changed, so
    /// the caller knows whether anything needs repainting.
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
            // Already behind, and a *further* generation arrived: the
            // header must name the newest one, not the first one missed.
            DraftState::Behind { newer }
                if newer != as_of && self.base.as_deref() != Some(as_of) =>
            {
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
                    edits.insert(new_cell, *value);
                    labels.insert(new_cell, (row_label.clone(), col_label.clone()));
                }
                None => dropped.push((row_label.clone(), col_label.clone())),
            }
        }

        self.edits = edits;
        self.labels = labels;
        self.base = model_of_newer.source_time.clone();
        self.state = if self.edits.is_empty() {
            DraftState::Clean
        } else {
            DraftState::Editing
        };
        (self.edits.len(), dropped)
    }

    /// Drop the edits and the base outright — `:discard`, which is how a
    /// trader says "show me the new document".
    pub fn discard(&mut self) {
        self.revert();
    }

    /// The header's one line about the draft. Times are the trader's local
    /// clock throughout (Phase 4a's ruling), so an RFC 3339 base is
    /// converted, and an unparseable one is shown verbatim rather than
    /// hidden — a panel that cannot read its own base should say so.
    pub fn summary(&self) -> String {
        let count = self.edits.len();
        match &self.state {
            DraftState::Clean => String::new(),
            DraftState::Behind { newer } => {
                format!("newer document received {}", local_hhmm(newer))
            }
            DraftState::Editing => self.count_phrase(count, ""),
            DraftState::Sent => self.count_phrase(count, " sent"),
        }
    }

    fn count_phrase(&self, count: usize, verb: &str) -> String {
        let plural = if count == 1 { "" } else { "s" };
        match &self.base {
            Some(base) => format!(
                "{count} edit{plural}{verb} on {}'s document",
                local_hhmm(base)
            ),
            None => format!("{count} edit{plural}{verb}"),
        }
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
                    toml::Value::Float(*value),
                ]))
            })
            .collect();
        table.insert("edits".into(), toml::Value::Array(edits));
        table
    }

    /// Read a session's draft back. Every edit lands at
    /// [`UNRESOLVED_COLUMN`] until [`Draft::rebase`] against the first
    /// model resolves it by label; a malformed entry is skipped rather
    /// than taking the whole draft with it (unsent work is worth more than
    /// tidiness).
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
            let (Some(row_label), Some(col_label), Some(value)) =
                (triple[0].as_str(), triple[1].as_str(), as_f64(&triple[2]))
            else {
                continue;
            };
            let cell = (i, UNRESOLVED_COLUMN);
            edits.insert(cell, value);
            labels.insert(cell, (row_label.to_string(), col_label.to_string()));
        }
        let state = if edits.is_empty() {
            DraftState::Clean
        } else {
            DraftState::Editing
        };
        Draft {
            base,
            edits,
            state,
            labels,
        }
    }
}

fn as_f64(value: &toml::Value) -> Option<f64> {
    // A whole number round-trips through TOML as an integer, so a draft
    // written as `1.0` reads back as `1` and must still be a value.
    value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))
}

fn local_hhmm(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(t) => t.with_timezone(&chrono::Local).format("%H:%M").to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

/// Parse a typed cell to the number a document holds.
///
/// Only `f64` and `i64` columns are editable — a document's values are
/// declared one of those two (spec §3.2) and an axis or attribute is
/// read-only in slice 1 — so anything else is refused by type rather than
/// coerced. Every message names the text it refused, because the inline
/// notice appears beside a field the trader can no longer see the whole of.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::matrix::{Cell, MatrixModel, RowModel};
    use geode_core::schema::ColumnType;
    use gpui::SharedString;

    const BASE: &str = "2026-09-12T14:02:00Z";
    const NEWER: &str = "2026-09-12T14:07:00Z";

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
        }
    }

    #[test]
    fn the_first_edit_records_the_base_and_a_second_edit_on_one_cell_keeps_the_latest() {
        let mut draft = Draft::default();
        assert_eq!(draft.state, DraftState::Clean);
        draft.set((0, 1), pair("T1", "-1"), 0.5, BASE);
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.base.as_deref(), Some(BASE));
        draft.set((0, 1), pair("T1", "-1"), 0.7, BASE);
        assert_eq!(draft.edits.len(), 1);
        assert_eq!(draft.edits.get(&(0, 1)), Some(&0.7));
        assert_eq!(
            draft.base.as_deref(),
            Some(BASE),
            "every edit in one draft is against one generation"
        );
    }

    #[test]
    fn on_delivered_stays_editing_on_the_same_generation_and_goes_behind_on_a_newer_one() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), 1.0, BASE);

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
        assert_eq!(draft.edits.get(&(0, 0)), Some(&1.0));
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
        draft.set((1, 1), pair("T_a", "-1"), 0.5, BASE);
        draft.set((0, 0), pair("T_b", "-20"), 0.25, BASE);
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
            Some(&0.5),
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
        draft.set((0, 0), pair("T_a", "-20"), 1.0, BASE);
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
        draft.set((0, 0), pair("T1", "-20"), 1.0, BASE);
        draft.set((0, 1), pair("T1", "-1"), 2.0, BASE);
        assert_eq!(draft.revert(), 2);
        assert_eq!(draft.state, DraftState::Clean);
        assert!(draft.edits.is_empty());
        assert_eq!(draft.base, None);
        assert_eq!(draft.revert(), 0);
    }

    #[test]
    fn discard_clears_everything_including_the_behind_state() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), 1.0, BASE);
        draft.on_delivered(NEWER);
        draft.discard();
        assert!(draft.edits.is_empty());
        assert_eq!(draft.state, DraftState::Clean);
        assert_eq!(draft.base, None);
        assert_eq!(draft.summary(), "");
    }

    #[test]
    fn bump_adds_the_delta_to_each_cells_current_value() {
        let mut draft = Draft::default();
        let cells = vec![
            ((0, 0), pair("T1", "-20"), 1.0),
            ((0, 1), pair("T1", "-1"), 2.5),
        ];
        assert_eq!(draft.bump(cells.into_iter(), 0.5, BASE), 2);
        assert_eq!(draft.edits.get(&(0, 0)), Some(&1.5));
        assert_eq!(draft.edits.get(&(0, 1)), Some(&3.0));
        assert_eq!(draft.state, DraftState::Editing);
        // Bumping again reads the caller's *current* value, which is the
        // draft's own by then — the tile passes what the model paints.
        let again = vec![((0, 0), pair("T1", "-20"), 1.5)];
        assert_eq!(draft.bump(again.into_iter(), 0.5, BASE), 1);
        assert_eq!(draft.edits.get(&(0, 0)), Some(&2.0));
    }

    #[test]
    fn summary_spells_each_state_in_the_traders_local_clock() {
        let local = |rfc: &str| {
            chrono::DateTime::parse_from_rfc3339(rfc)
                .unwrap()
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        };

        let mut draft = Draft::default();
        assert_eq!(draft.summary(), "");

        draft.set((0, 0), pair("T1", "-20"), 1.0, BASE);
        assert_eq!(
            draft.summary(),
            format!("1 edit on {}'s document", local(BASE))
        );
        draft.set((0, 1), pair("T1", "-1"), 1.0, BASE);
        draft.set((1, 1), pair("T2", "-1"), 1.0, BASE);
        assert_eq!(
            draft.summary(),
            format!("3 edits on {}'s document", local(BASE))
        );

        draft.state = DraftState::Sent;
        assert_eq!(
            draft.summary(),
            format!("3 edits sent on {}'s document", local(BASE))
        );

        draft.on_delivered(NEWER);
        draft.state = DraftState::Behind {
            newer: NEWER.to_string(),
        };
        assert_eq!(
            draft.summary(),
            format!("newer document received {}", local(NEWER))
        );
    }

    #[test]
    fn an_unparseable_base_is_shown_verbatim_rather_than_swallowed() {
        let mut draft = Draft::default();
        draft.set((0, 0), pair("T1", "-20"), 1.0, "not a time");
        assert_eq!(draft.summary(), "1 edit on not a time's document");
    }

    #[test]
    fn to_toml_and_from_toml_round_trip_the_edits_by_label_and_the_base() {
        let mut draft = Draft::default();
        draft.set((0, 1), pair("T1", "-1"), 0.5, BASE);
        draft.set((1, 0), pair("T2", "-20"), 0.25, BASE);

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
            assert_eq!(restored.edits.get(&(1, 1)), Some(&0.5), "T1 × -1");
            assert_eq!(restored.edits.get(&(0, 0)), Some(&0.25), "T2 × -20");
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
        assert_eq!(draft.edits.values().next(), Some(&3.0));
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
}
