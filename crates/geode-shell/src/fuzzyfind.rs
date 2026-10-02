//! A transient, tile-local search picker. Tiles supply results and identity-aware
//! reveal callbacks; the shell owns the query, focus, and session lifetime. A tile
//! may keep a weak handle while loading descendants without expanding its tree.

use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

mod rows;
use rows::Rows;
mod index;
pub use index::{FindIndex, FindLabel};
mod table;
use table::FindTable;

use gpui::prelude::*;
use gpui::{
    App, Context, EventEmitter, MouseButton, ScrollStrategy, SharedString, UniformListScrollHandle,
    Window, div, uniform_list,
};
use gpui_component::{ActiveTheme as _, h_flex};

use crate::{palette, shell::scale, vimnav};

mod scoring;
mod tree;
pub use tree::FindTree;
mod row;
pub use row::FindRow;
use std::collections::HashSet;

/// Space above the prompt for the selected ancestor path and result count,
/// in design pixels (scaled with the rest of the tile).
pub const CONTEXT_HEIGHT: f32 = 24.0;
/// Space the tile reserves for the search context and shell-owned input strip.
pub const FOOTER_HEIGHT: f32 = CONTEXT_HEIGHT + crate::shell::commandline_view::HEIGHT;

type Reveal = dyn Fn(&str, &mut Window, &mut App) -> Result<(), String>;

/// One result with a tile-owned reveal operation. The callback must resolve a
/// stable row identity, or reject a result that no longer exists.
#[derive(Clone)]
pub struct FindItem {
    id: SharedString,
    label: String,
    path: String,
    reveal: Rc<Reveal>,
}

