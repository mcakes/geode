//! The command palette (Task 6, spec §3.2/§3.3): a universal, fuzzy-filtered
//! list over every registered action and every bundled theme, driven by the
//! keyboard (`ctrl+k` / `ctrl+shift+p` open, Esc closes; typing filters;
//! up/down or ctrl+p/ctrl+n move the selection; Enter dispatches) and, since
//! the palette-input-polish task, the mouse (click a row to select it, click
//! outside the panel to dismiss — see `render`'s doc comment).
//!
//! Everything above the `render` section is the pure core: `PaletteItem`,
//! `PaletteState`, `fuzzy_match`, and the keystroke-rendering/index-building
//! helpers never import `gpui` and are fully unit-tested without a window
//! (plan constraint: "the fuzzy filter and PaletteState are PURE and TDD'd
//! — no deps, no gpui"). `render`, at the bottom, is the only part that
//! touches `gpui`/`gpui_component`; `ShellView` (`shell::mod`) owns the
//! `PaletteState` and drives it from real key events.
//!
//! **Query ownership** (palette-input-polish task): the query text itself is
//! no longer part of the free-typing key handling this module used to do
//! (`push_char`/`backspace`, both removed). `ShellView` now owns a real
//! gpui-component `Entity<InputState>` for the query field (mirroring the
//! toolbar's `filter_input`) and feeds `PaletteState::set_query` from an
//! `InputEvent::Change` subscription — see that struct's and method's own
//! doc comments, and `shell::mod`'s doc comment on `palette_input`, for the
//! full routing story (why up/down/ctrl+p/ctrl+n/enter/escape still reach
//! `ShellView::handle_palette_key` as bubbled `KeyDownEvent`s while
//! printable/caret/ctrl+a/ctrl+v are consumed natively by the `Input`).

use std::collections::BTreeMap;

use crate::actions::{ActionId, ActionRegistry};
use crate::keymap::{Keymap, Keystroke};
use crate::theme::ThemeService;

/// Palette-facing category for theme rows (brief: themes appear as
/// `"Theme: {name}"` under category `"Appearance"`).
const THEME_CATEGORY: &str = "Appearance";

/// One row the palette can show. `Action` carries the id (for dispatch),
/// its registry title/category, and its rendered binding text if the
/// keymap has one bound. `Theme`'s `String` is a fully qualified bundled
/// theme name (e.g. `"Gruvbox Dark"`) — exactly what
/// `ThemeService::apply`/`resolve` expect, so dispatch needs no further
/// lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteItem {
    Action(ActionId, String, String, Option<String>),
    Theme(String),
}

impl PaletteItem {
    /// Display/match title: the action's own title, or `"Theme: {name}"`.
    pub fn title(&self) -> String {
        match self {
            PaletteItem::Action(_, title, _, _) => title.clone(),
            PaletteItem::Theme(name) => format!("Theme: {name}"),
        }
    }

    pub fn category(&self) -> &str {
        match self {
            PaletteItem::Action(_, _, category, _) => category,
            PaletteItem::Theme(_) => THEME_CATEGORY,
        }
    }

    /// Rendered keybinding text (e.g. `"alt+p"`), if any — always `None`
    /// for a theme row, since themes are only ever reached by selecting
    /// them in the palette itself.
    pub fn binding(&self) -> Option<&str> {
        match self {
            PaletteItem::Action(_, _, _, binding) => binding.as_deref(),
            PaletteItem::Theme(_) => None,
        }
    }
}

/// Case-insensitive subsequence match: every character of `query`, in
/// order, must occur somewhere in `candidate` (skipping characters as
/// needed). `None` means `query` is not a subsequence of `candidate` at
/// all; otherwise the result is `(score, indices)` where higher `score`
/// means a better match and `indices` are the **char** (not byte) offsets
/// into `candidate` that matched, one per query character, in increasing
/// order — `render`'s highlighting turns them into styled spans over the
/// row title (see [`highlight_runs`]).
///
/// Matching is greedy-leftmost (each query character claims the earliest
/// remaining occurrence in `candidate`), and the score rewards, per
/// matched character: a match at position 0 (prefix start), a match right
/// after a separator (`' '`, `':'`, `'_'`, `'-'` — a "word start"), and
/// membership in a run of consecutive matched characters (the run bonus
/// grows with run length, so longer unbroken runs score more than the same
/// number of scattered hits).
///
/// The indices are positions in `candidate.to_lowercase().chars()`. For
/// every candidate this palette ever renders (plain-ASCII action titles
/// and `"Theme: {name}"` rows) lowercasing never changes the char count, so
/// those positions apply equally to the original-case `candidate` — a
/// property `render` relies on rather than re-deriving.
///
/// An empty query matches everything with score 0 and no matched indices —
/// see [`PaletteState::filtered`] for why that specific score value
/// matters.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<(u32, Vec<usize>)> {
    fuzzy_match_lowered(&query.to_lowercase(), &candidate.to_lowercase())
}

/// [`fuzzy_match`]'s core over ALREADY-lowercased inputs (post-merge
/// review perf 11): [`PaletteState`] stores each item's lowered match
/// text once at construction and lowercases the query once per edit, so
/// the per-item work in a filter pass is just this subsequence walk —
/// no per-item `title()` clone, no per-item `to_lowercase`. The public
/// wrapper above keeps the original lowercase-both contract for callers
/// (and tests) holding raw strings.
fn fuzzy_match_lowered(query: &str, candidate: &str) -> Option<(u32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }

    let cand: Vec<char> = candidate.chars().collect();
    let mut cand_idx = 0usize;
    let mut prev_matched: Option<usize> = None;
    let mut consecutive_run: u32 = 0;
    let mut score: u32 = 0;
    let mut indices: Vec<usize> = Vec::new();

    for qc in query.chars() {
        let offset = cand[cand_idx..].iter().position(|&c| c == qc)?;
        let idx = cand_idx + offset;

        let mut char_score: u32 = 1;
        if idx == 0 {
            char_score += 10;
        }
        if idx > 0 && matches!(cand[idx - 1], ' ' | ':' | '_' | '-') {
            char_score += 8;
        }
        if prev_matched.is_some_and(|p| p + 1 == idx) {
            consecutive_run += 1;
            char_score += 5 + consecutive_run;
        } else {
            consecutive_run = 0;
        }

        score += char_score;
        indices.push(idx);
        prev_matched = Some(idx);
        cand_idx = idx + 1;
    }

    Some((score, indices))
}

