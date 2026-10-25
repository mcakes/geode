//! The shell-hosted vol watchlist entity. It shows one list from the
//! resolved snapshot in `WatchlistGlobal` as rows of a vol board and
//! answers the shell's door (`crate::content::VolwatchContent`). This
//! file holds the entity, the snapshot settle and the keys; `data` the
//! board's request lifecycle; `chrome` the header model, notices, empty
//! state and the switcher menu; `table` the grid's delegate and paints.
//!
//! What paint reads (`prepared`, `header`, the notices and the empty
//! state) is rebuilt when the snapshot, the board, the view, the lens
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
use geode_core::link::{CursorScope, Emission, UNDERLYING, path_scope, underlying_scope};
use geode_core::reference::ReferenceData;
use geode_core::sort::SortOrder;
use geode_core::volboard::{Lookback, Role, VolBoard};
use geode_core::volviews::{Cells, VolView, VolViews};
use geode_core::watchlist::state::{Status, WatchlistSnapshot};
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::colfit::{FitMetrics, NOTHING_TO_FIT, SESSION_KEY, widths_to_toml};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::{Binding, KeyContext, Keystroke};
use geode_shell::linenumbers::{LineNumbers, UiSettings};
use geode_shell::module::{CloseHandle, ConfigObject, FindEvent, StackHandle};
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
use gpui_component::table::{TableEvent, TableState};
use table::{GridDelegate, GridEvent, Paints, restored_widths, sized_key, sortable};

use crate::content::Shared;
use crate::core::commands::{self, Command};
use crate::core::grouping::{self, Applicable, Pin, group_name, group_rows};
use crate::core::lens::{Lens, LensState, Metric, Shown};
use crate::core::model::{BandModel, HeaderModel, Prepared, Row, sort_value};
use crate::core::session::{self, State};
use crate::core::views::{
    Ask, Column, ColumnKind, KIND, REALIZED, SURFACE, VARSWAP, columns, order_tenors,
    reorder_columns, units,
};

/// The `g N` view actions, parallel to the first nine tabs.
const VIEW_ACTIONS: [&str; 9] = [
    "volwatch::view_1",
    "volwatch::view_2",
    "volwatch::view_3",
    "volwatch::view_4",
    "volwatch::view_5",
    "volwatch::view_6",
    "volwatch::view_7",
    "volwatch::view_8",
    "volwatch::view_9",
];
const VIEW_NEXT_ACTION: &str = "volwatch::view_next";
const VIEW_PREV_ACTION: &str = "volwatch::view_prev";
const LOOKBACK_ACTION: &str = "volwatch::lookback";
const LENS_ACTION: &str = "volwatch::lens";
const METRIC_ACTION: &str = "volwatch::metric";
pub(super) const WINDOW_ACTION: &str = "volwatch::window";
const SHOWN_ACTION: &str = "volwatch::shown";
const REFRESH_ACTION: &str = "volwatch::refresh";
pub(super) const SWITCH_ACTION: &str = "volwatch::switch";
const ESCAPE_ACTION: &str = "volwatch::escape";
const COMMIT_ACTION: &str = "volwatch::commit";
const PAINT_ACTION: &str = "volwatch::paint";
const SORT_ACTION: &str = "volwatch::sort";
const COL_LEFT_ACTION: &str = "volwatch::col_left";
const COL_RIGHT_ACTION: &str = "volwatch::col_right";
/// What `shift+h`/`shift+l` say when the unit is already at that end.
const COLUMN_AT_END: &str = "that is as far as this column goes";
const YANK_ACTION: &str = "volwatch::yank";
const VISUAL_ACTION: &str = "volwatch::visual_rows";
const INSPECT_ACTION: &str = "volwatch::inspect";
const TIMESERIES_ACTION: &str = "volwatch::timeseries";
pub(super) const MENU_ACTION: &str = "volwatch::menu";
pub(super) const TOGGLE_ACTION: &str = "volwatch::toggle";
const EXPAND_ACTION: &str = "volwatch::expand";
const COLLAPSE_ACTION: &str = "volwatch::collapse";
pub(super) const EXPAND_ALL_ACTION: &str = "volwatch::expand_all";
pub(super) const COLLAPSE_ALL_ACTION: &str = "volwatch::collapse_all";

