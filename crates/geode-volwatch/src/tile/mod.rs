//! The shell-hosted vol watchlist entity. It shows one list from the
//! resolved snapshot in `WatchlistGlobal` as rows of a vol board and
//! answers the shell's door (`crate::content::VolwatchContent`). This
//! file holds the entity, the snapshot settle and the keys; `data` the
//! board's request lifecycle; `chrome` the header model, notices, empty
//! state and the switcher menu; `table` the grid's delegate and paints.
//!
//! What paint reads (`prepared`, `header`, the notices and the empty
//! state) is rebuilt when the snapshot, the board, the view, the lookback
//! or the reference tables change, never in render.

mod band;
mod chrome;
mod data;
mod header;
pub(crate) mod table;
mod vega;

#[cfg(test)]
pub(crate) use chrome::MenuKind;
pub(crate) use chrome::MenuState;
use chrome::Notices;
#[cfg(test)]
mod tests;

use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use geode_core::link::{CursorScope, Emission, underlying_scope};
use geode_core::reference::ReferenceData;
use geode_core::sort::SortOrder;
use geode_core::volboard::{Lookback, Role, VolBoard};
use geode_core::watchlist::state::{Status, WatchlistSnapshot};
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::colfit::{
    FitMetrics, FittedWidths, NOTHING_TO_FIT, SESSION_KEY, widths_from_record, widths_to_toml,
};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::{Binding, KeyContext, Keystroke};
use geode_shell::module::{CloseHandle, FindEvent, StackHandle};
use geode_shell::reference::ReferenceGlobal;
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use geode_shell::watchlist::WatchlistGlobal;
use geode_tile::header::{HealthWatch, TimeRun};
use geode_tile::menu::{self, MenuHost};
use geode_tile::motion::{self, Motion};
use geode_tile::notice::{Dismissals, Notice};
use geode_tile::rows::{Order, RowModel};
use geode_tile::stale::StaleTimer;
use gpui::prelude::*;
use gpui::{App, ClipboardItem, Context, Entity, SharedString, Task, Window};
use gpui_component::ActiveTheme as _;
use gpui_component::table::TableState;
use table::{GridDelegate, GridEvent, Paints, sortable};

