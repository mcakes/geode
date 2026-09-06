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
    /// The tree column's disclosure glyph for each *shown* row in
    /// `cache`'s current window, aligned index-for-index with it
    /// (`glyphs[i]` is `cache.window().start + i`) — resolved once per
    /// window fill in `refill_window`, never in `render_td`, so
    /// painting the tree column never calls `path_of` (an allocating
    /// ancestor walk) per cell. Empty until the first `refill_window`.
    glyphs: Vec<&'static str>,
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
            glyphs: Vec::new(),
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
            Some(positions) => self.shown.extend(
                positions
                    .iter()
                    .filter_map(|&i| self.visible.get(i).copied()),
            ),
        }
        if let Some(path) = keep {
            self.cursor.row = restore_by_path(&self.shown, snapshot, plan, &path, self.cursor.row);
        }
        self.cursor.clamp(self.shown.len(), plan.columns.len());
        self.invalidate_cells();
    }

    /// Invalidate the format cache and the cached tree glyphs together.
    /// `FormatCache::invalidate` clears its rows but leaves `start`
    /// unchanged, so `render_td`'s `glyphs` lookup (keyed off
    /// `cache.window().start`) would otherwise keep serving a stale
    /// glyph for a row whose text just went blank — exactly when the
    /// visible row *range* doesn't change across a regroup/sort/narrow
    /// (so `TableState` never calls `visible_rows_changed` to refill
    /// either of them). Every `self.cache.invalidate()` call site must
    /// go through this instead.
    fn invalidate_cells(&mut self) {
        self.cache.invalidate();
        self.glyphs.clear();
    }

    /// `narrowed` names *positions* into `visible` (the domain
    /// `FindState`'s fzf narrowing returns — positions into
    /// `shown_texts()`, which is `visible`'s texts when a `/` session
    /// begins with nothing narrowed yet), not row ids: `shown[k] =
    /// visible[narrowed[k]]`, in the caller's order. A position past
    /// the end of `visible` is dropped rather than panicking.
    fn rebuild_shown(&mut self) {
        self.shown.clear();
        match &self.narrowed {
            None => self.shown.extend_from_slice(&self.visible),
            Some(positions) => self.shown.extend(
                positions
                    .iter()
                    .filter_map(|&i| self.visible.get(i).copied()),
            ),
        }
    }

    pub fn set_narrowed(&mut self, rows: Option<Vec<usize>>) {
        self.narrowed = rows;
        self.rebuild_shown();
        let cols = self.plan.as_ref().map_or(0, |p| p.columns.len());
        self.cursor.clamp(self.shown.len(), cols);
        self.invalidate_cells();
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
        self.cache
            .set_window(window.clone(), cols, |shown_row, col| {
                let row = *shown.get(shown_row)? as usize;
                cell(snapshot, plan, row, col)
            });
        // Scanned over the *whole* current window, not just the rows
        // this call's `fill` closure actually ran for: `set_window`
        // keeps overlapping rows without re-invoking `fill`, so a row
        // that entered on an earlier call and stayed cached must still
        // be able to hold the flag up after a scroll that brings in
        // nothing but non-determined rows.
        let determined_window = self.cache.window();
        self.any_determined = determined_window.into_iter().any(|row: usize| {
            (0..cols).any(|col| {
                self.cache
                    .get(row, col)
                    .is_some_and(|c| c.attribution == Attribution::DeterminedNonAdditive)
            })
        });
        self.glyphs = window
            .map(|shown_row| tree_glyph(snapshot, plan, &self.expansion, shown, shown_row))
            .collect();
    }

    /// The exact lookup `render_td` performs, minus its "no glyph
    /// cached" fallback — so a test can tell "a real glyph is cached"
    /// apart from "nothing is cached" (both of which `render_td` paints
    /// as `"·"`). Test-only: production code goes through `render_td`.
    #[cfg(test)]
    fn glyph_at(&self, row_ix: usize) -> Option<&'static str> {
        row_ix
            .checked_sub(self.cache.window().start)
            .and_then(|i| self.glyphs.get(i))
            .copied()
    }
}

