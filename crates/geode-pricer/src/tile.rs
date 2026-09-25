//! The line-pricer tile (spec §8): one `Sheet`, an `Rc<GridModel>`
//! installed into a `DataTable`, a header and a footer.
//!
//! **One door per kind of change.** A request-changing edit goes through
//! `apply_edit` (or `apply_edits`), which records its undo; a delivery through
//! `deliver`; a tick through `tick`. Each ends at `rebuild`, which builds
//! the grid model (never in `render`), installs it, re-prepares the
//! header and notifies.

use crate::content::{PricerSettings, Shared};
use crate::core::cell::{self, CellEditor};
use crate::core::clip::{put_place, spec_of};
use crate::core::columns::ColumnKind;
use crate::core::commands::{self, Command, ShiftField};
use crate::core::edit::{Edit, EditError, Undo};
use crate::core::entry::{history, next_place, place_for};
use crate::core::sheet::{Delivered, LineId, Refresh, Sheet};
use crate::core::shorthand::parse;
use crate::core::storage::{from_rows, to_rows};
use crate::core::template::Template;
use crate::core::tree::Expansion;
use crate::core::undo::UndoStack;
use crate::core::views::ColumnPlan;
use crate::core::{Place, RowSpec};
use crate::delegate::{ChevronClicked, EditorPaint, SheetDelegate};
use crate::grid::{GridModel, GridRowKind};
use crate::header::{self, HeaderInputs, HeaderModel};
use crate::popup::{Menu, MenuItem, choice_paint, render_menu};
use crate::session::Record;
use crate::store::Loaded;
use chrono::Utc;
use geode_core::clock::Clock;
use geode_core::document::DocumentRows;
use geode_core::pricing::{PriceLine, PriceOutcome, PriceParams};
use geode_core::query::QueryKey;
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, find_match};
use geode_shell::vimnav::NavCommand;
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Context, Entity, Focusable as _, SharedString, Task, Window, div,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, v_flex};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub(crate) const LOADING: &str = "loading…";

/// The wait before a refused submission asks again (planning decision
/// 5): nothing else would ever resubmit with the refresh timer off.
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(1);
/// The longest wait between retries: a data service that is gone for good
/// is asked twice a minute, not every second forever.
pub(crate) const RETRY_CAP: Duration = Duration::from_secs(30);

/// The wait after the `refusals`th consecutive refusal: `RETRY_AFTER`,
/// doubled per refusal after the first, capped at `RETRY_CAP`.
fn retry_delay(refusals: u32) -> Duration {
    let doublings = refusals.saturating_sub(1).min(5);
    (RETRY_AFTER * 2u32.pow(doublings)).min(RETRY_CAP)
}
pub(crate) const REFUSED: &str =
    "pricing request refused: the data service is busy or gone; retrying";

/// Spec §7.3: how long a change waits before the write-behind save fires.
/// A change inside the window re-arms it (replacing the task drops the
/// old one), so a burst saves once.
pub(crate) const SAVE_IDLE: Duration = Duration::from_secs(1);
pub(crate) const NOT_SAVED: &str = "sheet not saved: the store refused it; the next edit retries";

/// The save slot's standing notice after a failed load (see
/// `PricerTile::save_blocked`).
fn blocked_notice(name: &str, why: &str) -> SharedString {
    format!("sheet '{name}' did not load ({why}); edits are not saved").into()
}

/// `ctrl+d`/`ctrl+u` and `ctrl+f`/`ctrl+b` steps — `vimnav`'s fixed ±5
/// and ±10, the market-data panel's own constants, times the count.
pub(crate) const HALF_PAGE: usize = 5;
pub(crate) const FULL_PAGE: usize = 10;

/// `/` over the tree column's text (spec §8.5): the vim jump model — a
/// sheet's rows are the trader's own order, so find moves the cursor and
/// never narrows.
struct FindState {
    /// Where `/` opened; `escape` returns here.
    origin: Cursor,
    /// The last committed query, for `n`/`N`.
    committed: Option<String>,
}

/// The cursor by line identity (planning decision 10): an edit elsewhere,
/// a delivery or an expansion never moves it. `last_row` is where it was,
/// for the fallback when its line goes away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub line: Option<LineId>,
    /// A plan column (the tree column is never a cursor target).
    pub col: usize,
    pub last_row: usize,
}

/// The entry field (spec §8.4): where its rows will land, the field, and
/// the sheet's own lines to walk with `up`/`down`.
pub(crate) struct Entry {
    pub place: Place,
    pub input: Entity<InputState>,
    history: Vec<String>,
    /// `None`: the trader's own text; `Some(i)`: showing `history[i]`.
    history_ix: Option<usize>,
}

const ENTRY_HINT: &str = "-5 SPX DEC26 95%/105% CS";

/// A commit whose target line went away, or whose column is no longer the
/// one it opened on (a view switch), refuses with this.
pub(crate) const MOVED: &str = "the cell moved; edit refused";

/// The open cell editor (spec §8.4): its target by line identity and
/// column kind — re-checked at commit, so a line deleted or a view
/// switched under an open editor refuses rather than writing elsewhere.
pub(crate) enum Editor {
    Text {
        line: LineId,
        col: usize,
        kind: ColumnKind,
        input: Entity<InputState>,
    },
    Choice {
        line: LineId,
        col: usize,
        kind: ColumnKind,
        input: Entity<InputState>,
        list: ChoiceList,
        /// An unmatched query commits as typed (planning decision 17).
        free: bool,
        /// The trader moved the highlight (`up`/`down`, a row click) since
        /// the query last changed. In a `free` list, `enter` takes the
        /// highlighted option only when this is set or the query IS that
        /// option (case-insensitively): ranking is a subsequence match, so
        /// an untouched highlight is a guess — `HSI` would commit `HSCEI`.
        moved: bool,
    },
}

/// What an `enter` in the cell editor means before the cell parses it.
enum Choice {
    Value(String),
    /// Close with the cell unchanged (an untouched, empty free list).
    Keep,
    /// A closed vocabulary with nothing matching the query.
    NoMatch,
}

impl Editor {
    fn input(&self) -> &Entity<InputState> {
        match self {
            Editor::Text { input, .. } | Editor::Choice { input, .. } => input,
        }
    }

    fn target(&self) -> (LineId, usize, ColumnKind) {
        match self {
            Editor::Text {
                line, col, kind, ..
            }
            | Editor::Choice {
                line, col, kind, ..
            } => (*line, *col, *kind),
        }
    }

    fn set_col(&mut self, to: usize) {
        match self {
            Editor::Text { col, .. } | Editor::Choice { col, .. } => *col = to,
        }
    }
}

pub struct PricerTile {
    pub(crate) id: TileId,
    // Nothing reads it yet: the flip-barrier observer in `new` holds its
    // own handle.
    #[allow(dead_code)]
    frame: Entity<Frame>,
    pub(crate) data: DataHandle,
    pub(crate) shared: Rc<Shared>,
    pub(crate) sheet: Sheet,
    pub(crate) expansion: Expansion,
    pub(crate) plan: ColumnPlan,
    pub(crate) model: Rc<GridModel>,
    pub(crate) table: Entity<TableState<SheetDelegate>>,
    pub(crate) cursor: Cursor,
    pub(crate) visible: bool,
    pub(crate) loading: bool,
    /// The record's `expanded` while a load is pending: applied by
    /// `loaded`, and what `serialize` writes meanwhile, so a session save
    /// mid-load never overwrites a good record.
    held_expanded: Option<Vec<LineId>>,
    /// Transient header notice (an absent document, loading).
    pub(crate) notice: Option<SharedString>,
    /// Consecutive refused submissions (0: none standing). While non-zero
    /// the header shows `REFUSED` over `notice` without touching it, so
    /// the notice it covered returns when the streak ends — an admitted
    /// submission, or one with nothing left to ask for.
    refusals: u32,
    /// The view fallback's standing notice (`resolve_plan`).
    view_notice: Option<SharedString>,
    /// The save state's own header slot (spec §7.3): `NOT_SAVED` after a
    /// refused save, or `blocked_notice` after a failed load. Pricing
    /// notices never write it and `escape` does not clear it; only an
    /// accepted save does.
    save_notice: Option<SharedString>,
    /// A load FAILED (undecodable rows, or `loaded(Err(..))`): the sheet
    /// shown is the fallback, and saving it would publish it over the real
    /// document as the latest generation. `save_now` publishes nothing for
    /// the life of the tile. A genuinely absent document (`Missing`,
    /// `Ok(None)`) does not set this: §7.4 opens it empty under its name.
    save_blocked: bool,
    /// A change armed a save that no accepted save has published yet.
    /// Cleared only by an accepted save, so `on_release` flushes a refused
    /// save as well as one still waiting on its idle timer.
    dirty: bool,
    /// A user error for the footer (spec §8.3); cleared by the next verb.
    pub(crate) footer: Option<SharedString>,
    /// What the footer paints: `footer`, else the cursor row's failure.
    pub(crate) footer_text: Option<SharedString>,
    pub(crate) header: HeaderModel,
    title: SharedString,
    stack: Option<StackHandle>,
    pub(crate) clock: Clock,
    /// What `p`/`shift+p` put: the last `y y` or `d d`.
    pub(crate) register: Option<crate::core::RowSpec>,
    find: Option<FindState>,
    /// The latest submission's tag: an outcome with any other is dropped
    /// whole (spec §9.2).
    pub(crate) tag: u64,
    /// `id → revision` of the latest batch (planning decision 4): decides
    /// WHETHER to submit, never what — a batch always carries every stale
    /// line.
    in_flight: HashMap<LineId, u64>,
    /// Every inverse is recorded against the rows its edit left, so an
    /// edit reaches the sheet only through `apply_edit`/`apply_edits`
    /// (which record) and `history_step` (which replays); any other edit
    /// would leave an entry pointing at rows that moved.
    pub(crate) undo: UndoStack,
    refresh_task: Option<Task<()>>,
    retry_task: Option<Task<()>>,
    /// The write-behind idle timer (spec §7.3): armed by every change,
    /// re-armed by the next one, flushed by `on_release`.
    save_task: Option<Task<()>>,
    /// The open entry field (spec §8.4): `o`/`shift+o` open it, `enter`
    /// parses and inserts through `apply_edit`, `escape` or a click drops
    /// it. `None` in normal mode.
    pub(crate) entry: Option<Entry>,
    /// The open cell editor (spec §8.4): `i`/`enter`/double-click open it,
    /// `enter` commits one `Edit` through `apply_edit`, `escape` or a click
    /// drops it. `None` outside insert mode.
    pub(crate) editor: Option<Editor>,
    /// The window the editor last opened in: a rebuild that must close it
    /// has no `Window` of its own to blur through (`drop_orphaned_editor`).
    editor_window: Option<AnyWindowHandle>,
    /// The `.` action menu: `None` outside menu mode.
    pub(crate) menu: Option<Menu>,
    /// What a cell press that closed the entry field resolved — its line,
    /// `None` on the placeholder — with the grid row it was painted at
    /// (see `on_table_event`).
    click_anchor: Option<(usize, Option<LineId>)>,
    /// `click_anchor`, taken by the next press when it hit the same row:
    /// read only by that press's own `DoubleClickedCell` (every press
    /// emits `SelectCell` first, which overwrites it).
    pressed: Option<(usize, Option<LineId>)>,
}

fn app_clock(cx: &App) -> Clock {
    cx.try_global::<geode_shell::clock::AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| Clock::machine().0)
}

/// The first `untitled-N` with no document and no open tile (spec §7.4).
fn untitled(shared: &Shared) -> String {
    (1..)
        .map(|n| format!("untitled-{n}"))
        .find(|name| !shared.open.borrow().contains(name) && !shared.store.contains(name))
        .expect("an unbounded range finds a free name")
}

/// The flat row an insert at `place` puts its first row on.
fn landed_row(place: Place) -> usize {
    match place {
        Place::Root { at } => at,
        Place::Leg { package, leg } => package + 1 + leg,
    }
}

/// An empty sheet under `name`, carrying the record's view and refresh —
/// what a restore shows when there is no document to say otherwise.
fn fallback(name: &str, record: &Record) -> Sheet {
    let mut s = Sheet::new(name);
    if let Some(v) = &record.view {
        s.view = v.clone();
    }
    if let Some(r) = record.refresh {
        s.refresh = r;
    }
    s
}

