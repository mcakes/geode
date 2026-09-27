//! The panel's grid selection: state doors, and every verb that takes the
//! selection as its operand. Anchored by row label and column label, so a
//! redelivery, an inserted row or a rebase keeps it on the same cells; an
//! anchor no longer painted clears it with a notice rather than guessing a
//! neighbour.

use super::*;
use crate::core::bulk::{self, Skip, Skips};
use crate::core::draft::bumped;
use geode_core::grid::selection::{Lost, Selection};

/// One stepped cell ready to write: its model position, the labels that
/// make the edit portable across generations, and the new value.
type StepWrite = ((usize, usize), (String, String), Value);

/// The notice when a selection editor closes over steps it cannot undo.
pub(super) const STEPS_KEPT: &str = "steps kept: the document moved";

/// What closing a selection editor did with its live steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StepsUndo {
    /// Nothing was stepped; the draft is as it was.
    Nothing,
    /// The draft is back as `i` found it; the caller rebuilds.
    Restored,
    /// The painted generation moved while the editor was open, so the
    /// steps stay in the draft and the notice says so.
    Kept,
}

impl MarketDataTile {
    /// `v`/`V`: start at the cursor cell, switch kind keeping the anchor,
    /// or clear on the same kind again. Refused in the attribute strip,
    /// which is never a member.
    pub(super) fn start_selection(&mut self, kind: SelectKind) {
        match self.selection.as_ref().map(|s| s.kind) {
            Some(k) if k == kind => self.selection = None,
            Some(_) => {
                if let Some(s) = self.selection.as_mut() {
                    s.kind = kind;
                }
            }
            None => {
                let Cursor::Cell { row, col } = self.cursor else {
                    self.notice = Some("select from a grid cell".into());
                    return;
                };
                let (Some(r), Some(c)) = (self.model.rows.get(row), self.model.columns.get(col))
                else {
                    return;
                };
                self.selection = Some(Selection {
                    kind,
                    anchor_row: r.label.clone(),
                    anchor_col: c.clone(),
                });
            }
        }
    }

    pub(super) fn clear_selection(&mut self) {
        self.selection = None;
        self.resolved = None;
        self.selection_extent = None;
    }

    /// Re-resolve against the current model and cursor, preparing the
    /// footer extent. Answers whether the anchor was lost (the selection
    /// is then cleared and the notice set), so the caller re-prepares
    /// the header.
    pub(super) fn refresh_selection(&mut self) -> bool {
        let Some(sel) = &self.selection else {
            self.resolved = None;
            self.selection_extent = None;
            return false;
        };
        let outcome = match self.cursor {
            Cursor::Cell { row, col } => sel.resolve_with(
                (row, col),
                self.model.columns.len(),
                |label| self.model.rows.iter().position(|r| &r.label == label),
                |name| self.model.columns.iter().position(|c| c == name),
            ),
            // Unreachable: every door into the strip (`cursor_to_attr`)
            // clears the selection first, motions clamp, and `v` refuses
            // there. Should one be missed, clear without a notice — the
            // anchor is still painted, so "no longer shown" would be false.
            Cursor::Attr(_) => {
                self.clear_selection();
                return false;
            }
        };
        match outcome {
            Ok(r) => {
                let (rows, cols) = (r.rows.len(), r.cols.len());
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                self.selection_extent =
                    Some(format!("{rows} row{} × {cols} col{}", plural(rows), plural(cols)).into());
                self.resolved = Some(r);
                false
            }
            Err(lost) => {
                self.clear_selection();
                self.notice = Some(
                    match lost {
                        Lost::Row => "selection cleared: anchor row no longer shown",
                        Lost::Column => "selection cleared: anchor column no longer shown",
                    }
                    .into(),
                );
                true
            }
        }
    }

    /// The model cells a selection-wide edit visits, row-major. A `Rows`
    /// selection skips the leading slice values (`:bump row`'s rule: a
    /// term's forward/atm/skew never move with its ladder); a `Block` is
    /// exactly its rectangle. The row-label column is never a model
    /// column, so it is never here.
    pub(super) fn selection_cells(&self) -> Vec<(usize, usize)> {
        let Some(r) = &self.resolved else {
            return Vec::new();
        };
        let skip = match r.kind {
            SelectKind::Rows => self.model.slice_columns,
            SelectKind::Block => 0,
        };
        r.rows
            .clone()
            .flat_map(|row| {
                r.cols
                    .clone()
                    .filter(move |&c| c >= skip)
                    .map(move |c| (row, c))
            })
            .collect()
    }

