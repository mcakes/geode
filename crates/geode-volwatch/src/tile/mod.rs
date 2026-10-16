//! The shell-hosted vol watchlist entity. It shows one list from the
//! resolved snapshot in `WatchlistGlobal` as rows of a vol board and
//! answers the shell's door (`crate::content::VolwatchContent`). This
//! file holds the entity, the snapshot settle and the keys; `data` the
//! board's request lifecycle; `chrome` the header model, notices, empty
//! state and the switcher menu.
//!
//! What paint reads (`prepared`, `header`, the notices and the empty
//! state) is rebuilt when the snapshot, the board, the view, the lookback
//! or the reference tables change, never in render.

mod chrome;
mod data;

#[cfg(test)]
pub(crate) use chrome::MenuKind;
pub(crate) use chrome::MenuState;
use chrome::Notices;
#[cfg(test)]
mod tests;

use std::rc::Rc;
use std::sync::Arc;

use geode_core::reference::ReferenceData;
use geode_core::sort::SortOrder;
use geode_core::volboard::{Lookback, VolBoard};
use geode_core::watchlist::state::{Status, WatchlistSnapshot};
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::{Binding, KeyContext};
use geode_shell::module::{CloseHandle, FindEvent, StackHandle};
use geode_shell::reference::ReferenceGlobal;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use geode_shell::watchlist::WatchlistGlobal;
use geode_tile::header::HealthWatch;
use geode_tile::menu::{self, MenuHost};
use geode_tile::notice::{Dismissals, Notice};
use geode_tile::rows::{Order, RowModel};
use geode_tile::stale::StaleTimer;
use gpui::prelude::*;
use gpui::{App, Context, Entity, SharedString, Task, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::content::Shared;
use crate::core::commands::{self, Command};
use crate::core::model::{HeaderModel, Prepared, Row, sort_value};
use crate::core::session::{self, State};
use crate::core::views::{
    Ask, Column, KIND, REALIZED, SURFACE, VARSWAP, View, columns, order_tenors,
};

const VIEW_ACTIONS: [&str; 5] = [
    "volwatch::view_1",
    "volwatch::view_2",
    "volwatch::view_3",
    "volwatch::view_4",
    "volwatch::view_5",
];
const VIEW_NEXT_ACTION: &str = "volwatch::view_next";
const LOOKBACK_ACTION: &str = "volwatch::lookback";
const REFRESH_ACTION: &str = "volwatch::refresh";
pub(super) const SWITCH_ACTION: &str = "volwatch::switch";
const ESCAPE_ACTION: &str = "volwatch::escape";
const COMMIT_ACTION: &str = "volwatch::commit";

/// The reference table and column a member's long name comes from.
const REFERENCE_DATASET: &str = "underlyings";
const REFERENCE_NAME: &str = "name";

/// The history datasets the header's health chip asks about.
const HEALTH_DATASETS: [&str; 3] = [SURFACE, REALIZED, VARSWAP];

/// What every failure and refusal says the remedy is.
pub(crate) const RETRIES: &str = "\u{2014} shift+r retries";

/// A nominal monospace advance for column widths, until the grid
/// measures its own font.
const MONO_ADVANCE_PX: f32 = 7.0;

/// The resolved lists as the bridge last published them; empty for a
/// tile hosted without the global.
pub(super) fn snapshot(cx: &App) -> Arc<WatchlistSnapshot> {
    cx.try_global::<WatchlistGlobal>()
        .map(|g| g.0.clone())
        .unwrap_or_default()
}

/// The live reference tables; empty for a tile hosted without the global.
fn reference(cx: &App) -> Arc<ReferenceData> {
    cx.try_global::<ReferenceGlobal>()
        .map(|g| g.0.clone())
        .unwrap_or_default()
}

/// The grid order for `(column id, order)` over `columns`, reading values
/// through the ask the board answered; blanks sort last either way. `None`
/// when the column is not in the view.
fn order_for(sort: &(String, SortOrder), columns: &[Column], asked: &Ask) -> Option<Order<Row>> {
    let (id, order) = sort;
    let column = columns.iter().find(|c| c.id == *id)?.clone();
    let asked = asked.clone();
    let order = *order;
    let value = move |r: &Row| {
        sort_value(r, &column, &asked).map(|v| if order.absolute() { v.abs() } else { v })
    };
    Some(Order {
        key: format!("{id} {order:?}"),
        cmp: Rc::new(move |a, b| match (value(a), value(b)) {
            (Some(x), Some(y)) if order.descending() => y.total_cmp(&x),
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }),
    })
}

pub struct VolwatchTile {
    pub(crate) tile: TileId,
    #[expect(dead_code, reason = "the header's link chips read the frame")]
    frame: FrameRef,
    pub(crate) data: DataHandle,
    pub(crate) shared: Rc<Shared>,
    pub(crate) state: State,
    /// The surface's tenor listing; `None` until it answers, or when the
    /// source cannot enumerate (the views then use their conventions).
    pub(crate) tenors: Option<Vec<String>>,
    /// The tag of the tenor listing out, and the list it was asked for.
    pub(crate) keys_tag: u64,
    pub(crate) keys_for: Option<String>,
    /// The latest board ask admitted.
    pub(crate) asked: Option<Ask>,
    /// The latest admitted board's tag; an answer under any other is stale.
    pub(crate) tag: u64,
    pub(crate) in_flight: bool,
    pub(crate) board: Option<Arc<VolBoard>>,
    /// The ask the landed board answered: cells are read through it.
    pub(crate) board_ask: Option<Ask>,
    /// The shown list's live members, as the last snapshot gave them.
    pub(crate) members: Vec<String>,
    pub(crate) grid: RowModel<Row>,
    /// The board ask the grid's sort comparator reads values through.
    sorted_with: Option<Ask>,
    pub(crate) prepared: Prepared,
    pub(crate) columns: Vec<Column>,
    pub(crate) cursor_col: usize,
    pub(crate) paint: bool,
    pub(crate) header: HeaderModel,
    /// The body's empty state; `None` while a list is shown.
    pub(crate) empty: Option<SharedString>,
    /// The filter in force when a `/` search began; `None` while none is open.
    find_entry: Option<String>,
    health: HealthWatch,
    pub(crate) stale_timer: StaleTimer,
    notices: Notices,
    /// Every notice the header reports, rebuilt with the chrome.
    reported: Vec<Notice>,
    dismissed: Dismissals,
    pub(crate) menu: Option<MenuState>,
    chords: Arc<Vec<Binding>>,
    /// Whether a snapshot with lists has been settled yet, and whether the
    /// last settle showed a list: the switcher opens on the first, and
    /// again when the shown list goes away.
    settled: bool,
    was_shown: bool,
    pub(crate) refresh: Option<Task<()>>,
    pub(crate) visible: bool,
    focused: bool,
    close: Option<CloseHandle>,
    stack: Option<StackHandle>,
}

impl VolwatchTile {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        data: DataHandle,
        shared: Rc<Shared>,
        diagnostics: Entity<Diagnostics>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (state, restore) = restored.map(session::from_table).unwrap_or_default();
        let mut grid = RowModel::new();
        if let Some(name) = &state.cursor {
            grid.seed_cursor(name.clone());
        }
        let mut health = HealthWatch::new(diagnostics.clone(), tile);
        health.reask(cx, |d| d.health_for_datasets(&HEALTH_DATASETS));
        cx.observe(&diagnostics, |this: &mut Self, _, cx| {
            if this
                .health
                .refresh(cx, |d| d.health_for_datasets(&HEALTH_DATASETS))
            {
                cx.notify();
            }
        })
        .detach();
        // The bridge replaces the global only on a real change.
        cx.observe_global::<WatchlistGlobal>(|this, cx| {
            this.settle(cx);
            cx.notify();
        })
        .detach();
        cx.observe_global::<ReferenceGlobal>(|this, cx| {
            this.rebuild_rows(cx);
            cx.notify();
        })
        .detach();
        cx.observe_global::<tips::Chords>(|this, cx| {
            this.chords = menu::live_bindings(cx);
            if let Some(m) = this.menu.as_mut() {
                m.menu.rehint(&this.chords);
            }
            this.rebuild_chrome(cx);
            cx.notify();
        })
        .detach();
        // A stale wake-up notifies the tile; the rows it turned stale are
        // re-prepared here, outside render.
        cx.observe_self(|this, _| this.stale_fired()).detach();
        let mut this = VolwatchTile {
            tile,
            frame,
            data,
            shared,
            state,
            tenors: None,
            keys_tag: 0,
            keys_for: None,
            asked: None,
            tag: 0,
            in_flight: false,
            board: None,
            board_ask: None,
            members: Vec::new(),
            grid,
            sorted_with: None,
            prepared: Prepared::default(),
            columns: Vec::new(),
            cursor_col: 1,
            paint: true,
            header: HeaderModel::default(),
            empty: None,
            find_entry: None,
            health,
            stale_timer: StaleTimer::new(),
            notices: Notices {
                restore,
                ..Notices::default()
            },
            reported: Vec::new(),
            dismissed: Dismissals::default(),
            menu: None,
            chords: menu::live_bindings(cx),
            settled: false,
            was_shown: false,
            refresh: None,
            visible: false,
            focused: false,
            close: None,
            stack: None,
        };
        this.rebuild_columns();
        this.settle(cx);
        this
    }

    /// The tenors the views lay out: the listing, else the conventions.
    pub(crate) fn ordered_tenors(&self) -> Vec<String> {
        order_tenors(self.tenors.as_deref())
    }

    fn rebuild_columns(&mut self) {
        self.columns = columns(self.state.view, &self.ordered_tenors());
        self.cursor_col = self.cursor_col.clamp(1, self.columns.len().max(2) - 1);
    }

    /// The factory stored new settings: the refresh interval and the stale
    /// threshold apply at once.
    pub(crate) fn settings_changed(&mut self, cx: &mut Context<Self>) {
        self.arm_refresh(cx);
        self.prepare();
        cx.notify();
    }

    /// Whether the tile names a list the snapshot holds.
    fn shown_in(&self, snapshot: &WatchlistSnapshot) -> bool {
        self.state
            .list
            .as_deref()
            .is_some_and(|n| snapshot.lists.contains_key(n))
    }

    /// Bring the tile in line with the snapshot: the members compared with
    /// the last ones (unchanged asks nothing; changed rebuilds the rows and
    /// asks again), the tenor listing asked once per list shown, the
    /// switcher opened on the first snapshot with lists while nothing is
    /// shown and again when the shown list goes away.
    pub(crate) fn settle(&mut self, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let shown = self.shown_in(&snapshot);
        if self.was_shown && !shown {
            // Whatever is out answers a list that is gone.
            self.forget_board();
        }
        self.settle_menu(shown, &snapshot);
        if self.notices.nothing_to_switch && !snapshot.lists.is_empty() {
            self.notices.nothing_to_switch = false;
        }
        let members: Vec<String> = match (&self.state.list, shown) {
            (Some(list), true) => snapshot.members_of(list).map(|m| m.name.clone()).collect(),
            _ => Vec::new(),
        };
        if shown && self.keys_for != self.state.list {
            self.ask_keys(cx);
        }
        let changed = members != self.members;
        if changed {
            self.members = members;
            self.rebuild_rows(cx);
            self.resubmit(true, cx);
        }
        self.rebuild_chrome(cx);
        let current = self
            .state
            .list
            .as_deref()
            .and_then(|n| snapshot.lists.get(n))
            .is_some_and(|s| s.status == Status::Current);
        if current && self.board.is_some() {
            self.grid.forget_seed();
        }
    }

    /// Show `name`: the cursor, the filter, the board and every answer on
    /// its way belonged to the previous list; the view, lookback and sort
    /// are the tile's and stay.
    fn show(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.state.list.as_deref() != Some(name) {
            self.state.list = Some(name.to_string());
            self.state.cursor = None;
            self.grid = RowModel::new();
            self.sorted_with = None;
            self.find_entry = None;
            self.members.clear();
            self.keys_for = None;
            self.forget_board();
        }
        self.settle(cx);
        cx.notify();
    }

    /// Drop the board and stop waiting for any answer: one already out
    /// lands under a tag no longer awaited.
    fn forget_board(&mut self) {
        self.board = None;
        self.board_ask = None;
        self.asked = None;
        self.in_flight = false;
        self.notices.failure = None;
        self.refresh = None;
        self.stale_timer.disarm();
    }

    /// Rebuild the rows from the members, the board and the reference
    /// tables, then everything painted from them.
    pub(crate) fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let reference = reference(cx);
        let lookup = |name: &str| {
            reference
                .lookup(REFERENCE_DATASET, name, REFERENCE_NAME)
                .map(str::to_string)
        };
        let rows = crate::core::model::rows(&self.members, self.board.as_deref(), &lookup, None);
        self.sync_sort();
        self.grid.set_rows(rows);
        self.prepare();
        self.arm_stale(cx);
    }

    /// Hand the grid the order the state names, reading values through the
    /// board's ask. Re-set only when the order or that ask changed: setting
    /// it ends a selection.
    fn sync_sort(&mut self) {
        let empty = Ask::default();
        let asked = self.board_ask.as_ref().unwrap_or(&empty);
        let order = self
            .state
            .sort
            .as_ref()
            .and_then(|s| order_for(s, &self.columns, asked));
        let same_key = self.grid.sort().map(|o| &o.key) == order.as_ref().map(|o| &o.key);
        if same_key && self.sorted_with.as_ref() == self.board_ask.as_ref() {
            return;
        }
        self.sorted_with = self.board_ask.clone();
        self.grid.set_sort(order);
    }

    /// The one door every sort change takes; the session saves it. Every
    /// sortable column is numeric, so an absolute order is kept.
    fn set_sort(&mut self, sort: Option<(String, SortOrder)>, cx: &mut Context<Self>) {
        self.state.sort = sort.map(|(id, o)| (id, o.on_column(true)));
        self.sorted_with = None;
        self.grid.set_sort(None);
        self.sync_sort();
        cx.notify();
    }

    fn mode(&self) -> &'static str {
        if self.menu.is_some() {
            "menu"
        } else if self.grid.selecting() {
            "visual"
        } else {
            "normal"
        }
    }

    /// `volwatch` with its mode. No `counts` flag: the digits pick views.
    /// `tilelist` while a menu is up, so the shell's `j`/`k` step its rows.
    pub(crate) fn key_context(&self) -> KeyContext {
        let ctx = KeyContext::new(KIND).pair("mode", self.mode());
        if self.menu.is_some() {
            ctx.tilelist()
        } else {
            ctx
        }
    }

    /// `true` for the actions this tile carries out.
    pub(crate) fn dispatch(
        &mut self,
        action: &ActionId,
        _count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.user_acted(cx);
        let id = action.0.as_str();
        if let Some(i) = VIEW_ACTIONS.iter().position(|a| *a == id) {
            self.set_view(View::ALL[i], cx);
            return true;
        }
        match id {
            geode_tile::motion::MENU_DOWN | geode_tile::motion::MENU_UP if self.menu.is_some() => {
                let delta = if id == geode_tile::motion::MENU_DOWN {
                    1
                } else {
                    -1
                };
                if let Some(m) = self.menu.as_mut() {
                    m.menu.step(delta);
                }
                cx.notify();
            }
            SWITCH_ACTION => self.toggle_switcher(cx),
            COMMIT_ACTION if self.menu.is_some() => {
                let Some(at) = self.menu.as_ref().and_then(|m| m.menu.highlighted()) else {
                    return false;
                };
                self.menu_pick(at, window, cx);
            }
            ESCAPE_ACTION if self.menu.is_some() => {
                self.menu = None;
                cx.notify();
            }
            // Last in line: dismiss every warning and danger notice showing.
            ESCAPE_ACTION => {
                let shown = self.dismissed.visible(self.reported.iter().cloned());
                let mut any = false;
                for n in &shown {
                    any |= self.dismiss_notice(n, cx);
                }
                if !any {
                    return false;
                }
                cx.notify();
            }
            VIEW_NEXT_ACTION => self.set_view(self.state.view.next(), cx),
            LOOKBACK_ACTION => self.cycle_lookback(cx),
            REFRESH_ACTION => {
                self.resubmit(true, cx);
                cx.notify();
            }
            _ => return false,
        }
        true
    }

    /// A view switch lays out its columns and asks again only when its ask
    /// is not inside the last one.
    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.state.view = view;
        self.rebuild_columns();
        self.sync_sort();
        self.resubmit(false, cx);
        self.prepare();
        cx.notify();
    }

    /// D1 → W1 → M1 → D1; the request carries the one in force.
    fn cycle_lookback(&mut self, cx: &mut Context<Self>) {
        self.state.lookback = match self.state.lookback {
            Lookback::D1 => Lookback::W1,
            Lookback::W1 => Lookback::M1,
            Lookback::M1 => Lookback::D1,
        };
        self.resubmit(false, cx);
        self.prepare();
        cx.notify();
    }

    /// `:sort <col> [asc|desc|abs]`, bare `:sort` restores member order.
    pub(crate) fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String> {
        match commands::parse(line, &self.columns)? {
            Command::Sort { column, order } => self.set_sort(Some((column, order)), cx),
            Command::SortClear => self.set_sort(None, cx),
        }
        Ok(())
    }

    pub(crate) fn completions(&self, line: &str, cursor: usize) -> Vec<String> {
        commands::completions(line, cursor, &self.columns)
    }

    /// `/` narrows as the query is typed; committing keeps it, cancelling
    /// restores the filter in force when the search began.
    pub(crate) fn find(&mut self, event: FindEvent, cx: &mut Context<Self>) {
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
        cx.notify();
    }

    /// Called from the shell's render: the timers follow visibility, and
    /// nothing notifies (a notify during the draw is dropped).
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.arm_refresh(cx);
            self.arm_stale(cx);
        } else {
            self.refresh = None;
            self.stale_timer.disarm();
        }
    }

    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// The shell is removing the tile: whatever is out is cancelled by key
    /// and the timers go. No notify: this runs inside the shell's draw.
    pub(crate) fn closed(&mut self, _cx: &mut Context<Self>) {
        self.data.cancel(self.key());
        self.in_flight = false;
        self.menu = None;
        self.refresh = None;
        self.stale_timer.disarm();
    }

    pub(crate) fn set_stack(&mut self, stack: Option<StackHandle>) {
        self.stack = stack;
    }

    pub(crate) fn set_close(&mut self, close: CloseHandle) {
        self.close = Some(close);
    }

    pub(crate) fn title(&self) -> SharedString {
        self.header.title.clone().into()
    }

    /// The cursor is saved by name: a restored one still waiting, else the
    /// grid's.
    pub(crate) fn serialize(&self) -> toml::Table {
        session::to_table(&State {
            cursor: self.grid.saved_cursor().map(str::to_string),
            ..self.state.clone()
        })
    }

    /// The shown rows' names, in painted order.
    #[cfg(test)]
    pub(crate) fn shown_names(&self) -> Vec<String> {
        self.grid
            .visible()
            .iter()
            .map(|&i| self.grid.row(i).name.clone())
            .collect()
    }
}

/// A plain placeholder paint: the title, the empty state or the shown
/// rows' names, and the close control. The grid, the header and the
/// switcher's popup paint over this with the glyph work.
impl Render for VolwatchTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = self.tile.0;
        let close = self.close.as_ref().map(|c| c.button(theme, self.tile));
        let body = match &self.empty {
            Some(text) => div()
                .text_color(theme.muted_foreground)
                .child(text.clone())
                .into_any_element(),
            None => v_flex()
                .children(
                    self.grid
                        .visible()
                        .iter()
                        .filter_map(|&i| self.prepared.rows.get(i))
                        .map(|r| r.name.clone()),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .debug_selector(move || format!("volwatch-{id}"))
            .child(
                gpui_component::h_flex()
                    .justify_between()
                    .child(self.header.title.clone())
                    .children(close),
            )
            .child(body)
    }
}