impl PricerTile {
    /// `pub(crate)`: it takes the factory's crate-private [`Shared`];
    /// the shell reaches a tile only through `PricerFactory::create`.
    pub(crate) fn new(
        id: TileId,
        frame: Entity<Frame>,
        data: DataHandle,
        shared: Rc<Shared>,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let record = restored.map(Record::from_table).unwrap_or_default();
        let mut notices: Vec<String> = Vec::new();
        let mut blocked: Option<SharedString> = None;
        let name = match record.sheet.as_deref() {
            Some(n) if !shared.open.borrow().contains(n) => n.to_string(),
            Some(n) => {
                let fresh = untitled(&shared);
                notices.push(format!(
                    "sheet '{n}' is open in another tile; opened {fresh}"
                ));
                fresh
            }
            None => untitled(&shared),
        };
        shared.open.borrow_mut().insert(name.clone());
        let (sheet, loading) = match shared.store.load(&name) {
            Loaded::Rows(rows) => match from_rows(&name, &rows) {
                Ok(mut s) => {
                    s.mark_all_stale();
                    (s, false)
                }
                Err(e) => {
                    blocked = Some(blocked_notice(&name, &e));
                    (fallback(&name, &record), false)
                }
            },
            Loaded::Missing => {
                if record.sheet.is_some() && notices.is_empty() {
                    notices.push(format!("sheet '{name}' was not found; opened empty"));
                }
                (fallback(&name, &record), false)
            }
            Loaded::Pending => {
                notices.push(LOADING.to_string());
                (fallback(&name, &record), true)
            }
        };
        // A pending load's expansion waits for the rows: pruned against
        // the empty fallback it would lose every package (`loaded` applies
        // it). The record's cursor waits in `cursor.line`, which
        // `reconcile_cursor` leaves alone while loading.
        let (expansion, held_expanded) = if loading {
            (Expansion::default(), Some(record.expanded.clone()))
        } else {
            let mut e = Expansion::from_ids(record.expanded.iter().copied());
            e.retain_packages(&sheet);
            (e, None)
        };

        let delegate = SheetDelegate::new(cx.theme(), cx.weak_entity());
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(true)
                .row_header(false)
                .loop_selection(false)
                .col_resizable(false)
                .col_movable(false)
                .sortable(false)
        });
        cx.subscribe_in(&table, window, |this, _, event: &TableEvent, window, cx| {
            this.on_table_event(event, window, cx)
        })
        .detach();
        cx.subscribe_in(
            &table,
            window,
            |this, _, event: &ChevronClicked, window, cx| this.chevron_clicked(event.0, window, cx),
        )
        .detach();
        // Planning decision 6: arrive at every flip barrier at once.
        cx.observe(&frame, |this, frame, cx| {
            let key = QueryKey(this.id.0);
            let now = frame.read(cx).versions();
            if frame.read(cx).barrier_wants(key, now) {
                frame.update(cx, |f, cx| {
                    if f.arrived(key, now) {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        // The paints are a per-theme memo (planning decision 14): re-derived
        // here, once per theme change, never per cell.
        cx.observe_global::<gpui_component::Theme>(|this, cx| {
            let paints = crate::paint::Paints::derive(cx.theme());
            this.table.update(cx, |t, cx| {
                t.delegate_mut().paints = paints;
                cx.notify();
            });
        })
        .detach();
        // `priced_at` cells and the header time follow the app clock.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.clock = app_clock(cx);
            this.rebuild(cx);
        })
        .detach();
        // A closed tile flushes its sheet, cancels its pricing and gives
        // its name back (spec §7.4's open set).
        cx.on_release(|this: &mut PricerTile, _cx| {
            // Spec §7.3: the sheet is not lost until the tile is — a save
            // still waiting on its idle timer, or one the store refused,
            // runs now (`save_now` itself refuses a blocked sheet).
            this.save_task = None;
            if this.dirty {
                this.save_now();
            }
            this.data.cancel(QueryKey(this.id.0));
            this.shared.open.borrow_mut().remove(&this.sheet.name);
        })
        .detach();

        let cursor = Cursor {
            line: record.cursor,
            col: 0,
            last_row: 0,
        };
        let mut this = PricerTile {
            id,
            frame,
            data,
            shared,
            sheet,
            expansion,
            plan: ColumnPlan::default(),
            model: Rc::new(GridModel::default()),
            table,
            cursor,
            visible: false,
            loading,
            held_expanded,
            notice: (!notices.is_empty()).then(|| notices.join("; ").into()),
            refusals: 0,
            view_notice: None,
            save_blocked: blocked.is_some(),
            save_notice: blocked,
            dirty: false,
            footer: None,
            footer_text: None,
            header: HeaderModel::default(),
            title: SharedString::default(),
            stack: None,
            clock: app_clock(cx),
            register: None,
            find: None,
            tag: 0,
            in_flight: HashMap::new(),
            undo: UndoStack::default(),
            refresh_task: None,
            retry_task: None,
            save_task: None,
            entry: None,
            editor: None,
            editor_window: None,
            menu: None,
            click_anchor: None,
            pressed: None,
        };
        this.resolve_plan();
        this.rebuild(cx);
        this
    }

    // ---- what the shell reads ----------------------------------------

    /// `normal`, `insert` or `menu`.
    pub fn key_context(&self) -> KeyContext {
        KeyContext::new("pricer").pair("mode", self.mode()).counts()
    }

    /// `insert` while EITHER text field is open — the entry field or the
    /// cell editor. The shell treats a key as typing only when the focused
    /// tile holds focus AND its context reads `mode == insert`
    /// (`ShellView::occupant_insert_stack`); any other word lets a bare or
    /// shifted letter reach the shell's own bindings (`shift+d` duplicated
    /// the tile). `dispatch` tells the two fields apart by which is open.
    pub(crate) fn mode(&self) -> &'static str {
        if self.entry.is_some() || self.editor.is_some() {
            "insert"
        } else if self.menu.is_some() {
            "menu"
        } else {
            "normal"
        }
    }

    /// Does one of THIS tile's own fields (the entry field, the cell
    /// editor or the typeahead's field) hold window focus? Answered from
    /// the focus handles, never from the mode.
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        let entry = self
            .entry
            .as_ref()
            .is_some_and(|e| e.input.read(cx).focus_handle(cx).is_focused(window));
        let editor = self
            .editor
            .as_ref()
            .is_some_and(|e| e.input().read(cx).focus_handle(cx).is_focused(window));
        entry || editor
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    pub fn serialize(&self) -> toml::Table {
        Record {
            sheet: Some(self.sheet.name.clone()),
            view: Some(self.sheet.view.clone()),
            refresh: Some(self.sheet.refresh),
            cursor: self.cursor.line,
            expanded: match &self.held_expanded {
                Some(held) => held.clone(),
                None => self.expansion.live_ids(&self.sheet).collect(),
            },
        }
        .to_table()
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    /// A show reprices what is stale and starts the timer; a hide cancels
    /// in flight by key and stops it, keeping the stale marks (spec §9.5).
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.submit(cx);
            self.restart_timer(cx);
        } else {
            self.data.cancel(QueryKey(self.id.0));
            self.in_flight.clear();
            self.refresh_task = None;
            self.retry_task = None;
        }
        self.rebuild_chrome();
        cx.notify();
    }

    /// `/` (spec §8.5): every keystroke searches from the ORIGIN, so a
    /// lengthening query walks forward and a shortened one walks back
    /// (vim's incsearch); `escape` returns to the origin.
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        // `/` is a shell-owned binding, so `dispatch`'s own "any other
        // verb closes the menu and the fields" guard never sees it
        // (the market-data rule).
        self.close_menu(cx);
        self.close_entry(window, cx);
        self.close_editor(window, cx);
        match event {
            FindEvent::Changed(query) => {
                let origin = match &self.find {
                    Some(f) => f.origin,
                    None => {
                        self.find = Some(FindState {
                            origin: self.cursor,
                            committed: None,
                        });
                        self.cursor
                    }
                };
                let from = origin
                    .line
                    .and_then(|id| self.model.grid_row_of(id))
                    .unwrap_or(0);
                if let Some(row) =
                    find_match(&self.row_labels(), from, FindDirection::Forward, &query)
                {
                    self.set_cursor_row(row);
                }
            }
            FindEvent::Committed(query) => {
                if let Some(f) = self.find.as_mut()
                    && !query.is_empty()
                {
                    f.committed = Some(query);
                } else if !query.is_empty() {
                    self.find = Some(FindState {
                        origin: self.cursor,
                        committed: Some(query),
                    });
                }
            }
            FindEvent::Cancelled => {
                if let Some(f) = self.find.take() {
                    self.cursor = f.origin;
                }
            }
        }
        self.sync_cursor(cx);
        self.rebuild_chrome();
        cx.notify();
    }

    /// The one edit door (global constraints): apply, record the undo,
    /// then everything an edit implies.
    pub(crate) fn apply_edit(
        &mut self,
        edit: Edit,
        cx: &mut Context<Self>,
    ) -> Result<(), EditError> {
        let undo = self.sheet.apply(edit)?;
        self.undo.record(undo);
        self.after_edit(cx);
        Ok(())
    }

    /// Several edits as ONE undo entry (`:spot clear`). On a
    /// refusal the ones already applied are taken back and nothing is
    /// recorded.
    pub(crate) fn apply_edits(
        &mut self,
        edits: Vec<Edit>,
        cx: &mut Context<Self>,
    ) -> Result<(), EditError> {
        let mut undos: Vec<Undo> = Vec::new();
        for e in edits {
            match self.sheet.apply(e) {
                Ok(u) => undos.push(u),
                Err(err) => {
                    for u in undos.iter().rev() {
                        // Each inverse was recorded against the rows its
                        // edit left, so a refusal here means the sheet is
                        // partly rolled back and the history may point at
                        // rows that moved: drop it (`UndoStack`'s own rule
                        // for a refused inverse) and stop unwinding.
                        if let Err(back) = self.sheet.undo(u) {
                            tracing::error!(
                                target: "geode::pricing",
                                tile = self.id.0,
                                error = %back,
                                refused = %err,
                                "rolling back a refused multi-edit failed; the sheet is partly \
                                 applied and the undo history is cleared"
                            );
                            self.undo.clear();
                            break;
                        }
                    }
                    self.after_edit(cx);
                    return Err(err);
                }
            }
        }
        if undos.is_empty() {
            return Ok(());
        }
        // Take back the LAST edit first.
        self.undo.record(Undo {
            inverse: undos.into_iter().rev().flat_map(|u| u.inverse).collect(),
        });
        self.after_edit(cx);
        Ok(())
    }

    /// `o` / `shift+o`: a placeholder after (before) the cursor row, the
    /// field focused (spec §8.4). A leg place opens its package so the
    /// placeholder shows where it lands.
    fn open_entry(&mut self, below: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            self.footer = Some("the sheet is still loading".into());
            return;
        }
        self.close_entry(window, cx);
        let place = place_for(&self.sheet, self.cursor_sheet_row(), below);
        if let Place::Leg { package, .. } = place {
            self.expansion.set(self.sheet.id(package), true);
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(ENTRY_HINT));
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.entry = Some(Entry {
            place,
            input: input.clone(),
            history: history(&self.sheet),
            history_ix: None,
        });
        self.table
            .update(cx, |t, _| t.delegate_mut().entry = Some(input));
        self.rebuild(cx);
    }

    /// `enter`: parse, insert, reprice, and open the next placeholder
    /// below what landed; a parse error or a refusal keeps the text and
    /// says why in the footer.
    fn commit_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.as_mut() else {
            return;
        };
        let text = entry.input.read(cx).value().to_string();
        let spec = match parse(&text) {
            Ok(spec) => spec,
            Err(e) => {
                self.footer = Some(format!("{} (column {})", e.message, e.offset + 1).into());
                self.rebuild_chrome();
                cx.notify();
                return;
            }
        };
        let at = entry.place;
        // Move the placeholder first so the edit's own rebuild paints it
        // below what landed; put it back on a refusal.
        entry.place = next_place(at, &spec);
        match self.apply_edit(
            Edit::Insert {
                place: at,
                rows: vec![spec.clone()],
            },
            cx,
        ) {
            Ok(()) => {
                let id = self.sheet.id(landed_row(at));
                if matches!(spec, RowSpec::Package { .. }) {
                    self.expansion.set(id, true);
                }
                self.cursor.line = Some(id);
                if let Some(entry) = self.entry.as_mut() {
                    entry.history = history(&self.sheet);
                    entry.history_ix = None;
                    entry.input.update(cx, |s, cx| s.set_value("", window, cx));
                }
                self.rebuild(cx);
            }
            Err(e) => {
                if let Some(entry) = self.entry.as_mut() {
                    entry.place = at;
                }
                self.footer = Some(e.to_string().into());
                self.rebuild(cx);
            }
        }
    }

    /// Blur only if OUR field holds focus, then drop it (the market-data
    /// rule, CLAUDE.md): an unblurred dead handle leaves the window
    /// focused on nothing and the shell's focus return never fires.
    pub(crate) fn close_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.take() else {
            return;
        };
        if entry.input.read(cx).focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
        self.table.update(cx, |t, _| t.delegate_mut().entry = None);
        self.rebuild(cx);
    }

    /// `up` walks back through the sheet's lines, `down` forward; past the
    /// newest is an empty field.
    fn step_history(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.as_mut() else {
            return;
        };
        if entry.history.is_empty() {
            return;
        }
        let last = entry.history.len() as isize - 1;
        let next = match entry.history_ix {
            None if delta > 0 => Some(0),
            None => None,
            Some(i) => {
                let j = i as isize + delta;
                if j < 0 {
                    None
                } else {
                    Some(j.min(last) as usize)
                }
            }
        };
        entry.history_ix = next;
        let text = next.map(|i| entry.history[i].clone()).unwrap_or_default();
        entry
            .input
            .update(cx, |s, cx| s.set_value(text, window, cx));
    }

    // ---- the cell editor (spec §8.4) -----------------------------------

    /// `i`/`enter`/double-click: a text field on the cell's grammar
    /// spelling, or a typeahead over its vocabulary; a cell that does not
    /// edit says why in the footer.
    fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            self.footer = Some("the sheet is still loading".into());
            return;
        }
        self.close_editor(window, cx);
        let (Some(row), Some(planned)) = (
            self.cursor_sheet_row(),
            self.plan.columns.get(self.cursor.col),
        ) else {
            return;
        };
        let (line, col, kind) = (self.sheet.id(row), self.cursor.col, planned.def.kind);
        let editor = match cell::editor_for(&self.sheet, row, kind) {
            Err(why) => {
                self.footer = Some(why.into());
                return;
            }
            Ok(CellEditor::Text(text)) => {
                let input = cx.new(|cx| InputState::new(window, cx));
                input.update(cx, |s, cx| s.set_value(text, window, cx));
                Editor::Text {
                    line,
                    col,
                    kind,
                    input,
                }
            }
            Ok(CellEditor::Choice {
                options,
                current,
                free,
            }) => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(current.clone()));
                // Every keystroke re-ranks. The subscription dies with the
                // field, which `close_editor` drops.
                cx.subscribe_in(
                    &input,
                    window,
                    |this, input, event: &InputEvent, _window, cx| {
                        if let InputEvent::Change = event {
                            let query = input.read(cx).value().to_string();
                            // The borrow of `list` ends before `sync_editor`.
                            let changed = match &mut this.editor {
                                Some(Editor::Choice {
                                    list,
                                    input: own,
                                    moved,
                                    ..
                                }) if &*own == input => {
                                    let changed = list.set_query(&query);
                                    if changed {
                                        *moved = false;
                                    }
                                    changed
                                }
                                _ => false,
                            };
                            if changed {
                                this.sync_editor(cx);
                            }
                        }
                    },
                )
                .detach();
                let mut list = ChoiceList::new(options, DEFAULT_CAP);
                list.place(Some(&current));
                Editor::Choice {
                    line,
                    col,
                    kind,
                    input,
                    list,
                    free,
                    moved: false,
                }
            }
        };
        editor.input().read(cx).focus_handle(cx).focus(window, cx);
        self.editor = Some(editor);
        self.editor_window = Some(window.window_handle());
        self.sync_editor(cx);
    }

    /// `enter`: the live text (re-read — `set_value` emits no `Change`),
    /// the target re-checked by line and column kind, parsed into one
    /// `Edit`; a bad value keeps the editor open with the reason. The
    /// editor closes (blur first) BEFORE the edit applies, so the rebuild
    /// never paints a dead field.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The editor's borrow ends inside this block, before any `self` call.
        let (value, (line, col, kind)) = {
            let Some(editor) = self.editor.as_mut() else {
                return;
            };
            let text = editor.input().read(cx).value().to_string();
            let target = editor.target();
            let value = match editor {
                Editor::Text { .. } => Choice::Value(text),
                Editor::Choice {
                    list, free, moved, ..
                } => {
                    list.set_query(&text);
                    let highlighted = list.pick().map(|i| list.options()[i].clone());
                    let typed = text.trim();
                    if !*free {
                        highlighted.map_or(Choice::NoMatch, Choice::Value)
                    } else {
                        // The highlight is only a subsequence guess until
                        // the trader moves it or types it out in full.
                        let take_highlight = *moved
                            || highlighted
                                .as_deref()
                                .is_some_and(|o| o.eq_ignore_ascii_case(typed));
                        match highlighted {
                            Some(o) if take_highlight => Choice::Value(o),
                            _ if typed.is_empty() => Choice::Keep,
                            _ => Choice::Value(typed.to_ascii_uppercase()),
                        }
                    }
                }
            };
            (value, target)
        };
        let value = match value {
            Choice::Value(v) => Some(v),
            // An untouched free list with nothing typed: the cell keeps
            // its value; no edit, no undo entry, no save.
            Choice::Keep => {
                self.close_editor(window, cx);
                self.rebuild_chrome();
                cx.notify();
                return;
            }
            Choice::NoMatch => None,
        };
        let Some(value) = value else {
            self.footer = Some("no option matches".into());
            self.sync_editor(cx);
            self.rebuild_chrome();
            cx.notify();
            return;
        };
        let same_column = self
            .plan
            .columns
            .get(col)
            .is_some_and(|c| c.def.kind == kind);
        let Some(row) = self.sheet.index_of(line).filter(|_| same_column) else {
            self.close_editor(window, cx);
            self.footer = Some(MOVED.into());
            self.rebuild_chrome();
            cx.notify();
            return;
        };
        match cell::commit(&self.sheet, row, kind, &value) {
            Err(why) => {
                self.footer = Some(why.into());
                self.rebuild_chrome();
                cx.notify();
            }
            Ok(edit) => {
                self.close_editor(window, cx);
                if let Err(e) = self.apply_edit(edit, cx) {
                    self.footer = Some(e.to_string().into());
                    self.rebuild_chrome();
                    cx.notify();
                }
            }
        }
    }

    /// A click on a typeahead row: highlight it, then commit.
    pub(crate) fn choice_pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let picked = match &mut self.editor {
            Some(Editor::Choice {
                list, input, moved, ..
            }) => list.set_highlighted(row).then(|| {
                // A row click is a deliberate choice, like `up`/`down`.
                *moved = true;
                (
                    input.clone(),
                    list.highlighted_text().unwrap_or_default().to_string(),
                )
            }),
            _ => None,
        };
        if let Some((input, text)) = picked {
            input.update(cx, |s, cx| s.set_value(text, window, cx));
            self.footer = None;
            self.commit_edit(window, cx);
        }
    }

    /// Blur only if OUR field holds focus, then drop it (the market-data
    /// rule, CLAUDE.md): an unblurred dead handle leaves the window
    /// focused on nothing and the shell's focus return never fires.
    pub(crate) fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        if editor.input().read(cx).focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
        self.sync_editor(cx);
        cx.notify();
    }

    /// `up`/`down` (`shift`: ten) step a numeric editor by its text's own
    /// precision (planning decision 2); in a typeahead they move the
    /// highlight.
    fn nudge(&mut self, steps: i64, window: &mut Window, cx: &mut Context<Self>) {
        let refused = match &mut self.editor {
            Some(Editor::Text { kind, input, .. }) => {
                let text = input.read(cx).value().to_string();
                match cell::nudge(*kind, &text, steps) {
                    Ok(next) => {
                        input.update(cx, |s, cx| s.set_value(next, window, cx));
                        None
                    }
                    Err(why) => Some(why),
                }
            }
            // `up` moves the highlight up the list: a negative step.
            Some(Editor::Choice { list, moved, .. }) => {
                list.nav(NavCommand::Move(-steps));
                *moved = true;
                None
            }
            None => None,
        };
        if let Some(why) = refused {
            self.footer = Some(why.into());
        }
        self.sync_editor(cx);
    }

    /// Mirror the open editor into the delegate: its grid cell (looked up
    /// by line — a delivery can rebuild the model under it), its field,
    /// and the typeahead's prepared rows.
    fn sync_editor(&mut self, cx: &mut Context<Self>) {
        let paint = self.editor.as_ref().and_then(|e| {
            let (line, col, _) = e.target();
            let row = self.model.grid_row_of(line)?;
            let choice = match e {
                Editor::Choice { list, .. } => Some(Rc::new(choice_paint(list))),
                Editor::Text { .. } => None,
            };
            Some(EditorPaint {
                row,
                col,
                input: e.input().clone(),
                choice,
            })
        });
        self.table.update(cx, |t, cx| {
            t.delegate_mut().editor = paint;
            cx.notify();
        });
    }

    /// What every edit, undo and redo implies: rebuild, reprice what
    /// changed, arm the write-behind save. The open set is left whole (a
    /// removed package's id stays, so an undo reinstates it open). The
    /// refresh timer is not touched: it runs whenever the tile is visible
    /// with an interval set, and `tick` skips an empty sheet.
    pub(crate) fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.rebuild(cx);
        self.submit(cx);
        self.arm_save(cx);
    }

    /// Spec §7.3: every change arms a one-second idle timer; a change
    /// inside the window re-arms it (replacing the task drops the old
    /// one). A pending load owns the sheet until `loaded` swaps it in, so
    /// the empty fallback is never armed to save over it.
    fn arm_save(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.dirty = true;
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_IDLE).await;
            let _ = this.update(cx, |t, cx| {
                t.save_task = None;
                t.save_now();
                t.rebuild_chrome();
                cx.notify();
            });
        }));
    }

    /// The whole sheet, once. An empty sheet publishes nothing (the last
    /// non-empty generation stays as history, spec §7.2); a refusal paints
    /// the save slot and stays `dirty`, so the next burst or the close
    /// retries. A sheet whose load failed publishes nothing at all: the
    /// fallback would become the document's latest generation.
    pub(crate) fn save_now(&mut self) {
        if self.save_blocked {
            return;
        }
        let Some(rows) = to_rows(&self.sheet) else {
            return;
        };
        if self.shared.store.save(&self.sheet.name, rows) {
            self.dirty = false;
            self.save_notice = None;
        } else {
            self.save_notice = Some(NOT_SAVED.into());
        }
    }

    /// One `PriceParams` of every stale line, when some stale line is not
    /// already in flight at its current revision (spec §9.1, planning
    /// decision 4). A hidden or loading tile submits nothing.
    pub(crate) fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.loading {
            return;
        }
        let stale: Vec<usize> = self.sheet.stale_lines().collect();
        let needed = stale
            .iter()
            .any(|r| self.in_flight.get(&self.sheet.id(*r)) != Some(&self.sheet.revision(*r)));
        if !needed {
            // A standing refusal with nothing left to ask for (its lines
            // were answered, deleted or hidden away) would otherwise say
            // "retrying" with no retry that could ever succeed.
            if self.refusals > 0 {
                self.end_refusals();
                self.rebuild_chrome();
                cx.notify();
            }
            return;
        }
        let lines: Vec<PriceLine> = stale
            .iter()
            .filter_map(|r| {
                self.sheet.request(*r).map(|request| PriceLine {
                    id: self.sheet.id(*r).0,
                    revision: self.sheet.revision(*r),
                    request,
                })
            })
            .collect();
        let flight: HashMap<LineId, u64> =
            lines.iter().map(|l| (LineId(l.id), l.revision)).collect();
        self.tag += 1;
        let queued = self.data.price(PriceParams {
            key: QueryKey(self.id.0),
            tag: self.tag,
            submitted: Instant::now(),
            overrides: self.sheet.overrides().clone(),
            lines,
        });
        if queued {
            self.in_flight = flight;
            self.end_refusals();
        } else {
            // Planning decision 5: nothing else would ever resubmit. One
            // log line per streak: a closed channel refuses every retry.
            if self.refusals == 0 {
                tracing::warn!(
                    target: "geode::pricing",
                    tile = self.id.0,
                    "pricing request refused; retrying with backoff until one is admitted"
                );
            }
            self.in_flight.clear();
            self.refusals = self.refusals.saturating_add(1);
            self.arm_retry(cx);
        }
        self.rebuild_chrome();
        cx.notify();
    }

    /// The streak is over: no notice over `notice`, no retry pending, and
    /// the next refusal starts again at `RETRY_AFTER` with a log line.
    fn end_refusals(&mut self) {
        self.refusals = 0;
        self.retry_task = None;
    }

    /// One retry at a time; an edit refused while one is pending waits on
    /// it rather than re-arming a shorter one.
    fn arm_retry(&mut self, cx: &mut Context<Self>) {
        if self.retry_task.is_some() {
            return;
        }
        let wait = retry_delay(self.refusals);
        self.retry_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |t, cx| {
                t.retry_task = None;
                t.submit(cx);
            });
        }));
    }

    /// `Delivery::Price` for this tile (spec §9.2).
    pub fn deliver(&mut self, outcome: PriceOutcome, cx: &mut Context<Self>) {
        if outcome.key != QueryKey(self.id.0) || outcome.tag != self.tag {
            return;
        }
        let ids: Vec<(u64, u64)> = outcome
            .results
            .iter()
            .map(|(id, rev, _)| (*id, *rev))
            .collect();
        let answers = self.sheet.deliver_all(
            outcome
                .results
                .into_iter()
                .map(|(id, rev, r)| (LineId(id), rev, r)),
            Utc::now(),
        );
        for ((id, rev), answer) in ids.into_iter().zip(answers) {
            match answer {
                Delivered::Installed | Delivered::OldRevision { .. } => {}
                // Deleted mid-round-trip: ordinary (planning decision 19).
                Delivered::UnknownLine => tracing::debug!(
                    target: "geode::pricing",
                    tile = self.id.0, id, rev,
                    "price result for a line no longer on the sheet"
                ),
                // Bugs (spec §10.1): dropped and logged with the ids.
                Delivered::NotALine | Delivered::FutureRevision { .. } => tracing::warn!(
                    target: "geode::pricing",
                    tile = self.id.0, id, rev, answer = ?answer,
                    "price result dropped"
                ),
            }
        }
        // The latest batch is answered (a cancelled one partly): whatever
        // is still stale — an edit landed mid-flight, or a line the cancel
        // cut off — is resubmitted.
        self.in_flight.clear();
        self.rebuild(cx);
        self.submit(cx);
    }

    fn interval(&self) -> Option<Duration> {
        match self.sheet.refresh {
            Refresh::Every(d) => Some(d),
            Refresh::Off => None,
            Refresh::Default => self.shared.settings.borrow().refresh,
        }
    }

    /// The periodic reprice (spec §9.4): running only while visible;
    /// every tick marks every line stale and submits. Dropping the task
    /// stops it.
    pub(crate) fn restart_timer(&mut self, cx: &mut Context<Self>) {
        self.refresh_task = None;
        if !self.visible {
            return;
        }
        let Some(every) = self.interval() else {
            return;
        };
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                if this.update(cx, |t, cx| t.tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// A tick on an empty or loading sheet does nothing ("only while the
    /// sheet has a line"); the timer stays armed and costs one wake.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.sheet.is_empty() || self.loading {
            return;
        }
        self.reprice_all(cx);
    }

    /// Every line stale, then one batch (`:price`, the `price` verb, a
    /// tick).
    fn reprice_all(&mut self, cx: &mut Context<Self>) {
        self.sheet.mark_all_stale();
        self.rebuild(cx);
        self.submit(cx);
    }

    /// A `Pending` load's answer (planning decision 7): Part 4's
    /// `Delivery::Query` arm calls this with the decoded document.
    pub fn loaded(&mut self, answer: Result<Option<DocumentRows>, String>, cx: &mut Context<Self>) {
        if !self.loading {
            return;
        }
        self.loading = false;
        self.notice = None;
        let name = self.sheet.name.clone();
        match answer {
            Ok(Some(rows)) => match from_rows(&name, &rows) {
                Ok(mut s) => {
                    s.mark_all_stale();
                    self.sheet = s;
                    // The undo stack's inverses were recorded against the
                    // fallback sheet's rows (review finding): once it is
                    // gone, replaying one would either refuse against the
                    // real document or, worse, write a fallback value
                    // over it. Nothing to undo into is the safe state.
                    self.undo.clear();
                }
                Err(e) => self.block_saves(&name, &e),
            },
            Ok(None) => {
                self.notice = Some(format!("sheet '{name}' was not found; opened empty").into())
            }
            Err(e) => self.block_saves(&name, &e),
        }
        if let Some(held) = self.held_expanded.take() {
            self.expansion = Expansion::from_ids(held);
        }
        self.expansion.retain_packages(&self.sheet);
        self.resolve_plan();
        self.rebuild(cx);
        self.submit(cx);
    }

    /// A FAILED load (not an absent document): the fallback must never be
    /// published over the real document (see `save_blocked`).
    fn block_saves(&mut self, name: &str, why: &str) {
        self.save_blocked = true;
        self.save_notice = Some(blocked_notice(name, why));
    }

    /// A reload reached this tile (planning decision 20).
    pub(crate) fn config_changed(&mut self, cx: &mut Context<Self>) {
        self.resolve_plan();
        self.rebuild(cx);
        self.restart_timer(cx);
    }

    // ---- verbs ----------------------------------------------------------

    /// Every normal-mode verb (`pricer::*`). A verb this tile
    /// handles clears the footer first, so a stale refusal never outlives
    /// the next keystroke.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(verb) = action.0.strip_prefix("pricer::") else {
            return false;
        };
        let n = count.unwrap_or(1).max(1) as usize;
        self.footer = None;
        // Any verb but the fields' own closes an open field first (a
        // palette dispatch can arrive while one is open).
        let field_verb = matches!(
            verb,
            "commit" | "cancel" | "insert_up" | "insert_down" | "insert_up_big" | "insert_down_big"
        );
        if !field_verb {
            self.close_entry(window, cx);
            self.close_editor(window, cx);
        }
        // Any verb but the menu's own closes an open menu (a palette
        // dispatch, or a verb picked from the menu itself, can arrive
        // while one is open).
        if self.menu.is_some() && !verb.starts_with("menu") {
            self.menu = None;
        }
        match verb {
            "down" => self.step_rows(n as isize),
            "up" => self.step_rows(-(n as isize)),
            "page_down" => self.step_rows((HALF_PAGE * n) as isize),
            "page_up" => self.step_rows(-((HALF_PAGE * n) as isize)),
            "page_down_full" => self.step_rows((FULL_PAGE * n) as isize),
            "page_up_full" => self.step_rows(-((FULL_PAGE * n) as isize)),
            "top" => self.jump_row(count.map(|c| c as usize).unwrap_or(1)),
            "bottom" => self.jump_row(count.map(|c| c as usize).unwrap_or(usize::MAX)),
            "left" => self.cursor.col = self.cursor.col.saturating_sub(n),
            "right" => {
                let last = self.plan.columns.len().saturating_sub(1);
                self.cursor.col = (self.cursor.col + n).min(last);
            }
            "first_col" => self.cursor.col = 0,
            "last_col" => self.cursor.col = self.plan.columns.len().saturating_sub(1),
            "toggle" => return self.tree_verb(None, cx),
            "expand" => return self.tree_verb(Some(true), cx),
            "collapse" => return self.tree_verb(Some(false), cx),
            "expand_all" | "collapse_all" => {
                if verb == "expand_all" {
                    self.expansion.open_all(&self.sheet);
                } else {
                    // Off a leg, the cursor lands on its package (it is
                    // about to disappear).
                    if let Some(p) = self.cursor_sheet_row().and_then(|r| self.sheet.parent(r)) {
                        self.cursor.line = Some(self.sheet.id(p));
                    }
                    self.expansion.close_all();
                }
                self.rebuild(cx);
                return true;
            }
            "yank_row" => {
                if let Some(row) = self.cursor_sheet_row() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                        self.sheet.shorthand(row),
                    ));
                    self.register = Some(crate::core::clip::spec_of(&self.sheet, row));
                }
            }
            "yank_col" => {
                let col = self.cursor.col;
                let text = self
                    .model
                    .rows
                    .iter()
                    .filter(|r| r.kind != GridRowKind::Entry)
                    .map(|r| {
                        r.cells
                            .get(col)
                            .map(|c| c.text.to_string())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            }
            "find_next" => self.repeat_find(FindDirection::Forward, n),
            "find_prev" => self.repeat_find(FindDirection::Backward, n),
            "escape" => {
                self.find = None;
                // "loading…" is the only sign a load is pending; `loaded`
                // clears it when the rows (or the refusal) arrive.
                if !self.loading {
                    self.notice = None;
                }
            }
            "price" => {
                self.reprice_all(cx);
                return true;
            }
            "add_below" | "add_above" => {
                self.open_entry(verb == "add_below", window, cx);
                return true;
            }
            "edit" => {
                self.begin_edit(window, cx);
                self.rebuild_chrome();
                cx.notify();
                return true;
            }
            "commit" => {
                if self.entry.is_some() {
                    self.commit_entry(window, cx);
                } else {
                    self.commit_edit(window, cx);
                }
                return true;
            }
            "cancel" => {
                self.close_entry(window, cx);
                self.close_editor(window, cx);
                self.rebuild_chrome();
                return true;
            }
            "insert_up" | "insert_down" | "insert_up_big" | "insert_down_big" => {
                let up = verb.starts_with("insert_up");
                if self.entry.is_some() {
                    self.step_history(if up { 1 } else { -1 }, window, cx);
                } else {
                    let magnitude: i64 = if verb.ends_with("_big") { 10 } else { 1 };
                    let steps = (if up { magnitude } else { -magnitude }) * n as i64;
                    self.nudge(steps, window, cx);
                    self.rebuild_chrome();
                    cx.notify();
                }
                return true;
            }
            "delete" | "undo" | "redo" | "put_below" | "put_above" | "move_down" | "move_up"
            | "group" | "ungroup" => {
                if self.loading {
                    self.footer = Some("the sheet is still loading".into());
                } else {
                    let result = match verb {
                        "delete" => self.delete_row(cx),
                        "undo" => self.history_step(false, cx),
                        "redo" => self.history_step(true, cx),
                        "put_below" => self.put(true, cx),
                        "put_above" => self.put(false, cx),
                        "move_down" => self.move_row(n as isize, cx),
                        "move_up" => self.move_row(-(n as isize), cx),
                        "group" => self.group(n, cx),
                        _ => self.ungroup(cx),
                    };
                    if let Err(why) = result {
                        self.footer = Some(why.into());
                    }
                }
            }
            "menu" => {
                self.toggle_menu(cx);
                return true;
            }
            "menu_down" | "menu_up" => {
                if let Some(m) = self.menu.as_mut() {
                    let len = m.items.len() as isize;
                    let step = if verb == "menu_down" {
                        n as isize
                    } else {
                        -(n as isize)
                    };
                    m.highlighted = (m.highlighted as isize + step).clamp(0, len - 1) as usize;
                }
            }
            "menu_pick" => {
                let at = self.menu.as_ref().map(|m| m.highlighted);
                if let Some(at) = at {
                    self.menu_pick(at, window, cx);
                }
                return true;
            }
            "menu_close" => {
                self.close_menu(cx);
                return true;
            }
            _ => return false,
        }
        self.sync_cursor(cx);
        self.rebuild_chrome();
        cx.notify();
        true
    }

    /// The sheet row under the cursor.
    pub(crate) fn cursor_sheet_row(&self) -> Option<usize> {
        self.cursor_row().and_then(|r| self.model.rows[r].row)
    }

    /// `d d` (spec §8.5): no confirm — `u` is one key away. What was
    /// deleted is what `p` puts.
    fn delete_row(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let row = self.cursor_sheet_row().ok_or("no row")?;
        let spec = spec_of(&self.sheet, row);
        self.apply_edit(Edit::Remove { at: row }, cx)
            .map_err(|e| e.to_string())?;
        self.register = Some(spec);
        Ok(())
    }

    /// `u` / `ctrl+r`. A refused inverse clears the whole history
    /// (`UndoStack`'s rule) and says so.
    fn history_step(&mut self, redo: bool, cx: &mut Context<Self>) -> Result<(), String> {
        // A step that reinstates rows (a `Restore`: `d d` undone) puts the
        // cursor on the first of them; otherwise the cursor, keyed by id,
        // stayed on the row that had followed them.
        let restored = self.undo.peek(redo).and_then(|u| {
            u.inverse.iter().find_map(|e| match e {
                Edit::Restore { rows, .. } => rows.first().map(|r| r.id),
                _ => None,
            })
        });
        let stepped = if redo {
            self.undo.redo(&mut self.sheet)
        } else {
            self.undo.undo(&mut self.sheet)
        };
        match stepped {
            Ok(true) => {
                if let Some(id) = restored {
                    // A leg restored under a closed package opens it, or
                    // the cursor would sit on a hidden row.
                    if let Some(p) = self.sheet.index_of(id).and_then(|r| self.sheet.parent(r)) {
                        self.expansion.set(self.sheet.id(p), true);
                    }
                    self.cursor.line = Some(id);
                }
                self.after_edit(cx);
                Ok(())
            }
            Ok(false) => Err(if redo {
                "nothing to redo"
            } else {
                "nothing to undo"
            }
            .into()),
            Err(e) => {
                self.after_edit(cx);
                Err(format!(
                    "{} failed ({e}); history cleared",
                    if redo { "redo" } else { "undo" }
                ))
            }
        }
    }

    /// `p` / `shift+p`: the register as fresh rows (fresh ids, fresh
    /// requests) where `put_place` says (planning decision 12).
    fn put(&mut self, below: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let spec = self.register.clone().ok_or("nothing to put")?;
        let place = put_place(&self.sheet, self.cursor_sheet_row(), below, &spec);
        // A line landing on a leg slot of a collapsed package (`o`'s own
        // rule) must open it first, the way `open_entry` does — otherwise
        // the new leg paints into a hidden row and the cursor falls back
        // to wherever it was (review finding).
        if let Place::Leg { package, .. } = place {
            self.expansion.set(self.sheet.id(package), true);
        }
        self.apply_edit(
            Edit::Insert {
                place,
                rows: vec![spec.clone()],
            },
            cx,
        )
        .map_err(|e| e.to_string())?;
        let id = self.sheet.id(landed_row(place));
        if matches!(spec, RowSpec::Package { .. }) {
            self.expansion.set(id, true);
        }
        self.cursor.line = Some(id);
        self.rebuild(cx);
        Ok(())
    }

    /// `shift+j` / `shift+k`: within the parent; the cursor follows its
    /// line (it is keyed by id).
    fn move_row(&mut self, delta: isize, cx: &mut Context<Self>) -> Result<(), String> {
        let row = self.cursor_sheet_row().ok_or("no row")?;
        self.apply_edit(Edit::Move { row, delta }, cx)
            .map_err(|e| e.to_string())
    }

    /// `g p` (count): the cursor row and the next `count − 1` roots become
    /// a custom package, opened.
    fn group(&mut self, count: usize, cx: &mut Context<Self>) -> Result<(), String> {
        let first = self.cursor_sheet_row().ok_or("no row")?;
        self.apply_edit(
            Edit::Group {
                first,
                count,
                template: Template::Custom,
                id: None,
            },
            cx,
        )
        .map_err(|e| e.to_string())?;
        let id = self.sheet.id(first);
        self.expansion.set(id, true);
        self.cursor.line = Some(id);
        self.rebuild(cx);
        Ok(())
    }

    /// `g u`: the package under the cursor — on a leg, its package.
    fn ungroup(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let row = self.cursor_sheet_row().ok_or("no row")?;
        let package = if self.sheet.is_package(row) {
            row
        } else {
            self.sheet.parent(row).ok_or("not in a package")?
        };
        self.apply_edit(Edit::Ungroup { row: package }, cx)
            .map_err(|e| e.to_string())
    }

    fn set_sheet_shift(
        &mut self,
        field: ShiftField,
        value: Option<f64>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let mut s = self.sheet.sheet_shift();
        match field {
            ShiftField::Spot => s.spot_pct = value,
            ShiftField::Vol => s.vol_pts = value,
        }
        self.apply_edit(Edit::SetSheetShift(s), cx)
            .map_err(|e| e.to_string())
    }

    /// `:spot` (ruling 1): one underlying set or cleared, or every
    /// override cleared as ONE undo entry.
    fn set_spot(
        &mut self,
        underlying: Option<String>,
        level: Option<f64>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let edits: Vec<Edit> = match underlying {
            Some(underlying) => vec![Edit::SetSpotOverride { underlying, level }],
            None => self
                .sheet
                .overrides()
                .spot
                .keys()
                .map(|u| Edit::SetSpotOverride {
                    underlying: u.clone(),
                    level: None,
                })
                .collect(),
        };
        self.apply_edits(edits, cx).map_err(|e| e.to_string())
    }

    /// `:shift`/`:spot`/`:group`/`:ungroup` edit the sheet; a pending load
    /// holds the empty fallback sheet, so an edit landed there would be
    /// lost (silently, with its undo pointing at rows that no longer
    /// exist) the moment `loaded` swaps the real document in.
    fn refuse_while_loading(&self) -> Result<(), String> {
        if self.loading {
            Err("the sheet is still loading".into())
        } else {
            Ok(())
        }
    }

    fn menu_items(&self) -> Vec<MenuItem> {
        let row = self.cursor_sheet_row();
        let root_line =
            row.is_some_and(|r| self.sheet.is_line(r) && self.sheet.parent(r).is_none());
        let packaged =
            row.is_some_and(|r| self.sheet.is_package(r) || self.sheet.parent(r).is_some());
        let mut items = vec![
            MenuItem::Action {
                id: "pricer::price",
                title: "Price all",
                enabled: Ok(()),
            },
            MenuItem::Action {
                id: "pricer::group",
                title: "Group",
                enabled: if root_line {
                    Ok(())
                } else {
                    Err("group needs a top-level line")
                },
            },
            MenuItem::Action {
                id: "pricer::ungroup",
                title: "Ungroup",
                enabled: if packaged {
                    Ok(())
                } else {
                    Err("not in a package")
                },
            },
            MenuItem::Action {
                id: "pricer::undo",
                title: "Undo",
                enabled: if self.undo.can_undo() {
                    Ok(())
                } else {
                    Err("nothing to undo")
                },
            },
            MenuItem::Action {
                id: "pricer::redo",
                title: "Redo",
                enabled: if self.undo.can_redo() {
                    Ok(())
                } else {
                    Err("nothing to redo")
                },
            },
            MenuItem::Action {
                id: "pricer::delete",
                title: "Delete row",
                enabled: if row.is_some() { Ok(()) } else { Err("no row") },
            },
        ];
        for name in self.shared.views.borrow().names() {
            let label = if name == self.sheet.view {
                format!("view: {name} \u{2713}")
            } else {
                format!("view: {name}")
            };
            items.push(MenuItem::View {
                name: name.to_string().into(),
                label: label.into(),
            });
        }
        items
    }

    fn toggle_menu(&mut self, cx: &mut Context<Self>) {
        self.menu = match self.menu {
            Some(_) => None,
            None => Some(Menu {
                items: self.menu_items(),
                highlighted: 0,
            }),
        };
        cx.notify();
    }

    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// A disabled row says why and keeps the menu open; an enabled one
    /// closes it and dispatches through the same door a key would.
    pub(crate) fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.menu.as_ref().and_then(|m| m.items.get(index)).cloned() else {
            return;
        };
        match item {
            MenuItem::Action {
                enabled: Err(why), ..
            } => {
                self.footer = Some(why.into());
                self.rebuild_chrome();
                cx.notify();
            }
            MenuItem::Action { id, .. } => {
                self.menu = None;
                self.dispatch(&ActionId(id.to_string()), None, window, cx);
            }
            MenuItem::View { name, .. } => {
                self.menu = None;
                if let Err(why) = self.set_view(&name, cx) {
                    self.footer = Some(why.into());
                }
                self.rebuild_chrome();
                cx.notify();
            }
        }
    }

    fn step_rows(&mut self, delta: isize) {
        let rows: Vec<usize> = self.cursor_rows().collect();
        if rows.is_empty() {
            return;
        }
        let pos = self
            .cursor_row()
            .and_then(|r| rows.iter().position(|x| *x == r))
            .unwrap_or(0) as isize;
        let to = (pos + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.set_cursor_row(rows[to]);
    }

    /// `g g` / `shift+g`: the first/last row, or the `count`th (1-based).
    fn jump_row(&mut self, nth: usize) {
        let rows: Vec<usize> = self.cursor_rows().collect();
        if let Some(r) = rows.get(nth.saturating_sub(1).min(rows.len().saturating_sub(1))) {
            self.set_cursor_row(*r);
        }
    }

    /// `space`/`z a` (`None`), `z o`, `z c` on the cursor's package — on a
    /// leg, its package; closing from a leg lands the cursor on the
    /// package, the blotter's `z c` rule. A line with no package does
    /// nothing.
    fn tree_verb(&mut self, open: Option<bool>, cx: &mut Context<Self>) -> bool {
        let Some(row) = self.cursor_sheet_row() else {
            return true;
        };
        let package = if self.sheet.is_package(row) {
            row
        } else if let Some(p) = self.sheet.parent(row) {
            p
        } else {
            return true;
        };
        let id = self.sheet.id(package);
        let now_open = match open {
            Some(o) => {
                self.expansion.set(id, o);
                o
            }
            None => self.expansion.toggle(id),
        };
        if !now_open {
            self.cursor.line = Some(id);
        }
        self.rebuild(cx);
        true
    }

    fn row_labels(&self) -> Vec<String> {
        self.model.rows.iter().map(|r| r.tree.to_string()).collect()
    }

    fn repeat_find(&mut self, dir: FindDirection, count: usize) {
        let Some(query) = self.find.as_ref().and_then(|f| f.committed.clone()) else {
            return;
        };
        let labels = self.row_labels();
        if labels.is_empty() {
            return;
        }
        let mut at = self.cursor_row().unwrap_or(0);
        for _ in 0..count {
            let start = match dir {
                FindDirection::Forward => (at + 1) % labels.len(),
                FindDirection::Backward => (at + labels.len() - 1) % labels.len(),
            };
            match find_match(&labels, start, dir, &query) {
                Some(row) => at = row,
                None => return,
            }
        }
        self.set_cursor_row(at);
    }

    pub fn command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // `:` is a shell-owned binding, so `dispatch`'s own "any other
        // verb closes the menu and the fields" guard never sees it (the
        // market-data rule).
        self.close_menu(cx);
        self.close_entry(window, cx);
        self.close_editor(window, cx);
        match commands::parse(line)? {
            Command::View(name) => self.set_view(&name, cx),
            Command::Price => {
                self.reprice_all(cx);
                Ok(())
            }
            // `:view` (in `set_view`) and `:refresh` change the sheet too:
            // a pending load would replace them, so they refuse like the
            // edits below.
            Command::Refresh(r) => {
                self.refuse_while_loading()?;
                self.sheet.refresh = r;
                self.restart_timer(cx);
                self.rebuild_chrome();
                cx.notify();
                self.arm_save(cx);
                Ok(())
            }
            // These four edit the sheet (Shift/Spot through `apply_edit`
            // or `apply_edits`, Group/Ungroup through `apply_edit`):
            // refused while a load is pending, or the edit would land on
            // the empty fallback sheet and be lost when `loaded` swaps it
            // out from under the recorded undo (review finding).
            Command::Shift { field, value } => {
                self.refuse_while_loading()?;
                self.set_sheet_shift(field, value, cx)
            }
            Command::Spot { underlying, level } => {
                self.refuse_while_loading()?;
                self.set_spot(underlying, level, cx)
            }
            Command::Group(count) => {
                self.refuse_while_loading()?;
                self.group(count.unwrap_or(1), cx)
            }
            Command::Ungroup => {
                self.refuse_while_loading()?;
                self.ungroup(cx)
            }
        }
    }

    pub fn completions(&self, line: &str, cursor: usize) -> Vec<String> {
        let views: Vec<String> = self
            .shared
            .views
            .borrow()
            .names()
            .map(str::to_string)
            .collect();
        let mut unds: Vec<String> = (0..self.sheet.len())
            .filter_map(|r| self.sheet.instrument(r).map(|i| i.underlying().to_string()))
            .collect();
        unds.sort();
        unds.dedup();
        commands::completions(line, cursor, &views, &unds)
    }

    fn set_view(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        self.refuse_while_loading()?;
        {
            let views = self.shared.views.borrow();
            if views.get(name).is_none() {
                let have: Vec<&str> = views.names().collect();
                return Err(format!("no view '{name}' (have: {})", have.join(", ")));
            }
        }
        self.sheet.view = name.to_string();
        self.resolve_plan();
        self.rebuild(cx);
        self.arm_save(cx);
        Ok(())
    }

    // ---- the one rebuild path ------------------------------------------

    /// The sheet's view as a column plan, or the first loaded view with a
    /// standing notice (the sheet keeps its own `view`, so a view that
    /// comes back on the next reload is used again).
    pub(crate) fn resolve_plan(&mut self) {
        let views = self.shared.views.borrow();
        let (plan, notice) = match views.get(&self.sheet.view) {
            Some(v) => (ColumnPlan::build(v), None),
            None => match views.names().next().and_then(|n| views.get(n)) {
                Some(v) => (
                    ColumnPlan::build(v),
                    Some(
                        format!(
                            "view '{}' is not defined; showing '{}'",
                            self.sheet.view, v.name
                        )
                        .into(),
                    ),
                ),
                None => (
                    ColumnPlan::default(),
                    Some("no pricer views are defined".into()),
                ),
            },
        };
        drop(views);
        self.plan = plan;
        self.view_notice = notice;
        self.cursor.col = self
            .cursor
            .col
            .min(self.plan.columns.len().saturating_sub(1));
    }

    /// Model, table, chrome, notify — after every change that moves what
    /// the grid shows.
    pub(crate) fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.model = Rc::new(GridModel::build(
            &self.sheet,
            &self.expansion,
            &self.plan,
            self.entry_place(),
            self.clock,
        ));
        self.install_model(cx);
        self.rebuild_chrome();
        cx.notify();
    }

    /// The open entry field's place, if any (spec §8.4).
    pub(crate) fn entry_place(&self) -> Option<Place> {
        self.entry.as_ref().map(|e| e.place)
    }

    /// The only way a model reaches the table (spec §8.2).
    pub(crate) fn install_model(&mut self, cx: &mut Context<Self>) {
        let model = Rc::clone(&self.model);
        self.table.update(cx, |t, cx| {
            t.delegate_mut().model = model;
            t.refresh(cx);
        });
        self.sync_cursor(cx);
        // A rebuild moves grid rows, and a new plan moves columns: the
        // editor follows its line and its column kind, or closes.
        self.follow_editor(cx);
        self.sync_editor(cx);
    }

    /// A rebuild can move the plan (a view reload, `:view`) or the grid
    /// (an edit, a load) under an open editor. It follows its column KIND
    /// to that column's new index; when the kind left the plan or the
    /// line left the grid it closes with `MOVED` — it would otherwise
    /// paint over a different column, or stay focused painting nowhere.
    fn follow_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let (line, _, kind) = editor.target();
        let col = self.plan.columns.iter().position(|c| c.def.kind == kind);
        match col.filter(|_| self.model.grid_row_of(line).is_some()) {
            Some(c) => editor.set_col(c),
            None => self.drop_orphaned_editor(cx),
        }
    }

    /// `close_editor` for a rebuild with no `Window` (a reload, a load, a
    /// delivery): the field leaves the tile at once, and its blur runs at
    /// the end of this effect cycle — when no window is mid-update —
    /// through the window it opened in. The deferred closure holds the
    /// field's last handle, so it is still blurred before it drops, and
    /// only while it holds focus (a newer field is left alone).
    fn drop_orphaned_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        self.footer = Some(MOVED.into());
        let input = editor.input().clone();
        drop(editor);
        if let Some(handle) = self.editor_window {
            App::defer(cx, move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    if input.read(cx).focus_handle(cx).is_focused(window) {
                        window.blur(cx);
                    }
                });
            });
        }
    }

    pub(crate) fn rebuild_chrome(&mut self) {
        let settings: PricerSettings = self.shared.settings.borrow().clone();
        let notice = if self.refusals > 0 {
            Some(REFUSED.into())
        } else {
            self.notice.clone().or_else(|| self.view_notice.clone())
        };
        self.header = header::prepare(HeaderInputs {
            sheet: &self.sheet,
            notice,
            save: self.save_notice.clone(),
            settings: &settings,
            clock: self.clock,
        });
        self.title = format!("pricer · {}", self.sheet.name).into();
        self.footer_text = self.footer.clone().or_else(|| {
            let row = self.cursor_row().and_then(|r| self.model.rows[r].row)?;
            match self.sheet.state(row) {
                crate::core::LineState::Failed(m) => Some(m.clone().into()),
                _ => None,
            }
        });
        // An open menu's rows are prepared state like the header: a load
        // answer, a reload or a delivery can change what they would say
        // without a verb closing the menu, so they are re-checked here
        // rather than trusted from when it opened. The highlight keeps its
        // index, clamped to the new list.
        if self.menu.is_some() {
            let items = self.menu_items();
            if let Some(m) = self.menu.as_mut() {
                m.highlighted = m.highlighted.min(items.len().saturating_sub(1));
                m.items = items;
            }
        }
    }

    // ---- the cursor -------------------------------------------------------

    /// Grid rows a cursor may sit on (never the entry placeholder).
    pub(crate) fn cursor_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.model
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind != GridRowKind::Entry)
            .map(|(i, _)| i)
    }

    pub(crate) fn cursor_row(&self) -> Option<usize> {
        self.cursor.line.and_then(|id| self.model.grid_row_of(id))
    }

    /// Point the cursor at grid row `row` (clamped to a cursor row).
    pub(crate) fn set_cursor_row(&mut self, row: usize) {
        let rows: Vec<usize> = self.cursor_rows().collect();
        let Some(&target) = rows.iter().rev().find(|r| **r <= row).or(rows.first()) else {
            self.cursor.line = None;
            return;
        };
        self.cursor.line = self.model.rows[target].id;
        self.cursor.last_row = target;
    }

    /// A cursor whose line went away falls back to the row at its old
    /// index (planning decision 10).
    /// While a load is pending the model is the empty fallback, so the
    /// record's cursor line is kept as is for `loaded` to resolve.
    fn reconcile_cursor(&mut self) {
        if self.loading {
            return;
        }
        match self.cursor_row() {
            Some(r) => self.cursor.last_row = r,
            None => self.set_cursor_row(self.cursor.last_row),
        }
    }

    /// Mirror the cursor into the table: column before row, so the
    /// component ends in row mode (the market-data order).
    pub(crate) fn sync_cursor(&mut self, cx: &mut Context<Self>) {
        self.reconcile_cursor();
        let row = self.cursor_row();
        let col = self.cursor.col;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().cursor = row.map(|r| (r, col));
            match row {
                Some(r) => {
                    t.set_selected_col(col + 1, cx);
                    t.set_selected_row(r, cx);
                    t.scroll_to_row(r, cx);
                }
                None => t.clear_selection(cx),
            }
        });
    }

    /// The line painted at grid row `row` — `None` on the entry
    /// placeholder. Read BEFORE any field closes: the placeholder is a
    /// grid row, so once `close_entry` rebuilds the model every row below
    /// it names the line one lower.
    fn line_at(&self, row: usize) -> Option<LineId> {
        self.model.rows.get(row).and_then(|r| r.id)
    }

    /// The chevron at grid row `row` (spec §8.2: its click is `space`).
    /// A click is a click: it cancels an open entry or editor first,
    /// never commits it; a click on the placeholder only closes it.
    fn chevron_clicked(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_at(row);
        self.close_entry(window, cx);
        self.close_editor(window, cx);
        if let Some(id) = line {
            self.cursor.line = Some(id);
            self.tree_verb(None, cx);
        }
    }

    fn on_table_event(&mut self, event: &TableEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            TableEvent::SelectCell(row, col) => {
                // A click anywhere cancels an open entry or editor, never
                // commits it (global constraints). `SelectRow`/
                // `SelectColumn` are what `sync_cursor` itself emits when
                // it mirrors the cursor into the table (including from
                // inside `open_entry`'s and `begin_edit`'s own rebuilds),
                // so only a real cell click — `SelectCell` — closes a field.
                let line = self.line_at(*row);
                // A double-click's second press carries its first press's
                // row index. When the first press closed the entry, the
                // rows below the placeholder moved up under the pointer,
                // so that index now names the next line: the first press's
                // line is handed to the NEXT press only, and only for its
                // `DoubleClickedCell` (a lone click there still selects
                // what is painted under it).
                self.pressed = self.click_anchor.take().filter(|(r, _)| r == row);
                if self.entry.is_some() {
                    self.click_anchor = Some((*row, line));
                }
                self.close_entry(window, cx);
                self.close_editor(window, cx);
                if let Some(id) = line {
                    self.cursor.line = Some(id);
                    if let Some(c) = SheetDelegate::plan_col(*col) {
                        self.cursor.col = c;
                    }
                }
                self.sync_cursor(cx);
                self.rebuild_chrome();
                cx.notify();
            }
            // The mouse form of `i` (spec §8.4). The press's own
            // `SelectCell` (emitted first) already cancelled whatever was
            // open; the tree column and the placeholder open nothing.
            TableEvent::DoubleClickedCell(row, col) => {
                // A handed-on line wins, placeholder (`None`) included:
                // the row now painted under the pointer slid up there.
                let line = match self.pressed.take() {
                    Some((_, line)) => line,
                    None => self.line_at(*row),
                };
                self.close_entry(window, cx);
                let Some(id) = line else {
                    return;
                };
                // Before the tree-column check: this press's `SelectCell`
                // moved the cursor to the row that slid up.
                self.cursor.line = Some(id);
                self.sync_cursor(cx);
                let Some(c) = SheetDelegate::plan_col(*col) else {
                    self.rebuild_chrome();
                    cx.notify();
                    return;
                };
                self.cursor.col = c;
                self.sync_cursor(cx);
                self.footer = None;
                self.begin_edit(window, cx);
                self.rebuild_chrome();
                cx.notify();
            }
            // `SelectRow`/`SelectColumn` are what `sync_cursor` itself
            // emits: deliberately unmatched.
            _ => {}
        }
    }
}

