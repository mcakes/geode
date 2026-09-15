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
//!
//! **Ranking** (2026-09-12): a row's match text is `"{title} {category}"`
//! — the same `searchable_text` shape the keybindings and settings dialogs
//! filter over — with every score earned past the title's end divided by
//! [`CATEGORY_DIVISOR`], so the category is searchable but the lower-
//! preference field: the same letters in a title outrank them in a
//! category, and the alignment (and so the highlight, split across the
//! two labels by [`split_label_indices`]) lands on the title copy when
//! both hold them. On top of the match score each row adds its usage
//! bonus (`crate::palette_usage`, baked once per open by
//! [`PaletteState::with_usage`]): an empty query lists the rows a trader
//! has actually chosen first, most recent and most used first, and a typed
//! query lets a habitual row edge past a marginally better textual match.
//! The bonus is capped at two run bonuses (`palette_usage::MAX_BONUS`,
//! 18), and the ruling on where that cap meets the category discount
//! (review 2026-09-12) is: a bare scattered match never beats a
//! contiguous run whatever its usage; a maxed-out row whose only hit is
//! in its category leads an unused title prefix on a query of three
//! characters or fewer, ties it at four (registry order holds) and loses
//! to it from five on — pinned by
//! `a_used_category_hit_leads_a_short_title_prefix_and_loses_to_a_long_one`,
//! the test to read before moving either constant.

use std::collections::BTreeMap;

use crate::actions::{ActionId, ActionRegistry};
use crate::keymap::{Keymap, Keystroke};
use crate::theme::ThemeService;

/// Palette-facing category for theme rows (brief: themes appear as
/// `"Theme: {name}"` under category `"Appearance"`).
const THEME_CATEGORY: &str = "Appearance";

/// Palette-facing category for saved-scope rows (Phase 4a §3.9: `"Scope:
/// {name}"`, listed after the theme rows).
const SCOPE_CATEGORY: &str = "Scope";

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
    /// A saved scope's name (Phase 4a §3.9) — selecting it loads that
    /// scope via `Frame::load_scope`.
    Scope(String),
}

impl PaletteItem {
    /// Display/match title: the action's own title, `"Theme: {name}"`, or
    /// `"Scope: {name}"`.
    pub fn title(&self) -> String {
        match self {
            PaletteItem::Action(_, title, _, _) => title.clone(),
            PaletteItem::Theme(name) => format!("Theme: {name}"),
            PaletteItem::Scope(name) => format!("Scope: {name}"),
        }
    }

    pub fn category(&self) -> &str {
        match self {
            PaletteItem::Action(_, _, category, _) => category,
            PaletteItem::Theme(_) => THEME_CATEGORY,
            PaletteItem::Scope(_) => SCOPE_CATEGORY,
        }
    }

    /// The key this row's usage is recorded under
    /// (`crate::palette_usage::PaletteUsage`): the kind, then the identity
    /// the dispatch itself keys on — `action:{id}`, `theme:{name}`,
    /// `scope:{name}` — so a retitled action keeps its history and a theme
    /// and a scope that happen to share a name never share a record.
    pub fn usage_key(&self) -> String {
        match self {
            PaletteItem::Action(id, ..) => format!("action:{}", id.0),
            PaletteItem::Theme(name) => format!("theme:{name}"),
            PaletteItem::Scope(name) => format!("scope:{name}"),
        }
    }

