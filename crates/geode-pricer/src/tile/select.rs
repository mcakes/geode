//! The sheet's grid selection: the state doors and what they prepare for
//! the footer. Anchored by the row's `LineId` and the plan column's
//! vocabulary name, so a repricing, an edit elsewhere or a column move
//! keeps it on the same cells; an anchor no longer painted clears it
//! with a notice rather than guessing a neighbour.

use super::*;
use crate::core::cell::READ_ONLY;
use crate::core::package::{self, package_qty};
use crate::core::select::{
    NOT_CONTIGUOUS, Skip, Skips, Targets, group_plan, move_plan, risk_totals_visible, set_notice,
    step_notice, targets, top_most,
};
use crate::core::sheet::OwnShifts;
use geode_core::grid::selection::Lost;
use geode_core::pricing::{Instrument, Measure};
use std::collections::BTreeMap;

/// A selection a verb took before its edit, and the order it held.
pub(crate) type TakenSelection = (
    Option<Selection<At, &'static str>>,
    Option<crate::core::sort::Held>,
);

/// The strongest of several targets' read-only reasons: [`SPLIT`] over
/// [`PARTLY_HIDDEN`], so a selection holding both names the grouping.
fn first_refusal(reasons: impl Iterator<Item = Option<&'static str>>) -> Option<&'static str> {
    let mut out = None;
    for why in reasons.flatten() {
        if why == SPLIT {
            return Some(SPLIT);
        }
        out = out.or(Some(why));
    }
    out
}

/// What a bulk edit writes ([`PricerTile::edit_targets`]).
#[derive(Debug, Clone, Default)]
pub(crate) struct EditTargets {
    pub(crate) targets: Targets,
    /// The plan columns it writes.
    pub(crate) cols: Vec<usize>,
    /// Lines the scope hides that an edited grouping row would hold:
    /// never written, counted as skipped in each column's notice
    /// ([`PricerTile::hidden_skips`]).
    pub(crate) hidden: Vec<usize>,
}

/// The refusal for `v`/`V` with no row or column under the cursor: an
/// empty sheet or a view with no plan columns.
const NO_ANCHOR: &str = "nothing to select";

/// Whether a column's cells step under the arrows: the kinds `cell::nudge`
/// steps. A live step opens only on one of these.
pub(crate) fn steppable(kind: ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::Qty
            | ColumnKind::Strike
            | ColumnKind::Barrier
            | ColumnKind::SpotShift
            | ColumnKind::VolShift
    )
}

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
                // A line or package row, with its node's group path (a
                // split package paints once per node under one id), or a
                // grouping row's path. A group anchor is lost when its path
                // stops painting — a regroup, its last line leaving it, a
                // closed ancestor — and the selection clears with a notice.
                let at = self.cursor_row().and_then(|g| self.at_of_row(g));
                let col = self.plan.columns.get(self.cursor.col).map(|c| c.def.name);
                let (Some(at), Some(col)) = (at, col) else {
                    self.footer = Some(NO_ANCHOR.into());
                    return;
                };
                self.selection = Some(Selection {
                    kind,
                    anchor_row: at,
                    anchor_col: col,
                });
                // Under a sort the rows hold this painted order while the
                // selection lives (`PricerTile::held_order`).
                if self.sort.is_some() {
                    self.held_order =
                        Some(crate::core::sort::painted_order(&self.rollup, &self.sheet));
                }
            }
        }
    }

    /// End the selection before a verb's edit, keeping it — with the
    /// painted order it holds under a sort — for [`Self::restore_selection`]
    /// should the edit refuse. The edit's rebuild runs selection-less and
    /// drops the held order, so it must travel with the selection.
    pub(crate) fn take_selection(&mut self) -> TakenSelection {
        let kept = (self.selection.take(), self.held_order.clone());
        self.clear_selection();
        kept
    }

    /// Put back a selection [`Self::take_selection`] took, and its held
    /// order, then rebuild: the refused edit's rebuild ran in ranked order,
    /// and the selection must resolve over the order it was made on.
    pub(crate) fn restore_selection(&mut self, kept: TakenSelection, cx: &mut Context<Self>) {
        let (selection, held) = kept;
        self.selection = selection;
        self.held_order = held;
        self.rebuild(cx);
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
        self.follow_own_anchor();
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
                |at| self.anchor_row(at),
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

    /// A grouping-row anchor a grouping-row editor's own write moved (a
    /// step on the grouped column carries the anchor group's lines to
    /// another path) follows its lead line to the group now holding it at
    /// the anchor's depth, so the selection stays painted through the
    /// steps and is whole again after `escape` takes them back. Only
    /// during that write (`own_write`): a regroup or another writer losing
    /// the anchor still clears the selection.
    fn follow_own_anchor(&mut self) {
        let Some(lead) = self.own_write.as_ref().and_then(|f| f.anchor_lead) else {
            return;
        };
        let Some(At::Group(path)) = self.selection.as_ref().map(|s| &s.anchor_row) else {
            return;
        };
        if self.group_row(path).is_some() {
            return;
        }
        let dest = self
            .sheet
            .index_of(lead)
            .and_then(|row| self.group_holding(row, path.len()))
            .filter(|p| self.group_row(p).is_some());
        if let (Some(dest), Some(sel)) = (dest, self.selection.as_mut()) {
            sel.anchor_row = At::Group(dest);
        }
    }

    /// One footer cell per measure column the view shows, totalled over
    /// the top-most selected rows: a package already carries its legs, so
    /// a total over both would double them. An incomplete total is `—`,
    /// never a partial sum that reads as the position's.
    fn prepare_totals(&mut self) {
        let top = self.selection_total_rows();
        let planned: Vec<_> = self
            .plan
            .columns
            .iter()
            .filter_map(|c| match c.def.kind {
                ColumnKind::Measure { measure, usd } => Some(((measure, usd), c)),
                _ => None,
            })
            .collect();
        let measures: Vec<(Measure, bool)> = planned.iter().map(|(m, _)| *m).collect();
        let sums = risk_totals_visible(&self.sheet, &top, &measures, &self.visibility);
        self.totals.clear();
        for ((_, planned), sum) in planned.into_iter().zip(sums) {
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
            .filter_map(|g| self.model.sheet_row(g))
            .collect()
    }

    /// Why the selection is read-only, if it is: it holds a package row
    /// that is split ([`SPLIT`]) or that the scope partly hides
    /// ([`PARTLY_HIDDEN`]). A selected package stands for every leg
    /// (`lines_of`), those another node paints or the scope hides too, so
    /// a bulk write through it would reach legs its row does not show: the
    /// whole write refuses. A grouping row, open or closed, never refuses:
    /// it stands for the shown lines beneath it ([`Self::edit_targets`]),
    /// and a package it holds only part of is skipped by a quantity, not
    /// refused whole.
    pub(crate) fn selection_read_only(&self) -> Option<&'static str> {
        first_refusal(
            self.selected_sheet_rows()
                .into_iter()
                .map(|r| self.read_only(r)),
        )
    }

    /// Whether the resolved selection reaches a grouping row.
    fn selection_holds_group(&self) -> bool {
        self.resolved
            .as_ref()
            .is_some_and(|r| r.rows.clone().any(|g| self.model.path(g).is_some()))
    }

    /// The rows a total or a `V` yank reaches: each selected grid row's
    /// sheet rows (a grouping row its contents, a split package its legs
    /// under that node; `grid_rows_under`), each once in painted order
    /// (a yank keeps what the screen shows, under a sort or a grouping),
    /// top-most — a grouping row selected with its own descendants, or a
    /// package with its legs, counts every leg once.
    pub(crate) fn selection_total_rows(&self) -> Vec<usize> {
        let Some(r) = &self.resolved else {
            return Vec::new();
        };
        let rows = first_seen(
            r.rows.clone().flat_map(|g| self.grid_rows_under(g)),
            self.sheet.len(),
        );
        top_most(&self.sheet, &rows)
    }

    /// The one door that answers what a bulk edit (a typed commit or a
    /// live step) writes. Under a live selection: the selected rows' leaf
    /// lines (a package stands for its legs) together with the shown lines
    /// each selected grouping row stands for, each line once, and the plan
    /// columns — the cursor's alone under `V`, whose columns span every
    /// column including read-only ones, and the block's under `v`. With
    /// no selection and the cursor on a grouping row: that group's shown
    /// lines, in the cursor's column. Otherwise `None`: a single line's
    /// edit is the editor's own path. A typed commit writes the cursor's
    /// column alone in every case; see [`Self::commit_selection`].
    ///
    /// An editor open on a grouping row answers from the lines it froze
    /// at open ([`FrozenTargets`]), not from what the path holds now: its
    /// own steps may have moved them out, and a delivery may have moved
    /// others in. A frozen line deleted since is passed over.
    pub(crate) fn edit_targets(&self) -> Option<EditTargets> {
        let cols = self.edit_cols();
        if let Some(EditOn::Group { frozen, .. }) = self.editor.as_ref().map(|e| e.target().0) {
            let rows = |ids: &[LineId]| -> Vec<usize> {
                ids.iter()
                    .filter_map(|&id| self.sheet.index_of(id))
                    .collect()
            };
            return Some(EditTargets {
                targets: targets(&self.sheet, &rows(&frozen.direct), &rows(&frozen.grouped)),
                cols,
                hidden: rows(&frozen.hidden),
            });
        }
        let (direct, grouped, hidden) = self.live_targets()?;
        Some(EditTargets {
            targets: targets(&self.sheet, &direct, &grouped),
            cols,
            hidden,
        })
    }

    /// The plan columns a bulk edit writes: a grouping-row editor's, frozen
    /// at open by name (a column the view no longer plans is passed over);
    /// else the block's under `v`, the cursor's otherwise (none while a
    /// selection has not resolved).
    fn edit_cols(&self) -> Vec<usize> {
        if let Some(frozen) = self.editor.as_ref().and_then(|e| e.frozen()) {
            return frozen
                .cols
                .iter()
                .filter_map(|name| self.plan.columns.iter().position(|c| c.def.name == *name))
                .collect();
        }
        self.live_cols()
    }

    fn live_cols(&self) -> Vec<usize> {
        match (&self.selection, &self.resolved) {
            (Some(_), None) => Vec::new(),
            (_, Some(r)) if r.kind == SelectKind::Block => r.cols.clone().collect(),
            _ => vec![self.cursor.col],
        }
    }

    /// What [`Self::edit_targets`] reads from the grid as it is now:
    /// `(selected rows, grouping rows' shown lines, their hidden lines)`.
    fn live_targets(&self) -> Option<(Vec<usize>, Vec<usize>, Vec<usize>)> {
        let (direct, groups) = if self.selection.is_some() {
            let Some(r) = &self.resolved else {
                return Some(Default::default());
            };
            let groups: Vec<Path> = r
                .rows
                .clone()
                .filter_map(|g| self.model.path(g).cloned())
                .collect();
            (self.selected_sheet_rows(), groups)
        } else if self.cursor_on_group() {
            let Some(At::Group(path)) = &self.cursor.at else {
                return None;
            };
            (Vec::new(), vec![path.clone()])
        } else {
            return None;
        };
        let grouped: Vec<usize> = groups.iter().flat_map(|p| self.group_lines(p)).collect();
        let hidden = rollup::hidden_under(
            &self.sheet,
            &self.visibility,
            &self.chain,
            &self.shared.dims.borrow(),
            self.clock,
            &groups,
        );
        Some((direct, grouped, hidden))
    }

    /// The targets a grouping-row editor opening now on the group at
    /// `path` keeps for its whole life, by line identity and column name,
    /// with the line it follows: the first line the cursor's group paints
    /// — never the selection's first in sheet order, which may sit in
    /// another group. `None` when that group paints no line (only empty
    /// packages): there is nothing to follow, nor to write.
    pub(crate) fn freeze_targets(&self, path: &[Option<String>]) -> Option<FrozenTargets> {
        let lead = self.first_painted_line(path)?;
        let (direct, grouped, hidden) = self.live_targets().unwrap_or_default();
        let ids = |rows: Vec<usize>| rows.into_iter().map(|r| self.sheet.id(r)).collect();
        let anchor_lead = match self.selection.as_ref().map(|s| &s.anchor_row) {
            Some(At::Group(anchor)) => self.first_painted_line(anchor),
            _ => None,
        };
        Some(FrozenTargets {
            direct: ids(direct),
            grouped: ids(grouped),
            hidden: ids(hidden),
            cols: self
                .live_cols()
                .into_iter()
                .filter_map(|c| self.plan.columns.get(c).map(|p| p.def.name))
                .collect(),
            lead: self.sheet.id(lead),
            depth: path.len(),
            anchor_lead: anchor_lead.map(|r| self.sheet.id(r)),
        })
    }

    /// The first line, in painted order, under the group at `path`:
    /// preorder over the rollup, which a sort has ranked.
    fn first_painted_line(&self, path: &[Option<String>]) -> Option<usize> {
        let mut stack = vec![self.group_node(path)?];
        while let Some(id) = stack.pop() {
            let n = &self.rollup.nodes[id];
            if let NodeKind::Leaf { row } = n.kind {
                return Some(row);
            }
            stack.extend(n.children.iter().rev());
        }
        None
    }

    /// The hidden lines a column's notice counts: a quantity counts a
    /// hidden leg of a package it skips as split only once, as that
    /// package's refusal.
    fn hidden_skips(&self, t: &EditTargets, qty: bool) -> usize {
        t.hidden
            .iter()
            .filter(|&&r| {
                !(qty
                    && self
                        .sheet
                        .parent(r)
                        .is_some_and(|p| t.targets.split.contains(&p)))
            })
            .count()
    }

    /// Run `f` — a rebuild after a grouping-row editor's own write — with
    /// [`PricerTile::own_write`] holding its frozen targets, so a
    /// selection anchor that write moved follows its lead line rather than
    /// clearing. An outer call's targets stand inside a nested one.
    pub(crate) fn as_own_write<R>(
        &mut self,
        frozen: Option<Rc<FrozenTargets>>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let outer = self.own_write.clone();
        self.own_write = outer.clone().or(frozen);
        let out = f(self);
        self.own_write = outer;
        out
    }

    /// A typed value committed over [`Self::edit_targets`] — a live
    /// selection's, or a grouping row's lines: every target line's cell in
    /// the cursor's column that takes it is written, as ONE undo entry;
    /// the rest are skipped and counted in the notice, with the lines the
    /// scope hides from an edited group. Answers whether the editor closes
    /// — it stays open when no cell accepts the value or the sheet refuses
    /// the batch, and then nothing was written. `date` is the date field's
    /// value, which an expiry cell takes as a date rather than as text.
    ///
    /// The column is the cursor's under `v` too: one absolute text parsed
    /// into several column grammars (qty 5 and strike 5, a type option in
    /// the underlying) would be a plausible wrong value. Each cell is
    /// judged on its own line's instrument, never on the cursor cell's.
    ///
    /// A quantity sets each root ([`Targets`]): a selected package, or one
    /// a grouping row holds whole, scales its legs by the template's
    /// weights — the per-leg path would write the bare typed number to
    /// every leg, turning a -5/+5 spread into -5/-5; a package a group
    /// holds only part of is skipped and counted refused.
    ///
    /// Re-tagging a grouping row's lines (its own grouped column, or an
    /// enclosing group's) moves them out of its path: the cursor follows
    /// them to the group row they now sit under at the same depth.
    pub(crate) fn commit_selection(
        &mut self,
        text: &str,
        date: Option<chrono::NaiveDate>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.loading {
            self.footer = Some("the sheet is still loading".into());
            self.rebuild_chrome();
            cx.notify();
            return false;
        }
        if let Some(why) = self.selection_read_only() {
            self.footer = Some(why.into());
            self.sync_editor(cx);
            self.rebuild_chrome();
            cx.notify();
            return false;
        }
        let Some(et) = self.edit_targets() else {
            return false;
        };
        let Some(planned) = self.plan.columns.get(self.cursor.col) else {
            return false;
        };
        let (kind, editable, format) = (
            planned.def.kind,
            planned.def.editable,
            planned.format.clone(),
        );
        let hidden = self.hidden_skips(&et, kind == ColumnKind::Qty && editable);
        let t = et.targets;
        let mut edits = Vec::new();
        let mut set = 0usize;
        let mut skips = Skips::default();
        // A quantity sets each root: a package through its weights, a
        // bare line (or a leg selected on its own row) to the number.
        let lines = if kind == ColumnKind::Qty && editable {
            for &pkg in &t.packages {
                let legs = self.sheet.children(pkg);
                // In list form the legs no longer fit the template, so there
                // are no weights, and one typed number would land on every
                // leg unscaled: refused, none of its legs written.
                if package_qty(&self.sheet, pkg).is_none() {
                    skips.add(Skip::Refused);
                    continue;
                }
                match package::commit(&self.sheet, pkg, kind, &format, text) {
                    Ok(es) => {
                        set += legs.clone().filter(|&l| self.sheet.is_line(l)).count();
                        edits.extend(es);
                    }
                    Err(_) => skips.add(Skip::Refused),
                }
            }
            // A package a group holds only part of: one number through its
            // weights would rescale legs the group does not show.
            for _ in &t.split {
                skips.add(Skip::Refused);
            }
            t.qty_lines
        } else {
            t.lines
        };
        for &line in &lines {
            if !editable {
                skips.add(Skip::ReadOnly);
                continue;
            }
            // `cell` answers `READ_ONLY` for a barrier cell on a vanilla
            // line too; the notice tells the two apart, since the cell is
            // not read-only on the lines it applies to.
            let barrier_col = matches!(kind, ColumnKind::Barrier | ColumnKind::BarrierType);
            if barrier_col && matches!(self.sheet.instrument(line), Some(Instrument::Vanilla(_))) {
                skips.add(Skip::NotApplicable);
                continue;
            }
            let answer = match (kind, date) {
                (ColumnKind::Expiry, Some(d)) => cell::commit_date(&self.sheet, line, d),
                _ => cell::commit(&self.sheet, line, kind, text),
            };
            match answer {
                Ok(Some(edit)) => {
                    edits.push(edit);
                    set += 1;
                }
                // Already that value: it counts as set, with no edit.
                Ok(None) => set += 1,
                Err(why) if why == READ_ONLY => skips.add(Skip::ReadOnly),
                Err(_) => skips.add(Skip::Refused),
            }
        }
        for _ in 0..hidden {
            skips.add(Skip::Hidden);
        }
        if set == 0 {
            let whose = if self.selection.is_some() {
                "selected"
            } else {
                "grouped"
            };
            self.footer =
                Some(format!("no {whose} cell accepts '{text}'{}", skips.describe()).into());
            self.sync_editor(cx);
            self.rebuild_chrome();
            cx.notify();
            return false;
        }
        // A grouping-row editor closes before the write lands: the write
        // may carry the lead line anywhere, into a closed group even, and
        // a field still open across that rebuild would be dropped with a
        // footer for a commit that succeeded. The cursor then goes where
        // the lead-line rule puts it, or the nearest painted row above.
        let frozen = self.editor.as_ref().and_then(|e| e.frozen());
        if frozen.is_some() {
            self.release_editor(cx);
        }
        // Every edit is a `Set*` on its own line, so no edit shifts
        // another's row index; one batch makes one undo entry and a
        // refusal rolls the whole batch back. Defensive: every edit was
        // validated against the sheet above, so no route reaches this
        // refusal today — but should the sheet's own checks ever outgrow
        // the cell's, the value must not half-land (a grouping-row editor
        // is closed by then; the footer says why nothing changed).
        if !edits.is_empty()
            && let Err(e) = self.as_own_write(frozen.clone(), |this| this.apply_edits(edits, cx))
        {
            self.footer = Some(e.to_string().into());
            self.sync_editor(cx);
            self.rebuild_chrome();
            cx.notify();
            return false;
        }
        if let Some(frozen) = frozen {
            self.place_cursor_by_lead(&frozen, cx);
        }
        self.notice = Some(set_notice(set, &skips).into());
        self.rebuild_chrome();
        cx.notify();
        true
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
            "group" => Some("g p packages rows — use V"),
            "ungroup" => Some("g u unpackages rows — use V"),
            _ => None,
        }
    }

    /// The one door every structural verb passes before it mutates: `d`,
    /// `shift+j`/`shift+k`, `g p` and `g u` (keys, the `.` menu, and
    /// `:package`/`:unpackage`) refuse when their target could reach
    /// lines its row does not paint. In order: a counted `g p` under a
    /// value grouping ([`PACKAGE_GROUPED`]), a move or counted `g p` under
    /// a sort ([`MOVE_SORTED`], [`PACKAGE_SORTED`]) — sheet order is not
    /// the painted order there (a move under a value grouping is instead
    /// held to its own group by [`Self::move_lands`]); a target that is a grouping row
    /// ([`GROUP_ROW`]: no line behind it); a target package split across
    /// nodes ([`SPLIT`]) — for a move, a leg of one too — or partly hidden
    /// by the scope ([`PARTLY_HIDDEN`]). `selected`: the verb acts on the
    /// live selection, which then refuses as a whole — no part of it is
    /// acted on. Otherwise the target is the cursor row; for `g u` its
    /// package (a leg's parent). A counted `g p` takes the `count` sheet
    /// rows from the cursor (`Edit::Group`), so it also refuses with
    /// [`HIDDEN_IN_RANGE`] when any of them is hidden: it would package a
    /// line the trader never saw. (A selection's `g p` holds only shown
    /// rows, and `group_plan` refuses a gap.) A shown leg on its own is
    /// not a package and stays editable. Verbs that mutate nothing (yank,
    /// fold, find, motions) never ask.
    pub(crate) fn partly_hidden_refusal(
        &self,
        verb: &str,
        count: usize,
        selected: bool,
    ) -> Option<&'static str> {
        if !matches!(
            verb,
            "delete" | "move_down" | "move_up" | "group" | "ungroup"
        ) {
            return None;
        }
        if verb == "group" && !selected && count > 1 && self.grouped() {
            return Some(PACKAGE_GROUPED);
        }
        if matches!(verb, "move_down" | "move_up") && self.sort.is_some() {
            return Some(MOVE_SORTED);
        }
        if verb == "group" && !selected && count > 1 && self.sort.is_some() {
            return Some(PACKAGE_SORTED);
        }
        let targets: Vec<usize> = if selected {
            if self.selection_holds_group() {
                return Some(GROUP_ROW);
            }
            self.selected_sheet_rows()
        } else {
            if self.cursor_on_group() {
                return Some(GROUP_ROW);
            }
            let row = self.cursor_sheet_row()?;
            match verb {
                "ungroup" => vec![self.sheet.parent(row).unwrap_or(row)],
                "group" => {
                    let taken = row..row.saturating_add(count.max(1)).min(self.sheet.len());
                    if taken.clone().any(|r| !self.visibility.is_shown(r)) {
                        return Some(HIDDEN_IN_RANGE);
                    }
                    taken.collect()
                }
                _ => vec![row],
            }
        };
        let moving = matches!(verb, "move_down" | "move_up");
        first_refusal(targets.into_iter().map(|r| {
            self.read_only(r)
                .or_else(|| self.split_leg_refusal(r, moving))
        }))
    }

    /// A move of a leg whose package the grouping splits: its sibling
    /// legs paint under other groups' package rows, so its sibling order
    /// is not one painted order ([`SPLIT`]).
    pub(crate) fn split_leg_refusal(&self, row: usize, moving: bool) -> Option<&'static str> {
        (moving && self.sheet.parent(row).is_some_and(|p| self.split(p))).then_some(SPLIT)
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
                let top = self.selection_total_rows();
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
                // Every selected row, on screen or not, formatted on demand.
                let model = Rc::clone(&self.model);
                let mut pass = CellPass::new(self.fill_source(), &model);
                for g in r.rows.clone() {
                    lines.push(
                        r.cols
                            .clone()
                            .map(|c| {
                                pass.cell(g, c)
                                    .map(|x| x.text.to_string())
                                    .unwrap_or_default()
                            })
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
    /// and land in the register in painted order. The specs are read before
    /// any remove, since each remove shifts the indices after it; the
    /// removes run bottom-up so the earlier indices stay valid.
    pub(crate) fn delete_selection(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if let Some(why) = self
            .row_verb_refusal("delete")
            .or_else(|| self.partly_hidden_refusal("delete", 1, true))
        {
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
        let kept = self.take_selection();
        if let Err(e) = self.apply_edits(edits, cx) {
            // A refused batch leaves the sheet as it was, so the selection
            // still names what the user picked — re-resolved, or the footer
            // would show no extent or totals over a live selection.
            self.restore_selection(kept, cx);
            return Err(e.to_string());
        }
        let n = specs.len();
        self.register = Some(specs);
        self.notice = Some(format!("deleted {n} row{}", if n == 1 { "" } else { "s" }).into());
        Ok(())
    }

    /// `shift+j`/`shift+k` over a `V` selection: the block slides one
    /// sibling step as a unit, as ONE move of the neighbour across it —
    /// under a value grouping, the next neighbour in the block's own group
    /// ([`Self::move_lands`]). The selection stays: it is anchored by line
    /// id, so the rebuild re-resolves it onto the moved lines.
    pub(crate) fn move_selection(
        &mut self,
        down: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let top = top_most(&self.sheet, &self.selected_sheet_rows());
        let edit = {
            let (lands, edge) = self.move_lands(&top)?;
            move_plan(&self.sheet, &top, down, lands, edge)?
        };
        self.apply_edit(edit, cx).map_err(|e| e.to_string())
    }

    /// `g p` over a `V` selection, which it ends: contiguous root lines
    /// become one custom package, opened, with the cursor on it.
    pub(crate) fn group_selection(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let top = top_most(&self.sheet, &self.selected_sheet_rows());
        let (first, count) = group_plan(&self.sheet, &top).map_err(|why| {
            // Adjacent on screen is not adjacent in the sheet under a sort.
            match self.sort {
                Some(_) if why == NOT_CONTIGUOUS => GROUP_SORTED,
                _ => why,
            }
        })?;
        // Cleared first: the new package starts closed, so the rebuild
        // after the edit would otherwise report the anchor line as lost.
        let kept = self.take_selection();
        let edit = Edit::Group {
            first,
            count,
            template: Template::CUSTOM,
            id: None,
        };
        if let Err(e) = self.apply_edit(edit, cx) {
            // Re-resolved as well as restored: a bare restore leaves the
            // footer's extent and totals empty over a live selection.
            self.restore_selection(kept, cx);
            return Err(e.to_string());
        }
        let id = self.sheet.id(first);
        self.expansion.set(id, true);
        self.cursor.set_line(id);
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
        let kept = self.take_selection();
        if let Err(e) = self.apply_edits(edits, cx) {
            // A refused batch leaves the sheet as it was; the kept
            // selection is re-resolved so its footer strip comes back.
            self.restore_selection(kept, cx);
            return Err(e.to_string());
        }
        Ok(())
    }

    /// The open editor's live-step state, taken out of it.
    pub(crate) fn take_bulk(&mut self) -> Option<Bulk> {
        match &mut self.editor {
            Some(Editor::Text { bulk, .. }) => bulk.take(),
            _ => None,
        }
    }

    /// A sheet replace drops any live step unrecorded: its inverses
    /// address the old sheet's rows, and replaying or recording them
    /// against the new one would write into unrelated lines.
    pub(crate) fn forget_steps(&mut self) {
        self.take_bulk();
        self.edit_seq += 1;
    }

    /// The editor's arrow keys with a selection live and its text
    /// untouched: step every target cell by its own text's unit, in the
    /// sheet, now — each press reprices through the ordinary path, and
    /// the tile's in-flight bookkeeping supersedes the previous press's
    /// batch. `None` when this is not that case (no selection, typed
    /// text, a cursor cell that does not step, no step editor), so
    /// `nudge` keeps its single-field behaviour; `Some` otherwise.
    ///
    /// A press is all or nothing: a cell whose stepped value the sheet
    /// refuses (a qty stepping to zero) refuses the whole press, since
    /// the rest landing without it would be a plausible wrong block.
    /// Cells that cannot step at all (read-only, not a number, a barrier
    /// on a vanilla) are skipped and counted.
    ///
    /// Under `V` the cursor's column steps; under `v` every block column.
    /// A selected package's quantity steps as the PACKAGE quantity through
    /// its template's weights: its legs stepped one by one would turn a
    /// -5/+5 spread into -4/+6.
    pub(crate) fn bulk_step(
        &mut self,
        steps: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let (on, kind, input, seeded) = match &self.editor {
            Some(Editor::Text {
                on,
                kind,
                input,
                bulk: Some(b),
                ..
            }) => (on.clone(), *kind, input.clone(), b.seeded.clone()),
            _ => return None,
        };
        // A delivery that lost the anchor cleared the selection under an
        // open line editor: there are no targets, and the field is one
        // cell's. A grouping row's editor targets its own lines.
        if self.selection.is_none() && !matches!(on, EditOn::Group { .. }) {
            return None;
        }
        if input.read(cx).value().as_ref() != seeded || !steppable(kind) {
            return None;
        }
        // The editor's cell is the one the trader watches: steps taken
        // while the cursor sits elsewhere would move every target with no
        // sign of it in the field.
        if !self.cursor_on_editor(&on, kind) {
            return None;
        }
        // Refused here rather than answered `None`: the single-field
        // nudge would then step the editor's text, which the commit would
        // refuse anyway.
        if let Some(why) = self.selection_read_only() {
            self.footer = Some(why.into());
            self.rebuild_chrome();
            cx.notify();
            return Some(());
        }
        let et = self.edit_targets()?;
        let mut edits: Vec<Edit> = Vec::new();
        // Each stepped line's instrument and own shifts as this press has
        // left them so far. `SetInstrument` and `SetShift` rewrite the whole
        // record, so a block over strike and barrier, or both shifts, built
        // from the sheet per column would have the later column's edit put
        // back the earlier one's value.
        let mut line_work: BTreeMap<usize, (Instrument, OwnShifts)> = BTreeMap::new();
        let mut stepped = 0usize;
        let mut skips = Skips::default();
        let t = &et.targets;
        for &col in &et.cols {
            let Some(planned) = self.plan.columns.get(col) else {
                continue;
            };
            let (ckind, editable, format) = (
                planned.def.kind,
                planned.def.editable,
                planned.format.clone(),
            );
            for _ in 0..self.hidden_skips(&et, ckind == ColumnKind::Qty && editable) {
                skips.add(Skip::Hidden);
            }
            let col_lines = if ckind == ColumnKind::Qty && editable {
                &t.qty_lines
            } else {
                &t.lines
            };
            if ckind == ColumnKind::Qty && editable {
                // A package a group holds only part of has no weights to
                // step the shown legs by alone.
                for _ in &t.split {
                    skips.add(Skip::Refused);
                }
                for &pkg in &t.packages {
                    let legs = self.sheet.children(pkg);
                    // In list form the legs no longer fit the template, so
                    // there are no weights to step them by.
                    if package_qty(&self.sheet, pkg).is_none() {
                        skips.add(Skip::Refused);
                        continue;
                    }
                    let Some(text) = package::editor_text(&self.sheet, pkg, ckind, &format) else {
                        skips.add(Skip::ReadOnly);
                        continue;
                    };
                    let Ok(next) = cell::nudge(ckind, &text, steps) else {
                        skips.add(Skip::NotNumeric);
                        continue;
                    };
                    match package::commit(&self.sheet, pkg, ckind, &format, &next) {
                        Ok(es) => {
                            stepped += legs.filter(|&l| self.sheet.is_line(l)).count();
                            edits.extend(es);
                        }
                        Err(why) => return self.refuse_step(why, cx),
                    }
                }
            }
            for &l in col_lines {
                if !editable {
                    skips.add(Skip::ReadOnly);
                    continue;
                }
                // An inheriting shift steps from the value it paints.
                let text = match cell::editor_for(&self.sheet, l, ckind, &format) {
                    Ok(CellEditor::Text(t)) => cell::step_from(&self.sheet, ckind, &t),
                    Ok(_) => {
                        skips.add(Skip::NotNumeric);
                        continue;
                    }
                    // `editor_for` answers `READ_ONLY` for a barrier cell on
                    // a vanilla line too; the notice tells the two apart.
                    Err(_) => {
                        let vanilla =
                            matches!(self.sheet.instrument(l), Some(Instrument::Vanilla(_)));
                        skips.add(if ckind == ColumnKind::Barrier && vanilla {
                            Skip::NotApplicable
                        } else {
                            Skip::ReadOnly
                        });
                        continue;
                    }
                };
                let Ok(next) = cell::nudge(ckind, &text, steps) else {
                    skips.add(Skip::NotNumeric);
                    continue;
                };
                // Built on this press's earlier cells of the same line, not
                // on the sheet: see `line_work`.
                let Some((inst, own)) = line_work.get(&l).cloned().or_else(|| {
                    let i = self.sheet.instrument(l)?.clone();
                    Some((i, self.sheet.shift(l)))
                }) else {
                    skips.add(Skip::ReadOnly);
                    continue;
                };
                match cell::edit_on(&inst, own, l, ckind, &next) {
                    Ok(Edit::SetInstrument { instrument, .. }) => {
                        line_work.insert(l, (instrument, own));
                        stepped += 1;
                    }
                    Ok(Edit::SetShift { shift, .. }) => {
                        line_work.insert(l, (inst, shift));
                        stepped += 1;
                    }
                    Ok(e) => {
                        edits.extend(cell::changed(&self.sheet, l, e));
                        stepped += 1;
                    }
                    Err(why) if why == READ_ONLY => skips.add(Skip::ReadOnly),
                    Err(why) => return self.refuse_step(why, cx),
                }
            }
        }
        // One whole-record edit per line and record: each carries every
        // cell of the line this press stepped.
        for (row, (instrument, shift)) in line_work {
            edits.extend(cell::changed(
                &self.sheet,
                row,
                Edit::SetInstrument { row, instrument },
            ));
            edits.extend(cell::changed(
                &self.sheet,
                row,
                Edit::SetShift { row, shift },
            ));
        }
        if edits.is_empty() {
            return self.refuse_step(format!("no cells to step{}", skips.describe()), cx);
        }
        let touched: Vec<usize> = edits
            .iter()
            .filter_map(|e| match e {
                Edit::SetQty { row, .. }
                | Edit::SetInstrument { row, .. }
                | Edit::SetShift { row, .. } => Some(*row),
                _ => None,
            })
            .collect();
        let before: Vec<(LineId, StepMark)> = touched
            .iter()
            .map(|&r| (self.sheet.id(r), self.step_mark(r)))
            .collect();
        // Every rebuild below is this editor's own write: a step on the
        // grouped column (or under a `status` grouping) can move a
        // selection's grouping-row anchor, which then follows its lines
        // rather than clearing (the editor itself follows its lead line
        // on every rebuild, own write or not).
        let written = self.editor.as_ref().and_then(|e| e.frozen());
        let undo = match self.apply_batch(edits) {
            Ok(Some(u)) => u,
            Ok(None) => return Some(()),
            Err(e) => {
                // Unreachable while every edit is validated above; should
                // the sheet's checks outgrow the cell's, nothing half-lands
                // and the rebuild shows whatever a refused rollback left.
                self.as_own_write(written, |this| this.after_edit(cx));
                return self.refuse_step(e.to_string(), cx);
            }
        };
        let after: Vec<(LineId, StepMark)> = touched
            .into_iter()
            .map(|r| (self.sheet.id(r), self.step_mark(r)))
            .collect();
        // The step's inverse joins the bulk BEFORE the rebuild: a rebuild
        // that drops the editor (`follow_editor`) records the bulk's
        // inverses, and one not yet in it would leave this step in the
        // sheet with no history.
        let Some(Editor::Text {
            bulk: Some(bulk), ..
        }) = &mut self.editor
        else {
            self.undo.record(undo);
            self.as_own_write(written, |this| this.after_edit(cx));
            return Some(());
        };
        let mut inverse = undo.inverse;
        inverse.append(&mut bulk.undo.inverse);
        bulk.undo.inverse = inverse;
        bulk.steps += steps;
        // A line's first mark is kept (what `i` found); its after-mark is
        // replaced by each press. Keyed, so a long press over a large
        // selection stays linear.
        for (id, mark) in before {
            bulk.before.entry(id).or_insert(mark);
        }
        bulk.after.extend(after);
        let total = bulk.steps;
        self.as_own_write(written, |this| this.after_edit(cx));
        // The field follows its own cell: the step moved it too. A
        // grouping row's cell is the value its frozen lines share, still
        // empty where they differ.
        let reseed = self.plan.columns.get(self.cursor.col).and_then(|planned| {
            let format = &planned.format;
            let opened = match &on {
                EditOn::Line(line) => {
                    let row = self.sheet.index_of(*line)?;
                    cell::editor_for(&self.sheet, row, kind, format)
                }
                EditOn::Group { frozen, .. } => {
                    let lines: Vec<usize> = frozen
                        .grouped
                        .iter()
                        .filter_map(|&id| self.sheet.index_of(id))
                        .collect();
                    crate::core::select::group_editor(&self.sheet, &lines, kind, format)
                }
            };
            match opened {
                Ok(CellEditor::Text(t)) => Some(t),
                _ => None,
            }
        });
        let seq = self.edit_seq;
        let Some(Editor::Text {
            bulk: Some(bulk),
            opened,
            ..
        }) = &mut self.editor
        else {
            return Some(());
        };
        bulk.seq = seq;
        if let Some(text) = &reseed {
            // A package cell's commit checks the text it opened on; the
            // step changed that text, not the trader.
            if opened.is_some() {
                *opened = Some(text.clone());
            }
            bulk.seeded = text.clone();
        }
        let notice: SharedString = step_notice(stepped, total, &skips).into();
        bulk.notice = Some(notice.clone());
        self.notice = Some(notice);
        if let Some(text) = reseed {
            input.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        self.sync_editor(cx);
        Some(())
    }

    /// A refused press: the reason in the footer, nothing written, the
    /// editor and its steps so far as they were.
    fn refuse_step(&mut self, why: impl Into<SharedString>, cx: &mut Context<Self>) -> Option<()> {
        self.footer = Some(why.into());
        self.sync_editor(cx);
        Some(())
    }

    /// What a step may have changed on `row`, to tell later whether the
    /// sheet still holds the steps.
    fn step_mark(&self, row: usize) -> StepMark {
        (
            self.sheet.qty(row),
            self.sheet.instrument(row).cloned(),
            self.sheet.shift(row),
        )
    }

    /// End a live step. `keep` (an untouched `enter`) records the steps
    /// as ONE undo entry; otherwise they come out of the sheet
    /// ([`Self::take_back_steps`]) and the rebuild, reprice and re-armed
    /// save follow — a save mid-step persisted the stepped values, and
    /// the rollback's own save replaces them.
    pub(crate) fn settle_bulk(&mut self, bulk: Bulk, keep: bool, cx: &mut Context<Self>) {
        if keep {
            // Steps that net to nothing (up then down) leave every stepped
            // line as `i` found it: an entry for them would be an undo
            // that changes nothing.
            let net_zero = bulk.steps == 0
                && bulk.before.iter().all(|(id, mark)| {
                    self.sheet
                        .index_of(*id)
                        .is_some_and(|r| self.step_mark(r) == *mark)
                });
            if !bulk.undo.inverse.is_empty() && !net_zero {
                self.undo.record(bulk.undo);
            }
            return;
        }
        let shown = bulk.notice.clone();
        // A typed commit after steps takes them back with its grouping-row
        // editor still open: the rollback is its own write, so a selection
        // anchor the steps moved follows the lines back.
        let frozen = self.editor.as_ref().and_then(|e| e.frozen());
        if self.take_back_steps(bulk) {
            if shown.is_some() && self.notice == shown {
                self.notice = None;
            }
            self.as_own_write(frozen, |this| this.after_edit(cx));
        }
    }

    /// Take the steps back out of the sheet, only while they are still
    /// its last change: no recorded edit since (`edit_seq`) and every
    /// stepped line still as the last step left it. Otherwise something
    /// else wrote the sheet meanwhile, and replaying the inverses would
    /// undo that write or land on moved rows, so the steps are recorded
    /// as one undo entry instead and stay undoable. Answers whether the
    /// sheet changed; the caller rebuilds.
    pub(crate) fn take_back_steps(&mut self, bulk: Bulk) -> bool {
        if bulk.undo.inverse.is_empty() {
            return false;
        }
        let unchanged = bulk.seq == self.edit_seq
            && bulk.after.iter().all(|(id, mark)| {
                self.sheet
                    .index_of(*id)
                    .is_some_and(|r| self.step_mark(r) == *mark)
            });
        if !unchanged {
            self.undo.record(bulk.undo);
            return false;
        }
        if let Err(e) = self.sheet.undo(&bulk.undo) {
            // A refused inverse leaves the sheet partly rolled back; the
            // history's inverses assume the layout before it.
            tracing::error!(
                target: "geode::pricing",
                tile = self.id.0,
                error = %e,
                "rolling back a live step failed; the sheet is partly rolled back \
                 and the undo history is cleared"
            );
            self.undo.clear();
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn resolved(&self) -> Option<&Resolved> {
        self.resolved.as_ref()
    }
}

/// `rows` once each, in the order first met: the painted order a yank
/// keeps, which a sort or a grouping makes differ from sheet order. A
/// split package's legs reach here once per node that paints them.
pub(crate) fn first_seen(rows: impl IntoIterator<Item = usize>, universe: usize) -> Vec<usize> {
    let mut seen = vec![false; universe];
    rows.into_iter()
        .filter(|&r| match seen.get_mut(r) {
            Some(s) if !*s => {
                *s = true;
                true
            }
            _ => false,
        })
        .collect()
}