impl FindItem {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<String>,
        path: impl Into<String>,
        reveal: impl Fn(&str, &mut Window, &mut App) -> Result<(), String> + 'static,
    ) -> Self {
        let label = label.into();
        let path = path.into();
        Self {
            id: id.into(),
            label,
            path,
            reveal: Rc::new(reveal),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn reveal(&self, query: &str, window: &mut Window, cx: &mut App) -> Result<(), String> {
        (self.reveal)(query, window, cx)
    }
}

type RevealRow = dyn Fn(usize, &str, &mut Window, &mut App) -> Result<(), String>;

#[derive(Clone)]
enum Items {
    Local(Rc<Vec<FindItem>>),
    Indexed(Arc<FindIndex>, Rc<RevealRow>),
}

impl Default for Items {
    fn default() -> Self {
        Self::Local(Rc::new(Vec::new()))
    }
}

impl Items {
    fn id(&self, row: usize) -> SharedString {
        match self {
            Self::Local(items) => items[row].id.clone(),
            Self::Indexed(index, _) => index.label(row).id.clone(),
        }
    }
    fn item(&self, row: usize) -> FindItem {
        match self {
            Self::Local(items) => items[row].clone(),
            Self::Indexed(index, reveal) => {
                let label = index.label(row);
                let reveal = reveal.clone();
                FindItem::new(
                    label.id.clone(),
                    label.label.clone(),
                    label.path.clone(),
                    move |query, window, cx| reveal(row, query, window, cx),
                )
            }
        }
    }
    fn path(&self, row: usize) -> String {
        match self {
            Self::Local(items) => items[row].path.clone(),
            Self::Indexed(index, _) => index.label(row).path.clone(),
        }
    }
}

/// A pointer pick. Keyboard confirmation uses the same selected result.
pub struct Pick;

pub struct FuzzyFind {
    items: Items,
    ranked: Rows,
    matches: Arc<Vec<usize>>,
    ordered: Rows,
    hits: Option<Arc<[bool]>>,
    tree: Option<Arc<FindTree>>,
    folded: HashSet<usize>,
    natural: Rows,
    query: String,
    ranked_query: String,
    selected: usize,
    loading: bool,
    error: Option<String>,
    scroll: UniformListScrollHandle,
    index: Arc<[SearchText]>,
    index_ready: bool,
    revision: Arc<AtomicU64>,
    task: Option<gpui::Task<()>>,
    pending: bool,
    active: bool,
    expanding: bool,
    table: Option<gpui::Entity<gpui_component::table::TableState<FindTable>>>,
    /// The tile's word on the results, shown in place of the match count.
    notice: Option<String>,
}

impl Default for FuzzyFind {
    fn default() -> Self {
        Self {
            items: Items::default(),
            ranked: Rows::All(0),
            matches: Arc::new(Vec::new()),
            ordered: Rows::All(0),
            hits: None,
            tree: None,
            folded: HashSet::new(),
            natural: Rows::All(0),
            query: String::new(),
            ranked_query: String::new(),
            selected: 0,
            loading: true,
            error: None,
            scroll: UniformListScrollHandle::new(),
            index: Arc::from([]),
            index_ready: true,
            revision: Arc::new(AtomicU64::new(0)),
            task: None,
            pending: false,
            active: true,
            expanding: false,
            table: None,
            notice: None,
        }
    }
}

impl EventEmitter<Pick> for FuzzyFind {}

impl FuzzyFind {
    pub fn replace_items(&mut self, items: Vec<FindItem>, cx: &mut Context<Self>) {
        self.install_items(items, None, cx);
    }

    pub fn replace_tree_items(
        &mut self,
        items: Vec<FindItem>,
        tree: Arc<FindTree>,
        cx: &mut Context<Self>,
    ) {
        assert_eq!(items.len(), tree.len());
        self.install_items(items, Some(tree), cx);
    }

    fn install_items(
        &mut self,
        items: Vec<FindItem>,
        tree: Option<Arc<FindTree>>,
        cx: &mut Context<Self>,
    ) {
        if !self.active {
            return;
        }
        let selected = self.selected_item().map(|item| item.id);
        self.tree = tree;
        self.folded.clear();
        self.index_ready = true;
        self.index = items
            .iter()
            .map(|item| SearchText::new(&item.label, &item.path))
            .collect();
        self.natural = Rows::All(items.len());
        self.items = Items::Local(Rc::new(items));
        self.ranked = Rows::All(0);
        self.ranked_query.clear();
        self.matches = Arc::new(Vec::new());
        self.ordered = Rows::All(0);
        self.hits = None;
        self.loading = false;
        self.error = None;
        self.sync_table(cx);
        self.rank(selected, cx);
    }

    /// Install prebuilt data with one shared reveal callback, avoiding a UI-thread
    /// allocation and closure for every candidate. Indices address the source rows.
    pub fn replace_index(
        &mut self,
        index: Arc<FindIndex>,
        reveal: impl Fn(usize, &str, &mut Window, &mut App) -> Result<(), String> + 'static,
        cx: &mut Context<Self>,
    ) {
        if !self.active {
            return;
        }
        let selected = self.selected_item().map(|item| item.id);
        let same_tree = self
            .tree
            .as_ref()
            .zip(index.tree.as_ref())
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b));
        if !same_tree {
            self.folded.clear();
            self.ranked = Rows::All(0);
            self.hits = None;
        }
        self.tree = index.tree.clone();
        self.index_ready = index.ready;
        self.index = index.search.clone();
        self.natural = index.natural.clone();
        self.items = Items::Indexed(index, Rc::new(reveal));
        self.ranked_query.clear();
        self.matches = Arc::new(Vec::new());
        self.ordered = Rows::All(0);
        self.loading = false;
        self.error = None;
        self.sync_table(cx);
        self.rank(selected, cx);
    }

    /// Already-loaded rows remain usable while more descendants are prepared.
    pub fn set_expanding(&mut self, expanding: bool, cx: &mut Context<Self>) {
        self.expanding = expanding;
        self.sync_table(cx);
        cx.notify();
    }

    pub fn fail(&mut self, error: impl Into<String>, cx: &mut Context<Self>) {
        self.revision.fetch_add(1, Ordering::Relaxed);
        self.task = None;
        self.pending = false;
        self.expanding = false;
        self.loading = false;
        self.items = Items::default();
        self.tree = None;
        self.folded.clear();
        self.index = Arc::from([]);
        self.index_ready = true;
        self.ranked = Rows::All(0);
        self.ranked_query.clear();
        self.matches = Arc::new(Vec::new());
        self.ordered = Rows::All(0);
        self.hits = None;
        self.natural = self.ranked.clone();
        self.error = Some(error.into());
        self.sync_table(cx);
        cx.notify();
    }

    pub fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if self.query == query {
            return;
        }
        self.query = query;
        self.folded.clear();
        self.rank(None, cx);
    }

    fn rank(&mut self, selected: Option<SharedString>, cx: &mut Context<Self>) {
        let revision = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
        self.task = None;
        if self.query.trim().is_empty() {
            if self.folded.is_empty() {
                self.apply_ranked(self.natural.clone(), selected, cx);
            } else {
                // Finishing deferred indexing must not briefly reopen folds.
                self.matches = Arc::new(Vec::new());
                self.ordered = self.natural.clone();
                self.hits = None;
                self.filter_branches(None, cx);
            }
            return;
        }
        self.pending = true;
        if let Some(table) = &self.table {
            table.update(cx, |table, cx| {
                table.delegate_mut().message = "Searching…".into();
                cx.notify();
            });
        }
        if !self.index_ready {
            // Keep the complete displayed tree while its search text is built.
            // The latest query is ranked when the ready index is installed.
            cx.notify();
            return;
        }
        let index = self.index.clone();
        let query = self.query.trim().to_lowercase();
        // Only a single-word extension guarantees a subset. Multi-word matching
        // uses disjoint greedy placements, so it must examine the full index.
        let candidates = can_narrow(&self.ranked_query, &query).then(|| self.matches.clone());
        let current = self.revision.clone();
        let tree = self.tree.clone();
        let folded = self.folded.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let ranked = cx
                .background_executor()
                .spawn(async move {
                    let matches = Arc::new(rank_index(
                        &index,
                        &query,
                        candidates.as_deref().map(Vec::as_slice),
                        &current,
                        revision,
                    )?);
                    let (ordered, hits) = match &tree {
                        Some(tree) => {
                            let (order, hits) = tree.rank(&matches, &current, revision)?;
                            (Rows::Ranked(order), Some(hits))
                        }
                        None => (Rows::Ranked(matches.clone()), None),
                    };
                    let visible = match &tree {
                        Some(tree) => tree.visible(&ordered, &folded, &current, revision)?,
                        None => ordered.clone(),
                    };
                    Some((matches, ordered, hits, visible))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.revision.load(Ordering::Relaxed) == revision
                    && let Some((matches, ordered, hits, visible)) = ranked
                {
                    this.matches = matches;
                    this.ordered = ordered;
                    this.hits = hits;
                    this.show_rows(visible, selected, cx);
                }
            });
        }));
        cx.notify();
    }

    fn apply_ranked(
        &mut self,
        ranked: Rows,
        selected: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.matches = Arc::new(Vec::new());
        self.ordered = ranked.clone();
        self.hits = None;
        self.show_rows(ranked, selected, cx);
    }

    fn show_rows(&mut self, ranked: Rows, selected: Option<SharedString>, cx: &mut Context<Self>) {
        self.pending = false;
        self.ranked = ranked;
        self.ranked_query = self.query.trim().to_lowercase();
        self.selected = selected
            .and_then(|id| self.ranked.iter().position(|row| self.items.id(row) == id))
            .or_else(|| {
                self.matches
                    .first()
                    .and_then(|best| self.ranked.iter().position(|row| row == *best))
            })
            .unwrap_or(0);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        self.sync_table(cx);
        cx.notify();
    }

    /// Install the tile's columns and its original header and cell presentation. The tile renders this
    /// entity in its table body, keeping its own header and original table alive.
    ///
    /// `on_rows` receives the source rows ([`FindRow::source_row`]) shown now, in display order: when
    /// the table's visible range changes, and at the next layout after any result-set change (typing,
    /// a fold, new items), the range clamped to the rows that remain. The tile formats those rows
    /// there and nowhere else; `render_cell` only reads what it prepared. It runs at table layout,
    /// never inside the tile's own update, so it may read the tile.
    pub fn set_table(
        &mut self,
        columns: Vec<gpui_component::table::Column>,
        render_header: impl Fn(usize, &mut Window, &mut App) -> gpui::AnyElement + 'static,
        render_cell: impl Fn(&FindRow<'_>, usize, &mut App) -> gpui::AnyElement + 'static,
        on_rows: impl Fn(&[usize], &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.table.is_none() {
            cx.observe_global::<crate::linenumbers::UiSettings>(|this, cx| {
                this.sync_table(cx);
                cx.notify();
            })
            .detach();
        }
        let delegate = FindTable::new(
            columns,
            Rc::new(render_header),
            Rc::new(render_cell),
            Rc::new(on_rows),
            cx.entity().downgrade(),
        );
        self.table = Some(cx.new(|cx| {
            gpui_component::table::TableState::new(delegate, window, cx)
                .row_selectable(false)
                .col_selectable(false)
                .cell_selectable(false)
                .col_movable(false)
                .col_resizable(false)
                .sortable(false)
        }));
        self.sync_table(cx);
        cx.notify();
    }

    /// Replace presentation after an asynchronous descendant load.
    pub fn update_table(
        &mut self,
        columns: Vec<gpui_component::table::Column>,
        render_header: impl Fn(usize, &mut Window, &mut App) -> gpui::AnyElement + 'static,
        render_cell: impl Fn(&FindRow<'_>, usize, &mut App) -> gpui::AnyElement + 'static,
        cx: &mut Context<Self>,
    ) {
        if let Some(table) = &self.table {
            table.update(cx, |table, cx| {
                table.delegate_mut().set_presentation(
                    columns,
                    Rc::new(render_header),
                    Rc::new(render_cell),
                );
                // New columns may change what the tile prepares per row.
                table.delegate_mut().mark_stale();
                table.refresh(cx);
                cx.notify();
            });
        }
    }

    /// End the session before releasing it: a rendered table may retain a view
    /// handle until the next frame, so release alone is not a dismissal signal.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        self.pending = false;
        self.revision.fetch_add(1, Ordering::Relaxed);
        self.task = None;
        cx.notify();
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// How many result rows the table shows now.
    pub fn result_count(&self) -> usize {
        self.ranked.len()
    }

    /// The tile's results changed under the same rows (a price refresh):
    /// re-report the rows shown at the next layout.
    pub fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        if let Some(table) = &self.table {
            table.update(cx, |table, cx| {
                table.delegate_mut().mark_stale();
                cx.notify();
            });
        }
    }

    /// Put the tile's word on the results in the status line, in place of
    /// the match count, until the session ends.
    pub fn set_notice(&mut self, notice: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(notice.into());
        cx.notify();
    }

    pub fn has_table(&self) -> bool {
        self.table.is_some()
    }

    pub fn context(&self) -> (String, String) {
        let path = self
            .ranked
            .get(self.selected)
            .map(|row| self.items.path(row))
            .unwrap_or_default();
        let status = if self.loading || self.pending || self.expanding {
            "Searching…".into()
        } else if let Some(error) = &self.error {
            error.clone()
        } else if let Some(notice) = &self.notice {
            notice.clone()
        } else {
            format!(
                "{} matches",
                if self.ranked_query.is_empty() {
                    self.natural.len()
                } else {
                    self.matches.len()
                }
            )
        };
        (path, status)
    }

    fn sync_table(&self, cx: &mut Context<Self>) {
        if let Some(table) = &self.table {
            table.update(cx, |table, cx| {
                let d = table.delegate_mut();
                d.items = self.items.clone();
                d.clear_highlights();
                d.ranked = self.ranked.clone();
                d.mark_stale();
                d.message = self.error.clone().unwrap_or_else(|| {
                    if self.loading || self.pending || self.expanding {
                        "Searching…"
                    } else {
                        "No matches"
                    }
                    .into()
                });
                if !self.ranked.is_empty() {
                    table.set_selected_row(self.selected, cx);
                }
                table
                    .vertical_scroll_handle
                    .scroll_to_item(self.selected, ScrollStrategy::Nearest);
                cx.notify();
            });
        }
    }

    /// Fold only the temporary search tree. Ranking is retained; hiding its
    /// descendants runs on the worker and cannot block input on a large branch.
    pub fn toggle_selected_branch(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.ranked.get(self.selected) {
            self.toggle_branch(row, cx);
        }
    }

    fn toggle_branch(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.pending || !self.tree.as_ref().is_some_and(|tree| tree.branch(row)) {
            return;
        }
        if !self.folded.remove(&row) {
            self.folded.insert(row);
        }
        self.filter_branches(Some(row), cx);
    }

    fn filter_branches(&mut self, selected_row: Option<usize>, cx: &mut Context<Self>) {
        let Some(tree) = self.tree.clone() else {
            return;
        };
        let selected = selected_row
            .or_else(|| self.ranked.get(self.selected))
            .map(|row| self.items.id(row));
        let revision = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
        let current = self.revision.clone();
        let order = self.ordered.clone();
        let folded = self.folded.clone();
        self.pending = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            let visible = cx
                .background_executor()
                .spawn(async move { tree.visible(&order, &folded, &current, revision) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.revision.load(Ordering::Relaxed) == revision
                    && let Some(visible) = visible
                {
                    this.show_rows(visible, selected, cx);
                }
            });
        }));
        cx.notify();
    }

    pub fn navigate(&mut self, command: vimnav::NavCommand, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.selected = vimnav::apply(self.selected, self.ranked.len(), command);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        if let Some(table) = &self.table {
            table.update(cx, |table, cx| {
                if !self.ranked.is_empty() {
                    table.set_selected_row(self.selected, cx);
                }
                table
                    .vertical_scroll_handle
                    .scroll_to_item(self.selected, ScrollStrategy::Nearest);
                cx.notify();
            });
        }
        cx.notify();
    }

    pub fn selected_item(&self) -> Option<FindItem> {
        if !self.active || self.pending || self.loading {
            return None;
        }
        self.ranked
            .get(self.selected)
            .map(|row| self.items.item(row))
    }

    fn indices(&self, row: usize) -> Vec<usize> {
        // Highlight the query that produced these rows while a new search runs.
        self.index
            .get(row)
            .map(|text| text.indices(&self.ranked_query))
            .unwrap_or_default()
    }

    pub fn query(&self) -> &str {
        &self.query
    }
}