use crate::content::Shared;
use crate::core::commands::{self, Command};
use crate::core::model::{BandModel, HeaderModel, Prepared, Row, sort_value};
use crate::core::session::{self, State};
use crate::core::views::{
    Ask, Column, ColumnKind, KIND, REALIZED, SURFACE, VARSWAP, View, columns, order_tenors,
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
const PAINT_ACTION: &str = "volwatch::paint";
const SORT_ACTION: &str = "volwatch::sort";
const YANK_ACTION: &str = "volwatch::yank";
const VISUAL_ACTION: &str = "volwatch::visual_rows";
const INSPECT_ACTION: &str = "volwatch::inspect";
const TIMESERIES_ACTION: &str = "volwatch::timeseries";
pub(super) const MENU_ACTION: &str = "volwatch::menu";

/// The tile's motion actions and the shared grid motions they name.
const MOTIONS: [(&str, &str); 6] = [
    ("volwatch::down", motion::DOWN),
    ("volwatch::up", motion::UP),
    ("volwatch::left", motion::LEFT),
    ("volwatch::right", motion::RIGHT),
    ("volwatch::top", motion::TOP),
    ("volwatch::bottom", motion::BOTTOM),
];

/// The grid's empty states: no members, and a `/` keeping none.
const NO_MEMBERS: &str = "no members";
const NO_MATCH: &str = "no member matches \u{2014} escape restores the filter";

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

/// The display clock: the app's, or the machine's where a tile is hosted
/// without one.
fn app_clock(cx: &App) -> Clock {
    cx.try_global::<geode_shell::clock::AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| Clock::machine().0)
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
    pub(crate) prepared: Rc<Prepared>,
    pub(crate) columns: Rc<Vec<Column>>,
    /// The grid's table, created once; the prepared model and the cursor
    /// are handed to its delegate whenever they change.
    pub(crate) table: Entity<TableState<GridDelegate>>,
    /// The rem the table's widths were last scaled to.
    last_rem: f32,
    pub(crate) cursor_col: usize,
    pub(crate) paint: bool,
    pub(crate) header: HeaderModel,
    /// The footer's cursor readout, rebuilt whenever the cursor, its
    /// column, the board or the view changes.
    pub(crate) readout: SharedString,
    /// The inspection band for the cursor row while `state.band` is open,
    /// rebuilt with the readout.
    pub(crate) band_model: BandModel,
    /// The cluster's source times, one run per dataset the board read,
    /// rebuilt with the prepared model; `times_at` holds each run's source
    /// time for the stale wake-ups.
    pub(crate) times: Vec<TimeRun>,
    times_at: Vec<Option<DateTime<Utc>>>,
    /// The view strip's step key, resolved when the chords change.
    strip_keys: Option<Vec<Keystroke>>,
    /// The display clock the readout's dates and the times' labels read.
    clock: Clock,
    /// The body's empty state; `None` while a list is shown.
    pub(crate) empty: Option<SharedString>,
    /// The filter in force when a `/` search began; `None` while none is open.
    find_entry: Option<String>,
    health: HealthWatch,
    pub(crate) stale_timer: StaleTimer,
    notices: Notices,
    pub(crate) vega: vega::VegaState,
    /// Every notice the header reports, rebuilt with the chrome.
    reported: Vec<Notice>,
    dismissed: Dismissals,
    pub(crate) menu: Option<MenuState>,
    /// Where a right press hung the actions menu; `None` hangs it from the
    /// header.
    pub(crate) menu_at: Option<gpui::Point<gpui::Pixels>>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let vega_setting = shared.settings.borrow().vega.clone();
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
        // The readout's dates and the times' labels read the display clock.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.clock = app_clock(cx);
            this.prepare();
            this.sync_table(cx);
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
        cx.observe_self(|this, cx| {
            if this.stale_fired() {
                this.sync_table(cx);
            }
        })
        .detach();
        // Only the vega query reads the frame; the board does not.
        cx.observe(frame.entity(), |this, _, cx| this.on_frame_changed(cx))
            .detach();
        let paints = Paints::from_theme(cx.theme());
        let mut delegate = GridDelegate::new(paints);
        delegate.fitted = widths_from_record(restored);
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
                .col_resizable(false)
                .col_movable(false)
                .sortable(true)
                .loop_selection(false)
        });
        cx.subscribe(&table, |this, _, e: &GridEvent, cx| this.grid_event(*e, cx))
            .detach();
        // Prepared cells hold glyph cores, not colors: a theme change
        // rebuilds the paints alone.
        cx.observe_global::<gpui_component::theme::Theme>(|this, cx| {
            let paints = Paints::from_theme(cx.theme());
            this.table.update(cx, |t, cx| {
                t.delegate_mut().set_paints(paints);
                cx.notify();
            });
            cx.notify();
        })
        .detach();
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
            prepared: Rc::default(),
            columns: Rc::default(),
            table,
            last_rem: scale::DESIGN_REM,
            cursor_col: 1,
            paint: true,
            header: HeaderModel::default(),
            readout: SharedString::default(),
            band_model: BandModel::default(),
            times: Vec::new(),
            times_at: Vec::new(),
            strip_keys: None,
            clock: app_clock(cx),
            empty: None,
            find_entry: None,
            health,
            stale_timer: StaleTimer::new(),
            vega: vega::VegaState {
                setting: vega_setting,
                ..Default::default()
            },
            notices: Notices {
                restore,
                ..Notices::default()
            },
            reported: Vec::new(),
            dismissed: Dismissals::default(),
            menu: None,
            menu_at: None,
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
        let mut columns = columns(self.state.view, &self.ordered_tenors());
        // No setting, or a refused view: no vega column.
        if !self.vega_offered() {
            columns.retain(|c| c.kind != ColumnKind::Vega);
        }
        self.columns = Rc::new(columns);
        self.cursor_col = self.cursor_col.clamp(1, self.columns.len().max(2) - 1);
    }

    /// The factory stored new settings: the refresh interval and the stale
    /// threshold apply at once.
    pub(crate) fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let vega = self.shared.settings.borrow().vega.clone();
        if vega != self.vega.setting {
            self.forget_vega();
            self.vega.setting = vega;
            self.rebuild_columns();
            self.rebuild_rows(cx);
            self.submit_vega(cx);
        }
        self.arm_refresh(cx);
        // A new `stale_after` moves every deadline: re-arm the wake-ups so
        // a row turns stale at the new threshold, not the old one.
        self.arm_stale(cx);
        self.prepare();
        self.sync_table(cx);
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
        self.forget_vega();
        self.rebuild_columns();
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
        let rows = crate::core::model::rows(
            &self.members,
            self.board.as_deref(),
            &lookup,
            self.vega.values.as_ref(),
        );
        self.sync_sort();
        self.grid.set_rows(rows);
        self.prepare();
        self.arm_stale(cx);
        self.sync_table(cx);
    }

    /// The footer's readout for the cursor cell, read through the board's
    /// ask; empty with no cursor or before any board lands (a row is not
    /// yet known to be missing).
    fn rebuild_readout(&mut self) {
        let row = self
            .grid
            .cursor()
            .and_then(|at| self.grid.visible().get(at))
            .map(|&i| self.grid.row(i));
        let column = self.columns.get(self.cursor_col);
        let notes = self
            .board
            .as_deref()
            .map_or(&[][..], |b| b.notes.as_slice());
        self.readout = match (row, column, self.board_ask.as_ref()) {
            (Some(row), Some(column), Some(asked)) => {
                crate::core::model::readout(row, column, asked, notes, &self.clock).into()
            }
            _ => SharedString::default(),
        };
    }

    /// Hand the table what it paints: the prepared model, the columns, the
    /// shown rows, the cursor, the selection and the sort mark. Every
    /// cursor, board and view change passes through here, so the footer's
    /// readout and the band are rebuilt here too. The
    /// table's selected row is the cursor row (its highlight, and its
    /// scroll-to); its columns are re-read only when their layout changed.
    pub(crate) fn sync_table(&mut self, cx: &mut Context<Self>) {
        let prepared = self.prepared.clone();
        let columns = self.columns.clone();
        let sorted = self.state.sort.as_ref().and_then(|(id, o)| {
            self.columns
                .iter()
                .position(|c| c.id == *id)
                .map(|i| (i, o.descending()))
        });
        self.rebuild_readout();
        self.rebuild_band();
        let visible = self.grid.visible().to_vec();
        let cursor = self.grid.cursor().map(|r| (r, self.cursor_col));
        let selection = self.grid.selected();
        let empty = SharedString::new_static(if self.grid.query().is_empty() {
            NO_MEMBERS
        } else {
            NO_MATCH
        });
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            let relayout = d.set_model(prepared, columns, sorted);
            d.set_view(visible, cursor, selection, empty);
            if relayout {
                t.refresh(cx);
            }
            match cursor.map(|(r, _)| r) {
                Some(r) if t.selected_row() != Some(r) => t.set_selected_row(r, cx),
                Some(_) => {}
                None => t.clear_selection(cx),
            }
            cx.notify();
        });
    }

    /// A press in the grid: a row takes the cursor (shift extends the
    /// selection); a sortable head walks its click cycle. The right press
    /// opens the actions menu at the pointer.
    fn grid_event(&mut self, e: GridEvent, cx: &mut Context<Self>) {
        match e {
            GridEvent::RowPressed { row, shift } => {
                self.grid.click(row, shift);
                self.sync_table(cx);
                cx.notify();
            }
            GridEvent::RowContext { row, position } => {
                self.user_acted(cx);
                if !self.grid.selected().is_some_and(|s| s.contains(&row)) {
                    self.grid.click(row, false);
                }
                // The tile paints the cursor and the selection itself, not
                // the table's right-press outline.
                self.table
                    .update(cx, |t, cx| t.set_right_clicked_row(None, cx));
                self.sync_table(cx);
                self.open_actions(Some(position));
                cx.notify();
            }
            GridEvent::HeadPressed(col) => {
                let Some(c) = self.columns.get(col) else {
                    return;
                };
                if !sortable(&c.kind) {
                    return;
                }
                let id = c.id.clone();
                let next = SortOrder::click_cycle(self.sort_on(&id), true);
                self.set_sort(next.map(|o| (id, o)), cx);
            }
        }
    }

    /// The order in force on column `id`, if the sort is on it.
    fn sort_on(&self, id: &str) -> Option<SortOrder> {
        self.state
            .sort
            .as_ref()
            .filter(|(s, _)| s == id)
            .map(|(_, o)| *o)
    }

    /// The header's sort chip: the column's head with its group (an ATFM
    /// group by tenor alone), wrapped in `|…|` for an absolute order, then
    /// the direction (`↓` descending, `↑` ascending): `|Δ 3M| ↓`,
    /// `vol 3M ↑`, `ATFM 3M ↓` under Levels.
    pub(crate) fn sort_label(&self) -> Option<String> {
        let (id, order) = self.state.sort.as_ref()?;
        let c = self.columns.iter().find(|c| c.id == *id)?;
        let name = match &c.group {
            Some(g) => format!("{} {}", c.head, g.strip_prefix("ATFM ").unwrap_or(g)),
            None => c.head.clone(),
        };
        let name = if order.absolute() {
            format!("|{name}|")
        } else {
            name
        };
        let arrow = if order.descending() {
            '\u{2193}'
        } else {
            '\u{2191}'
        };
        Some(format!("{name} {arrow}"))
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
        self.rebuild_header();
        self.sync_table(cx);
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
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let id = action.0.as_str();
        // `escape` peels layers and finally dismisses the notices itself, so
        // it must not clear a verb's notice before the peel reaches it.
        if id != ESCAPE_ACTION {
            self.user_acted(cx);
        }
        if let Some(i) = VIEW_ACTIONS.iter().position(|a| *a == id) {
            self.set_view(View::ALL[i], cx);
            return true;
        }
        if self.menu.is_none()
            && let Some((_, shared)) = MOTIONS.iter().find(|(a, _)| *a == id)
        {
            if let Some(m) = motion::parse(&ActionId((*shared).to_string()), count) {
                self.motion(m, cx);
            }
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
            MENU_ACTION => self.toggle_menu(cx),
            TIMESERIES_ACTION => self.timeseries(cx),
            COMMIT_ACTION if self.menu.is_some() => {
                let Some(at) = self.menu.as_ref().and_then(|m| m.menu.highlighted()) else {
                    return false;
                };
                self.menu_pick(at, window, cx);
            }
            ESCAPE_ACTION if self.menu.is_some() => {
                self.close_menu();
                cx.notify();
            }
            ESCAPE_ACTION if self.state.band => self.close_band(cx),
            INSPECT_ACTION => self.inspect(cx),
            ESCAPE_ACTION if self.grid.selecting() => {
                self.grid.clear_selection();
                self.sync_table(cx);
                cx.notify();
            }
            ESCAPE_ACTION if !self.grid.query().is_empty() => {
                self.find_entry = None;
                self.grid.set_filter("");
                self.sync_table(cx);
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
            VISUAL_ACTION => {
                self.grid.start_selection();
                self.sync_table(cx);
                cx.notify();
            }
            PAINT_ACTION => {
                self.paint = !self.paint;
                self.prepare();
                self.sync_table(cx);
                cx.notify();
            }
            SORT_ACTION => self.sort_cursor_column(cx),
            YANK_ACTION => self.yank(cx),
            _ => return false,
        }
        true
    }

    /// `enter`: open the band on the cursor row, or close it. Opening
    /// grows the ask (a requery) and scrolls the cursor row into view above
    /// the band; a member the landed board lacks refuses with a notice.
    fn inspect(&mut self, cx: &mut Context<Self>) {
        if self.state.band {
            self.close_band(cx);
            return;
        }
        let Some(at) = self.grid.cursor() else {
            return;
        };
        let Some(row) = self.grid.visible().get(at).map(|&i| self.grid.row(i)) else {
            return;
        };
        if self.board.is_some() && !row.present {
            self.notices.refused = Some(format!("{} is not in the surface", row.name));
            self.rebuild_chrome(cx);
            cx.notify();
            return;
        }
        self.state.band = true;
        self.resubmit(false, cx);
        self.sync_table(cx);
        self.table.update(cx, |t, cx| t.scroll_to_row(at, cx));
        cx.notify();
    }

    /// Close the band; the cursor keeps its row. The ask shrinks without a
    /// requery: the next ask simply leaves the band's measures out.
    fn close_band(&mut self, cx: &mut Context<Self>) {
        self.state.band = false;
        self.sync_table(cx);
        cx.notify();
    }

    /// A row motion moves the cursor (extending a live selection); a
    /// column motion steps the cursor column, never onto Underlying.
    fn motion(&mut self, m: Motion, cx: &mut Context<Self>) {
        if m.moves_rows() {
            let selecting = self.grid.selecting();
            self.grid.move_cursor(m, selecting);
        } else {
            self.cursor_col = motion::col(self.cursor_col, self.columns.len(), m).max(1);
        }
        self.sync_table(cx);
        cx.notify();
    }

    /// `s`: asc → desc → member order on the cursor column, through the
    /// shared cycle. A column with no number to order by refuses.
    fn sort_cursor_column(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.columns.get(self.cursor_col) else {
            return;
        };
        if !sortable(&c.kind) {
            self.notices.refused = Some(format!("{} is not sortable", c.head));
            self.rebuild_chrome(cx);
            cx.notify();
            return;
        }
        let id = c.id.clone();
        let next = SortOrder::cycle(self.sort_on(&id), false, true);
        self.set_sort(next.map(|o| (id, o)), cx);
    }

    /// `t`: the history series behind the cursor cell. No timeseries tile
    /// can open a vol-surface history series yet, so this names the gap: the
    /// dataset, the key tuple (member, then the measure's first input's
    /// suffix) and the value column. A column with no series says so.
    fn timeseries(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.columns.get(self.cursor_col) else {
            return;
        };
        let measure = match &c.kind {
            ColumnKind::Level { measure, .. }
            | ColumnKind::Change { measure }
            | ColumnKind::ForwardChange { measure }
            | ColumnKind::Pctl { measure }
            | ColumnKind::Bar { measure } => Some(measure),
            ColumnKind::Underlying
            | ColumnKind::Spark { .. }
            | ColumnKind::Term
            | ColumnKind::Vega => None,
        };
        let name = self
            .grid
            .cursor()
            .and_then(|at| self.grid.visible().get(at))
            .map(|&i| self.grid.row(i).name.clone());
        let text = match (measure.and_then(|m| m.inputs().into_iter().next()), name) {
            (Some(input), Some(member)) => {
                let dataset = match input.role {
                    Role::Surface => SURFACE,
                    Role::Realized => REALIZED,
                    Role::Varswap => VARSWAP,
                };
                let key = std::iter::once(member)
                    .chain(input.suffix)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "timeseries cannot open a history series yet: {dataset} [{key}] {}",
                    input.column
                )
            }
            (None, _) => format!(
                "timeseries cannot open a history series yet: {} has no series",
                c.head
            ),
            (Some(_), None) => return,
        };
        self.notices.refused = Some(text);
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// `y`: the cursor cell's text, or the cursor column's text of every
    /// selected row, one per line; a selection ends.
    fn yank(&mut self, cx: &mut Context<Self>) {
        let col = self.cursor_col;
        let text_at = |at: usize| {
            self.grid
                .visible()
                .get(at)
                .and_then(|&i| self.prepared.rows.get(i))
                .and_then(|r| r.cells.get(col))
                .map(|c| c.text.to_string())
        };
        let text = match self.grid.selected() {
            Some(span) => span.filter_map(text_at).collect::<Vec<_>>().join("\n"),
            None => match self.grid.cursor().and_then(text_at) {
                Some(t) => t,
                None => return,
            },
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.grid.clear_selection();
        self.sync_table(cx);
        cx.notify();
    }

    /// The cursor row's member as a one-value `underlying_ref` path;
    /// `Nothing` while no row is under the cursor.
    pub(crate) fn emission(&self) -> Emission {
        let name = self
            .grid
            .cursor()
            .and_then(|at| self.grid.visible().get(at))
            .map(|&i| self.grid.row(i).name.as_str());
        Emission {
            cursor: match name {
                Some(name) => CursorScope::Path(underlying_scope(name)),
                None => CursorScope::Nothing,
            },
            ..Emission::default()
        }
    }

    /// A view switch lays out its columns and asks again only when its ask
    /// is not inside the last one.
    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.state.view = view;
        self.rebuild_columns();
        self.sync_sort();
        self.resubmit(false, cx);
        self.prepare();
        self.sync_table(cx);
        cx.notify();
    }

    /// D1 → W1 → M1 → D1; the request carries the one in force.
    fn cycle_lookback(&mut self, cx: &mut Context<Self>) {
        let next = match self.state.lookback {
            Lookback::D1 => Lookback::W1,
            Lookback::W1 => Lookback::M1,
            Lookback::M1 => Lookback::D1,
        };
        self.set_lookback(next, cx);
    }

    /// The one door a lookback change takes (`d`, a header chip): the
    /// chip moves at once and the board is asked again.
    fn set_lookback(&mut self, lookback: Lookback, cx: &mut Context<Self>) {
        if self.state.lookback == lookback {
            return;
        }
        self.state.lookback = lookback;
        self.resubmit(false, cx);
        self.prepare();
        self.sync_table(cx);
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
        self.sync_table(cx);
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
        self.close_menu();
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
        self.header.title.clone()
    }

    /// The cursor is saved by name: a restored one still waiting, else the
    /// grid's.
    pub(crate) fn serialize(&self, cx: &App) -> toml::Table {
        let mut t = session::to_table(&State {
            cursor: self.grid.saved_cursor().map(str::to_string),
            ..self.state.clone()
        });
        if let Some(w) = widths_to_toml(&self.table.read(cx).delegate().fitted) {
            t.insert(SESSION_KEY.into(), w);
        }
        t
    }

    /// `:autosize` / `tile::autosize_columns`: fit every column to its
    /// head and widest cell at the window's rem, or `reset` to the prepared
    /// widths. Nothing to measure before a board has landed refuses.
    pub(crate) fn autosize_columns(
        &mut self,
        reset: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let fitted = if reset {
            FittedWidths::new()
        } else {
            // Before a board lands the rows are blank placeholders.
            if self.board.is_none() {
                return Err(NOTHING_TO_FIT);
            }
            let metrics = FitMetrics::xsmall_mono(window.rem_size());
            self.table
                .read(cx)
                .delegate()
                .fit_columns(&metrics)
                .ok_or(NOTHING_TO_FIT)?
        };
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set_fitted(fitted);
            t.refresh(cx);
        });
        cx.notify();
        Ok(())
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

/// The header, the view strip, the grid and the footer (`header`).
impl Render for VolwatchTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Column widths follow the window's rem, which the delegate cannot
        // read itself; only a changed rem re-lays the columns.
        let rem = f32::from(window.rem_size());
        if rem != self.last_rem {
            self.last_rem = rem;
            self.table.update(cx, |t, cx| {
                if t.delegate_mut().set_rem(rem) {
                    t.refresh(cx);
                }
            });
        }
        header::render(self, cx)
    }
}
