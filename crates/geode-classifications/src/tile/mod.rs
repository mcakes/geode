//! The shell-hosted classifications entity. It shows one classification
//! from the factory's configuration snapshot, answers the shell's door
//! (`crate::content::ClassificationsContent`) and paints the tile: the
//! header (`header`), the switcher hung beneath its name, the `⋯` menu, the
//! grid of source values (`table`), and an empty state while there is
//! nothing to show.
//!
//! The grid's rows are the classification's map joined with the source
//! column's distinct values, which the tile asks the data tier for when a
//! classification is shown, when its source column changes, and on
//! `classifications::refresh`. A refused or failed read leaves the map's
//! rows alone on screen with a notice; `shift+r` asks again.
//!
//! What paint reads is prepared in `Chrome` and the table's `Prepared`
//! whenever the configuration, the values, the filter or the sort change,
//! never in render.

mod header;
mod table;

use std::rc::Rc;
use std::sync::Arc;

use geode_core::classification;
use geode_core::query::{AsOf, DistinctOutcome, DistinctParams, QueryKey};
use geode_core::scope::Scope;
use geode_core::sort::SortOrder;
use geode_data::{DataHandle, Refusal};
use geode_shell::actions::ActionId;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::{Binding, KeyContext};
use geode_shell::module::{CloseHandle, FindEvent, StackHandle};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_tile::header::{HEADER_HEIGHT, Mode, link_chips};
use geode_tile::menu::{self, ActionRow, Hint, Menu, MenuHost, MenuIds, MenuPick, Row};
use geode_tile::notice::Notice;
use gpui::prelude::*;
use gpui::{Context, Entity, SharedString, Window, div};
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::content::{ClassificationsConfig, Shared, action_title};
use crate::core::grid::GridModel;
use crate::core::session::{self, SortCol, State};
use header::HeaderModel;
use table::{GridDelegate, Prepared, RowPressed, SortClicked};

/// Which menu is up: the switcher, hung under the header's name, or the
/// `⋯` action menu, hung from the header's right edge. Both share menu
/// mode, its keys and its pick door; opening one replaces the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Switch,
    Actions,
}

/// What a menu row does when picked.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pick {
    /// Show this classification.
    Show(SharedString),
    /// Run this registered action through the tile's own door.
    Action(&'static str),
}

impl MenuPick for Pick {
    fn element_name(&self) -> SharedString {
        match self {
            Pick::Show(name) => name.clone(),
            Pick::Action(id) => SharedString::new_static(id),
        }
    }
}

const NEW_ACTION: &str = "classifications::new";
const REFRESH_ACTION: &str = "classifications::refresh";

/// Prepared paint input.
#[derive(Default)]
struct Chrome {
    header: HeaderModel,
    /// The body's empty state, `None` while a classification is shown.
    empty: Option<SharedString>,
    notices: Vec<Notice>,
}

/// The source column a values read asked about, for the classification it
/// was asked for. A change of either asks again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    name: String,
    from: String,
}

pub struct ClassificationsTile {
    id: TileId,
    frame: FrameRef,
    /// The data tier's door: the grid asks it for each source value's row
    /// count.
    data: DataHandle,
    shared: Rc<Shared>,
    state: State,
    grid: GridModel,
    table: Entity<TableState<GridDelegate>>,
    /// The last values read's tag: an answer carrying any other is stale.
    tag: u64,
    /// What the last values read asked about; `None` before the first.
    asked: Option<Asked>,
    /// The source column's values and row counts, as last answered.
    observed: Vec<(String, u64)>,
    /// Why the values are not on screen: a refused or failed read.
    values_notice: Option<String>,
    /// The filter in force when a `/` search began, which a cancelled
    /// search restores; `None` while no search is open.
    find_entry: Option<String>,
    /// The rem the table's column widths were last scaled to.
    last_rem: f32,
    menu: Option<(MenuKind, Menu<Pick>)>,
    /// The live keymap the menus' hints are resolved against.
    chords: Arc<Vec<Binding>>,
    stack: Option<StackHandle>,
    close: Option<CloseHandle>,
    /// Restore and refusal notices, painted in the header's cluster.
    notices: Vec<String>,
    chrome: Chrome,
    menu_selector: SharedString,
    menu_tip: SharedString,
    switch_tip: SharedString,
    /// How many times `sync_table` refreshed the table's columns.
    #[cfg(test)]
    refreshes: usize,
}

