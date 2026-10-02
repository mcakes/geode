//! Command-palette items, ranking, display labels, and rendering.
//!
//! The shell creates a fresh snapshot of actions, themes, saved scopes, binding
//! badges, and usage bonuses when opening the palette. Its Input owns text editing;
//! query changes update [`PaletteState`], which caches ranked matches. The renderer
//! reads that cache and delegates row clicks to the shell's commit handler.
//!
//! Matching uses lowercase `title + space + category`. Category contributions are
//! discounted and each surviving row receives a bounded usage bonus. Usage cannot
//! make a nonmatching item visible. Equal totals retain input order. The ranking
//! snapshot is fixed while open; changing the query does not refresh usage or rows.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::SharedString;

use crate::actions::{ActionId, ActionRegistry};
use crate::keymap::{Keymap, Keystroke};
use crate::shell::scale;
use crate::theme::ThemeService;

/// Palette category for theme rows.
const THEME_CATEGORY: &str = "Appearance";

/// Palette category for saved-scope rows.
const SCOPE_CATEGORY: &str = "Scope";

/// One dispatchable palette row: an action with title/category/binding
/// badge, a fully qualified bundled theme name, or a live saved-scope name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteItem {
    Action(ActionId, String, String, Option<Vec<Keystroke>>),
    Theme(String),
    /// Saved-scope name, loaded into the frame when selected.
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

    /// The action's keybinding (one keystroke or a sequence), if any —
    /// always `None` for a theme or saved-scope row, since both are only
    /// ever reached by selecting them in the palette itself.
    pub fn binding(&self) -> Option<&[Keystroke]> {
        match self {
            PaletteItem::Action(_, _, _, binding) => binding.as_deref(),
            PaletteItem::Theme(_) | PaletteItem::Scope(_) => None,
        }
    }
}

/// Case-insensitive fuzzy match returning the best score and its alignment.
///
/// A single-word query is a subsequence match: every query character must
/// appear in order. Scores reward prefix starts, starts after
/// space/colon/underscore/hyphen, and consecutive matches. Dynamic programming
/// chooses the best alignment; backtracking favors the earliest equal-score
/// endpoint and continuation of a run.
///
/// A query of several whitespace-separated words matches when every word
/// matches in any order on characters of its own, so `scope clear` finds
/// "Clear scope" and `scope scope` does not. [`ORDER_BONUS`] rewards the typed
/// order: either the whole query, spaces included, matches as one subsequence,
/// or the words' separate alignments fall one after another. Each word first
/// takes its best alignment; when two of those share a character, the words
/// are placed again one at a time, longest first, each on characters the
/// earlier ones left free. That placement is greedy, so a candidate a
/// different assignment would fit can still be missed.
///
/// Indices address characters in the lowercased candidate, not bytes or guaranteed
/// positions in the original text. Lowercase expansion can shift highlights.
/// An empty query matches with score zero and no indices. Whitespace in a
/// query without two words (leading, trailing, or alone) is literal.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<(u32, Vec<usize>)> {
    fuzzy_match_lowered(&query.to_lowercase(), &candidate.to_lowercase(), usize::MAX)
}

/// Match already-lowercased text without rebuilding it per candidate.
/// `title_len` is the lowered title's character length in `title + space + category`;
/// contributions at or beyond it are divided by CATEGORY_DIVISOR and rounded up.
/// Use usize::MAX when the whole candidate has title weighting.
pub(crate) fn fuzzy_match_lowered(
    query: &str,
    candidate: &str,
    title_len: usize,
) -> Option<(u32, Vec<usize>)> {
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.len() < 2 {
        return align(query, candidate, title_len, None);
    }
    // A word that matches nowhere fails every placement, the whole-query
    // subsequence included, so a row missing any word stops here.
    let alone = words
        .iter()
        .map(|w| align(w, candidate, title_len, None))
        .collect::<Option<Vec<_>>>()?;
    let by_word =
        combine_words(alone).or_else(|| align_words_disjoint(&words, candidate, title_len));
    let ordered = align(query, candidate, title_len, None).map(|(s, ix)| (s + ORDER_BONUS, ix));
    match (ordered, by_word) {
        (Some(o), Some(w)) if w.0 > o.0 => Some(w),
        (Some(o), _) => Some(o),
        (None, w) => w,
    }
}