    /// `y` in visual mode: tab-separated, a header line first. `Rows`
    /// copies what `y y` copies for each row (the label where it is
    /// painted, then every column) under a header of the same shape, so
    /// the pasted grid lines up with its column names; `Block` copies its
    /// own columns' header and cells, no label.
    pub(super) fn selection_tsv(&self) -> Option<String> {
        let r = self.resolved.as_ref()?;
        let label = matches!(r.kind, SelectKind::Rows) && self.spec.rows.shown();
        let mut out = Vec::with_capacity(r.rows.len() + 1);
        let header: Vec<&str> = label
            .then_some(self.spec.rows.column)
            .into_iter()
            .chain(
                r.cols
                    .clone()
                    .filter_map(|c| self.model.columns.get(c).map(|s| s.as_ref())),
            )
            .collect();
        out.push(header.join("\t"));
        for row in r.rows.clone() {
            let m = self.model.rows.get(row)?;
            let line: Vec<&str> = label
                .then(|| m.label.as_ref())
                .into_iter()
                .chain(
                    r.cols
                        .clone()
                        .filter_map(|c| m.cells.get(c).map(|cell| cell.text.as_ref())),
                )
                .collect();
            out.push(line.join("\t"));
        }
        Some(out.join("\n"))
    }

    /// `d` in visual mode: a `Rows` selection deletes every selected row
    /// and ends the selection. Labels are collected before any delete,
    /// because dropping an inserted row shifts every later index. A
    /// `Block` refuses rather than deleting whole rows it only partly
    /// covers. Refused outright (selection kept) only when every row was
    /// already deleted.
    pub(super) fn delete_selected_rows(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self
            .resolved
            .as_ref()
            .is_some_and(|r| r.kind == SelectKind::Block)
        {
            return Err("d deletes rows — use V".to_string());
        }
        let (_, base) = self.row_verb_target(window, cx)?;
        // `row_verb_target` may have closed a provisional row-label editor,
        // dropping that row and shifting every later index; labels must
        // be read through a range resolved against the model as it is
        // now, or they name the wrong rows. If the anchor went with the
        // dropped row the selection is gone: refuse with the notice that
        // says so, not with "already deleted" over an empty label list.
        if self.refresh_selection() || self.resolved.is_none() {
            return Err(self
                .notice
                .as_ref()
                .map_or_else(|| "selection cleared".to_string(), |n| n.to_string()));
        }
        let labels: Vec<String> = self
            .resolved
            .as_ref()
            .map(|r| {
                r.rows
                    .clone()
                    .filter_map(|i| self.model.rows.get(i).map(|m| m.label.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let mut already = 0usize;
        for label in &labels {
            if self.draft.delete_row(label, &base) == RowDelete::Already {
                already += 1;
            }
        }
        if already == labels.len() {
            return Err(ALREADY_DELETED.to_string());
        }
        self.clear_selection();
        self.notice = None;
        self.rebuild_model(cx);
        Ok(())
    }

    /// Write one step per cell, each `(cell, current, declared type,
    /// delta)`, answering how many were written. Every result is computed
    /// before any write, so one refusal (a fractional delta on an integer
    /// column, an overflow) leaves the whole draft as it was: a partly
    /// stepped block would be a plausible wrong state. A document row's
    /// cell is keyed for `Draft::edits` by its `cell_ref` (the document
    /// position — `commit_cell_value`'s rule); an inserted row's goes to
    /// its `RowEdit.cells` by label, with the same arithmetic. Does not
    /// rebuild the model; the caller does, once.
    pub(super) fn write_steps(
        &mut self,
        values: Vec<((usize, usize), Value, ColumnType, f64)>,
    ) -> Result<usize, String> {
        let base = self.edit_base()?;
        let mut writes: Vec<StepWrite> = Vec::with_capacity(values.len());
        for (cell, current, ty, delta) in values {
            let labels = self.model.label_of(cell);
            let labels = (labels.0.to_string(), labels.1.to_string());
            let value = bumped(&current, delta, ty, &labels.1)?;
            writes.push((cell, labels, value));
        }
        let n = writes.len();
        for (cell, (row_label, col_label), value) in writes {
            let row = &self.model.rows[cell.0];
            match row.state {
                RowState::Inserted => {
                    self.draft.set_row_cell(&row_label, &col_label, value);
                }
                RowState::Document | RowState::Deleted => {
                    let cell_ref = row.cells[cell.1].cell_ref;
                    self.draft
                        .set(cell_ref, (row_label, col_label), value, &base);
                }
            }
        }
        Ok(n)
    }

    /// Collect and write one step over the selection: every member on a
    /// live row whose column is a number and whose value is not NULL, each
    /// moved by `delta_of(col, ty)` (`None` for a non-number column).
    /// Answers how many were written and why the rest were not. Gates:
    /// `held_refusal`, then `edit_base`. Shared by `:bump` and the live
    /// step, so both apply the same arithmetic and the same skip rules.
    pub(super) fn step_selection_cells(
        &mut self,
        delta_of: impl Fn(usize, ColumnType) -> Option<f64>,
    ) -> Result<(usize, Skips), String> {
        if let Some(refusal) = self.held_refusal() {
            return Err(refusal.to_string());
        }
        self.edit_base()?;
        let mut skips = Skips::default();
        let mut values = Vec::new();
        for (row, col) in self.selection_cells() {
            if self.model.rows[row].state == RowState::Deleted {
                skips.add(Skip::Deleted);
                continue;
            }
            let ty = self.column_type(col);
            let Some(delta) = delta_of(col, ty) else {
                skips.add(Skip::NotNumeric);
                continue;
            };
            match self.current_numeric(row, col) {
                Some(v) => values.push(((row, col), v, ty, delta)),
                None => skips.add(Skip::Empty),
            }
        }
        if values.is_empty() {
            return Err(format!("no numeric cells to step{}", skips.describe()));
        }
        let n = self.write_steps(values)?;
        Ok((n, skips))
    }

    /// `:bump <delta>` with a selection live: every selected number moves
    /// by `delta`, and the selection stays (a repeatable edit).
    pub(super) fn bump_selection(
        &mut self,
        delta: f64,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let numbers: Vec<bool> = (0..self.model.columns.len())
            .map(|c| matches!(self.model.kind_of(c), Some(CellKind::Number(_))))
            .collect();
        let (n, skips) = self.step_selection_cells(|col, _| {
            numbers.get(col).copied().unwrap_or(false).then_some(delta)
        })?;
        self.rebuild_model(cx);
        self.notice = Some(format!("bumped {}{}", bulk::cells(n), skips.describe()).into());
        self.changed(cx);
        Ok(())
    }

    /// The editor's arrow keys with a selection live and its text
    /// untouched: step every selected number by its own column's unit, in
    /// the draft, now, so the grid shows the block as it moves. `None`
    /// when this is not that case (typed text, a cursor cell that is not
    /// a number, no selection editor), so `nudge` keeps its own
    /// single-cell behaviour; `Some(chrome)` otherwise. A refusal (behind,
    /// no numbers) writes nothing and leaves the editor as it was.
    pub(super) fn bulk_step(
        &mut self,
        steps: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        let editing = self.editor.as_ref()?;
        let bulk = editing.bulk.as_ref()?;
        let EditorState::Text(state) = &editing.state else {
            return None;
        };
        let EditTarget::Cell { cell, labels } = editing.target.clone() else {
            return None;
        };
        let state = state.clone();
        if state.read(cx).value().as_ref() != bulk.seeded {
            return None;
        }
        if !matches!(self.model.kind_of(cell.1), Some(CellKind::Number(_))) {
            return None;
        }
        let precisions: Vec<Option<u8>> = (0..self.model.columns.len())
            .map(|c| match self.model.kind_of(c) {
                Some(CellKind::Number(f)) => Some(f.precision),
                _ => None,
            })
            .collect();
        let result = self.step_selection_cells(|col, ty| {
            precisions
                .get(col)
                .copied()
                .flatten()
                .map(|p| bulk::step_delta(ty, p, steps))
        });
        let (n, skips) = match result {
            Ok(done) => done,
            Err(e) => {
                self.notice = Some(e.into());
                return Some(true);
            }
        };
        self.rebuild_model(cx);
        // The editor follows its own cell by label: the rebuild keeps rows
        // in place (a step never inserts or drops one), and a grid that
        // moved anyway seeds nothing rather than another cell's value.
        let text = (self.model.label_of(cell) == labels)
            .then(|| self.model.rows[cell.0].cells[cell.1].text.to_string());
        if let Some(text) = &text {
            state.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
        }
        let bulk = self.editor.as_mut()?.bulk.as_mut()?;
        if let Some(text) = text {
            bulk.seeded = text;
        }
        bulk.steps += steps;
        bulk.stepped = true;
        self.notice = Some(bulk::step_notice(n, bulk.steps, &skips).into());
        Some(true)
    }

    /// Take a closing selection editor's live steps back out of the
    /// draft. Only while the generation it opened on is still painted:
    /// after an automatic rebase or replace, `before` is keyed to a grid
    /// no longer on screen, and restoring it would put edits on the wrong
    /// cells, so the steps are kept and the notice says so. A delivery
    /// held behind meanwhile stays news (`Draft::restore_from`), and an
    /// empty restore is never behind, so the retained base is dropped.
    /// Does not rebuild; on `Restored` the caller does.
    pub(super) fn undo_steps(&mut self, bulk: Bulk) -> StepsUndo {
        if !bulk.stepped {
            return StepsUndo::Nothing;
        }
        // Whole-pair equality: a generation this snapshot cannot vouch for
        // is treated as moved, which keeps work rather than misplacing it.
        if bulk.painted != self.model.base {
            self.notice = Some(STEPS_KEPT.into());
            return StepsUndo::Kept;
        }
        self.draft.restore_from(bulk.before);
        if !self.draft.is_behind() {
            self.leave_behind();
        }
        // The step count is no longer true of the draft.
        self.notice = None;
        StepsUndo::Restored
    }

    /// `enter` on a typed value with a selection live: write it to every
    /// selected cell whose kind accepts it, skipping and counting the
    /// rest. Every member is judged before any write, so when nothing
    /// accepts the commit is refused with the editor open and the draft
    /// untouched. Keeps the selection (a repeatable edit). Answers
    /// whether the header needs re-preparing.
    pub(super) fn commit_bulk(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(refusal) = self.held_refusal() {
            self.notice = Some(refusal.into());
            return true;
        }
        let base = match self.edit_base() {
            Ok(base) => base,
            Err(e) => {
                self.notice = Some(e.into());
                return true;
            }
        };
        let nothing_accepts = |skips: &Skips| {
            format!(
                "no selected cell accepts '{}'{}",
                text.trim(),
                skips.describe()
            )
        };
        let mut skips = Skips::default();
        let mut writes = Vec::new();
        for (row, col) in self.selection_cells() {
            if self.model.rows[row].state == RowState::Deleted {
                skips.add(Skip::Deleted);
                continue;
            }
            // An inserted row's cell lands by label in the draft's own row
            // edit (`set_row_cell`). One the model paints but the draft no
            // longer holds would take no write; judging it here, before
            // anything is written, keeps the count honest and lets a
            // selection of nothing else refuse with the draft untouched.
            // Every draft change rebuilds the model, so no production
            // route is known to reach this; it guards the notice against
            // claiming cells the trader will not find edited.
            if self.model.rows[row].state == RowState::Inserted
                && !matches!(
                    self.draft.row_state(self.model.rows[row].label.as_ref()),
                    Some(RowEdit::Inserted { .. })
                )
            {
                skips.add(Skip::Moved);
                continue;
            }
            let Some(kind) = self.model.kind_of(col) else {
                continue;
            };
            let ty = declared_type(self.spec, &self.model, col);
            match bulk::accept(kind, ty, self.column_required(col), text) {
                Ok(value) => writes.push(((row, col), value)),
                Err(skip) => skips.add(skip),
            }
        }
        if writes.is_empty() {
            self.notice = Some(nothing_accepts(&skips).into());
            return true;
        }
        // Typed text replaces any live steps: a cell that refuses the
        // value goes back to its pre-`i` value rather than keeping a
        // half-step. The restore changes no row structure (a step never
        // inserts or drops a row), so the model's labels and `cell_ref`s
        // read below still hold.
        let kept = match self.editor.as_mut().and_then(|e| e.bulk.take()) {
            Some(bulk) => self.undo_steps(bulk) == StepsUndo::Kept,
            None => false,
        };
        // Labels and `cell_ref`s are read from the model as it stands; it
        // is rebuilt only after every write, so no write shifts another's
        // target.
        let mut n = 0;
        for ((row, col), value) in writes {
            let labels = self.model.label_of((row, col));
            let m = &self.model.rows[row];
            let written = match m.state {
                RowState::Inserted => {
                    self.draft
                        .set_row_cell(labels.0.as_ref(), labels.1.as_ref(), value)
                }
                RowState::Document => {
                    let cell_ref = m.cells[col].cell_ref;
                    self.draft.set(
                        cell_ref,
                        (labels.0.to_string(), labels.1.to_string()),
                        value,
                        &base,
                    );
                    true
                }
                // Filtered above; never written.
                RowState::Deleted => false,
            };
            // Judged above, so a refused write here is a broken
            // invariant; it is still counted as skipped rather than
            // claimed as set.
            if written {
                n += 1;
            } else {
                skips.add(Skip::Moved);
            }
        }
        if n == 0 {
            self.notice = Some(nothing_accepts(&skips).into());
            return true;
        }
        self.close_editor(window, cx);
        self.close_popup_with_window(window, cx);
        self.rebuild_model(cx);
        let set = bulk::set_notice(n, &skips);
        self.notice = Some(
            if kept {
                format!("{set}; {STEPS_KEPT}")
            } else {
                set
            }
            .into(),
        );
        true
    }

    /// The live selection as last resolved — the test reader for what
    /// the delegate was handed.
    #[cfg(test)]
    pub(crate) fn resolved(&self) -> Option<&Resolved> {
        self.resolved.as_ref()
    }
}