impl ClassificationsTile {
    pub(crate) fn new(
        id: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        data: DataHandle,
        shared: Rc<Shared>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ClassificationsTile {
        let (state, notices) = restored.map(session::from_table).unwrap_or_default();
        let mut grid = GridModel::new();
        grid.set_sort(state.sort);
        // A restored cursor lands on its source once the rows arrive.
        if let Some(source) = &state.cursor {
            grid.seed_cursor(source.clone());
        }
        let table = cx.new(|cx| {
            TableState::new(GridDelegate::new(), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
                .col_resizable(true)
                .col_movable(false)
                .sortable(true)
                .loop_selection(false)
        });
        // The window, for the double-click's edit through the tile's door.
        cx.subscribe_in(&table, window, |this, _, e: &RowPressed, window, cx| {
            this.row_pressed(*e, window, cx)
        })
        .detach();
        cx.subscribe(&table, |this, _, e: &SortClicked, cx| {
            this.sort_clicked(e.0, cx)
        })
        .detach();
        // A header drag's widths are recorded in the delegate, so the next
        // refresh (a heading, a sort, the rem) keeps them. The table's
        // other events are its own: the cursor and selection come through
        // `RowPressed`, and the tile sets the table's selected row itself.
        cx.subscribe(&table, |this, _, e: &TableEvent, cx| {
            if let TableEvent::ColumnWidthsChanged(widths) = e {
                this.table
                    .update(cx, |t, _| t.delegate_mut().record_widths(widths));
            }
        })
        .detach();
        // A keymap reload re-resolves an open menu's hints at once.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.chords = menu::live_bindings(cx);
            if let Some((_, m)) = this.menu.as_mut() {
                m.rehint(&this.chords);
                cx.notify();
            }
        })
        .detach();
        let mut tile = ClassificationsTile {
            id,
            frame,
            data,
            shared,
            state,
            grid,
            table,
            tag: 0,
            asked: None,
            observed: Vec::new(),
            values_notice: None,
            find_entry: None,
            last_rem: 0.0,
            menu: None,
            chords: menu::live_bindings(cx),
            stack: None,
            close: None,
            notices,
            chrome: Chrome::default(),
            menu_selector: format!("classifications-menu-button-{}", id.0).into(),
            menu_tip: format!("tip-classifications-menu-{}", id.0).into(),
            switch_tip: format!("tip-classifications-switch-{}", id.0).into(),
            #[cfg(test)]
            refreshes: 0,
        };
        tile.settle(cx);
        tile
    }

    /// The factory stored a new configuration snapshot.
    pub fn config_changed(&mut self, cx: &mut Context<Self>) {
        self.settle(cx);
        cx.notify();
    }

    /// Bring the tile in line with the snapshot: a tile showing nothing
    /// (new, or its classification gone) offers the switcher at once when
    /// there is something to pick; an open switcher takes the new names.
    /// The values are asked for again when the shown classification or its
    /// source column changed, and the rows rebuilt from the map either way.
    fn settle(&mut self, cx: &mut Context<Self>) {
        self.settle_menu();
        self.ask_values(false);
        self.rebuild_rows(cx);
    }

    fn settle_menu(&mut self) {
        let shown = self.shown().is_some();
        match self.menu.as_ref().map(|(k, _)| *k) {
            Some(MenuKind::Switch) => {
                let rows = self.switch_rows();
                if rows.is_empty() {
                    self.menu = None;
                } else if let Some((_, m)) = self.menu.as_mut() {
                    m.replace_rows(rows, &self.chords);
                }
            }
            _ if !shown => {
                let rows = self.switch_rows();
                if !rows.is_empty() {
                    self.menu = Some((MenuKind::Switch, Menu::new(rows, &self.chords)));
                }
            }
            _ => {}
        }
    }