/// Combine per-word alignments, given in query order: the scores sum and the
/// indices merge in ascending order. `None` when two words share a character,
/// since one letter cannot stand for two typed ones. [`ORDER_BONUS`] applies
/// when each word's alignment lies wholly after the previous word's.
fn combine_words(alignments: Vec<(u32, Vec<usize>)>) -> Option<(u32, Vec<usize>)> {
    let mut score = 0;
    let mut in_order = true;
    let mut last_end: Option<usize> = None;
    let mut indices = Vec::new();
    for (s, ix) in alignments {
        score += s;
        // `ix` is non-empty and ascending: a word has at least one char.
        in_order &= last_end.is_none_or(|end| ix[0] > end);
        last_end = ix.last().copied();
        indices.extend(ix);
    }
    indices.sort_unstable();
    if indices.windows(2).any(|w| w[0] == w[1]) {
        return None;
    }
    if in_order {
        score += ORDER_BONUS;
    }
    Some((score, indices))
}

/// Place the words one at a time, longest first (ties in query order), each
/// on characters no earlier word claimed. Runs only after the words' best
/// alignments collided; `None` when a word finds no free placement.
fn align_words_disjoint(
    words: &[&str],
    candidate: &str,
    title_len: usize,
) -> Option<(u32, Vec<usize>)> {
    let mut claimed = vec![false; candidate.chars().count()];
    let mut by_length: Vec<usize> = (0..words.len()).collect();
    by_length.sort_by_key(|&w| std::cmp::Reverse(words[w].chars().count()));
    let mut placed: Vec<Option<(u32, Vec<usize>)>> = vec![None; words.len()];
    for w in by_length {
        let (s, ix) = align(words[w], candidate, title_len, Some(&claimed))?;
        for &j in &ix {
            claimed[j] = true;
        }
        placed[w] = Some((s, ix));
    }
    combine_words(placed.into_iter().collect::<Option<Vec<_>>>()?)
}

