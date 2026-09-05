//! The `TableDelegate` adapter (Phase 3 spec §6.6). Owns everything the
//! table renders — snapshot, plan, expansion, the flattened and shown
//! row lists, cursor, mode, sort, the format cache — so every
//! `render_td` is a lookup. The pure core does the work; this file only
//! sequences it and paints.

use crate::core::cache::{FormatCache, cell};
use crate::core::cursor::{Cursor, Mode, restore_by_path, selection};
use crate::core::expansion::{Expansion, Path, depth_bound, path_of};
use crate::core::flatten::{SortSpec, flatten};
use crate::core::format::Sign;
use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::attribution::Attribution;
use geode_core::snapshot::Snapshot;
use geode_core::view::{Colour, ViewSpec};
use geode_shell::fonts;
use gpui::prelude::*;
use gpui::{App, Context, Div, IntoElement, SharedString, Stateful, TextAlign, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::table::{Column, ColumnSort, TableDelegate, TableState};
use std::ops::Range;
use std::sync::Arc;

const INDENT: f32 = 14.0;
const DETERMINED_MARK: &str = "†";

pub struct BlotterDelegate {
    pub snapshot: Option<Arc<Snapshot>>,
    pub plan: Option<ColumnPlan>,
    pub expansion: Expansion,
    /// The full flatten.
    pub visible: Vec<u32>,
    /// What the table shows: `visible`, or its fzf-narrowed subset.
    pub shown: Vec<u32>,
    pub cursor: Cursor,
    pub mode: Mode,
    pub sort: Option<SortSpec>,
    pub cache: FormatCache,
    pub narrowed: Option<Vec<usize>>,
    pub unplaced: usize,
    /// Whether any painted cell carried the dagger, for the footer.
    pub any_determined: bool,
    pub semi_joined: Vec<String>,
}

impl Default for BlotterDelegate {
    fn default() -> Self {
        Self::new()
    }
}

impl BlotterDelegate {
    pub fn new() -> Self {
        BlotterDelegate {
            snapshot: None,
            plan: None,
            expansion: Expansion::default(),
            visible: Vec::new(),
            shown: Vec::new(),
            cursor: Cursor::default(),
            mode: Mode::Normal,
            sort: None,
            cache: FormatCache::default(),
            narrowed: None,
            unplaced: 0,
            any_determined: false,
            semi_joined: Vec::new(),
        }
    }

    pub fn cursor_path(&self) -> Option<Path> {
        let snapshot = self.snapshot.as_ref()?;
        let plan = self.plan.as_ref()?;
        let row = *self.shown.get(self.cursor.row)? as usize;
        Some(path_of(snapshot, plan, row))
    }

    pub fn cursor_row_index(&self) -> Option<usize> {
        self.shown.get(self.cursor.row).map(|r| *r as usize)
    }

    pub fn apply_snapshot(
        &mut self,
        snapshot: Arc<Snapshot>,
        view: &ViewSpec,
        grouping: &[String],
    ) {
        let keep = self.cursor_path();
        let rebuild = match &self.plan {
            None => true,
            Some(p) => p.grouping != grouping || !p.same_columns(&snapshot),
        };
        if rebuild {
            self.plan = Some(ColumnPlan::build(view, grouping, &snapshot));
            self.sort = self
                .sort
                .filter(|s| s.column < self.plan.as_ref().unwrap().columns.len());
        }
        self.expansion.prune_to(grouping.len());
        self.unplaced = snapshot.tree().unplaced();
        self.semi_joined = self
            .plan
            .as_ref()
            .map(|p| {
                let mut v: Vec<String> = p
                    .columns
                    .iter()
                    .flat_map(|c| c.semi_joined.iter().cloned())
                    .collect();
                v.sort();
                v.dedup();
                v
            })
            .unwrap_or_default();
        self.snapshot = Some(snapshot);
        self.narrowed = None;
        self.reflatten_keeping(keep);
    }

    pub fn reflatten(&mut self) {
        let keep = self.cursor_path();
        self.reflatten_keeping(keep);
    }

    fn reflatten_keeping(&mut self, keep: Option<Path>) {
        let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) else {
            self.visible.clear();
            self.shown.clear();
            return;
        };
        flatten(
            snapshot,
            plan,
            &self.expansion,
            self.sort.as_ref(),
            &mut self.visible,
        );
        // Inlined `rebuild_shown`: it only touches `shown`/`narrowed`/
        // `visible`, disjoint fields from `snapshot`/`plan` above, but
        // going through a `&mut self` method here would conflict with
        // the still-live borrows those two variables hold below.
        self.shown.clear();
        match &self.narrowed {
            None => self.shown.extend_from_slice(&self.visible),
            Some(rows) => self.shown.extend(
                self.visible
                    .iter()
                    .copied()
                    .filter(|r| rows.contains(&(*r as usize))),
            ),
        }
        if let Some(path) = keep {
            self.cursor.row = restore_by_path(&self.shown, snapshot, plan, &path, self.cursor.row);
        }
        self.cursor.clamp(self.shown.len(), plan.columns.len());
        self.cache.invalidate();
    }

    /// `narrowed` names *rows* (raw snapshot row indices, the same
    /// domain as `visible`'s elements) rather than positions in any
    /// list — `BlotterTile::find` (Task 7) maps a `FindState` match
    /// position to a row via `shown[position]` before calling
    /// `set_narrowed`, so what lands here is already a row. `shown`
    /// becomes the subset of `visible` those rows name, in `visible`'s
    /// own order (never the caller's order), so a stale or duplicate
    /// row cannot reorder what's on screen.
    fn rebuild_shown(&mut self) {
        self.shown.clear();
        match &self.narrowed {
            None => self.shown.extend_from_slice(&self.visible),
            Some(rows) => self.shown.extend(
                self.visible
                    .iter()
                    .copied()
                    .filter(|r| rows.contains(&(*r as usize))),
            ),
        }
    }

    pub fn set_narrowed(&mut self, rows: Option<Vec<usize>>) {
        self.narrowed = rows;
        self.rebuild_shown();
        let cols = self.plan.as_ref().map_or(0, |p| p.columns.len());
        self.cursor.clamp(self.shown.len(), cols);
        self.cache.invalidate();
    }

    pub fn shown_texts(&self) -> Vec<String> {
        let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) else {
            return Vec::new();
        };
        self.shown
            .iter()
            .map(|&r| {
                plan.tree_text(snapshot, r as usize)
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    }

    pub fn depth_bound(&self, grouping_len: usize) -> usize {
        depth_bound(&self.expansion, grouping_len)
    }

    /// `zo`/`zc`/`za` on the cursor row. `Some(true)` opens, `Some(false)`
    /// closes (or closes the parent when the row is a leaf or closed,
    /// vim-style), `None` toggles. Returns whether the row is open after.
    pub fn expand_cursor(&mut self, open: Option<bool>) -> bool {
        let Some(path) = self.cursor_path() else {
            return false;
        };
        if path.is_empty() {
            return true; // the root is always open
        }
        let now_open = match open {
            Some(true) => {
                self.expansion.open(path.clone());
                true
            }
            Some(false) => {
                if self.expansion.is_open(&path) {
                    self.expansion.close(&path);
                } else if path.len() > 1 {
                    let parent = &path[..path.len() - 1];
                    self.expansion.close(parent);
                    // Move the cursor to the parent it just closed.
                    if let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) {
                        self.cursor.row =
                            restore_by_path(&self.shown, snapshot, plan, parent, self.cursor.row);
                    }
                }
                false
            }
            None => self.expansion.toggle(path.clone()),
        };
        self.reflatten();
        now_open
    }

    /// The cursor row is open but has no materialised children: the
    /// snapshot stopped at the depth bound and a requery is needed.
    pub fn cursor_needs_more_depth(&self, grouping_len: usize) -> bool {
        let (Some(snapshot), Some(path)) = (&self.snapshot, self.cursor_path()) else {
            return false;
        };
        let Some(row) = self.cursor_row_index() else {
            return false;
        };
        !path.is_empty()
            && path.len() < grouping_len
            && self.expansion.is_open(&path)
            && !snapshot.tree().has_children(row)
    }

    /// Fill the cache for a window of *shown* rows.
    pub fn refill_window(&mut self, window: Range<usize>) {
        let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) else {
            return;
        };
        let cols = plan.columns.len();
        let shown = &self.shown;
        let mut any_determined = false;
        self.cache.set_window(window, cols, |shown_row, col| {
            let row = *shown.get(shown_row)? as usize;
            let c = cell(snapshot, plan, row, col)?;
            if c.attribution == Attribution::DeterminedNonAdditive {
                any_determined = true;
            }
            Some(c)
        });
        self.any_determined = any_determined;
    }
}