    /// The shown classification's name and source column.
    fn shown_asked(&self) -> Option<Asked> {
        let name = self.state.name.as_deref()?;
        let config = self.shared.config.borrow();
        let dim = config.as_ref()?.dims.get(name)?;
        Some(Asked {
            name: dim.name.clone(),
            from: dim.from.clone(),
        })
    }

    /// Ask for the shown classification's source values when it or its
    /// source column changed since the last read, or always under `force`
    /// (the refresh). Values read for another source column are dropped at
    /// once: they would count the wrong column's rows. A refusal keeps
    /// whatever values are on screen and says why in the header.
    fn ask_values(&mut self, force: bool) {
        let now = self.shown_asked();
        if now == self.asked && !force {
            return;
        }
        if now.as_ref().map(|a| &a.from) != self.asked.as_ref().map(|a| &a.from) {
            self.observed.clear();
        }
        self.asked = now.clone();
        self.values_notice = None;
        let Some(asked) = now else {
            return;
        };
        self.tag += 1;
        let params = DistinctParams {
            key: QueryKey(self.id.0),
            tag: self.tag,
            column: asked.from,
            scope: Scope::default(),
            as_of: AsOf::Live,
        };
        if let Err(refusal) = self.data.distinct(params) {
            self.values_notice = Some(match refusal {
                Refusal::Busy => format!("values not loaded: {refusal} \u{2014} R retries"),
                Refusal::Stopped => format!("values not loaded: {refusal}"),
            });
        }
    }

    /// A values answer. One carrying an older tag, or another column, was
    /// overtaken by a later read and is dropped.
    pub fn on_distinct(&mut self, outcome: DistinctOutcome, cx: &mut Context<Self>) {
        let current = self
            .asked
            .as_ref()
            .is_some_and(|a| a.from == outcome.column);
        if outcome.tag != self.tag || !current {
            return;
        }
        let answered = match outcome.values {
            Ok(values) => {
                self.observed = values;
                self.values_notice = None;
                true
            }
            Err(why) => {
                self.values_notice = Some(format!("values not loaded: {why} \u{2014} R retries"));
                false
            }
        };
        self.rebuild_rows(cx);
        // A restored cursor the values did not hold has nowhere left to
        // arrive from. A failed read is not that answer: `R` may still
        // bring its row.
        if answered {
            self.grid.forget_seed();
        }
        cx.notify();
    }