impl Render for FuzzyFind {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::{Sizable as _, Size, table::DataTable};
        if let Some(table) = &self.table {
            return div()
                .size_full()
                .debug_selector(|| "fuzzy-find".to_string())
                .child(
                    DataTable::new(table)
                        .with_size(Size::XSmall)
                        .bordered(false)
                        .stripe(false),
                )
                .into_any_element();
        }
        let theme = cx.theme();
        let paint = crate::shell::listrow::row_paint(theme);
        let muted = theme.muted_foreground;
        let mut surface = div()
            .id("fuzzy-find")
            .size_full()
            .occlude()
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .debug_selector(|| "fuzzy-find".to_string());
        let message = if self.loading {
            Some("Searching…")
        } else if let Some(error) = &self.error {
            Some(error.as_str())
        } else if self.ranked.is_empty() {
            Some("No matches")
        } else {
            None
        };
        if let Some(message) = message {
            return surface
                .child(div().p_2().text_color(muted).child(message.to_string()))
                .into_any_element();
        }
        let entity = cx.entity().downgrade();
        let list = uniform_list(
            "fuzzy-find-results",
            self.ranked.len(),
            move |range, _, cx| {
                let Some(entity) = entity.upgrade() else {
                    return Vec::new();
                };
                let state = entity.read(cx);
                range
                    .map(|position| {
                        let row = state.ranked.get(position).unwrap();
                        let item = state.items.item(row);
                        let indices = state.indices(row);
                        let label_len = item.label.chars().count();
                        let label_indices: Vec<_> =
                            indices.iter().copied().filter(|&i| i < label_len).collect();
                        let path_indices: Vec<_> = indices
                            .iter()
                            .filter_map(|i| i.checked_sub(label_len + 1))
                            .collect();
                        let entity = entity.downgrade();
                        let row = h_flex()
                            .id(item.id.clone())
                            .w_full()
                            .h(scale::design(28.0))
                            .px_2()
                            .gap_3();
                        crate::shell::listrow::paint_row(row, paint, position == state.selected)
                            .text_color(paint.text)
                            .debug_selector(move || format!("find-result-{position}"))
                            .child(div().min_w_0().truncate().child(palette::highlighted_title(
                                &item.label,
                                &label_indices,
                                paint.accent,
                            )))
                            .child(div().min_w_0().truncate().text_color(muted).child(
                                palette::highlighted_title(&item.path, &path_indices, paint.accent),
                            ))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = entity.update(cx, |state, cx| {
                                    if !state.pending {
                                        state.selected = position;
                                        cx.emit(Pick);
                                    }
                                });
                            })
                    })
                    .collect()
            },
        )
        .size_full()
        .track_scroll(&self.scroll)
        .debug_selector(|| "find-results".to_string());
        surface = surface.child(list);
        surface.into_any_element()
    }
}