/// Merge [`fuzzy_match`]'s matched char indices into contiguous runs and
/// convert each run to a byte [`Range`](std::ops::Range) over `title`,
/// suitable for `gpui::StyledText::with_highlights`. Adjacent indices
/// (`i`, `i+1`, `i+2`, ...) collapse into one range rather than one per
/// character, so a prefix match like `"app"` against `"Apple Pie"`
/// highlights `"App"` as a single run instead of three abutting ones —
/// cheaper to build and (for a bold weight) visually identical either way.
///
/// Empty `indices` (an empty query, per [`fuzzy_match`]'s doc) yields no
/// runs at all.
///
/// `pub(crate)` since the filter-first dialog UX:
/// `keybindings_view::highlighted_text` paints its rows' fuzzy matches
/// through this same conversion rather than growing a second copy — and
/// it leans on the out-of-range guard below deliberately, since it feeds
/// one row's indices to two separate label lines (title, then category)
/// and each pass must simply skip the other line's.
///
/// **Out-of-range guard** (fix-round nit): the indices are positions in
/// the *lowered* match text while `boundaries` comes from the
/// original-case `title`, and `str::to_lowercase` is not always
/// char-count-preserving — 'İ' (U+0130, dotted capital I) lowercases to
/// the two chars `"i\u{307}"`, so a title containing it produces match
/// indices past its own last char. Every title this palette renders
/// today is plain ASCII (action titles and `"Theme: {name}"` rows over
/// the bundled theme names), so this is unreachable in practice — but
/// this function runs on the render path, where an out-of-bounds
/// `boundaries[..]` would be a panic mid-frame rather than a wrong
/// highlight, and free-form theme names would be all it takes. Indices
/// that fall outside the title are therefore skipped (the in-range
/// characters still highlight normally) instead of indexing.
pub(crate) fn highlight_runs(title: &str, indices: &[usize]) -> Vec<std::ops::Range<usize>> {
    if indices.is_empty() {
        return Vec::new();
    }

    // char index -> byte offset for every char boundary in `title`, plus
    // one trailing sentinel for "one past the last char" so the final run
    // can close out at `title.len()` without a special case.
    let boundaries: Vec<usize> = title
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(title.len()))
        .collect();
    // `boundaries` holds one entry per char plus the sentinel, so this is
    // the title's char count — the exclusive upper bound on a usable
    // index (see the guard note above).
    let char_count = boundaries.len() - 1;

    let mut runs = Vec::new();
    let mut run: Option<(usize, usize)> = None;
    for &idx in indices {
        if idx >= char_count {
            continue;
        }
        run = match run {
            Some((start, end)) if idx == end + 1 => Some((start, idx)),
            Some((start, end)) => {
                runs.push(boundaries[start]..boundaries[end + 1]);
                Some((idx, idx))
            }
            None => Some((idx, idx)),
        };
    }
    if let Some((start, end)) = run {
        runs.push(boundaries[start]..boundaries[end + 1]);
    }
    runs
}

/// Fuzzy-filtered, keyboard-navigable palette state. Pure: no `gpui`, no
/// I/O. `ShellView` builds one fresh (via [`build_items`]) each time the
/// palette opens and drops it on close — nothing here is per-frame state.
///
/// **Filter caching** (post-merge review perf 11): the filter result is
/// computed once per query edit (`recompute_filtered`, from `new` and
/// `set_query`) and cached in `filtered`; [`PaletteState::filtered`] and
/// every selection accessor read the cache without re-matching — before
/// this, each call re-fuzzy-matched the whole list, so a single Enter
/// press computed the same filter three times (`selected_item`, then the
/// close, then the next render). `items` cannot change while a
/// `PaletteState` lives, so the query is the ONLY invalidation key —
/// verified, not assumed: the sole construction site is
/// `ShellView::toggle_palette` (fresh from the post-init-fixed
/// `ActionRegistry` and the fixed bundled-theme list), and the one
/// reload path that could change palette inputs (`apply_reload`'s
/// keymap-docs-changed branch) CLOSES the palette (`self.palette =
/// None`) rather than mutating an open one.
pub struct PaletteState {
    items: Vec<PaletteItem>,
    /// Each item's lowercased match text, built once at construction so
    /// a filter pass does no per-item `title()` clone or `to_lowercase`
    /// (perf 11 — see [`fuzzy_match_lowered`]).
    lowered: Vec<String>,
    query: String,
    selected: usize,
    /// The cached filter result: index into `items` plus the matched
    /// char indices, best match first (ties in `items` order — the sort
    /// is stable). Indices, not `&PaletteItem` borrows, so the cache can
    /// live beside the items it points into.
    filtered: Vec<(usize, Vec<usize>)>,
    /// Test-only honesty counter for the no-re-match guarantee: bumped
    /// at the real `fuzzy_match_lowered` call site in
    /// `recompute_filtered`, per instance (a `Cell` field, not a global
    /// static, so parallel tests can't race it).
    #[cfg(test)]
    match_calls: std::cell::Cell<usize>,
}

impl PaletteState {
    pub fn new(items: Vec<PaletteItem>) -> Self {
        let lowered = items
            .iter()
            .map(|item| item.title().to_lowercase())
            .collect();
        let mut state = PaletteState {
            items,
            lowered,
            query: String::new(),
            selected: 0,
            filtered: Vec::new(),
            #[cfg(test)]
            match_calls: std::cell::Cell::new(0),
        };
        state.recompute_filtered();
        state
    }

    /// Recompute the cached filter result for the current query — the
    /// one place matching happens (called from `new` and `set_query`
    /// only; see the struct doc for why those are the only two
    /// invalidation points).
    fn recompute_filtered(&mut self) {
        let query = self.query.to_lowercase();
        let mut scored: Vec<(usize, u32, Vec<usize>)> = Vec::new();
        for (i, lowered) in self.lowered.iter().enumerate() {
            #[cfg(test)]
            self.match_calls.set(self.match_calls.get() + 1);
            if let Some((score, indices)) = fuzzy_match_lowered(&query, lowered) {
                scored.push((i, score, indices));
            }
        }
        // Stable sort: ties — including an empty query, where every item
        // scores the same 0 — keep their original `items` order (the
        // brief's "empty query returns all in registry order").
        scored.sort_by_key(|(_, score, _)| std::cmp::Reverse(*score));
        self.filtered = scored
            .into_iter()
            .map(|(i, _, indices)| (i, indices))
            .collect();
    }

