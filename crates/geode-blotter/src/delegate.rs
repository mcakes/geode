//! Table adapter owning the snapshot, column plan, expansion, visible rows,
//! cursor, selection, sort, and display caches. Pure core helpers prepare the
//! rows and aggregates; render reads cached text, glyphs, and selection state.
//! Theme and gutter presentation use separate memos to avoid repeated work.

use crate::core::cache::{FormatCache, cell};
use crate::core::cursor::{Cursor, find_by_path, restore_by_path};
use crate::core::expansion::{Expansion, Path, depth_bound, path_of};
use crate::core::flatten::{SortOrder, SortSpec, flatten};
use crate::core::format::Sign;
use crate::core::plan::{ColumnKind, ColumnPlan};
use crate::core::select::summarize;
use geode_core::attribution::Attribution;
use geode_core::colour::{Anchors, NamedColours, Tokens};
use geode_core::grid::selection::{Lost, Resolved, SelectKind, Selection, UNSUMMABLE_MARK};
use geode_core::snapshot::Snapshot;
use geode_core::view::{Colour, ViewSpec};
use geode_shell::colfit::{FitMetrics, FittedWidths};
use geode_shell::fonts;
use geode_shell::linenumbers::{GUTTER_GAP_PX, LineNumbers, gutter_number};
use geode_shell::shell::aggregates::{AggregateCell, CellPaint};
use geode_shell::shell::colours::{anchors_from_theme, theme_signature, tokens_from_theme};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_tile::colour::{ColourCache, Resolved as ColourResolved};
use gpui::prelude::*;
use gpui::{
    App, ClickEvent, Context, Div, EventEmitter, Hsla, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, SharedString, Stateful, TextAlign, Window, div, px,
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

/// A cell, gutter, or row-background selection gesture sent to the tile.
/// The tile's `pointer` handler updates the cursor and selection through the
/// same delegate state used by keyboard actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellPointer {
    Press {
        row: usize,
        col: usize,
        shift: bool,
        gutter: bool,
    },
    Drag {
        row: usize,
        col: usize,
        gutter: bool,
    },
}

impl EventEmitter<CellPointer> for TableState<BlotterDelegate> {}

/// A column's `colour` setting reduced to what a paint site needs to
/// branch on — see `BlotterDelegate::colour_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColourKind {
    Plain,
    Sign,
    Named,
}