/// The tree column's disclosure glyph for one *shown* row (§6.5's blank/
/// dagger/⋈ markers are separate; this is only the `▸`/`▾`/`…`/`·`
/// expand-state glyph). Resolved once per window fill
/// (`BlotterDelegate::refill_window`) rather than per paint, since
/// `path_of` walks and allocates one `Option<String>` per ancestor —
/// exactly the per-frame heap churn `render_td` must never do.
fn tree_glyph(
    snapshot: &Snapshot,
    plan: &ColumnPlan,
    expansion: &Expansion,
    shown: &[u32],
    shown_row: usize,
) -> &'static str {
    let Some(&row) = shown.get(shown_row) else {
        return "·";
    };
    let row = row as usize;
    let tree = snapshot.tree();
    let depth = tree.depth(row);
    if depth >= snapshot.grouping_len() {
        return "·";
    }
    if tree.has_children(row) {
        if expansion.is_open(&path_of(snapshot, plan, row)) {
            "▾"
        } else {
            "▸"
        }
    } else if expansion.is_open(&path_of(snapshot, plan, row)) {
        "…" // open, not yet materialised: a requery is in flight
    } else {
        "▸"
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
        // `TableState::move_column` (gpui-component) calls this directly
        // and never fires `visible_rows_changed`, so a bare
        // `self.cache.invalidate()` here would leave the window's cells
        // blank until the next scroll. Capture the window that was
        // already on screen *before* invalidating (`invalidate` clears
        // `rows` but keeps `start`, so `window()` after it is an empty
        // range), then refill it immediately with the reordered plan.
        // The glyphs are untouched by a column reorder — the tree
        // column never moves (`ColumnPlan::move_column` refuses `from ==
        // 0 || to == 0`) — so this goes through `self.cache.invalidate()`
        // + `refill_window`, not `invalidate_cells`, deliberately.
        let w = self.cache.window();
        self.cache.invalidate();
        self.refill_window(w);
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
        // The glyph itself is never computed here — `path_of` (which
        // `tree_glyph` calls) allocates one `Option<String>` per
        // ancestor, and `refill_window` has already resolved it for
        // every row in `cache`'s current window.
        if kind == Some(ColumnKind::Tree)
            && let (Some(snapshot), Some(&row)) = (&self.snapshot, self.shown.get(row_ix))
        {
            let depth = snapshot.tree().depth(row as usize);
            let glyph = row_ix
                .checked_sub(self.cache.window().start)
                .and_then(|i| self.glyphs.get(i))
                .copied()
                .unwrap_or("·");
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
        // visible is now [0, 1, 3, 4, 2]; SPX and NDX sit at *positions*
        // 2 and 3 (their row ids, 3 and 4, are not their positions).
        d.set_narrowed(Some(vec![2, 3]));
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

    #[test]
    fn narrowing_uses_positions_into_visible_not_row_ids_even_when_they_differ() {
        // Regression for a reviewer-caught defect: `set_narrowed`'s
        // argument is *positions* into `visible` (what `FindState`'s
        // fzf narrowing returns — positions into `shown_texts()`, which
        // is `visible`'s texts when narrowing begins), not raw row ids.
        // Pick a fixture where a row's position in `visible` differs
        // from its row id, so a row-id-based (wrong) implementation and
        // a position-based (right) one disagree.
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.cursor.row = 1; // L1
        d.expand_cursor(Some(true));
        // visible = [0, 1, 3, 4, 2]: L2 (row id 2) sits at position 4,
        // not position 2 — id and position disagree for this row.
        assert_eq!(d.visible, vec![0, 1, 3, 4, 2]);
        d.set_narrowed(Some(vec![4]));
        assert_eq!(
            d.shown,
            vec![2],
            "position 4 in `visible` is row id 2 (L2); a row-id filter \
             would have kept nothing, since 4 is SPX's row id but SPX \
             sits at position 3, not 4"
        );
        assert_eq!(d.shown_texts(), vec!["L2".to_string()]);
    }

    #[test]
    fn a_regroup_to_a_shallower_grouping_prunes_a_path_deeper_than_it_can_reach() {
        // `depth_bound` alone can't distinguish a pruned from an
        // unpruned expansion when another open path already sits at
        // exactly the new grouping length (its `.min(grouping_len)` cap
        // masks the difference) — this fixture keeps the deep path the
        // *only* thing open, so the mutation this guards against would
        // otherwise slip past `a_regroup_prunes_expansion_and_rebuilds_the_plan`.
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        // A length-3 path, opened directly (no navigation needed): one
        // level deeper than `snapshot()`'s own materialised rows go, but
        // `Expansion` tracks paths, not rows, so this is legal on its own.
        d.expansion.open(vec![
            Some("L1".to_string()),
            Some("SPX".to_string()),
            Some("POS1".to_string()),
        ]);
        let by_two = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1"), s("L2")])),
                (
                    dim("underlying_ref"),
                    TestColumn::Dict(vec![None, None, None]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)]),
                ),
            ],
            2,
        ));
        d.apply_snapshot(
            by_two,
            &view(),
            &["lhu".to_string(), "underlying_ref".to_string()],
        );
        assert_eq!(
            d.depth_bound(2),
            1,
            "the length-3 path was pruned; nothing open reaches depth 2, \
             so the bound falls back to the first level"
        );
    }

    #[test]
    fn apply_snapshot_invalidates_the_cache_even_without_narrowing() {
        // The cache-invalidation entry the reviewer asked for: distinct
        // from `narrowing_changes_what_is_shown_and_the_cache_window_follows_shown_rows`,
        // whose final assertion goes through `set_narrowed`'s own
        // `invalidate()` call, not `apply_snapshot`'s.
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.refill_window(0..3);
        assert!(
            d.cache.get(0, 1).is_some(),
            "the grand total's delta01 is cached"
        );
        assert_eq!(d.narrowed, None, "narrowing plays no part in this");
        d.apply_snapshot(snapshot(), &view(), &grouping());
        assert!(
            d.cache.get(0, 1).is_none(),
            "apply_snapshot invalidates the cache on its own"
        );
    }

    #[test]
    fn a_regroup_that_keeps_the_window_clears_the_cached_glyphs() {
        // Reviewer-caught defect: `FormatCache::invalidate` clears the
        // cache's rows but leaves `start` unchanged, and neither
        // `reflatten_keeping` nor `set_narrowed` touched `glyphs` when
        // they called it — so a regroup/sort/narrow whose visible row
        // range doesn't change (gpui-component's `TableState` only
        // calls `visible_rows_changed` when the numeric range differs)
        // left `render_td` painting the pre-invalidation glyph for a
        // row whose cell text correctly went blank.
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        // shown = [0, 1, 2]; row 1 (L1) has children (SPX, NDX) and is
        // not open, so its disclosure glyph is the real "▸", not the
        // "no glyph cached" fallback.
        d.refill_window(0..3);
        assert_eq!(
            d.glyph_at(1),
            Some("▸"),
            "L1 has children, closed: a real disclosure glyph is cached"
        );
        // Re-apply the same snapshot/grouping without calling
        // `refill_window` again: `shown`'s numeric range is unchanged,
        // matching the scenario where `TableState` would not re-fire
        // `visible_rows_changed` — but the cache itself was invalidated
        // and the glyph must not survive stale.
        d.apply_snapshot(snapshot(), &view(), &grouping());
        assert_eq!(d.shown, vec![0, 1, 2], "the visible row range is unchanged");
        assert_eq!(
            d.glyph_at(1),
            None,
            "the cache was invalidated; the glyph must not paint stale"
        );
    }

    #[test]
    fn any_determined_reflects_the_whole_window_not_just_newly_entered_rows() {
        // A determined cell that stays cached across a scroll must keep
        // `any_determined` true even when nothing newly entered is
        // itself determined — `FormatCache::set_window` keeps
        // overlapping rows without re-invoking the fill closure, so a
        // delta-only computation (only rows the closure actually ran
        // for) would wrongly drop the flag.
        //
        // delta01 is DeterminedNonAdditive at depth 2 (rows 2-5, the
        // A/B/C/D leaves) and Additive above it (rows 0-1); the `dim`
        // helper only ever gives Additive, so this builds its own
        // per-depth attribution directly.
        let determined = |depth: usize| {
            if depth == 2 {
                Attribution::DeterminedNonAdditive
            } else {
                Attribution::Additive
            }
        };
        let view_text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[t.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", view_text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let mut d = BlotterDelegate::new();
        let snap = Arc::new(Snapshot::for_tests(
            vec![
                (
                    ColumnMeta {
                        name: "lhu".into(),
                        attribution_by_depth: vec![Attribution::Additive; 4],
                        scope_semantics: ScopeSemantics::Direct,
                    },
                    TestColumn::Dict(vec![None, s("L1"), s("SPX"), s("SPX"), s("SPX"), s("SPX")]),
                ),
                (
                    ColumnMeta {
                        name: "underlying_ref".into(),
                        attribution_by_depth: vec![Attribution::Additive; 4],
                        scope_semantics: ScopeSemantics::Direct,
                    },
                    TestColumn::Dict(vec![None, None, s("A"), s("B"), s("C"), s("D")]),
                ),
                (
                    ColumnMeta {
                        name: "row_depth".into(),
                        attribution_by_depth: vec![Attribution::Additive; 4],
                        scope_semantics: ScopeSemantics::Direct,
                    },
                    TestColumn::I32(vec![0, 1, 2, 2, 2, 2]),
                ),
                (
                    ColumnMeta {
                        name: "delta01".into(),
                        attribution_by_depth: (0..4).map(determined).collect(),
                        scope_semantics: ScopeSemantics::Direct,
                    },
                    TestColumn::F64(vec![
                        Some(9.0),
                        Some(9.0),
                        Some(1.0),
                        Some(2.0),
                        Some(3.0),
                        Some(4.0),
                    ]),
                ),
            ],
            2,
        ));
        d.apply_snapshot(
            snap,
            &view,
            &["lhu".to_string(), "underlying_ref".to_string()],
        );
        d.cursor.row = 1; // L1
        d.expand_cursor(Some(true));
        // shown = [root(0), L1(1), A(2), B(3), C(4), D(5)]; A..D are
        // DeterminedNonAdditive (depth 2).
        assert_eq!(d.shown, vec![0, 1, 2, 3, 4, 5]);
        d.refill_window(2..6); // A, B, C, D: all determined
        assert!(d.any_determined);
        d.refill_window(1..5); // L1 enters (not determined); A, B, C stay cached
        assert!(
            d.any_determined,
            "A/B/C stayed cached and determined even though only L1 \
             (not determined) newly entered"
        );
    }

    /// Ruled-in fix (Task 6 review, applied here): gpui-component's
    /// `TableState::move_column` calls `self.delegate.move_column(..)`
    /// directly and never fires `visible_rows_changed`, so a bare
    /// `self.cache.invalidate()` in `move_column` left the window blank
    /// — no `refill_window` call was coming until the next scroll. The
    /// glyphs stay valid across a column reorder (the tree column never
    /// moves — `ColumnPlan::move_column` refuses `from == 0 || to ==
    /// 0`), so only the cell cache needs to be repainted, immediately,
    /// for the window that was already on screen.
    #[gpui::test]
    fn move_column_refills_the_window_immediately(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let view_text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
             [[t.columns]]\nname = \"delta01\"\n[[t.columns]]\nname = \"gamma01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", view_text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let snap = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1])),
                (dim("delta01"), TestColumn::F64(vec![Some(9.0), Some(5.0)])),
                (dim("gamma01"), TestColumn::F64(vec![Some(1.0), Some(2.0)])),
            ],
            1,
        ));

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    cx.new(|cx| TableState::new(BlotterDelegate::new(), window, cx))
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let table = window.root(&mut vcx).unwrap();

        table.update(&mut vcx, |t, _| {
            let d = t.delegate_mut();
            d.apply_snapshot(snap, &view, &["lhu".to_string()]);
            d.refill_window(0..2);
        });

        // Assert inside the same `update_in` call that performs the move,
        // immediately after it returns and before anything else runs —
        // deliberately not a separate `read_with` afterwards, since the
        // test window's own effect-flushing can trigger a real layout
        // pass between two top-level calls, and a first-ever real draw
        // would establish `TableState`'s own visible range and refill
        // the cache on its own, independent of whether `move_column`
        // itself refills — which would make the test pass regardless of
        // the fix. Checking synchronously inside the same call isolates
        // exactly what `move_column` itself did.
        table.update_in(&mut vcx, |t, window, cx| {
            let d = t.delegate_mut();
            assert_eq!(
                d.cache.get(0, 2).map(|c| c.text.to_string()),
                Some("1.00".into()),
                "gamma01 (col 2) is cached before the move"
            );

            // Swap delta01 (col 1) and gamma01 (col 2).
            d.move_column(1, 2, window, cx);

            assert_eq!(
                d.cache.window(),
                0..2,
                "the window is refilled immediately, not left empty until \
                 the next scroll"
            );
            assert_eq!(
                d.cache.get(0, 1).map(|c| c.text.to_string()),
                Some("1.00".into()),
                "gamma01 moved into column 1 and repainted immediately"
            );
            assert_eq!(
                d.cache.get(0, 2).map(|c| c.text.to_string()),
                Some("9.00".into()),
                "delta01 moved into column 2 and repainted immediately"
            );
        });
    }
}
