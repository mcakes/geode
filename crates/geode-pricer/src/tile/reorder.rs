//! Moving rows: the predicate both the keys' steps and the grip's drag
//! land by, held to the mover's own rollup group under a value grouping,
//! and the grip's drag itself.
//!
//! A grip press prepares a [`RowDragState`]: the movers (the row, or the
//! live `V` selection it sits in) and their [`DropPlan`], or the refusal
//! the keys would give, shown at once. The tile's body follows the gpui
//! drag: the pointer's grid row comes from the table's scroll geometry
//! (uniform rows), snapped to the nearest legal gap, mirrored to the
//! delegate for the drop line; near the body's top or bottom edge a tick
//! scrolls the table and re-reads the gap under the still pointer. A drop
//! applies the plan's edits as one undo entry; `escape` cancels; a
//! release anywhere else does nothing. Every install re-prepares a live
//! plan from the movers' ids, so a rebuild mid-drag never drops onto a
//! stale index.

use super::*;
use crate::core::reorder::{DropPlan, Placements, Stop};
use crate::core::select::{OFF_END, OFF_GROUP_END, top_most};
use crate::delegate::RowDrag;
use gpui::{Pixels, Point, point, px};

/// The siblings a move may land beside, boxed: shown, and under a value
/// grouping painted in the movers' own group.
pub(crate) type Lands<'a> = Box<dyn Fn(usize) -> bool + 'a>;

/// How often the edge scroll steps while the pointer rests in the band.
const AUTO_SCROLL_TICK: Duration = Duration::from_millis(16);

/// A grip drag from press to drop.
pub(crate) struct RowDragState {
    /// The grabbed row's line and group path: the cursor lands on it when
    /// no selection holds it.
    grabbed: At,
    /// The movers by id, re-resolved on every install.
    movers: Vec<LineId>,
    pub(crate) plan: Result<DropPlan, &'static str>,
    pub(crate) gap: Option<usize>,
    pointer: Option<Point<Pixels>>,
    /// Whether the live selection ends when the drag starts: the grip's
    /// row is outside a `V` selection, or a `v` block is live. Either
    /// selection spans painted rows, and a row moved in or out of them
    /// would widen or shift it onto lines the trader never picked.
    ends_selection: bool,
    /// Whether the edge-scroll tick is running.
    scrolling: bool,
    _scroll: Option<Task<()>>,
}

