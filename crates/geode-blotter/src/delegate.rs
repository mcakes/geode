//! The `TableDelegate` adapter (Phase 3 spec §6.6). Owns everything the
//! table renders — snapshot, plan, expansion, the flattened and shown
//! row lists, cursor, mode, sort, the format cache — so every
//! `render_td` is a lookup. The pure core does the work; this file only
//! sequences it and paints.

use crate::colour_cache::ColourCache;
use crate::core::cache::{FormatCache, cell};
use crate::core::cursor::{Cursor, Mode, restore_by_path, selection};
use crate::core::expansion::{Expansion, Path, depth_bound, path_of};
use crate::core::flatten::{SortOrder, SortSpec, flatten};
use crate::core::format::Sign;
use crate::core::plan::{ColumnKind, ColumnPlan};
use geode_core::attribution::Attribution;
use geode_core::colour::{Anchors, NamedColours, Tokens};
use geode_core::snapshot::Snapshot;
use geode_core::view::{Colour, ViewSpec};
use geode_shell::fonts;
use geode_shell::linenumbers::{LineNumbers, gutter_digits, gutter_number};
use geode_shell::shell::colours::{anchors_from_theme, theme_signature, tokens_from_theme};
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, Context, Div, EventEmitter, Hsla, IntoElement, SharedString, Stateful,
    TextAlign, Window, div, px,
};
use gpui_component::table::{Column, ColumnFixed, ColumnSort, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Theme};
use std::ops::Range;
use std::sync::Arc;

/// A single left click landed on the tree column's disclosure glyph of
/// the *shown* row it carries. The table has already selected that row
/// (so `TableEvent::SelectRow` has moved the cursor there); the tile
/// answers by toggling it, exactly as `space` does. Emitted from
/// `render_td`'s glyph listener, which has no path to the tile except an
/// event on the `TableState` it renders into — `TableEvent` is
/// gpui-component's own closed enum, so the blotter emits its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChevronClicked(pub usize);

impl EventEmitter<ChevronClicked> for TableState<BlotterDelegate> {}

/// A column's `colour` setting reduced to what a paint site needs to
/// branch on — see `BlotterDelegate::colour_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColourKind {
    Plain,
    Sign,
    Named,
}

const INDENT: f32 = 14.0;
const DETERMINED_MARK: &str = "†";
/// One digit cell of the line-number gutter, in px: the mono face's
/// advance at the default UI size, rounded up so a gutter never wraps.
/// Same absolute-px school as `INDENT` above.
const GUTTER_DIGIT_PX: f32 = 8.0;
/// The gap between the gutter's last digit and the tree indent.
const GUTTER_GAP_PX: f32 = 6.0;

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
    /// The last window `refill_window` was actually asked to fill,
    /// *not* `cache.window()`. `TableState` (`update_visible_range_if_need`,
    /// pinned release, `gpui-component-0.6.2/src/table/state.rs`) only
    /// records a new visible range when it has more than one row — a
    /// filter/narrow that shrinks the table to 0 or 1 rows leaves
    /// `TableState`'s own recorded range stale, so when the rows return
    /// to the same count as before, it never fires
    /// `visible_rows_changed` again. `invalidate_cells` must
    /// refill *this* field, not the (possibly shrunken) format cache's
    /// own window, or the cache stays stuck at the shrunken size forever
    /// — every cell outside it paints blank with nothing left to ever
    /// refill it. `invalidate_cells` itself fills through the private
    /// `fill_window`, not `refill_window`, so its own clamped refill
    /// (after e.g. an empty snapshot) never shrinks this field.
    requested_window: Range<usize>,
    /// The tree column's disclosure glyph for each *shown* row in
    /// `cache`'s current window, aligned index-for-index with it
    /// (`glyphs[i]` is `cache.window().start + i`) — resolved once per
    /// window fill in `refill_window`, never in `render_td`, so
    /// painting the tree column never calls `path_of` (an allocating
    /// ancestor walk) per cell. Empty until the first `refill_window`.
    glyphs: Vec<&'static str>,
    /// `[ui] line_numbers` (user ruling 2026-09-11), mirrored from the
    /// `linenumbers::UiSettings` global by the tile (`BlotterTile::
    /// on_ui_settings`) — the delegate has no `App` of its own in the
    /// paths that need it (`column`, `fill_window`).
    pub line_numbers: LineNumbers,
    /// The gutter text for each *shown* row in `numbers_stamp`'s range,
    /// aligned index-for-index like `glyphs` — rebuilt by
    /// `ensure_numbers` only when the window, the cursor row or the mode
    /// changed since the last build (`numbers_stamp`), so painting the
    /// gutter never formats a number per frame: a scroll or a cursor
    /// move costs one small `String` per visible row, once.
    numbers: Vec<SharedString>,
    numbers_stamp: Option<(Range<usize>, usize, LineNumbers)>,
    /// The named colours a `Colour::Named` column resolves against
    /// (Part 2c §6.2), handed down by the tile out of the one `Arc` the
    /// factory shares with every tile — refreshed on every plan the tile
    /// applies, which is every delivered snapshot, and a config reload
    /// makes every visible tile requery (`Frame::note_config_reloaded`).
    /// So a reloaded `colours.toml` reaches the paint one query later,
    /// exactly as a reloaded view definition does.
    colours: Arc<NamedColours>,
    /// One resolve per name per theme; see `colour_cache`'s module doc.
    colour_cache: ColourCache,
    /// The theme's own colours as `Anchors`/`Tokens`, memoised behind the
    /// signature they were derived from — the final review's I-1.
    ///
    /// `render_td` runs per visible cell, so deriving the pair at the
    /// paint site cost N x 28 `Hsla -> Rgb` conversions per named column
    /// per frame, where N is the visible row count; spec §6.3 promises
    /// one comparison a frame. `None` until the first named cell paints,
    /// which is what keeps the lazy read: a blotter naming no colour
    /// never builds it at all. See [`BlotterDelegate::ensure_theme_inputs`].
    theme_inputs: Option<([Hsla; 28], Anchors, Tokens)>,
}