    #[cfg(test)]
    fn match_call_count(&self) -> usize {
        self.match_calls.get()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Replace the whole query in one step and reset the selection to the
    /// top match — the palette-input-polish task moved character-at-a-time
    /// editing (the removed `push_char`/`backspace`) into a real
    /// gpui-component `Input`; this is what feeds that `Entity<InputState>`'s
    /// `InputEvent::Change` value back into the pure filter/selection core
    /// (`ShellView`'s subscription, set up once in `new`, calls this on
    /// every edit). Filtering can shrink or reorder the result list out from
    /// under a selection further down it, so every query edit snaps the
    /// selection back to a row that's guaranteed to still exist — same
    /// reasoning `push_char`/`backspace` used to document, just triggered by
    /// a whole-string replace instead of one character at a time.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
        self.recompute_filtered();
    }

    /// Set the selection to an absolute row index — a mouse click on a
    /// result row (`render`'s per-row `on_mouse_down`), which names exactly
    /// which row was hit rather than a `±1` step the way keyboard nav does
    /// (`move_selection`). Defensively clamped the same way that method is:
    /// an empty filtered list leaves `selected` at 0 untouched, and an
    /// index past the end of the current filtered list (stale by the time a
    /// click is actually processed — e.g. the query changed between the
    /// frame that painted the row and the click landing) clamps to the last
    /// row rather than panicking or silently going out of range.
    pub fn set_selected(&mut self, index: usize) {
        let len = self.filtered.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = index.min(len - 1);
    }

    /// Every item whose title fuzzy-matches the current query, best match
    /// first, paired with the matched char indices `render` highlights.
    /// Ties — including an empty query, where every item scores the same
    /// 0 — keep their original `items` order (see `recompute_filtered`).
    /// A cache read (perf 11 — see the struct doc): no matching happens
    /// here, only a walk of the stored result. The signature still
    /// returns owned index `Vec`s (a handful of small clones) rather
    /// than borrows purely to keep the pre-cache API shape; the cost
    /// this existed to kill — the full fuzzy re-match per call — is
    /// gone.
    pub fn filtered(&self) -> Vec<(&PaletteItem, Vec<usize>)> {
        self.filtered
            .iter()
            .map(|(i, indices)| (&self.items[*i], indices.clone()))
            .collect()
    }

    /// Move the selection by `delta` rows (arrow keys / ctrl+p / ctrl+n
    /// pass ±1), wrapping at both ends — pressing up at index 0 selects the
    /// LAST filtered item; pressing down at the last item wraps to 0. An
    /// empty result list remains a no-op. `render` now draws every filtered
    /// row inside a scrollable viewport (rather than truncating to a fixed
    /// window), and the caller that drives real key events
    /// (`ShellView::handle_palette_key`) is responsible for scrolling the
    /// newly selected row into view after each call here — see
    /// `gpui::ScrollHandle::scroll_to_item`, invoked from that same
    /// selection-change path; scroll-follow handles any index, including
    /// wrap-around jumps.
    pub fn move_selection(&mut self, delta: i32) {
        let len = self.filtered.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        // Wrapping modular arithmetic: safely handles negative deltas and
        // out-of-bounds movement. Formula: ((current + delta) % len + len) % len
        // The double-modulo ensures the result is always in [0, len).
        let next = ((self.selected as i32 + delta) % len as i32 + len as i32) % len as i32;
        self.selected = next as usize;
    }

    /// The currently selected row, if any (an empty filtered list, or a
    /// `selected` index somehow past its end, both yield `None` rather than
    /// panicking). Returns an owned clone so callers can dispatch it after
    /// dropping the palette state (e.g. closing the palette first).
    pub fn selected_item(&self) -> Option<PaletteItem> {
        self.filtered
            .get(self.selected)
            .map(|(i, _)| self.items[*i].clone())
    }
}

/// Render one keystroke back to display text: `mods+key`, modifiers in a
/// fixed ctrl/alt/shift/cmd order, `+`-joined (e.g. `"ctrl+shift+p"`).
/// Deliberately a standalone copy of `shell::status`'s pending-keystroke
/// formatting rather than a shared call into it: that module pulls in
/// `gpui`/`gpui_component`, and this module's pure core must not. `pub(
/// crate)` so `shell::whichkey`'s pure core — equally gpui-free — can reuse
/// it rather than adding a third copy.
pub(crate) fn render_keystroke(ks: &Keystroke) -> String {
    let mut parts = Vec::new();
    if ks.mods.ctrl {
        parts.push("ctrl");
    }
    if ks.mods.alt {
        parts.push("alt");
    }
    if ks.mods.shift {
        parts.push("shift");
    }
    if ks.mods.cmd {
        parts.push("cmd");
    }
    parts.push(ks.key.as_str());
    parts.join("+")
}

