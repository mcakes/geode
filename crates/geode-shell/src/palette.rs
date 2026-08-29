//! The command palette (Task 6, spec §3.2/§3.3): a universal, fuzzy-filtered
//! list over every registered action and every bundled theme, driven purely
//! by the keyboard (`ctrl+k` / `ctrl+shift+p` open, Esc closes; typing
//! filters; up/down or ctrl+p/ctrl+n move the selection; Enter dispatches).
//!
//! Everything above the `render` section is the pure core: `PaletteItem`,
//! `PaletteState`, `fuzzy_match`, and the keystroke-rendering/index-building
//! helpers never import `gpui` and are fully unit-tested without a window
//! (plan constraint: "the fuzzy filter and PaletteState are PURE and TDD'd
//! — no deps, no gpui"). `render`, at the bottom, is the only part that
//! touches `gpui`/`gpui_component`; `ShellView` (`shell::mod`) owns the
//! `PaletteState` and drives it from real key events.

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
    if query.is_empty() {
        return Some((0, Vec::new()));
    }

    let cand: Vec<char> = candidate.to_lowercase().chars().collect();
    let mut cand_idx = 0usize;
    let mut prev_matched: Option<usize> = None;
    let mut consecutive_run: u32 = 0;
    let mut score: u32 = 0;
    let mut indices: Vec<usize> = Vec::new();

    for qc in query.to_lowercase().chars() {
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
fn highlight_runs(title: &str, indices: &[usize]) -> Vec<std::ops::Range<usize>> {
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

    let mut runs = Vec::new();
    let mut run_start = indices[0];
    let mut run_end = indices[0];
    for &idx in &indices[1..] {
        if idx == run_end + 1 {
            run_end = idx;
            continue;
        }
        runs.push(boundaries[run_start]..boundaries[run_end + 1]);
        run_start = idx;
        run_end = idx;
    }
    runs.push(boundaries[run_start]..boundaries[run_end + 1]);
    runs
}

/// Fuzzy-filtered, keyboard-navigable palette state. Pure: no `gpui`, no
/// I/O. `ShellView` builds one fresh (via [`build_items`]) each time the
/// palette opens and drops it on close — nothing here is per-frame state.
pub struct PaletteState {
    items: Vec<PaletteItem>,
    query: String,
    selected: usize,
}

impl PaletteState {
    pub fn new(items: Vec<PaletteItem>) -> Self {
        PaletteState {
            items,
            query: String::new(),
            selected: 0,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Append one typed character to the query and reset the selection to
    /// the top match. Filtering can shrink or reorder the result list out
    /// from under a selection further down it, so every query edit snaps
    /// the selection back to a row that's guaranteed to still exist.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
    }

    /// Remove the last character of the query (a no-op on an empty query),
    /// resetting the selection for the same reason as [`push_char`].
    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    /// Every item whose title fuzzy-matches the current query
    /// ([`fuzzy_match`]), best match first, paired with the matched char
    /// indices `render` highlights. Ties — including an empty query, where
    /// every item scores the same `Some(0)` — keep their original `items`
    /// order, because `sort_by_key` is a stable sort: this is exactly the
    /// brief's "empty query returns all in registry order".
    pub fn filtered(&self) -> Vec<(&PaletteItem, Vec<usize>)> {
        let mut scored: Vec<(&PaletteItem, u32, Vec<usize>)> = self
            .items
            .iter()
            .filter_map(|item| {
                fuzzy_match(&self.query, &item.title())
                    .map(|(score, indices)| (item, score, indices))
            })
            .collect();
        scored.sort_by_key(|(_, score, _)| std::cmp::Reverse(*score));
        scored
            .into_iter()
            .map(|(item, _, indices)| (item, indices))
            .collect()
    }

    /// Move the selection by `delta` rows (arrow keys / ctrl+p / ctrl+n
    /// pass ±1), clamped to the current filtered list's bounds — the FULL
    /// list, not just what fits in one screenful. `render` now draws every
    /// filtered row inside a scrollable viewport (rather than truncating to
    /// a fixed window), and the caller that drives real key events
    /// (`ShellView::handle_palette_key`) is responsible for scrolling the
    /// newly selected row into view after each call here — see
    /// `gpui::ScrollHandle::scroll_to_item`, invoked from that same
    /// selection-change path. Leaves the selection at 0 without panicking
    /// when nothing matches.
    pub fn move_selection(&mut self, delta: i32) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = next as usize;
    }

    /// The currently selected row, if any (an empty filtered list, or a
    /// `selected` index somehow past its end, both yield `None` rather than
    /// panicking). Returns an owned clone so callers can dispatch it after
    /// dropping the palette state (e.g. closing the palette first).
    pub fn selected_item(&self) -> Option<PaletteItem> {
        self.filtered()
            .get(self.selected)
            .map(|(item, _)| (*item).clone())
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
use gpui::{App, FontWeight, HighlightStyle, IntoElement, ScrollHandle, StyledText, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::fonts;

/// Target overlay width in pixels (brief: "~560px wide").
const WIDTH: f32 = 560.0;

/// Sizing hint only (no longer a selection clamp — see `PaletteState::
/// move_selection`'s doc comment): the number of rows the results viewport
/// is tall enough to show before it needs to scroll. Item count today is
/// 66 (`register_builtin_actions`' 15 actions + `theme::load_bundled`'s 51
/// bundled theme entries — counted directly, not estimated, while doing
/// this task's inventory), well within what a plain scrollable `div`
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

/// The palette overlay: a centered, top-third, ~560px-wide panel on
/// `cx.theme().popover`, an input line (rendered text + a trailing caret
/// glyph — no `Input` entity needed for this), and every filtered result
/// inside a fixed-height (~[`VISIBLE_ROWS`] rows), scrollable list with the
/// selected row highlighted (`cx.theme().selection` background,
/// `cx.theme().primary` text — plan constraint: no raw colors, `cx.theme()`
/// roles only) and its binding right-aligned in `cx.theme().muted_foreground`.
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
/// selection-change path (`move_selection` calls, plus the selection resets
/// in `push_char`/`backspace`) calls `scroll_handle.scroll_to_item(new_
/// selected)` — a real per-frame layout measurement, not a pixel-math
/// guess — so the newly selected row always ends up visible; this function
/// only wires the handle into the container, it never calls `scroll_to_item`
/// itself.
///
/// `viewport_width`/`viewport_height` are the window's own drawable size
/// (`Window::viewport_size`, same source `ShellView::render` already reads
/// for the tile surface) — passed in rather than read from `cx` so this
/// stays a pure function of its arguments, the same shape as
/// `shell::status::status_bar`.
pub fn render(
    state: &PaletteState,
    scroll_handle: &ScrollHandle,
    viewport_width: f32,
    viewport_height: f32,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let width = WIDTH.min((viewport_width - 32.0).max(160.0));
    let left = ((viewport_width - width) / 2.0).max(0.0);
    let top = (viewport_height / 3.0).max(0.0);

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

    // No placeholder helper text (plan constraint: removed entirely) — an
    // empty query renders as just the trailing caret glyph, alone and
    // first, rather than falling back to hint text.
    let input_row = div()
        .w_full()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(theme.border)
        .child(format!("{}|", state.query()));

    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(px(8.))
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
        for c in "split".chars() {
            state.push_char(c);
        }
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
        for c in "app".chars() {
            state.push_char(c);
        }
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
    fn backspace_removes_the_last_query_character() {
        let mut state = PaletteState::new(vec![action("a", "Focus left", "Workspace", None)]);
        state.push_char('x');
        state.push_char('y');
        state.backspace();
        assert_eq!(state.query(), "x");
        state.backspace();
        assert_eq!(state.query(), "");
        // Backspacing an already-empty query is a no-op, not a panic.
        state.backspace();
        assert_eq!(state.query(), "");
    }

    #[test]
    fn move_selection_clamps_at_both_ends() {
        let mut state = PaletteState::new(vec![
            action("a", "A", "Test", None),
            action("b", "B", "Test", None),
            action("c", "C", "Test", None),
        ]);
        assert_eq!(state.selected(), 0);
        state.move_selection(-1);
        assert_eq!(state.selected(), 0, "cannot go below the first row");

        state.move_selection(1);
        state.move_selection(1);
        state.move_selection(1);
        state.move_selection(1);
        assert_eq!(state.selected(), 2, "cannot go past the last row");
    }

    #[test]
    fn move_selection_reaches_the_last_of_many_filtered_items() {
        // Item count well past one screenful (today's real registry+theme
        // set is 66: 15 actions + 51 themes) — the selection must walk all the
        // way to the last FILTERED row, not clamp at some fixed visible
        // window. `render` now draws every filtered row inside a
        // scrollable viewport rather than truncating, so there is no
        // shorter bound to clamp against here any more.
        const ITEM_COUNT: usize = 78;
        let items: Vec<PaletteItem> = (0..ITEM_COUNT)
            .map(|i| action(&format!("a{i}"), &format!("Item {i}"), "Test", None))
            .collect();
        let mut state = PaletteState::new(items);
        for _ in 0..(ITEM_COUNT + 8) {
            state.move_selection(1);
        }
        assert_eq!(
            state.selected(),
            ITEM_COUNT - 1,
            "selection must clamp at the last filtered row, however many there are"
        );
    }

    #[test]
    fn move_selection_on_an_empty_result_set_does_not_panic() {
        let mut state = PaletteState::new(vec![action("a", "Focus left", "Workspace", None)]);
        for c in "nomatch".chars() {
            state.push_char(c);
        }
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
        state.push_char('a');
        assert_eq!(state.selected(), 0);

        state.move_selection(1);
        assert_eq!(state.selected(), 1);
        state.backspace();
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
        for c in "nomatch".chars() {
            state.push_char(c);
        }
        assert_eq!(state.selected_item(), None);
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
