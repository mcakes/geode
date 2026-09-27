//! The sheet's grid selection: the state doors and what they prepare for
//! the footer. Anchored by the row's `LineId` and the plan column's
//! vocabulary name, so a repricing, an edit elsewhere or a column move
//! keeps it on the same cells; an anchor no longer painted clears it
//! with a notice rather than guessing a neighbour.

use super::*;
use crate::core::select::{RISK, lines_of, risk_totals, top_most};
use geode_core::grid::selection::Lost;

/// The refusal for `v`/`V` on a row with no line behind it.
const NO_ANCHOR: &str = "select from a line or package row";

impl PricerTile {
    /// `v`/`V`: start at the cursor cell, switch kind keeping the anchor,
    /// or clear on the same kind again.
    pub(crate) fn start_selection(&mut self, kind: SelectKind) {
        match self.selection.as_ref().map(|s| s.kind) {
            Some(k) if k == kind => self.clear_selection(),
            Some(_) => {
                if let Some(s) = self.selection.as_mut() {
                    s.kind = kind;
                }
            }
            None => {
                let line = self
                    .cursor_row()
                    .and_then(|g| self.model.rows.get(g))
                    .and_then(|r| r.id);
                let col = self.plan.columns.get(self.cursor.col).map(|c| c.def.name);
                let (Some(line), Some(col)) = (line, col) else {
                    self.footer = Some(NO_ANCHOR.into());
                    return;
                };
                self.selection = Some(Selection {
                    kind,
                    anchor_row: line,
                    anchor_col: col,
                });
            }
        }
    }

    /// Drop the selection and everything prepared from it.
    pub(crate) fn clear_selection(&mut self) {
        self.selection = None;
        self.resolved = None;
        self.selection_extent = None;
        self.totals.clear();
    }

    /// Re-resolve against the current model and cursor, preparing the
    /// footer's extent and totals. Answers whether the anchor was lost:
    /// the selection is then cleared and the footer says why, so the
    /// caller re-prepares the chrome.
    pub(crate) fn refresh_selection(&mut self) -> bool {
        let Some(sel) = &self.selection else {
            self.clear_selection();
            return false;
        };
        // No cursor row means an empty grid, where the anchor is not
        // painted either.
        let outcome = match self.cursor_row() {
            Some(row) => sel.resolve_with(
                (row, self.cursor.col),
                self.plan.columns.len(),
                |id| self.model.grid_row_of(*id),
                |name| self.plan.columns.iter().position(|c| c.def.name == *name),
            ),
            None => Err(Lost::Row),
        };
        match outcome {
            Ok(r) => {
                let (rows, cols) = (r.rows.len(), r.cols.len());
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                self.selection_extent =
                    Some(format!("{rows} row{} × {cols} col{}", plural(rows), plural(cols)).into());
                self.resolved = Some(r);
                self.prepare_totals();
                false
            }
            Err(lost) => {
                self.clear_selection();
                self.footer = Some(
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

    /// One footer cell per risk column the view shows, totalled over the
    /// top-most selected rows: a package already carries its legs, so a
    /// total over both would double them. An incomplete total is `—`,
    /// never a partial sum that reads as the position's.
    fn prepare_totals(&mut self) {
        let top = top_most(&self.sheet, &self.selected_sheet_rows());
        let sums = risk_totals(&self.sheet, &top);
        self.totals.clear();
        for (kind, sum) in RISK.iter().zip(sums) {
            let Some(planned) = self.plan.columns.iter().find(|c| c.def.kind == *kind) else {
                continue;
            };
            let cell = match sum {
                Some(v) => {
                    let f = geode_core::format::format_number(v, &planned.format);
                    AggregateCell {
                        label: planned.label.clone().into(),
                        text: f.text.into(),
                        sign: Some(f.sign),
                        refused: false,
                    }
                }
                None => AggregateCell {
                    label: planned.label.clone().into(),
                    text: "—".into(),
                    sign: None,
                    refused: true,
                },
            };
            self.totals.push(cell);
        }
    }

    /// The selected grid rows' sheet rows, in grid order.
    pub(crate) fn selected_sheet_rows(&self) -> Vec<usize> {
        let Some(r) = &self.resolved else {
            return Vec::new();
        };
        r.rows
            .clone()
            .filter_map(|g| self.model.rows.get(g).and_then(|m| m.row))
            .collect()
    }

    /// What a bulk edit writes: the selected rows' leaf lines (a package
    /// stands for its legs) and the plan columns — the cursor's alone
    /// under `V`, whose columns span every column including read-only
    /// ones, and the block's under `v`.
    // Its readers are the bulk edit verbs.
    #[allow(dead_code)]
    pub(crate) fn selection_targets(&self) -> (Vec<usize>, Vec<usize>) {
        let Some(r) = &self.resolved else {
            return (Vec::new(), Vec::new());
        };
        let lines = lines_of(&self.sheet, &self.selected_sheet_rows());
        let cols = match r.kind {
            SelectKind::Rows => vec![self.cursor.col],
            SelectKind::Block => r.cols.clone().collect(),
        };
        (lines, cols)
    }

    #[cfg(test)]
    pub(crate) fn resolved(&self) -> Option<&Resolved> {
        self.resolved.as_ref()
    }
}