impl PricerTile {
    /// What a move of `movers` (top-most rows sharing a parent) steps by,
    /// and its refusal at the end. With no value grouping: every shown
    /// sibling, to the end of the roots or the package ([`OFF_END`]).
    /// Under one: only siblings painted in the movers' own group node, so
    /// the step hops other groups' lines and the group's painted order
    /// changes exactly as asked; roots stop at the group's end
    /// ([`OFF_GROUP_END`]), legs at their package's ([`OFF_END`]). Movers
    /// painted under more than one group refuse.
    pub(crate) fn move_lands(
        &self,
        movers: &[usize],
    ) -> Result<(Lands<'_>, &'static str), &'static str> {
        let shown = |r: usize| self.visibility.is_shown(r);
        if !self.grouped() {
            return Ok((Box::new(shown), OFF_END));
        }
        let same = Placements::of(&self.rollup).same_node(movers)?;
        let roots = movers
            .first()
            .is_some_and(|&r| self.sheet.parent(r).is_none());
        let edge = if roots { OFF_GROUP_END } else { OFF_END };
        Ok((Box::new(move |r| shown(r) && same(r)), edge))
    }

    /// Per grid row, whether it paints a grip: a line, package or leg the
    /// keys could move — never a grouping row, never under a sort (sheet
    /// order is not the painted order), never a package the grouping
    /// splits or the scope partly hides, nor a leg of a split package
    /// (each refuses the keys too, so a grip there would only promise a
    /// refusal). A selection's own refusals are the press's to report.
    pub(crate) fn grip_rows(&self) -> Vec<bool> {
        if self.sort.is_some() || self.loading {
            return Vec::new();
        }
        let split: std::collections::HashSet<usize> = self
            .rollup
            .nodes
            .iter()
            .filter_map(|n| match n.kind {
                NodeKind::Package {
                    row, split: true, ..
                } => Some(row),
                _ => None,
            })
            .collect();
        (0..self.model.len())
            .map(|g| {
                let Some(row) = self.model.sheet_row(g) else {
                    return false;
                };
                let package = self.sheet.parent(row).unwrap_or(row);
                let partial =
                    self.sheet.is_package(row) && self.visibility.is_partial(&self.sheet, row);
                !split.contains(&package) && !partial
            })
            .collect()
    }

    /// The grid rows `row` paints in group `within` (`None`: ungrouped,
    /// where it paints once): its own row and, open, a package's legs.
    fn painted_span(&self, row: usize, within: Option<&Path>) -> Option<std::ops::Range<usize>> {
        let id = self.sheet.id(row);
        let start = match within {
            Some(path) => self.exact_row(id, path)?,
            None => self.model.grid_row_of(id)?,
        };
        let legs = (start + 1..self.model.len())
            .take_while(|&g| self.model.parent(g) == Some(start))
            .count();
        Some(start..start + 1 + legs)
    }

    /// Where a drag of `movers` may drop: the movers and every sibling
    /// they may land beside, each with the grid rows it paints.
    fn drop_plan(&self, movers: &[usize]) -> Result<DropPlan, &'static str> {
        let Some(&first) = movers.first() else {
            return Err("no row");
        };
        let parent = self.sheet.parent(first);
        if movers.iter().any(|&r| self.sheet.parent(r) != parent) {
            return Err("can't move: selection spans packages");
        }
        let (lands, _) = self.move_lands(movers)?;
        let within = self
            .grouped()
            .then(|| Placements::of(&self.rollup).group(first).cloned())
            .flatten();
        let stops = self
            .sheet
            .siblings(first)
            .into_iter()
            .filter(|&r| movers.contains(&r) || lands(r))
            .filter_map(|row| {
                Some(Stop {
                    row,
                    span: self.painted_span(row, within.as_ref())?,
                })
            })
            .collect();
        Ok(DropPlan {
            movers: movers.to_vec(),
            stops,
        })
    }

    /// The movers' current sheet rows, or why a drag of them cannot drop.
    fn resolve_movers(&self, ids: &[LineId]) -> Result<Vec<usize>, &'static str> {
        ids.iter()
            .map(|&id| self.sheet.index_of(id).ok_or("no row"))
            .collect()
    }

    /// A grip pressed on grid row `g`: the row moves alone, or with the
    /// live `V` selection holding it — refused as a whole, as the keys
    /// refuse it, with the footer saying why. Nothing else changes: no
    /// cursor move, no selection, no editor.
    pub(crate) fn grip_pressed(&mut self, g: usize, cx: &mut Context<Self>) {
        self.row_drag = None;
        let (Some(grabbed), Some(row)) = (self.at_of_row(g), self.model.sheet_row(g)) else {
            self.mirror_drop_gap(cx);
            return;
        };
        let selected = self
            .resolved
            .as_ref()
            .is_some_and(|r| r.kind == SelectKind::Rows && r.contains_row(g));
        let movers = if selected {
            match self.partly_hidden_refusal("move_down", 1, true) {
                Some(why) => Err(why),
                None => Ok(top_most(&self.sheet, &self.selected_sheet_rows())),
            }
        } else {
            Ok(vec![row])
        };
        let ends_selection = self.selection.is_some() && !selected;
        let movers = movers.and_then(|m| self.move_refusal(&m).map_or(Ok(m), Err));
        let ids = match &movers {
            Ok(m) => m.iter().map(|&r| self.sheet.id(r)).collect(),
            Err(_) => Vec::new(),
        };
        let plan = movers.and_then(|m| self.drop_plan(&m));
        if let Err(why) = plan {
            self.footer = Some(why.into());
            self.rebuild_chrome();
            cx.notify();
        }
        self.row_drag = Some(RowDragState {
            grabbed,
            movers: ids,
            plan,
            gap: None,
            pointer: None,
            ends_selection,
            scrolling: false,
            _scroll: None,
        });
        self.mirror_drop_gap(cx);
    }

    /// Why `movers` may not move now, as the keys would refuse them: a
    /// sort ([`MOVE_SORTED`]), or a mover that is read-only (a split or
    /// partly hidden package) or a leg of a split package ([`SPLIT`]).
    fn move_refusal(&self, movers: &[usize]) -> Option<&'static str> {
        if self.sort.is_some() {
            return Some(MOVE_SORTED);
        }
        movers.iter().find_map(|&r| {
            self.read_only(r)
                .or_else(|| self.split_leg_refusal(r, true))
        })
    }

    /// After an install: a live drag's plan re-prepared against the new
    /// index (its gap re-read under the pointer), so a rebuild mid-drag
    /// never drops onto rows that moved — and refused, so no line shows
    /// and a release moves nothing, when the rebuild made the move one the
    /// keys refuse (a sort turned on, a package split or partly hidden).
    pub(crate) fn refresh_row_drag(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.row_drag.as_ref() else {
            return;
        };
        if d.plan.is_err() {
            return;
        }
        let ids = d.movers.clone();
        let plan = self.resolve_movers(&ids).and_then(|m| {
            self.move_refusal(&m)
                .map_or_else(|| self.drop_plan(&m), Err)
        });
        // A drag the rebuild refused says why, as a refused press does.
        if let Err(why) = plan {
            self.footer = Some(why.into());
            self.rebuild_chrome();
            cx.notify();
        }
        if let Some(d) = self.row_drag.as_mut() {
            d.plan = plan;
        }
        self.update_drop_gap(cx);
    }

    /// The table's body moved a drag: ours (this table's) is followed;
    /// any other is not this tile's to follow.
    pub(crate) fn row_drag_moved(
        &mut self,
        drag: &RowDrag,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if drag.table != self.table.entity_id() {
            return;
        }
        let Some(d) = self.row_drag.as_mut() else {
            return;
        };
        d.pointer = Some(at);
        // The first move is the drag's start: a selection the drag would
        // reshape ends here, not at the press (a grip click keeps it) —
        // and only when the drag can move rows: a refused one keeps the
        // selection and its refusal footer.
        let moves = d.plan.is_ok();
        if std::mem::take(&mut d.ends_selection) && moves && self.selection.is_some() {
            self.clear_selection();
            self.footer = Some(ROW_MOVED_SELECTION.into());
            self.sync_cursor(cx);
            self.rebuild_chrome();
            cx.notify();
        }
        self.update_drop_gap(cx);
        self.auto_scroll(cx);
    }

    /// The legal gap under the pointer, from the table's own geometry:
    /// rows are uniform, so the grid row is the pointer's offset into the
    /// scrolled body over the row height; the empty body below the last
    /// row is that row's lower half (the gap after it). `None` off the
    /// body, outside the plan's rows, or at a gap that moves nothing.
    fn gap_at_pointer(&self, cx: &App) -> Option<usize> {
        let d = self.row_drag.as_ref()?;
        let at = d.pointer?;
        let plan = d.plan.as_ref().ok()?;
        let (bounds, offset) = self.body_geometry(cx);
        if !bounds.contains(&at) {
            return None;
        }
        let rel = (at.y - bounds.top() - offset.y) / crate::delegate::TABLE_SIZE.table_row_height();
        if rel < 0. {
            return None;
        }
        let row = rel.floor() as usize;
        let len = self.model.len();
        if row >= len {
            return plan.target(&self.sheet, len.checked_sub(1)?, true);
        }
        plan.target(&self.sheet, row, rel - rel.floor() >= 0.5)
    }

    /// The table body's viewport and its scroll offset.
    fn body_geometry(&self, cx: &App) -> (gpui::Bounds<Pixels>, Point<Pixels>) {
        let t = self.table.read(cx);
        let h = t.vertical_scroll_handle.0.borrow();
        (h.base_handle.bounds(), h.base_handle.offset())
    }

    fn update_drop_gap(&mut self, cx: &mut Context<Self>) {
        let gap = self.gap_at_pointer(cx);
        let Some(d) = self.row_drag.as_mut() else {
            return;
        };
        if d.gap != gap {
            d.gap = gap;
            self.mirror_drop_gap(cx);
        }
    }

    /// The delegate's copy of the drop gap, repainted.
    fn mirror_drop_gap(&mut self, cx: &mut Context<Self>) {
        let gap = self.row_drag.as_ref().and_then(|d| d.gap);
        self.table.update(cx, |t, cx| {
            if t.delegate().drop_gap != gap {
                t.delegate_mut().drop_gap = gap;
                cx.notify();
            }
        });
    }

    /// Pixels the edge band scrolls by per tick: negative near the top,
    /// positive near the bottom, from a tenth of a row at the band's inner
    /// edge to four tenths at (or past) the body's edge — the band is one
    /// row tall; zero elsewhere or off the body's width.
    fn scroll_step(&self, cx: &App) -> f32 {
        let Some(at) = self.row_drag.as_ref().and_then(|d| d.pointer) else {
            return 0.;
        };
        let (bounds, _) = self.body_geometry(cx);
        if at.x < bounds.left() || at.x >= bounds.right() {
            return 0.;
        }
        let band = crate::delegate::TABLE_SIZE.table_row_height();
        let depth = if at.y < bounds.top() + band {
            -((bounds.top() + band - at.y) / band)
        } else if at.y > bounds.bottom() - band {
            (at.y - (bounds.bottom() - band)) / band
        } else {
            return 0.;
        };
        depth.signum() * f32::from(band) * (0.1 + 0.3 * depth.abs().min(1.))
    }

    /// Start the edge-scroll tick when the pointer enters the band.
    fn auto_scroll(&mut self, cx: &mut Context<Self>) {
        if self.scroll_step(cx) == 0. {
            return;
        }
        let Some(d) = self.row_drag.as_mut() else {
            return;
        };
        if d.scrolling {
            return;
        }
        d.scrolling = true;
        d._scroll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTO_SCROLL_TICK).await;
                let go = this
                    .update(cx, |t, cx| t.auto_scroll_tick(cx))
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        }));
    }

    /// One edge-scroll step: the table scrolls, and the gap under the
    /// still pointer is read again. Stops when the drag ended, the
    /// pointer left the band, or the table can scroll no further.
    fn auto_scroll_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !cx.has_active_drag() {
            self.row_drag = None;
            self.mirror_drop_gap(cx);
            return false;
        }
        let step = self.scroll_step(cx);
        let moved = step != 0. && {
            let t = self.table.read(cx);
            let h = t.vertical_scroll_handle.0.borrow();
            let base = &h.base_handle;
            let offset = base.offset();
            let max = base.max_offset();
            let y = (offset.y - px(step)).clamp(-max.y, px(0.));
            base.set_offset(point(offset.x, y));
            y != offset.y
        };
        if !moved {
            if let Some(d) = self.row_drag.as_mut() {
                d.scrolling = false;
            }
            return false;
        }
        self.table.update(cx, |_, cx| cx.notify());
        self.update_drop_gap(cx);
        true
    }

    /// The drag dropped on the body: the plan's edits at the painted gap,
    /// as one undo entry. A gap that is not legal, or leaves the order as
    /// it is, changes nothing. An open editor or entry bar is cancelled
    /// first, as any pointer gesture cancels it. The cursor lands on the
    /// grabbed line unless a selection holds it (moving the cursor would
    /// reshape the selection).
    pub(crate) fn row_dropped(
        &mut self,
        drag: &RowDrag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if drag.table != self.table.entity_id() {
            return;
        }
        let Some(d) = self.row_drag.take() else {
            return;
        };
        self.mirror_drop_gap(cx);
        let (Ok(plan), Some(gap)) = (&d.plan, d.gap) else {
            return;
        };
        let Some(mut edits) = plan.edits(&self.sheet, gap) else {
            return;
        };
        self.close_entry(window, cx);
        self.close_editor(window, cx);
        self.footer = None;
        let applied = if edits.len() == 1 {
            self.apply_edit(edits.remove(0), cx)
        } else {
            self.apply_edits(edits, cx)
        };
        match applied {
            Err(e) => self.footer = Some(e.to_string().into()),
            Ok(()) if self.selection.is_none() => {
                self.cursor.at = Some(d.grabbed);
                self.sync_cursor(cx);
            }
            Ok(()) => {}
        }
        self.rebuild_chrome();
        cx.notify();
    }

    /// `escape` during a live grip drag: the drag ends where it is, and
    /// nothing moves. `false` when no drag of ours is live (a stale state
    /// from a release elsewhere is cleared on the way).
    pub(crate) fn cancel_row_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.row_drag.take().is_none() {
            return false;
        }
        self.mirror_drop_gap(cx);
        cx.stop_active_drag(window)
    }

    /// A release that drops nothing (outside the body, or with no drag
    /// started: a grip click) ends the drag state here.
    pub(crate) fn row_drag_released_outside(&mut self, cx: &mut Context<Self>) {
        if self.row_drag.take().is_some() {
            self.mirror_drop_gap(cx);
        }
    }
}