impl Drop for FuzzyFind {
    fn drop(&mut self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }
}

struct SearchText {
    search: String,
    label_bytes: usize,
    // Most text lowercases one character to one character. Allocate an offset
    // map only for expansions (e.g. İ), rather than per character in every row.
    original: Option<Vec<usize>>,
}

impl SearchText {
    fn indices(&self, query: &str) -> Vec<usize> {
        let Some((_, mut indices)) = palette::fuzzy_match_lowered(query, &self.search, usize::MAX)
        else {
            return Vec::new();
        };
        if let Some(map) = &self.original {
            for ix in &mut indices {
                *ix = map[*ix];
            }
            indices.dedup();
        }
        indices
    }

    fn new(label: &str, path: &str) -> Self {
        let mut search = label.to_lowercase();
        let label_bytes = search.len();
        if !path.is_empty() {
            search.push(' ');
            search.push_str(&path.to_lowercase());
        }
        let chars = || {
            label
                .chars()
                .chain((!path.is_empty()).then_some(' '))
                .chain(path.chars())
        };
        let original = chars().any(|ch| ch.to_lowercase().count() != 1).then(|| {
            chars()
                .enumerate()
                .flat_map(|(ix, ch)| ch.to_lowercase().map(move |_| ix))
                .collect()
        });
        Self {
            search,
            label_bytes,
            original,
        }
    }
}

