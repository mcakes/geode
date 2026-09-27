//! The sheet's grid selection: the state doors and what they prepare for
//! the footer. Anchored by the row's `LineId` and the plan column's
//! vocabulary name, so a repricing, an edit elsewhere or a column move
//! keeps it on the same cells; an anchor no longer painted clears it
//! with a notice rather than guessing a neighbour.

use super::*;
use crate::core::select::{RISK, group_plan, lines_of, move_plan, risk_totals, top_most};
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

    /// A row verb's refusal under a `v` block, whose cells are not a set
    /// of rows: acting on the block's rows would edit rows the user never
    /// picked as rows.
    pub(crate) fn row_verb_refusal(&self, verb: &str) -> Option<&'static str> {
        if self.selection.as_ref().map(|s| s.kind) != Some(SelectKind::Block) {
            return None;
        }
        match verb {
            "delete" => Some("d deletes rows — use V"),
            "move_down" | "move_up" => Some("shift+j/k move rows — use V"),
            "group" => Some("g p groups rows — use V"),
            "ungroup" => Some("g u ungroups rows — use V"),
            _ => None,
        }
    }

    /// `y` over a selection, which it ends. Under `V` the clipboard gets
    /// the top-most rows' shorthand, one per line, and the register their
    /// specs — a package's legs are already in its own spec, so copying
    /// them too would double them on `p`. Under `v` the clipboard gets the
    /// block as TSV under its column labels; a block is not rows, so the
    /// register keeps what it held.
    pub(crate) fn yank_selection(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(r) = self.resolved.clone() else {
            return Err("select with V or v first".into());
        };
        match r.kind {
            SelectKind::Rows => {
                let top = top_most(&self.sheet, &self.selected_sheet_rows());
                let text = top
                    .iter()
                    .map(|&row| self.sheet.shorthand(row))
                    .collect::<Vec<_>>()
                    .join("\n");
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                self.register = Some(top.iter().map(|&row| spec_of(&self.sheet, row)).collect());
            }
            SelectKind::Block => {
                // Grid cells are indexed by plan column, as `Resolved.cols` is.
                let header = r
                    .cols
                    .clone()
                    .map(|c| self.plan.columns[c].label.as_str())
                    .collect::<Vec<_>>()
                    .join("\t");
                let mut lines = vec![header];
                for g in r.rows.clone() {
                    let row = &self.model.rows[g];
                    lines.push(
                        r.cols
                            .clone()
                            .map(|c| row.cells.get(c).map(|x| x.text.as_ref()).unwrap_or(""))
                            .collect::<Vec<_>>()
                            .join("\t"),
                    );
                }
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(lines.join("\n")));
            }
        }
        self.clear_selection();
        Ok(())
    }

    /// `d` over a `V` selection: the top-most rows go as ONE undo entry
    /// and land in the register in sheet order. The specs are read before
    /// any remove, since each remove shifts the indices after it; the
    /// removes run bottom-up so the earlier indices stay valid.
    pub(crate) fn delete_selection(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if let Some(why) = self.row_verb_refusal("delete") {
            return Err(why.into());
        }
        let top = top_most(&self.sheet, &self.selected_sheet_rows());
        if top.is_empty() {
            return Err("no row".into());
        }
        let specs: Vec<RowSpec> = top.iter().map(|&row| spec_of(&self.sheet, row)).collect();
        let mut at = top.clone();
        at.sort_unstable_by(|a, b| b.cmp(a));
        let edits = at.into_iter().map(|at| Edit::Remove { at }).collect();
        // Cleared first: the rebuild after the edit would otherwise find the
        // anchor gone and report a lost selection over a deliberate delete.
        let kept = self.selection.take();
        self.clear_selection();
        if let Err(e) = self.apply_edits(edits, cx) {
            // A refused batch leaves the sheet as it was, so the selection
            // still names what the user picked — re-resolved, or the footer
            // would show no extent or totals over a live selection.
            self.selection = kept;
            self.refresh_selection();
            self.rebuild_chrome();
            return Err(e.to_string());
        }
        let n = specs.len();
        self.register = Some(specs);
        self.notice = Some(format!("deleted {n} row{}", if n == 1 { "" } else { "s" }).into());
        Ok(())
    }

    /// `shift+j`/`shift+k` over a `V` selection: the block slides one
    /// sibling step as a unit, as ONE move of the neighbour across it.
    /// The selection stays: it is anchored by line id, so the rebuild
    /// re-resolves it onto the moved lines.
    pub(crate) fn move_selection(
        &mut self,
        down: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let top = top_most(&self.sheet, &self.selected_sheet_rows());
        let edit = move_plan(&self.sheet, &top, down)?;
        self.apply_edit(edit, cx).map_err(|e| e.to_string())
    }

    /// `g p` over a `V` selection, which it ends: contiguous root lines
    /// become one custom package, opened, with the cursor on it.
    pub(crate) fn group_selection(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let top = top_most(&self.sheet, &self.selected_sheet_rows());
        let (first, count) = group_plan(&self.sheet, &top)?;
        // Cleared first: the new package starts closed, so the rebuild
        // after the edit would otherwise report the anchor line as lost.
        let kept = self.selection.take();
        self.clear_selection();
        let edit = Edit::Group {
            first,
            count,
            template: Template::CUSTOM,
            id: None,
        };
        if let Err(e) = self.apply_edit(edit, cx) {
            // Re-resolved as well as restored: a bare restore leaves the
            // footer's extent and totals empty over a live selection.
            self.selection = kept;
            self.refresh_selection();
            self.rebuild_chrome();
            return Err(e.to_string());
        }
        let id = self.sheet.id(first);
        self.expansion.set(id, true);
        self.cursor.line = Some(id);
        self.rebuild(cx);
        Ok(())
    }

    /// `g u` over a `V` selection, which it ends: every top-most selected
    /// package dissolves as ONE undo entry. The ungroups run bottom-up
    /// so each earlier package's index is still valid when it is reached.
    pub(crate) fn ungroup_selection(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let mut packages: Vec<usize> = top_most(&self.sheet, &self.selected_sheet_rows())
            .into_iter()
            .filter(|&r| self.sheet.is_package(r))
            .collect();
        if packages.is_empty() {
            return Err("no package selected".into());
        }
        packages.sort_unstable_by(|a, b| b.cmp(a));
        let edits = packages
            .into_iter()
            .map(|row| Edit::Ungroup { row })
            .collect();
        // Cleared first: an anchor on a package row vanishes with it, and
        // that is the verb's intent, not a lost selection.
        let kept = self.selection.take();
        self.clear_selection();
        if let Err(e) = self.apply_edits(edits, cx) {
            // A refused batch leaves the sheet as it was; the kept
            // selection is re-resolved so its footer strip comes back.
            self.selection = kept;
            self.refresh_selection();
            self.rebuild_chrome();
            return Err(e.to_string());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn resolved(&self) -> Option<&Resolved> {
        self.resolved.as_ref()
    }
}