/// Render a full binding — one keystroke or a sequence — back to display
/// text, sequence parts space-joined (e.g. a `"g g"` binding renders as
/// `"g g"`).
pub fn render_binding(keystrokes: &[Keystroke]) -> String {
    keystrokes
        .iter()
        .map(render_keystroke)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the `ActionId -> rendered binding` reverse index the palette shows
/// next to each action. Built once per `Keymap::bindings()` call at
/// palette-open time (brief: "not per frame"), not recomputed per render.
/// When an action has more than one binding (e.g. `palette::toggle`'s
/// `ctrl+k` and `ctrl+shift+p`), the first one encountered in
/// `Keymap::bindings()`'s own (layer-then-declaration) order wins — the
/// earliest-declared binding is treated as the "primary" one for display.
pub fn build_binding_index(keymap: &Keymap) -> BTreeMap<ActionId, String> {
    let mut index = BTreeMap::new();
    for binding in keymap.bindings() {
        index
            .entry(binding.action.clone())
            .or_insert_with(|| render_binding(&binding.keystrokes));
    }
    index
}

/// Build the full palette item list: every registered action, in registry
/// order (`ActionRegistry::iter`, sorted by id), each paired with its
/// rendered binding from `bindings` if it has one; then every bundled theme
/// (`ThemeService::names`' sorted order) as a `Theme` row.
pub fn build_items(
    registry: &ActionRegistry,
    theme: &ThemeService,
    bindings: &BTreeMap<ActionId, String>,
) -> Vec<PaletteItem> {
    let mut items: Vec<PaletteItem> = registry
        .iter()
        .map(|def| {
            PaletteItem::Action(
                def.id.clone(),
                def.title.clone(),
                def.category.clone(),
                bindings.get(&def.id).cloned(),
            )
        })
        .collect();
    items.extend(theme.names().into_iter().map(PaletteItem::Theme));
    items
}

// ---------------------------------------------------------------------
// Rendering (gpui) — everything above this line is the pure core.
// ---------------------------------------------------------------------

use gpui::prelude::*;
use gpui::{
    App, Entity, FontWeight, HighlightStyle, IntoElement, MouseButton, ScrollHandle, StyledText,
    Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

use crate::fonts;

/// Target overlay width in pixels (brief: "~560px wide").
const WIDTH: f32 = 560.0;

/// Sizing hint only (no longer a selection clamp — see `PaletteState::
/// move_selection`'s doc comment): the number of rows the results viewport
/// is tall enough to show before it needs to scroll. Item count today is
/// 84 (`register_builtin_actions`' 40 actions + `theme::load_bundled`'s 44
/// bundled theme entries — counted directly, not estimated, by
/// `build_binding_index_and_items_cover_the_whole_registry_and_theme_set`),
/// well within what a plain scrollable `div`
/// handles without virtualization — see `ROW_HEIGHT` below for how this
/// becomes a pixel height.
const VISIBLE_ROWS: usize = 12;

/// Estimated row height in pixels (`px_2`/`py_1` padding plus one line of
/// default-size text) — used only to size the scrollable viewport to
/// [`VISIBLE_ROWS`] rows; not load-bearing for correctness the way it would
/// be for a hand-rolled offset calculation, because scroll-follow here goes
/// through `gpui::ScrollHandle::scroll_to_item`, which measures real
/// per-row layout bounds rather than trusting this estimate.
const ROW_HEIGHT: f32 = 28.0;

/// Render one row title with its [`fuzzy_match`]ed characters styled —
/// `cx.theme().primary` plus a bold weight (plan constraint: no raw
/// colors; bold is "cheap" per the brief).
///
/// **Mechanism, checked against the pinned gpui rev before building this**
/// (`gpui::elements::text::StyledText`, re-exported at the crate root):
/// `StyledText::with_highlights` takes `(byte Range, HighlightStyle)`
/// pairs and — unlike `with_default_highlights` — needs no `TextStyle` of
/// our own to seed the unhighlighted runs; it resolves them lazily from
/// `Window::text_style()` at layout time, i.e. whatever ambient text style
/// this element inherits from its ancestors (the row's `.text_color(...)`
/// when selected). That is a better fit here than gpui-component's
/// `highlighter` module (a full syntax-highlighter keyed to a language
/// grammar — built for code panes, not fuzzy-match spans) or hand-rolled
/// span children (would need to re-slice `title` into N+1 `div`s per row
/// and fight `h_flex`'s gaps to keep them visually glued together).
/// `highlight_runs` (pure core, above) does the char-index -> merged
/// byte-range conversion this needs.
fn highlighted_title(title: &str, indices: &[usize], primary: gpui::Hsla) -> StyledText {
    let runs = highlight_runs(title, indices);
    if runs.is_empty() {
        return StyledText::new(title.to_string());
    }
    let style = HighlightStyle {
        color: Some(primary),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    StyledText::new(title.to_string()).with_highlights(runs.into_iter().map(|r| (r, style)))
}

/// The palette overlay: a horizontally centered ~560px-wide panel, top
/// edge on the same line as every shell dialog
/// (`dialog::MODAL_TOP_RATIO`), on
/// `cx.theme().popover`, a real gpui-component `Input` for the query
/// (`query_input` — native caret/selection/clipboard, see this module's own
/// doc comment for the routing story), and every filtered result inside a
/// fixed-height (~[`VISIBLE_ROWS`] rows), scrollable list with the selected
/// row highlighted (`cx.theme().selection` background, `cx.theme().primary`
/// text — plan constraint: no raw colors, `cx.theme()` roles only) and its
/// binding right-aligned in `cx.theme().muted_foreground`.
///
/// **`Input` styling** (inventoried against `Input`'s own builder methods at
/// the pinned checkout, `crates/ui/src/input/input.rs`): `.appearance(false)`
/// strips `Input`'s own background/border/rounding (`self.appearance`
/// gates all three there), which would otherwise paint a second, competing
/// box inside this panel's own chrome; it does *not* touch `Input`'s
/// internal horizontal/vertical padding (`input_px`/`input_py`, applied
/// unconditionally for a single-line input regardless of `appearance`), so
/// `input_row` below needs no padding of its own beyond the bottom border
/// that visually separates it from `list` — the same convention the
/// toolbar's bare `Input::new(filter_input)` already uses (`shell::
/// toolbar::toolbar`, no wrapping padding there either).
///
/// **Mouse** (palette-input-polish task): each row's `on_mouse_down`
/// SELECTS it via `on_row_click` (a caller-supplied, cheaply `Clone`-able
/// closure — see [`ShellView::render`](../shell/struct.ShellView.html)'s
/// call site for how it's built from a `WeakEntity<ShellView>`, sidestepping
/// per-row heap allocation) — moves the highlight only, Enter (still routed
/// through `ShellView::handle_palette_key`) is what dispatches. The panel's
/// own `on_mouse_down` calls `cx.stop_propagation()` (precedent:
/// `dialog::render_modal`'s panel does the same over its backdrop) so a
/// click anywhere inside the panel — a row, the input, empty space — never
/// also reaches the full-window click-catcher `ShellView::render` wraps
/// this element in, which would otherwise dismiss the palette out from
/// under the very click that's interacting with it. That catcher (and its
/// dismiss-on-click-outside handling) lives in `shell::mod`, not here — see
/// that module's doc comment on why the backdrop-vs-panel split stays there.
///
/// **Scroll mechanism, checked against the pinned gpui rev before building
/// this** (`gpui::elements::div::{ScrollHandle, StatefulInteractiveElement}`,
/// re-exported at the crate root): `scroll_handle` is a `Clone`-cheap
/// (`Rc<RefCell<..>>`) handle the caller owns across frames (`ShellView`
/// keeps one alongside `PaletteState`, both rebuilt together on palette
/// open); `.id(..).overflow_y_scroll().track_scroll(scroll_handle)` on the
/// list container turns it into a real scrollable viewport with mouse-wheel
/// support built in (gpui-component's own `Scrollable`/list machinery — see
/// `crates/ui/src/scroll/`, `crates/ui/src/list/list.rs` in the pinned
/// gpui-component checkout — layers a custom scrollbar and virtualization
/// on top of exactly this primitive; at 66 items neither is needed here,
/// so this uses the primitive directly rather than pulling in `List`'s
/// virtualized-row bookkeeping for a list this small). `ShellView`'s
/// selection-change path (`move_selection` calls, the selection reset in
/// `set_query`, and row clicks via `set_selected`) calls `scroll_handle.
/// scroll_to_item(new_selected)` — a real per-frame layout measurement, not
/// a pixel-math guess — so the newly selected row always ends up visible;
/// this function only wires the handle into the container, it never calls
/// `scroll_to_item` itself.
///
/// `viewport_width`/`viewport_height` are the window's own drawable size
/// (`Window::viewport_size`, same source `ShellView::render` already reads
/// for the tile surface) — passed in rather than read from `cx` so this
/// stays a pure function of its arguments, the same shape as
/// `shell::status::status_bar`.
pub fn render(
    state: &PaletteState,
    scroll_handle: &ScrollHandle,
    query_input: &Entity<InputState>,
    on_row_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    viewport_width: f32,
    viewport_height: f32,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let width = WIDTH.min((viewport_width - 32.0).max(160.0));
    let left = ((viewport_width - width) / 2.0).max(0.0);
    // Same top edge as every shell dialog (`dialog::MODAL_TOP_RATIO`, user
    // direction — the palette is dialog-like, and one shared line beats a
    // separate top-third anchor for spatial memory).
    let top = (viewport_height * crate::shell::dialog::MODAL_TOP_RATIO).max(0.0);

    let results = state.filtered();

    // Fixed-height, scrollable viewport over the FULL filtered list (no
    // truncation) — `.id(..)` makes this a `Stateful<Div>`, required for
    // `overflow_y_scroll`/`track_scroll` (gpui::elements::div::
    // StatefulInteractiveElement); mouse-wheel scrolling comes for free
    // from `overflow_y_scroll` once the container is tracked. Sized to
    // `VISIBLE_ROWS` * `ROW_HEIGHT` when there's more than one screenful,
    // or exactly the content height otherwise, so a short result list
    // doesn't leave dead scrollable space below it.
    let mut list = v_flex()
        .id("palette-results")
        .w_full()
        .h(px(
            (results.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)
        ))
        .overflow_y_scroll()
        .track_scroll(scroll_handle)
        // Test-only (no-op outside `cfg(test)`/`test-support`, see gpui's
        // `debug_selector` doc comment): lets a `#[gpui::test]` recover
        // this container's painted bounds via `VisualTestContext::
        // debug_bounds` and check a row's bounds actually fall inside it
        // — i.e. that scroll-follow, not just selection, moved.
        .debug_selector(|| "palette-list".to_string());
    if results.is_empty() {
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_color(theme.muted_foreground)
                .child("No matches"),
        );
    } else {
        for (i, (item, indices)) in results.into_iter().enumerate() {
            let is_selected = i == state.selected();
            let mut row = h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .gap_3()
                .px_2()
                .py_1()
                .rounded(px(4.));
            if is_selected {
                row = row.bg(theme.selection).text_color(theme.primary);
            }
            // Test-only, see `list`'s `debug_selector` comment above.
            let row = row.debug_selector(move || format!("palette-row-{i}"));
            // A click selects this row (moves the highlight, no dispatch —
            // see this function's own doc comment); `on_row_click` is
            // cloned per row rather than shared some other way because each
            // row's closure needs to close over its own `i`.
            let click = on_row_click.clone();
            let row = row.on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                click(i, window, cx);
            });
            let label = h_flex()
                .gap_2()
                .items_center()
                .child(div().child(highlighted_title(&item.title(), &indices, theme.primary)))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(item.category().to_string()),
                );
            let binding = div()
                .font_family(fonts::MONO)
                .text_color(theme.muted_foreground)
                .child(item.binding().unwrap_or("").to_string());
            list = list.child(row.child(label).child(binding));
        }
    }

    // A muted search icon in the `prefix` slot, and still no placeholder
    // helper text: an empty query shows the icon and a caret, never hint
    // text. This is gpui-component's own idiom for this exact surface —
    // its command palette builds the identical `prefix` +
    // `appearance(false)` pair (pinned checkout, `crates/ui/src/command/
    // state.rs:838-846`), which is why the icon keeps its default size —
    // and the same one `shell::dialog::filter_row` wears, so all three
    // filtering surfaces read alike.
    //
    // `.appearance(false)` strips `Input`'s own border/background (see
    // this function's doc comment) but not its prefix, which that flag
    // never guards (`crates/ui/src/input/input.rs:578-584`); the bottom
    // border below is `input_row`'s own, standing in for the chrome
    // `appearance(true)` would otherwise have drawn, just scoped to
    // separating the query row from `list` rather than boxing the input
    // itself.
    let input_row = div()
        .w_full()
        .border_b_1()
        .border_color(theme.border)
        .child(
            Input::new(query_input)
                .appearance(false)
                .prefix(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                .w_full(),
        );

    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        // The same panel frame `dialog::render_modal` wears — background,
        // text, border, radius token and drop shadow — so the palette and
        // the dialogs read as siblings (user direction: "unify the
        // appearance"). The radius was a hardcoded `px(8.)` before this;
        // `theme.radius_lg` is the same idea but follows the theme, which
        // is what every other rounded surface in this crate already does.
        //
        // What deliberately still differs: the interior rhythm (this is a
        // denser surface — `ROW_HEIGHT` 28px against the dialogs' 44px, so
        // matching their `px_4`/`gap_3` would loosen it into looking like a
        // different component), the absent title row (a palette has no
        // title to show), and the undimmed backdrop — see the click-catcher
        // in `ShellView::render` for why that one is a decision, not an
        // omission.
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .shadow(crate::shell::dialog::overlay_panel_shadow())
        // Blocks hover and scroll under the panel, not just the clicks the
        // `on_mouse_down` below already stops — `render_modal`'s panel has
        // had this from the start; the palette simply never grew it.
        .occlude()
        // Test-only, see `list`'s `debug_selector` comment above — lets a
        // `#[gpui::test]` recover the panel's own painted bounds to click
        // inside it (precedent: `dialog::render_modal`'s
        // `"shell-modal-panel"`).
        .debug_selector(|| "palette-panel".to_string())
        // See this function's doc comment: stops a click anywhere in the
        // panel from also reaching the click-catcher `ShellView::render`
        // wraps this element in.
        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
            cx.stop_propagation();
        })
        .child(input_row)
        .child(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(id: &str, title: &str, category: &str, binding: Option<&str>) -> PaletteItem {
        PaletteItem::Action(
            ActionId(id.to_string()),
            title.to_string(),
            category.to_string(),
            binding.map(str::to_string),
        )
    }

    // -- fuzzy_match ----------------------------------------------------

    #[test]
    fn empty_query_matches_everything_with_score_zero() {
        assert_eq!(fuzzy_match("", "anything at all"), Some((0, Vec::new())));
        assert_eq!(fuzzy_match("", ""), Some((0, Vec::new())));
    }

    #[test]
    fn non_subsequence_is_none() {
        assert_eq!(fuzzy_match("xyz", "split horizontal"), None);
        assert_eq!(fuzzy_match("longer than candidate", "short"), None);
    }

    #[test]
    fn subsequence_match_is_case_insensitive() {
        assert!(fuzzy_match("SpLiT", "split horizontal").is_some());
        assert!(fuzzy_match("split", "SPLIT HORIZONTAL").is_some());
        assert_eq!(
            fuzzy_match("split", "split horizontal"),
            fuzzy_match("SPLIT", "split horizontal"),
            "case should not affect the score or indices, only whether it matches"
        );
    }

    #[test]
    fn out_of_order_characters_do_not_match() {
        // "ts" is not a subsequence of "split" (s comes before t is wrong
        // way around: 's','p','l','i','t' has no 't' before an 's').
        assert_eq!(fuzzy_match("ts", "split"), None);
    }

    #[test]
    fn prefix_match_scores_higher_than_scattered_match() {
        let (prefix, _) = fuzzy_match("app", "Apple Pie").unwrap();
        let (scattered, _) = fuzzy_match("app", "Snap Pool").unwrap();
        assert!(
            prefix > scattered,
            "prefix={prefix} should beat scattered={scattered}"
        );
    }

    #[test]
    fn consecutive_run_scores_higher_than_the_same_letters_scattered() {
        // Both start with the query's first letter at position 0 (so both
        // get the same prefix bonus) and neither scatters a later letter
        // right after a separator (so word-start bonuses don't confound
        // the comparison) — the only thing distinguishing them is that
        // "splendid" matches s-p-l as one consecutive run.
        let (consecutive, _) = fuzzy_match("spl", "splendid").unwrap();
        let (scattered, _) = fuzzy_match("spl", "supplier").unwrap();
        assert!(
            consecutive > scattered,
            "consecutive={consecutive} should beat scattered={scattered}"
        );
    }

    #[test]
    fn word_start_match_scores_higher_than_mid_word_match() {
        let (word_start, _) = fuzzy_match("h", "split horizontal").unwrap();
        let (mid_word, _) = fuzzy_match("h", "ghost").unwrap();
        assert!(
            word_start > mid_word,
            "word_start={word_start} should beat mid_word={mid_word}"
        );
    }

    // -- fuzzy_match indices ----------------------------------------------

    #[test]
    fn indices_are_contiguous_for_a_prefix_match() {
        // "app" against "Apple Pie" (lowered "apple pie") matches
        // a(0) p(1) p(2) — one unbroken run at the very start.
        let (_, indices) = fuzzy_match("app", "Apple Pie").unwrap();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn indices_land_on_the_word_start_character() {
        // "split horizontal": s0 p1 l2 i3 t4 ' '5 h6 ... — the single "h"
        // query character claims the word-start h, not some other h later
        // in "horizontal".
        let (_, indices) = fuzzy_match("h", "split horizontal").unwrap();
        assert_eq!(indices, vec![6]);
    }

    #[test]
    fn indices_scatter_for_a_non_contiguous_match() {
        // "supplier": s0 u1 p2 p3 l4 i5 e6 r7 — "spl" greedy-leftmost
        // matches s(0), then the first "p" at 2 (skipping "u"), then the
        // first "l" after that at 4 (skipping the second "p") — three
        // indices with gaps, not one run.
        let (_, indices) = fuzzy_match("spl", "supplier").unwrap();
        assert_eq!(indices, vec![0, 2, 4]);
    }

    #[test]
    fn indices_one_per_query_character_in_increasing_order() {
        let (_, indices) = fuzzy_match("plt", "split horizontal").unwrap();
        assert_eq!(indices.len(), 3);
        assert!(
            indices.windows(2).all(|w| w[0] < w[1]),
            "indices should be strictly increasing: {indices:?}"
        );
    }

    // -- highlight_runs -----------------------------------------------------

    #[test]
    fn highlight_runs_merges_a_contiguous_prefix_into_one_range() {
        assert_eq!(highlight_runs("Apple Pie", &[0, 1, 2]), vec![0..3]);
    }

    #[test]
    fn highlight_runs_keeps_scattered_indices_as_separate_ranges() {
        assert_eq!(
            highlight_runs("supplier", &[0, 2, 4]),
            vec![0..1, 2..3, 4..5]
        );
    }

    #[test]
    fn highlight_runs_on_no_matched_indices_is_empty() {
        assert!(highlight_runs("anything", &[]).is_empty());
    }

    /// Fix-round nit guard: 'İ' (U+0130) lowercases to "i\u{307}" — TWO
    /// chars — so match indices (positions in the LOWERED text, per
    /// `fuzzy_match`'s contract) can exceed the original title's char
    /// count. Unreachable with today's all-ASCII titles, but it was a
    /// panic-on-the-render-path landmine (`boundaries[run_end + 1]` out
    /// of bounds) if theme names ever go free-form. The guard skips the
    /// expansion-only indices instead of panicking; the in-range chars
    /// still highlight.
    #[test]
    fn highlight_runs_survives_lowercase_char_expansion() {
        let title = "İstanbul"; // 8 chars; lowered "i\u{307}stanbul" is 9
        let (_, indices) = fuzzy_match(title, title).unwrap();
        assert_eq!(
            indices.len(),
            9,
            "sanity: the self-match produces one index per LOWERED char, \
             one more than the title has"
        );
        let runs = highlight_runs(title, &indices);
        assert_eq!(
            runs,
            vec![0..title.len()],
            "the whole title highlights; the expansion-only index is skipped, \
             not a panic"
        );
    }

    #[test]
    fn highlight_runs_uses_byte_offsets_past_a_multibyte_prefix() {
        // A non-ASCII char earlier in the string shifts later byte offsets
        // away from the char index — proving the conversion is char-aware,
        // not a byte-index passthrough. "é" is 2 bytes in UTF-8.
        assert_eq!(highlight_runs("é match", &[2, 3]), vec![3..5]);
    }

    // -- PaletteState -----------------------------------------------------

    #[test]
    fn empty_query_returns_every_item_in_registry_order() {
        let items = vec![
            action("workspace::focus_left", "Focus left", "Workspace", None),
            action(
                "workspace::split_right",
                "Split right",
                "Workspace",
                Some("ctrl+v"),
            ),
            PaletteItem::Theme("Gruvbox Dark".to_string()),
        ];
        let expected = items.clone();
        let state = PaletteState::new(items);
        let expected_filtered: Vec<(&PaletteItem, Vec<usize>)> =
            expected.iter().map(|item| (item, Vec::new())).collect();
        assert_eq!(state.filtered(), expected_filtered);
    }

    #[test]
    fn query_filters_out_non_matching_items() {
        let mut state = PaletteState::new(vec![
            action("workspace::split_right", "Split right", "Workspace", None),
            action("workspace::focus_left", "Focus left", "Workspace", None),
            PaletteItem::Theme("Gruvbox Dark".to_string()),
        ]);
        state.set_query("split");
        let titles: Vec<String> = state
            .filtered()
            .iter()
            .map(|(item, _)| item.title())
            .collect();
        assert_eq!(titles, vec!["Split right".to_string()]);
    }

    #[test]
    fn query_ranks_the_best_match_first() {
        let mut state = PaletteState::new(vec![
            action("a", "Snap Pool", "Test", None),
            action("b", "Apple Pie", "Test", None),
        ]);
        state.set_query("app");
        let titles: Vec<String> = state
            .filtered()
            .iter()
            .map(|(item, _)| item.title())
            .collect();
        assert_eq!(
            titles,
            vec!["Apple Pie".to_string(), "Snap Pool".to_string()]
        );
    }

    #[test]
    fn set_query_replaces_the_whole_query_and_resets_selection() {
        let mut state = PaletteState::new(vec![
            action("a", "Apple", "Test", None),
            action("b", "Banana", "Test", None),
        ]);
        state.move_selection(1);
        assert_eq!(state.selected(), 1);

        state.set_query("banana");
        assert_eq!(state.query(), "banana");
        assert_eq!(
            state.selected(),
            0,
            "replacing the query should reset the selection to the top match"
        );
        let titles: Vec<String> = state
            .filtered()
            .iter()
            .map(|(item, _)| item.title())
            .collect();
        assert_eq!(titles, vec!["Banana".to_string()]);

        // A later call replaces the whole string, it doesn't append —
        // proving this really is a whole-value setter, not `push_char` in
        // disguise.
        state.set_query("apple");
        assert_eq!(state.query(), "apple");
    }

    #[test]
    fn move_selection_wraps_at_both_ends() {
        let mut state = PaletteState::new(vec![
            action("a", "A", "Test", None),
            action("b", "B", "Test", None),
            action("c", "C", "Test", None),
        ]);
        assert_eq!(state.selected(), 0);
        // Up at index 0 wraps to the last item
        state.move_selection(-1);
        assert_eq!(state.selected(), 2, "up at 0 should wrap to last index");

        // Down at the last item wraps to 0
        state.move_selection(1);
        assert_eq!(state.selected(), 0, "down at last should wrap to 0");
    }

    #[test]
    fn move_selection_wraps_up_from_start_with_large_list() {
        // 70+ item list to test wrapping with a large dataset
        const ITEM_COUNT: usize = 75;
        let items: Vec<PaletteItem> = (0..ITEM_COUNT)
            .map(|i| action(&format!("a{i}"), &format!("Item {i}"), "Test", None))
            .collect();
        let mut state = PaletteState::new(items);
        assert_eq!(state.selected(), 0);
        // Up at index 0 wraps to last
        state.move_selection(-1);
        assert_eq!(
            state.selected(),
            ITEM_COUNT - 1,
            "up from 0 should wrap to last with a large list"
        );
    }

    #[test]
    fn move_selection_wraps_down_from_end_with_large_list() {
        // 70+ item list to test wrapping with a large dataset
        const ITEM_COUNT: usize = 75;
        let items: Vec<PaletteItem> = (0..ITEM_COUNT)
            .map(|i| action(&format!("a{i}"), &format!("Item {i}"), "Test", None))
            .collect();
        let mut state = PaletteState::new(items);
        // Move to last item (index ITEM_COUNT - 1 = 74)
        for _ in 0..(ITEM_COUNT - 1) {
            state.move_selection(1);
        }
        assert_eq!(state.selected(), ITEM_COUNT - 1);
        // Down at last wraps to 0
        state.move_selection(1);
        assert_eq!(
            state.selected(),
            0,
            "down from last should wrap to 0 with a large list"
        );
    }

    #[test]
    fn move_selection_single_item_wraps_to_itself() {
        let items = vec![action("a", "Only Item", "Test", None)];
        let mut state = PaletteState::new(items);
        assert_eq!(state.selected(), 0);
        // Up wraps to itself
        state.move_selection(-1);
        assert_eq!(state.selected(), 0, "single item up should stay at 0");
        // Down wraps to itself
        state.move_selection(1);
        assert_eq!(state.selected(), 0, "single item down should stay at 0");
    }

    #[test]
    fn move_selection_on_an_empty_result_set_does_not_panic() {
        let mut state = PaletteState::new(vec![action("a", "Focus left", "Workspace", None)]);
        state.set_query("nomatch");
        assert!(state.filtered().is_empty());
        state.move_selection(1);
        state.move_selection(-1);
        assert_eq!(state.selected(), 0);
    }

    #[test]
    fn editing_the_query_resets_the_selection() {
        // Both titles match a lone "a" query, so the list stays 2 items
        // wide across the edits below — this test is about the selection
        // resetting on every edit, not about filtering narrowing it.
        let mut state = PaletteState::new(vec![
            action("a", "Apple", "Test", None),
            action("b", "Banana", "Test", None),
        ]);
        state.move_selection(1);
        assert_eq!(state.selected(), 1);
        state.set_query("a");
        assert_eq!(state.selected(), 0);

        state.move_selection(1);
        assert_eq!(state.selected(), 1);
        state.set_query("");
        assert_eq!(state.selected(), 0);
    }

    #[test]
    fn set_selected_clamps_to_the_current_filtered_list() {
        let items = vec![
            action("a", "A", "Test", None),
            action("b", "B", "Test", None),
            action("c", "C", "Test", None),
        ];
        let mut state = PaletteState::new(items);

        state.set_selected(2);
        assert_eq!(state.selected(), 2);

        // Past the end (e.g. the query narrowed the list between the frame
        // that painted this row and the click landing) clamps to the last
        // row rather than panicking or going out of range.
        state.set_selected(50);
        assert_eq!(state.selected(), 2);
    }

    #[test]
    fn set_selected_on_an_empty_result_set_does_not_panic() {
        let mut state = PaletteState::new(vec![action("a", "Focus left", "Workspace", None)]);
        state.set_query("nomatch");
        assert!(state.filtered().is_empty());
        state.set_selected(3);
        assert_eq!(state.selected(), 0);
    }

    #[test]
    fn selected_item_reflects_the_current_filter_and_selection() {
        let mut state = PaletteState::new(vec![
            action("a", "Focus left", "Workspace", None),
            action("b", "Focus right", "Workspace", None),
        ]);
        state.move_selection(1);
        assert_eq!(
            state.selected_item(),
            Some(action("b", "Focus right", "Workspace", None))
        );
    }

    #[test]
    fn selected_item_is_none_when_nothing_matches() {
        let mut state = PaletteState::new(vec![action("a", "Focus left", "Workspace", None)]);
        state.set_query("nomatch");
        assert_eq!(state.selected_item(), None);
    }

    /// Post-merge review perf 11: the filter runs once per query edit,
    /// never per `filtered()` call — before the cache, every call
    /// re-fuzzy-matched the whole item list (an Enter press computed it
    /// three times: `selected_item`, the close-path bookkeeping, and the
    /// next render). Observed honestly through the test-only per-instance
    /// match-call counter incremented at the real `fuzzy_match_lowered`
    /// invocation site — not a stand-in assertion.
    #[test]
    fn filtered_matches_once_per_query_edit_not_per_call() {
        let mut state = PaletteState::new(vec![
            action("a", "Apple", "Test", None),
            action("b", "Banana", "Test", None),
            action("c", "Cherry", "Test", None),
        ]);
        // Construction itself filters once (the empty query's full list).
        assert_eq!(state.match_call_count(), 3);

        state.set_query("an");
        assert_eq!(
            state.match_call_count(),
            6,
            "a query edit re-matches every item exactly once"
        );

        let first = state.filtered();
        let titles: Vec<String> = first.iter().map(|(item, _)| item.title()).collect();
        assert_eq!(titles, vec!["Banana".to_string()]);
        assert_eq!(
            first[0].1,
            vec![1, 2],
            "the cached result carries the matched char indices"
        );

        // Repeat reads — the per-Enter triple-compute shape and more —
        // must do no matching at all.
        let _ = state.filtered();
        let _ = state.filtered();
        let _ = state.selected_item();
        state.move_selection(1);
        state.set_selected(0);
        assert_eq!(
            state.match_call_count(),
            6,
            "repeated filtered()/selection reads must not re-match"
        );

        // And the next edit matches exactly once more per item.
        state.set_query("a");
        assert_eq!(state.match_call_count(), 9);
    }

    // -- PaletteItem ------------------------------------------------------

    #[test]
    fn theme_item_title_and_category_follow_the_brief() {
        let item = PaletteItem::Theme("Gruvbox Dark".to_string());
        assert_eq!(item.title(), "Theme: Gruvbox Dark");
        assert_eq!(item.category(), "Appearance");
        assert_eq!(item.binding(), None);
    }

    #[test]
    fn action_item_exposes_its_registry_fields_and_binding() {
        let item = action(
            "workspace::split_right",
            "Split right",
            "Workspace",
            Some("ctrl+v"),
        );
        assert_eq!(item.title(), "Split right");
        assert_eq!(item.category(), "Workspace");
        assert_eq!(item.binding(), Some("ctrl+v"));
    }

    // -- render_binding / build_binding_index / build_items ---------------

    #[test]
    fn render_binding_joins_modifiers_and_sequence_parts() {
        use crate::keymap::Modifiers;

        let single = vec![Keystroke {
            mods: Modifiers::CTRL.union(Modifiers {
                shift: true,
                ..Modifiers::NONE
            }),
            key: "p".to_string(),
        }];
        assert_eq!(render_binding(&single), "ctrl+shift+p");

        let sequence = vec![
            Keystroke {
                mods: Modifiers::NONE,
                key: "g".to_string(),
            },
            Keystroke {
                mods: Modifiers::NONE,
                key: "g".to_string(),
            },
        ];
        assert_eq!(render_binding(&sequence), "g g");
    }

    #[test]
    fn build_binding_index_and_items_cover_the_whole_registry_and_theme_set() {
        use crate::defaults::{BUILTIN_KEYMAP, default_mod, register_builtin_actions};
        use crate::keymap::build_keymap;
        use geode_core::config::LayerDoc;

        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = crate::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");

        let bindings = build_binding_index(&keymap);
        // palette::toggle is bound to both "ctrl+k" and "ctrl+shift+p" in
        // BUILTIN_KEYMAP's [bindings.keys] table; TOML tables here iterate
        // by sorted key spelling (see keymap::build's own doc comment), so
        // "ctrl+k" (alphabetically first: 'k' < 's') is the one that wins as
        // build_keymap's first-encountered binding, and so the one
        // build_binding_index's first-wins rule keeps for display.
        assert_eq!(
            bindings
                .get(&ActionId("palette::toggle".to_string()))
                .map(String::as_str),
            Some("ctrl+k")
        );

        let items = build_items(&registry, &theme, &bindings);

        for def in registry.iter() {
            let found = items
                .iter()
                .find(|item| matches!(item, PaletteItem::Action(id, ..) if id == &def.id));
            assert!(
                found.is_some(),
                "missing palette item for action {}",
                def.id
            );
            if let Some(PaletteItem::Action(_, title, category, _)) = found {
                assert_eq!(title, &def.title);
                assert_eq!(category, &def.category);
            }
        }

        for name in theme.names() {
            assert!(
                items.contains(&PaletteItem::Theme(name.clone())),
                "missing palette item for theme {name}"
            );
        }

        let split = items
            .iter()
            .find(|item| matches!(item, PaletteItem::Action(id, ..) if id.0 == "workspace::split_right"))
            .unwrap();
        assert_eq!(split.binding(), Some("ctrl+v"));
    }
}