    /// Rebuild the grid's rows from the shown classification's map and the
    /// observed values, then everything painted from them.
    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let rows = {
            let config = self.shared.config.borrow();
            let dim = self
                .state
                .name
                .as_deref()
                .and_then(|n| config.as_ref()?.dims.get(n));
            dim.map(|d| classification::rows(d, &self.observed))
                .unwrap_or_default()
        };
        self.grid.set_rows(rows);
        self.rebuild_chrome();
        self.sync_table(true, cx);
    }

    /// Hand the table what it paints. `rows` re-prepares the shown rows
    /// (and the columns' headings and sort marks); a cursor or selection
    /// change alone moves only the painted cursor and tint.
    fn sync_table(&mut self, rows: bool, cx: &mut Context<Self>) {
        let prepared = rows.then(|| {
            let (from, name) = self
                .asked
                .as_ref()
                .map_or(("", ""), |a| (a.from.as_str(), a.name.as_str()));
            Rc::new(Prepared::build(&self.grid, from, name))
        });
        let (title, help) = if self.grid.query().is_empty() {
            (
                "No source values",
                "Values appear here once the source column has data.",
            )
        } else {
            (
                "No matching values",
                "Escape restores the filter in force before the search.",
            )
        };
        let selected = self.grid.selected();
        let sort = self.grid.sort();
        let cursor = self.grid.cursor();
        let refreshed = self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.set_selected(selected);
            // The table reads headings and sort marks only on a refresh,
            // which also re-lays every column: refresh only when one of
            // them changed, never for a filter keystroke or new values.
            let mut refresh = d.set_sort(sort);
            if let Some(prepared) = prepared {
                refresh |= d.set(prepared);
                d.set_empty(title.into(), help.into());
            }
            if refresh {
                t.refresh(cx);
            }
            match cursor {
                Some(c) if t.selected_row() != Some(c) => t.set_selected_row(c, cx),
                Some(_) => {}
                None => t.clear_selection(cx),
            }
            cx.notify();
            refresh
        });
        #[cfg(test)]
        {
            self.refreshes += usize::from(refreshed);
        }
        #[cfg(not(test))]
        let _ = refreshed;
    }

    fn rebuild_chrome(&mut self) {
        let config = self.shared.config.borrow();
        let dim = self
            .state
            .name
            .as_deref()
            .and_then(|n| config.as_ref()?.dims.get(n));
        let layer = dim.and_then(|d| config.as_ref()?.layers.get(&d.name).copied());
        self.chrome.header = HeaderModel::prepare(dim, layer, self.grid.counts());
        let none_defined = config
            .as_ref()
            .is_none_or(|c| c.dims.all().next().is_none());
        let new = action_title(NEW_ACTION);
        self.chrome.empty = match (&self.state.name, dim) {
            (_, Some(_)) => None,
            (Some(gone), None) if none_defined => {
                Some(format!("{gone} no longer exists. {new} makes a classification.").into())
            }
            (Some(gone), None) => {
                Some(format!("{gone} no longer exists. Pick another classification.").into())
            }
            (None, None) if none_defined => {
                Some(format!("No classifications yet. {new} makes one.").into())
            }
            (None, None) => Some(SharedString::new_static("Pick a classification.")),
        };
        self.chrome.notices = self
            .notices
            .iter()
            .cloned()
            .map(Notice::danger)
            .chain(self.values_notice.iter().cloned().map(Notice::warning))
            .collect();
    }

    /// The classification on screen: the one the tile names, while the
    /// snapshot still defines it.
    fn shown(&self) -> Option<String> {
        let name = self.state.name.as_deref()?;
        let config = self.shared.config.borrow();
        config
            .as_ref()
            .and_then(|c| c.dims.get(name))
            .map(|d| d.name.clone())
    }

    /// One row per classification, alphabetical, the shown one ticked.
    fn switch_rows(&self) -> Vec<Row<Pick>> {
        let config = self.shared.config.borrow();
        let mut names: Vec<&str> = config
            .iter()
            .flat_map(|c: &ClassificationsConfig| c.dims.all().map(|d| d.name.as_str()))
            .collect();
        names.sort_unstable();
        names
            .into_iter()
            .map(|name| {
                let checked = self.state.name.as_deref() == Some(name);
                let label = SharedString::from(name.to_string());
                Row::Action(ActionRow::new(Pick::Show(label.clone()), label).checked(checked))
            })
            .collect()
    }

    /// The `⋯` menu: the tile's actions with their live chords.
    fn action_rows(&self) -> Vec<Row<Pick>> {
        let action = |id: &'static str| {
            Row::Action(ActionRow::new(Pick::Action(id), action_title(id)).hint(Hint::chord(id)))
        };
        vec![action("classifications::switch")]
    }

    /// Open the `kind` menu, or close it when it is the one open. The other
    /// kind's open menu is replaced, not stacked. A switcher with nothing to
    /// list refuses into the header rather than paint an empty list.
    fn toggle_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_some_and(|(k, _)| *k == kind) {
            self.menu = None;
        } else {
            let rows = match kind {
                MenuKind::Switch => self.switch_rows(),
                MenuKind::Actions => self.action_rows(),
            };
            if rows.is_empty() {
                self.menu = None;
                self.notice("no classifications to switch to".into());
            } else {
                // The switcher opens on the row in force.
                let at = rows
                    .iter()
                    .position(|r| r.action().is_some_and(|a| a.tick() == Some(true)));
                self.menu = Some((kind, Menu::new(rows, &self.chords).open_at(at)));
            }
        }
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn notice(&mut self, text: String) {
        if !self.notices.contains(&text) {
            self.notices.push(text);
            self.rebuild_chrome();
        }
    }

    /// Show `name`. The cursor, the filter and a selection belonged to the
    /// previous classification's values, so they are dropped; the sort is
    /// the tile's and stays.
    fn show(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.state.name.as_deref() != Some(name) {
            self.state.name = Some(name.to_string());
            self.state.cursor = None;
            self.grid = GridModel::new();
            self.grid.set_sort(self.state.sort);
            self.find_entry = None;
        }
        self.ask_values(false);
        self.rebuild_rows(cx);
        cx.notify();
    }

    /// A press on shown row `row`: a double-click opens the label editor
    /// on it, shift extends a row selection to it, a plain press moves the
    /// cursor there and ends a selection.
    fn row_pressed(&mut self, e: RowPressed, window: &mut Window, cx: &mut Context<Self>) {
        self.grid.click(e.row, e.shift);
        self.sync_table(false, cx);
        if e.clicks >= 2 && !e.shift {
            let edit = ActionId("classifications::edit".to_string());
            self.dispatch(&edit, None, window, cx);
        }
        cx.notify();
    }

    /// A header press cycles its column the way every grid tile's header
    /// does (`SortOrder::click_cycle`): desc → asc → the default order;
    /// another column starts at desc. No column here has a signed
    /// magnitude, so the absolute orders are skipped.
    fn sort_clicked(&mut self, col: SortCol, cx: &mut Context<Self>) {
        let current = self
            .grid
            .sort()
            .filter(|(c, _)| *c == col)
            .map(|(_, desc)| {
                if desc {
                    SortOrder::Desc
                } else {
                    SortOrder::Asc
                }
            });
        let next = SortOrder::click_cycle(current, false).map(|o| (col, o.descending()));
        self.set_sort(next, cx);
    }

    /// The one door every sort change takes; the session saves it.
    fn set_sort(&mut self, sort: Option<(SortCol, bool)>, cx: &mut Context<Self>) {
        self.state.sort = sort;
        self.grid.set_sort(sort);
        self.sync_table(true, cx);
        cx.notify();
    }

    /// `:sort <source|label|rows> [asc|desc]`, tile-local; a bare `:sort`
    /// restores the default order (unclassified first, then by label).
    pub fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("sort") => {}
            Some(verb) => return Err(format!("not a classifications command: {verb}")),
            None => return Err("not a classifications command".into()),
        }
        const USAGE: &str = "sort takes source, label or rows, then optionally asc or desc";
        let sort = match (words.next(), words.next(), words.next()) {
            (None, _, _) => None,
            (Some(col), dir, None) => {
                let col = SortCol::parse(col).ok_or(USAGE)?;
                match dir {
                    None | Some("asc") => Some((col, false)),
                    Some("desc") => Some((col, true)),
                    Some(_) => return Err(USAGE.into()),
                }
            }
            _ => return Err(USAGE.into()),
        };
        self.set_sort(sort, cx);
        Ok(())
    }

    /// The `:` vocabulary: `sort`, its three columns, then a direction.
    pub fn completions(&self, line: &str, cursor: usize) -> Vec<String> {
        let mut end = cursor.min(line.len());
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        let mut words: Vec<&str> = line[..end].split(char::is_whitespace).collect();
        words.pop(); // the word under the cursor
        words.retain(|w| !w.is_empty());
        let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        match words.as_slice() {
            [] => strs(&["sort"]),
            ["sort"] => strs(&["source", "label", "rows"]),
            ["sort", _] => strs(&["asc", "desc"]),
            _ => Vec::new(),
        }
    }

    /// `/` narrows the rows as the query is typed; committing keeps it,
    /// cancelling restores the filter in force when the search began.
    pub fn find(&mut self, event: FindEvent, cx: &mut Context<Self>) {
        let query = match event {
            FindEvent::Changed(q) => {
                if self.find_entry.is_none() {
                    self.find_entry = Some(self.grid.query().to_string());
                }
                q
            }
            FindEvent::Committed(q) => {
                self.find_entry = None;
                q
            }
            FindEvent::Cancelled => match self.find_entry.take() {
                Some(entry) => entry,
                None => return,
            },
        };
        self.grid.set_filter(&query);
        self.sync_table(true, cx);
        cx.notify();
    }

    /// `menu` while a menu is up, `visual` while a row selection is live,
    /// `normal` otherwise.
    fn mode(&self) -> &'static str {
        if self.menu.is_some() {
            "menu"
        } else if self.grid.selecting() {
            "visual"
        } else {
            "normal"
        }
    }

    /// `grid` in every mode (the shell's motions bind only in normal and
    /// visual), with counts; `tilelist` only while a menu is up, so the
    /// shell's shared `j`/`k` and arrows step its rows instead.
    pub fn key_context(&self) -> KeyContext {
        let ctx = KeyContext::new(crate::KIND)
            .grid()
            .pair("mode", self.mode())
            .counts();
        if self.menu.is_some() {
            ctx.tilelist()
        } else {
            ctx
        }
    }

    /// `true` for the actions this tile carries out and, while a menu is
    /// up, the shell's shared list steps. A registered action the tile does
    /// not carry out yet falls through to the shell.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // The grid's motions, while no menu holds the keys. A column
        // motion has no column to move in this rows-only grid.
        if self.menu.is_none()
            && let Some(m) = geode_tile::motion::parse(action, count)
        {
            if m.moves_rows() {
                self.grid.move_cursor(m, false);
                self.sync_table(false, cx);
                cx.notify();
            }
            return true;
        }
        match action.0.as_str() {
            geode_tile::motion::MENU_DOWN | geode_tile::motion::MENU_UP if self.menu.is_some() => {
                let delta = if action.0 == geode_tile::motion::MENU_DOWN {
                    1
                } else {
                    -1
                };
                if let Some((_, m)) = self.menu.as_mut() {
                    m.step(delta);
                }
                cx.notify();
            }
            "classifications::switch" => self.toggle_menu(MenuKind::Switch, cx),
            "classifications::menu" => self.toggle_menu(MenuKind::Actions, cx),
            "classifications::commit" => {
                let Some(at) = self.menu.as_ref().and_then(|(_, m)| m.highlighted()) else {
                    return false;
                };
                self.menu_pick(at, window, cx);
            }
            "classifications::cancel" if self.menu.is_some() => self.close_menu(cx),
            "classifications::cancel" if self.grid.selecting() => {
                self.grid.clear_selection();
                self.sync_table(false, cx);
                cx.notify();
            }
            "classifications::visual_rows" if self.menu.is_none() => {
                if self.grid.selecting() {
                    self.grid.clear_selection();
                } else {
                    self.grid.start_selection();
                }
                self.sync_table(false, cx);
                cx.notify();
            }
            REFRESH_ACTION => {
                self.ask_values(true);
                self.rebuild_chrome();
                cx.notify();
            }
            _ => return false,
        }
        true
    }

    /// A values read answers whether the tile is shown or not, so being
    /// shown or hidden changes nothing.
    pub fn set_visible(&mut self, _visible: bool) {}

    pub fn closed(&mut self) {
        self.menu = None;
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    pub fn set_close(&mut self, close: CloseHandle, cx: &mut Context<Self>) {
        self.close = Some(close);
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        match &self.chrome.header.name {
            Some(name) => format!("Classification: {name}").into(),
            None => SharedString::new_static(header::NONE_SHOWN),
        }
    }

    /// The cursor is saved by source value: a restored one still waiting
    /// for its row, else the grid's.
    pub fn serialize(&self) -> toml::Table {
        session::to_table(&State {
            cursor: self.grid.saved_cursor().map(str::to_string),
            ..self.state.clone()
        })
    }

    /// The source values a label verb acts on.
    #[cfg(test)]
    fn targets(&self) -> Vec<String> {
        self.grid.targets()
    }

    /// The shown rows' source values, in painted order.
    #[cfg(test)]
    fn shown_sources(&self) -> Vec<String> {
        self.grid
            .visible()
            .iter()
            .map(|&i| self.grid.row(i).source.clone())
            .collect()
    }

    #[cfg(test)]
    fn notice_texts(&self) -> Vec<String> {
        self.chrome
            .notices
            .iter()
            .map(|n| n.text().to_string())
            .collect()
    }

    #[cfg(test)]
    fn title_text(&self) -> String {
        self.chrome.header.text()
    }

    /// The open switcher's rows and ticks; `None` while it is closed.
    #[cfg(test)]
    fn switcher_rows(&self) -> Option<Vec<(String, bool)>> {
        let (MenuKind::Switch, m) = self.menu.as_ref()? else {
            return None;
        };
        Some(
            m.rows()
                .iter()
                .filter_map(Row::action)
                .map(|a| (a.title().to_string(), a.tick() == Some(true)))
                .collect(),
        )
    }

    #[cfg(test)]
    fn empty_text(&self) -> Option<String> {
        self.chrome.empty.as_ref().map(|s| s.to_string())
    }
}