impl gpui::Render for PricerTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Staleness is a compare per frame, never a format (spec §9.4).
        let stale_after = self.shared.settings.borrow().stale_after;
        let stale = self.header.last_priced.is_some_and(|at| {
            chrono::Utc::now()
                .signed_duration_since(at)
                .to_std()
                .unwrap_or_default()
                > stale_after
        });
        let theme = cx.theme();
        let tile = cx.entity();
        let header = header::render(&self.header, stale, theme, self.stack.as_ref(), self.id);
        // The menu is anchored off a zero-size, absolutely positioned
        // sibling at the header's own right edge (the market-data
        // arrangement) — `relative` on the wrapper is what makes that
        // positioning read against the header rather than the window.
        let header =
            div()
                .relative()
                .w_full()
                .child(header)
                .when_some(self.menu.as_ref(), |el, m| {
                    el.child(
                        div()
                            .absolute()
                            .right_0()
                            .top(scale::design(header::HEADER_HEIGHT))
                            .child(render_menu(m, &tile, cx)),
                    )
                });
        let body = div().flex_1().min_h_0().w_full().child(
            DataTable::new(&self.table)
                .with_size(Size::XSmall)
                .bordered(false)
                .stripe(false),
        );
        let footer = header::render_footer(self.footer_text.as_ref(), theme);
        v_flex()
            .size_full()
            .debug_selector(|| format!("tile-content-{}", self.id.0))
            .child(header)
            .child(body)
            .child(footer)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::content::{PricerFactory, PricerSettings};
    use crate::core::{Edit, Place, RowSpec, Sheet, Views, parse, to_rows};
    use crate::store::{MemorySheetStore, SheetStore as _};
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::pricing::{PriceOutcome, PriceParams, PriceResult};
    use geode_core::query::QueryKey;
    use geode_core::scopes::SavedScopes;
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::ActionId;
    use geode_shell::diagnostics::Diagnostics;
    use geode_shell::frame::Frame;
    use geode_shell::module::{Delivery, ModuleFactory, TileContent};
    use geode_shell::tiling::TileId;
    use gpui::{Entity, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc::Receiver;

    pub(crate) const TILE: u64 = 5;

    struct Built {
        content: Box<dyn TileContent>,
        tile: Entity<PricerTile>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
    }

    // Not every field and method has a reader in every build.
    #[allow(dead_code)]
    pub(crate) struct Harness {
        pub tile: Entity<PricerTile>,
        pub content: Box<dyn TileContent>,
        pub frame: Entity<Frame>,
        pub diagnostics: Entity<Diagnostics>,
        pub factory: Rc<PricerFactory>,
        pub store: MemorySheetStore,
        pub data: DataHandle,
        rx: RefCell<Option<Receiver<Request>>>,
    }

    /// A sheet named `book` in a fresh store, built from shorthand lines,
    /// and the session record that restores it.
    pub(crate) fn seeded(lines: &[&str]) -> (MemorySheetStore, toml::Table) {
        let mut s = Sheet::new("book");
        let rows: Vec<RowSpec> = lines.iter().map(|l| parse(l).unwrap()).collect();
        s.apply(Edit::Insert {
            place: Place::Root { at: 0 },
            rows,
        })
        .unwrap();
        let store = MemorySheetStore::default();
        assert!(store.save("book", to_rows(&s).unwrap()));
        let mut t = toml::Table::new();
        t.insert("sheet".into(), "book".into());
        (store, t)
    }

    pub(crate) fn open(cx: &mut gpui::TestAppContext) -> (Harness, VisualTestContext) {
        open_full(
            cx,
            None,
            MemorySheetStore::default(),
            PricerSettings::default(),
        )
    }

    pub(crate) fn open_seeded(
        cx: &mut gpui::TestAppContext,
        lines: &[&str],
    ) -> (Harness, VisualTestContext) {
        let (store, record) = seeded(lines);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        (h, vcx)
    }

    pub(crate) fn open_full(
        cx: &mut gpui::TestAppContext,
        restored: Option<toml::Table>,
        store: MemorySheetStore,
        settings: PricerSettings,
    ) -> (Harness, VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(geode_shell::shell::dialog::init_reclaimed_keybindings);
        cx.update(crate::init);
        let (data, rx) = DataHandle::for_tests();
        let factory = Rc::new(PricerFactory::new(
            data.clone(),
            Rc::new(store.clone()),
            Views::builtin(),
            settings,
        ));
        let slot: Rc<RefCell<Option<Built>>> = Rc::new(RefCell::new(None));
        let window = cx
            .update(|cx| {
                let slot = slot.clone();
                let factory = factory.clone();
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let occupant = factory.create(
                        TileId(TILE),
                        restored.as_ref(),
                        frame.clone(),
                        diagnostics.clone(),
                        window,
                        cx,
                    );
                    let tile = occupant.view.clone().downcast::<PricerTile>().unwrap();
                    *slot.borrow_mut() = Some(Built {
                        content: occupant.content,
                        tile: tile.clone(),
                        frame,
                        diagnostics,
                    });
                    // `Root` is load-bearing: gpui-component registers the
                    // focused `InputState` on it (the entry field and editor).
                    cx.new(|cx| gpui_component::Root::new(tile, window, cx))
                })
            })
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let built = slot.borrow_mut().take().expect("the factory built one");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            Harness {
                tile: built.tile,
                content: built.content,
                frame: built.frame,
                diagnostics: built.diagnostics,
                factory,
                store,
                data,
                rx: RefCell::new(Some(rx)),
            },
            vcx,
        )
    }

    #[allow(dead_code)]
    impl Harness {
        pub fn command(&self, vcx: &mut VisualTestContext, line: &str) -> Result<(), String> {
            vcx.update(|window, cx| self.content.command(line, window, cx))
        }
        pub fn dispatch(
            &self,
            vcx: &mut VisualTestContext,
            verb: &str,
            count: Option<u32>,
        ) -> bool {
            let id = ActionId(format!("pricer::{verb}"));
            vcx.update(|window, cx| self.content.dispatch(&id, count, window, cx))
        }
        pub fn visible(&self, vcx: &mut VisualTestContext, visible: bool) {
            vcx.update(|_, cx| self.content.set_visible(visible, cx));
        }
        pub fn close_channel(&self) {
            self.rx.borrow_mut().take();
        }
        /// Fill the bounded request queue so the next submission is
        /// refused; `requests()` drains it and admits the next one.
        pub fn fill_queue(&self) {
            while self.data.cancel(QueryKey(u64::MAX)) {}
        }
        /// Every request since the last drain, `Cancel` included.
        pub fn requests(&self) -> Vec<Request> {
            match self.rx.borrow().as_ref() {
                Some(rx) => rx.try_iter().collect(),
                None => Vec::new(),
            }
        }
        /// The price batches since the last drain, in order.
        pub fn prices(&self) -> Vec<PriceParams> {
            self.requests()
                .into_iter()
                .filter_map(|r| match r {
                    Request::Price(p) => Some(p),
                    _ => None,
                })
                .collect()
        }
        pub fn deliver(&self, vcx: &mut VisualTestContext, outcome: PriceOutcome) {
            vcx.update(|window, cx| self.content.deliver(Delivery::Price(outcome), window, cx));
        }
        /// Answer `params` in full: every line `Ok(result(price))`.
        pub fn answer(&self, vcx: &mut VisualTestContext, params: &PriceParams, price: f64) {
            self.deliver(
                vcx,
                PriceOutcome {
                    key: params.key,
                    tag: params.tag,
                    submitted: std::time::Instant::now(),
                    results: params
                        .lines
                        .iter()
                        .map(|l| (l.id, l.revision, Ok(result(price))))
                        .collect(),
                },
            );
        }
        pub fn title(&self, vcx: &mut VisualTestContext) -> String {
            vcx.update(|_, cx| self.content.title(cx).to_string())
        }
        pub fn mode(&self, vcx: &mut VisualTestContext) -> String {
            vcx.update(|_, cx| {
                self.content
                    .key_context(cx)
                    .get("mode")
                    .unwrap_or("")
                    .to_string()
            })
        }
        pub fn serialize(&self, vcx: &mut VisualTestContext) -> toml::Table {
            vcx.update(|_, cx| self.content.serialize(cx))
        }
        /// Column 0's text per grid row — the tree the table paints.
        pub fn tree(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| {
                t.model.rows.iter().map(|r| r.tree.to_string()).collect()
            })
        }
        pub fn columns(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| {
                t.model
                    .columns
                    .iter()
                    .map(|c| c.label.to_string())
                    .collect()
            })
        }
        /// One cell's painted text, by grid row and column label.
        pub fn cell(&self, vcx: &VisualTestContext, row: usize, column: &str) -> String {
            self.tile.read_with(vcx, |t, _| {
                let c = t
                    .model
                    .columns
                    .iter()
                    .position(|c| c.label.as_ref() == column)
                    .expect("column");
                t.model.rows[row].cells[c].text.to_string()
            })
        }
        pub fn header(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| t.header.texts())
        }
        pub fn notice(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile
                .read_with(vcx, |t, _| t.header.notice.as_ref().map(|n| n.to_string()))
        }
        /// The save state's own header slot.
        pub fn save_notice(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile
                .read_with(vcx, |t, _| t.header.save.as_ref().map(|n| n.to_string()))
        }
        pub fn footer(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile
                .read_with(vcx, |t, _| t.footer_text.as_ref().map(|n| n.to_string()))
        }
        /// `(grid row, plan column)` of the cursor.
        pub fn cursor(&self, vcx: &VisualTestContext) -> Option<(usize, usize)> {
            self.tile
                .read_with(vcx, |t, _| t.cursor_row().map(|r| (r, t.cursor.col)))
        }
        pub fn sheet_len(&self, vcx: &VisualTestContext) -> usize {
            self.tile.read_with(vcx, |t, _| t.sheet.len())
        }
        pub fn draw(&self, vcx: &mut VisualTestContext) {
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
    }

    #[allow(dead_code)]
    pub(crate) fn result(price: f64) -> PriceResult {
        PriceResult {
            price,
            delta: 0.5,
            gamma: 0.01,
            vega: 1.0,
            theta: -0.5,
            rho: 0.1,
        }
    }

    // ---- the factory, restore and session ----

    #[gpui::test]
    fn the_factory_is_kind_pricer_and_a_fresh_tile_opens_untitled_1(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert_eq!(h.factory.kind(), "pricer");
        assert_eq!(h.factory.contexts(), vec!["pricer"]);
        assert_eq!(h.title(&mut vcx), "pricer · untitled-1");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.columns(&vcx)[0], "qty", "the vanilla view");
        // A second tile skips the name the first holds.
        let second = vcx.update(|window, cx| {
            h.factory.create(
                TileId(TILE + 1),
                None,
                h.frame.clone(),
                h.diagnostics.clone(),
                window,
                cx,
            )
        });
        assert_eq!(
            vcx.update(|_, cx| second.content.title(cx).to_string()),
            "pricer · untitled-2"
        );
    }

    #[gpui::test]
    fn a_restored_sheet_loads_its_rows_cursor_and_expansion(cx: &mut gpui::TestAppContext) {
        let (store, mut record) = seeded(&[
            "SPX Z26 5000 C",
            "-5 SPX Z26 4800/5200 CS",
            "SPX Z26 4000 P",
        ]);
        // Ids are 1 (A), 2 (the package), 3–4 (legs), 5 (B).
        record.insert("cursor".into(), toml::Value::Integer(5));
        record.insert(
            "expanded".into(),
            toml::Value::Array(vec![toml::Value::Integer(2)]),
        );
        record.insert("view".into(), "barrier".into());
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        assert_eq!(h.title(&mut vcx), "pricer · book");
        assert_eq!(
            h.tree(&vcx).len(),
            5,
            "the package is open, so its legs show"
        );
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(4),
            "the cursor is on line 5"
        );
        assert!(
            !h.columns(&vcx).contains(&"barrier".to_string()),
            "the DOCUMENT's view (vanilla) wins over the record's (barrier)"
        );
        assert!(h.notice(&vcx).is_none());
    }

    #[gpui::test]
    fn a_restored_name_with_no_document_opens_empty_under_that_name(cx: &mut gpui::TestAppContext) {
        let mut record = toml::Table::new();
        record.insert("sheet".into(), "gone".into());
        record.insert("view".into(), "barrier".into());
        let (h, mut vcx) = open_full(
            cx,
            Some(record),
            MemorySheetStore::default(),
            PricerSettings::default(),
        );
        assert_eq!(h.title(&mut vcx), "pricer · gone");
        assert_eq!(
            h.notice(&vcx).as_deref(),
            Some("sheet 'gone' was not found; opened empty")
        );
        assert!(
            h.columns(&vcx).contains(&"barrier".to_string()),
            "the record's view, with no document to say otherwise"
        );
    }

    #[gpui::test]
    fn a_pending_load_paints_loading_until_the_rows_arrive(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&["SPX Z26 5000 C"]);
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        assert_eq!(h.notice(&vcx).as_deref(), Some("loading…"));
        assert_eq!(h.sheet_len(&vcx), 0);
        // Part 4's `Delivery::Query` arm calls exactly this.
        h.tile
            .update(&mut vcx, |t, cx| t.loaded(Ok(Some(rows)), cx));
        assert_eq!(h.sheet_len(&vcx), 1);
        assert!(h.notice(&vcx).is_none());
    }

    /// The pending path is Part 4's production restore (planning decision
    /// 7): the record's cursor and expansion wait for the rows, and a
    /// session save while loading writes them back unchanged.
    #[gpui::test]
    fn a_pending_load_keeps_the_records_cursor_and_expansion(cx: &mut gpui::TestAppContext) {
        let (store, mut record) = seeded(&[
            "SPX Z26 5000 C",
            "-5 SPX Z26 4800/5200 CS",
            "SPX Z26 4000 P",
        ]);
        // Ids are 1 (A), 2 (the package), 3–4 (legs), 5 (B).
        record.insert("cursor".into(), toml::Value::Integer(5));
        record.insert(
            "expanded".into(),
            toml::Value::Array(vec![toml::Value::Integer(2)]),
        );
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        let saved = crate::session::Record::from_table(&h.serialize(&mut vcx));
        assert_eq!(saved.cursor, Some(crate::core::LineId(5)));
        assert_eq!(saved.expanded, vec![crate::core::LineId(2)]);
        h.tile
            .update(&mut vcx, |t, cx| t.loaded(Ok(Some(rows)), cx));
        assert_eq!(
            h.tree(&vcx).len(),
            5,
            "the package is open, so its legs show"
        );
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(4),
            "the cursor is on line 5"
        );
        let saved = crate::session::Record::from_table(&h.serialize(&mut vcx));
        assert_eq!(saved.cursor, Some(crate::core::LineId(5)));
        assert_eq!(saved.expanded, vec![crate::core::LineId(2)]);
    }

    #[gpui::test]
    fn the_session_record_round_trips_the_tile(cx: &mut gpui::TestAppContext) {
        let (store, mut record) = seeded(&["SPX Z26 5000 C", "SPX Z26 4000 P"]);
        record.insert("cursor".into(), toml::Value::Integer(2));
        let (h, mut vcx) = open_full(cx, Some(record), store.clone(), PricerSettings::default());
        h.command(&mut vcx, "view barrier").unwrap();
        let saved = h.serialize(&mut vcx);
        let r = crate::session::Record::from_table(&saved);
        assert_eq!(r.sheet.as_deref(), Some("book"));
        assert_eq!(r.view.as_deref(), Some("barrier"));
        assert_eq!(r.cursor, Some(crate::core::LineId(2)));
    }

    #[gpui::test]
    fn colon_view_switches_the_columns_and_an_unknown_view_is_refused(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C"]);
        assert!(!h.columns(&vcx).contains(&"barrier".to_string()));
        h.command(&mut vcx, "view barrier").unwrap();
        assert!(h.columns(&vcx).contains(&"barrier".to_string()));
        assert_eq!(
            h.command(&mut vcx, "view nope"),
            Err("no view 'nope' (have: vanilla, barrier)".into())
        );
        let words = h.tile.read_with(&vcx, |t, _| t.completions("view ", 5));
        assert_eq!(words, vec!["vanilla", "barrier"]);
    }

    #[gpui::test]
    fn a_reloaded_views_doc_reaches_an_open_tile(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C"]);
        let doc = geode_core::config::merge_docs(
            "pricer_views",
            &[geode_core::config::LayerDoc::builtin(
                "pricer_views",
                "[slim]\ncolumns = [\"qty\", \"price\"]\n",
            )
            .unwrap()],
        );
        let (views, diags) = Views::from_doc(&doc);
        assert!(diags.is_empty());
        vcx.update(|_, cx| {
            h.factory
                .reload(views, None, std::time::Duration::from_secs(60), cx)
        });
        assert_eq!(
            h.columns(&vcx),
            vec!["qty", "price"],
            "the sheet's `vanilla` is gone, so the first view shows"
        );
        assert_eq!(
            h.notice(&vcx).as_deref(),
            Some("view 'vanilla' is not defined; showing 'slim'")
        );
        assert_eq!(h.factory.settings().refresh, None);
    }

    /// Planning decision 6: a visible pricer submits no view query, so it
    /// must arrive at a flip barrier itself or every other following tile
    /// waits out the deadline (`geode-diagnostics`' own test, copied).
    #[gpui::test]
    fn the_tile_answers_a_flip_barrier_it_has_nothing_coming_for(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.frame.update(&mut vcx, |f, cx| {
            f.set_scope(geode_core::scope::Scope {
                text: Some("spx".into()),
                ..Default::default()
            });
            f.open_flip([QueryKey(TILE)], std::time::Instant::now());
            cx.notify();
        });
        assert!(!h.frame.read_with(&vcx, |f, _| f.barrier_open()));
    }

    #[gpui::test]
    fn a_closed_tile_gives_its_sheet_name_back(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let (factory, frame, diagnostics) =
            (h.factory.clone(), h.frame.clone(), h.diagnostics.clone());
        drop(h);
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        drop(vcx);
        // A tile in a fresh window takes `untitled-1` again.
        let title = cx.update(|cx| {
            let mut title = String::new();
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let o = factory.create(
                    TileId(TILE + 2),
                    None,
                    frame.clone(),
                    diagnostics.clone(),
                    window,
                    cx,
                );
                title = o.content.title(cx).to_string();
                cx.new(|cx| gpui_component::Root::new(o.view, window, cx))
            })
            .unwrap();
            title
        });
        assert_eq!(title, "pricer · untitled-1");
    }

    pub(crate) fn centre_of(
        vcx: &mut VisualTestContext,
        selector: &str,
    ) -> gpui::Point<gpui::Pixels> {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // `debug_bounds` wants a `&'static str`: leaked, a test-only cost.
        let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
        vcx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"))
            .center()
    }

    /// A left mouse-down/up pair carrying `click_count` (gpui's own
    /// `simulate_click` hardwires 1).
    pub(crate) fn click_at(
        vcx: &mut VisualTestContext,
        at: gpui::Point<gpui::Pixels>,
        click_count: usize,
    ) {
        vcx.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count,
        });
    }

    /// [A, P(L1, L2), B] with P closed: grid rows A=0, P=1, B=2.
    const BOOK: [&str; 3] = [
        "SPX Z26 5000 C",
        "-5 SPX Z26 4800/5200 CS",
        "SPX Z26 4000 P",
    ];

    // ---- motions, tree verbs, yank and find ----

    #[gpui::test]
    fn motions_move_the_cursor_and_never_into_the_tree_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert_eq!(
            h.cursor(&vcx),
            Some((0, 0)),
            "a restore with no cursor lands on the first row"
        );
        h.dispatch(&mut vcx, "down", Some(2));
        assert_eq!(h.cursor(&vcx), Some((2, 0)));
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.cursor(&vcx), Some((2, 0)), "clamped at the last row");
        h.dispatch(&mut vcx, "top", None);
        assert_eq!(h.cursor(&vcx), Some((0, 0)));
        h.dispatch(&mut vcx, "bottom", None);
        assert_eq!(h.cursor(&vcx), Some((2, 0)));
        h.dispatch(&mut vcx, "left", None);
        assert_eq!(
            h.cursor(&vcx),
            Some((2, 0)),
            "column 0 is the first plan column; the tree is not a target"
        );
        h.dispatch(&mut vcx, "right", Some(3));
        assert_eq!(h.cursor(&vcx), Some((2, 3)));
        h.dispatch(&mut vcx, "last_col", None);
        let last = h.columns(&vcx).len() - 1;
        assert_eq!(h.cursor(&vcx), Some((2, last)));
        h.dispatch(&mut vcx, "first_col", None);
        assert_eq!(h.cursor(&vcx), Some((2, 0)));
        h.dispatch(&mut vcx, "page_up", None);
        assert_eq!(
            h.cursor(&vcx),
            Some((0, 0)),
            "half a page (5) clamps at the top"
        );
    }

    #[gpui::test]
    fn tree_verbs_open_and_close_packages_and_a_leg_collapses_to_its_package(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "toggle", None);
        assert_eq!(
            h.tree(&vcx).len(),
            5,
            "space opens the package under the cursor"
        );
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2), "on the first leg");
        h.dispatch(&mut vcx, "collapse", None);
        assert_eq!(h.tree(&vcx).len(), 3);
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(1),
            "z c on a leg closes its package and lands on it"
        );
        h.dispatch(&mut vcx, "expand", None);
        assert_eq!(h.tree(&vcx).len(), 5);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "collapse_all", None);
        assert_eq!(h.tree(&vcx).len(), 3);
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(1),
            "z M off a leg lands on its package"
        );
        h.dispatch(&mut vcx, "expand_all", None);
        assert_eq!(h.tree(&vcx).len(), 5);
        let r = crate::session::Record::from_table(&h.serialize(&mut vcx));
        assert_eq!(
            r.expanded,
            vec![crate::core::LineId(2)],
            "the session carries the open package"
        );
    }

    #[gpui::test]
    fn a_chevron_click_toggles_once_even_on_a_double_click(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let at = centre_of(&mut vcx, "pricer-chevron-1");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.tree(&vcx).len(), 5);
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(1),
            "the chevron click also moves the tile cursor onto the package"
        );
        let at = centre_of(&mut vcx, "pricer-chevron-1");
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(
            h.tree(&vcx).len(),
            3,
            "the second press of a double-click is ignored"
        );
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(1),
            "closing from the chevron lands the cursor back on the package"
        );
    }

    #[gpui::test]
    fn a_cell_click_moves_the_cursor_to_that_cell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let at = centre_of(&mut vcx, "pricer-cell-2-3");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(
            h.cursor(&vcx),
            Some((2, 2)),
            "table column 3 is plan column 2"
        );
    }

    #[gpui::test]
    fn yy_yanks_the_rows_shorthand_and_yc_the_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "yank_row", None);
        let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(clip.as_deref(), Some("-5 SPX Z26 4800/5200 CS"));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.register.is_some()),
            "p puts what y y yanked"
        );
        h.dispatch(&mut vcx, "right", Some(3)); // strike
        h.dispatch(&mut vcx, "yank_col", None);
        let clip = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(
            clip.as_deref(),
            Some("5000\n\n4000"),
            "the package's strike is blank"
        );
    }

    #[gpui::test]
    fn find_jumps_from_its_origin_repeats_with_n_and_escape_returns(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let find = |vcx: &mut VisualTestContext, e: FindEvent| {
            vcx.update(|window, cx| h.content.find(e, window, cx));
        };
        find(&mut vcx, FindEvent::Changed("4000".into()));
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2));
        find(&mut vcx, FindEvent::Committed("spx".into()));
        h.dispatch(&mut vcx, "find_next", None);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(0), "wraps");
        h.dispatch(&mut vcx, "find_prev", None);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2));
        find(&mut vcx, FindEvent::Changed("cs".into()));
        find(&mut vcx, FindEvent::Cancelled);
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(0),
            "escape returns to where `/` opened"
        );
    }

    // ---- repricing ----

    fn edit(h: &Harness, vcx: &mut VisualTestContext, e: Edit) {
        h.tile.update(vcx, |t, cx| t.apply_edit(e, cx)).unwrap();
    }

    fn new_strike(h: &Harness, vcx: &VisualTestContext, row: usize, strike: f64) -> Edit {
        let i = h
            .tile
            .read_with(vcx, |t, _| t.sheet.instrument(row).unwrap().clone());
        let geode_core::pricing::Instrument::Vanilla(mut v) = i else {
            panic!("vanilla")
        };
        v.strike = geode_core::pricing::Strike::Absolute(strike);
        Edit::SetInstrument {
            row,
            instrument: geode_core::pricing::Instrument::Vanilla(v),
        }
    }

    #[gpui::test]
    fn a_shown_tile_prices_every_stale_line_in_one_batch_and_the_answer_paints(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let batches = h.prices();
        assert_eq!(batches.len(), 1, "one PriceParams for the frame");
        let b = &batches[0];
        assert_eq!(b.key, QueryKey(TILE));
        assert_eq!(
            b.lines.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![1, 3, 4, 5],
            "lines only, never the package"
        );
        assert_eq!(
            h.header(&vcx)
                .iter()
                .filter(|t| t.ends_with("pricing…"))
                .count(),
            1
        );
        h.answer(&mut vcx, b, 12.5);
        assert_eq!(h.cell(&vcx, 0, "price"), "12.50");
        assert_eq!(
            h.cell(&vcx, 1, "price"),
            "0.00",
            "−5 × 12.5 + 5 × 12.5: the package sums signed legs"
        );
        assert!(!h.header(&vcx).iter().any(|t| t.ends_with("pricing…")));
        assert!(
            h.prices().is_empty(),
            "nothing left stale, nothing resubmitted"
        );
    }

    #[gpui::test]
    fn a_request_changing_edit_resubmits_and_a_qty_edit_does_not(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        assert!(h.prices().is_empty(), "qty changes no request (spec §9.3)");
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        let again = h.prices();
        assert_eq!(again.len(), 1);
        assert_eq!(
            again[0].lines.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![1],
            "only the edited line is stale"
        );
        assert!(again[0].tag > b.tag);
    }

    /// Planning decision 4: a newer batch carries every stale line, so
    /// dropping the older batch's outcome whole loses nothing.
    #[gpui::test]
    fn an_older_submissions_outcome_is_dropped_whole(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let first = h.prices().remove(0);
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        let second = h.prices().remove(0);
        assert_eq!(
            second.lines.len(),
            4,
            "the new batch carries the old one's lines too"
        );
        h.answer(&mut vcx, &first, 99.0);
        assert_eq!(
            h.cell(&vcx, 2, "price"),
            "",
            "the older tag installs nothing"
        );
        h.answer(&mut vcx, &second, 12.5);
        assert_eq!(h.cell(&vcx, 2, "price"), "12.50");
    }

    #[gpui::test]
    fn an_answer_for_an_old_revision_leaves_the_line_stale_and_resubmits_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let first = h.prices().remove(0);
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        let second = h.prices().remove(0);
        // The CURRENT tag, but line 1 answered at the revision before the
        // edit (an edit landed during the round trip, spec §9.2).
        h.deliver(
            &mut vcx,
            PriceOutcome {
                key: second.key,
                tag: second.tag,
                submitted: std::time::Instant::now(),
                results: first
                    .lines
                    .iter()
                    .map(|l| (l.id, l.revision, Ok(result(12.5))))
                    .collect(),
            },
        );
        assert_eq!(
            h.cell(&vcx, 0, "price"),
            "",
            "line 1's answer is for an older request"
        );
        assert_eq!(h.cell(&vcx, 2, "price"), "12.50", "the rest install");
        let again = h.prices();
        assert_eq!(again.len(), 1);
        assert_eq!(
            again[0].lines.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![1],
            "only line 1 goes again"
        );
    }

    #[gpui::test]
    fn a_delivery_for_another_key_is_ignored(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        let mut other = b.clone();
        other.key = QueryKey(99);
        h.answer(&mut vcx, &other, 12.5);
        assert_eq!(h.cell(&vcx, 0, "price"), "");
    }

    #[gpui::test]
    fn a_failed_line_paints_a_dash_names_itself_in_the_footer_and_fails_its_package(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.deliver(
            &mut vcx,
            PriceOutcome {
                key: b.key,
                tag: b.tag,
                submitted: std::time::Instant::now(),
                results: b
                    .lines
                    .iter()
                    .map(|l| {
                        (
                            l.id,
                            l.revision,
                            if l.id == 3 {
                                Err("refused by the mock".into())
                            } else {
                                Ok(result(1.0))
                            },
                        )
                    })
                    .collect(),
            },
        );
        assert_eq!(
            h.cell(&vcx, 1, "price"),
            "—",
            "a failed leg fails its package"
        );
        h.dispatch(&mut vcx, "down", None);
        assert!(
            h.footer(&vcx).unwrap().ends_with("refused by the mock"),
            "the cursor row's failure in the footer"
        );
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.footer(&vcx), None);
    }

    #[gpui::test]
    fn hide_cancels_by_key_and_prices_nothing_until_shown(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let _ = h.prices();
        h.visible(&mut vcx, false);
        assert!(
            h.requests()
                .iter()
                .any(|r| matches!(r, Request::Cancel { key } if *key == QueryKey(TILE)))
        );
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        assert!(
            h.prices().is_empty(),
            "a hidden tile keeps its stale marks and submits nothing"
        );
        h.visible(&mut vcx, true);
        assert_eq!(h.prices().len(), 1, "and resubmits on show");
    }

    #[gpui::test]
    fn a_refused_submission_notices_and_retries_after_a_second(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&BOOK);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.close_channel();
        h.visible(&mut vcx, true);
        assert_eq!(h.notice(&vcx).as_deref(), Some(REFUSED));
        let tag = h.tile.read_with(&vcx, |t, _| t.tag);
        vcx.executor().advance_clock(RETRY_AFTER);
        vcx.run_until_parked();
        assert!(
            h.tile.read_with(&vcx, |t, _| t.tag) > tag,
            "the retry fired and asked again"
        );
    }

    #[gpui::test]
    fn the_refresh_tick_marks_every_line_stale_and_submits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        vcx.executor()
            .advance_clock(std::time::Duration::from_secs(30));
        vcx.run_until_parked();
        let tick = h.prices();
        assert_eq!(tick.len(), 1);
        assert_eq!(tick[0].lines.len(), 4, "every line, at unchanged revisions");
        assert_eq!(
            tick[0].lines.iter().map(|l| l.revision).collect::<Vec<_>>(),
            b.lines.iter().map(|l| l.revision).collect::<Vec<_>>()
        );
    }

    #[gpui::test]
    fn colon_refresh_sets_this_sheets_interval_and_off_stops_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        h.command(&mut vcx, "refresh off").unwrap();
        vcx.executor()
            .advance_clock(std::time::Duration::from_secs(120));
        vcx.run_until_parked();
        assert!(h.prices().is_empty());
        h.command(&mut vcx, "refresh 5s").unwrap();
        vcx.executor()
            .advance_clock(std::time::Duration::from_secs(5));
        vcx.run_until_parked();
        assert_eq!(h.prices().len(), 1);
        let r = crate::session::Record::from_table(&h.serialize(&mut vcx));
        assert_eq!(
            r.refresh,
            Some(crate::core::Refresh::Every(std::time::Duration::from_secs(
                5
            )))
        );
    }

    #[gpui::test]
    fn colon_price_reprices_everything_now(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        h.command(&mut vcx, "price").unwrap();
        assert_eq!(h.prices()[0].lines.len(), 4);
        assert!(
            h.dispatch(&mut vcx, "price", None),
            "the palette's action is the same verb"
        );
    }

    #[gpui::test]
    fn a_missing_pricer_names_itself_in_the_header(cx: &mut gpui::TestAppContext) {
        let settings = PricerSettings {
            pricer: "vendor".into(),
            pricer_missing: true,
            ..PricerSettings::default()
        };
        let (h, vcx) = open_full(cx, None, MemorySheetStore::default(), settings);
        assert_eq!(
            h.notice(&vcx).as_deref(),
            Some("pricer \"vendor\" is not built into this binary")
        );
    }

    #[gpui::test]
    fn the_header_time_reads_the_app_clock(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        vcx.update(|_, cx| {
            cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::utc()))
        });
        let utc = h.tile.read_with(&vcx, |t, _| t.header.time.clone());
        vcx.update(|_, cx| {
            cx.set_global(geode_shell::clock::AppClock(
                geode_core::clock::Clock::in_zone_named("Asia/Tokyo"),
            ))
        });
        let tokyo = h.tile.read_with(&vcx, |t, _| t.header.time.clone());
        assert_ne!(utc, tokyo, "a zone change re-prepares the header");
    }

    // ---- the entry field ----

    fn typed(h: &Harness, vcx: &mut VisualTestContext, text: &str) {
        vcx.simulate_input(text);
        h.draw(vcx);
    }

    fn focused(vcx: &mut VisualTestContext) -> bool {
        vcx.update(|window, cx| window.focused(cx).is_some())
    }

    /// Spec §12: `o`, a line, `enter` adds a row and submits one request.
    #[gpui::test]
    fn o_then_a_line_then_enter_adds_a_row_submits_it_and_opens_the_next_placeholder(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "add_below", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(focused(&mut vcx), "the field owns focus");
        typed(&h, &mut vcx, "-5 SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 1);
        let batches = h.prices();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].lines.len(), 1);
        assert_eq!(
            h.mode(&mut vcx),
            "insert",
            "a fresh placeholder opens below"
        );
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model.entry_row()), Some(1));
        typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 4, "a package with its two legs");
        assert_eq!(
            h.tree(&vcx).len(),
            5,
            "the typed package opens so its legs show, plus the placeholder"
        );
    }

    #[gpui::test]
    fn a_parse_error_keeps_the_text_and_names_the_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 5000 CX");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 0);
        assert_eq!(h.mode(&mut vcx), "insert");
        let footer = h.footer(&vcx).unwrap();
        assert!(footer.ends_with("(column 14)"), "{footer}");
        let text = h.tile.read_with(&vcx, |t, cx| {
            t.entry.as_ref().unwrap().input.read(cx).value().to_string()
        });
        assert_eq!(text, "SPX Z26 5000 CX", "the text is kept for fixing");
    }

    #[gpui::test]
    fn shift_o_on_a_leg_inserts_a_leg_before_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "down", Some(2)); // the second leg
        h.dispatch(&mut vcx, "add_above", None);
        typed(&h, &mut vcx, "SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        let legs = h.tile.read_with(&vcx, |t, _| t.sheet.children(1).len());
        assert_eq!(legs, 3);
        let middle = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(3));
        assert_eq!(middle, "SPX Z26 5000 C", "between the two legs");
    }

    #[gpui::test]
    fn a_package_typed_at_a_leg_place_is_refused_in_the_footer(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "add_below", None); // a package row: its first leg
        typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("a package cannot hold a package")
        );
        assert_eq!(h.mode(&mut vcx), "insert");
    }

    #[gpui::test]
    fn up_and_down_walk_the_sheets_own_lines_newest_first(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        let text = |vcx: &VisualTestContext| {
            h.tile.read_with(vcx, |t, cx| {
                t.entry.as_ref().unwrap().input.read(cx).value().to_string()
            })
        };
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(text(&vcx), "SPX Z26 4000 P");
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(text(&vcx), "-5 SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(text(&vcx), "SPX Z26 4000 P");
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(text(&vcx), "", "past the newest is an empty field");
    }

    #[gpui::test]
    fn escape_removes_the_placeholder_and_the_field_blurs_before_it_drops(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        assert!(focused(&mut vcx));
        h.dispatch(&mut vcx, "cancel", None);
        assert!(!focused(&mut vcx), "blurred, then dropped (CLAUDE.md)");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model.entry_row()), None);
        assert_eq!(h.sheet_len(&vcx), 5, "the sheet never held the placeholder");
    }

    #[gpui::test]
    fn a_click_on_the_table_cancels_the_entry_and_another_verb_closes_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        let at = centre_of(&mut vcx, "pricer-cell-0-2");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal", "a click cancels, never commits");
        h.dispatch(&mut vcx, "add_below", None);
        h.dispatch(&mut vcx, "down", None); // from the palette: not an entry verb
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!focused(&mut vcx));
    }

    // ---- the cell editor ----

    fn editor_text(h: &Harness, vcx: &VisualTestContext) -> Option<String> {
        h.tile.read_with(vcx, |t, cx| match &t.editor {
            Some(Editor::Text { input, .. } | Editor::Choice { input, .. }) => {
                Some(input.read(cx).value().to_string())
            }
            None => None,
        })
    }

    fn set_editor(h: &Harness, vcx: &mut VisualTestContext, text: &str) {
        let text = text.to_string();
        vcx.update(|window, cx| {
            let input = match &h.tile.read(cx).editor {
                Some(Editor::Text { input, .. } | Editor::Choice { input, .. }) => input.clone(),
                None => panic!("an editor is open"),
            };
            // `set_value` emits no `Change` (CLAUDE.md's trap): every commit
            // path must re-read the live text, and this proves it does.
            input.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
        });
    }

    /// Spec §12: editing the strike marks it stale and resubmits.
    #[gpui::test]
    fn i_on_a_strike_edits_it_and_enter_reprices_that_line(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let b = h.prices().remove(0);
        h.answer(&mut vcx, &b, 12.5);
        h.dispatch(&mut vcx, "right", Some(3)); // strike
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("5000"));
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the shell's insert-focus predicate sees the editor"
        );
        set_editor(&h, &mut vcx, "5100");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
        assert_eq!(h.cell(&vcx, 0, "strike"), "5100");
        let again = h.prices();
        assert_eq!(
            again[0].lines.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![1]
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.undo.can_undo()),
            "a cell commit is one undo entry"
        );
    }

    #[gpui::test]
    fn a_bad_value_keeps_the_editor_open_with_the_reason(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "edit", None); // qty
        set_editor(&h, &mut vcx, "0");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert_eq!(h.footer(&vcx).as_deref(), Some("quantity must not be zero"));
    }

    #[gpui::test]
    fn a_read_only_cell_and_a_package_row_say_so(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "last_col", None); // rho
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.footer(&vcx).as_deref(), Some(crate::core::READ_ONLY));
        h.dispatch(&mut vcx, "first_col", None);
        h.dispatch(&mut vcx, "down", None); // the package
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(crate::core::READ_ONLY));
    }

    #[gpui::test]
    fn up_and_down_nudge_by_the_texts_precision_and_shift_steps_ten(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("5001"));
        h.dispatch(&mut vcx, "insert_down_big", None);
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4991"));
        h.dispatch(&mut vcx, "insert_up", Some(3));
        assert_eq!(
            editor_text(&h, &vcx).as_deref(),
            Some("4994"),
            "a count multiplies"
        );
    }

    #[gpui::test]
    fn an_empty_shift_commit_inherits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let spot = h
            .columns(&vcx)
            .iter()
            .position(|c| c == "spot_shift")
            .unwrap();
        h.dispatch(&mut vcx, "right", Some(spot as u32));
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "2");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.sheet.shift(0).spot_pct),
            Some(2.0)
        );
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("2"));
        set_editor(&h, &mut vcx, "");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.sheet.shift(0).spot_pct),
            None,
            "empty means inherit"
        );
    }

    #[gpui::test]
    fn a_type_cell_opens_a_typeahead_that_filters_and_enter_picks(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(4)); // type
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(
            h.tile
                .read_with(&vcx, |t, _| matches!(t.editor, Some(Editor::Choice { .. })))
        );
        vcx.simulate_input("p");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.cell(&vcx, 0, "type"), "P");
        // An unknown underlying commits as typed (planning decision 17).
        h.dispatch(&mut vcx, "first_col", None);
        h.dispatch(&mut vcx, "right", None); // underlying
        h.dispatch(&mut vcx, "edit", None);
        vcx.simulate_input("ndx");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.cell(&vcx, 0, "underlying"), "NDX");
        // A closed vocabulary refuses a query nothing matches.
        h.dispatch(&mut vcx, "right", Some(3)); // type
        h.dispatch(&mut vcx, "edit", None);
        vcx.simulate_input("x");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some("no option matches"));
        assert_eq!(h.mode(&mut vcx), "insert");
    }

    /// Spec §12 ("the editor blurs before it drops"), both closers.
    #[gpui::test]
    fn the_editor_gives_up_focus_before_it_is_dropped(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        assert!(focused(&mut vcx));
        h.dispatch(&mut vcx, "cancel", None);
        assert!(!focused(&mut vcx), "cancel: blurred, then dropped");
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        h.dispatch(&mut vcx, "commit", None);
        assert!(!focused(&mut vcx), "commit: blurred, then dropped");
        h.dispatch(&mut vcx, "right", None); // type: the typeahead's field too
        h.dispatch(&mut vcx, "edit", None);
        assert!(focused(&mut vcx));
        h.dispatch(&mut vcx, "cancel", None);
        assert!(!focused(&mut vcx));
    }

    #[gpui::test]
    fn a_click_cancels_an_open_editor_and_never_commits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        let at = centre_of(&mut vcx, "pricer-cell-2-2");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "nothing was committed");
    }

    #[gpui::test]
    fn a_double_click_opens_the_editor_on_that_cell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let at = centre_of(&mut vcx, "pricer-cell-2-4"); // B's strike
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4000"));
    }

    /// An editor whose line leaves the grid closes at once rather than
    /// staying focused with nothing painted: nothing it could commit
    /// would reach the line it was opened on.
    #[gpui::test]
    fn an_editor_whose_line_went_away_closes_with_moved(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        h.tile
            .update(&mut vcx, |t, cx| t.apply_edit(Edit::Remove { at: 0 }, cx))
            .unwrap();
        vcx.run_until_parked();
        h.draw(&mut vcx);
        assert_eq!(h.footer(&vcx).as_deref(), Some(MOVED));
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!focused(&mut vcx), "blurred, then dropped");
    }

    fn slim_views(columns: &str) -> Views {
        let doc = geode_core::config::merge_docs(
            "pricer_views",
            &[geode_core::config::LayerDoc::builtin(
                "pricer_views",
                &format!("[slim]\ncolumns = [{columns}]\n"),
            )
            .unwrap()],
        );
        let (views, diags) = Views::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        views
    }

    fn editor_paint_col(h: &Harness, vcx: &VisualTestContext) -> Option<usize> {
        h.tile.read_with(vcx, |t, cx| {
            t.table.read(cx).delegate().editor.as_ref().map(|e| e.col)
        })
    }

    /// A reload that moves the edited column: the field follows its
    /// column kind, paints over it, and its commit writes that column.
    #[gpui::test]
    fn an_open_editor_follows_its_column_through_a_view_reload(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3)); // strike: plan column 3
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(editor_paint_col(&h, &vcx), Some(3), "fixture");
        set_editor(&h, &mut vcx, "5100");
        let views = slim_views("\"qty\", \"strike\", \"underlying\"");
        vcx.update(|_, cx| {
            h.factory
                .reload(views, None, std::time::Duration::from_secs(60), cx)
        });
        vcx.run_until_parked();
        assert_eq!(h.columns(&vcx), vec!["qty", "strike", "underlying"]);
        assert_eq!(h.mode(&mut vcx), "insert", "the field stays open");
        assert_eq!(editor_paint_col(&h, &vcx), Some(1), "strike's new column");
        let _ = centre_of(&mut vcx, "pricer-editor-0-2"); // tree column + 1
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.footer(&vcx), None, "the commit was not refused");
        assert_eq!(h.cell(&vcx, 0, "strike"), "5100");
    }

    /// A reload that drops the edited column closes the field (blurred,
    /// then dropped) and says why.
    #[gpui::test]
    fn a_view_reload_without_the_edited_column_closes_the_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3)); // strike
        h.dispatch(&mut vcx, "edit", None);
        assert!(focused(&mut vcx), "fixture: the field owns focus");
        let views = slim_views("\"qty\", \"price\"");
        vcx.update(|_, cx| {
            h.factory
                .reload(views, None, std::time::Duration::from_secs(60), cx)
        });
        vcx.run_until_parked();
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(editor_paint_col(&h, &vcx), None, "nothing paints");
        assert!(!focused(&mut vcx), "blurred, then dropped");
        assert_eq!(h.footer(&vcx).as_deref(), Some(MOVED));
        assert_eq!(h.cell(&vcx, 0, "qty"), "1", "nothing was committed");
    }

    /// The typeahead paints under its cell (through `deferred`, over the
    /// rows below) and a click on a row picks it — the pointer route of
    /// `choice_pick`, not a call.
    #[gpui::test]
    fn a_click_on_a_typeahead_row_picks_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(4)); // type
        h.dispatch(&mut vcx, "edit", None);
        let cell = centre_of(&mut vcx, "pricer-editor-0-5");
        let at = centre_of(&mut vcx, "pricer-choice-row-1"); // "P"
        assert!(at.y > cell.y, "the list hangs under its cell");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, "type"), "P");
        assert!(
            !focused(&mut vcx),
            "the pick closes the field: blurred, then dropped"
        );
    }

    // ---- undo, put, move, group and the menu ----

    fn answer_all(h: &Harness, vcx: &mut VisualTestContext, price: f64) {
        for b in h.prices() {
            h.answer(vcx, &b, price);
        }
    }

    /// Spec §12: `dd` then `u` restores the row with its numbers and asks
    /// for nothing.
    #[gpui::test]
    fn dd_then_u_restores_the_row_with_its_numbers_and_no_request(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "bottom", None);
        h.dispatch(&mut vcx, "delete", None);
        assert_eq!(h.tree(&vcx).len(), 2);
        assert!(
            h.prices().is_empty(),
            "a removal changes no remaining request"
        );
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.tree(&vcx).len(), 3);
        assert_eq!(
            h.cell(&vcx, 2, "price"),
            "12.50",
            "its last result came back with it"
        );
        assert!(h.prices().is_empty(), "…so nothing is re-requested");
        h.dispatch(&mut vcx, "redo", None);
        assert_eq!(h.tree(&vcx).len(), 2);
        assert!(h.dispatch(&mut vcx, "redo", None));
        assert_eq!(h.footer(&vcx).as_deref(), Some("nothing to redo"));
    }

    #[gpui::test]
    fn undo_of_a_strike_edit_restores_it_and_reprices(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        answer_all(&h, &mut vcx, 13.0);
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.cell(&vcx, 0, "strike"), "5000");
        assert_eq!(
            h.prices().len(),
            1,
            "an instrument change is a request change (spec §9.3)"
        );
    }

    #[gpui::test]
    fn p_puts_the_yanked_row_with_fresh_ids_and_prices_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "yank_row", None);
        h.dispatch(&mut vcx, "bottom", None);
        h.dispatch(&mut vcx, "put_below", None);
        assert_eq!(
            h.tree(&vcx),
            vec![
                "SPX Z26 5000 C".to_string(),
                "-5 SPX Z26 4800/5200 CS".to_string(),
                "SPX Z26 4000 P".to_string(),
                "SPX Z26 5000 C".to_string(),
            ]
        );
        let ids = h
            .tile
            .read_with(&vcx, |t, _| (t.sheet.id(0), t.sheet.id(5)));
        assert_ne!(ids.0, ids.1, "a put takes fresh ids");
        assert_eq!(h.prices()[0].lines.len(), 1, "and asks for its own price");
        // A package put from a leg lands at a root boundary.
        h.dispatch(&mut vcx, "top", None);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "yank_row", None);
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "down", None); // first leg
        h.dispatch(&mut vcx, "put_above", None);
        let roots = h.tile.read_with(&vcx, |t, _| t.sheet.roots().count());
        assert_eq!(roots, 5);
    }

    #[gpui::test]
    fn shift_j_and_k_move_within_the_parent_and_off_the_end_is_refused(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "move_down", None);
        assert_eq!(
            h.tree(&vcx)[1],
            "SPX Z26 5000 C",
            "A hopped over the package"
        );
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(1),
            "the cursor follows its line"
        );
        h.dispatch(&mut vcx, "move_down", Some(5));
        assert_eq!(h.footer(&vcx).as_deref(), Some("cannot move past the end"));
        h.dispatch(&mut vcx, "top", None);
        h.dispatch(&mut vcx, "move_down", None); // the package hops down
        assert_eq!(h.tree(&vcx)[1], "-5 SPX Z26 4800/5200 CS");
    }

    #[gpui::test]
    fn g_p_groups_roots_into_a_custom_package_and_g_u_ungroups(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"]);
        answer_all(&h, &mut vcx, 1.0);
        h.dispatch(&mut vcx, "group", Some(2));
        assert_eq!(
            h.tree(&vcx)[0],
            "CUSTOM SPX Z26",
            "a custom package, opened"
        );
        assert_eq!(h.tree(&vcx).len(), 4);
        assert_eq!(
            h.cell(&vcx, 0, "price"),
            "2.00",
            "its sum: two legs of 1.00"
        );
        assert!(h.prices().is_empty(), "grouping changes no request");
        h.dispatch(&mut vcx, "down", None); // a leg: g u acts on its package
        h.dispatch(&mut vcx, "ungroup", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()), 3);
        h.dispatch(&mut vcx, "group", Some(9));
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("group needs a contiguous run of top-level lines")
        );
    }

    /// Spec §12: `:shift spot 2` reprices only the lines that inherit it.
    #[gpui::test]
    fn colon_shift_spot_reprices_only_inheriting_lines(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(
            &h,
            &mut vcx,
            Edit::SetShift {
                row: 0,
                shift: crate::core::OwnShifts {
                    spot_pct: Some(1.0),
                    vol_pts: None,
                },
            },
        );
        answer_all(&h, &mut vcx, 12.5);
        h.command(&mut vcx, "shift spot 2").unwrap();
        let b = h.prices().remove(0);
        assert_eq!(
            b.lines.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![3, 4, 5],
            "line 1 has its own spot shift"
        );
        assert!(h.header(&vcx).contains(&"spot +2%".to_string()));
        h.command(&mut vcx, "shift spot clear").unwrap();
        assert!(!h.header(&vcx).iter().any(|t| t.starts_with("spot")));
    }

    #[gpui::test]
    fn colon_spot_rides_in_the_batch_and_clear_is_one_undo(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.command(&mut vcx, "spot spx 5100").unwrap();
        let b = h.prices().remove(0);
        assert_eq!(b.overrides.spot.get("SPX"), Some(&5100.0));
        assert_eq!(b.lines.len(), 4, "every SPX line is restaled (spec §9.3)");
        h.command(&mut vcx, "spot ndx 18000").unwrap();
        answer_all(&h, &mut vcx, 12.5);
        h.command(&mut vcx, "spot clear").unwrap();
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.sheet.overrides().spot.is_empty())
        );
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.sheet.overrides().spot.len()),
            2,
            "one undo restores both"
        );
    }

    #[gpui::test]
    fn the_menu_opens_steps_and_picks_and_a_disabled_row_says_why(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&mut vcx), "menu");
        // Rows: Price all, Group, Ungroup, Undo, Redo, Delete row, then views.
        h.dispatch(&mut vcx, "menu_down", Some(2)); // Ungroup: A is not in a package
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some("not in a package"));
        assert_eq!(
            h.mode(&mut vcx),
            "menu",
            "a disabled row keeps the menu open"
        );
        h.dispatch(&mut vcx, "menu_close", None);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_pick", None); // Price all
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.prices()[0].lines.len(), 4);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_down", Some(7)); // the second view: barrier
        h.dispatch(&mut vcx, "menu_pick", None);
        assert!(h.columns(&vcx).contains(&"barrier".to_string()));
    }

    /// A load answer under an open menu (the menu opens while the
    /// sheet is loading) re-checks its rows: `Delete row`, disabled on
    /// the empty fallback, is enabled once the rows arrive, and picking
    /// it deletes.
    #[gpui::test]
    fn a_load_answer_under_an_open_menu_rechecks_its_rows(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&["SPX Z26 5000 C"]);
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_down", Some(5)); // Delete row
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("no row"),
            "fixture: nothing to delete while loading"
        );
        h.tile
            .update(&mut vcx, |t, cx| t.loaded(Ok(Some(rows)), cx));
        assert_eq!(h.mode(&mut vcx), "menu", "the answer leaves the menu open");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.menu.as_ref().map(|m| m.highlighted)),
            Some(5),
            "the highlight stays where it was"
        );
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(h.footer(&vcx), None, "Delete row was not refused");
        assert_eq!(h.sheet_len(&vcx), 0, "the loaded row was deleted");
    }

    /// A reload under an open menu re-lists its view rows, and the
    /// highlight is clamped to the shorter list.
    #[gpui::test]
    fn a_reload_under_an_open_menu_relists_its_views(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_down", Some(7)); // view: barrier, the last row
        vcx.update(|_, cx| {
            h.factory.reload(
                slim_views("\"qty\", \"price\""),
                None,
                std::time::Duration::from_secs(60),
                cx,
            )
        });
        let (views, highlighted) = h.tile.read_with(&vcx, |t, _| {
            let m = t.menu.as_ref().expect("the menu stays open");
            let views: Vec<String> = m
                .items
                .iter()
                .filter_map(|i| match i {
                    MenuItem::View { label, .. } => Some(label.to_string()),
                    MenuItem::Action { .. } => None,
                })
                .collect();
            (views, m.highlighted)
        });
        assert_eq!(views, vec!["view: slim"]);
        assert_eq!(highlighted, 6, "clamped to the last row");
    }

    #[gpui::test]
    fn every_colon_command_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let lines = [
            "view barrier",
            "shift spot 2",
            "spot SPX 5100",
            "price",
            "refresh 10s",
            "group",
            "ungroup",
        ];
        for word in crate::core::commands::VERBS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let before = h.frame.read_with(&vcx, |f, _| f.versions());
        for line in lines {
            assert!(
                crate::core::commands::parse(line).is_ok(),
                "`{line}` no longer parses"
            );
            let _ = h.command(&mut vcx, line);
            let after = h.frame.read_with(&vcx, |f, _| f.versions());
            assert_eq!(
                (after.scope, after.grouping, after.as_of),
                (before.scope, before.grouping, before.as_of),
                "`:{line}` moved the frame"
            );
            let (level, overlay) = h.diagnostics.update(&mut vcx, |d, _| {
                (d.take_pending_level(), d.take_pending_overlay_toggle())
            });
            assert!(level.is_none() && !overlay, "`:{line}` reached the app");
        }
    }

    // ---- loading guards, put into a closed package, menu clicks ----

    /// Review finding: `:shift`/`:spot`/`:group`/`:ungroup` must not edit
    /// the empty fallback sheet while a load is pending — the edit would
    /// be lost, undo included, the moment `loaded` swaps the real
    /// document in. And even if something else bypassed that guard,
    /// `loaded` clears the undo stack rather than trust inverses recorded
    /// against rows that are about to disappear.
    #[gpui::test]
    fn shift_spot_group_ungroup_refuse_while_loading_and_loaded_clears_any_undo(
        cx: &mut gpui::TestAppContext,
    ) {
        let (store, record) = seeded(&["SPX Z26 5000 C"]);
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        let loading = Err("the sheet is still loading".to_string());
        assert_eq!(h.command(&mut vcx, "shift spot 2"), loading);
        assert_eq!(h.command(&mut vcx, "spot spx 5100"), loading);
        assert_eq!(h.command(&mut vcx, "group"), loading);
        assert_eq!(h.command(&mut vcx, "ungroup"), loading);
        // `:view` and `:refresh` change the sheet as well: `loaded` would
        // replace what they set, so they refuse too (whole-branch review).
        assert_eq!(h.command(&mut vcx, "view barrier"), loading);
        assert_eq!(h.command(&mut vcx, "refresh 10s"), loading);
        // The menu's view rows: Price all, Group, Ungroup, Undo, Redo,
        // Delete row, then views — the second view is row 7.
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_down", Some(7));
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("the sheet is still loading")
        );
        assert!(!h.columns(&vcx).contains(&"barrier".to_string()));
        // Seed the undo stack directly — every production edit path
        // already refuses while loading, this one included once fixed —
        // so `loaded` swapping the sheet has something to lose if it did
        // not clear it.
        h.tile.update(&mut vcx, |t, cx| {
            t.apply_edit(
                Edit::SetSheetShift(crate::core::OwnShifts {
                    spot_pct: Some(1.0),
                    vol_pts: None,
                }),
                cx,
            )
            .unwrap();
        });
        assert!(h.tile.read_with(&vcx, |t, _| t.undo.can_undo()));
        h.tile
            .update(&mut vcx, |t, cx| t.loaded(Ok(Some(rows)), cx));
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some("nothing to undo"));
    }

    /// Review finding: a line put onto a collapsed package's leg slot
    /// (`put_place`'s own rule) must open the package first, the way `o`
    /// does — otherwise the new leg paints into a hidden row.
    #[gpui::test]
    fn put_below_onto_a_collapsed_packages_leg_slot_opens_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "top", None);
        h.dispatch(&mut vcx, "yank_row", None); // yank A: a line
        h.dispatch(&mut vcx, "down", None); // the still-collapsed package
        h.dispatch(&mut vcx, "put_below", None);
        assert_eq!(
            h.tree(&vcx),
            vec![
                "SPX Z26 5000 C".to_string(),
                "CS SPX Z26".to_string(),
                "SPX Z26 5000 C".to_string(),
                "-5 SPX Z26 4800 C".to_string(),
                "5 SPX Z26 5200 C".to_string(),
                "SPX Z26 4000 P".to_string(),
            ],
            "the package opened so the new leg (and its siblings) paint"
        );
        let cursor_row = h.cursor(&vcx).map(|c| c.0);
        assert_eq!(
            cursor_row,
            Some(2),
            "the cursor lands on the new leg, not wherever it fell back to"
        );
    }

    /// Review finding: the menu's row must paint clickable above the
    /// `DataTable` (a later sibling), not be occluded by it.
    #[gpui::test]
    fn a_click_on_a_menu_row_picks_it_and_paints_above_the_table(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "menu", None);
        let at = centre_of(&mut vcx, "pricer-menu-row-0"); // Price all
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(
            h.mode(&mut vcx),
            "normal",
            "the click reached the menu row and picked it"
        );
        assert_eq!(
            h.prices()[0].lines.len(),
            4,
            "Price all reached the tile, not the table underneath it"
        );
    }

    // ---- the write-behind save ----

    fn settle(vcx: &mut VisualTestContext, d: std::time::Duration) {
        vcx.executor().advance_clock(d);
        vcx.run_until_parked();
    }

    fn stored(h: &Harness) -> Sheet {
        crate::core::from_rows("book", &h.store.get("book").expect("a document")).unwrap()
    }

    #[gpui::test]
    fn an_edit_burst_saves_once_after_a_second_of_quiet(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let base = h.store.save_count();
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, std::time::Duration::from_millis(500));
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 3 });
        settle(&mut vcx, std::time::Duration::from_millis(500));
        assert_eq!(
            h.store.save_count(),
            base,
            "a second edit inside the window re-arms it"
        );
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base + 1, "one save for the burst");
        assert_eq!(stored(&h).qty(0), 3);
    }

    #[gpui::test]
    fn an_emptied_sheet_publishes_nothing_and_the_last_document_stays(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 5000 C"]);
        let base = h.store.save_count();
        h.dispatch(&mut vcx, "delete", None);
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(
            h.store.save_count(),
            base,
            "a zero-row document is never published (spec §7.2)"
        );
        assert_eq!(stored(&h).len(), 1);
    }

    #[gpui::test]
    fn a_refused_save_notices_and_the_next_burst_retries(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.store.set_refusing(true);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
        h.store.set_refusing(false);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 4 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.save_notice(&vcx), None);
        assert_eq!(stored(&h).qty(0), 4);
    }

    #[gpui::test]
    fn view_and_refresh_changes_are_saved_with_the_sheet(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.command(&mut vcx, "view barrier").unwrap();
        h.command(&mut vcx, "refresh off").unwrap();
        settle(&mut vcx, SAVE_IDLE);
        let s = stored(&h);
        assert_eq!(s.view, "barrier");
        assert_eq!(s.refresh, crate::core::Refresh::Off);
    }

    #[gpui::test]
    fn closing_flushes_a_pending_save_and_the_next_tile_reopens_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 9 });
        let (store, factory, frame, diagnostics) = (
            h.store.clone(),
            h.factory.clone(),
            h.frame.clone(),
            h.diagnostics.clone(),
        );
        drop(h);
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        drop(vcx);
        let saved = crate::core::from_rows("book", &store.get("book").unwrap()).unwrap();
        assert_eq!(saved.qty(0), 9, "the pending save ran on close");
        let mut record = toml::Table::new();
        record.insert("sheet".into(), "book".into());
        let title = cx.update(|cx| {
            let mut title = String::new();
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let o = factory.create(
                    TileId(TILE + 3),
                    Some(&record),
                    frame.clone(),
                    diagnostics.clone(),
                    window,
                    cx,
                );
                title = o.content.title(cx).to_string();
                cx.new(|cx| gpui_component::Root::new(o.view, window, cx))
            })
            .unwrap();
            title
        });
        assert_eq!(
            title, "pricer · book",
            "the name was given back, so it opens under it"
        );
    }

    // ---- whole-branch review fixes ----

    /// Three underlyings the sheet already holds: HSCEI, NKY, SPX.
    const UNDERLYINGS: [&str; 3] = ["SPX Z26 5000 C", "HSCEI Z26 9000 C", "NKY Z26 30000 C"];

    fn open_underlying(h: &Harness, vcx: &mut VisualTestContext) {
        h.dispatch(vcx, "first_col", None);
        h.dispatch(vcx, "right", None); // underlying
        h.dispatch(vcx, "edit", None);
        assert!(
            h.tile
                .read_with(vcx, |t, _| matches!(t.editor, Some(Editor::Choice { .. })))
        );
    }

    /// Review finding: ranking is a subsequence match, so `HSI` ranks
    /// `HSCEI` first. An untouched highlight is a guess: `enter` commits
    /// the typed text unless the query IS the option or the trader moved
    /// the highlight.
    #[gpui::test]
    fn a_free_typeahead_commits_the_typed_underlying_unless_it_is_an_option_or_the_highlight_moved(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &UNDERLYINGS);
        open_underlying(&h, &mut vcx);
        vcx.simulate_input("HSI");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.cell(&vcx, 0, "underlying"),
            "HSI",
            "a subsequence match is not the trader's answer"
        );
        open_underlying(&h, &mut vcx);
        vcx.simulate_input("hscei");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.cell(&vcx, 0, "underlying"),
            "HSCEI",
            "the option itself, typed in any case"
        );
        // Row 0 is HSCEI now; the options are HSCEI and NKY. `K` matches
        // only NKY, and is not it: only the moved highlight commits NKY.
        open_underlying(&h, &mut vcx);
        vcx.simulate_input("K");
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "insert_down", None);
        let highlighted = h.tile.read_with(&vcx, |t, _| match &t.editor {
            Some(Editor::Choice { list, .. }) => list.highlighted_text().map(str::to_string),
            _ => None,
        });
        assert_eq!(highlighted.as_deref(), Some("NKY"));
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.cell(&vcx, 0, "underlying"),
            "NKY",
            "a moved highlight is a choice"
        );
        // An untouched, empty query keeps the cell's value.
        open_underlying(&h, &mut vcx);
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, "underlying"), "NKY");
        assert_eq!(h.footer(&vcx), None);
    }

    /// Review finding: after a FAILED load, the next edit's save would
    /// publish the near-empty fallback over the real document.
    #[gpui::test]
    fn a_failed_load_blocks_every_save_and_says_so_past_escape(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&BOOK);
        let mut broken = store.get("book").unwrap();
        broken.axes.clear();
        assert!(store.save("book", broken.clone()));
        let base = store.save_count();
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        let blocked = h.save_notice(&vcx).expect("a failed load says so");
        assert!(
            blocked.starts_with("sheet 'book' did not load (")
                && blocked.ends_with("); edits are not saved"),
            "{blocked}"
        );
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(h.sheet_len(&vcx), 1, "the edit landed on the fallback");
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base, "nothing was published");
        assert_eq!(h.store.get("book"), Some(broken));
        h.dispatch(&mut vcx, "escape", None);
        assert_eq!(
            h.save_notice(&vcx),
            Some(blocked),
            "escape does not clear it"
        );
    }

    /// The same block through a pending load answered with an error (Part
    /// 4's store), and none for a genuinely absent document.
    #[gpui::test]
    fn a_pending_load_answered_with_an_error_blocks_saves_and_an_absent_one_does_not(
        cx: &mut gpui::TestAppContext,
    ) {
        let (store, record) = seeded(&BOOK);
        let good = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.tile
            .update(&mut vcx, |t, cx| t.loaded(Err("boom".into()), cx));
        assert_eq!(
            h.save_notice(&vcx).as_deref(),
            Some("sheet 'book' did not load (boom); edits are not saved")
        );
        let base = h.store.save_count();
        h.command(&mut vcx, "refresh off").unwrap();
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base);
        assert_eq!(h.store.get("book"), Some(good));

        let (h2, mut vcx2) = {
            let store = MemorySheetStore::default();
            store.set_pending(true);
            let mut record = toml::Table::new();
            record.insert("sheet".into(), "gone".into());
            open_full(cx, Some(record), store, PricerSettings::default())
        };
        h2.tile.update(&mut vcx2, |t, cx| t.loaded(Ok(None), cx));
        assert_eq!(h2.save_notice(&vcx2), None);
        h2.command(&mut vcx2, "refresh off").unwrap();
        h2.dispatch(&mut vcx2, "add_below", None);
        typed(&h2, &mut vcx2, "SPX Z26 5000 C");
        h2.dispatch(&mut vcx2, "commit", None);
        settle(&mut vcx2, SAVE_IDLE);
        assert!(h2.store.get("gone").is_some(), "an absent document saves");
    }

    /// Review finding: after a refused save the idle task has fired, so a
    /// close that flushed only a pending task dropped the unsaved sheet.
    #[gpui::test]
    fn closing_after_a_refused_save_flushes_the_unsaved_sheet(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.store.set_refusing(true);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
        h.store.set_refusing(false);
        let store = h.store.clone();
        drop(h);
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        drop(vcx);
        let saved = crate::core::from_rows("book", &store.get("book").unwrap()).unwrap();
        assert_eq!(saved.qty(0), 7, "the dirty sheet was saved on close");
    }

    /// Review finding: the save state had shared the pricing notice's
    /// slot, so `REFUSED` overwrote it and a later good submit cleared it.
    #[gpui::test]
    fn a_refused_saves_notice_outlives_pricing_notices_and_escape(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.store.set_refusing(true);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "price", None);
        let batches = h.prices();
        assert!(!batches.is_empty(), "a successful submit");
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
        for b in batches {
            h.answer(&mut vcx, &b, 12.5);
        }
        h.close_channel();
        h.dispatch(&mut vcx, "price", None);
        assert_eq!(h.notice(&vcx).as_deref(), Some(REFUSED));
        assert_eq!(
            h.save_notice(&vcx).as_deref(),
            Some(NOT_SAVED),
            "both show: the pricing notice never overwrites the save state"
        );
        h.dispatch(&mut vcx, "escape", None);
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
    }

    /// Review finding: `:` and `/` are shell-owned and bypass `dispatch`,
    /// so they must close the menu and the fields themselves.
    #[gpui::test]
    fn colon_and_find_close_the_menu_and_an_open_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&mut vcx), "menu");
        h.command(&mut vcx, "price").unwrap();
        assert_eq!(h.mode(&mut vcx), "normal", "`:` closed the menu");
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        assert_eq!(h.mode(&mut vcx), "insert");
        vcx.update(|window, cx| h.content.find(FindEvent::Changed("SPX".into()), window, cx));
        assert_eq!(h.mode(&mut vcx), "normal", "`/` closed the editor");
        assert!(!focused(&mut vcx), "blurred, then dropped");
        assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "nothing was committed");
    }

    /// Review finding: a chevron click is a click — it cancels an open
    /// editor before it toggles.
    #[gpui::test]
    fn a_chevron_click_cancels_an_open_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        let at = centre_of(&mut vcx, "pricer-chevron-1");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, "strike"), "5000", "nothing was committed");
        assert_eq!(h.tree(&vcx).len(), 5, "and the package toggled");
    }

    // ---- undo keeps what the trader had open ----

    /// Deleting an open package and undoing brings it back open, with the
    /// cursor on it — not closed, with the cursor on the row after it.
    #[gpui::test]
    fn dd_then_u_on_an_open_package_restores_it_open_under_the_cursor(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None); // P
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "delete", None);
        assert_eq!(h.tree(&vcx).len(), 2);
        let r = crate::session::Record::from_table(&h.serialize(&mut vcx));
        assert!(r.expanded.is_empty(), "the session never carries a dead id");
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.tree(&vcx).len(), 5, "P came back open");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1), "on P");
        let r = crate::session::Record::from_table(&h.serialize(&mut vcx));
        assert_eq!(r.expanded, vec![crate::core::LineId(2)]);
        h.dispatch(&mut vcx, "redo", None);
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.tree(&vcx).len(), 5, "and again after a redo");
    }

    /// A leg restored under a package closed since opens it, so the
    /// cursor it lands on is a painted row.
    #[gpui::test]
    fn undo_of_a_leg_delete_opens_its_package_and_lands_on_the_leg(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None); // P
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "down", None); // its first leg
        h.dispatch(&mut vcx, "delete", None);
        h.dispatch(&mut vcx, "collapse_all", None);
        assert_eq!(h.tree(&vcx).len(), 3);
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.tree(&vcx).len(), 5, "P opened");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2), "on the restored leg");
    }

    #[gpui::test]
    fn g_u_then_u_on_an_open_package_regroups_it_open(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None); // P
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "ungroup", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.sheet.roots().count()), 4);
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.tree(&vcx).len(), 5, "P came back open");
    }

    // ---- notices ----

    /// "loading…" is the only sign a load is in progress: `escape` may
    /// clear find, never it.
    #[gpui::test]
    fn escape_keeps_the_loading_notice_until_the_rows_arrive(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&["SPX Z26 5000 C"]);
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));
        h.dispatch(&mut vcx, "escape", None);
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));
        h.tile
            .update(&mut vcx, |t, cx| t.loaded(Ok(Some(rows)), cx));
        assert_eq!(h.notice(&vcx), None);
    }

    /// A refusal whose lines then stop being stale (deleted) has nothing
    /// to retry: the next submit clears the notice rather than leaving
    /// "retrying" standing with no retry pending.
    #[gpui::test]
    fn a_refusal_with_nothing_left_to_price_clears(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&["SPX Z26 5000 C"]);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.close_channel();
        h.visible(&mut vcx, true);
        assert_eq!(h.notice(&vcx).as_deref(), Some(REFUSED));
        h.dispatch(&mut vcx, "delete", None);
        assert_eq!(h.sheet_len(&vcx), 0);
        settle(&mut vcx, RETRY_AFTER);
        assert_eq!(h.notice(&vcx), None);
    }

    /// A refusal paints over the notice it found, never destroys it.
    #[gpui::test]
    fn a_standing_notice_survives_a_refusal_and_returns_after_it(cx: &mut gpui::TestAppContext) {
        let mut record = toml::Table::new();
        record.insert("sheet".into(), "gone".into());
        let (h, mut vcx) = open_full(
            cx,
            Some(record),
            MemorySheetStore::default(),
            PricerSettings::default(),
        );
        h.visible(&mut vcx, true);
        let gone = Some("sheet 'gone' was not found; opened empty".to_string());
        assert_eq!(h.notice(&vcx), gone);
        h.fill_queue();
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.notice(&vcx).as_deref(), Some(REFUSED));
        let _ = h.requests(); // the queue drains
        settle(&mut vcx, RETRY_AFTER);
        assert_eq!(h.prices().len(), 1, "the retry was admitted");
        assert_eq!(h.notice(&vcx), gone);
    }

    /// Each consecutive refusal doubles the wait: a closed channel asks
    /// at 1 s, then 2 s later, not every second forever.
    #[gpui::test]
    fn consecutive_refusals_back_off(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&BOOK);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.close_channel();
        h.visible(&mut vcx, true);
        let tag = |vcx: &VisualTestContext| h.tile.read_with(vcx, |t, _| t.tag);
        let first = tag(&vcx);
        settle(&mut vcx, RETRY_AFTER);
        let second = tag(&vcx);
        assert!(second > first, "the first retry fires at 1 s");
        settle(&mut vcx, RETRY_AFTER);
        assert_eq!(tag(&vcx), second, "the second does not fire 1 s later");
        settle(&mut vcx, RETRY_AFTER);
        assert!(tag(&vcx) > second, "but does by 2 s");
    }

    #[test]
    fn the_retry_delay_doubles_to_a_thirty_second_cap() {
        let secs: Vec<u64> = (1..=8).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 30, 30, 30]);
    }

    // ---- the repricing rules ----

    /// Planning decision 4: `in_flight` decides WHETHER to submit. Every
    /// line in flight at its current revision asks for nothing more; an
    /// edit to one line sends a batch of every stale line.
    #[gpui::test]
    fn price_submits_nothing_while_every_line_is_in_flight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let first = h.prices().remove(0);
        h.command(&mut vcx, "price").unwrap();
        h.dispatch(&mut vcx, "price", None);
        assert!(h.prices().is_empty(), "every line is already on its way");
        let e = new_strike(&h, &vcx, 0, 5100.0);
        edit(&h, &mut vcx, e);
        let again = h.prices();
        assert_eq!(again.len(), 1);
        assert_eq!(
            again[0].lines.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![1, 3, 4, 5],
            "the edited line and every other still-stale one"
        );
        assert!(again[0].tag > first.tag);
    }

    #[gpui::test]
    fn an_admitted_submission_clears_the_refusal(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.fill_queue();
        h.dispatch(&mut vcx, "price", None);
        assert_eq!(h.notice(&vcx).as_deref(), Some(REFUSED));
        let _ = h.requests(); // the queue drains
        h.dispatch(&mut vcx, "price", None);
        assert_eq!(h.prices().len(), 1);
        assert_eq!(h.notice(&vcx), None);
    }

    /// A hide cancels by key, so the batch before it may answer in part;
    /// the show's batch carries a newer tag and the old outcome installs
    /// nothing.
    #[gpui::test]
    fn an_outcome_from_before_a_hide_installs_nothing_after_the_show(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let before = h.prices().remove(0);
        h.visible(&mut vcx, false);
        h.visible(&mut vcx, true);
        let after = h.prices().remove(0);
        assert_eq!(after.tag, before.tag + 1);
        h.answer(&mut vcx, &before, 99.0);
        assert_eq!(h.cell(&vcx, 0, "price"), "", "the older tag is dropped");
        h.answer(&mut vcx, &after, 12.5);
        assert_eq!(h.cell(&vcx, 0, "price"), "12.50");
    }

    // ---- clicks while the entry field is open ----

    /// [A, P, Q], both packages closed: grid rows A=0, P=1, Q=2.
    const TWO_PACKAGES: [&str; 3] = [
        "SPX Z26 5000 C",
        "-5 SPX Z26 4800/5200 CS",
        "SPX Z26 4000/4400 CS",
    ];

    /// Three roots A, B, C: strikes 5000, 4000, 3000.
    const THREE_LINES: [&str; 3] = ["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"];

    /// The placeholder is a grid row: a click painted below it names the
    /// row one index lower once the entry closes. The chevron's row must
    /// be read before the close, or P's chevron toggles Q.
    #[gpui::test]
    fn a_chevron_click_below_an_open_entry_toggles_that_package(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &TWO_PACKAGES);
        h.dispatch(&mut vcx, "down", None); // P
        h.dispatch(&mut vcx, "add_above", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.model.entry_row()), Some(1));
        let at = centre_of(&mut vcx, "pricer-chevron-2"); // P, under the placeholder
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal", "the click closed the entry");
        assert_eq!(
            h.tree(&vcx),
            vec![
                "SPX Z26 5000 C".to_string(),
                "-5 SPX Z26 4800/5200 CS".to_string(),
                "-5 SPX Z26 4800 C".to_string(),
                "5 SPX Z26 5200 C".to_string(),
                "SPX Z26 4000/4400 CS".to_string(),
            ],
            "P opened, not Q"
        );
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1), "on P");
    }

    #[gpui::test]
    fn a_cell_click_below_an_open_entry_lands_on_that_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "down", None); // B
        h.dispatch(&mut vcx, "add_above", None);
        let at = centre_of(&mut vcx, "pricer-cell-2-4"); // B's strike
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cursor(&vcx), Some((1, 3)), "B's strike, not C's");
    }

    /// A click on the placeholder itself only closes the entry.
    #[gpui::test]
    fn a_click_on_the_placeholder_only_closes_the_entry(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "down", None); // B
        h.dispatch(&mut vcx, "add_above", None);
        let at = centre_of(&mut vcx, "pricer-cell-1-4"); // the placeholder
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cursor(&vcx), Some((1, 0)), "the cursor stays on B");
    }

    #[gpui::test]
    fn a_double_click_below_an_open_entry_edits_that_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "down", None); // B
        h.dispatch(&mut vcx, "add_above", None);
        let at = centre_of(&mut vcx, "pricer-cell-2-4"); // B's strike
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("4000"), "B's strike");
    }

    /// A double-click on the placeholder only closes the entry: the row
    /// that slides up under its second press opens nothing.
    #[gpui::test]
    fn a_double_click_on_the_placeholder_opens_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "down", None); // B
        h.dispatch(&mut vcx, "add_above", None);
        let at = centre_of(&mut vcx, "pricer-cell-1-4"); // the placeholder
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(editor_text(&h, &vcx), None, "no editor on B");
    }

    /// A double-click on the tree column opens nothing, but the cursor
    /// stays on the row it was aimed at, not the one that slid up.
    #[gpui::test]
    fn a_tree_column_double_click_below_an_open_entry_keeps_that_row(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "down", None); // B
        h.dispatch(&mut vcx, "add_above", None);
        let at = centre_of(&mut vcx, "pricer-cell-2-0"); // B's shorthand
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1), "on B, not C");
    }

    /// The closing press's line is handed to the next press only: a
    /// later double-click at the same spot edits what is painted there.
    #[gpui::test]
    fn a_later_double_click_at_the_same_spot_edits_the_row_painted_there(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "down", None); // B
        h.dispatch(&mut vcx, "add_above", None);
        let at = centre_of(&mut vcx, "pricer-cell-2-4"); // B's strike
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.cursor(&vcx), Some((1, 3)), "on B");
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(editor_text(&h, &vcx).as_deref(), Some("3000"), "C's strike");
    }
}
