//! The panel's grid selection: state doors, and every verb that takes the
//! selection as its operand. Anchored by row label and column label, so a
//! redelivery, an inserted row or a rebase keeps it on the same cells; an
//! anchor no longer painted clears it with a notice rather than guessing a
//! neighbour.

use super::*;
use geode_core::grid::selection::{Lost, Selection};

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
    #[allow(dead_code)] // the selection-wide edits are its callers
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

    /// The live selection as last resolved — the test reader for what
    /// the delegate was handed.
    #[cfg(test)]
    pub(crate) fn resolved(&self) -> Option<&Resolved> {
        self.resolved.as_ref()
    }
}