/// The fold verbs, the blotter's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fold {
    Toggle,
    Open,
    Close,
    OpenAll,
    CloseAll,
}

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
/// The shell's `[ui] line_numbers`; off before the shell publishes it.
fn ui_line_numbers(cx: &App) -> LineNumbers {
    cx.try_global::<UiSettings>()
        .map_or(LineNumbers::Off, |s| s.line_numbers)
}

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
/// through the ask the board answered under `lens`; blanks sort last
/// either way. `None` when the column is not in the view. The lens is in
/// the key: a companion column orders by whichever number it shows.
fn order_for(
    sort: &(String, SortOrder),
    columns: &[Column],
    asked: &Ask,
    lens: LensState,
) -> Option<Order<Row>> {
    let (id, order) = sort;
    let column = columns.iter().find(|c| c.id == *id)?.clone();
    let asked = asked.clone();
    let order = *order;
    let value = move |r: &Row| {
        sort_value(r, &column, &asked, &lens).map(|v| if order.absolute() { v.abs() } else { v })
    };
    Some(Order {
        key: format!("{id} {order:?} {lens:?}"),
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
    /// The chain the tile last resolved (pin, slot or frame), so a frame
    /// or dimensions notify that moves neither asks for no regroup.
    chain: Vec<String>,
    /// The chain applied: what groups and what the header names skipped.
    pub(crate) grouping: Applicable,
    pub(crate) grid: RowModel<Row>,
    /// The board ask the grid's sort comparator reads values through.
    sorted_with: Option<Ask>,
    /// `[ui] line_numbers`, mirrored from the shell's `UiSettings`: the
    /// grid's gutter follows it (`sync_table`).
    line_numbers: LineNumbers,
    pub(crate) prepared: Rc<Prepared>,
    pub(crate) columns: Rc<Vec<Column>>,
    /// The shown view's cell shape, carried beside its columns.
    pub(crate) cells: Cells,
    /// The views the columns were last built from: a settings push that
    /// changes them rebuilds the tabs, one that does not asks nothing.
    applied_views: VolViews,
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
    /// The strip's tabs (name, the live chord of its `view_N` action for
    /// the first nine; empty past them or unbound) and the shown one's
    /// name, prepared with the columns and re-resolved with the hints
    /// when the chords change: paint clones nothing but handles.
    pub(crate) strip_tabs: Vec<(SharedString, Vec<Keystroke>)>,
    shown_name: Option<SharedString>,
    /// The strip's lens readout (`moves · Δ 1d`), prepared with the hints.
    strip_readout: SharedString,
    /// The strip's key hints in paint order: (action id, its live chord,
    /// the label). Only the keys that matter under the lens and the
    /// view's cell shape are listed, each resolved on its own when the
    /// chords change: an unbound one hides only its hint.
    strip_hints: Vec<(&'static str, Vec<Keystroke>, SharedString)>,
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
        let applied_views = shared.settings.borrow().views.clone();
        let (mut state, restore) = restored.map(session::from_table).unwrap_or_default();
        // A restored view the document no longer has: the default shows,
        // and the notice says which name went.
        let warning = state
            .view
            .take_if(|n| applied_views.get(n).is_none())
            .map(|name| format!("view {name} is gone"));
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
        // `[ui] line_numbers` arrives through the shell's `UiSettings`
        // global: settings, `ui::line_numbers_cycle` and a reload alike.
        cx.observe_global::<UiSettings>(|this, cx| this.on_ui_settings(cx))
            .detach();
        // A stale wake-up notifies the tile; the rows it turned stale are
        // re-prepared here, outside render.
        cx.observe_self(|this, cx| {
            if this.stale_fired() {
                this.sync_table(cx);
            }
        })
        .detach();
        // A frame change regroups first (an unpinned tile follows the
        // frame's chain), then the vega query checks its scope and as-of;
        // the board never reads the frame.
        cx.observe(frame.entity(), |this, _, cx| this.on_frame_changed(cx))
            .detach();
        let paints = Paints::from_theme(cx.theme());
        let mut delegate = GridDelegate::new(paints);
        delegate.fitted = restored_widths(restored);
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
                .col_resizable(true)
                .col_movable(false)
                .sortable(true)
                .loop_selection(false)
        });
        cx.subscribe(&table, |this, _, e: &GridEvent, cx| this.grid_event(*e, cx))
            .detach();
        // A header drag's widths are recorded in the delegate for the shown
        // view, so the next refresh (a sort, the rem, a view round trip)
        // keeps them. The table's other events are its own.
        cx.subscribe(&table, |this, _, e: &TableEvent, cx| {
            if let TableEvent::ColumnWidthsChanged(widths) = e {
                this.table.update(cx, |t, cx| {
                    t.delegate_mut().record_widths(widths);
                    t.refresh(cx);
                });
                cx.notify();
            }
        })
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
            chain: Vec::new(),
            grouping: Applicable::default(),
            grid,
            sorted_with: None,
            line_numbers: ui_line_numbers(cx),
            prepared: Rc::default(),
            columns: Rc::default(),
            cells: Cells::Pair,
            applied_views,
            table,
            last_rem: scale::DESIGN_REM,
            cursor_col: 1,
            paint: true,
            header: HeaderModel::default(),
            readout: SharedString::default(),
            band_model: BandModel::default(),
            times: Vec::new(),
            times_at: Vec::new(),
            strip_tabs: Vec::new(),
            shown_name: None,
            strip_readout: SharedString::default(),
            strip_hints: Vec::new(),
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
                warning,
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
        // The restored pin resolves against the startup dimensions before
        // the first rows are built, so the restore groups at once.
        this.chain = this.requested_chain(cx);
        this.grouping = grouping::applicable(&this.chain, &this.shared.dims.borrow());
        let depth = this.chain.len();
        this.state.collapsed.retain(|p| p.len() <= depth);
        this.settle(cx);
        this
    }

    /// The chain the pin asks for: a pinned list, a pinned slot's current
    /// content, nothing for a flat pin, else the frame's active grouping.
    fn requested_chain(&self, cx: &App) -> Vec<String> {
        let frame = self.frame.read(cx);
        match &self.state.pin {
            Pin::Grouping(g) => g.clone(),
            Pin::Slot(n) => frame
                .slots()
                .get(*n)
                .map(<[String]>::to_vec)
                .unwrap_or_default(),
            Pin::Flat => Vec::new(),
            Pin::None => frame
                .active_grouping()
                .map(<[String]>::to_vec)
                .unwrap_or_default(),
        }
    }

    /// Resolve the chain against the dimensions and rebuild the tree when
    /// either changed (or on `force`: a pin verb, a dimensions push whose
    /// labels may have moved). Collapsed paths deeper than the requested
    /// chain cannot exist under it and go; the rest stay, dead or not: a
    /// path whose dimension is skipped today folds again when a reload
    /// brings the classification back. Never a board requery: the tree is
    /// arranged from the rows held.
    pub(super) fn regroup(&mut self, force: bool, cx: &mut Context<Self>) {
        let chain = self.requested_chain(cx);
        let applicable = grouping::applicable(&chain, &self.shared.dims.borrow());
        if !force && chain == self.chain && applicable == self.grouping {
            return;
        }
        self.chain = chain;
        self.grouping = applicable;
        let depth = self.chain.len();
        self.state.collapsed.retain(|p| p.len() <= depth);
        self.rebuild_rows(cx);
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// The row under the cursor.
    pub(super) fn cursor_row(&self) -> Option<&Row> {
        self.grid
            .cursor()
            .and_then(|at| self.grid.visible().get(at))
            .map(|&i| self.grid.row(i))
    }

    /// The tenors the views lay out: the listing, else the conventions.
    pub(crate) fn ordered_tenors(&self) -> Vec<String> {
        order_tenors(self.tenors.as_deref())
    }

    /// The tab names in order.
    pub(crate) fn tabs(&self) -> Vec<String> {
        self.shared
            .settings
            .borrow()
            .views
            .views
            .iter()
            .map(|v| v.name.clone())
            .collect()
    }

    /// The view shown: the state's name when the document has it, else
    /// the default; `None` with no views at all.
    pub(crate) fn shown_view(&self) -> Option<VolView> {
        let settings = self.shared.settings.borrow();
        self.state
            .view
            .as_deref()
            .and_then(|n| settings.views.get(n))
            .or_else(|| settings.views.default_view())
            .cloned()
    }

    /// The shown tab as the shell's editor names it (`vol_views`, the
    /// view's name), so `config::vol_views` opens on it; `None` with no
    /// views at all.
    pub(crate) fn config_object(&self) -> Option<ConfigObject> {
        self.shown_view().map(|view| ConfigObject {
            doc: geode_core::config::VOL_VIEWS_DOC,
            name: view.name,
        })
    }

    /// The shown view's columns under the lens, in the session's order
    /// for that view (a `shift+h`/`shift+l` move; file order where none
    /// was saved); none with no views. A vega column only when the
    /// setting names a view and the query was not refused. The strip's
    /// tabs, shown name and hints follow, since each reads the document,
    /// the view's cell shape or the lens.
    fn rebuild_columns(&mut self) {
        let vega = self.vega_offered();
        let shown = self.shown_view();
        let columns = match &shown {
            Some(view) => {
                self.cells = view.cells;
                let order = self
                    .state
                    .column_order
                    .get(&view.name)
                    .map_or(&[][..], Vec::as_slice);
                reorder_columns(columns(view, &self.state.lens, vega), order)
            }
            None => Vec::new(),
        };
        self.columns = Rc::new(columns);
        self.cursor_col = self.cursor_col.clamp(1, self.columns.len().max(2) - 1);
        self.shown_name = shown.map(|v| SharedString::from(v.name));
        self.strip_tabs = self
            .tabs()
            .into_iter()
            .map(|name| (SharedString::from(name), Vec::new()))
            .collect();
        self.rebuild_strip_hints();
    }

    /// The strip's tab chords, readout and hints. Each of the first nine
    /// tabs carries the live chord of its `view_N` action (`g 1`); a
    /// tenth tab, or an unbound one, carries none. The hints: the lens
    /// flip, then under moves the lookback, under rich/cheap the metric
    /// and the window, then in a single view the swap, then the step. A
    /// label names what the key does from here (`rich/cheap`,
    /// `percentile`, `show level`).
    fn rebuild_strip_hints(&mut self) {
        for (i, (_, keys)) in self.strip_tabs.iter_mut().enumerate() {
            *keys = VIEW_ACTIONS
                .get(i)
                .and_then(|a| tips::chord_for(&self.chords, a))
                .unwrap_or_default();
        }
        let single = self.cells == Cells::Single;
        let l = self.state.lens;
        self.strip_readout = SharedString::from(l.readout(single));
        let mut wanted: Vec<(&'static str, SharedString)> = vec![(
            LENS_ACTION,
            SharedString::new_static(match l.lens {
                Lens::Moves => "rich/cheap",
                Lens::Rich => "moves",
            }),
        )];
        match l.lens {
            Lens::Moves => wanted.push((LOOKBACK_ACTION, SharedString::new_static("lookback"))),
            Lens::Rich => {
                wanted.push((
                    METRIC_ACTION,
                    SharedString::new_static(match l.metric {
                        Metric::Z => "percentile",
                        Metric::Pctl => "z-score",
                    }),
                ));
                wanted.push((WINDOW_ACTION, SharedString::new_static("window")));
            }
        }
        if single {
            wanted.push((
                SHOWN_ACTION,
                match l.shown {
                    Shown::Level => SharedString::from(format!("show {}", l.companion_head())),
                    Shown::Companion => SharedString::new_static("show level"),
                },
            ));
        }
        wanted.push((VIEW_NEXT_ACTION, SharedString::new_static("next view")));
        self.strip_hints = wanted
            .into_iter()
            .filter_map(|(action, label)| {
                Some((action, tips::chord_for(&self.chords, action)?, label))
            })
            .collect();
    }

    /// The factory stored new settings: the refresh interval and the stale
    /// threshold apply at once; changed views rebuild the tabs, falling
    /// back to the default (with a notice) when the shown one went, and
    /// ask again only for what the new columns need.
    pub(crate) fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let views_changed = {
            let s = self.shared.settings.borrow();
            s.views != self.applied_views
        };
        if views_changed {
            self.applied_views = self.shared.settings.borrow().views.clone();
            // A shown view that went: the default shows, and the standing
            // warning says which name went until the trader picks a view.
            if let Some(name) = self
                .state
                .view
                .take_if(|n| self.applied_views.get(n).is_none())
            {
                self.notices.warning = Some(format!("view {name} is gone"));
            }
            self.rebuild_columns();
            self.resubmit(false, cx);
            // The new columns reach the grid even when the ask is inside the
            // last one and nothing is asked; the chrome follows the columns.
            self.rebuild_rows(cx);
            self.rebuild_chrome(cx);
        }
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

    /// The factory stored new dimensions: the labels may have moved, so
    /// the tree is rebuilt even under an unchanged chain.
    pub(crate) fn dims_changed(&mut self, cx: &mut Context<Self>) {
        self.regroup(true, cx);
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
    /// its way belonged to the previous list; the view, lens and sort are
    /// the tile's and stay.
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
    /// tables, arranged under the applicable chain with the saved folds
    /// applied, then everything painted from them.
    pub(crate) fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let reference = reference(cx);
        let lookup = |name: &str| {
            reference
                .lookup(REFERENCE_DATASET, name, REFERENCE_NAME)
                .map(str::to_string)
        };
        let leaves = crate::core::model::rows(
            &self.members,
            self.board.as_deref(),
            &lookup,
            self.vega.values.as_ref(),
        );
        let empty = Ask::default();
        let asked = self.board_ask.as_ref().unwrap_or(&empty);
        let rows = group_rows(
            leaves,
            &self.grouping.kept,
            &self.shared.dims.borrow(),
            asked,
        );
        self.sync_sort();
        self.grid.set_rows(rows);
        self.grid
            .set_collapsed(self.state.collapsed.iter().map(|p| group_name(p)));
        self.prepare();
        self.arm_stale(cx);
        self.sync_table(cx);
    }

    /// The footer's readout for the cursor cell, read through the board's
    /// ask; empty with no cursor or before any board lands (a row is not
    /// yet known to be missing).
    fn rebuild_readout(&mut self) {
        let column = self.columns.get(self.cursor_col);
        let notes = self
            .board
            .as_deref()
            .map_or(&[][..], |b| b.notes.as_slice());
        self.readout = match (self.cursor_row(), column, self.board_ask.as_ref()) {
            (Some(row), Some(column), Some(asked)) => crate::core::model::readout(
                row,
                column,
                asked,
                notes,
                &self.clock,
                &self.state.lens,
            )
            .into(),
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
        let shown = visible.len();
        let cursor = self.grid.cursor().map(|r| (r, self.cursor_col));
        let selection = self.grid.selected();
        let collapsed: Vec<bool> = visible.iter().map(|&i| self.grid.is_collapsed(i)).collect();
        let line_numbers = self.line_numbers;
        let empty = SharedString::new_static(if self.grid.query().is_empty() {
            NO_MEMBERS
        } else {
            NO_MATCH
        });
        let view = self.shown_view().map(|v| v.name).unwrap_or_default();
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            let relayout = d.set_model(prepared, columns, sorted, &view);
            d.set_view(visible, cursor, selection, empty, collapsed);
            let regutter = d.set_gutter(line_numbers, shown, cursor.map(|(r, _)| r));
            if relayout || regutter {
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
    /// selection); a group row's chevron folds it as `space` would, the
    /// cursor staying put; a sortable head walks its click cycle. The
    /// right press opens the actions menu at the pointer.
    fn grid_event(&mut self, e: GridEvent, cx: &mut Context<Self>) {
        match e {
            GridEvent::RowPressed { row, shift } => {
                self.grid.click(row, shift);
                self.sync_table(cx);
                cx.notify();
            }
            GridEvent::TogglePressed { row } => {
                self.user_acted(cx);
                let Some(&i) = self.grid.visible().get(row) else {
                    return;
                };
                if self.grid.is_group(i) {
                    let folded = !self.grid.is_collapsed(i);
                    self.set_folded(i, folded, cx);
                }
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
    /// `vol 3M ↑`.
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
            .and_then(|s| order_for(s, &self.columns, asked, self.state.lens));
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

    /// `volwatch` with its mode, `grid` (the flag the shell's shared
    /// motion bindings are written under: without it no motion key
    /// reaches the tile) and `counts`, so a bare digit multiplies the
    /// motion that follows; the tabs sit on `g N`. `tilelist` while a
    /// menu is up, so the shell's `j`/`k` step its rows.
    pub(crate) fn key_context(&self) -> KeyContext {
        let ctx = KeyContext::new(KIND).grid().pair("mode", self.mode());
        if self.menu.is_some() {
            ctx.tilelist()
        } else {
            ctx
        }
        .counts()
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
            // A chord past the last tab does nothing.
            if let Some(name) = self.tabs().get(i).cloned() {
                let _ = self.set_view_by_name(&name, cx);
            }
            return true;
        }
        // The shell's shared motions, with their count; a menu takes the
        // menu steps below instead.
        if self.menu.is_none()
            && let Some(m) = motion::parse(action, count)
        {
            self.motion(m, cx);
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
            VIEW_NEXT_ACTION => self.step_view(1, cx),
            VIEW_PREV_ACTION => self.step_view(-1, cx),
            LOOKBACK_ACTION => self.cycle_lookback(cx),
            LENS_ACTION => self.set_lens(|l| l.lens = l.lens.toggle(), false, cx),
            METRIC_ACTION => self.set_lens(|l| l.metric = l.metric.toggle(), false, cx),
            WINDOW_ACTION => self.set_lens(|l| l.window = l.window.next(), true, cx),
            SHOWN_ACTION => self.set_lens(|l| l.shown = l.shown.toggle(), false, cx),
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
            COL_LEFT_ACTION => self.move_column(-1, count, cx),
            COL_RIGHT_ACTION => self.move_column(1, count, cx),
            YANK_ACTION => self.yank(cx),
            TOGGLE_ACTION => self.fold(Fold::Toggle, cx),
            EXPAND_ACTION => self.fold(Fold::Open, cx),
            COLLAPSE_ACTION => self.fold(Fold::Close, cx),
            EXPAND_ALL_ACTION => self.fold(Fold::OpenAll, cx),
            COLLAPSE_ALL_ACTION => self.fold(Fold::CloseAll, cx),
            _ => return false,
        }
        true
    }

    /// `Space`/`z a` toggle the cursor's group row (a member folds
    /// nothing); `z o`/`z c` open and close the cursor's group, a member's
    /// being its parent, and closing puts the cursor on the group so it
    /// never rests on a hidden row; `z shift+r`/`z shift+m` open and close
    /// every group. The set is saved by path.
    fn fold(&mut self, verb: Fold, cx: &mut Context<Self>) {
        match verb {
            Fold::OpenAll => {
                self.state.collapsed.clear();
                self.apply_folds(cx);
                return;
            }
            Fold::CloseAll => {
                self.state.collapsed = self
                    .grid
                    .group_rows()
                    .into_iter()
                    .map(|g| grouping::full_path(self.grid.row(g)))
                    .collect();
                self.apply_folds(cx);
                return;
            }
            Fold::Toggle | Fold::Open | Fold::Close => {}
        }
        let Some(&i) = self
            .grid
            .cursor()
            .and_then(|at| self.grid.visible().get(at))
        else {
            return;
        };
        let target = if self.grid.is_group(i) {
            Some(i)
        } else if verb == Fold::Toggle {
            None
        } else {
            self.grid.parent(i)
        };
        let Some(t) = target else {
            return;
        };
        let folded = match verb {
            Fold::Toggle => !self.grid.is_collapsed(t),
            Fold::Open => false,
            Fold::Close | Fold::OpenAll | Fold::CloseAll => true,
        };
        if folded
            && t != i
            && let Some(pos) = self.grid.visible().iter().position(|&v| v == t)
        {
            self.grid.click(pos, false);
        }
        self.set_folded(t, folded, cx);
    }

    /// Fold or open group row `row` (an index into the grid's rows): the
    /// saved set is written by path, then applied, so the next rebuild
    /// keeps the fold.
    fn set_folded(&mut self, row: usize, folded: bool, cx: &mut Context<Self>) {
        let path = grouping::full_path(self.grid.row(row));
        if folded {
            self.state.collapsed.insert(path);
        } else {
            self.state.collapsed.remove(&path);
        }
        self.apply_folds(cx);
    }

    /// Hand the grid the collapsed set and repaint; the aggregates are
    /// unchanged, so nothing is re-prepared.
    fn apply_folds(&mut self, cx: &mut Context<Self>) {
        self.grid
            .set_collapsed(self.state.collapsed.iter().map(|p| group_name(p)));
        self.sync_table(cx);
        cx.notify();
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
        let refused = match self.cursor_row() {
            None => return,
            Some(row) if row.group.is_some() => Some("inspect needs an underlying".to_string()),
            Some(row) => (self.board.is_some() && !row.present)
                .then(|| format!("{} is not in the surface", row.name)),
        };
        if let Some(text) = refused {
            self.notices.refused = Some(text);
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

    /// Mirror the line-number setting and re-sync the table on a change:
    /// the gutter's texts and the Underlying column's width follow it.
    fn on_ui_settings(&mut self, cx: &mut Context<Self>) {
        let mode = ui_line_numbers(cx);
        if self.line_numbers != mode {
            self.line_numbers = mode;
            self.sync_table(cx);
            cx.notify();
        }
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

    /// `shift+h`/`shift+l`: move the cursor's unit (a pair's level and
    /// companion together, a sparkline or vega alone) `count` places in
    /// `dir`, stopping at the ends; a unit already at that end refuses.
    /// The whole order is saved for the shown view, as stems, so a stem
    /// the tab no longer has drops here; the cursor follows its column.
    /// Tile presentation only: `vol_views.toml` is never written.
    fn move_column(&mut self, dir: i64, count: Option<u32>, cx: &mut Context<Self>) {
        let units = units(&self.columns);
        let Some(at) = units.iter().position(|(_, r)| r.contains(&self.cursor_col)) else {
            return;
        };
        let Some(view) = self.shown_view() else {
            return;
        };
        let by = i64::from(count.unwrap_or(1).max(1)) * dir;
        let last = units.len() as i64 - 1;
        let to = (at as i64).saturating_add(by).clamp(0, last) as usize;
        if to == at {
            self.notices.refused = Some(COLUMN_AT_END.into());
            self.rebuild_chrome(cx);
            cx.notify();
            return;
        }
        let mut stems: Vec<String> = units.into_iter().map(|(s, _)| s).collect();
        let moved = stems.remove(at);
        stems.insert(to, moved);
        let cursor_id = self.columns[self.cursor_col].id.clone();
        self.state.column_order.insert(view.name, stems);
        self.rebuild_columns();
        if let Some(ix) = self.columns.iter().position(|c| c.id == cursor_id) {
            self.cursor_col = ix;
        }
        self.prepare();
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
        if self.cursor_row().is_some_and(|r| r.group.is_some()) {
            self.notices.refused = Some("timeseries needs an underlying".into());
            self.rebuild_chrome(cx);
            cx.notify();
            return;
        }
        let Some(c) = self.columns.get(self.cursor_col) else {
            return;
        };
        let measure = match &c.kind {
            ColumnKind::Level { measure, .. }
            | ColumnKind::Companion { measure, .. }
            | ColumnKind::Single { measure, .. } => Some(measure),
            ColumnKind::Underlying | ColumnKind::Spark { .. } | ColumnKind::Vega => None,
        };
        let name = self.cursor_row().map(|r| r.name.clone());
        let text = match (measure.and_then(|m| m.inputs().into_iter().next()), name) {
            (Some(input), Some(member)) => {
                let dataset = match &input.role {
                    Role::Surface => SURFACE,
                    Role::Realized => REALIZED,
                    Role::Varswap => VARSWAP,
                    Role::Table(dataset) => dataset.as_str(),
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
    /// selected row, one per line; a selection ends. On a group row with
    /// no selection, its members' names one per line.
    fn yank(&mut self, cx: &mut Context<Self>) {
        if self.grid.selected().is_none()
            && let Some(g) = self.cursor_row().and_then(|r| r.group.as_ref())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(g.members.join("\n")));
            cx.notify();
            return;
        }
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

    /// The cursor row's path: the applicable group values in chain order,
    /// then `underlying_ref` on a member, so a follower narrows to the
    /// group on a group row and to the member on a leaf. A path through
    /// the unmapped group names its dimension as NULL and posts nothing.
    /// `Nothing` while no row is under the cursor.
    pub(crate) fn emission(&self) -> Emission {
        let cursor = match self.cursor_row() {
            None => CursorScope::Nothing,
            Some(row) => {
                let mut pairs: Vec<(String, String)> = self
                    .grouping
                    .kept
                    .iter()
                    .cloned()
                    .zip(row.path.iter().cloned())
                    .collect();
                match (&row.group, self.grouping.kept.get(row.path.len())) {
                    (Some(g), Some(dim)) => pairs.push((dim.clone(), g.label.clone())),
                    (Some(_), None) => {}
                    (None, _) => pairs.push((UNDERLYING.to_string(), row.name.clone())),
                }
                match pairs.iter().find(|(_, v)| v == grouping::UNMAPPED) {
                    // A member's own key is exact for its row whatever
                    // group it fell under; the `—` group itself has no
                    // value a scope can say, so it refuses.
                    Some(_) if row.group.is_none() => {
                        CursorScope::Path(underlying_scope(&row.name))
                    }
                    Some((dim, _)) => CursorScope::NullIn(dim.clone()),
                    None => CursorScope::Path(path_scope(&pairs)),
                }
            }
        };
        Emission {
            cursor,
            ..Emission::default()
        }
    }

    /// Show the tab `name` (`g N`, the strip, the menu, `:view`): its
    /// columns are laid out and the board asked again only when its ask
    /// is not inside the last one. A name the document lacks refuses.
    fn set_view_by_name(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if self.shared.settings.borrow().views.get(name).is_none() {
            return Err(format!("no view '{name}'"));
        }
        self.state.view = Some(name.to_string());
        self.notices.warning = None;
        self.rebuild_columns();
        self.sync_sort();
        self.resubmit(false, cx);
        self.prepare();
        self.sync_table(cx);
        self.rebuild_chrome(cx);
        cx.notify();
        Ok(())
    }

    /// `tab` / `shift+tab`: the next or previous tab, wrapping; nothing
    /// with no views.
    fn step_view(&mut self, delta: isize, cx: &mut Context<Self>) {
        let tabs = self.tabs();
        let Some(current) = self.shown_view() else {
            return;
        };
        let n = tabs.len() as isize;
        let at = tabs.iter().position(|t| *t == current.name).unwrap_or(0) as isize;
        let next = ((at + delta) % n + n) % n;
        let _ = self.set_view_by_name(&tabs[next as usize], cx);
    }

    /// One door for every lens key and chip: the columns are rebuilt (the
    /// companion head and a single view's group heads follow the lens),
    /// the sort re-resolved, and the board asked again only when
    /// `requery` (a window or lookback change; a lens, metric or shown
    /// change paints from the board held).
    pub(super) fn set_lens(
        &mut self,
        f: impl FnOnce(&mut LensState),
        requery: bool,
        cx: &mut Context<Self>,
    ) {
        let before = self.state.lens;
        f(&mut self.state.lens);
        if self.state.lens == before {
            return;
        }
        self.rebuild_columns();
        self.sync_sort();
        if requery {
            self.resubmit(false, cx);
        }
        self.prepare();
        self.sync_table(cx);
        cx.notify();
    }

    /// D1 → W1 → M1 → D1; the request carries the one in force.
    fn cycle_lookback(&mut self, cx: &mut Context<Self>) {
        let next = match self.state.lens.lookback {
            Lookback::D1 => Lookback::W1,
            Lookback::W1 => Lookback::M1,
            Lookback::M1 => Lookback::D1,
        };
        self.set_lens(|l| l.lookback = next, true, cx);
    }

    /// `:sort <col> [asc|desc|abs]`, bare `:sort` restores member order;
    /// `:view <name>` shows a tab; `:group <dims> | slot N | none` pins a
    /// grouping and `:unpin` releases it to the frame. A pin that groups
    /// nothing refuses rather than pin an ungrouped list the trader did
    /// not ask for; `:group underlying_ref` alone is the flat list by name
    /// and pins.
    pub(crate) fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String> {
        match commands::parse(line, &self.columns)? {
            Command::Sort { column, order } => self.set_sort(Some((column, order)), cx),
            Command::SortClear => self.set_sort(None, cx),
            Command::View(name) => self.set_view_by_name(&name, cx)?,
            Command::Group(dims) => {
                let a = grouping::applicable(&dims, &self.shared.dims.borrow());
                if a.kept.is_empty() && !dims.iter().any(|d| d == geode_core::link::UNDERLYING) {
                    return Err(format!(
                        "nothing in {} applies to a vol tile: it groups by underlying_ref or a classification over it",
                        dims.join(" ")
                    ));
                }
                self.state.pin = Pin::Grouping(dims);
                self.regroup(true, cx);
            }
            Command::GroupSlot(n) => {
                if self.frame.read(cx).slots().get(n).is_none() {
                    return Err(format!("slot {n} is empty"));
                }
                self.state.pin = Pin::Slot(n);
                self.regroup(true, cx);
            }
            Command::GroupNone => {
                self.state.pin = Pin::Flat;
                self.regroup(true, cx);
            }
            Command::Unpin => {
                self.state.pin = Pin::None;
                self.regroup(true, cx);
            }
        }
        Ok(())
    }

    pub(crate) fn completions(&self, line: &str, cursor: usize) -> Vec<String> {
        let dims = grouping::vocabulary(&self.shared.dims.borrow());
        commands::completions(line, cursor, &self.columns, &self.tabs(), &dims)
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

    /// `:autosize` / `tile::autosize_columns`: fit the shown view's
    /// columns to their head and widest cell at the window's rem, or
    /// `reset` that view to its prepared widths. Another view's widths
    /// are untouched. Nothing to measure before a board has landed
    /// refuses.
    pub(crate) fn autosize_columns(
        &mut self,
        reset: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let fit = if reset {
            None
        } else {
            // Before a board lands the rows are blank placeholders.
            if self.board.is_none() {
                return Err(NOTHING_TO_FIT);
            }
            let metrics = FitMetrics::xsmall_mono(window.rem_size());
            Some(
                self.table
                    .read(cx)
                    .delegate()
                    .fit_columns(&metrics)
                    .ok_or(NOTHING_TO_FIT)?,
            )
        };
        let view = self.shown_view().map(|v| v.name).unwrap_or_default();
        let prefix = sized_key(&view, "");
        self.table.update(cx, |t, cx| {
            let mut fitted = t.delegate().fitted.clone();
            fitted.retain(|k, _| !k.starts_with(&prefix));
            fitted.extend(fit.into_iter().flatten());
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