fn can_narrow(previous: &str, query: &str) -> bool {
    !previous.is_empty() && !query.chars().any(char::is_whitespace) && query.starts_with(previous)
}

fn rank_index(
    index: &[SearchText],
    query: &str,
    candidates: Option<&[usize]>,
    current: &AtomicU64,
    revision: u64,
) -> Option<Vec<usize>> {
    let mut matcher = scoring::Scorer::new(query);
    let mut scored = Vec::new();
    let count = candidates.map_or(index.len(), <[usize]>::len);
    for position in 0..count {
        if current.load(Ordering::Relaxed) != revision {
            return None;
        }
        let row = candidates.map_or(position, |rows| rows[position]);
        let text = &index[row];
        if let Some(score) = matcher.score(&text.search) {
            let label = &text.search[..text.label_bytes];
            let class = if label == query {
                0u8
            } else if label.starts_with(query) {
                1
            } else if label.contains(query) {
                2
            } else {
                3
            };
            scored.push((class, std::cmp::Reverse(score), row));
        }
    }
    if current.load(Ordering::Relaxed) != revision {
        return None;
    }
    // Explicit source-order ties also preserve natural order when the input is
    // a previous query's ranked subset. No extra stable-sort allocation needed.
    scored.sort_unstable();
    if current.load(Ordering::Relaxed) != revision {
        return None;
    }
    Some(scored.into_iter().map(|(_, _, row)| row).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fzf_ranking_and_visible_highlights_preserve_the_original_matcher() {
        let index = [
            ("ab", "Book A"),
            ("a_b", "Book B"),
            ("aba", "Book A"),
            ("abb", "Book A"),
            ("same", "ab"),
            ("İstanbul", "Türkiye"),
            ("ABB", "Book C"),
            ("a-b", "Book A"),
            ("Éclair", "Café"),
        ]
        .map(|(label, path)| SearchText::new(label, path));
        let current = AtomicU64::new(1);
        let mut previous = String::new();
        let mut rows = Vec::new();
        for query in [
            "a", "ab", "abb", "ab", "b", "book a", "a book", "a a", "i\u{307}", "é", "missing",
        ] {
            let mut expected = index
                .iter()
                .enumerate()
                .filter_map(|(row, text)| {
                    let (score, _) = palette::fuzzy_match_lowered(query, &text.search, usize::MAX)?;
                    let label = &text.search[..text.label_bytes];
                    let class = if label == query {
                        0
                    } else if label.starts_with(query) {
                        1
                    } else if label.contains(query) {
                        2
                    } else {
                        3
                    };
                    Some((class, std::cmp::Reverse(score), row))
                })
                .collect::<Vec<_>>();
            expected.sort();
            rows = rank_index(
                &index,
                query,
                can_narrow(&previous, query).then_some(rows.as_slice()),
                &current,
                1,
            )
            .unwrap();
            assert_eq!(
                rows,
                expected
                    .into_iter()
                    .map(|(_, _, row)| row)
                    .collect::<Vec<_>>(),
                "{query}"
            );
            previous = query.into();
        }
        assert_eq!(
            index[5].indices("i\u{307}"),
            vec![0],
            "lowercase expansion highlights the original character once"
        );
        assert_eq!(index[8].indices("café"), vec![7, 8, 9, 10]);
        current.store(2, Ordering::Relaxed);
        assert!(rank_index(&index, "a", None, &current, 1).is_none());
    }

    #[test]
    #[ignore = "diagnostic timing over 1.5 million rows; run explicitly with --release --ignored --nocapture"]
    fn fzf_rank_large_index() {
        let index = (0..1_500_000)
            .map(|i| SearchText::new(&format!("Contract {i:07}"), "Equities › US › Book A"))
            .collect::<Vec<_>>();
        let current = AtomicU64::new(1);
        let mut previous = String::new();
        let mut rows = Vec::new();
        for query in [
            "c",
            "co",
            "contract",
            "149",
            "1499",
            "14999",
            "contract 1499",
        ] {
            let start = std::time::Instant::now();
            rows = rank_index(
                &index,
                query,
                can_narrow(&previous, query).then_some(rows.as_slice()),
                &current,
                1,
            )
            .unwrap();
            eprintln!("{query:?}: {:?}, {} matches", start.elapsed(), rows.len());
            previous = query.into();
        }
    }

    fn item(id: &'static str, label: &str, path: &str) -> FindItem {
        FindItem::new(id, label, path, |_, _, _| Ok(()))
    }

    #[gpui::test]
    fn fzf_displays_all_deferred_rows_and_waits_for_index_before_ranking(
        cx: &mut gpui::TestAppContext,
    ) {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = count.clone();
        let results = cx.new(|_| FuzzyFind::default());
        results.update(cx, |r, cx| {
            r.replace_index(
                Arc::new(FindIndex::deferred(100_000, move |row| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    FindLabel::new(format!("{row}"), format!("Row {row}"), "Parent")
                })),
                |_, _, _, _| Ok(()),
                cx,
            );
            assert_eq!(r.ranked.len(), 100_000);
            assert_eq!(
                count.load(Ordering::Relaxed),
                0,
                "opening does not prepare labels for every row"
            );
            r.navigate(vimnav::NavCommand::Bottom, cx);
            assert_eq!(r.selected_item().unwrap().label(), "Row 99999");
            assert!(r.indices(99_999).is_empty());
            r.set_query("row".into(), cx);
            r.set_query("target".into(), cx);
            assert_eq!(
                r.ranked.len(),
                100_000,
                "keep the complete display while indexing"
            );
            assert!(
                r.selected_item().is_none(),
                "pending queries cannot pick unfiltered rows"
            );
            r.replace_index(
                Arc::new(FindIndex::new(vec![FindLabel::new(
                    "match", "Target", "Parent",
                )])),
                |_, _, _, _| Ok(()),
                cx,
            );
        });
        cx.run_until_parked();
        results.read_with(cx, |r, _| {
            assert_eq!(r.query(), "target");
            assert_eq!(r.selected_item().unwrap().label(), "Target");
        });
    }

    #[gpui::test]
    fn fzf_finishing_index_preserves_search_folds_without_flashing_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let tree = Arc::new(FindTree::from_depths([0, 1, 1]));
        let results = cx.new(|_| FuzzyFind::default());
        results.update(cx, |r, cx| {
            r.replace_index(
                Arc::new(
                    FindIndex::deferred(3, |row| {
                        FindLabel::new(format!("{row}"), format!("Row {row}"), "")
                    })
                    .with_tree(tree.clone()),
                ),
                |_, _, _, _| Ok(()),
                cx,
            );
            r.toggle_branch(0, cx);
        });
        cx.run_until_parked();
        results.update(cx, |r, cx| {
            assert_eq!(r.ranked.iter().collect::<Vec<_>>(), vec![0]);
            r.replace_index(
                Arc::new(
                    FindIndex::new(
                        (0..3)
                            .map(|row| FindLabel::new(format!("{row}"), format!("Row {row}"), ""))
                            .collect(),
                    )
                    .with_tree(tree),
                ),
                |_, _, _, _| Ok(()),
                cx,
            );
            assert_eq!(
                r.ranked.iter().collect::<Vec<_>>(),
                vec![0],
                "keep the folded display while filtering"
            );
        });
        cx.run_until_parked();
        results.read_with(cx, |r, _| {
            assert_eq!(r.ranked.iter().collect::<Vec<_>>(), vec![0]);
            assert_eq!(r.selected_item().unwrap().label(), "Row 0");
        });
    }

    #[gpui::test]
    fn fzf_tree_folds_preserve_candidates_and_query_edits_reopen_matches(
        cx: &mut gpui::TestAppContext,
    ) {
        let results = cx.new(|_| FuzzyFind::default());
        results.update(cx, |r, cx| {
            r.replace_tree_items(
                vec![
                    item("0", "Total", ""),
                    item("1", "Desk A", ""),
                    item("2", "a target", "Desk A"),
                    item("3", "Desk B", ""),
                    item("4", "target", "Desk B"),
                ],
                Arc::new(FindTree::from_depths([0, 1, 2, 1, 2])),
                cx,
            );
            r.set_query("targ".into(), cx);
        });
        cx.run_until_parked();
        results.update(cx, |r, cx| {
            assert_eq!(r.ranked.iter().collect::<Vec<_>>(), vec![0, 3, 4, 1, 2]);
            assert_eq!(r.selected_item().unwrap().label(), "target");
            assert_eq!(r.context().1, "2 matches");
            assert!(r.hits.as_ref().unwrap()[4]);
            assert!(!r.hits.as_ref().unwrap()[3]);
            r.toggle_branch(3, cx);
            assert!(r.selected_item().is_none());
        });
        cx.run_until_parked();
        results.update(cx, |r, cx| {
            assert_eq!(r.ranked.iter().collect::<Vec<_>>(), vec![0, 3, 1, 2]);
            assert_eq!(r.selected_item().unwrap().label(), "Desk B");
            assert_eq!(
                r.context().1,
                "2 matches",
                "folds do not change match counts"
            );
            r.set_query("target".into(), cx);
        });
        cx.run_until_parked();
        results.update(cx, |r, cx| {
            assert_eq!(
                r.ranked.iter().collect::<Vec<_>>(),
                vec![0, 3, 4, 1, 2],
                "narrow from direct matches including folded descendants"
            );
            r.toggle_branch(0, cx);
            r.set_query("missing".into(), cx);
        });
        cx.run_until_parked();
        assert!(
            results.read_with(cx, |r, _| r.ranked.is_empty()),
            "stale fold work cannot replace a newer query"
        );
    }

    #[gpui::test]
    fn fzf_narrowing_resets_on_edits_and_dataset_replacement(cx: &mut gpui::TestAppContext) {
        let results = cx.new(|_| FuzzyFind::default());
        results.update(cx, |r, cx| {
            r.replace_items(vec![item("a", "alpha", ""), item("b", "beta", "")], cx);
        });
        for (query, expected) in [
            ("al", vec![0]),
            ("alp", vec![0]),
            ("a", vec![0, 1]),
            ("be", vec![1]),
            ("zzz", vec![]),
            ("b", vec![1]),
        ] {
            results.update(cx, |r, cx| r.set_query(query.into(), cx));
            cx.run_until_parked();
            results.read_with(cx, |r, _| {
                assert_eq!(r.ranked.iter().collect::<Vec<_>>(), expected, "{query}")
            });
        }
        results.update(cx, |r, cx| {
            // The new match lies outside the previous query's candidate set.
            r.replace_index(
                Arc::new(FindIndex::new(vec![FindLabel::new("c", "bravo", "")])),
                |_, _, _, _| Ok(()),
                cx,
            );
            r.set_query("br".into(), cx);
        });
        cx.run_until_parked();
        results.read_with(cx, |r, _| {
            assert_eq!(r.selected_item().unwrap().label(), "bravo")
        });
    }

    #[gpui::test]
    fn fzf_preserves_a_pick_by_identity_when_loaded_results_reorder(cx: &mut gpui::TestAppContext) {
        let results = cx.new(|_| FuzzyFind::default());
        results.update(cx, |results, cx| {
            results.set_query("sp".into(), cx);
            results.replace_items(
                vec![item("a", "SPX", "Book A"), item("b", "SPX", "Book B")],
                cx,
            );
        });
        cx.run_until_parked();
        results.update(cx, |results, cx| {
            results.navigate(vimnav::NavCommand::Move(1), cx);
            assert_eq!(results.selected_item().unwrap().path(), "Book B");
            results.replace_items(
                vec![item("b", "SPX", "Book B"), item("a", "SPX", "Book A")],
                cx,
            );
        });
        cx.run_until_parked();
        results.update(cx, |results, cx| {
            assert_eq!(results.selected_item().unwrap().path(), "Book B");
            results.set_query("book a sx".into(), cx);
        });
        cx.run_until_parked();
        results.update(cx, |results, cx| {
            assert_eq!(results.selected_item().unwrap().path(), "Book A");
            results.set_query("spx".into(), cx);
            results.fail("Search failed", cx);
        });
        cx.run_until_parked();
        assert!(
            results.read_with(cx, |r, _| r.selected_item().is_none()),
            "an error cannot confirm stale results"
        );
    }

    #[gpui::test]
    fn fzf_keeps_input_responsive_and_only_applies_the_latest_query(cx: &mut gpui::TestAppContext) {
        let results = cx.new(|_| FuzzyFind::default());
        results.update(cx, |results, cx| {
            results.replace_items(
                (0..10_000)
                    .map(|i| {
                        FindItem::new(
                            format!("{i}"),
                            format!("Contract {i:05}"),
                            "Book",
                            |_, _, _| Ok(()),
                        )
                    })
                    .collect(),
                cx,
            );
            results.set_query("contract 00001".into(), cx);
            results.set_query("contract 09999".into(), cx);
            assert_eq!(
                results.query(),
                "contract 09999",
                "input is updated before any ranking work runs"
            );
            assert!(
                results.selected_item().is_none(),
                "Enter cannot commit results from an older query"
            );
        });
        cx.run_until_parked();
        assert_eq!(
            results.read_with(cx, |r, _| r.selected_item().unwrap().label().to_string()),
            "Contract 09999"
        );
        results.update(cx, |r, cx| {
            r.set_query("contract 00001".into(), cx);
            r.set_query(String::new(), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            results.read_with(cx, |r, _| r.selected_item().unwrap().label().to_string()),
            "Contract 00000"
        );
    }
}