/// The name column `col_ix` carries a `Colour::Named` of, if it does.
///
/// A free function over the plan rather than a `&self` method for the
/// borrow reason [`BlotterDelegate::cell_colour`] gives: the returned
/// `&str` borrows `plan` alone, leaving the delegate's other fields free
/// for the `&mut` the colour cache needs.
fn named_colour_of(plan: Option<&ColumnPlan>, col_ix: usize) -> Option<&str> {
    match plan
        .and_then(|p| p.columns.get(col_ix))
        .map(|c| &c.format.colour)
    {
        Some(Colour::Named(name)) => Some(name.as_str()),
        _ => None,
    }
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
            requested_window: 0..0,
            glyphs: Vec::new(),
            line_numbers: LineNumbers::Off,
            numbers: Vec::new(),
            numbers_stamp: None,
            colours: Arc::new(NamedColours::default()),
            colour_cache: ColourCache::new(),
            theme_inputs: None,
        }
    }

    /// The tile hands these down whenever it gives the delegate a plan.
    /// A different `Arc` means a reloaded `colours.toml`: everything
    /// resolved so far was resolved from the old definitions, so the
    /// cache goes with it. Pointer equality, not a deep compare — the
    /// factory shares exactly one `Arc` per loaded doc, so the same
    /// pointer IS the same definitions, and the common case (every
    /// snapshot, no reload) costs one pointer compare.
    pub fn set_colours(&mut self, colours: Arc<NamedColours>) {
        if !Arc::ptr_eq(&self.colours, &colours) {
            self.colours = colours;
            self.colour_cache.invalidate();
        }
    }

    /// The resolved named colour of column `col_ix`, or `None` for
    /// `none`, `sign` and a name the doc lacks — all three painted in
    /// the theme's foreground (§6.3). The one door both `render_td` and
    /// `render_th` go through, so a cell and its header can never
    /// disagree about a column's colour.
    pub fn cell_colour(
        &mut self,
        col_ix: usize,
        anchors: &Anchors,
        tokens: &Tokens,
    ) -> Option<Hsla> {
        // Borrows `self.plan` only, so the `&mut self.colour_cache`
        // below is a disjoint field — which is what lets the name stay a
        // `&str` rather than being cloned per cell per frame. That is
        // also why the lookup is a free function over `plan` rather than
        // a `&self` method: a method's returned `&str` would borrow the
        // whole delegate and shut the cache's own `&mut` out.
        let name = named_colour_of(self.plan.as_ref(), col_ix)?;
        self.colour_cache.get(&self.colours, name, anchors, tokens)
    }

    /// [`BlotterDelegate::cell_colour`] against the theme's own colours,
    /// derived at most once per theme rather than once per painted cell
    /// (Part 2c final review, I-1).
    ///
    /// The one door both paint sites use, so a cell and its header can no
    /// more disagree about the memo than they can about the colour.
    pub fn themed_cell_colour(&mut self, col_ix: usize, theme: &Theme) -> Option<Hsla> {
        self.ensure_theme_inputs(theme);
        // Four disjoint field borrows in one body — `plan` and `colours`
        // and `theme_inputs` shared, `colour_cache` mutable. Splitting
        // any of them out into a `&self` method would borrow the whole
        // delegate and this would not compile.
        let name = named_colour_of(self.plan.as_ref(), col_ix)?;
        // Bound under their own names, not `anchors`/`tokens`: the
        // mutation harness anchors an entry on `cell_colour`'s otherwise
        // identical call line, and two verbatim copies would make it
        // ambiguous.
        let (_, memo_anchors, memo_tokens) = self.theme_inputs.as_ref().expect("set just above");
        self.colour_cache
            .get(&self.colours, name, memo_anchors, memo_tokens)
    }

    /// Re-derive `theme_inputs` if and only if one of the twenty-eight
    /// theme colours the derivation reads has moved.
    ///
    /// The compare is the FULL signature, not a sentinel or two: a theme
    /// change that leaves `background`/`foreground` equal while moving an
    /// anchor would otherwise keep painting the old colour, and the
    /// `ColourCache` sitting behind this could never catch it — the stale
    /// derived pair IS its key (`colours::theme_signature`'s own doc).
    /// The steady path is 28 `Hsla` copies and 28 `Hsla` compares, with
    /// no `Hsla -> Rgb` conversion at all.
    fn ensure_theme_inputs(&mut self, theme: &Theme) {
        let signature = theme_signature(theme);
        match &self.theme_inputs {
            Some((have, ..)) if *have == signature => {}
            _ => {
                self.theme_inputs = Some((
                    signature,
                    anchors_from_theme(theme),
                    tokens_from_theme(theme),
                ));
            }
        }
    }

    /// Which of the three `Colour` shapes column `col_ix` carries, as a
    /// `Copy` classification. `render_td` reads this per cell per frame
    /// and must not clone the `String` a `Colour::Named` carries to do
    /// it — per-frame heap churn is a defect (PHILOSOPHY.md).
    fn colour_kind(&self, col_ix: usize) -> Option<ColourKind> {
        self.plan
            .as_ref()
            .and_then(|p| p.columns.get(col_ix))
            .map(|c| match c.format.colour {
                Colour::None => ColourKind::Plain,
                Colour::Sign => ColourKind::Sign,
                Colour::Named(_) => ColourKind::Named,
            })
    }

    /// The gutter's width in px for the current mode and row count —
    /// `0` when off. Read by `column` (the tree column widens by it, so
    /// the tree text keeps its own room) and by `render_td` (the gutter
    /// element's own width). Depends on `shown.len()`'s digit count, so
    /// a table growing past a power of ten widens on its next
    /// `TableState::refresh`, which every reflatten already triggers.
    pub fn gutter_px(&self) -> f32 {
        match self.line_numbers {
            LineNumbers::Off => 0.0,
            _ => gutter_digits(self.shown.len()) as f32 * GUTTER_DIGIT_PX + GUTTER_GAP_PX,
        }
    }

    /// Rebuild `numbers` for the cache's current window if anything it
    /// depends on changed — see the field's doc for why this is stamped
    /// rather than rebuilt per call.
    fn ensure_numbers(&mut self) {
        let window = self.cache.window();
        let mode = self.line_numbers;
        let cursor = self.cursor.row;
        // Only `rel` reads the cursor, so only `rel` stamps it: an `on`
        // gutter must not rebuild identical strings on every `j`/`k`
        // (review Minor 1).
        let stamped_cursor = match mode {
            LineNumbers::Relative => cursor,
            _ => usize::MAX,
        };
        let stamp = (window.clone(), stamped_cursor, mode);
        if self.numbers_stamp.as_ref() == Some(&stamp) {
            return;
        }
        self.numbers.clear();
        self.numbers.extend(window.map(|row| {
            gutter_number(mode, row, cursor)
                .map(|n| SharedString::from(n.to_string()))
                .unwrap_or_default()
        }));
        self.numbers_stamp = Some(stamp);
    }

    /// The gutter text `render_td` paints for a shown row — `None` when
    /// the gutter is off or the row is outside the cached window.
    /// Test-only: production code goes through `render_td`.
    #[cfg(test)]
    pub(crate) fn gutter_text(&mut self, row_ix: usize) -> Option<SharedString> {
        if self.line_numbers == LineNumbers::Off {
            return None;
        }
        self.ensure_numbers();
        row_ix
            .checked_sub(self.cache.window().start)
            .and_then(|i| self.numbers.get(i))
            .cloned()
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

    /// Installs a snapshot and answers whether the column plan was
    /// replaced. The plan is always built afresh from `view` — it is
    /// where a column's label, width, format and colour live, and a
    /// Views-dialog presentation edit (2c §5) arrives as a requery whose
    /// snapshot has the SAME columns at the same indices, so a gate on
    /// "grouping or column set changed" kept the stale plan painting until
    /// the next regroup (found on a display 2026-09-13). Building costs a
    /// walk over the view's columns, nothing against the snapshot; the
    /// old plan is kept only when the fresh one is equal in every field the
    /// paint reads, so an unchanged redelivery still moves nothing.
    pub fn apply_snapshot(
        &mut self,
        snapshot: Arc<Snapshot>,
        view: &ViewSpec,
        grouping: &[String],
    ) -> bool {
        let keep = self.cursor_path();
        let fresh = ColumnPlan::build(view, grouping, &snapshot);
        let rebuild = self.plan.as_ref() != Some(&fresh);
        if rebuild {
            self.plan = Some(fresh);
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
        rebuild
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

    /// Invalidate the format cache and the cached tree glyphs together,
    /// then immediately refill whatever window was on screen.
    ///
    /// `FormatCache::invalidate` clears its rows but leaves `start`
    /// unchanged, so `render_td`'s `glyphs` lookup (keyed off
    /// `cache.window().start`) would otherwise keep serving a stale
    /// glyph for a row whose text just went blank — exactly when the
    /// visible row *range* doesn't change across a regroup/sort/narrow
    /// (so `TableState` never calls `visible_rows_changed` to refill
    /// either of them). Clearing alone was C1 (final review): with more
    /// rows than the viewport the visible range essentially never
    /// changes, so `visible_rows_changed` was the *only* refill path and
    /// it never fired again — every `render_td` after the first
    /// regroup/sort/narrow/expand/collapse painted blank forever.
    /// `move_column` (gpui-component calls it directly, bypassing
    /// `visible_rows_changed` too) already captured its window and
    /// refilled immediately; this generalises that fix to every call
    /// site instead of just that one. `end` clamps to `self.shown.len()`
    /// because a narrow/regroup can shrink `shown` out from under the
    /// old window — nothing to refill then, and the cache/glyphs stay
    /// cleared, which is correct (there's nothing there to paint).
    /// Every `self.cache.invalidate()` call site must go through this
    /// instead.
    ///
    /// Refills `requested_window`, not `self.cache.window()`: gpui-
    /// component's `TableState::update_visible_range_if_need` stops
    /// reporting a new visible range once it has length ≤ 1 (`if
    /// visible_range.len() <= 1 { return; }`, pinned release,
    /// `gpui-component-0.6.2/src/table/state.rs`), so a filter/narrow
    /// that shrinks the table to 0 or 1 rows leaves the *cache's*
    /// window stuck at that shrunken size — when the row count later
    /// returns to what `TableState` last recorded, it sees no change
    /// and never fires `visible_rows_changed` again, so a
    /// `cache.window()`-based refill here would have nothing
    /// to widen back out from. `requested_window` is what the table last
    /// actually asked to see (remembered by `refill_window`, the only
    /// place that writes it), independent of how small the cache
    /// happened to shrink to since. The fill below goes through the
    /// private `fill_window`, not `refill_window` itself, so this
    /// clamped refill can never shrink `requested_window` back down —
    /// only a real `refill_window` call (`visible_rows_changed`,
    /// `move_column`, or a test standing in for either) may do that.
    fn invalidate_cells(&mut self) {
        let w = self.requested_window.clone();
        self.cache.invalidate();
        self.glyphs.clear();
        if !w.is_empty() {
            let end = w.end.min(self.shown.len());
            if w.start < end {
                self.fill_window(w.start..end);
            }
        }
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

    /// The tree text of every *un-narrowed* visible row — the domain
    /// `set_narrowed`'s positions are into. An fzf `/` session must match
    /// against this on every keystroke rather than `shown_texts()`:
    /// after the first narrow, `shown` is already the previous match
    /// subset, so re-matching against it would return positions in that
    /// subset's own index space, not in `visible`'s — silently narrowing
    /// into the wrong rows and making backspace unable to widen back out
    /// (review round 1, Finding 1).
    pub fn visible_texts(&self) -> Vec<String> {
        let (Some(snapshot), Some(plan)) = (&self.snapshot, &self.plan) else {
            return Vec::new();
        };
        self.visible
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

    /// Fill the cache for a window of *shown* rows, and remember it as
    /// `requested_window` — the window the table actually asked to see,
    /// which `invalidate_cells` falls back to refilling when `TableState`
    /// itself won't call this again (see `requested_window`'s own doc
    /// comment). This is the entry point `visible_rows_changed` and
    /// `move_column` call; `invalidate_cells`'s own (possibly clamped)
    /// refill goes through the private `fill_window` below instead, so
    /// it never shrinks what's remembered here.
    pub fn refill_window(&mut self, window: Range<usize>) {
        self.requested_window = window.clone();
        self.fill_window(window);
    }

    /// Fill the cache for a window of *shown* rows, without recording it
    /// as the requested window — used only by `invalidate_cells`'s own
    /// (possibly clamped-down) refill, so that refill can never shrink
    /// `requested_window` itself.
    fn fill_window(&mut self, window: Range<usize>) {
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

impl BlotterDelegate {
    /// Whether plan column `col` is a measure — the only kind with a
    /// magnitude to sort on.
    pub(crate) fn is_measure(&self, col: usize) -> bool {
        self.plan
            .as_ref()
            .and_then(|p| p.columns.get(col))
            .is_some_and(|c| c.kind == ColumnKind::Measure)
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
        let own_sort = self.sort.filter(|s| s.column == col_ix);
        let sort = match own_sort {
            Some(s) if s.order.descending() => Some(ColumnSort::Descending),
            Some(_) => Some(ColumnSort::Ascending),
            None => Some(ColumnSort::Default),
        };
        let mut label = c.label.clone();
        if !c.semi_joined.is_empty() {
            label.push_str(" ⋈");
        }
        // gpui-component's header arrow only knows a direction, so an
        // absolute sort says so in the label: `delta01 |x| ▾`.
        if own_sort.is_some_and(|s| s.order.absolute()) {
            label.push_str(" |x|");
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
            // The tree column carries the line-number gutter (below),
            // so it widens by the gutter's width rather than giving up
            // its own text room to it.
            width: px(if c.kind == ColumnKind::Tree {
                c.width + self.gutter_px()
            } else {
                c.width
            }),
            movable: c.kind != ColumnKind::Tree,
            // The tree column is pinned at the left (user ruling
            // 2026-09-12): the row's identity must stay readable however
            // far right the measures scroll. gpui-component renders a
            // `ColumnFixed::Left` column in its own unscrolled region
            // (`TableState::col_fixed`, on by default) and `scroll_to_col`
            // already subtracts the fixed count, so `sync_cursor`'s
            // absolute column index still lands where it should.
            fixed: if c.kind == ColumnKind::Tree {
                Some(ColumnFixed::Left)
            } else {
                None
            },
            ..Column::default()
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        _sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        // The tree column paints no sort icon (`column()` hands it
        // `sort: None`) and the component returns early on that, so this
        // is unreachable for it today; the guard keeps the hook honest
        // should the component ever call through anyway, matching the
        // keyboard path's own `col == 0` refusal.
        if self
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(col_ix))
            .is_none_or(|c| c.kind == ColumnKind::Tree)
        {
            return;
        }
        // gpui-component proposes the next of ITS three states, computed
        // from the arrow it cached for this column; the blotter has five
        // (spec §6.3), so the proposal is ignored and the click steps the
        // delegate's own cycle from the delegate's own state. The
        // component's cache (`col_groups`: the arrow, and the header name
        // the drag preview shows — the painted label itself is read live
        // through `render_th`) is now stale, and only `refresh` re-reads
        // `column()` — deferred, because `TableState` is the entity
        // currently on the stack. gpui drains effects FIFO and paints only
        // once the queue is empty, so no frame shows the component's
        // proposed arrow. The closure relies on the `cx.notify()` below
        // (and the component's own, after this hook returns) for the
        // repaint; it does not notify itself. The row highlight follows
        // the cursor the way `sync_cursor` does after a keyboard sort:
        // `reflatten` keeps the cursor by path, so its row index moves.
        let current = self.sort.filter(|s| s.column == col_ix).map(|s| s.order);
        let next = SortOrder::click_cycle(current, self.is_measure(col_ix));
        self.sort = next.map(|order| SortSpec {
            column: col_ix,
            order,
        });
        self.reflatten();
        cx.defer_in(window, |table, _, cx| {
            table.refresh(cx);
            let row = table.delegate().cursor.row;
            table.set_selected_row(row, cx);
        });
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
        // and never fires `visible_rows_changed`, so this needs its own
        // immediate refill rather than waiting for the next scroll —
        // `invalidate_cells` now does exactly that (C1 fix; this method
        // is what the fix generalised from). Recomputing the glyphs here
        // too is unnecessary work (the tree column never moves —
        // `ColumnPlan::move_column` refuses `from == 0 || to == 0`) but
        // harmless: `refill_window` recomputes the same values from the
        // same tree, and the window is only ever tens of rows.
        self.invalidate_cells();
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

    /// The default's element (`div().size_full().child(name)`) plus the
    /// column's own named colour (§6.3), so a coloured column is
    /// identifiable from its header and not only from cells that happen
    /// to be additive. Everything around it — the sort arrow, the
    /// header cell's padding, borders and drag handle — is the
    /// component's own (`TableState::render_th` wraps this), so
    /// overriding here loses none of it.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let name = self.column(col_ix, cx).name.clone();
        // Same lazy read as `render_td`'s named arm, for the same
        // reason: an uncoloured column touches the theme's twelve+fifteen
        // colours not at all — `themed_cell_colour` derives them only
        // when this arm is the one taken, and only when the theme moved.
        let colour = match self.colour_kind(col_ix) {
            Some(ColourKind::Named) => self.themed_cell_colour(col_ix, cx.theme()),
            _ => None,
        };
        div()
            .size_full()
            .when_some(colour, |el, c| el.text_color(c))
            .child(name)
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
        let colour = self.colour_kind(col_ix);
        let mut el = div()
            .size_full()
            .flex()
            .items_center()
            .px_1()
            .font_family(fonts::MONO)
            // I4 (final review): lets a test locate this exact cell with
            // `cx.debug_bounds` and, via `TableDelegate::cache`, read
            // back what was painted into it. `debug_selector` is a
            // gpui-provided no-op in a non-test/non-`test-support` build
            // (the closure is dropped unevaluated, pinned release,
            // `gpui-pre-0.3.5/src/elements/div.rs`), so this costs
            // nothing on the render thread in release.
            .debug_selector(|| format!("blotter-cell-{row_ix}-{col_ix}"))
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
            let indent = px(depth as f32 * INDENT);
            // The line-number gutter (`[ui] line_numbers`, user ruling
            // 2026-09-11) sits at the cell's leading edge, before the
            // indent, right-aligned in a slot sized to the row total's
            // digit count — a *gutter*, not a column: `h`/`l`, sort,
            // yank and the column plan never see it. The cursor row's
            // number is painted in the full foreground (in `rel` mode
            // it is the row's absolute number, the hybrid), every other
            // row's in the muted one. Its text comes from `numbers`,
            // rebuilt only when `ensure_numbers`'s stamp changes.
            if self.line_numbers != LineNumbers::Off {
                let (fg, muted) = (theme.foreground, theme.muted_foreground);
                // The off branch's `pl(indent)` replaces the root's
                // `px_1` left padding (a depth-0 row sits flush); the
                // gutter must start flush too, or the tree text loses
                // that padding's worth of the room `column()` widened
                // by (review Minor 2).
                el = el.pl(px(0.));
                self.ensure_numbers();
                let text = row_ix
                    .checked_sub(self.cache.window().start)
                    .and_then(|i| self.numbers.get(i))
                    .cloned()
                    .unwrap_or_default();
                let on_cursor_row = self.cursor.row == row_ix;
                el = el.child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .justify_end()
                        .w(px(self.gutter_px()))
                        .pr(px(GUTTER_GAP_PX))
                        .mr(indent)
                        .text_color(if on_cursor_row { fg } else { muted })
                        .debug_selector(|| format!("blotter-gutter-{row_ix}"))
                        .child(text),
                );
            } else {
                el = el.pl(indent);
            }
            // The glyph is a click target: a single click on it toggles
            // the row (`ChevronClicked`, handled by the tile). It stops
            // propagation so the row's own click handler never sees the
            // press — otherwise a fast double-click on the chevron would
            // toggle here AND again through `TableEvent::DoubleClickedRow`
            // — and it ignores the second press of a pair itself, so
            // that double-click toggles exactly once. The listener
            // captures one `usize`; gpui boxes it per element either way.
            el = el.child(
                div()
                    .id(("chevron", row_ix))
                    .w(px(14.))
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .debug_selector(|| format!("blotter-chevron-{row_ix}"))
                    .on_click(cx.listener(move |this, e: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                        if e.click_count() > 1 {
                            return;
                        }
                        this.set_selected_row(row_ix, cx);
                        cx.emit(ChevronClicked(row_ix));
                    }))
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
                    (Some(ColourKind::Sign), Some(Sign::Negative)) => {
                        el.text_color(theme.chart_bearish)
                    }
                    (Some(ColourKind::Sign), Some(Sign::Positive)) => {
                        el.text_color(theme.chart_bullish)
                    }
                    // A named colour ignores the sign entirely (§6.3):
                    // `sign` and a name are alternatives, not layers.
                    // An unknown name falls back to the theme's own
                    // foreground — the same thing an uncoloured cell
                    // paints in, so a deleted definition is invisible
                    // rather than wrong (`load_views` warns about it).
                    //
                    // The theme is read into the resolver's vocabulary
                    // *here*, inside the arm, rather than once at the top
                    // of the method: it is twelve plus fifteen
                    // `Hsla -> Rgb` conversions, and a blotter whose
                    // columns name no colour (every one of them today)
                    // must not pay them at all. `themed_cell_colour`
                    // keeps that laziness and adds the memo the final
                    // review's I-1 asked for: this arm runs per visible
                    // cell, so the conversions themselves happen once per
                    // theme, behind a 28-value signature compare, not
                    // once per cell per frame. The cache's invalidation
                    // stays free either way — the derived pair IS its
                    // key, so a theme swap empties it with nothing to
                    // remember to call, and the signature is what makes
                    // sure the memo hands it a *fresh* pair to be keyed
                    // on.
                    (Some(ColourKind::Named), _) => el.text_color(
                        self.themed_cell_colour(col_ix, theme)
                            .unwrap_or(theme.foreground),
                    ),
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
    use geode_core::colour::{Anchors, Definition, NamedColours, Rgb, Token, Tokens};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_core::view::{Colour, ViewPresentationSpec, ViewSpec};

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

    /// A single-level grouping (`["lhu"]`): the root plus `n - 1` direct
    /// children, all visible with no explicit expand (`flatten`'s root is
    /// always open, and a node's direct children are always shown —
    /// `crate::core::flatten`'s own
    /// `collapsed_shows_the_root_and_its_children_only_when_the_root_is_open`).
    /// `n == 1` gives just the root: the "a filter emptied the table"
    /// shape the C1-successor defect below reproduces.
    fn snapshot_with_rows(n: usize) -> Arc<Snapshot> {
        let mut lhu = vec![None];
        let mut row_depth = vec![0i32];
        let mut delta = vec![Some(0.0)];
        for i in 0..n.saturating_sub(1) {
            lhu.push(s(&format!("R{i}")));
            row_depth.push(1);
            delta.push(Some(i as f64));
        }
        Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(lhu)),
                (dim("row_depth"), TestColumn::I32(row_depth)),
                (dim("delta01"), TestColumn::F64(delta)),
            ],
            1,
        ))
    }
    fn flat_view() -> ViewSpec {
        let text =
            "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[t.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0.remove(0)
    }
    fn flat_grouping() -> Vec<String> {
        vec!["lhu".into()]
    }

    /// The disclosure glyphs and the determined-attribution mark are the
    /// code points the design names, spelled here as ASCII escapes on
    /// purpose. On 2026-09-13 this file was committed with every
    /// non-ASCII character double-encoded through Latin-1 (`▸` became
    /// `â–¸`, painted verbatim in the tree column), and the suite stayed
    /// green because the tests' own literal `"▸"` expectations were
    /// corrupted identically. An escape cannot be re-encoded, so this
    /// test fails the moment the literals are.
    #[test]
    fn tree_glyphs_are_the_code_points_the_design_names() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.refill_window(0..3);
        // Row 1 (L1) has children and is closed.
        assert_eq!(
            d.glyph_at(1),
            Some("\u{25B8}"),
            "closed: BLACK RIGHT-POINTING SMALL TRIANGLE"
        );
        d.expansion
            .toggle(path_of(&snapshot(), d.plan.as_ref().unwrap(), 1));
        d.refill_window(0..3);
        assert_eq!(
            d.glyph_at(1),
            Some("\u{25BE}"),
            "open: BLACK DOWN-POINTING SMALL TRIANGLE"
        );
        assert_eq!(DETERMINED_MARK, "\u{2020}", "the determined mark is DAGGER");
    }

    /// Regression for the successor to C1: `TableState::
    /// update_visible_range_if_need` (pinned release,
    /// `gpui-component-0.6.2/src/table/state.rs`) only records a new
    /// visible range when it has more than one row, so a snapshot
    /// that shrinks the table to 0 or 1 rows leaves
    /// `TableState`'s own recorded range stale. When rows return to a
    /// count `TableState` has already seen, it never fires
    /// `visible_rows_changed` again — a refill keyed off the format
    /// cache's own (possibly still-shrunken) window would then have no
    /// path back to the full window ever again. `refill_window` here
    /// stands in for what `visible_rows_changed` itself does (it records
    /// `requested_window` on the way); the middle `apply_snapshot`
    /// deliberately does not call it, matching `TableState` never firing
    /// on the way back.
    #[test]
    fn a_window_shrunk_by_an_empty_snapshot_grows_back_when_rows_return() {
        let mut d = BlotterDelegate::new();
        let big = snapshot_with_rows(40);
        d.apply_snapshot(big.clone(), &flat_view(), &flat_grouping());
        d.refill_window(0..10);
        assert!(d.cache.get(5, 0).is_some(), "sanity: row 5 is cached");
        // The table shrinks to one row: `TableState` never records the
        // new range (`len() <= 1`), so no `visible_rows_changed` follows
        // — `requested_window` stays `0..10` throughout.
        d.apply_snapshot(snapshot_with_rows(1), &flat_view(), &flat_grouping());
        // Rows return. `TableState` sees the same range it last recorded
        // and does not call `visible_rows_changed`. `invalidate_cells`
        // must have refilled the last requested window on its own.
        d.apply_snapshot(big, &flat_view(), &flat_grouping());
        assert!(
            d.cache.get(5, 0).is_some(),
            "row 5 must be cached again without a visible_rows_changed"
        );
        assert!(d.cache.get(9, 0).is_some());
    }

    /// Same defect, reached through `/` narrowing rather than a shrunken
    /// snapshot — `set_narrowed` goes through `invalidate_cells` exactly
    /// the same way.
    #[test]
    fn a_window_shrunk_by_narrowing_to_one_row_grows_back_when_narrowing_clears() {
        let mut d = BlotterDelegate::new();
        let big = snapshot_with_rows(40);
        d.apply_snapshot(big, &flat_view(), &flat_grouping());
        d.refill_window(0..10);
        assert!(d.cache.get(5, 0).is_some(), "sanity: row 5 is cached");
        d.set_narrowed(Some(vec![0]));
        d.set_narrowed(None);
        assert!(
            d.cache.get(5, 0).is_some(),
            "row 5 must be cached again without a visible_rows_changed"
        );
        assert!(d.cache.get(9, 0).is_some());
    }

    /// `[ui] line_numbers`: off paints nothing and costs no width; `on`
    /// is the 1-based shown index; `rel` is the distance from the cursor
    /// with the cursor row showing its own absolute number (the hybrid,
    /// by ruling), and a cursor move re-derives it without a refill.
    #[test]
    fn the_gutter_follows_the_mode_and_the_cursor() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.refill_window(0..3);
        assert_eq!(d.shown, vec![0, 1, 2], "sanity: three shown rows");

        assert_eq!(d.gutter_text(0), None, "off paints nothing");
        assert_eq!(d.gutter_px(), 0.0, "off costs no width");

        d.line_numbers = LineNumbers::On;
        let texts = |d: &mut BlotterDelegate| -> Vec<String> {
            (0..3)
                .map(|r| d.gutter_text(r).unwrap().to_string())
                .collect()
        };
        assert_eq!(texts(&mut d), vec!["1", "2", "3"]);
        assert_eq!(
            d.gutter_px(),
            2.0 * GUTTER_DIGIT_PX + GUTTER_GAP_PX,
            "two digit cells for three rows (the floor), plus the gap"
        );

        d.line_numbers = LineNumbers::Relative;
        d.cursor.row = 1;
        assert_eq!(
            texts(&mut d),
            vec!["1", "2", "1"],
            "cursor row shows its absolute number"
        );
        d.cursor.row = 2;
        assert_eq!(
            texts(&mut d),
            vec!["2", "1", "3"],
            "a cursor move re-derives the offsets"
        );
        assert_eq!(
            d.gutter_text(3),
            None,
            "a row outside the cached window has no gutter text"
        );
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
        // C1 (final review): `set_narrowed` still invalidates the stale
        // narrowed-window cache, but `invalidate_cells` now also refills
        // the window it had immediately (against the un-narrowed
        // `shown` list here) — it is not simply left blank.
        assert_eq!(
            d.cache.get(1, 0).map(|c| c.text.to_string()),
            Some("L1".to_string()),
            "the window was refilled immediately against the widened shown list"
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
    fn apply_snapshot_invalidates_the_cache_and_refills_it() {
        // The cache-invalidation entry the reviewer asked for: distinct
        // from `narrowing_changes_what_is_shown_and_the_cache_window_follows_shown_rows`,
        // whose final assertion goes through `set_narrowed`'s own
        // `invalidate()` call, not `apply_snapshot`'s.
        //
        // C1 (final review) updated this test's shape: `apply_snapshot`
        // still invalidates the stale cache on its own, but
        // `invalidate_cells` now also refills the window it had
        // immediately — so this proves invalidation actually happened
        // by re-applying a snapshot whose grand-total value genuinely
        // changed and checking the cache holds the *new* value, not a
        // stale (or blank) one.
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.refill_window(0..3);
        assert_eq!(
            d.cache.get(0, 1).map(|c| c.text.to_string()),
            Some("9.00".to_string()),
            "the grand total's delta01 is cached"
        );
        assert_eq!(d.narrowed, None, "narrowing plays no part in this");
        let changed = Arc::new(Snapshot::for_tests(
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
                    TestColumn::F64(vec![Some(99.0), Some(5.0), Some(4.0), Some(2.0), Some(3.0)]),
                ),
            ],
            3,
        ));
        d.apply_snapshot(changed, &view(), &grouping());
        assert_eq!(
            d.cache.get(0, 1).map(|c| c.text.to_string()),
            Some("99.00".to_string()),
            "apply_snapshot invalidated the stale cache and refilled it \
             from the new snapshot, with no explicit refill_window call"
        );
    }

    #[test]
    fn a_regroup_that_keeps_the_window_refills_it_immediately() {
        // C1 (final review): the previous shape of this test asserted
        // that a regroup/sort/narrow whose visible row range doesn't
        // change (gpui-component's `TableState` only calls
        // `visible_rows_changed` when the numeric range differs, which
        // it never does once there are more rows than the viewport)
        // left the window blank — and called that correct. It wasn't:
        // with `visible_rows_changed` as the *only* other refill path,
        // "blank until invalidated" meant "blank forever" for any tile
        // with a scrollbar. `invalidate_cells` now captures the window
        // it had and refills it immediately (the same fix `move_column`
        // already needed on its own, generalised here), so this asserts
        // the glyph AND the cell text are back with no explicit
        // `refill_window` call in between — this doubles as test (a)
        // from I4.
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
        // `visible_rows_changed`.
        d.apply_snapshot(snapshot(), &view(), &grouping());
        assert_eq!(d.shown, vec![0, 1, 2], "the visible row range is unchanged");
        assert_eq!(
            d.glyph_at(1),
            Some("▸"),
            "invalidate_cells refills the window it had — the glyph is \
             back with no explicit refill_window call"
        );
        assert_eq!(
            d.cache.get(1, 0).map(|c| c.text.to_string()),
            Some("L1".to_string()),
            "the cell text is refilled too, not just the glyph"
        );
    }

    #[test]
    fn invalidate_cells_clears_stale_glyphs_when_shown_shrinks_past_the_old_window() {
        // The half of the old defect that's still real: `invalidate_
        // cells`'s refill only reaches rows `shown` still has (`end =
        // w.end.min(shown.len())`) — when the old window's start is at
        // or past the new, shorter `shown.len()` there is nothing left
        // to refill, and the explicit `self.glyphs.clear()` is what
        // stops a stale glyph surviving that. `render_td` itself would
        // never surface this (a row index past `shown.len()` is never
        // painted), so this reaches directly for `glyph_at`, which
        // (like `render_td`) only guards against an empty cache window,
        // not against `shown` having shrunk.
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.refill_window(0..3);
        assert_eq!(d.glyph_at(1), Some("▸"), "sanity: a real glyph is cached");
        // Narrow to no matches: `shown` becomes empty, well short of the
        // old window's start (0), so `invalidate_cells` cannot refill.
        d.set_narrowed(Some(vec![]));
        assert!(d.shown.is_empty());
        assert_eq!(
            d.glyph_at(1),
            None,
            "the stale glyph must not survive just because nothing could \
             be refilled"
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
    /// §6.3: a column naming a colour paints that colour, resolved
    /// against the theme's own anchors/tokens; `none`, `sign` and a name
    /// `colours.toml` does not define all resolve to nothing, which the
    /// paint sites read as "the theme's foreground" — never a stale or
    /// invented colour. Drives the one door both `render_td` and
    /// `render_th` go through, so this covers the header label too.
    #[test]
    fn a_named_column_paints_its_resolved_colour() {
        let grey = Rgb {
            r: 0.5,
            g: 0.5,
            b: 0.5,
        };
        let red = Rgb {
            r: 0.75,
            g: 0.125,
            b: 0.125,
        };
        let anchors = Anchors {
            normal: [grey; 6],
            light: [grey; 6],
        };
        let tokens = Tokens {
            foreground: red,
            muted: grey,
            primary: grey,
            accent: grey,
            danger: grey,
            warning: grey,
            success: grey,
            info: grey,
            chart: [grey; 5],
            bullish: grey,
            bearish: grey,
            background: grey,
        };
        let mut colours = NamedColours::default();
        // A token definition, so the expected value is exactly the
        // token's own colour and the assertion reads as one.
        colours.insert("delta".into(), Definition::Token(Token::Foreground));

        // Columns 1..3: a named colour, `sign`, and a name the doc lacks.
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                    [[t.columns]]\nname = \"delta01\"\nformat = { colour = \"delta\" }\n\
                    [[t.columns]]\nname = \"gamma01\"\nformat = { colour = \"sign\" }\n\
                    [[t.columns]]\nname = \"vega01\"\nformat = { colour = \"ghost\" }\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let snapshot = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1])),
                (dim("delta01"), TestColumn::F64(vec![Some(9.0), Some(5.0)])),
                (dim("gamma01"), TestColumn::F64(vec![Some(1.0), Some(2.0)])),
                (dim("vega01"), TestColumn::F64(vec![Some(1.0), Some(2.0)])),
            ],
            1,
        ));

        let mut d = BlotterDelegate::new();
        d.set_colours(Arc::new(colours));
        d.apply_snapshot(snapshot, &view, &["lhu".to_string()]);

        assert_eq!(
            d.cell_colour(1, &anchors, &tokens),
            Some(geode_shell::shell::colours::to_hsla(red)),
            "a named column resolves its own definition against the theme"
        );
        assert_eq!(
            d.cell_colour(0, &anchors, &tokens),
            None,
            "the tree column names no colour"
        );
        assert_eq!(
            d.cell_colour(2, &anchors, &tokens),
            None,
            "`sign` is not a named colour — the sign arm paints it"
        );
        assert_eq!(
            d.cell_colour(3, &anchors, &tokens),
            None,
            "a name colours.toml does not define paints in foreground, \
             never some other column's colour"
        );
        assert_eq!(
            d.cell_colour(99, &anchors, &tokens),
            None,
            "a column index the plan does not have is not a panic"
        );
    }

    /// I-1 (Part 2c final review): the theme -> `Anchors`/`Tokens`
    /// derivation is memoised behind the full 28-value signature, so a
    /// steady theme re-derives NOTHING across paints and a moved theme
    /// colour re-derives on the next one.
    ///
    /// Both halves are asserted through a deliberately poisoned memo,
    /// because that is the only way the difference is observable: a
    /// re-derivation under an unchanged theme produces a pair equal to
    /// the one it replaced, so an assertion on the pair's *value* would
    /// pass whether the memo works or not. The anchor moved in the
    /// second half is `red` — neither `background` nor `foreground`, so
    /// this also pins the reason the signature is 28 values rather than
    /// the two-sentinel sketch.
    #[test]
    fn the_theme_input_memo_re_derives_only_when_a_theme_colour_moves() {
        let mut theme = Theme::default();
        let mut d = BlotterDelegate::new();
        assert!(
            d.theme_inputs.is_none(),
            "the memo is lazy — a blotter that paints no named colour never builds it"
        );
        d.ensure_theme_inputs(&theme);

        let poison = Rgb {
            r: 0.01,
            g: 0.02,
            b: 0.03,
        };
        d.theme_inputs.as_mut().expect("derived once").1.normal[0] = poison;
        d.ensure_theme_inputs(&theme);
        assert_eq!(
            d.theme_inputs.as_ref().expect("still memoised").1.normal[0],
            poison,
            "an unchanged theme re-derived the pair — the signature compare is not holding,              and every painted cell is paying 28 conversions again"
        );

        theme.red = gpui::hsla(0.0, 0.8, 0.25, 1.0);
        d.ensure_theme_inputs(&theme);
        assert_eq!(
            d.theme_inputs.as_ref().expect("re-derived").1.normal[0],
            geode_shell::shell::colours::to_rgb(theme.red),
            "a moved anchor must re-derive the pair — a two-sentinel compare would              have missed this one, and the colour cache could not, since the stale              pair is its own key"
        );
    }

    /// The trader edits a column's presentation in the Views dialog (label,
    /// width, colour — 2c §5); the reload requeries and the SAME columns
    /// come back at the same indices. The plan is where those settings
    /// live, so it must be rebuilt anyway — the old gate (grouping or
    /// column set changed) kept the stale label, width and colour painting
    /// until the next regroup, which is what a trader reported on
    /// 2026-09-13. A same-view redelivery still rebuilds nothing.
    #[test]
    fn a_presentation_change_rebuilds_the_plan_on_a_same_column_snapshot() {
        let mut d = BlotterDelegate::new();
        assert!(
            d.apply_snapshot(snapshot(), &view(), &grouping()),
            "the first snapshot builds the plan"
        );
        let before = d.plan.clone().expect("built");
        assert!(
            !d.apply_snapshot(snapshot(), &view(), &grouping()),
            "an identical view and snapshot rebuild nothing"
        );

        let mut views = vec![view()];
        let doc = merge_docs(
            "view_presentation",
            &[LayerDoc::builtin(
                "view_presentation",
                "[t.columns.delta01]\nlabel = \"Δ\"\nwidth = 90\ncolour = \"delta\"\n",
            )
            .unwrap()],
        );
        let (presentation, diags) = ViewPresentationSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(presentation.apply(&mut views).is_empty());
        assert!(
            d.apply_snapshot(snapshot(), &views[0], &grouping()),
            "a presentation change must rebuild the plan even though no column moved"
        );
        let after = d.plan.clone().expect("rebuilt");
        assert_ne!(before, after);
        let col = after
            .columns
            .iter()
            .find(|c| c.name == "delta01")
            .expect("delta01 planned");
        assert_eq!(col.label, "Δ");
        assert_eq!(col.width, 90.0);
        assert_eq!(col.format.colour, Colour::Named("delta".into()));

        // Hiding it drops it from the plan on the same-column redelivery too.
        let mut hidden = vec![view()];
        let doc = merge_docs(
            "view_presentation",
            &[LayerDoc::builtin("view_presentation", "[t]\nhidden = [\"delta01\"]\n").unwrap()],
        );
        let (presentation, _) = ViewPresentationSpec::from_doc(&doc);
        assert!(presentation.apply(&mut hidden).is_empty());
        assert!(d.apply_snapshot(snapshot(), &hidden[0], &grouping()));
        assert!(
            d.plan
                .as_ref()
                .unwrap()
                .columns
                .iter()
                .all(|c| c.name != "delta01"),
            "a hidden column leaves the plan on redelivery"
        );
    }
}