    /// Rendered keybinding text (e.g. `"alt+p"`), if any — always `None`
    /// for a theme or saved-scope row, since both are only ever reached by
    /// selecting them in the palette itself.
    pub fn binding(&self) -> Option<&str> {
        match self {
            PaletteItem::Action(_, _, _, binding) => binding.as_deref(),
            PaletteItem::Theme(_) | PaletteItem::Scope(_) => None,
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
/// Matching is an optimal alignment, not a greedy walk: every way of
/// placing the query's characters, in order, over `candidate` is scored
/// and the best-scoring placement is the one returned — so the indices
/// ARE the alignment the score ranks by. The score rewards, per matched
/// character: a match at position 0 (prefix start), a match right after a
/// separator (`' '`, `':'`, `'_'`, `'-'` — a "word start"), and each pair
/// of consecutive matched characters ([`RUN_BONUS`]), so one unbroken run
/// scores more than the same letters split across runs or scattered.
/// Between equally-scored placements the earliest wins, and a run is
/// continued rather than restarted.
///
/// Why not greedy-leftmost (what this was until 2026-09-12): the settings
/// dialog matches `ling` over the joined text "add tile tiling", and a
/// greedy walk claimed the `l` of "tile" first, then scattered `i`, `n`,
/// `g` across "tiling" — painting `Add ti[l]e` / `T[i]li[ng]` — while the
/// whole query sat as one run in "tiling". Greedy also under-scored such
/// rows, since the placement it scored was not the best one available.
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
    fuzzy_match_lowered(&query.to_lowercase(), &candidate.to_lowercase(), usize::MAX)
}

/// [`fuzzy_match`]'s core over ALREADY-lowercased inputs (post-merge
/// review perf 11): [`PaletteState`] stores each item's lowered match
/// text once at construction and lowercases the query once per edit, so
/// the per-item work in a filter pass is just this subsequence walk —
/// no per-item `title()` clone, no per-item `to_lowercase`. The public
/// wrapper above keeps the original lowercase-both contract for callers
/// (and tests) holding raw strings.
///
/// `title_len` is the char index where the candidate's title ends and its
/// category begins (the palette matches `"{title} {category}"`, the same
/// `searchable_text` shape the keybindings and settings dialogs use):
/// every score a character at or past that boundary earns — its own base
/// and the run bonus for continuing onto it — is divided by
/// [`CATEGORY_DIVISOR`], so the same letters in a title outrank them in a
/// category, and the alignment lands on the title copy when both hold
/// them (the highlight has to agree with the ranking). `usize::MAX` means
/// the whole candidate is title.
fn fuzzy_match_lowered(
    query: &str,
    candidate: &str,
    title_len: usize,
) -> Option<(u32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }

    let q: Vec<char> = query.chars().collect();
    let c: Vec<char> = candidate.chars().collect();
    let (n, m) = (q.len(), c.len());
    if n > m {
        return None;
    }
    let base_at = |j: usize| discounted(char_base(&c, j), j, title_len);
    let run_at = |j: usize| discounted(RUN_BONUS, j, title_len);

    // Two row-major `n × m` tables (Smith–Waterman in the mould of fzf's
    // v2 matcher, without its gap penalty):
    //   ends_at[i][j]  best score with `q[..=i]` placed and `q[i]` AT `c[j]`
    //   within [i][j]  best score with `q[..=i]` placed somewhere in `c[..=j]`
    // `within` only advances on a strictly better cell, so among equal
    // scores it remembers the earliest — which is what makes the
    // backtrack below prefer the leftmost of two tied placements.
    let mut ends_at: Vec<Option<u32>> = vec![None; n * m];
    let mut within: Vec<Option<u32>> = vec![None; n * m];
    for i in 0..n {
        let mut best_so_far: Option<u32> = None;
        let mut any = false;
        for j in 0..m {
            let mut cell = None;
            if c[j] == q[i] && j >= i {
                let base = base_at(j);
                if i == 0 {
                    cell = Some(base);
                } else {
                    let prev = (i - 1) * m + (j - 1);
                    let fresh = within[prev];
                    let cont = ends_at[prev].map(|s| s + run_at(j));
                    cell = fresh.max(cont).map(|s| s + base);
                }
            }
            ends_at[i * m + j] = cell;
            if let Some(s) = cell
                && best_so_far.is_none_or(|b| s > b)
            {
                best_so_far = Some(s);
            }
            within[i * m + j] = best_so_far;
            any |= cell.is_some();
        }
        if !any {
            return None;
        }
    }

    let score = within[(n - 1) * m + (m - 1)]?;

    // Backtrack from the earliest cell holding the final score, at each
    // step continuing a run when that reproduces the cell's score (so a
    // tie between "continue" and "restart" paints one run, not two) and
    // otherwise jumping to the earliest cell of the previous row that
    // carries `within`'s remembered best.
    let mut indices = vec![0usize; n];
    let mut j = (0..m).find(|&j| ends_at[(n - 1) * m + j] == Some(score))?;
    indices[n - 1] = j;
    for i in (1..n).rev() {
        let cell = ends_at[i * m + j]?;
        let base = base_at(j);
        let prev = (i - 1) * m + (j - 1);
        j = match ends_at[prev] {
            Some(s) if s + run_at(j) + base == cell => j - 1,
            _ => {
                let target = within[prev];
                (0..j).find(|&jj| ends_at[(i - 1) * m + jj] == target)?
            }
        };
        indices[i - 1] = j;
    }

    Some((score, indices))
}

/// The bonus for matching the candidate's first character.
const PREFIX_BONUS: u32 = 10;

/// The bonus for matching the first character after a separator.
const WORD_START_BONUS: u32 = 8;

/// The score every pair of consecutive matched characters adds — constant
/// per pair, so a run of `k` matched characters earns `(k - 1) × RUN_BONUS`
/// and one long run beats the same pairs split across shorter runs.
///
/// It must exceed [`WORD_START_BONUS`]: the alignment weighs "extend the
/// run" against "restart at the next word start" cell by cell, and with
/// the run bonus the smaller, `app` on "Apple Pie" would paint `Ap` + `P`
/// rather than `App` — a contiguous region is the better match, and the
/// highlight has to say so.
pub(crate) const RUN_BONUS: u32 = 9;
const _: () = assert!(RUN_BONUS > WORD_START_BONUS);

/// What a character matched in the category region earns, as a divisor
/// over the score a title character would have earned in the same place
/// (base and run bonus alike, rounded up so every match still scores at
/// least 1). At 2, a word-start run in a category (`work` in "Workspace",
/// 23) sits below the same letters mid-word in a title (`work` in
/// "Framework tools", 31) and well below a title word start (39) — the
/// category is searchable, and it is also the lower-preference field.
const CATEGORY_DIVISOR: u32 = 2;

/// `score` as earned at char index `idx` of a `"{title} {category}"`
/// candidate whose title is `title_len` chars: unchanged inside the title,
/// divided by [`CATEGORY_DIVISOR`] from the separating space onward.
fn discounted(score: u32, idx: usize, title_len: usize) -> u32 {
    if idx >= title_len {
        score.div_ceil(CATEGORY_DIVISOR)
    } else {
        score
    }
}

/// A matched character's own score at `idx`, independent of what was
/// matched around it: 1, plus [`PREFIX_BONUS`] for the candidate's first
/// character, plus [`WORD_START_BONUS`] for the first character after a
/// separator.
fn char_base(cand: &[char], idx: usize) -> u32 {
    let mut score = 1;
    if idx == 0 {
        score += PREFIX_BONUS;
    }
    if idx > 0 && matches!(cand[idx - 1], ' ' | ':' | '_' | '-') {
        score += WORD_START_BONUS;
    }
    score
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
    /// Each item's lowercased match text — `"{title} {category}"`, the
    /// same `searchable_text` shape the keybindings and settings dialogs
    /// filter over — built once at construction so a filter pass does no
    /// per-item `title()` clone or `to_lowercase` (perf 11 — see
    /// [`fuzzy_match_lowered`]).
    lowered: Vec<String>,
    /// Each item's lowered title length in chars: where `lowered`'s title
    /// ends and its category begins, the boundary [`fuzzy_match_lowered`]
    /// discounts past and [`split_label_indices`] splits the highlight at.
    title_len: Vec<usize>,
    /// Each item's usage bonus (`crate::palette_usage`), baked once at
    /// construction against the clock as it stood when the palette opened
    /// — a filter pass adds this integer to each match's score and never
    /// looks a key up. All zero from [`PaletteState::new`].
    bonus: Vec<u32>,
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
    /// A palette with no usage history: every row's bonus is 0, so an
    /// empty query is registry order and a typed one is score order.
    pub fn new(items: Vec<PaletteItem>) -> Self {
        let bonus = vec![0; items.len()];
        Self::with_bonus(items, bonus)
    }

    /// A palette ranked by `usage` as of `now` (unix seconds): each row's
    /// bonus is read once here and added to its match score on every
    /// filter pass — an empty query lists the used rows first, best bonus
    /// first, and the rest in registry order; a typed query lets a
    /// well-used row edge past a marginally better textual match, within
    /// the bound the module doc states (`palette_usage::MAX_BONUS`).
    pub fn with_usage(
        items: Vec<PaletteItem>,
        usage: &crate::palette_usage::PaletteUsage,
        now: u64,
    ) -> Self {
        let bonus = items
            .iter()
            .map(|item| usage.bonus(&item.usage_key(), now))
            .collect();
        Self::with_bonus(items, bonus)
    }

    fn with_bonus(items: Vec<PaletteItem>, bonus: Vec<u32>) -> Self {
        let mut lowered = Vec::with_capacity(items.len());
        let mut title_len = Vec::with_capacity(items.len());
        for item in &items {
            let mut text = item.title().to_lowercase();
            title_len.push(text.chars().count());
            text.push(' ');
            text.push_str(&item.category().to_lowercase());
            lowered.push(text);
        }
        let mut state = PaletteState {
            items,
            lowered,
            title_len,
            bonus,
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
            if let Some((score, indices)) = fuzzy_match_lowered(&query, lowered, self.title_len[i])
            {
                scored.push((i, score + self.bonus[i], indices));
            }
        }
        // Stable sort: ties — including an empty query, where every item
        // scores the same 0 plus its usage bonus — keep their original
        // `items` order (the brief's "empty query returns all in registry
        // order", now after the rows a trader has actually used).
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
    /// which row was hit, and the keyboard path too, which lands the row
    /// `vimnav::apply` answered (`handle_palette_key`; spec §20.5: a bare
    /// ±1 wraps, anything larger clamps). Defensively clamped: an empty
    /// filtered list leaves `selected` at 0 untouched, and an
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

    /// Every item whose title or category fuzzy-matches the current
    /// query, best score plus usage bonus first, paired with the matched
    /// char indices over `"{title} {category}"`. Ties — including an empty
    /// query, where every item scores its bonus alone — keep their
    /// original `items` order (see `recompute_filtered`). A cache read
    /// (perf 11 — see the struct doc): no matching happens here, only a
    /// walk of the stored result. The signature still returns owned index
    /// `Vec`s (a handful of small clones) rather than borrows purely to
    /// keep the pre-cache API shape for tests; `render` walks
    /// [`PaletteState::rows`] instead, which borrows.
    pub fn filtered(&self) -> Vec<(&PaletteItem, Vec<usize>)> {
        self.filtered
            .iter()
            .map(|(i, indices)| (&self.items[*i], indices.clone()))
            .collect()
    }

    /// [`filtered`](Self::filtered) without the clones — the render's
    /// per-frame walk: each row's item, its matched indices over
    /// `"{title} {category}"`, and the title length the matcher scored
    /// against (the lowered title's char count), which is what
    /// [`split_label_indices`] must split at for the highlight to agree
    /// with the alignment by construction.
    pub fn rows(&self) -> impl Iterator<Item = (&PaletteItem, &[usize], usize)> {
        self.filtered
            .iter()
            .map(|(i, indices)| (&self.items[*i], indices.as_slice(), self.title_len[*i]))
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
/// (`ThemeService::names`' sorted order) as a `Theme` row; then every saved
/// scope (`saved`'s own `BTreeMap` order — spelling, per Phase 4a's
/// interface note) as a `Scope` row.
pub fn build_items(
    registry: &ActionRegistry,
    theme: &ThemeService,
    bindings: &BTreeMap<ActionId, String>,
    saved: &geode_core::scopes::SavedScopes,
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
    items.extend(saved.keys().cloned().map(PaletteItem::Scope));
    items
}

/// Split ranked `indices` (char offsets into a row's `searchable_text`,
/// `"{title} {category}"` — see `searchable_text` in this module and in
/// `settings_view`) back across the two label lines a row paints them on.
/// `title_len` is the title's own char count; the offset at exactly
/// `title_len` is the separating space and belongs to neither returned
/// list. Shared by both list dialogs' `build` (`keybindings_view` and
/// `settings_view`) rather than duplicated: the arithmetic is only
/// correct as long as *both* modules' `searchable_text` stays
/// `"{title} {category}"`, so one copy is what keeps a future separator
/// change from silently mis-highlighting whichever module didn't get the
/// memo.
pub(crate) fn split_label_indices(indices: &[usize], title_len: usize) -> (Vec<usize>, Vec<usize>) {
    let title_ix = indices.iter().copied().filter(|&i| i < title_len).collect();
    let cat_ix = indices
        .iter()
        .filter(|&&i| i > title_len)
        .map(|&i| i - title_len - 1)
        .collect();
    (title_ix, cat_ix)
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

/// Sizing hint only (no longer a selection clamp — every motion goes
/// through `vimnav::apply`, spec §20.5): the number of rows the results viewport
/// is tall enough to show before it needs to scroll. Item count today is
/// 84 (`register_builtin_actions`' 40 actions + `theme::load_bundled`'s 44
/// bundled theme entries — counted directly, not estimated, by
/// `build_binding_index_and_items_cover_the_whole_registry_and_theme_set`),
/// well within what a plain scrollable `div`
/// handles without virtualization — see `ROW_HEIGHT` below for how this
/// becomes a pixel height.
///
/// `pub(crate)` since the dimension pickers (Phase 4a §3.3): `shell::
/// picker`'s values list is a `uniform_list`, sized to this same rhythm
/// rather than growing its own rows-visible constant.
pub(crate) const VISIBLE_ROWS: usize = 12;

/// Estimated row height in pixels (`px_2`/`py_1` padding plus one line of
/// default-size text) — used only to size the scrollable viewport to
/// [`VISIBLE_ROWS`] rows; not load-bearing for correctness the way it would
/// be for a hand-rolled offset calculation, because scroll-follow here goes
/// through `gpui::ScrollHandle::scroll_to_item`, which measures real
/// per-row layout bounds rather than trusting this estimate.
///
/// `pub(crate)` — see [`VISIBLE_ROWS`]'s own doc comment.
pub(crate) const ROW_HEIGHT: f32 = 28.0;

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
pub(crate) fn highlighted_title(title: &str, indices: &[usize], primary: gpui::Hsla) -> StyledText {
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
/// **Mouse** (palette-input-polish task; dispatch added 2026-09-12): each
/// row's `on_mouse_down` hands its index to `on_row_click` (a
/// caller-supplied, cheaply `Clone`-able closure — see
/// [`ShellView::render`](../shell/struct.ShellView.html)'s call site for
/// how it's built from a `WeakEntity<ShellView>`, sidestepping per-row heap
/// allocation), which SELECTS the row and then COMMITS it — the mouse form
/// of Enter, through the same `ShellView::commit_selected` door the key
/// takes (the interaction model's §17.1 rule 2, adopted here by user
/// request: a click that only moved the highlight left a mouse user one
/// keystroke short of everything). The panel's
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
/// selection-change path (`set_selected` after `vimnav::apply`, the
/// selection reset in `set_query`, and row clicks via `set_selected`) calls `scroll_handle.
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

    let row_count = state.filtered.len();

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
            (row_count.max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)
        ))
        .overflow_y_scroll()
        .track_scroll(scroll_handle)
        // Test-only (no-op outside `cfg(test)`/`test-support`, see gpui's
        // `debug_selector` doc comment): lets a `#[gpui::test]` recover
        // this container's painted bounds via `VisualTestContext::
        // debug_bounds` and check a row's bounds actually fall inside it
        // — i.e. that scroll-follow, not just selection, moved.
        .debug_selector(|| "palette-list".to_string());
    if row_count == 0 {
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_color(theme.muted_foreground)
                .child("No matches"),
        );
    } else {
        for (i, (item, indices, title_len)) in state.rows().enumerate() {
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
            // The indices are over `"{title} {category}"`, ascending, so
            // each label paints only its own half: the title's are a
            // prefix slice (no allocation) and the category's are rebased
            // past the separating space — the same split
            // `split_label_indices` makes for the dialogs, done in place
            // here because this runs once per row per frame. A category
            // match glows in the category, not off the end of the title.
            let split = indices.partition_point(|&ix| ix < title_len);
            let title_ix = &indices[..split];
            let cat_ix: Vec<usize> = indices[split..]
                .iter()
                .filter(|&&ix| ix > title_len)
                .map(|&ix| ix - title_len - 1)
                .collect();
            let label = h_flex()
                .gap_2()
                .items_center()
                .child(div().child(highlighted_title(&item.title(), title_ix, theme.primary)))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(highlighted_title(item.category(), &cat_ix, theme.primary)),
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
        // Spec §20.5: reclaims `tab`/`shift-tab` from gpui-component's
        // `Root` (`dialog::init_reclaimed_keybindings`'s `"GeodePalette"`
        // entries) so they cannot cycle focus off `query_input`.
        .key_context("GeodePalette")
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

    /// A keyboard step exactly as `handle_palette_key`'s fallback arm
    /// spells it (spec §20.5): `vimnav::apply` decides the row — a bare
    /// ±1 wraps, anything larger clamps — and `set_selected` lands it.
    /// `PaletteState` has no motion method of its own any more.
    fn step(state: &mut PaletteState, delta: i64) {
        let next = crate::vimnav::apply(
            state.selected(),
            state.filtered().len(),
            crate::vimnav::NavCommand::Move(delta),
        );
        state.set_selected(next);
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
        // indices with gaps, not one run — but the alignment still
        // prefers the "pl" run at 3-4 over the earlier, lonelier "p" at 2.
        let (_, indices) = fuzzy_match("spl", "supplier").unwrap();
        assert_eq!(indices, vec![0, 3, 4]);
    }

    #[test]
    fn indices_prefer_a_later_contiguous_run_over_an_earlier_scattered_one() {
        // The settings row "Add tile" / "Tiling" is matched over the
        // joined text "add tile tiling" (a0 d1 d2 ' '3 t4 i5 l6 e7 ' '8
        // t9 i10 l11 i12 n13 g14). A greedy-leftmost walk claims the `l`
        // of "tile" (6) and then scatters i(10) n(13) g(14) across
        // "tiling" — painting `Add ti[l]e` / `T[i]li[ng]` — when "tiling"
        // holds the whole query as one run at 11..=14, which is what a
        // trader typing `ling` means and expects to see highlighted.
        let (score, indices) = fuzzy_match("ling", "add tile tiling").unwrap();
        assert_eq!(indices, vec![11, 12, 13, 14]);
        // And that alignment IS the score the row ranks by, not a
        // separately-derived highlight: the same run scores at least
        // what a lone "tiling" candidate scores for it.
        let (alone, _) = fuzzy_match("ling", "tiling").unwrap();
        assert!(
            score >= alone,
            "joined={score} should carry the run's own score {alone}"
        );
    }

    #[test]
    fn a_single_long_run_beats_the_same_pairs_split_across_runs() {
        // "abcd" as one run (three consecutive pairs) against "abcd"
        // split as "ab" + "cd" (two pairs) by a NON-separator — a `_`
        // would hand `c` a word-start bonus and confound the comparison:
        // the run bonus is per consecutive pair, so the unbroken run
        // scores strictly higher.
        let (one_run, ix1) = fuzzy_match("abcd", "abcd").unwrap();
        let (two_runs, ix2) = fuzzy_match("abcd", "abxcd").unwrap();
        assert_eq!(ix1, vec![0, 1, 2, 3]);
        assert_eq!(ix2, vec![0, 1, 3, 4]);
        assert!(one_run > two_runs, "one_run={one_run} two_runs={two_runs}");
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
                "workspace::close_tile",
                "Close tile",
                "Workspace",
                Some("ctrl+w"),
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
            action("workspace::close_tile", "Close tile", "Workspace", None),
            // Under "Workspace" this row WOULD match: `close` is a
            // subsequence of "focus left workspace" once the category
            // joins the match text, so the fixture keeps it out of reach.
            action("workspace::focus_left", "Focus left", "Tiling", None),
            PaletteItem::Theme("Gruvbox Dark".to_string()),
        ]);
        state.set_query("close");
        let titles: Vec<String> = state
            .filtered()
            .iter()
            .map(|(item, _)| item.title())
            .collect();
        assert_eq!(titles, vec!["Close tile".to_string()]);
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
        step(&mut state, 1);
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
    fn a_bare_step_wraps_at_both_ends() {
        let mut state = PaletteState::new(vec![
            action("a", "A", "Test", None),
            action("b", "B", "Test", None),
            action("c", "C", "Test", None),
        ]);
        assert_eq!(state.selected(), 0);
        // Up at index 0 wraps to the last item
        step(&mut state, -1);
        assert_eq!(state.selected(), 2, "up at 0 should wrap to last index");

        // Down at the last item wraps to 0
        step(&mut state, 1);
        assert_eq!(state.selected(), 0, "down at last should wrap to 0");
    }

    #[test]
    fn a_bare_step_wraps_up_from_start_with_large_list() {
        // 70+ item list to test wrapping with a large dataset
        const ITEM_COUNT: usize = 75;
        let items: Vec<PaletteItem> = (0..ITEM_COUNT)
            .map(|i| action(&format!("a{i}"), &format!("Item {i}"), "Test", None))
            .collect();
        let mut state = PaletteState::new(items);
        assert_eq!(state.selected(), 0);
        // Up at index 0 wraps to last
        step(&mut state, -1);
        assert_eq!(
            state.selected(),
            ITEM_COUNT - 1,
            "up from 0 should wrap to last with a large list"
        );
    }

    #[test]
    fn a_bare_step_wraps_down_from_end_with_large_list() {
        // 70+ item list to test wrapping with a large dataset
        const ITEM_COUNT: usize = 75;
        let items: Vec<PaletteItem> = (0..ITEM_COUNT)
            .map(|i| action(&format!("a{i}"), &format!("Item {i}"), "Test", None))
            .collect();
        let mut state = PaletteState::new(items);
        // Move to last item (index ITEM_COUNT - 1 = 74)
        for _ in 0..(ITEM_COUNT - 1) {
            step(&mut state, 1);
        }
        assert_eq!(state.selected(), ITEM_COUNT - 1);
        // Down at last wraps to 0
        step(&mut state, 1);
        assert_eq!(
            state.selected(),
            0,
            "down from last should wrap to 0 with a large list"
        );
    }

    #[test]
    fn a_bare_step_on_a_single_item_wraps_to_itself() {
        let items = vec![action("a", "Only Item", "Test", None)];
        let mut state = PaletteState::new(items);
        assert_eq!(state.selected(), 0);
        // Up wraps to itself
        step(&mut state, -1);
        assert_eq!(state.selected(), 0, "single item up should stay at 0");
        // Down wraps to itself
        step(&mut state, 1);
        assert_eq!(state.selected(), 0, "single item down should stay at 0");
    }

    #[test]
    fn a_bare_step_on_an_empty_result_set_does_not_panic() {
        let mut state = PaletteState::new(vec![action("a", "Focus left", "Workspace", None)]);
        state.set_query("nomatch");
        assert!(state.filtered().is_empty());
        step(&mut state, 1);
        step(&mut state, -1);
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
        step(&mut state, 1);
        assert_eq!(state.selected(), 1);
        state.set_query("a");
        assert_eq!(state.selected(), 0);

        step(&mut state, 1);
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
        step(&mut state, 1);
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
        step(&mut state, 1);
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
            "workspace::close_tile",
            "Close tile",
            "Workspace",
            Some("ctrl+w"),
        );
        assert_eq!(item.title(), "Close tile");
        assert_eq!(item.category(), "Workspace");
        assert_eq!(item.binding(), Some("ctrl+w"));
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

        let items = build_items(
            &registry,
            &theme,
            &bindings,
            &geode_core::scopes::SavedScopes::new(),
        );

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

        let close = items
            .iter()
            .find(|item| matches!(item, PaletteItem::Action(id, ..) if id.0 == "workspace::close_tile"))
            .unwrap();
        assert_eq!(close.binding(), Some("ctrl+w"));
    }

    // -- split_label_indices ---------------------------------------------

    #[test]
    fn split_label_indices_partitions_around_the_separating_space() {
        // "Toggle palette Palette" — title "Toggle palette" is 14 chars
        // (indices 0..=13), index 14 is the separating space, category
        // "Palette" starts at 15.
        let title_len = "Toggle palette".chars().count();
        assert_eq!(title_len, 14);
        // One index from the title (0), the separator itself (14, must be
        // dropped by both sides), and one from the category (15, the
        // category's own first char).
        let (title_ix, cat_ix) = split_label_indices(&[0, 14, 15], title_len);
        assert_eq!(
            title_ix,
            vec![0],
            "the separator index must not land in the title half"
        );
        assert_eq!(
            cat_ix,
            vec![0],
            "a category-side index is rebased to be relative to the category's own start"
        );
    }

    #[test]
    fn split_label_indices_on_empty_indices_is_two_empty_lists() {
        let (title_ix, cat_ix) = split_label_indices(&[], 5);
        assert!(title_ix.is_empty());
        assert!(cat_ix.is_empty());
    }

    // -- category matching ----------------------------------------------

    #[test]
    fn a_category_only_match_is_found() {
        let mut state = PaletteState::new(vec![
            action("a", "Focus left", "Workspace", None),
            action("b", "Perf overlay", "Diagnostics", None),
        ]);
        state.set_query("workspace");
        let titles: Vec<String> = state.filtered().iter().map(|(i, _)| i.title()).collect();
        assert_eq!(titles, vec!["Focus left"]);
    }

    /// The ranking half of the category discount: `work` sits mid-word in
    /// "Framework tools" (no prefix or word-start bonus at all) and at a
    /// word start in the category "Workspace" — undiscounted, the category
    /// placement would score higher, so this only passes while a category
    /// character earns less than a title character.
    #[test]
    fn a_mid_word_title_match_outranks_a_word_start_category_match() {
        let mut state = PaletteState::new(vec![
            action("a", "Focus left", "Workspace", None),
            action("b", "Framework tools", "Tiling", None),
        ]);
        state.set_query("work");
        let titles: Vec<String> = state.filtered().iter().map(|(i, _)| i.title()).collect();
        assert_eq!(titles, vec!["Framework tools", "Focus left"]);
    }

    /// The alignment half: when both the title and the category contain the
    /// letters, the indices land in the title — the highlight must agree
    /// with the ranking's preference.
    #[test]
    fn indices_prefer_the_title_over_the_category() {
        let mut state = PaletteState::new(vec![action("b", "Framework tools", "Workspace", None)]);
        state.set_query("work");
        let filtered = state.filtered();
        let (_, indices) = &filtered[0];
        assert_eq!(indices, &vec![5, 6, 7, 8]);
    }

    /// A query can span the title and the category — `left work` finds
    /// "Focus left" under "Workspace".
    #[test]
    fn a_query_can_span_title_and_category() {
        let mut state = PaletteState::new(vec![
            action("a", "Focus left", "Workspace", None),
            action("b", "Focus left", "Dock", None),
        ]);
        state.set_query("left work");
        let filtered = state.filtered();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0.category(), "Workspace");
        let (title_ix, cat_ix) = split_label_indices(&filtered[0].1, "Focus left".chars().count());
        assert_eq!(title_ix, vec![6, 7, 8, 9]);
        assert_eq!(cat_ix, vec![0, 1, 2, 3]);
    }

    // -- usage ranking --------------------------------------------------

    use crate::palette_usage::PaletteUsage;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn usage_key_names_the_kind_and_the_identity() {
        assert_eq!(
            action("workspace::close_tile", "Close tile", "Workspace", None).usage_key(),
            "action:workspace::close_tile"
        );
        assert_eq!(
            PaletteItem::Theme("Gruvbox Dark".to_string()).usage_key(),
            "theme:Gruvbox Dark"
        );
        assert_eq!(
            PaletteItem::Scope("eu-books".to_string()).usage_key(),
            "scope:eu-books"
        );
    }

    #[test]
    fn an_empty_query_lists_used_items_first_by_bonus_then_registry_order() {
        let items = vec![
            action("a", "Alpha", "Test", None),
            action("b", "Beta", "Test", None),
            action("c", "Gamma", "Test", None),
            action("d", "Delta", "Test", None),
        ];
        let mut usage = PaletteUsage::new();
        usage.record("action:c", NOW - 3 * 24 * 60 * 60);
        usage.record("action:d", NOW);
        let state = PaletteState::with_usage(items, &usage, NOW);
        let titles: Vec<String> = state.filtered().iter().map(|(i, _)| i.title()).collect();
        assert_eq!(titles, vec!["Delta", "Gamma", "Alpha", "Beta"]);
    }

    /// `Tiles` and `Toggle` both take `t` as a prefix hit — a tie on
    /// score, which usage breaks in favour of the row chosen before.
    #[test]
    fn a_used_item_wins_a_tie_on_score() {
        let items = vec![
            action("tiles", "Tiles", "Test", None),
            action("toggle", "Toggle", "Test", None),
        ];
        let mut usage = PaletteUsage::new();
        usage.record("action:toggle", NOW);
        let mut state = PaletteState::with_usage(items, &usage, NOW);
        state.set_query("t");
        let titles: Vec<String> = state.filtered().iter().map(|(i, _)| i.title()).collect();
        assert_eq!(titles, vec!["Toggle", "Tiles"]);
    }

    /// The bonus is bounded at two run bonuses: even at the cap it cannot
    /// drag a bare scattered match (`tog` mid-word across "Batch log",
    /// three points) past a never-used row's contiguous prefix run.
    #[test]
    fn the_capped_bonus_cannot_lift_a_scattered_match_over_a_contiguous_run() {
        let items = vec![
            action("scattered", "Batch log", "Test", None),
            action("toggle", "Toggle", "Test", None),
        ];
        let mut usage = PaletteUsage::new();
        for _ in 0..20 {
            usage.record("action:scattered", NOW);
        }
        let mut state = PaletteState::with_usage(items, &usage, NOW);
        state.set_query("tog");
        let titles: Vec<String> = state.filtered().iter().map(|(i, _)| i.title()).collect();
        assert_eq!(titles, vec!["Toggle", "Batch log"]);
    }

    #[test]
    fn new_ranks_with_no_usage_at_all() {
        let mut usage = PaletteUsage::new();
        usage.record("action:b", NOW);
        let items = vec![
            action("a", "Alpha", "Test", None),
            action("b", "Beta", "Test", None),
        ];
        let plain = PaletteState::new(items.clone());
        let used = PaletteState::with_usage(items, &usage, NOW);
        assert_eq!(plain.filtered()[0].0.title(), "Alpha");
        assert_eq!(used.filtered()[0].0.title(), "Beta");
    }

    /// The bound where the usage cap and the category discount meet,
    /// pinned as the ruling it is (review 2026-09-12): a maxed-out row
    /// whose only hit is in its category leads an unused title prefix on a
    /// SHORT query (`wor`: 17 + 18 against 31) — that is the brain-reading
    /// point — and loses to it once the prefix run reaches five characters
    /// (`works`: 29 + 18 against 51). At four they tie and registry order
    /// holds. Move either constant and this is the test that says so.
    #[test]
    fn a_used_category_hit_leads_a_short_title_prefix_and_loses_to_a_long_one() {
        let items = vec![
            action("focus", "Focus left", "Workspace", None),
            action("next", "Workspace: next", "Tiling", None),
        ];
        let mut usage = PaletteUsage::new();
        for _ in 0..20 {
            usage.record("action:focus", NOW);
        }
        let mut state = PaletteState::with_usage(items, &usage, NOW);
        state.set_query("wor");
        let titles: Vec<String> = state.filtered().iter().map(|(i, _)| i.title()).collect();
        assert_eq!(titles, vec!["Focus left", "Workspace: next"]);
        state.set_query("works");
        let titles: Vec<String> = state.filtered().iter().map(|(i, _)| i.title()).collect();
        assert_eq!(titles, vec!["Workspace: next", "Focus left"]);
    }

    /// The backtrack must use the same discounted constants as the
    /// forward pass: with the plain `RUN_BONUS` there, `ic` on
    /// "Perf overlay" / "Diagnostics" keeps its score but paints the
    /// title's `i` plus the category's `c` — a scattered highlight for a
    /// contiguous category match (review 2026-09-12).
    #[test]
    fn a_contiguous_category_match_backtracks_to_one_run() {
        let mut state =
            PaletteState::new(vec![action("perf", "Perf overlay", "Diagnostics", None)]);
        state.set_query("ic");
        let filtered = state.filtered();
        let title_len = "Perf overlay".chars().count();
        let (title_ix, cat_ix) = split_label_indices(&filtered[0].1, title_len);
        assert!(title_ix.is_empty(), "{title_ix:?}");
        assert_eq!(cat_ix, vec![8, 9]);
    }

    /// `rows()` is the render's borrow-only walk of the cache: the same
    /// order and indices `filtered()` hands out, plus each row's title
    /// length so the render splits the highlight without recounting.
    #[test]
    fn rows_borrow_the_same_result_filtered_clones() {
        let mut state = PaletteState::new(vec![
            action("a", "Focus left", "Workspace", None),
            action("b", "Framework tools", "Tiling", None),
        ]);
        state.set_query("work");
        let cloned = state.filtered();
        let rows: Vec<_> = state.rows().collect();
        assert_eq!(rows.len(), cloned.len());
        for ((item, indices), (row_item, row_indices, title_len)) in cloned.iter().zip(&rows) {
            assert_eq!(*item, *row_item);
            assert_eq!(indices, row_indices);
            assert_eq!(*title_len, item.title().chars().count());
        }
    }
}