impl TableDelegate for BlotterDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.plan.as_ref().map_or(0, |p| p.columns.len())
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.shown.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(c) = self.plan.as_ref().and_then(|p| p.columns.get(col_ix)) else {
            return Column::default();
        };
        let sort = match self.sort {
            Some(s) if s.column == col_ix && s.descending => Some(ColumnSort::Descending),
            Some(s) if s.column == col_ix => Some(ColumnSort::Ascending),
            _ => Some(ColumnSort::Default),
        };
        let mut label = c.label.clone();
        if !c.semi_joined.is_empty() {
            label.push_str(" ⋈");
        }
        Column {
            key: SharedString::from(c.name.clone()),
            name: SharedString::from(label),
            align: if c.kind == ColumnKind::Measure {
                TextAlign::Right
            } else {
                TextAlign::Left
            },
            sort: if c.kind == ColumnKind::Tree {
                None
            } else {
                sort
            },
            width: px(c.width),
            movable: c.kind != ColumnKind::Tree,
            ..Column::default()
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.sort = match sort {
            ColumnSort::Default => None,
            ColumnSort::Ascending => Some(SortSpec {
                column: col_ix,
                descending: false,
            }),
            ColumnSort::Descending => Some(SortSpec {
                column: col_ix,
                descending: true,
            }),
        };
        self.reflatten();
        cx.notify();
    }

    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if let Some(p) = self.plan.as_mut() {
            p.move_column(col_ix, to_ix);
        }
        self.cache.invalidate();
        cx.notify();
    }

    fn visible_rows_changed(
        &mut self,
        visible_range: Range<usize>,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        self.refill_window(visible_range);
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let range = selection(&self.mode, &self.cursor);
        let in_visual = matches!(self.mode, Mode::Visual { .. }) && range.contains(&row_ix);
        div()
            .id(("row", row_ix))
            .when(in_visual, |el| el.bg(cx.theme().selection.opacity(0.35)))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let is_cursor = self.cursor.row == row_ix && self.cursor.col == col_ix;
        let kind = self
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(col_ix))
            .map(|c| c.kind);
        let colour = self
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(col_ix))
            .map(|c| c.format.colour);
        let mut el = div()
            .size_full()
            .flex()
            .items_center()
            .px_1()
            .font_family(fonts::MONO)
            .when(kind == Some(ColumnKind::Measure), |el| el.justify_end())
            .when(is_cursor, |el| {
                el.border_1().border_color(theme.table_active_border)
            });

        // The tree column: indent and a disclosure glyph, then the text.
        if kind == Some(ColumnKind::Tree)
            && let (Some(snapshot), Some(&row)) = (&self.snapshot, self.shown.get(row_ix))
        {
            let tree = snapshot.tree();
            let row = row as usize;
            let depth = tree.depth(row);
            let could = depth < snapshot.grouping_len();
            let glyph = if !could {
                "·"
            } else if tree.has_children(row) {
                let open = self
                    .plan
                    .as_ref()
                    .is_some_and(|p| self.expansion.is_open(&path_of(snapshot, p, row)));
                if open { "▾" } else { "▸" }
            } else if self
                .plan
                .as_ref()
                .is_some_and(|p| self.expansion.is_open(&path_of(snapshot, p, row)))
            {
                "…" // open, not yet materialised: a requery is in flight
            } else {
                "▸"
            };
            el = el.pl(px(depth as f32 * INDENT)).child(
                div()
                    .w(px(14.))
                    .text_color(theme.muted_foreground)
                    .child(glyph),
            );
        }

        let Some(cell) = self.cache.get(row_ix, col_ix) else {
            return el; // blank: NULL, NonAttributable, or not yet cached
        };
        let text: SharedString = SharedString::from(Arc::clone(&cell.text));
        match cell.attribution {
            Attribution::NonAttributable => el, // never a number here
            Attribution::DeterminedNonAdditive => el
                .text_color(theme.muted_foreground)
                .child(text)
                .child(div().pl_1().child(DETERMINED_MARK)),
            Attribution::Additive => {
                let el = match (colour, cell.sign) {
                    (Some(Colour::Sign), Some(Sign::Negative)) => {
                        el.text_color(theme.chart_bearish)
                    }
                    (Some(Colour::Sign), Some(Sign::Positive)) => {
                        el.text_color(theme.chart_bullish)
                    }
                    _ => el,
                };
                el.child(text)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::ViewSpec;

    fn dim(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 4],
            scope_semantics: ScopeSemantics::Direct,
        }
    }
    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }
    /// Root; L1, L2; L1/SPX, L1/NDX. L2's children are not materialised
    /// (depth bound 2 would give them, this fixture stops at L1's).
    fn snapshot() -> Arc<Snapshot> {
        Arc::new(Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Dict(vec![None, s("L1"), s("L2"), s("L1"), s("L1")]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Dict(vec![None, None, None, s("SPX"), s("NDX")]),
                ),
                (dim("position_ref"), TestColumn::Str(vec![None; 5])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2, 2])),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0), Some(2.0), Some(3.0)]),
                ),
            ],
            3,
        ))
    }
    fn view() -> ViewSpec {
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\", \"position_ref\"]\n[[t.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.remove(0)
    }
    fn grouping() -> Vec<String> {
        vec!["lhu".into(), "underlying_ref".into(), "position_ref".into()]
    }

    #[test]
    fn applying_a_snapshot_builds_the_plan_flattens_and_keeps_the_cursor_node() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        assert_eq!(d.shown, vec![0, 1, 2]);
        assert_eq!(d.plan.as_ref().unwrap().columns.len(), 2);
        d.cursor.row = 2; // L2
        assert!(
            d.expand_cursor(None),
            "L2 opens (nothing materialised beneath, but the path is open)"
        );
        assert!(
            d.cursor_needs_more_depth(3),
            "L2's children are past the bound"
        );
        assert_eq!(d.depth_bound(3), 2);
        // A new snapshot with L2 first: the cursor follows L2.
        let reordered = Arc::new(Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Dict(vec![None, s("L2"), s("L1"), s("L2")]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Dict(vec![None, None, None, s("RUT")]),
                ),
                (dim("position_ref"), TestColumn::Str(vec![None; 4])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2])),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(4.0), Some(5.0), Some(4.0)]),
                ),
            ],
            3,
        ));
        d.apply_snapshot(reordered, &view(), &grouping());
        assert_eq!(d.shown, vec![0, 1, 3, 2], "L2 is open and now has a child");
        assert_eq!(d.cursor.row, 1, "still on L2");
        assert!(!d.cursor_needs_more_depth(3), "its child is here");
    }

    #[test]
    fn a_regroup_prunes_expansion_and_rebuilds_the_plan() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.cursor.row = 1;
        d.expand_cursor(Some(true));
        d.cursor.row = 2;
        assert_eq!(d.shown, vec![0, 1, 3, 4, 2]);
        let by_lhu = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1"), s("L2")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)]),
                ),
            ],
            1,
        ));
        d.apply_snapshot(by_lhu, &view(), &["lhu".to_string()]);
        assert_eq!(d.plan.as_ref().unwrap().grouping, vec!["lhu".to_string()]);
        assert_eq!(d.shown, vec![0, 1, 2]);
        assert_eq!(
            d.depth_bound(1),
            1,
            "an L1 path deeper than the grouping was pruned"
        );
    }

    #[test]
    fn narrowing_changes_what_is_shown_and_the_cache_window_follows_shown_rows() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.cursor.row = 1;
        d.expand_cursor(Some(true));
        d.set_narrowed(Some(vec![3, 4]));
        assert_eq!(d.shown, vec![3, 4]);
        assert_eq!(d.shown_texts(), vec!["SPX".to_string(), "NDX".to_string()]);
        d.refill_window(0..2);
        assert_eq!(
            d.cache.get(1, 0).map(|c| c.text.to_string()),
            Some("NDX".into())
        );
        assert_eq!(
            d.cache.get(1, 1).map(|c| c.text.to_string()),
            Some("3.00".into())
        );
        d.set_narrowed(None);
        assert_eq!(d.shown, vec![0, 1, 3, 4, 2]);
        assert!(
            d.cache.get(1, 0).is_none(),
            "invalidated with the narrowing"
        );
    }
}