const INDENT: f32 = 14.0;
/// The tree cell's chevron slot (`render_td`'s `w(px(14.))`).
const CHEVRON_PX: f32 = 14.0;
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
    /// A live grid selection, anchored by row
    /// path and column name so it survives a re-sort, a column move or a
    /// redelivery. `None` in the ordinary cursor-only state.
    pub selection: Option<Selection<Path, String>>,
    /// `selection` re-resolved against the current `shown`/`plan` by
    /// `refresh_selection` — the only field `render_tr`/`render_td` read
    /// to paint the tint, so painting never re-resolves anything itself.
    pub resolved: Option<Resolved>,
    /// The footer's per-column aggregates over `resolved`, rebuilt
    /// alongside it.
    pub summary: Vec<AggregateCell>,
    /// The `plan.columns` index each `summary` group was taken from —
    /// what `ensure_summary_paint` resolves its colors against.
    summary_cols: Vec<usize>,
    /// Bumped whenever `summary` is rebuilt, so the paint memo knows the
    /// groups it colored are stale.
    summary_generation: u64,
    /// Colours aligned with `summary`, memoized until the summary, theme
    /// inputs, or named-colour definitions change.
    pub summary_paint: Vec<CellPaint>,
    summary_paint_stamp: Option<(u64, [Hsla; 28])>,
    /// The footer's `"{rows} rows × {cols} cols"` readout, leading the
    /// strip while a selection is live; `None` otherwise. Prepared with
    /// `summary` so render formats nothing.
    pub selection_extent: Option<SharedString>,
    /// Whether any `summary` cell refuses a total with `†` / `‡`, so
    /// render shows each legend without scanning the strings per frame.
    pub summary_non_additive: bool,
    pub summary_unsummable: bool,
    /// Set by `refresh_selection` when a live selection's anchor row —
    /// or a block's anchor column — is no longer displayed and the
    /// selection was cleared as a result, naming which one; taken by the
    /// tile to raise its one-shot notice.
    pub selection_lost: Option<Lost>,
    /// The last resolved anchor row index, kept as `find_by_path`'s
    /// `near` so a redelivery's re-resolution starts its search where
    /// the anchor was last seen rather than from row 0.
    anchor_hint: usize,
    pub sort: Option<SortSpec>,
    /// The column whose sort `apply_snapshot` just dropped because a
    /// rebuild no longer carries it (a view edit hid it, or a regroup
    /// folded it into the tree column) — `None` once the tile has read
    /// and cleared it. The rows reorder to default order either way, so
    /// this is what lets the tile tell the trader why, rather than
    /// leaving a bare reorder for them to puzzle out.
    pub dropped_sort: Option<String>,
    pub cache: FormatCache,
    pub narrowed: Option<Vec<usize>>,
    pub unplaced: usize,
    /// Whether any painted cell carried the dagger, for the footer.
    pub any_determined: bool,
    pub semi_joined: Vec<String>,
    /// Last window requested by the table, retained even when fewer rows
    /// remain. Invalidation refills this range because an unchanged numeric
    /// range may produce no table callback; see `invalidate_cells`.
    requested_window: Range<usize>,
    /// The tree column's disclosure glyph for each *shown* row in
    /// `cache`'s current window, aligned index-for-index with it
    /// (`glyphs[i]` is `cache.window().start + i`) — resolved once per
    /// window fill in `refill_window`, never in `render_td`, so
    /// painting the tree column never calls `path_of` (an allocating
    /// ancestor walk) per cell. Empty until the first `refill_window`.
    glyphs: Vec<&'static str>,
    /// `[ui] line_numbers`, mirrored from `linenumbers::UiSettings` by the
    /// tile. `column` and `fill_window` need this value without an `App`.
    pub line_numbers: LineNumbers,
    /// Gutter text aligned with the format-cache window. `ensure_numbers`
    /// rebuilds it when the window or mode changes, or when the cursor moves
    /// in relative mode. Painting otherwise reuses these strings.
    numbers: Vec<SharedString>,
    numbers_stamp: Option<(Range<usize>, usize, LineNumbers)>,
    /// Named-colour definitions supplied by the tile from the factory's
    /// shared `Arc`. Snapshot delivery refreshes this reference; config
    /// reload requeries visible tiles, so changed definitions reach this
    /// delegate with the next applied snapshot.
    colours: Arc<NamedColours>,
    /// One resolve per name per theme; see `geode_tile::colour`'s module doc.
    colour_cache: ColourCache,
    /// Theme-derived `Anchors`/`Tokens`, memoized behind all 28 input colours.
    /// Named-cell, header, and footer lookups compare the signature on each
    /// call, but convert colours only when it changes. Remains empty while
    /// no named-colour lookup occurs; see [`Self::ensure_theme_inputs`].
    theme_inputs: Option<([Hsla; 28], Anchors, Tokens)>,
    /// Last cell reported by a drag, suppressing repeated events while the
    /// pointer stays within it. A press resets this memo; it has no effect
    /// when `drag_origin` is absent.
    drag_last: Option<(usize, usize)>,
    /// Whether the primary button is currently down because of a press
    /// this delegate's own cell or gutter caught, and if so, which of the
    /// two it was (`true` for the gutter). `None` outside such a press —
    /// including while some other element owns the drag (a scrollbar
    /// thumb, a header column reorder, a tile-split divider, a text
    /// selection started in another tile) — so `render_td`'s move handler
    /// can tell "the button is down over a cell" apart from "the button
    /// is down because of a press that started somewhere else and is now
    /// passing over a cell", and emit `CellPointer::Drag` only for the
    /// former. Set by a cell/gutter's own mouse-down; cleared by a mouse
    /// release anywhere (gpui dispatches every registered mouse listener
    /// for every mouse event, hit or not — each one's own hit check, not
    /// tree position, decides whether it fires — so a cell's own
    /// `on_mouse_up`/`on_mouse_up_out` pair sees every release exactly
    /// once regardless of where it lands).
    drag_origin: Option<bool>,
    /// Widths `:autosize` fitted, keyed by `PlannedColumn::name` (the
    /// tree column's name is empty), in pixels without the gutter.
    /// `column()` prefers an entry here over the plan's width, so every
    /// `TableState::refresh` keeps it; a column with no entry keeps its
    /// configured width. The tile persists it and clears it on a view switch.
    pub fitted: FittedWidths,
    /// Chevron pointer states memoized by all inputs read by the contrast
    /// calculation. Reusing the result avoids conversions and contrast
    /// correction for every visible row on every frame.
    chevron: Option<(control::ControlInputs, control::ControlPaint)>,
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
            selection: None,
            resolved: None,
            summary: Vec::new(),
            summary_cols: Vec::new(),
            summary_generation: 0,
            summary_paint: Vec::new(),
            summary_paint_stamp: None,
            selection_extent: None,
            summary_non_additive: false,
            summary_unsummable: false,
            selection_lost: None,
            anchor_hint: 0,
            sort: None,
            dropped_sort: None,
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
            chevron: None,
            drag_last: None,
            drag_origin: None,
            fitted: FittedWidths::new(),
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
            // The footer's memoized group colors came from the old
            // definitions too.
            self.summary_paint_stamp = None;
        }
    }

    /// Named colour of column `col_ix`, including its base and sign variants.
    /// Returns `None` for plain/sign columns and undefined names. Paint sites
    /// handle plain and sign settings separately and use the foreground for
    /// undefined names. Named cells use the sign variant; headers use the base.
    pub fn cell_colour(
        &mut self,
        col_ix: usize,
        anchors: &Anchors,
        tokens: &Tokens,
    ) -> Option<ColourResolved> {
        // Borrows `self.plan` only, so the `&mut self.colour_cache`
        // below is a disjoint field — which is what lets the name stay a
        // `&str` rather than being cloned per cell per frame. That is
        // also why the lookup is a free function over `plan` rather than
        // a `&self` method: a method's returned `&str` would borrow the
        // whole delegate and shut the cache's own `&mut` out.
        let name = named_colour_of(self.plan.as_ref(), col_ix)?;
        self.colour_cache.get(&self.colours, name, anchors, tokens)
    }

    /// Resolve a column's named colour using memoized theme inputs. Cells,
    /// headers, and footer totals share this lookup and its invalidation.
    pub fn themed_cell_colour(&mut self, col_ix: usize, theme: &Theme) -> Option<ColourResolved> {
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

    /// Resolve footer label and total colours. Labels use a named colour's
    /// base; totals use its sign variants, the bullish/bearish pair for `sign`,
    /// or foreground otherwise. Rebuild after a summary change, a theme-input
    /// change, or `set_colours` invalidating the stamp. Render only updates this
    /// presentation memo.
    pub fn ensure_summary_paint(&mut self, theme: &Theme) {
        let signature = theme_signature(theme);
        if self.summary_paint_stamp == Some((self.summary_generation, signature)) {
            return;
        }
        let cols = std::mem::take(&mut self.summary_cols);
        let paint = cols
            .iter()
            .map(|&c| match self.colour_kind(c) {
                Some(ColourKind::Named) => match self.themed_cell_colour(c, theme) {
                    Some(r) => CellPaint {
                        label: Some(r.base),
                        positive: r.positive,
                        negative: r.negative,
                        zero: r.base,
                    },
                    // An unknown name paints as the foreground, as its
                    // cells do.
                    None => CellPaint::plain(theme),
                },
                Some(ColourKind::Sign) => CellPaint {
                    positive: theme.chart_bullish,
                    negative: theme.chart_bearish,
                    ..CellPaint::plain(theme)
                },
                Some(ColourKind::Plain) | None => CellPaint::plain(theme),
            })
            .collect();
        self.summary_cols = cols;
        self.summary_paint = paint;
        self.summary_paint_stamp = Some((self.summary_generation, signature));
    }

    /// The chevron's pointer states for `theme`, re-derived only when one
    /// of the colours they read has moved: the steady path is one
    /// `ControlInputs` build (seven `Hsla` copies) and one compare per
    /// chevron per frame. A blotter cell sits on `table`, the component's
    /// row fill, which is the surface the text is floored against.
    fn chevron_states(&mut self, theme: &Theme) -> control::ControlPaint {
        let inputs = control::ControlInputs::new(
            theme,
            control::Rest::Bare,
            theme.table,
            theme.muted_foreground,
        );
        match &self.chevron {
            Some((have, paint)) if *have == inputs => *paint,
            _ => {
                let paint = control::control_paint(&inputs);
                self.chevron = Some((inputs, paint));
                paint
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
        geode_shell::linenumbers::gutter_px(self.line_numbers, self.shown.len())
    }

    /// Rebuild `numbers` for the cache's current window if anything it
    /// depends on changed — see the field's doc for why this is stamped
    /// rather than rebuilt per call.
    fn ensure_numbers(&mut self) {
        let window = self.cache.window();
        let mode = self.line_numbers;
        let cursor = self.cursor.row;
        // Only relative numbers depend on the cursor; absolute numbers must
        // rebuild only when their window or mode changes.
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

    /// The cursor row's underlying under the applied grouping, or `None`
    /// (see [`crate::core::launch::underlying_at`]).
    pub fn cursor_underlying(&self) -> Option<String> {
        let plan = self.plan.as_ref()?;
        crate::core::launch::underlying_at(&self.cursor_path()?, &plan.grouping)
    }

    /// Start a selection of `kind` at the cursor. With a live selection,
    /// switch kind while keeping the anchor, or clear when the kind matches.
    pub fn start_selection(&mut self, kind: SelectKind) {
        match self.selection.as_ref().map(|s| s.kind) {
            Some(k) if k == kind => self.selection = None,
            Some(_) => {
                if let Some(s) = self.selection.as_mut() {
                    s.kind = kind;
                }
            }
            None => {
                let (Some(path), Some(col)) = (
                    self.cursor_path(),
                    self.plan
                        .as_ref()
                        .and_then(|p| p.columns.get(self.cursor.col))
                        .map(|c| c.name.clone()),
                ) else {
                    return;
                };
                self.anchor_hint = self.cursor.row;
                self.selection = Some(Selection {
                    kind,
                    anchor_row: path,
                    anchor_col: col,
                });
            }
        }
        self.refresh_selection();
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.refresh_selection();
    }

    /// Re-resolve the selection against the current rows and columns and
    /// rebuild the summary — every change point calls this, so render
    /// only ever looks `resolved` and `summary` up. An anchor no longer
    /// shown clears the selection and records which lookup failed in
    /// `selection_lost` for the tile's notice; no neighbouring row is
    /// guessed.
    pub fn refresh_selection(&mut self) {
        let outcome = match (&self.selection, &self.snapshot, &self.plan) {
            (Some(sel), Some(snapshot), Some(plan)) => {
                let hint = self.anchor_hint;
                let shown = &self.shown;
                Some(sel.resolve_with(
                    (self.cursor.row, self.cursor.col),
                    plan.columns.len(),
                    |path| find_by_path(shown, snapshot, plan, path, hint),
                    |name| plan.position_of(name),
                ))
            }
            // No rows to resolve against: the anchor row is not shown.
            (Some(_), _, _) => Some(Err(Lost::Row)),
            (None, _, _) => None,
        };
        let resolved = match outcome {
            Some(Ok(r)) => Some(r),
            Some(Err(lost)) => {
                self.selection = None;
                self.selection_lost = Some(lost);
                None
            }
            None => None,
        };
        if let Some(r) = &resolved {
            // `resolve` orders the range, so the anchor is whichever end
            // the cursor is not on.
            self.anchor_hint = if r.rows.start == self.cursor.row {
                r.rows.end - 1
            } else {
                r.rows.start
            };
        }
        let summaries = match (&resolved, &self.snapshot, &self.plan) {
            (Some(r), Some(snapshot), Some(plan)) => summarize(snapshot, plan, &self.shown, r),
            _ => Vec::new(),
        };
        let refuses = |mark: char| {
            summaries
                .iter()
                .any(|c| c.total.refused && c.total.text.contains(mark))
        };
        self.summary_non_additive = refuses('†');
        self.summary_unsummable = refuses(UNSUMMABLE_MARK);
        self.summary_cols = summaries.iter().map(|c| c.col).collect();
        self.summary = summaries
            .into_iter()
            .map(|c| AggregateCell {
                label: c.label.into(),
                text: c.total.text.into(),
                sign: c.total.sign,
                refused: c.total.refused,
            })
            .collect();
        self.summary_generation = self.summary_generation.wrapping_add(1);
        self.selection_extent = resolved.as_ref().map(|r| {
            let (rows, cols) = (r.rows.len(), r.cols.len());
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            format!("{rows} row{} × {cols} col{}", plural(rows), plural(cols)).into()
        });
        self.resolved = resolved;
    }

    /// Install a snapshot and return whether its column plan was replaced.
    /// Build a fresh plan from the view and snapshot metadata on every
    /// delivery: labels, widths, formats, or colours may change while column
    /// names and indices stay the same. Keep the existing plan when equal.
    ///
    /// Restore cursor identity by row path and column name, prune expansion
    /// to the grouping depth, clear narrowing, and rebuild displayed rows.
    /// If the sorted column disappears, clear the sort and record its name
    /// in `dropped_sort` for the tile's notice.
    pub fn apply_snapshot(
        &mut self,
        snapshot: Arc<Snapshot>,
        view: &ViewSpec,
        grouping: &[String],
    ) -> bool {
        let keep = self.cursor_path();
        // Captured before the reorder for the same reason `move_column`
        // captures it: the cursor is a screen position into
        // `plan.columns`, not a name, and a rebuild that hides a column
        // shifts every later index down. A bare `clamp` afterwards only
        // catches an index that runs off the end — one that stays in
        // range but now names a different column slides the cursor onto
        // it silently, and the next sort orders by whatever the cursor
        // lands on, not what the trader was looking at.
        let under_cursor = self
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(self.cursor.col))
            .map(|c| c.name.clone());
        // The tree column's labels and depths are the grouping's: a width
        // fitted under another grouping measured different text. This is
        // the one door every grouping change (a pin, a slot, the frame)
        // reaches the delegate through. The measures' widths stay.
        if self
            .plan
            .as_ref()
            .is_some_and(|p| p.grouping.as_slice() != grouping)
        {
            self.fitted.remove("");
        }
        let fresh = ColumnPlan::build(view, grouping, &snapshot);
        let rebuild = self.plan.as_ref() != Some(&fresh);
        self.dropped_sort = None;
        if rebuild {
            self.plan = Some(fresh);
            // Re-resolved, not bounds-checked: a rebuild that hides a column
            // or folds a dimension into the tree shortens the list, and an
            // in-range index then names a different column than the trader
            // sorted by. The rows reorder to default order either way, so
            // the dropped name is kept for the tile to report rather than
            // discarded here.
            if let Some(s) = self.sort.take() {
                match self.plan.as_ref().and_then(|p| p.position_of(&s.column)) {
                    Some(_) => self.sort = Some(s),
                    None => self.dropped_sort = Some(s.column),
                }
            }
            // Same re-resolution as the sort above, and for the same
            // reason: a column hidden to the left of the cursor shifts
            // every later index down, so the old index now names a
            // different column even though it is still in range. A
            // cursor whose own column was the one hidden finds no match
            // here and falls through to the ordinary clamp below, same
            // as `move_column` does.
            if let Some(name) = under_cursor
                && let Some(i) = self.plan.as_ref().and_then(|p| p.position_of(&name))
            {
                self.cursor.col = i;
            }
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
            self.refresh_selection();
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
        self.refresh_selection();
    }

    /// Clear formatted cells and glyphs together, then refill the last
    /// requested window, clamped to the rows still shown. An empty result
    /// leaves both caches clear. All format-cache invalidation goes here.
    ///
    /// The table only reports a changed numeric visible range. Sorting,
    /// regrouping, narrowing, or moving columns may leave that range equal,
    /// so waiting for another callback would leave cells blank indefinitely.
    ///
    /// Refill `requested_window`, not the cache's clamped range: the pinned
    /// `TableState::update_visible_range_if_need` does not record ranges of
    /// length zero or one. When rows return, its recorded range may still
    /// match, producing no callback. `fill_window` preserves the original
    /// request so the cache can widen again without a new table event.
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
        self.refresh_selection();
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

    /// Tree text of every un-narrowed visible row, matching the positional
    /// domain accepted by `set_narrowed`. Match against this full list on
    /// every search edit: using the previous subset would misinterpret its
    /// indices and prevent backspace from restoring excluded rows.
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

    /// Record and fill the window requested by `visible_rows_changed`.
    /// Invalidation refills through `fill_window` without changing this
    /// request, even when fewer rows are temporarily available.
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

/// Disclosure glyph for a shown row: `▸`, `▾`, `…`, or `·`. Attribution
/// and semi-join markers are independent. Compute during window fill because
/// `path_of` walks and allocates ancestor strings; paint only reads the result.
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

    /// Wires a cell's or its gutter's mouse-selection gestures
    /// onto `el`: a press (plain, or shift-extending) and, only while
    /// the primary button has stayed down since a press that landed on
    /// a cell or gutter, a drag. `gutter` fixes what kind that press (and
    /// every drag it goes on to arm) means — `true` for the gutter's own
    /// call, `false` for the cell's — and is carried into every `Drag`
    /// this element emits verbatim: the selection a drag makes is decided
    /// by where it started, not by whatever cell the pointer is over now.
    ///
    /// None of the four listeners stop propagation: the row's own
    /// `SelectRow`/tile-focus press must still arrive, and a fast
    /// double-click still reaches gpui's own click-count tracking.
    fn wire_pointer(
        el: Div,
        cx: &Context<TableState<Self>>,
        row_ix: usize,
        col_ix: usize,
        gutter: bool,
    ) -> Div {
        el.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                let d = this.delegate_mut();
                d.drag_last = Some((row_ix, col_ix));
                // The gutter is a child of this cell, so a press on it
                // reaches this handler too, bubbling after the gutter's
                // own — `get_or_insert` keeps that repeat from
                // overwriting the gutter's `true` with this call's
                // `false`; on an ordinary cell press, nothing set it
                // first, so it still lands here.
                d.drag_origin.get_or_insert(gutter);
                cx.emit(CellPointer::Press {
                    row: row_ix,
                    col: col_ix,
                    shift: e.modifiers.shift,
                    gutter,
                });
            }),
        )
        .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
            if e.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let d = this.delegate_mut();
            // No press recorded here means the button came down over
            // something else — a scrollbar thumb, a header column
            // reorder, a tile-split divider, another tile's own text
            // selection — and is only now passing over this cell; such a
            // drag must never start or extend a selection.
            let Some(started_on_gutter) = d.drag_origin else {
                return;
            };
            if d.drag_last == Some((row_ix, col_ix)) {
                return;
            }
            d.drag_last = Some((row_ix, col_ix));
            cx.emit(CellPointer::Drag {
                row: row_ix,
                col: col_ix,
                gutter: started_on_gutter,
            });
        }))
        // A release anywhere ends the drag this cell or gutter may have
        // armed. gpui dispatches every registered mouse listener for
        // every mouse event regardless of hit position — each listener's
        // own hit check (not tree position) decides whether it fires —
        // so between a bubble `on_mouse_up` (release lands here) and a
        // capture `on_mouse_up_out` (release lands anywhere else), this
        // element sees every release exactly once.
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| {
                this.delegate_mut().drag_origin = None;
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| {
                this.delegate_mut().drag_origin = None;
            }),
        )
    }
}