impl MenuHost for ClassificationsTile {
    /// A disabled row says why and keeps the menu open; an enabled one
    /// closes it and acts.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picked) = self.menu.as_ref().and_then(|(_, m)| m.pick(index)) else {
            return;
        };
        match picked {
            Err(why) => {
                self.notice(why.to_string());
                cx.notify();
            }
            Ok(Pick::Show(name)) => {
                self.menu = None;
                self.show(&name, cx);
            }
            Ok(Pick::Action(id)) => {
                self.menu = None;
                self.dispatch(&ActionId(id.to_string()), None, window, cx);
                cx.notify();
            }
        }
    }

    /// Change-only: gpui fires this on every pointer move over a row.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.menu.as_mut().is_some_and(|(_, m)| m.highlight(index)) {
            cx.notify();
        }
    }
}

impl Render for ClassificationsTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.0;
        // Column widths follow the window's rem, which the delegate cannot
        // read itself; only a changed rem re-lays the columns.
        let rem = f32::from(window.rem_size());
        if rem != self.last_rem {
            self.last_rem = rem;
            self.table.update(cx, |t, cx| {
                t.delegate_mut().set_rem(rem);
                t.refresh(cx);
            });
        }
        let tile = cx.entity();
        let ids = MenuIds::new("classifications-menu", "classifications-menu-row");
        // Each menu's outside press closes only that menu: a press on the
        // other menu's trigger has already swapped it in (the triggers act
        // in the capture phase), and must not see it closed again.
        let close = |kind: MenuKind| {
            move |t: &mut ClassificationsTile, _: &mut Window, cx: &mut Context<Self>| {
                if t.menu.as_ref().is_some_and(|(k, _)| *k == kind) {
                    t.close_menu(cx);
                }
            }
        };
        let switcher = self
            .menu
            .as_ref()
            .filter(|(k, _)| *k == MenuKind::Switch)
            .map(|(_, m)| {
                div()
                    .debug_selector(|| "classifications-switcher".into())
                    .child(menu::render_menu(
                        m,
                        &ids,
                        gpui::Anchor::TopLeft,
                        &tile,
                        close(MenuKind::Switch),
                        cx,
                    ))
                    .into_any_element()
            });
        let actions = self
            .menu
            .as_ref()
            .filter(|(k, _)| *k == MenuKind::Actions)
            .map(|(_, m)| {
                menu::render_menu(
                    m,
                    &ids,
                    gpui::Anchor::TopRight,
                    &tile,
                    close(MenuKind::Actions),
                    cx,
                )
            });
        let theme = cx.theme();
        let header = header::render(
            &self.chrome.header,
            header::HeaderChrome {
                tile: &tile,
                tile_id: self.id,
                stack: self.stack.as_ref(),
                close: self.close.as_ref(),
                mode: Mode::from_key_mode(self.mode()),
                links: link_chips(&self.frame, cx),
                notices: self.chrome.notices.clone(),
                actions_open: actions.is_some(),
                switcher,
                menu_selector: self.menu_selector.clone(),
                menu_tip: self.menu_tip.clone(),
                switch_tip: self.switch_tip.clone(),
            },
            theme,
        );
        let body = match &self.chrome.empty {
            Some(text) => v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .px_4()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .debug_selector(move || format!("classifications-empty-{id}"))
                        .child(text.clone()),
                ),
            None => v_flex()
                .flex_1()
                .min_h_0()
                .child(table::table_el(&self.table, id)),
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(header)
                    .when_some(actions, |el, m| {
                        el.child(
                            div()
                                .absolute()
                                .right_0()
                                .top(scale::design(HEADER_HEIGHT))
                                .child(m),
                        )
                    }),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests;