/// Subsequence alignment of the whole query, whitespace included; see
/// [`fuzzy_match_lowered`] for the other arguments. A candidate character
/// whose `claimed` entry is true cannot be matched.
fn align(
    query: &str,
    candidate: &str,
    title_len: usize,
    claimed: Option<&[bool]>,
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

    // Track best scores ending at each cell and within each prefix. Both
    // n-by-m tables are retained for alignment backtracking.
    let mut ends_at: Vec<Option<u32>> = vec![None; n * m];
    let mut within: Vec<Option<u32>> = vec![None; n * m];
    for i in 0..n {
        let mut best_so_far: Option<u32> = None;
        let mut any = false;
        for j in 0..m {
            let mut cell = None;
            if c[j] == q[i] && j >= i && !claimed.is_some_and(|taken| taken[j]) {
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

    // Backtrack through the same score rules, preferring a continuing run
    // when it attains the cell's score, otherwise the earliest matching endpoint.
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

/// The bonus for a multi-word query whose words appear in the typed order.
/// Worth a prefix start: the typed order outranks the same words reversed,
/// while a much tighter match in the wrong order can still outrank a
/// scattered one in the right order. It is not category-discounted.
const ORDER_BONUS: u32 = PREFIX_BONUS;

/// The bonus for matching the candidate's first character.
pub(crate) const PREFIX_BONUS: u32 = 10;

/// The bonus for matching the first character after a separator.
pub(crate) const WORD_START_BONUS: u32 = 8;

/// Bonus per consecutive matched pair. It exceeds WORD_START_BONUS so
/// continuing a run beats restarting at another word boundary in a score tie.
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

/// Merge matched character indices into contiguous byte ranges in the
/// original title. Skip indices outside its character count to avoid indexing
/// past the boundary table after lowercase expansion. This guards bounds but
/// does not map expanded lowercase characters back to their original positions;
/// highlight placement can differ for such text.
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

/// One ranked palette row as painted: its index into the items and its
/// highlight byte ranges over the prepared title and category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteRow {
    pub item: usize,
    pub title_runs: Vec<std::ops::Range<usize>>,
    pub category_runs: Vec<std::ops::Range<usize>>,
}

/// What the virtualized list closure owns: shared slices, so a paint clones
/// four reference counts and formats nothing.
#[derive(Clone)]
pub struct PaletteView {
    pub items: Rc<[PaletteItem]>,
    pub titles: Rc<[SharedString]>,
    pub categories: Rc<[SharedString]>,
    pub rows: Rc<[PaletteRow]>,
}

/// A fixed item/usage snapshot with cached fuzzy ranking and selection.
/// Construction and every set_query call recompute matches and the prepared
/// rows ([`view`](Self::view)). Selection and render accessors read the cache;
/// item text and usage bonuses cannot change in place, so the query is the only
/// ranking input that moves while the palette is open. The shell constructs
/// fresh state for each palette session, which is where new usage enters.
pub struct PaletteState {
    items: Rc<[PaletteItem]>,
    /// Each item's display title (`"Theme: …"`, `"Scope: …"` formatted here,
    /// once per open) and category, shared with the painted list.
    titles: Rc<[SharedString]>,
    categories: Rc<[SharedString]>,
    /// The ranked rows as the list paints them, rebuilt with `filtered`.
    view_rows: Rc<[PaletteRow]>,
    /// Each item's lowercased match text — `"{title} {category}"`, the
    /// same `searchable_text` shape the keybindings and settings dialogs
    /// filter over — built once at construction so a filter pass does no
    /// per-item `title()` clone or `to_lowercase` (perf 11 — see
    /// [`fuzzy_match_lowered`]).
    lowered: Vec<String>,
    /// Each item's lowered title length in chars: where `lowered`'s title
    /// ends and its category begins, the boundary [`fuzzy_match_lowered`]
    /// discounts past and `split_label_indices` splits the highlight at.
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
    /// Count actual matcher invocations so tests detect repeated matching on cache reads.
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

    /// Snapshot each item's usage bonus at `now` (Unix seconds), then rank.
    /// Bonus values stay fixed until a new PaletteState is constructed.
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
        let mut titles = Vec::with_capacity(items.len());
        let mut categories = Vec::with_capacity(items.len());
        for item in &items {
            let title = SharedString::from(item.title());
            let category = SharedString::from(item.category().to_string());
            let mut text = title.to_lowercase();
            title_len.push(text.chars().count());
            text.push(' ');
            text.push_str(&category.to_lowercase());
            lowered.push(text);
            titles.push(title);
            categories.push(category);
        }
        let mut state = PaletteState {
            items: items.into(),
            titles: titles.into(),
            categories: categories.into(),
            view_rows: Rc::from(Vec::new()),
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

    /// Recompute scores and alignments for the current query and frozen usage bonuses.
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
        // Usage bonuses rank previously chosen items first under an empty query.
        // Ties retain registry order.
        scored.sort_by_key(|(_, score, _)| std::cmp::Reverse(*score));
        self.filtered = scored
            .into_iter()
            .map(|(i, _, indices)| (i, indices))
            .collect();
        // The painted rows follow the ranking they were split from: each
        // label's highlight ranges, once per query, never per paint. The
        // indices are over `"{title} {category}"`, ascending, so the title's
        // are a prefix and the category's are rebased past the separating
        // space; a category match glows in the category, not off the end of
        // the title.
        self.view_rows = self
            .filtered
            .iter()
            .map(|(i, indices)| {
                let title_len = self.title_len[*i];
                let split = indices.partition_point(|&ix| ix < title_len);
                let category: Vec<usize> = indices[split..]
                    .iter()
                    .filter(|&&ix| ix > title_len)
                    .map(|&ix| ix - title_len - 1)
                    .collect();
                PaletteRow {
                    item: *i,
                    title_runs: highlight_runs(&self.titles[*i], &indices[..split]),
                    category_runs: highlight_runs(&self.categories[*i], &category),
                }
            })
            .collect::<Vec<_>>()
            .into();
    }

    /// The prepared list for the painter: shared slices, so taking it clones
    /// four reference counts.
    pub fn view(&self) -> PaletteView {
        PaletteView {
            items: self.items.clone(),
            titles: self.titles.clone(),
            categories: self.categories.clone(),
            rows: self.view_rows.clone(),
        }
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

    /// Replace the query, reset selection to row zero, and recompute matches.
    /// Even an identical supplied query reranks; the Input subscription owns edit
    /// notifications. Empty results leave the index at zero with no selected item.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
        self.recompute_filtered();
    }

    /// Set a filtered-row index, clamped to the last result or zero when empty.
    /// This handles stale click indices after filtering without indexing past the list.
    pub fn set_selected(&mut self, index: usize) {
        let len = self.filtered.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = index.min(len - 1);
    }

    /// Read ranked item references and cloned highlight indices from the cache.
    /// Scores include usage; equal scores retain original item order. No fuzzy matching
    /// runs here. [`Self::rows`] avoids index-vector clones for rendering.
    pub fn filtered(&self) -> Vec<(&PaletteItem, Vec<usize>)> {
        self.filtered
            .iter()
            .map(|(i, indices)| (&self.items[*i], indices.clone()))
            .collect()
    }

    /// [`filtered`](Self::filtered) without the clones — the raw cache the
    /// prepared [`view`](Self::view) is split from: each row's index into the
    /// unfiltered items (its stable element id), the item, its matched
    /// indices over `"{title} {category}"`, and the title length the matcher
    /// scored against (the lowered title's char count), which is what
    /// `split_label_indices` must split at for the highlight to agree with
    /// the alignment by construction.
    pub fn rows(&self) -> impl Iterator<Item = (usize, &PaletteItem, &[usize], usize)> {
        self.filtered
            .iter()
            .map(|(i, indices)| (*i, &self.items[*i], indices.as_slice(), self.title_len[*i]))
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

/// Render one keystroke back to the keymap's own spelling: `mods+key`,
/// modifiers in a fixed ctrl/alt/shift/cmd order, `+`-joined (e.g.
/// `"ctrl+shift+p"`). This is text for sentences, selectors and sort keys —
/// what a trader would type into a keymap file. A key painted on its own
/// goes through `shell::kbd` instead.
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

/// Build binding badges once at palette open. Keep the first compiled
/// binding encountered for each action ID. This is a display index: it neither
/// evaluates contexts nor checks whether a later binding shadows that key.
pub fn build_binding_index(keymap: &Keymap) -> BTreeMap<ActionId, Vec<Keystroke>> {
    let mut index = BTreeMap::new();
    for binding in keymap.bindings() {
        index
            .entry(binding.action.clone())
            .or_insert_with(|| binding.keystrokes.clone());
    }
    index
}

/// Append actions in registry-ID order, themes in sorted name order, and
/// live saved scopes in name order. Registered scope actions and live Scope rows
/// are both retained, so the same scope can appear through two dispatch routes.
pub fn build_items(
    registry: &ActionRegistry,
    theme: &ThemeService,
    bindings: &BTreeMap<ActionId, Vec<Keystroke>>,
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

/// Split indices in `title + space + category` across its two labels.
/// The separating space at `title_len` belongs to neither result. Callers must
/// use a title length and indices measured in the same character space.
pub(crate) fn split_label_indices(indices: &[usize], title_len: usize) -> (Vec<usize>, Vec<usize>) {
    let title_ix = indices.iter().copied().filter(|&i| i < title_len).collect();
    let cat_ix = indices
        .iter()
        .filter(|&&i| i > title_len)
        .map(|&i| i - title_len - 1)
        .collect();
    (title_ix, cat_ix)
}

// Rendering and shared highlight helpers.

use gpui::prelude::*;
use gpui::{
    App, Entity, FontWeight, HighlightStyle, IntoElement, MouseButton, Pixels, StyledText,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex};

/// Target panel width at the design rem size.
const WIDTH: f32 = 560.0;

/// Maximum viewport height in estimated rows. This does not truncate
/// results or clamp selection; the full result list is scrollable.
pub const VISIBLE_ROWS: usize = 12;

/// Estimated row height at the design rem size, used to size the viewport.
/// Scroll-follow uses the uniform list's measured row height.
pub(crate) const ROW_HEIGHT: f32 = 28.0;

/// What [`render`] needs to know about the window it paints into: the
/// drawable size in pixels, and the rem its own chrome lengths resolve
/// against (`shell::scale`).
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub rem_size: Pixels,
}

/// Render merged match spans in the supplied accent colour and bold weight.
/// Other spans inherit ambient text style. [`highlight_runs`] supplies UTF-8 byte
/// ranges and handles indices outside the original label's character count.
pub fn highlighted_title(title: &str, indices: &[usize], primary: gpui::Hsla) -> StyledText {
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

/// [`highlighted_title`] over prepared byte ranges and shared text: no index
/// conversion and no text copy at paint.
pub fn highlighted_runs(
    text: &gpui::SharedString,
    runs: &[std::ops::Range<usize>],
    primary: gpui::Hsla,
) -> StyledText {
    if runs.is_empty() {
        return StyledText::new(text.clone());
    }
    let style = HighlightStyle {
        color: Some(primary),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    StyledText::new(text.clone()).with_highlights(runs.iter().cloned().map(|r| (r, style)))
}

/// Render a centered palette panel with Input and a scrollable full result
/// list. The viewport shows at most VISIBLE_ROWS estimated rows; the list is a
/// `uniform_list` over the prepared [`PaletteView`], so only rows in view build
/// elements. Selection scrolling is the controller's responsibility
/// (`ScrollStrategy::Nearest` through the same handle).
///
/// Row clicks call the supplied handler with a filtered-row index; the shell
/// selects and commits through its ordinary palette dispatch path. The panel
/// stops mouse-down propagation so its enclosing outside-click handler cannot
/// dismiss it during an inside interaction. This function attaches the retained
/// scroll handle but does not mutate selection, focus, query, or scroll position.
pub fn render(
    state: &PaletteState,
    scroll_handle: &UniformListScrollHandle,
    query_input: &Entity<InputState>,
    on_row_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    viewport: Viewport,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let row_paint = crate::shell::listrow::row_paint(theme);
    let Viewport {
        width: viewport_width,
        height: viewport_height,
        rem_size,
    } = viewport;
    // The panel's own width and row heights are chrome lengths on the rem
    // scale (`shell::scale`); the viewport clamps stay in window pixels.
    let width = scale::design_px(WIDTH, rem_size).min((viewport_width - 32.0).max(160.0));
    let row_height = scale::design_px(ROW_HEIGHT, rem_size);
    let left = ((viewport_width - width) / 2.0).max(0.0);
    // Use the same proportional top position as shell modal dialogs.
    let top = (viewport_height * crate::shell::dialog::MODAL_TOP_RATIO).max(0.0);

    let view = state.view();
    let selected = state.selected();
    let row_count = view.rows.len();

    // A viewport capped at VISIBLE_ROWS estimates over a virtualized list:
    // only the rows in view build elements. Short results shrink the
    // viewport; empty results keep one message row.
    let list = if row_count == 0 {
        div()
            .id("palette-results")
            .w_full()
            .h(px(row_height))
            // Expose the list bounds to UI tests without changing production behavior.
            .debug_selector(|| "palette-list".to_string())
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(theme.muted_foreground)
                    .child("no matches"),
            )
            .into_any_element()
    } else {
        let muted = theme.muted_foreground;
        let radius = theme.radius;
        uniform_list("palette-results", row_count, move |range, _window, _cx| {
            range
                .map(|i| {
                    let shown = &view.rows[i];
                    let item_ix = shown.item;
                    let row = h_flex()
                        .id(("palette-row", item_ix))
                        .w_full()
                        .justify_between()
                        .items_center()
                        .gap_3()
                        .px_2()
                        .py_1()
                        .rounded(radius);
                    // The list-row tokens through the one door (`shell::listrow`):
                    // the highlighted row is the state, the hovered row is the
                    // pointer, and they are distinct fills.
                    let row = crate::shell::listrow::paint_row(row, row_paint, i == selected);
                    // Test-only, see `palette-list`'s `debug_selector` above.
                    let row = row.debug_selector(move || format!("palette-row-{i}"));
                    // The shell callback selects and commits this filtered row.
                    // Clone the callback so each closure keeps its own row index.
                    let click = on_row_click.clone();
                    let row = row.on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        click(i, window, cx);
                    });
                    // Prepared text and highlight ranges (`PaletteState::view`):
                    // the paint clones shared strings and formats nothing.
                    let label = h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().child(highlighted_runs(
                            &view.titles[item_ix],
                            &shown.title_runs,
                            row_paint.accent,
                        )))
                        .child(div().text_color(muted).child(highlighted_runs(
                            &view.categories[item_ix],
                            &shown.category_runs,
                            row_paint.accent,
                        )));
                    let binding = crate::shell::kbd::binding(
                        view.items[item_ix].binding().unwrap_or_default(),
                    );
                    row.child(label).child(binding).into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .w_full()
        .h(px(row_count.min(VISIBLE_ROWS) as f32 * row_height))
        .track_scroll(scroll_handle)
        // Expose the list bounds to UI tests without changing production behavior.
        .debug_selector(|| "palette-list".to_string())
        .into_any_element()
    };

    // Input supplies its own padding and search prefix. The outer row draws
    // only the separator so the query is not boxed inside another panel.
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
        // Reclaim Tab focus cycling through the GeodePalette key context.
        .key_context("GeodePalette")
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        // Use the shared dialog frame tokens and shadow. Interior spacing stays
        // compact for the palette's denser result rows.
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
        // Expose panel bounds so tests distinguish inside clicks from backdrop clicks.
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
            binding.map(|spec| {
                crate::keymap::parse_binding(spec, crate::keymap::Modifiers::NONE).unwrap()
            }),
        )
    }

    /// Apply the same resolved navigation and selection clamp as the palette controller.
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

    // -- fuzzy_match words ------------------------------------------------

    #[test]
    fn words_typed_out_of_order_still_match() {
        let (_, indices) = fuzzy_match("scope clear", "Clear scope").unwrap();
        assert_eq!(indices, vec![0, 1, 2, 3, 4, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn words_in_the_candidate_order_score_higher_than_reversed() {
        let (ordered, _) = fuzzy_match("clear scope", "Clear scope").unwrap();
        let (reversed, _) = fuzzy_match("scope clear", "Clear scope").unwrap();
        assert!(ordered > reversed, "ordered={ordered} reversed={reversed}");
        // Across candidates too: the one holding the words in the typed
        // order outranks the one holding the same words reversed.
        let (in_order, _) = fuzzy_match("tile add", "tile add").unwrap();
        let (out_of_order, _) = fuzzy_match("tile add", "add tile").unwrap();
        assert!(
            in_order > out_of_order,
            "in_order={in_order} out_of_order={out_of_order}"
        );
    }

    #[test]
    fn words_in_order_without_a_literal_space_between_them_earn_the_order_bonus() {
        let (split, _) = fuzzy_match("clear scope", "clearscope").unwrap();
        let (reversed, _) = fuzzy_match("scope clear", "clearscope").unwrap();
        assert!(split > reversed, "split={split} reversed={reversed}");
    }

    #[test]
    fn every_word_must_match() {
        assert_eq!(fuzzy_match("scope xyz", "Clear scope"), None);
    }

    #[test]
    fn words_claim_distinct_characters() {
        // A second "scope" cannot reuse the first one's letters.
        assert_eq!(fuzzy_match("scope scope", "Clear scope"), None);
        // A repeated prefix still matches when it has letters of its own:
        // each word's best alignment alone is the prefix, so the typed
        // order (space included) supplies the distinct characters.
        let (_, indices) = fuzzy_match("tile til", "tile tiling").unwrap();
        assert_eq!(indices, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn colliding_words_are_placed_again_on_free_characters() {
        // Both words' best alignment is the prefix, and reversed they are
        // no subsequence (no `e` after "til" in "tiling"): the longer word
        // keeps the prefix and `til` moves to "tiling".
        let (_, indices) = fuzzy_match("til tile", "tile tiling").unwrap();
        assert_eq!(indices, vec![0, 1, 2, 3, 5, 6, 7]);
        // The single-letter word takes the second `e` once "clear" has
        // claimed the first.
        let (_, indices) = fuzzy_match("e clear", "Clear scope").unwrap();
        assert_eq!(indices, vec![0, 1, 2, 3, 4, 10]);
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

    /// Lowercasing İ expands to two characters. Highlight conversion skips
    /// indices beyond the original label rather than indexing past its boundaries.
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
    fn query_words_in_either_order_find_the_action() {
        let mut state = PaletteState::new(vec![
            action("frame::scope_clear", "Clear scope", "Frame", None),
            action("frame::scope_edit", "Edit scope", "Frame", None),
        ]);
        state.set_query("scope clear");
        let titles: Vec<String> = state
            .filtered()
            .iter()
            .map(|(item, _)| item.title())
            .collect();
        assert_eq!(titles, vec!["Clear scope".to_string()]);
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

    /// Construction and query changes match each item once; repeated result
    /// reads must use the cache. The counter records actual matcher calls.
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

        // Repeated reads must not recompute fuzzy matches.
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
        assert_eq!(
            item.binding().map(render_binding).as_deref(),
            Some("ctrl+w")
        );
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
        // The compiler sorts key spellings within the builtin keys table.
        // Control-K precedes Control-Shift-P, so the first-binding badge uses it.
        assert_eq!(
            bindings
                .get(&ActionId("palette::toggle".to_string()))
                .map(|keys| render_binding(keys))
                .as_deref(),
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
        assert_eq!(
            close.binding().map(render_binding).as_deref(),
            Some("ctrl+w")
        );
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

    /// The capped bonus cannot lift this three-character scattered match
    /// above the fixture's contiguous prefix match.
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

    /// A maximum-usage category match beats an unused three-character title
    /// prefix, ties at four, and loses at five with the configured scoring constants.
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

    /// Backtracking must use category discounts too, or its highlighted
    /// alignment can disagree with the score computed by the forward pass.
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
        for ((item, indices), (_, row_item, row_indices, title_len)) in cloned.iter().zip(&rows) {
            assert_eq!(*item, *row_item);
            assert_eq!(indices, row_indices);
            assert_eq!(*title_len, item.title().chars().count());
        }
    }

    /// The prepared view is exactly what the old per-render loop computed:
    /// same order, same title and category highlight ranges, for a fixed
    /// query set — and the same under a usage snapshot that reorders it.
    #[test]
    fn the_prepared_palette_rows_match_the_old_render_loop() {
        let items = vec![
            action(
                "workspace::focus_left",
                "Focus left",
                "Workspace",
                Some("alt+h"),
            ),
            action(
                "palette::toggle",
                "Toggle palette",
                "Palette",
                Some("ctrl+k"),
            ),
            PaletteItem::Theme("Gruvbox Dark".into()),
            PaletteItem::Scope("my book".into()),
        ];
        let mut usage = crate::palette_usage::PaletteUsage::new();
        usage.record(&items[3].usage_key(), 1_000);
        let fresh = || PaletteState::new(items.clone());
        let used = || PaletteState::with_usage(items.clone(), &usage, 1_000);
        let mut category_hits = 0;
        for make in [&fresh as &dyn Fn() -> PaletteState, &used] {
            for query in ["", "fo", "th gr", "pal wk", "scope", "workspace", "zzz"] {
                let mut state = make();
                state.set_query(query);
                let view = state.view();
                category_hits += view
                    .rows
                    .iter()
                    .filter(|r| !r.category_runs.is_empty())
                    .count();
                let old: Vec<PaletteRow> = state
                    .rows()
                    .map(|(ix, item, indices, title_len)| {
                        let split = indices.partition_point(|&i| i < title_len);
                        let cat: Vec<usize> = indices[split..]
                            .iter()
                            .filter(|&&i| i > title_len)
                            .map(|&i| i - title_len - 1)
                            .collect();
                        PaletteRow {
                            item: ix,
                            title_runs: highlight_runs(&item.title(), &indices[..split]),
                            category_runs: highlight_runs(item.category(), &cat),
                        }
                    })
                    .collect();
                assert_eq!(view.rows.to_vec(), old, "query {query:?}");
                for r in view.rows.iter() {
                    assert_eq!(view.titles[r.item].as_ref(), items[r.item].title());
                    assert_eq!(view.categories[r.item].as_ref(), items[r.item].category());
                    assert_eq!(view.items[r.item], items[r.item]);
                }
            }
        }
        assert!(category_hits > 0, "the query set reaches a category match");
        // The usage snapshot reorders the prepared rows, not only the cache.
        let state = used();
        assert_eq!(state.view().rows[0].item, 3);
    }
}