impl BlotterDelegate {
    /// Fit every planned column to its header and the rows in the format
    /// cache's window: the rows the table last asked to see, not the
    /// whole snapshot. Formatting a million rows on the UI thread would
    /// break the 8 ms budget, so rows outside the window are not measured.
    ///
    /// `m` carries the table's own cell padding; this adds the cell's
    /// `px_1`, a sortable header's sort icon, and in the tree column each
    /// row's indent and chevron slot. The gutter is not included:
    /// `column()` adds it to the tree column's width on its own.
    ///
    /// `None` while there is nothing to measure: no plan yet, or no row in
    /// the cache (an empty result).
    pub fn fit_columns(&self, m: &FitMetrics, cx: &App) -> Option<FittedWidths> {
        let plan = self.plan.as_ref()?;
        if self.cache.window().is_empty() {
            return None;
        }
        let m = m.with_extra_padding(0.5 * m.rem_px);
        // `Icon::size_3` (0.75rem) inside the sort toggle's `p(px(2.))`.
        let sort_icon = 0.75 * m.rem_px + 4.0;
        let window = self.cache.window();
        plan.columns
            .iter()
            .enumerate()
            .map(|(col, c)| {
                let tree = c.kind == ColumnKind::Tree;
                let header = self.column(col, cx).name;
                let header = m.text_px(&header) + if tree { 0.0 } else { sort_icon };
                let cells = window.clone().filter_map(|row| {
                    let cell = self.cache.get(row, col)?;
                    let mut w = m.text_px(&cell.text);
                    if cell.attribution == Attribution::DeterminedNonAdditive {
                        w += m.text_px(DETERMINED_MARK) + 0.25 * m.rem_px;
                    }
                    if tree {
                        let depth = self
                            .shown
                            .get(row)
                            .zip(self.snapshot.as_ref())
                            .map_or(0, |(&r, s)| s.tree().depth(r as usize));
                        w += depth as f32 * INDENT + CHEVRON_PX;
                    }
                    Some(w)
                });
                (c.name.clone(), m.fit(std::iter::once(header).chain(cells)))
            })
            .collect::<FittedWidths>()
            .into()
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
        let own_sort = self.sort.as_ref().filter(|s| s.column == c.name);
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
            width: px({
                let width = self.fitted.get(&c.name).copied().unwrap_or(c.width);
                if c.kind == ColumnKind::Tree {
                    width + self.gutter_px()
                } else {
                    width
                }
            }),
            movable: c.kind != ColumnKind::Tree,
            // The tree column stays visible while measures scroll horizontally.
            // `ColumnFixed::Left` uses the component's unscrolled region, and
            // `scroll_to_col` accounts for fixed columns when positioning the cursor.
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
        // Reject tree and missing columns, matching keyboard sorting. The tree
        // header has no sort icon, but the delegate also guards direct calls.
        if self
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(col_ix))
            .is_none_or(|c| c.kind == ColumnKind::Tree)
        {
            return;
        }
        // The component proposes a three-state sort, but measures have a
        // five-state cycle including absolute sorts. Advance from the delegate's
        // state using the column kind, shared with keyboard sorting.
        // Refresh the component's cached arrow and drag-preview header after this
        // entity update returns. Deferred effects run before paint. Restore the
        // component's selected row to the cursor, which `reflatten` keeps by path.
        let name = match self.plan.as_ref().and_then(|p| p.columns.get(col_ix)) {
            Some(c) => c.name.clone(),
            None => return,
        };
        let current = self
            .sort
            .as_ref()
            .filter(|s| s.column == name)
            .map(|s| s.order);
        let next = SortOrder::click_cycle(current, self.is_measure(col_ix));
        self.sort = next.map(|order| SortSpec {
            column: name,
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
        // Captured before the reorder: the sort already names its column
        // rather than a position, so it needs no remap here, but the
        // cursor is a screen position and would otherwise land on
        // whatever slid into its old slot, making the next `s` cycle the
        // sort of a column the trader was not looking at.
        let under_cursor = self
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(self.cursor.col))
            .map(|c| c.name.clone());
        if let Some(p) = self.plan.as_mut() {
            p.move_column(col_ix, to_ix);
        }
        if let Some(name) = under_cursor
            && let Some(i) = self.plan.as_ref().and_then(|p| p.position_of(&name))
        {
            self.cursor.col = i;
        }
        // The component calls this hook without `visible_rows_changed`, so
        // refill immediately. The shared invalidation path also recomputes glyphs;
        // column movement leaves their values unchanged because the tree stays fixed.
        self.invalidate_cells();
        self.refresh_selection();
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

    /// Paint the header label in a named colour's base variant. The component
    /// owns the surrounding padding, borders, sort arrow, and drag handle.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let name = self.column(col_ix, cx).name.clone();
        // Only named columns request theme-derived colour inputs. Conversion is
        // memoized; plain and sign-column headers use the inherited foreground.
        let colour = match self.colour_kind(col_ix) {
            Some(ColourKind::Named) => self.themed_cell_colour(col_ix, cx.theme()),
            _ => None,
        };
        // The base, never a sign variant: a header has no sign.
        div()
            .size_full()
            .when_some(colour, |el, c| el.text_color(c.base))
            .child(name)
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let tint = self
            .resolved
            .as_ref()
            .is_some_and(|r| r.kind == SelectKind::Rows && r.contains_row(row_ix));
        div()
            .id(("row", row_ix))
            .when(tint, |el| el.bg(cx.theme().selection.opacity(0.35)))
            // A press on the row outside every cell — the trailing filler
            // column, row padding — is still a plain click: it
            // reports a press at the cursor's column so the tile's one
            // pointer door clears or extends exactly as a cell press
            // would. The row bubbles after its cells, so a press a cell or
            // the gutter already caught has set `drag_origin` and is not
            // reported twice; this press arms no drag.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    let d = this.delegate_mut();
                    if d.drag_origin.is_some() {
                        return;
                    }
                    let col = d.cursor.col;
                    cx.emit(CellPointer::Press {
                        row: row_ix,
                        col,
                        shift: e.modifiers.shift,
                        gutter: false,
                    });
                }),
            )
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
        let in_block = self
            .resolved
            .as_ref()
            .is_some_and(|r| r.kind == SelectKind::Block && r.contains(row_ix, col_ix));
        let kind = self
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(col_ix))
            .map(|c| c.kind);
        let colour = self.colour_kind(col_ix);
        let el = div()
            .size_full()
            .flex()
            .items_center()
            .px_1()
            .font_family(fonts::MONO)
            // Expose cell geometry to UI tests without formatting a selector in
            // production: GPUI drops this closure when test support is disabled.
            .debug_selector(|| format!("blotter-cell-{row_ix}-{col_ix}"));
        let mut el = Self::wire_pointer(el, cx, row_ix, col_ix, false)
            .when(kind == Some(ColumnKind::Measure), |el| el.justify_end())
            .when(in_block, |el| el.bg(theme.selection.opacity(0.35)))
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
            let chevron_states = self.chevron_states(theme);
            // The gutter precedes indentation and uses the shown row count to size
            // its right-aligned text. It is outside the column plan, so navigation,
            // sort, and yank do not treat it as a column. The cursor row uses full
            // foreground and, in relative mode, its absolute number; other rows use
            // muted text. `ensure_numbers` prepares strings only when its stamp changes.
            if self.line_numbers != LineNumbers::Off {
                let (fg, muted) = (theme.foreground, theme.muted_foreground);
                // Start the gutter flush, matching the no-gutter branch's `pl(indent)`.
                // Keeping the inherited left padding would consume room reserved for
                // text when `column()` widens the tree column.
                el = el.pl(px(0.));
                self.ensure_numbers();
                let text = row_ix
                    .checked_sub(self.cache.window().start)
                    .and_then(|i| self.numbers.get(i))
                    .cloned()
                    .unwrap_or_default();
                let on_cursor_row = self.cursor.row == row_ix;
                // A gutter press means "rows" the way a cell's own means
                // "block" — otherwise wired through the same door as the
                // cell (`wire_pointer`, `gutter: true`).
                let gutter_el = div()
                    .flex()
                    .flex_shrink_0()
                    .justify_end()
                    .w(px(self.gutter_px()))
                    .pr(px(GUTTER_GAP_PX))
                    .mr(indent)
                    .text_color(if on_cursor_row { fg } else { muted })
                    .debug_selector(|| format!("blotter-gutter-{row_ix}"));
                el = el.child(Self::wire_pointer(gutter_el, cx, row_ix, col_ix, true).child(text));
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
                    .rounded(theme.radius_tokens().sm)
                    .text_color(theme.muted_foreground)
                    // The one clickable glyph in a blotter cell takes a
                    // bare control's pointer states (the design guide's
                    // hover and pressed rows; cursor stays the arrow),
                    // memoised per theme, not derived per row.
                    .pointer_states(chevron_states)
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
        // The unanimity marker is not a value: muted like the other
        // not-a-plain-value text in the grid, and never colored as one.
        if cell.mixed {
            return el.text_color(theme.muted_foreground).child(text);
        }
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
                    // Named colours supply their own sign variants; `sign` colouring is a
                    // separate setting. Zero and nonnumeric cells use the named base, and an
                    // undefined name falls back to the foreground. Theme conversion is lazy
                    // and memoized behind the full signature, so repeated cells only look
                    // up the prepared colour.
                    (Some(ColourKind::Named), sign) => el.text_color(
                        self.themed_cell_colour(col_ix, theme)
                            .map_or(theme.foreground, |c| c.for_sign(sign)),
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
    use geode_shell::linenumbers::GUTTER_DIGIT_PX;

    fn dim(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 4],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
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

    /// A root with `n - 1` direct children in a single-level grouping. All
    /// rows are visible without explicit expansion because the root stays
    /// open. `n == 1` represents a table reduced to its root.
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

    /// Use ASCII escapes for expected disclosure and attribution code points.
    /// Encoding damage to source glyphs must fail the test even if a text
    /// conversion also changes every non-ASCII expectation in this file.
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

    /// The root is always in `shown` (`flatten` pushes every root before
    /// descending), so the grand-total row is reachable by the cursor too.
    #[test]
    fn the_cursor_underlying_follows_the_cursor_row() {
        let mut d = BlotterDelegate::new();
        d.apply_snapshot(snapshot(), &view(), &grouping());
        d.expansion
            .toggle(path_of(&snapshot(), d.plan.as_ref().unwrap(), 1));
        d.reflatten();
        let at = |d: &BlotterDelegate, snap_row: u32| {
            d.shown.iter().position(|r| *r == snap_row).unwrap()
        };
        d.cursor.row = at(&d, 1);
        assert_eq!(
            d.cursor_underlying(),
            None,
            "L1 is above the underlying level"
        );
        d.cursor.row = at(&d, 3);
        assert_eq!(d.cursor_underlying(), Some("SPX".into()));
        d.cursor.row = at(&d, 4);
        assert_eq!(d.cursor_underlying(), Some("NDX".into()));
        d.cursor.row = at(&d, 0);
        assert_eq!(d.cursor_underlying(), None, "the grand total");
    }

    /// The pinned table does not record visible ranges of length zero or
    /// one. After the row count recovers, its recorded range can match again
    /// without firing a callback. `apply_snapshot` must refill the complete
    /// last request itself; only the initial call here simulates a table
    /// visible-range notification.
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

    /// Off mode paints no numbers and reserves no width. Absolute mode uses
    /// one-based shown indices. Relative mode uses cursor distance except at
    /// the cursor, where it shows the absolute index. Cursor moves update
    /// relative numbers without requiring a format-cache refill.
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
        // Clearing narrowing must replace cached subset values immediately,
        // without waiting for the table to report a different numeric range.
        assert_eq!(
            d.cache.get(1, 0).map(|c| c.text.to_string()),
            Some("L1".to_string()),
            "the window was refilled immediately against the widened shown list"
        );
    }

    #[test]
    fn narrowing_uses_positions_into_visible_not_row_ids_even_when_they_differ() {
        // `set_narrowed` accepts positions into `visible`, not snapshot row IDs.
        // This fixture places L2 at different indices in those two domains so an
        // incorrect direct row-ID lookup cannot pass.
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
        // A snapshot replacement must invalidate and refill its own cached
        // values. Changing the grand total distinguishes a fresh refill from
        // both stale content and an empty cache, without calling `set_narrowed`.
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
        // An unchanged numeric window produces no table callback. Reapplying
        // the snapshot must therefore restore cell text and disclosure glyphs
        // immediately, with no intervening `refill_window` call.
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
        // When the requested window starts past the new shown-row count, no
        // refill can replace its glyphs. Invalidation must clear them explicitly.
        // Read `glyph_at` directly because the table would not paint these rows.
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
                        summable: false,
                        mixed_flag: None,
                    },
                    TestColumn::Dict(vec![None, s("L1"), s("SPX"), s("SPX"), s("SPX"), s("SPX")]),
                ),
                (
                    ColumnMeta {
                        name: "underlying_ref".into(),
                        attribution_by_depth: vec![Attribution::Additive; 4],
                        scope_semantics: ScopeSemantics::Direct,
                        summable: false,
                        mixed_flag: None,
                    },
                    TestColumn::Dict(vec![None, None, s("A"), s("B"), s("C"), s("D")]),
                ),
                (
                    ColumnMeta {
                        name: "row_depth".into(),
                        attribution_by_depth: vec![Attribution::Additive; 4],
                        scope_semantics: ScopeSemantics::Direct,
                        summable: false,
                        mixed_flag: None,
                    },
                    TestColumn::I32(vec![0, 1, 2, 2, 2, 2]),
                ),
                (
                    ColumnMeta {
                        name: "delta01".into(),
                        attribution_by_depth: (0..4).map(determined).collect(),
                        scope_semantics: ScopeSemantics::Direct,
                        summable: false,
                        mixed_flag: None,
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

    /// The component calls `move_column` without a visible-range callback.
    /// The hook must refill the current window immediately so reordered
    /// columns paint their new values before any subsequent scroll.
    #[gpui::test]
    fn a_header_column_move_keeps_a_block_on_its_column_names(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let view_text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
             [[t.columns]]\nname = \"a\"\n[[t.columns]]\nname = \"b\"\n\
             [[t.columns]]\nname = \"c\"\n[[t.columns]]\nname = \"d\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", view_text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let f = || TestColumn::F64(vec![Some(1.0), Some(2.0)]);
        let snap = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1])),
                (dim("a"), f()),
                (dim("b"), f()),
                (dim("c"), f()),
                (dim("d"), f()),
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
        let names = |d: &BlotterDelegate| -> Vec<String> {
            let plan = d.plan.as_ref().unwrap();
            d.resolved
                .as_ref()
                .unwrap()
                .cols
                .clone()
                .map(|c| plan.columns[c].name.clone())
                .collect()
        };
        table.update_in(&mut vcx, |t, window, cx| {
            let d = t.delegate_mut();
            d.apply_snapshot(snap, &view, &["lhu".to_string()]);
            // Plan: [tree, a, b, c, d]. Anchor on `a`, cursor on `b`.
            d.cursor.col = 1;
            d.start_selection(SelectKind::Block);
            d.cursor.col = 2;
            d.refresh_selection();
            assert_eq!(names(d), ["a", "b"], "fixture");

            // `d`, outside the block, moves to its left edge.
            d.move_column(4, 1, window, cx);
            assert_eq!(names(t.delegate()), ["a", "b"], "an outside move");

            // The cursor's own column `b` moves to the far right: the
            // block still runs from the anchor `a` to the cursor on `b`.
            let d = t.delegate_mut();
            let from = d.plan.as_ref().unwrap().position_of("b").unwrap();
            d.move_column(from, 4, window, cx);
            let got = names(t.delegate());
            assert_eq!(
                (
                    got.first().map(String::as_str),
                    got.last().map(String::as_str)
                ),
                (Some("a"), Some("b")),
                "the block's edges stay on the anchor's and the cursor's columns: {got:?}"
            );
        });
    }

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

        // Assert within the update that moves the column. Flushing effects
        // between updates can trigger the window's first layout and refill the
        // cache independently, masking a missing refill in `move_column`.
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

    /// A drag reorders `ColumnPlan::columns`; `Cursor.col` stays a
    /// position (it drives the on-screen highlight), so the hook has to
    /// re-derive it by name across the move or the highlight lands on
    /// whatever slid into the old slot, and the next `s` would cycle the
    /// sort of a column the trader was not looking at.
    #[gpui::test]
    fn the_cursor_follows_its_column_across_a_move(cx: &mut gpui::TestAppContext) {
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

        table.update_in(&mut vcx, |t, window, cx| {
            let d = t.delegate_mut();
            d.apply_snapshot(snap, &view, &["lhu".to_string()]);
            let from = d.plan.as_ref().unwrap().position_of("delta01").unwrap();
            d.cursor.col = from;
            let name_before = d.plan.as_ref().unwrap().columns[d.cursor.col].name.clone();

            d.move_column(from, from + 1, window, cx);

            let name_after = d.plan.as_ref().unwrap().columns[d.cursor.col].name.clone();
            assert_eq!(
                name_before, name_after,
                "the cursor rests on the column it rested on, not the position"
            );
            assert_eq!(
                d.cursor.col,
                from + 1,
                "the cursor's position moved to the column's new slot"
            );
        });
    }

    /// Named colours use the theme's anchors and tokens for both cells and
    /// headers. Plain/sign settings and unknown names return no named colour;
    /// their paint sites choose the appropriate fallback.
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
        colours.insert("delta".into(), Definition::token(Token::Foreground));

        // Columns 1..3: a named colour, `sign`, and a name the doc lacks.
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                    [[t.columns]]\nname = \"delta01\"\nformat = { color = \"delta\" }\n\
                    [[t.columns]]\nname = \"gamma01\"\nformat = { color = \"sign\" }\n\
                    [[t.columns]]\nname = \"vega01\"\nformat = { color = \"ghost\" }\n";
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
            Some(ColourResolved::plain(geode_shell::shell::colours::to_hsla(
                red
            ))),
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

    /// A `tint_sign` colour: `cell_colour` hands back the triad — the
    /// three the cache resolved together — and `render_td`'s named arm
    /// picks by the cell's own sign through `ColourResolved::for_sign`, while
    /// the header takes the base. The cell signs come from the cache the
    /// same way the `sign` arm reads them.
    #[test]
    fn a_tint_sign_column_resolves_a_variant_per_sign() {
        let grey = Rgb {
            r: 0.5,
            g: 0.5,
            b: 0.5,
        };
        let green = Rgb {
            r: 0.2,
            g: 0.7,
            b: 0.3,
        };
        let anchors = Anchors {
            normal: [grey; 6],
            light: [grey; 6],
        };
        let tokens = Tokens {
            foreground: green,
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
            background: Rgb {
                r: 0.05,
                g: 0.05,
                b: 0.05,
            },
        };
        let mut colours = NamedColours::default();
        colours.insert(
            "delta".into(),
            Definition::token(Token::Foreground).tinted(),
        );
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                    [[t.columns]]\nname = \"delta01\"\nformat = { color = \"delta\" }\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let snapshot = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1"), s("L2")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![Some(0.0), Some(5.0), Some(-5.0)]),
                ),
            ],
            1,
        ));
        let mut d = BlotterDelegate::new();
        d.set_colours(Arc::new(colours));
        d.apply_snapshot(snapshot, &view, &["lhu".to_string()]);
        d.refill_window(0..3);

        let resolved = d.cell_colour(1, &anchors, &tokens).unwrap();
        let expect = |sign| {
            geode_shell::shell::colours::to_hsla(geode_core::colour::resolve_signed(
                &Definition::token(Token::Foreground).tinted(),
                sign,
                &anchors,
                &tokens,
            ))
        };
        assert_eq!(resolved.base, expect(Sign::Zero));
        assert_eq!(resolved.positive, expect(Sign::Positive));
        assert_eq!(resolved.negative, expect(Sign::Negative));
        // The cells' signs, as the painter reads them off the cache.
        let sign_at = |d: &BlotterDelegate, row: usize| d.cache.get(row, 1).unwrap().sign;
        assert_eq!(sign_at(&d, 0), Some(Sign::Zero));
        assert_eq!(sign_at(&d, 1), Some(Sign::Positive));
        assert_eq!(sign_at(&d, 2), Some(Sign::Negative));
        assert_eq!(resolved.for_sign(sign_at(&d, 1)), resolved.positive);
        assert_eq!(resolved.for_sign(sign_at(&d, 2)), resolved.negative);
        assert_eq!(resolved.for_sign(sign_at(&d, 0)), resolved.base);
    }

    /// Rows 1..=2 of a one-measure view (`L1` +5, `L2` −5) selected, so
    /// the footer carries one group for `delta01` at plan column 1.
    fn summary_fixture(view_text: &str, colours: NamedColours) -> BlotterDelegate {
        let doc = merge_docs("views", &[LayerDoc::builtin("views", view_text).unwrap()]);
        let view = ViewSpec::from_doc(&doc).0.remove(0);
        let snapshot = Arc::new(Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Dict(vec![None, s("L1"), s("L2")])),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (
                    dim("delta01"),
                    TestColumn::F64(vec![Some(0.0), Some(5.0), Some(-5.0)]),
                ),
            ],
            1,
        ));
        let mut d = BlotterDelegate::new();
        d.set_colours(Arc::new(colours));
        d.apply_snapshot(snapshot, &view, &["lhu".to_string()]);
        d.cursor.row = 1;
        d.start_selection(SelectKind::Rows);
        d.cursor.row = 2;
        d.refresh_selection();
        assert_eq!(d.summary.len(), 1, "one measure group");
        d
    }

    /// The footer group of a named-color column takes the header's color
    /// for its label and the cells' sign variants for its totals.
    #[test]
    fn a_named_column_paints_its_footer_group_like_its_header_and_cells() {
        let mut colours = NamedColours::default();
        colours.insert("delta".into(), Definition::token(Token::Info).tinted());
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                    [[t.columns]]\nname = \"delta01\"\nformat = { color = \"delta\" }\n";
        let mut d = summary_fixture(text, colours);
        let theme = Theme::default();
        d.ensure_summary_paint(&theme);
        let resolved = d.themed_cell_colour(1, &theme).expect("a named column");
        let paint = d.summary_paint[0];
        assert_eq!(paint.label, Some(resolved.base), "the header's color");
        assert_eq!(paint.positive, resolved.positive);
        assert_eq!(paint.negative, resolved.negative);
        assert_eq!(paint.zero, resolved.base);
    }

    /// A colors.toml reload (a new definitions `Arc`) repaints the footer
    /// even when the summary and the theme are unchanged.
    #[test]
    fn a_colors_reload_repaints_the_footer() {
        let colours = |token| {
            let mut c = NamedColours::default();
            c.insert("delta".into(), Definition::token(token));
            c
        };
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                    [[t.columns]]\nname = \"delta01\"\nformat = { color = \"delta\" }\n";
        let mut d = summary_fixture(text, colours(Token::Info));
        // Distinct tokens: the default theme's colors can coincide, which
        // would let a stale paint pass.
        let theme = Theme {
            colors: gpui_component::ThemeColor {
                info: gpui::green(),
                danger: gpui::red(),
                ..Theme::default().colors
            },
            ..Theme::default()
        };
        d.ensure_summary_paint(&theme);
        let before = d.summary_paint[0].label;
        d.set_colours(Arc::new(colours(Token::Danger)));
        d.ensure_summary_paint(&theme);
        let now = d.themed_cell_colour(1, &theme).expect("a named column");
        assert_ne!(before, Some(now.base), "the fixture needs two colors");
        assert_eq!(
            d.summary_paint[0].label,
            Some(now.base),
            "the reloaded color"
        );
    }

    /// A `sign` column's totals take the cells' bullish/bearish; its label
    /// stays muted, as its header is uncolored. The memo rebuilds only
    /// when the summary does.
    #[test]
    fn a_sign_column_paints_its_totals_by_sign_and_the_memo_follows_the_summary() {
        let text = "[t]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                    [[t.columns]]\nname = \"delta01\"\nformat = { color = \"sign\" }\n";
        let mut d = summary_fixture(text, NamedColours::default());
        // Distinct poles: the default theme's bullish and bearish can
        // coincide, which would let a crossed pair pass.
        let theme = Theme {
            colors: gpui_component::ThemeColor {
                chart_bullish: gpui::green(),
                chart_bearish: gpui::red(),
                ..Theme::default().colors
            },
            ..Theme::default()
        };
        d.ensure_summary_paint(&theme);
        let paint = d.summary_paint[0];
        assert_eq!(paint.label, None);
        assert_eq!(
            (paint.positive, paint.negative, paint.zero),
            (theme.chart_bullish, theme.chart_bearish, theme.foreground)
        );
        // Poison the memo: an unchanged summary and theme must not rebuild.
        d.summary_paint.clear();
        d.ensure_summary_paint(&theme);
        assert!(
            d.summary_paint.is_empty(),
            "a steady footer resolves nothing"
        );
        d.refresh_selection();
        d.ensure_summary_paint(&theme);
        assert_eq!(d.summary_paint.len(), 1, "a rebuilt summary repaints");
    }

    /// Theme inputs use all 28 colour values as their memo key. Poisoning the
    /// cached result exposes unwanted recomputation under an unchanged theme;
    /// ordinary value equality would hide it. Changing a red anchor then
    /// proves invalidation does not depend only on foreground/background.
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

    /// Presentation edits must replace the column plan even when the next
    /// snapshot has the same column names and indices. An unchanged view
    /// keeps the existing plan; changed labels, widths, colours, or hidden
    /// columns take effect on redelivery.
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
                "[t.columns.delta01]\nlabel = \"Δ\"\nwidth = 90\ncolor = \"delta\"\n",
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
