//! Line-pricer tile state and prepared rendering models.
//!
//! The tile owns one Sheet, an Rc<GridModel> installed into a DataTable, and prepared
//! header/footer state. Request-changing edits use apply_edit or apply_edits to record
//! undo; deliveries use deliver and refresh ticks use tick. These paths rebuild and
//! install the model on change, outside rendering.

use crate::content::{PricerSettings, Shared};
use crate::core::cell::{self, CellEditor};
use crate::core::clip::{put_place, spec_of};
use crate::core::columns::ColumnKind;
use crate::core::commands::{self, Command, ShiftField};
use crate::core::edit::{Edit, EditError, Undo};
use crate::core::entry::{history, next_place, place_for, target_label};
use crate::core::sheet::{Delivered, LineId, Refresh, Sheet};
use crate::core::shorthand::parse;
use crate::core::shorthand::render_expiry;
use crate::core::storage::{from_rows, rows_from_snapshot, to_rows};
use crate::core::template::Template;
use crate::core::tree::Expansion;
use crate::core::undo::UndoStack;
use crate::core::views::ColumnPlan;
use crate::core::{Place, RowSpec};
use crate::delegate::{ChevronClicked, DateFieldPaint, EditorField, EditorPaint, SheetDelegate};
use crate::grid::GridModel;
use crate::header::{self, HeaderInputs, HeaderModel};
use crate::popup::{Menu, MenuItem, choice_paint, render_menu};
use crate::session::Record;
use crate::store::Loaded;
use chrono::Utc;
use geode_core::clock::Clock;
use geode_core::document::DocumentRows;
use geode_core::pricing::{PriceLine, PriceOutcome, PriceParams};
use geode_core::query::{QueryKey, QueryOutcome};
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::linenumbers::{LineNumbers, UiSettings};
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, find_match};
use geode_shell::vimnav::NavCommand;
use geode_widgets::datefield::{DateTimeField, FieldKey, Precision, Segment, route};
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, SharedString,
    Subscription, Task, Window, div,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, v_flex};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub(crate) const LOADING: &str = "loading…";

/// Initial retry delay after a refused pricing submission. Retries run even when
/// periodic refresh is disabled.
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(1);
/// Maximum delay between retries in a consecutive refusal streak.
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

/// The save slot after a queued save's outcome reported a failure.
fn not_saved(reason: &str) -> SharedString {
    format!("sheet not saved: {reason}; the next edit retries").into()
}

/// Why a load the store never submitted failed (`Loaded::Refused`).
pub(crate) const LOAD_REFUSED: &str =
    "the store refused the load: the data service is busy or gone";

/// The save slot's standing notice after a failed load (see
/// `PricerTile::save_blocked`).
fn blocked_notice(name: &str, why: &str) -> SharedString {
    format!("sheet '{name}' did not load ({why}); edits are not saved").into()
}

/// The footer after an `:rm` confirm answered anything but `y`.
pub(crate) const NOT_REMOVED: &str = "sheet not removed";

/// An armed `:rm` confirm (planning decision 14, the market-data upload
/// confirm's shape). The confirm holds the keyboard on its own `focus`
/// handle, tracked by the prompt the header paints, whose `on_key_down`
/// runs before the shell root's listener (`PricerTile::confirm_key`).
/// `_blur` is the focus-leaving half: any move of window focus off the
/// prompt cancels. Dropping this drops the subscription, so a confirm
/// answered by a key never also hears its own blur.
pub(crate) struct PendingRemove {
    sheet: String,
    prompt: SharedString,
    focus: FocusHandle,
    _blur: Subscription,
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

/// The entry bar (entry-bar spec §4): where its rows will land, the
/// field, and the sheet's own lines to walk with `up`/`down`.
pub(crate) struct Entry {
    pub place: Place,
    pub input: Entity<InputState>,
    /// Where `enter` lands, as the bar's muted label: `target_label` of
    /// `place`, rebuilt whenever `place` changes, never in render.
    pub label: SharedString,
    /// A refused `enter`'s reason, under the field. Any typed edit clears
    /// it (it describes text that is no longer there), as does a history
    /// step.
    pub error: Option<SharedString>,
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
    /// An expiry's segmented date field (every expiry, a tenor included):
    /// a pure field the tile routes keys into (`date_field_key`), its own
    /// focus handle — what `holds_focus` answers from and what makes the
    /// shell read keys as typing — and the segments prepared on every
    /// change, so paint formats nothing.
    Date {
        line: LineId,
        col: usize,
        kind: ColumnKind,
        field: DateTimeField,
        focus: FocusHandle,
        paint: DateFieldPaint,
        /// The footer's standing note while the field is open — a
        /// tenor's "opened on today" — restored after any key or refusal
        /// and dropped only when the field commits or cancels, so the
        /// trader still sees that `enter` replaces the tenor.
        note: Option<SharedString>,
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
    /// The text field, `None` for the date field.
    fn input(&self) -> Option<&Entity<InputState>> {
        match self {
            Editor::Text { input, .. } | Editor::Choice { input, .. } => Some(input),
            Editor::Date { .. } => None,
        }
    }

    /// The handle that holds window focus while this editor is open.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Editor::Text { input, .. } | Editor::Choice { input, .. } => {
                input.read(cx).focus_handle(cx)
            }
            Editor::Date { focus, .. } => focus.clone(),
        }
    }

    fn target(&self) -> (LineId, usize, ColumnKind) {
        match self {
            Editor::Text {
                line, col, kind, ..
            }
            | Editor::Choice {
                line, col, kind, ..
            }
            | Editor::Date {
                line, col, kind, ..
            } => (*line, *col, *kind),
        }
    }

    fn set_col(&mut self, to: usize) {
        match self {
            Editor::Text { col, .. } | Editor::Choice { col, .. } | Editor::Date { col, .. } => {
                *col = to
            }
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
    /// Consecutive pricing refusals. While nonzero, REFUSED overlays the header notice
    /// without replacing it. Admission, or a submit with no further work needed, ends
    /// the streak and reveals the underlying notice.
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
    /// A change not yet in any queued save. Cleared only when the store
    /// accepts a save (queues it), so `on_release` flushes a refused save
    /// as well as one still waiting on its idle timer.
    dirty: bool,
    /// The latest queued save's outcome was a failure: the document does
    /// not hold what this tile last queued, so the next save (a burst, or
    /// the close) must write it again. Save outcomes carry no link to the
    /// save that produced them; every one is delivered, in the writer's
    /// order, so the last to arrive is the latest queued save's — a later
    /// `Ok` clears this, a later `Err` sets it.
    pub(crate) save_failed: bool,
    /// The latest save attempt was refused admission (`NOT_SAVED`): an
    /// `Ok` arriving afterwards confirms an earlier save, not this one,
    /// so it must not clear that notice.
    save_refused: bool,
    /// The latest load's tag: a `Delivery::Query` under any other is an
    /// earlier (cancelled or superseded) load's and is dropped. Separate
    /// from the pricing `tag`; both ride this tile's `QueryKey`, but the
    /// query pool and the pricing worker are separate lanes.
    load_tag: u64,
    /// A hide cancelled by key while loading, which cancels the load too:
    /// the next show asks again under a fresh tag.
    load_cancelled: bool,
    /// `:name`'s old name, forgotten once a save under the new name is
    /// CONFIRMED (never on submission): a save that fails, or never
    /// lands, leaves the old document where it was.
    rename_from: Option<String>,
    /// Forgets this tile asked for and not yet answered — an `:rm`'s
    /// sheet, or a `:name`'s retired old name — so a failure is painted
    /// here, where it was asked for.
    forgetting: Vec<String>,
    /// The armed `:rm` confirm: `None` outside it.
    pub(crate) confirm: Option<PendingRemove>,
    /// Loading, but the load is not submitted: this sheet's name has a
    /// save queued and unanswered (`Shared::pending_saves`), and a read
    /// now could return the generation before it. The factory's
    /// `save_answered` starts the load.
    pub(crate) load_waiting: bool,
    /// A user error for the footer (spec §8.3); cleared by the next verb.
    pub(crate) footer: Option<SharedString>,
    /// What the footer paints: `footer`, else the cursor row's failure.
    pub(crate) footer_text: Option<SharedString>,
    pub(crate) header: HeaderModel,
    title: SharedString,
    /// The header `⋯` tooltip's selector, built once from the tile id.
    menu_tip: SharedString,
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
    /// Undo records depend on the rows left by their edits. New recorded edits use
    /// apply_edit/apply_edits; history_step replays their inverses. Other structural
    /// mutation would invalidate that history.
    pub(crate) undo: UndoStack,
    refresh_task: Option<Task<()>>,
    retry_task: Option<Task<()>>,
    /// The write-behind idle timer (spec §7.3): armed by every change,
    /// re-armed by the next one, flushed by `on_release`.
    save_task: Option<Task<()>>,
    /// The entry bar (entry-bar spec §4): `o` opens it, `enter` parses and
    /// inserts through `apply_edit`, `escape`, a click or another verb
    /// drops it. `None` in normal mode.
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
}

fn app_clock(cx: &App) -> Clock {
    cx.try_global::<geode_shell::clock::AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| Clock::machine().0)
}

/// The first `untitled-N` with no document, no queued save and no open
/// tile (spec §7.4). Known limitation (planning decision 12): before the
/// first catalog lands, a name with a document this session has not seen
/// can be picked; its first save adds a generation to that document.
fn untitled(shared: &Shared) -> String {
    (1..)
        .map(|n| format!("untitled-{n}"))
        .find(|name| !shared.open.borrow().contains(name) && !shared.taken(name))
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
        // A name open elsewhere or being removed is refused here as `:e`
        // refuses it: two tiles never write one sheet, and no tile opens a
        // document a queued forget is about to delete.
        let name = match record.sheet.as_deref() {
            Some(n) if shared.open.borrow().contains(n) => {
                let fresh = untitled(&shared);
                notices.push(format!(
                    "sheet '{n}' is open in another tile; opened {fresh}"
                ));
                fresh
            }
            Some(n) if shared.refuse_retiring(n).is_err() => {
                let fresh = untitled(&shared);
                notices.push(format!("sheet '{n}' is being removed; opened {fresh}"));
                fresh
            }
            Some(n) => n.to_string(),
            None => untitled(&shared),
        };
        shared.open.borrow_mut().insert(name.clone());
        // The tile's first load is tag 1; `start_load` bumps it for every
        // later one, so an answer to an earlier load never installs.
        let load_tag = 1;
        // A save of this name still queued (a closed tile's flush): the
        // read waits for its answer (see `load_waiting`).
        let load_waiting = shared.save_pending(&name);
        let answer = if load_waiting {
            Loaded::Pending
        } else {
            shared.store.load(&name, QueryKey(id.0), load_tag)
        };
        let (sheet, loading) = match answer {
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
            // The load was never submitted: nothing will ever call
            // `loaded` to clear a `loading` state, so this is the
            // failed-load path (spec §7.1), not a load still on its
            // way — a refused read must never let the empty fallback
            // stand in for, and then overwrite, the real document.
            Loaded::Refused => {
                blocked = Some(blocked_notice(&name, LOAD_REFUSED));
                (fallback(&name, &record), false)
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

        let mut delegate = SheetDelegate::new(cx.theme(), cx.weak_entity());
        delegate.line_numbers = cx
            .try_global::<UiSettings>()
            .map_or(LineNumbers::Off, |s| s.line_numbers);
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
        // `[ui] line_numbers` arrives through the shell's `UiSettings`
        // global; the module contract forbids reaching into the shell.
        cx.observe_global::<UiSettings>(|this, cx| this.on_ui_settings(cx))
            .detach();
        // `priced_at` cells and the header time follow the app clock.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.clock = app_clock(cx);
            this.rebuild(cx);
        })
        .detach();
        // Release flushes a dirty sheet once, cancels pricing, and releases its name
        // for another tile to open.
        cx.on_release(|this: &mut PricerTile, _cx| {
            // Spec §7.3: the sheet is not lost until the tile is — a save
            // still waiting on its idle timer, or one the store refused,
            // runs now (`save_now` itself refuses a blocked sheet).
            // A save queued and still unconfirmed is already on the
            // writer: nothing extra.
            this.flush_save();
            this.data.cancel(QueryKey(this.id.0));
            this.shared.open.borrow_mut().remove(&this.sheet.name);
            // A rename not yet confirmed keeps its old document.
            if let Some(old) = this.rename_from.take() {
                this.shared.retiring.borrow_mut().remove(&old);
            }
            // An armed `:rm` confirm drops here without a blur: release
            // has no `Window`. The shell moves focus off a closing tile
            // before it drops it (the close's focus move), so the prompt's
            // handle no longer holds focus by now, and dropping it strands
            // nothing.
            this.confirm = None;
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
            save_failed: false,
            save_refused: false,
            load_tag,
            load_cancelled: false,
            rename_from: None,
            forgetting: Vec::new(),
            confirm: None,
            load_waiting,
            footer: None,
            footer_text: None,
            header: HeaderModel::default(),
            title: SharedString::default(),
            menu_tip: format!("tip-pricer-menu-button-{}", id.0).into(),
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

    /// `insert` while EITHER field is open — the entry field or the cell
    /// editor (the expiry's date field included, though it is no text
    /// input). The shell treats a key as typing only when the focused
    /// tile holds focus AND its context reads `mode == insert`
    /// (`ShellView::occupant_insert_stack`); any other word lets a bare or
    /// shifted letter reach the shell's own bindings (`shift+d` duplicated
    /// the tile). `dispatch` tells the two fields apart by which is open.
    ///
    /// An armed `:rm` confirm is `insert` too: its prompt holds the
    /// keyboard exactly as a field does.
    pub(crate) fn mode(&self) -> &'static str {
        if self.confirm.is_some() || self.entry.is_some() || self.editor.is_some() {
            "insert"
        } else if self.menu.is_some() {
            "menu"
        } else {
            "normal"
        }
    }

    /// Does one of THIS tile's own fields (the entry field, the cell
    /// editor, the typeahead's field or the date field) hold window focus? Answered from
    /// the focus handles, never from the mode.
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        let entry = self
            .entry
            .as_ref()
            .is_some_and(|e| e.input.read(cx).focus_handle(cx).is_focused(window));
        let editor = self
            .editor
            .as_ref()
            .is_some_and(|e| e.focus_handle(cx).is_focused(window));
        let confirm = self
            .confirm
            .as_ref()
            .is_some_and(|c| c.focus.is_focused(window));
        entry || editor || confirm
    }

    /// The open date field, if the editor is one.
    #[cfg(test)]
    pub(crate) fn date_field(&self) -> Option<&DateTimeField> {
        match &self.editor {
            Some(Editor::Date { field, .. }) => Some(field),
            _ => None,
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The sheet as this tile holds it, read-only: a host's tests observe
    /// what a load installed without reaching into the tile's state.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sheet(&self) -> &Sheet {
        &self.sheet
    }

    /// Whether the tile is waiting for its sheet's load (painted
    /// `loading…`), including a load deferred behind a queued save.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_loading(&self) -> bool {
        self.loading
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
            if self.loading && self.load_cancelled {
                self.start_load(cx);
            }
            self.submit(cx);
            self.restart_timer(cx);
        } else {
            // The cancel below reaches the query pool too, so a pending
            // load gets no answer: the next show asks again.
            if self.loading {
                self.load_cancelled = true;
            }
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

    /// Apply one edit, record its inverse, then rebuild, reprice, and schedule saving.
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

    /// Apply several edits as one undo entry. On refusal, replay prior inverses in
    /// reverse order without recording the batch. A refused rollback clears history and
    /// leaves the partially rolled-back sheet for after_edit to rebuild.
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
                        // A refused inverse can leave a partially rolled-back sheet.
                        // Clear history because its remaining inverses depend on the
                        // previous row layout.
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

    /// `o`: the entry bar under the header, the field focused (entry-bar
    /// spec §4.1). Lines land below the cursor row; a leg place opens
    /// its package so what lands is visible.
    fn open_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            self.footer = Some("the sheet is still loading".into());
            return;
        }
        self.close_entry(window, cx);
        let place = place_for(&self.sheet, self.cursor_sheet_row(), true);
        if let Place::Leg { package, .. } = place {
            self.expansion.set(self.sheet.id(package), true);
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(ENTRY_HINT));
        cx.subscribe_in(&input, window, |this, _input, event, _window, cx| {
            if let InputEvent::Change = event
                && let Some(entry) = this.entry.as_mut()
                && entry.error.take().is_some()
            {
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.entry = Some(Entry {
            place,
            input,
            label: target_label(&self.sheet, place).into(),
            error: None,
            history: history(&self.sheet),
            history_ix: None,
        });
        self.rebuild(cx);
    }

    /// `enter`: parse, insert, reprice, and advance the place past what
    /// landed; a parse error or a refusal keeps the text and says why
    /// under the field.
    fn commit_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.as_mut() else {
            return;
        };
        let text = entry.input.read(cx).value().to_string();
        let spec = match parse(&text) {
            Ok(spec) => spec,
            Err(e) => {
                entry.error = Some(format!("{} (column {})", e.message, e.offset + 1).into());
                cx.notify();
                return;
            }
        };
        let at = entry.place;
        // Advance the place first so the edit's own rebuild labels what
        // comes next; put it back on a refusal.
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
                    entry.label = target_label(&self.sheet, entry.place).into();
                    entry.error = None;
                    entry.input.update(cx, |s, cx| s.set_value("", window, cx));
                }
                self.rebuild(cx);
            }
            Err(e) => {
                if let Some(entry) = self.entry.as_mut() {
                    entry.place = at;
                    entry.error = Some(e.to_string().into());
                }
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
        entry.error = None;
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
            Ok(CellEditor::Date(date)) => {
                // A tenor has no date here (the library's calendar resolves
                // it, never the pricer): the field opens on today by the
                // app clock, and the footer says so — a commit replaces
                // the tenor with that date.
                let note: Option<SharedString> = date.is_none().then(|| {
                    let tenor = self
                        .sheet
                        .instrument(row)
                        .map(|i| render_expiry(i.expiry()))
                        .unwrap_or_default();
                    format!("{tenor} is a tenor: opened on today; enter sets a date").into()
                });
                self.footer = note.clone();
                let date = date.unwrap_or_else(|| app_clock(cx).today(Utc::now()));
                let field = DateTimeField::open(
                    date.and_hms_opt(0, 0, 0).expect("midnight exists"),
                    Precision::Date,
                    Segment::Day,
                );
                let paint = DateFieldPaint::of(&field, self.id.0);
                Editor::Date {
                    line,
                    col,
                    kind,
                    field,
                    focus: cx.focus_handle(),
                    paint,
                    note,
                }
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
        editor.focus_handle(cx).focus(window, cx);
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
        if matches!(self.editor, Some(Editor::Date { .. })) {
            self.commit_date(window, cx);
            return;
        }
        // The editor's borrow ends inside this block, before any `self` call.
        let (value, (line, col, kind)) = {
            let Some(editor) = self.editor.as_mut() else {
                return;
            };
            let Some(input) = editor.input() else {
                return;
            };
            let text = input.read(cx).value().to_string();
            let target = editor.target();
            let value = if let Editor::Choice {
                list, free, moved, ..
            } = editor
            {
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
            } else {
                Choice::Value(text)
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
        let Some(row) = self.editor_row(line, col, kind, window, cx) else {
            return;
        };
        let answer = cell::commit(&self.sheet, row, kind, &value);
        self.finish_commit(answer, window, cx);
    }

    /// Settle an editor's parsed commit: a refusal keeps the editor open
    /// with the reason; `Ok(None)` — the value the line already holds —
    /// closes it with no edit (no undo entry, no reprice, no save); an
    /// edit closes it (blur first, so the rebuild never paints a dead
    /// field) and then applies through `apply_edit`.
    fn finish_commit(
        &mut self,
        answer: Result<Option<Edit>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match answer {
            Err(why) => {
                self.footer = Some(why.into());
                self.rebuild_chrome();
                cx.notify();
            }
            Ok(None) => {
                self.close_editor(window, cx);
                self.rebuild_chrome();
                cx.notify();
            }
            Ok(Some(edit)) => {
                self.close_editor(window, cx);
                if let Err(e) = self.apply_edit(edit, cx) {
                    self.footer = Some(e.to_string().into());
                    self.rebuild_chrome();
                    cx.notify();
                }
            }
        }
    }

    /// The sheet row an editor opened on (`line`, `col`, `kind`) still
    /// commits to, or `None` after closing it with `MOVED`: its line went
    /// away, or a view switch put another column kind at its index.
    fn editor_row(
        &mut self,
        line: LineId,
        col: usize,
        kind: ColumnKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let same_column = self
            .plan
            .columns
            .get(col)
            .is_some_and(|c| c.def.kind == kind);
        let row = self.sheet.index_of(line).filter(|_| same_column);
        if row.is_none() {
            self.close_editor(window, cx);
            self.footer = Some(MOVED.into());
            self.rebuild_chrome();
            cx.notify();
        }
        row
    }

    /// `enter` in the date field: finish a half-typed segment or refuse
    /// naming it (the field stays open), re-check the target, then one
    /// `Edit` through `apply_edit` — or nothing at all when the line
    /// already expires on that date (no undo entry, no reprice, no save).
    /// The field closes (blur first) before the edit applies.
    fn commit_date(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id.0;
        let Some(Editor::Date {
            line,
            col,
            kind,
            field,
            paint,
            ..
        }) = self.editor.as_mut()
        else {
            return;
        };
        let (line, col, kind) = (*line, *col, *kind);
        let finished = field.complete_pending();
        *paint = DateFieldPaint::of(field, id);
        let date = field.date();
        if let Err(segment) = finished {
            self.footer = Some(format!("finish the {} or backspace", segment.name()).into());
            self.sync_editor(cx);
            self.rebuild_chrome();
            cx.notify();
            return;
        }
        let Some(row) = self.editor_row(line, col, kind, window, cx) else {
            return;
        };
        let answer = cell::commit_date(&self.sheet, row, date);
        self.finish_commit(answer, window, cx);
    }

    /// A key on the focused date field, before it bubbles to the shell.
    /// `geode_widgets::datefield::route` is the one key table: arrows move
    /// and step (`shift`: ten), digits type, `backspace` clears the
    /// segment, `enter` commits, `escape` cancels. A chord, or any key the
    /// table does not name, answers `false` and bubbles on. A handled key
    /// retires a standing footer (a refusal, the tenor note).
    pub(crate) fn date_field_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = event.keystroke.modifiers;
        let chord = modifiers.control || modifiers.alt || modifiers.platform;
        let Some(key) = route(event.keystroke.key.as_str(), modifiers.shift, chord) else {
            return false;
        };
        let id = self.id.0;
        let Some(Editor::Date {
            field, paint, note, ..
        }) = self.editor.as_mut()
        else {
            return false;
        };
        match key {
            FieldKey::Commit => {
                self.footer = None;
                self.commit_edit(window, cx);
            }
            FieldKey::Cancel => {
                self.footer = None;
                self.close_editor(window, cx);
                self.rebuild_chrome();
                cx.notify();
            }
            other => {
                if field.apply(other) {
                    *paint = DateFieldPaint::of(field, id);
                }
                // A standing refusal retires; the field's own note stays.
                let note = note.clone();
                self.sync_editor(cx);
                if self.footer != note {
                    self.footer = note;
                    self.rebuild_chrome();
                }
                cx.notify();
            }
        }
        true
    }

    /// A mouse-down on a date segment: select it, and take the keyboard
    /// back if focus had moved off the field while it stayed open.
    pub(crate) fn date_segment_clicked(
        &mut self,
        segment: Segment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.id.0;
        let Some(Editor::Date {
            field,
            paint,
            focus,
            ..
        }) = self.editor.as_mut()
        else {
            return;
        };
        field.select(segment);
        *paint = DateFieldPaint::of(field, id);
        if !focus.is_focused(window) {
            focus.focus(window, cx);
        }
        self.sync_editor(cx);
        cx.notify();
    }

    /// Move the typeahead highlight on pointer hover, notifying only on change. Do not
    /// set moved: hovering alone must not make Enter choose a free-list guess instead
    /// of the typed text. Keyboard navigation and explicit clicks do.
    pub(crate) fn choice_hover(&mut self, row: usize, cx: &mut Context<Self>) {
        let changed = match &mut self.editor {
            Some(Editor::Choice { list, .. }) => {
                list.highlighted() != row && list.set_highlighted(row)
            }
            _ => false,
        };
        if changed {
            self.sync_editor(cx);
            cx.notify();
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
        if editor.focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
        self.sync_editor(cx);
        cx.notify();
    }

    /// `up`/`down` (`shift`: ten) step a numeric editor by its text's own
    /// precision (planning decision 2); in a typeahead they move the
    /// highlight.
    fn nudge(&mut self, steps: i64, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id.0;
        let refused = match &mut self.editor {
            // The date field steps its active segment — the same step its
            // own `up`/`down` take.
            Some(Editor::Date {
                field, paint, note, ..
            }) => {
                field.step(steps);
                *paint = DateFieldPaint::of(field, id);
                // `dispatch` cleared the footer: the field's note stands.
                self.footer = note.clone();
                None
            }
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
            let (field, choice) = match e {
                Editor::Choice { list, input, .. } => (
                    EditorField::Text(input.clone()),
                    Some(Rc::new(choice_paint(list))),
                ),
                Editor::Text { input, .. } => (EditorField::Text(input.clone()), None),
                Editor::Date { paint, focus, .. } => (
                    EditorField::Date {
                        paint: paint.clone(),
                        focus: focus.clone(),
                    },
                    None,
                ),
            };
            Some(EditorPaint {
                row,
                col,
                field,
                choice,
            })
        });
        self.table.update(cx, |t, cx| {
            t.delegate_mut().editor = paint;
            cx.notify();
        });
    }

    /// Rebuild after edits and history replay, submit stale pricing work, and arm
    /// write-behind saving. Keep expansion IDs so undo can restore an open package.
    /// Periodic refresh keeps its own timer and skips empty sheets.
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
                let _ = t.save_now();
                t.rebuild_chrome();
                cx.notify();
            });
        }));
    }

    /// Save now what the idle timer would have saved, or what a failed
    /// save left unsaved, and drop the timer. A close and the app's quit
    /// both come here: neither may leave edits behind a timer that will
    /// never fire.
    pub(crate) fn flush_save(&mut self) {
        self.save_task = None;
        if self.dirty || self.save_failed {
            let _ = self.save_now();
        }
    }

    /// The whole sheet, once. An empty sheet publishes nothing (the last
    /// non-empty generation stays as history, spec §7.2); a refusal paints
    /// the save slot and stays `dirty`, so the next burst or the close
    /// retries. A sheet whose load failed publishes nothing at all: the
    /// fallback would become the document's latest generation.
    ///
    /// An accepted save is only QUEUED: it clears `dirty` (the close has
    /// nothing to flush) but not the save notice, which only the outcome
    /// (`save_answered`) settles; the name is taken meanwhile
    /// (`Shared::pending_saves`).
    ///
    /// `false` only when the store refused the save — the sheet is still
    /// unsaved; a blocked or empty sheet has nothing to write and answers
    /// `true`.
    pub(crate) fn save_now(&mut self) -> bool {
        if self.save_blocked {
            return true;
        }
        let Some(rows) = to_rows(&self.sheet) else {
            return true;
        };
        if self.shared.store.save(&self.sheet.name, rows) {
            self.shared.save_queued(&self.sheet.name);
            self.shared
                .save_origins
                .borrow_mut()
                .insert(self.sheet.name.clone(), self.id);
            self.dirty = false;
            self.save_failed = false;
            self.save_refused = false;
            true
        } else {
            self.save_refused = true;
            self.save_notice = Some(NOT_SAVED.into());
            false
        }
    }

    /// The outcome of this sheet's latest queued save (spec §7.3): `Ok`
    /// clears a failure and its notice; `Err` paints the reason and marks
    /// the sheet to be written again by the next burst or the close. A
    /// sheet whose load failed queues nothing, so no outcome is its own:
    /// its standing notice is never cleared by one.
    ///
    /// An `Ok` after `:name` means the sheet is stored under its new name:
    /// only now is the old name's document forgotten.
    pub(crate) fn save_answered(&mut self, answer: Result<(), String>, cx: &mut Context<Self>) {
        if self.save_blocked {
            return;
        }
        match answer {
            Ok(()) => {
                self.save_failed = false;
                if !self.save_refused {
                    self.save_notice = None;
                }
                if let Some(old) = self.rename_from.take() {
                    if self.shared.open.borrow().contains(&old) {
                        // A tile holds the old name: forgetting would
                        // delete a sheet in use. `:e` and a restore refuse
                        // a retiring name, so no route reaches this today.
                        self.shared.retiring.borrow_mut().remove(&old);
                    } else if self.shared.store.forget(&old) {
                        // Reserved until the forget is answered.
                        self.forgetting.push(old);
                    } else {
                        self.shared.retiring.borrow_mut().remove(&old);
                        self.notice = Some(
                            format!("old sheet '{old}' not removed: the store refused it").into(),
                        );
                    }
                }
            }
            Err(reason) => {
                self.save_failed = true;
                self.save_notice = Some(not_saved(&reason));
            }
        }
        self.rebuild_chrome();
        cx.notify();
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
            // End a refusal streak when no additional submission is needed, including
            // when its stale lines have been removed.
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
            // Retry independently of periodic refresh. Log once per streak so a closed
            // channel does not produce a warning on every attempt.
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

    /// Clear the refusal overlay and pending retry. The next refusal starts at
    /// RETRY_AFTER and logs a new streak.
    fn end_refusals(&mut self) {
        self.refusals = 0;
        self.retry_task = None;
    }

    /// Keep at most one retry task. A further refusal while it is pending increments
    /// the streak without replacing that task's scheduled delay.
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

    /// Ask the store for this tile's sheet under a fresh load tag: the
    /// first show after a hide cancelled a pending load, and `:e`. A
    /// standing pricing refusal streak ends here — nothing prices while
    /// loading, so `REFUSED` would otherwise cover `loading…` with no
    /// retry left to end it.
    pub(crate) fn start_load(&mut self, cx: &mut Context<Self>) {
        self.load_tag += 1;
        self.load_cancelled = false;
        self.loading = true;
        self.end_refusals();
        if self.shared.save_pending(&self.sheet.name) {
            self.load_waiting = true;
            self.notice = Some(LOADING.into());
            self.rebuild(cx);
            return;
        }
        self.load_waiting = false;
        let key = QueryKey(self.id.0);
        let answer = match self.shared.store.load(&self.sheet.name, key, self.load_tag) {
            Loaded::Pending => {
                self.notice = Some(LOADING.into());
                self.rebuild(cx);
                return;
            }
            Loaded::Rows(rows) => Ok(Some(rows)),
            Loaded::Missing => Ok(None),
            // Never submitted: nothing is coming, so this is the failed
            // load, not a `loading` that never resolves.
            Loaded::Refused => Err(LOAD_REFUSED.to_string()),
        };
        self.loaded(answer, cx);
    }

    /// `Delivery::Query` for this tile: only the latest load's answer,
    /// decoded (`rows_from_snapshot`) into `loaded`. A decode failure is
    /// a failed load (saves blocked), never a half-installed sheet.
    pub fn query_answered(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        if outcome.key != QueryKey(self.id.0) || outcome.tag != self.load_tag || !self.loading {
            return;
        }
        let name = self.sheet.name.clone();
        let answer = outcome
            .snapshot
            .and_then(|snapshot| rows_from_snapshot(&name, &snapshot));
        self.loaded(answer, cx);
    }

    /// A save this tile queued under `sheet`, answered after the tile
    /// moved on from that name (`:e`/`:new`/`:name`) or while it waits to
    /// load it again: a failure means the edits it last queued there were
    /// never stored, so it says so; an `Ok` is nothing to it.
    pub(crate) fn left_save_answered(
        &mut self,
        sheet: &str,
        answer: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let Err(reason) = answer else {
            return;
        };
        if self.rename_from.as_deref() == Some(sheet) {
            // `:name` moved this sheet's edits to its new name, whose own
            // save carries them: a failure here only leaves the old
            // name's copy stale, and that copy is forgotten once the new
            // name's save is confirmed.
            return;
        }
        let text: SharedString =
            format!("sheet '{sheet}' was not saved: {reason}; its last edits were not stored")
                .into();
        if self.sheet.name == sheet {
            // Waiting to load it: `loaded` clears `notice`, so this goes
            // in the save slot, where the next confirmed save clears it.
            self.save_notice = Some(text);
        } else {
            self.notice = Some(text);
        }
        self.rebuild_chrome();
        cx.notify();
    }

    /// A forget of `sheet` this tile asked for (`:rm`, or `:name`
    /// retiring its old name) landed or failed; a failure says so in the
    /// header. Any other sheet's is not this tile's.
    pub(crate) fn forget_answered(
        &mut self,
        sheet: &str,
        answer: &Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let Some(at) = self.forgetting.iter().position(|s| s == sheet) else {
            return;
        };
        self.forgetting.remove(at);
        if let Err(reason) = answer {
            self.notice = Some(format!("sheet '{sheet}' not removed: {reason}").into());
            self.rebuild_chrome();
            cx.notify();
        }
    }

    /// A `Pending` load's answer (planning decision 7), reached from the
    /// `Delivery::Query` arm through `query_answered`.
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
        // The loaded sheet's own `refresh` sets the interval.
        self.restart_timer(cx);
    }

    /// A FAILED load (not an absent document): the fallback must never be
    /// published over the real document (see `save_blocked`).
    /// A standing save notice (a waiting tile told its last edits were
    /// not stored) is kept after the block: the block says why nothing
    /// saves now, the older one what was already lost.
    fn block_saves(&mut self, name: &str, why: &str) {
        self.save_blocked = true;
        let blocked = blocked_notice(name, why);
        self.save_notice = Some(match self.save_notice.take() {
            Some(lost) => format!("{blocked}; {lost}").into(),
            None => blocked,
        });
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
        // A verb arriving under an armed `:rm` (a palette dispatch; a key
        // never gets here, the prompt consumes it) answers "no" first.
        if self.confirm.is_some() {
            self.cancel_remove(window, cx);
        }
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
            "add_below" => {
                self.open_entry(window, cx);
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
                    let delta = if verb == "menu_down" {
                        n as isize
                    } else {
                        -(n as isize)
                    };
                    m.highlighted = crate::popup::step(&m.items, m.highlighted, delta);
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
        // Select the first row reinstated by a Restore inverse. Other history steps
        // keep the cursor's existing LineId, subject to the rebuild's cursor
        // resolution.
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
                    // Open a restored leg's parent so the selected row is visible.
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

    /// Prepare action groups followed by the available Views. Command titles come from
    /// content::ACTIONS. Enabled actions show default keys in the trailing lane;
    /// disabled actions show their refusal reason there.
    fn menu_items(&self) -> Vec<MenuItem> {
        let row = self.cursor_sheet_row();
        let root_line =
            row.is_some_and(|r| self.sheet.is_line(r) && self.sheet.parent(r).is_none());
        let packaged =
            row.is_some_and(|r| self.sheet.is_package(r) || self.sheet.parent(r).is_some());
        let action = |id: &'static str, hint, enabled| MenuItem::Action {
            id,
            title: crate::content::action_title(id),
            hint,
            enabled,
        };
        let mut items = vec![
            action("pricer::price", ":price", Ok(())),
            MenuItem::Separator,
            action(
                "pricer::group",
                "g p",
                if root_line {
                    Ok(())
                } else {
                    Err("group needs a top-level line")
                },
            ),
            action(
                "pricer::ungroup",
                "g u",
                if packaged {
                    Ok(())
                } else {
                    Err("not in a package")
                },
            ),
            MenuItem::Separator,
            action(
                "pricer::undo",
                "u",
                if self.undo.can_undo() {
                    Ok(())
                } else {
                    Err("nothing to undo")
                },
            ),
            action(
                "pricer::redo",
                "ctrl+r",
                if self.undo.can_redo() {
                    Ok(())
                } else {
                    Err("nothing to redo")
                },
            ),
            MenuItem::Separator,
            action(
                "pricer::delete",
                "d d",
                if row.is_some() { Ok(()) } else { Err("no row") },
            ),
        ];
        let views = self.shared.views.borrow();
        if !views.is_empty() {
            items.push(MenuItem::Separator);
            items.push(MenuItem::Section("View"));
            items.extend(views.names().map(|name| MenuItem::View {
                name: name.to_string().into(),
                current: name == self.sheet.view,
            }));
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

    /// Move the menu highlight on pointer hover, notifying only on change. Action and
    /// View rows qualify, including disabled actions; separators and headings do not.
    pub(crate) fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(m) = self.menu.as_mut() else {
            return;
        };
        if m.highlighted == index || !m.items.get(index).is_some_and(MenuItem::pickable) {
            return;
        }
        m.highlighted = index;
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
            // Structure, not a row: nothing to pick.
            MenuItem::Separator | MenuItem::Section(_) => {}
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
        // A question standing over another verb is withdrawn, not answered.
        let _ = self.disarm_remove(window, cx);
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
            Command::Edit(name) => self.edit_sheet(name, cx),
            Command::New => {
                // Chosen while this tile still holds its name, so `:new`
                // never lands back on the sheet it leaves.
                let name = untitled(&self.shared);
                self.switch_sheet(name, false, cx)
            }
            Command::Name(name) => self.rename(name, cx),
            Command::Remove(name) => self.arm_remove(name, window, cx),
        }
    }

    // ---- sheets: `:e`, `:new`, `:name`, `:rm` (spec §7.4) --------------

    /// `:e <sheet>` (planning decision 13): refused when another tile
    /// holds `name` (two writers would race) or its document is being
    /// removed; the tile's own name is a no-op, unless its load failed
    /// (`save_blocked`): then it reloads, the in-place retry of a refused
    /// or failed load (nothing on a blocked sheet was ever saved, so
    /// nothing is lost). A name with a save queued
    /// and not yet answered is claimed at once but its load waits for
    /// that answer (`start_load`): reads run on the query pool and saves
    /// on the ingest writer, unordered, so a read now could return the
    /// generation before the save.
    fn edit_sheet(&mut self, name: String, cx: &mut Context<Self>) -> Result<(), String> {
        if name == self.sheet.name {
            if self.save_blocked {
                return self.switch_sheet(name, true, cx);
            }
            return Ok(());
        }
        self.shared.refuse_retiring(&name)?;
        if self.shared.open.borrow().contains(&name) {
            return Err(format!("sheet '{name}' is open in another tile"));
        }
        self.switch_sheet(name, true, cx)
    }

    /// Put `name` in this tile: flush the outgoing sheet if it has
    /// unsaved changes (refused → stay, so nothing is lost to the switch),
    /// give its name back, claim `name`, and start over — undo, expansion,
    /// cursor and every per-sheet save state belong to the sheet left
    /// behind. Pricing in flight is cancelled and its tag retired: line
    /// ids restart per sheet, so an old answer would install onto a new
    /// sheet's line. `load`: ask the store (`:e`); otherwise the sheet is
    /// new and empty (`:new`).
    fn switch_sheet(
        &mut self,
        name: String,
        load: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.save_task = None;
        if (self.dirty || self.save_failed) && !self.save_now() {
            self.rebuild_chrome();
            cx.notify();
            return Err(format!(
                "sheet '{}' was not saved; still open here",
                self.sheet.name
            ));
        }
        {
            let mut open = self.shared.open.borrow_mut();
            open.remove(&self.sheet.name);
            open.insert(name.clone());
        }
        self.data.cancel(QueryKey(self.id.0));
        self.tag += 1;
        self.in_flight.clear();
        self.sheet = Sheet::new(&name);
        self.undo.clear();
        self.expansion = Expansion::default();
        self.held_expanded = None;
        self.cursor = Cursor::default();
        self.find = None;
        self.notice = None;
        self.dirty = false;
        self.save_failed = false;
        self.save_refused = false;
        self.save_notice = None;
        self.save_blocked = false;
        // A rename not yet confirmed keeps its old document.
        if let Some(old) = self.rename_from.take() {
            self.shared.retiring.borrow_mut().remove(&old);
        }
        self.load_waiting = false;
        self.loading = false;
        self.load_cancelled = false;
        self.resolve_plan();
        if load {
            self.start_load(cx);
        } else {
            // A higher tag drops any answer to a load the old sheet had
            // pending (`query_answered` also wants `loading`).
            self.load_tag += 1;
            self.rebuild(cx);
            self.submit(cx);
        }
        self.restart_timer(cx);
        Ok(())
    }

    /// `:name <new>` (planning decision 13): refused when `new` is open or
    /// already a document (or about to be one: a queued save). Otherwise
    /// the tile takes the new name at once and saves the whole sheet under
    /// it now. The OLD name's document is forgotten only once a save under
    /// the new name is confirmed (`save_answered`). If that save FAILS the
    /// tile keeps the new name (the trader asked for it), the save notice
    /// paints the reason, the old document is left alone, and the next
    /// edit retries — its confirmation retires the old name then. An empty
    /// sheet writes nothing (spec §7.2), so its old document stays as it
    /// was. A tile closed or switched before the confirmation leaves the
    /// old document too: nothing is ever lost to a rename.
    fn rename(&mut self, name: String, cx: &mut Context<Self>) -> Result<(), String> {
        if name == self.sheet.name {
            return Ok(());
        }
        self.refuse_while_loading()?;
        if self.save_blocked {
            // The sheet shown is the fallback: saved under a new name and
            // the old one forgotten, the real document would be lost.
            return Err(format!(
                "sheet '{}' did not load; it cannot be renamed",
                self.sheet.name
            ));
        }
        if self.rename_from.is_some() {
            return Err("the last rename is not saved yet".into());
        }
        self.shared.refuse_retiring(&name)?;
        if self.shared.open.borrow().contains(&name) || self.shared.taken(&name) {
            return Err(format!("sheet '{name}' already exists"));
        }
        let old = std::mem::replace(&mut self.sheet.name, name.clone());
        {
            let mut open = self.shared.open.borrow_mut();
            open.remove(&old);
            open.insert(name);
        }
        self.save_task = None;
        self.save_failed = false;
        self.save_refused = false;
        self.save_notice = None;
        if to_rows(&self.sheet).is_some() {
            self.shared.retiring.borrow_mut().insert(old.clone());
            self.rename_from = Some(old);
        }
        // Dirty until queued: a refused save is retried by the next edit
        // or the close.
        self.dirty = true;
        let _ = self.save_now();
        self.rebuild_chrome();
        cx.notify();
        Ok(())
    }

    /// `:rm <sheet>` (planning decisions 13–14): refused for any open
    /// sheet (this tile's own included) and for a name that is no
    /// document; otherwise arms the y/n confirm. Nothing is forgotten here.
    fn arm_remove(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if name == self.sheet.name {
            return Err(format!(
                "sheet '{name}' is open here: close it or `:e` another sheet first"
            ));
        }
        self.shared.refuse_retiring(&name)?;
        if self.shared.open.borrow().contains(&name) {
            return Err(format!("sheet '{name}' is open in another tile"));
        }
        if !self.shared.taken(&name) {
            return Err(format!("no sheet '{name}'"));
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            if this.confirm.is_some() {
                this.cancel_remove(window, cx);
            }
        });
        self.confirm = Some(PendingRemove {
            prompt: format!("remove sheet '{name}' and all its history? (y/n)").into(),
            sheet: name,
            focus,
            _blur: blur,
        });
        self.rebuild_chrome();
        cx.notify();
        Ok(())
    }

    /// The confirm's own key handler, run from the prompt's `on_key_down`
    /// in `header::render` — on the focused element, so before the shell
    /// root's listener. While armed EVERY key is consumed (`true`): bare
    /// `y` forgets, anything else cancels. A keystroke that answers the
    /// question must not also act on the tile or the shell.
    pub(crate) fn confirm_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.confirm.is_none() {
            return false;
        }
        let ks = &event.keystroke;
        if ks.key == "y" && !ks.modifiers.modified() {
            self.submit_remove(window, cx);
        } else {
            self.cancel_remove(window, cx);
        }
        true
    }

    /// A pointer press anywhere on the tile while armed cancels (the tile
    /// root's capture-phase mouse-down): a press on the header or the
    /// menu button moves no focus, so the blur half alone would leave the
    /// question standing behind the click.
    pub(crate) fn cancel_remove_on_pointer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_some() {
            self.cancel_remove(window, cx);
        }
    }

    /// Drop the armed confirm, giving up the keyboard first when its
    /// prompt holds it (a surface dropping a focused handle blurs it).
    fn disarm_remove(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PendingRemove> {
        let pending = self.confirm.take()?;
        if pending.focus.is_focused(window) {
            window.blur(cx);
        }
        Some(pending)
    }

    fn cancel_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self.disarm_remove(window, cx);
        self.footer = Some(NOT_REMOVED.into());
        self.rebuild_chrome();
        cx.notify();
    }

    /// `y`: forget the sheet. Whether it went reaches the store through
    /// `PricerFactory::forget_answered`, and a failure is painted here.
    fn submit_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.disarm_remove(window, cx) else {
            return;
        };
        // The name was checked when the question was armed; a tile may
        // have opened it, or a `:name` begun retiring it, since. Forgetting
        // then would delete a sheet in use or race that rename's forget.
        let refusal = if self.shared.open.borrow().contains(&pending.sheet) {
            Some("it is open in another tile")
        } else if self.shared.retiring.borrow().contains(&pending.sheet) {
            Some("it is being removed")
        } else {
            None
        };
        if let Some(why) = refusal {
            self.footer = Some(format!("sheet '{}' not removed: {why}", pending.sheet).into());
        } else if self.shared.store.forget(&pending.sheet) {
            // Reserved until the forget is answered.
            self.shared
                .retiring
                .borrow_mut()
                .insert(pending.sheet.clone());
            self.forgetting.push(pending.sheet);
        } else {
            self.footer = Some(
                format!(
                    "sheet '{}' not removed: the store refused it",
                    pending.sheet
                )
                .into(),
            );
        }
        self.rebuild_chrome();
        cx.notify();
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
        commands::completions(line, cursor, &views, &unds, &self.shared.sheet_names())
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
            self.clock,
        ));
        self.install_model(cx);
        self.rebuild_chrome();
        cx.notify();
    }

    /// The only way a model reaches the table (spec §8.2).
    pub(crate) fn install_model(&mut self, cx: &mut Context<Self>) {
        let model = Rc::clone(&self.model);
        let loading = self.loading;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().model = model;
            t.delegate_mut().loading = loading;
            // Before `refresh`, which re-reads the tree column's width.
            t.delegate_mut().refresh_numbers();
            t.refresh(cx);
        });
        // Resolve the editor by LineId and ColumnKind before cursor synchronization, so
        // a moved column keeps the editor and cursor aligned.
        self.follow_editor(cx);
        self.sync_cursor(cx);
        self.sync_editor(cx);
    }

    /// Keep an open editor attached to its LineId and ColumnKind across rebuilds.
    /// Update its plan index and the cursor column together. If either target leaves
    /// the visible grid or plan, close the editor and show MOVED.
    fn follow_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let (line, _, kind) = editor.target();
        let col = self.plan.columns.iter().position(|c| c.def.kind == kind);
        match col.filter(|_| self.model.grid_row_of(line).is_some()) {
            Some(c) => {
                editor.set_col(c);
                self.cursor.col = c;
            }
            None => self.drop_orphaned_editor(cx),
        }
    }

    /// Remove an editor whose target disappeared during a rebuild without a Window.
    /// Retain its focus handle until deferred access to the opening window can blur it.
    /// Check that it still owns focus so a newer field is not blurred.
    fn drop_orphaned_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        self.footer = Some(MOVED.into());
        let focus = editor.focus_handle(cx);
        drop(editor);
        if let Some(handle) = self.editor_window {
            App::defer(cx, move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    if focus.is_focused(window) {
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
            prompt: self.confirm.as_ref().map(|c| c.prompt.clone()),
            save: self.save_notice.clone(),
            settings: &settings,
            clock: self.clock,
        });
        self.title = format!("Pricer · {}", self.sheet.name).into();
        self.footer_text = self.footer.clone().or_else(|| {
            let row = self.cursor_row().and_then(|r| self.model.rows[r].row)?;
            match self.sheet.state(row) {
                crate::core::LineState::Failed(m) => Some(m.clone().into()),
                _ => None,
            }
        });
        // Recompute an open menu after load, reload, or delivery changes its contents.
        // Preserve its index when possible, otherwise clamp and snap to an Action or
        // View row; snapping includes disabled actions.
        if self.menu.is_some() {
            let items = self.menu_items();
            if let Some(m) = self.menu.as_mut() {
                m.highlighted = crate::popup::snap(&items, m.highlighted);
                m.items = items;
            }
        }
    }

    // ---- the cursor -------------------------------------------------------

    /// Grid rows a cursor may sit on.
    pub(crate) fn cursor_rows(&self) -> impl Iterator<Item = usize> + '_ {
        0..self.model.rows.len()
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
            t.delegate_mut().refresh_numbers();
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

    /// Mirror the line-number setting and refresh the table on a change:
    /// the tree column's width includes the gutter, and `TableState`
    /// caches column widths.
    fn on_ui_settings(&mut self, cx: &mut Context<Self>) {
        let mode = cx
            .try_global::<UiSettings>()
            .map_or(LineNumbers::Off, |s| s.line_numbers);
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            if d.line_numbers != mode {
                d.line_numbers = mode;
                d.refresh_numbers();
                t.refresh(cx);
                cx.notify();
            }
        });
    }

    /// Resolve a painted grid row to its LineId before closing fields.
    fn line_at(&self, row: usize) -> Option<LineId> {
        self.model.rows.get(row).and_then(|r| r.id)
    }

    /// Chevron activation cancels open fields, then toggles the package resolved before
    /// the close.
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
            // Double-click uses the same edit route as i. SelectCell has already
            // cancelled the previous field; the tree column opens no editor.
            TableEvent::DoubleClickedCell(row, col) => {
                let line = self.line_at(*row);
                self.close_entry(window, cx);
                let Some(id) = line else {
                    return;
                };
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
        let header = header::render(
            &self.header,
            header::HeaderChrome {
                stale,
                stack: self.stack.as_ref(),
                tile_id: self.id,
                tile: &tile,
                menu_open: self.menu.is_some(),
                menu_tip: self.menu_tip.clone(),
                confirm: self.confirm.as_ref().map(|c| &c.focus),
            },
            theme,
        );
        let paints = self.table.read(cx).delegate().paints;
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
                            .child(render_menu(m, &paints, &tile, cx)),
                    )
                });
        let body = div().flex_1().min_h_0().w_full().child(
            DataTable::new(&self.table)
                .with_size(Size::XSmall)
                .bordered(false)
                .stripe(false),
        );
        let bar = self
            .entry
            .as_ref()
            .map(|e| header::render_entry_bar(&e.input, &e.label, e.error.as_ref(), theme));
        let footer = header::render_footer(self.footer_text.as_ref(), theme);
        // A pointer press anywhere on the tile cancels an armed `:rm`
        // confirm — capture phase, so it runs before the press reaches
        // whatever it was aimed at, and it never stops propagation.
        let cancel_tile = tile.clone();
        v_flex()
            .size_full()
            .debug_selector(|| format!("tile-content-{}", self.id.0))
            .when(self.confirm.is_some(), |el| {
                el.capture_any_mouse_down(move |_, window, cx| {
                    cancel_tile.update(cx, |t, cx| t.cancel_remove_on_pointer(window, cx));
                })
            })
            .child(header)
            .children(bar)
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
    use chrono::Datelike as _;
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::pricing::Expiry;
    use geode_core::pricing::{PriceOutcome, PriceParams, PriceResult};
    use geode_core::query::QueryKey;
    use geode_core::scopes::SavedScopes;
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::ActionId;
    use geode_shell::diagnostics::Diagnostics;
    use geode_shell::frame::Frame;
    use geode_shell::module::{Delivery, ModuleFactory, TileContent};
    use geode_shell::tiling::TileId;
    use geode_widgets::datefield::Segment;
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
        /// The planned columns' vocabulary names, in order.
        pub fn columns(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| {
                t.model.columns.iter().map(|c| c.name.to_string()).collect()
            })
        }
        /// The planned columns' header labels, in order.
        pub fn labels(&self, vcx: &VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| {
                t.model
                    .columns
                    .iter()
                    .map(|c| c.label.to_string())
                    .collect()
            })
        }
        /// One cell's painted text, by grid row and vocabulary name.
        pub fn cell(&self, vcx: &VisualTestContext, row: usize, column: &str) -> String {
            self.tile.read_with(vcx, |t, _| {
                let c = t
                    .model
                    .columns
                    .iter()
                    .position(|c| c.name == column)
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
        pub fn entry_text(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile.read_with(vcx, |t, cx| {
                t.entry
                    .as_ref()
                    .map(|e| e.input.read(cx).value().to_string())
            })
        }
        pub fn entry_label(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile
                .read_with(vcx, |t, _| t.entry.as_ref().map(|e| e.label.to_string()))
        }
        pub fn entry_error(&self, vcx: &VisualTestContext) -> Option<String> {
            self.tile.read_with(vcx, |t, _| {
                t.entry
                    .as_ref()
                    .and_then(|e| e.error.as_ref().map(|s| s.to_string()))
            })
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
        assert_eq!(h.title(&mut vcx), "Pricer · untitled-1");
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
            "Pricer · untitled-2"
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
        assert_eq!(h.title(&mut vcx), "Pricer · book");
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
        assert_eq!(h.title(&mut vcx), "Pricer · gone");
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
        assert_eq!(title, "Pricer · untitled-1");
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

    /// `[ui] line_numbers` reaches a live sheet through the shell's
    /// `UiSettings` global. Off paints no gutter; `on` paints one per
    /// painted row (an expanded package's legs included) beside the tree
    /// cell, widening the pinned tree column by exactly the gutter, in one
    /// lane whatever the row's depth. `rel` re-numbers on a cursor move,
    /// and the cursor row's own number is the one `NG` jumps to. Off
    /// gives the width back.
    #[gpui::test]
    fn the_line_numbers_global_paints_a_gutter_beside_the_tree_column(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.draw(&mut vcx);
        assert!(
            vcx.debug_bounds("pricer-gutter-0").is_none(),
            "no gutter while the setting is off (no global set)"
        );
        let bounds = |vcx: &mut VisualTestContext, sel: &'static str| {
            vcx.debug_bounds(sel)
                .unwrap_or_else(|| panic!("{sel} painted"))
        };
        let tree = bounds(&mut vcx, "pricer-cell-0-0");
        let value = bounds(&mut vcx, "pricer-cell-0-1");
        let set = |vcx: &mut VisualTestContext, mode: LineNumbers| {
            vcx.update(|_, cx| cx.set_global(UiSettings { line_numbers: mode }));
            h.draw(vcx);
        };
        let texts = |vcx: &mut VisualTestContext| -> Vec<String> {
            h.tile.read_with(vcx, |t, cx| {
                let d = t.table.read(cx).delegate();
                (0..t.model.rows.len())
                    .map(|r| d.gutter_text(r).map(|s| s.to_string()).unwrap_or_default())
                    .collect()
            })
        };

        set(&mut vcx, LineNumbers::On);
        let gutter = bounds(&mut vcx, "pricer-gutter-0");
        let width = h
            .tile
            .read_with(&vcx, |t, cx| t.table.read(cx).delegate().gutter_px());
        assert!(width > 0.0, "sanity: a live gutter has width");
        let tree_on = bounds(&mut vcx, "pricer-cell-0-0");
        assert!(
            (f32::from(bounds(&mut vcx, "pricer-cell-0-1").left() - value.left()) - width).abs()
                < 0.5,
            "the tree column widened by the gutter ({width}); the observer must \
             `refresh` the table, which caches `column()`'s width"
        );
        assert!(
            (tree_on.size.width - tree.size.width).abs() < gpui::px(0.5),
            "the tree cell keeps its own width"
        );
        assert!(
            gutter.right() <= tree_on.left(),
            "the gutter sits beside the tree cell: {gutter:?} then {tree_on:?}"
        );
        assert_eq!(texts(&mut vcx), ["1", "2", "3"]);

        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "toggle", None);
        h.draw(&mut vcx);
        assert_eq!(
            texts(&mut vcx),
            ["1", "2", "3", "4", "5"],
            "an open package's legs are numbered rows"
        );
        assert!(
            (bounds(&mut vcx, "pricer-gutter-2").left()
                - bounds(&mut vcx, "pricer-gutter-0").left())
            .abs()
                < gpui::px(0.5),
            "a leg's number sits in the same lane as a root's; the indent starts after it"
        );

        set(&mut vcx, LineNumbers::Relative);
        assert_eq!(
            texts(&mut vcx),
            ["1", "2", "1", "2", "3"],
            "cursor on the package (row 2): its own number, then distances"
        );
        h.dispatch(&mut vcx, "down", Some(2));
        assert_eq!(
            texts(&mut vcx),
            ["3", "2", "1", "4", "1"],
            "a move re-numbers"
        );
        h.dispatch(&mut vcx, "bottom", Some(2));
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(1));
        assert_eq!(texts(&mut vcx)[1], "2", "`2G` lands on the row numbered 2");

        set(&mut vcx, LineNumbers::Off);
        assert!(
            vcx.debug_bounds("pricer-gutter-0").is_none(),
            "off again on the next draw"
        );
        assert!(
            (bounds(&mut vcx, "pricer-cell-0-1").left() - value.left()).abs() < gpui::px(0.5),
            "and the tree column gave the width back"
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
            Some(
                "pricer 'vendor' is not built into this binary; set [pricing] adapter and restart"
            )
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

    /// Entry-bar spec §4: `o`, a line, `enter` adds a row below the
    /// cursor and keeps the bar open; the next `enter` lands below that.
    #[gpui::test]
    fn o_then_lines_then_enter_adds_each_below_the_last_and_keeps_the_bar_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "add_below", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(focused(&mut vcx), "the field owns focus");
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("at end"));
        typed(&h, &mut vcx, "-5 SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 1);
        let batches = h.prices();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].lines.len(), 1);
        assert_eq!(h.mode(&mut vcx), "insert", "the bar stays open");
        assert_eq!(h.entry_text(&vcx).as_deref(), Some(""));
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("at end"));
        typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 4, "a package with its two legs");
        assert_eq!(
            h.tree(&vcx).len(),
            4,
            "the typed package opens; no placeholder row"
        );
        assert_eq!(
            h.cursor(&vcx).map(|c| c.0),
            Some(1),
            "the cursor is on what landed"
        );
    }

    #[gpui::test]
    fn o_lands_below_the_cursor_row_and_the_label_says_so(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("after SPX Z26 5000 C"));
        typed(&h, &mut vcx, "SPX Z26 3000 P");
        h.dispatch(&mut vcx, "commit", None);
        let second = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(1));
        assert_eq!(second, "SPX Z26 3000 P", "below row 0, above the package");
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("after SPX Z26 3000 P"));
    }

    #[gpui::test]
    fn a_parse_error_shows_under_the_field_keeps_the_text_and_typing_clears_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 5000 CX");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.sheet_len(&vcx), 0);
        assert_eq!(h.mode(&mut vcx), "insert");
        let error = h.entry_error(&vcx).expect("the reason is under the field");
        assert!(error.ends_with("(column 14)"), "{error}");
        assert_eq!(h.footer(&vcx), None, "not in the footer");
        assert!(vcx.debug_bounds("pricer-entry-error").is_some());
        assert_eq!(h.entry_text(&vcx).as_deref(), Some("SPX Z26 5000 CX"));
        typed(&h, &mut vcx, "\u{8}");
        assert_eq!(h.entry_error(&vcx), None, "an edit answers the error");
    }

    #[gpui::test]
    fn a_history_step_clears_the_error(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "nonsense");
        h.dispatch(&mut vcx, "commit", None);
        assert!(h.entry_error(&vcx).is_some());
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.entry_error(&vcx), None);
    }

    #[gpui::test]
    fn o_on_a_leg_inserts_the_next_leg(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "expand", None);
        h.dispatch(&mut vcx, "down", None); // the first leg
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        let legs = h.tile.read_with(&vcx, |t, _| t.sheet.children(1).len());
        assert_eq!(legs, 3);
        let middle = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(3));
        assert_eq!(middle, "SPX Z26 5000 C", "between the two legs");
    }

    #[gpui::test]
    fn a_package_typed_at_a_leg_place_is_refused_under_the_field(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "add_below", None); // a package row: its first leg
        assert_eq!(h.entry_label(&vcx).as_deref(), Some("into CS"));
        typed(&h, &mut vcx, "SPX Z26 4800/5200 CS");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.entry_error(&vcx).as_deref(),
            Some("a package cannot hold a package")
        );
        assert_eq!(
            h.entry_label(&vcx).as_deref(),
            Some("into CS"),
            "the place is restored"
        );
        assert_eq!(h.mode(&mut vcx), "insert");
    }

    #[gpui::test]
    fn a_palette_add_while_the_bar_is_open_reopens_it_at_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "half typed");
        h.dispatch(&mut vcx, "add_below", None);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(focused(&mut vcx));
        assert_eq!(h.entry_text(&vcx).as_deref(), Some(""));
    }

    /// A refused insert puts the place back even when the refused spec
    /// would have advanced it. `place_for` answers no such place today
    /// (its one reachable refusal, a package at a leg place, never
    /// advances), so the place is planted: a root boundary inside a
    /// package's leg run, which `apply` refuses.
    #[gpui::test]
    fn a_refused_line_puts_the_place_back(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        h.tile.update(&mut vcx, |t, _| {
            t.entry.as_mut().unwrap().place = Place::Root { at: 2 };
        });
        typed(&h, &mut vcx, "SPX Z26 3000 P");
        h.dispatch(&mut vcx, "commit", None);
        assert!(h.entry_error(&vcx).is_some(), "refused");
        let place = h
            .tile
            .read_with(&vcx, |t, _| t.entry.as_ref().unwrap().place);
        assert_eq!(place, Place::Root { at: 2 });
    }

    #[gpui::test]
    fn up_and_down_walk_the_sheets_own_lines_newest_first(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        let text = |vcx: &VisualTestContext| h.entry_text(vcx).unwrap();
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
    fn escape_closes_the_bar_and_the_field_blurs_before_it_drops(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        assert!(focused(&mut vcx));
        h.dispatch(&mut vcx, "cancel", None);
        assert!(!focused(&mut vcx), "blurred, then dropped (CLAUDE.md)");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!painted(&mut vcx, "pricer-entry"), "the bar is gone");
        assert_eq!(h.sheet_len(&vcx), 5, "the bar never held a row");
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
            Some(Editor::Date { .. }) | None => None,
        })
    }

    fn set_editor(h: &Harness, vcx: &mut VisualTestContext, text: &str) {
        let text = text.to_string();
        vcx.update(|window, cx| {
            let input = match &h.tile.read(cx).editor {
                Some(Editor::Text { input, .. } | Editor::Choice { input, .. }) => input.clone(),
                Some(Editor::Date { .. }) | None => panic!("a text editor is open"),
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
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor.col),
            1,
            "the cursor sits on the editor's column"
        );
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

    /// The in-cell editor paints no field chrome: its text sits flush
    /// against the cell's own content edge (inside the cursor's 1px
    /// border), right-aligned in a numeric cell and left-aligned in a
    /// text one, and inside the row's height. A bordered, padded `Input`
    /// insets its text by its own padding and border.
    #[gpui::test]
    fn the_cell_editor_paints_no_chrome_and_its_text_sits_where_the_cells_did(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let text_bounds = |h: &Harness, vcx: &mut VisualTestContext| {
            h.draw(vcx);
            h.tile.read_with(vcx, |t, cx| {
                let input = t
                    .editor
                    .as_ref()
                    .and_then(|e| e.input())
                    .expect("a text editor is open")
                    .clone();
                input.read(cx).text_bounds().expect("the field was painted")
            })
        };
        // Strike: right-aligned. Table column = plan column + 1.
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        let cell = vcx.debug_bounds("pricer-cell-0-4").expect("strike cell");
        let text = text_bounds(&h, &mut vcx);
        let border = gpui::px(1.0);
        assert!(
            (text.right() - (cell.right() - border)).abs() < gpui::px(0.5),
            "right-aligned text ends at the cell's edge: {text:?} in {cell:?}"
        );
        assert!(text.top() >= cell.top() && text.bottom() <= cell.bottom());
        h.dispatch(&mut vcx, "cancel", None);
        // Underlying: a left-aligned typeahead field.
        h.dispatch(&mut vcx, "first_col", None);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "edit", None);
        let cell = vcx
            .debug_bounds("pricer-cell-0-2")
            .expect("underlying cell");
        let text = text_bounds(&h, &mut vcx);
        assert!(
            (text.left() - (cell.left() + border)).abs() < gpui::px(0.5),
            "left-aligned text starts at the cell's edge: {text:?} in {cell:?}"
        );
    }

    /// The bar paints between the header and the column headers.
    #[gpui::test]
    fn the_entry_bar_paints_between_the_header_and_the_table(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "add_below", None);
        h.draw(&mut vcx);
        let bar = vcx
            .debug_bounds("pricer-entry")
            .expect("the bar is painted");
        let th = vcx
            .debug_bounds("pricer-th-1")
            .expect("the headers are painted");
        assert!(bar.bottom() <= th.top(), "{bar:?} above {th:?}");
        let field = vcx.debug_bounds("pricer-entry-field").expect("the field");
        let label = vcx.debug_bounds("pricer-entry-label").expect("the label");
        assert!(label.right() <= field.left(), "the label leads the field");
    }

    // ---- the expiry date field ----

    /// A dated line (`Z26` = 2026-12-18) and a tenor line. Expiry is plan
    /// column 2 (table column 3).
    const DATED: [&str; 2] = ["SPX Z26 5000 C", "NDX 3m 100% C"];

    fn ymd(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn expiry_of(h: &Harness, vcx: &VisualTestContext, row: usize) -> Expiry {
        h.tile.read_with(vcx, |t, _| {
            t.sheet.instrument(row).unwrap().expiry().clone()
        })
    }

    /// The open date field's painted segments and active segment.
    fn date_field(h: &Harness, vcx: &VisualTestContext) -> Option<(Vec<String>, Segment)> {
        h.tile.read_with(vcx, |t, _| {
            t.date_field().map(|f| {
                (
                    f.segments().iter().map(|s| s.text.to_string()).collect(),
                    f.segment(),
                )
            })
        })
    }

    /// Real keystrokes into the focused field.
    fn keys(h: &Harness, vcx: &mut VisualTestContext, keys: &str) {
        vcx.simulate_keystrokes(keys);
        vcx.run_until_parked();
        h.draw(vcx);
    }

    fn open_expiry(h: &Harness, vcx: &mut VisualTestContext, row: usize) {
        if row > 0 {
            h.dispatch(vcx, "down", Some(row as u32));
        }
        h.dispatch(vcx, "right", Some(2));
        h.dispatch(vcx, "edit", None);
        h.draw(vcx);
    }

    fn can_undo(h: &Harness, vcx: &VisualTestContext) -> bool {
        h.tile.read_with(vcx, |t, _| t.undo.can_undo())
    }

    #[gpui::test]
    fn an_expiry_edits_in_a_date_field_and_enter_commits_one_undoable_edit(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        open_expiry(&h, &mut vcx, 0);
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the shell's insert-focus predicate sees the date field"
        );
        assert_eq!(
            date_field(&h, &vcx),
            Some((vec!["2026".into(), "12".into(), "18".into()], Segment::Day)),
            "seeded from the line's own date, on the day"
        );
        keys(&h, &mut vcx, "up");
        keys(&h, &mut vcx, "left 1 1");
        assert_eq!(
            date_field(&h, &vcx),
            Some((vec!["2026".into(), "11".into(), "19".into()], Segment::Day)),
            "up stepped the day, two digits typed the month"
        );
        keys(&h, &mut vcx, "enter");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!focused(&mut vcx), "blurred, then dropped");
        assert_eq!(expiry_of(&h, &vcx, 0), Expiry::Date(ymd(2026, 11, 19)));
        assert!(can_undo(&h, &vcx), "a date commit is one undo entry");
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(expiry_of(&h, &vcx, 0), Expiry::Date(ymd(2026, 12, 18)));
    }

    /// A tenor has no date the pricer could seed (it never resolves
    /// one): the field opens on today by the app clock, says so, and a
    /// commit turns the line into a date expiry.
    #[gpui::test]
    fn a_tenor_opens_on_the_app_clocks_today_and_enter_makes_it_a_date(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        let clock = geode_core::clock::Clock::in_zone_named("Pacific/Kiritimati");
        vcx.update(|_, cx| cx.set_global(geode_shell::clock::AppClock(clock)));
        open_expiry(&h, &mut vcx, 1);
        let today = clock.today(chrono::Utc::now());
        assert_eq!(
            date_field(&h, &vcx).map(|(s, _)| s),
            Some(vec![
                format!("{:04}", today.year()),
                format!("{:02}", today.month()),
                format!("{:02}", today.day()),
            ])
        );
        let footer = h.footer(&vcx).unwrap_or_default();
        assert!(
            footer.contains("3m") && footer.contains("today"),
            "{footer}"
        );
        keys(&h, &mut vcx, "enter");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(expiry_of(&h, &vcx, 1), Expiry::Date(today));
        assert!(can_undo(&h, &vcx));
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(expiry_of(&h, &vcx, 1), Expiry::Tenor("3m".into()));
    }

    #[gpui::test]
    fn escape_leaves_a_tenor_untouched_and_blurs_the_field(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        open_expiry(&h, &mut vcx, 1);
        assert!(focused(&mut vcx), "fixture: the field owns focus");
        keys(&h, &mut vcx, "up");
        keys(&h, &mut vcx, "escape");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!focused(&mut vcx), "blurred, then dropped");
        assert_eq!(expiry_of(&h, &vcx, 1), Expiry::Tenor("3m".into()));
        assert!(!can_undo(&h, &vcx), "nothing was applied");
    }

    /// A commit that lands on the line's own date applies nothing: no
    /// undo entry, no reprice.
    #[gpui::test]
    fn an_unchanged_date_commit_is_no_edit(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        let _ = h.prices();
        open_expiry(&h, &mut vcx, 0);
        keys(&h, &mut vcx, "up");
        keys(&h, &mut vcx, "down");
        keys(&h, &mut vcx, "enter");
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(expiry_of(&h, &vcx, 0), Expiry::Date(ymd(2026, 12, 18)));
        assert!(!can_undo(&h, &vcx), "an unchanged commit records no undo");
        assert!(h.prices().is_empty(), "and asks for no price");
    }

    #[gpui::test]
    fn a_half_typed_segment_refuses_the_commit_and_names_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        open_expiry(&h, &mut vcx, 0);
        keys(&h, &mut vcx, "0");
        keys(&h, &mut vcx, "enter");
        assert_eq!(h.mode(&mut vcx), "insert", "the field stays open");
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("finish the day or backspace")
        );
        assert_eq!(expiry_of(&h, &vcx, 0), Expiry::Date(ymd(2026, 12, 18)));
    }

    /// The field paints flush in its cell (no chrome), a click on a
    /// segment selects it and leaves the field open, and a click on
    /// another cell cancels it like the text editor.
    #[gpui::test]
    fn a_segment_click_selects_it_and_a_click_elsewhere_cancels(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        open_expiry(&h, &mut vcx, 0);
        let cell = vcx.debug_bounds("pricer-cell-0-3").expect("expiry cell");
        let year = vcx
            .debug_bounds("pricer-date-seg-5-0")
            .expect("the year segment is painted");
        assert!(
            (year.left() - (cell.left() + gpui::px(1.0))).abs() < gpui::px(0.5),
            "flush against the cell's edge: {year:?} in {cell:?}"
        );
        assert!(
            year.top() >= cell.top() && year.bottom() <= cell.bottom(),
            "{year:?} in {cell:?}"
        );
        click_at(&mut vcx, year.center(), 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "insert", "the click did not cancel");
        assert_eq!(date_field(&h, &vcx).map(|(_, s)| s), Some(Segment::Year));
        keys(&h, &mut vcx, "up");
        assert_eq!(
            date_field(&h, &vcx).map(|(s, _)| s[0].clone()),
            Some("2027".into())
        );
        let at = centre_of(&mut vcx, "pricer-cell-1-4");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal", "a click elsewhere cancels");
        assert!(
            !vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the field no longer holds focus"
        );
        assert_eq!(expiry_of(&h, &vcx, 0), Expiry::Date(ymd(2026, 12, 18)));
    }

    /// A text commit that parses to the value the cell already holds is
    /// no edit — `5000.0` is the strike `5000` — so no undo entry and no
    /// reprice; the editor closes as after any commit.
    #[gpui::test]
    fn an_unchanged_text_commit_is_no_edit(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let _ = h.prices();
        h.dispatch(&mut vcx, "right", Some(3)); // strike
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5000.0");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&mut vcx), "normal", "the editor closed");
        assert!(!focused(&mut vcx), "blurred, then dropped");
        assert!(!can_undo(&h, &vcx), "an unchanged commit records no undo");
        assert!(h.prices().is_empty(), "and asks for no price");
        h.dispatch(&mut vcx, "edit", None);
        set_editor(&h, &mut vcx, "5100");
        h.dispatch(&mut vcx, "commit", None);
        assert!(
            can_undo(&h, &vcx),
            "fixture: a changed commit still records"
        );
    }

    /// The tenor note stands while the field is open — through a stepped
    /// segment and a refused commit — and goes when the field closes.
    #[gpui::test]
    fn the_tenor_note_stands_until_the_field_closes(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        open_expiry(&h, &mut vcx, 1);
        let note = h.footer(&vcx).expect("the tenor note");
        keys(&h, &mut vcx, "up");
        assert_eq!(h.footer(&vcx), Some(note.clone()), "a key keeps the note");
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(
            h.footer(&vcx),
            Some(note.clone()),
            "so does the fragment's step"
        );
        keys(&h, &mut vcx, "0 enter");
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("finish the day or backspace")
        );
        keys(&h, &mut vcx, "backspace");
        assert_eq!(
            h.footer(&vcx),
            Some(note),
            "the refusal retires to the note"
        );
        keys(&h, &mut vcx, "escape");
        assert_eq!(h.footer(&vcx), None, "cancel drops it");
        assert_eq!(expiry_of(&h, &vcx, 1), Expiry::Tenor("3m".into()));
    }

    /// A reload that moves the expiry column carries the open date field
    /// with it; its commit writes the expiry.
    #[gpui::test]
    fn an_open_date_field_follows_its_column_through_a_view_reload(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        open_expiry(&h, &mut vcx, 0);
        assert_eq!(editor_paint_col(&h, &vcx), Some(2), "fixture");
        let views = slim_views("\"expiry\", \"qty\"");
        vcx.update(|_, cx| {
            h.factory
                .reload(views, None, std::time::Duration::from_secs(60), cx)
        });
        vcx.run_until_parked();
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "insert", "the field stays open");
        assert_eq!(editor_paint_col(&h, &vcx), Some(0), "expiry's new column");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor.col), 0);
        assert!(
            vcx.debug_bounds("pricer-date-seg-5-0").is_some(),
            "painted in its new cell"
        );
        keys(&h, &mut vcx, "up enter");
        assert_eq!(h.footer(&vcx), None, "the commit was not refused");
        assert_eq!(expiry_of(&h, &vcx, 0), Expiry::Date(ymd(2026, 12, 19)));
    }

    /// A reload without the expiry column closes the open date field with
    /// MOVED; its focus handle is released (the deferred blur).
    #[gpui::test]
    fn a_view_reload_without_the_expiry_closes_the_date_field(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &DATED);
        open_expiry(&h, &mut vcx, 0);
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
        assert_eq!(expiry_of(&h, &vcx, 0), Expiry::Date(ymd(2026, 12, 18)));
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
        assert!(h.header(&vcx).contains(&"spot +2.0%".to_string()));
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
    fn the_menu_opens_steps_and_picks_and_skips_a_disabled_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 12.5);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&mut vcx), "menu");
        // Rows: Price all, Group, Ungroup, Undo, Redo, Delete row, then
        // views. Ungroup (A is not in a package), Undo and Redo are
        // greyed, so the second step lands on Delete row. (A pick on a
        // greyed row is the pointer's:
        // `a_pointer_over_a_disabled_menu_row_lands_without_a_fill`.)
        h.dispatch(&mut vcx, "menu_down", Some(2));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.menu.as_ref().map(|m| m.highlighted)),
            Some(8),
            "over the three greyed rows onto Delete row"
        );
        h.dispatch(&mut vcx, "menu_close", None);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_pick", None); // Price all
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.prices()[0].lines.len(), 4);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_down", Some(4)); // the second view: barrier
        h.dispatch(&mut vcx, "menu_pick", None);
        assert!(h.columns(&vcx).contains(&"barrier".to_string()));
    }

    /// A load answer refreshes an open menu's availability. Delete becomes enabled when
    /// a row arrives, and selecting it uses the ordinary delete route.
    #[gpui::test]
    fn a_load_answer_under_an_open_menu_rechecks_its_rows(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&["SPX Z26 5000 C"]);
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.dispatch(&mut vcx, "menu", None);
        // Delete row is greyed while loading, so `j` would step over it:
        // the pointer puts the highlight there.
        let at = centre_of(&mut vcx, "pricer-menu-row-8");
        vcx.simulate_mouse_move(at, None, gpui::Modifiers::default());
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
            Some(8),
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
                    MenuItem::View { name, current } => Some(format!("{name} {current}")),
                    _ => None,
                })
                .collect();
            (views, m.highlighted)
        });
        assert_eq!(views, vec!["slim false"]);
        assert_eq!(highlighted, 11, "clamped to the last row");
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
            "rm gone",
            "name fresh",
            "new",
            "e book",
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

    /// At quit the app flushes every tile before the data service stops:
    /// a sheet still waiting on its idle timer, or whose last save failed,
    /// is saved now; a clean sheet writes nothing.
    #[gpui::test]
    fn flush_all_saves_every_unsaved_sheet_now(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let base = h.store.save_count();
        vcx.update(|_, cx| h.factory.flush_all(cx));
        assert_eq!(
            h.store.save_count(),
            base,
            "a clean sheet has nothing to flush"
        );
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        vcx.update(|_, cx| h.factory.flush_all(cx));
        assert_eq!(
            h.store.save_count(),
            base + 1,
            "saved without the idle wait"
        );
        assert_eq!(stored(&h).qty(0), 2);
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(
            h.store.save_count(),
            base + 1,
            "the idle save is not repeated"
        );
        vcx.update(|_, cx| h.factory.save_answered("book", Err("disk full".into()), cx));
        vcx.update(|_, cx| h.factory.flush_all(cx));
        assert_eq!(h.store.save_count(), base + 2, "a failed save is retried");
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
        assert_eq!(stored(&h).qty(0), 4);
        // The notice clears when the queued save is confirmed landed.
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert_eq!(h.save_notice(&vcx), None);
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
            title, "Pricer · book",
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

    /// Undo restores a deleted package's open state and selects the restored package.
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

    /// Restoring a leg opens its parent so the restored cursor target is visible.
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

    /// Removing the refused request's stale lines ends the retry streak on the next
    /// submit, instead of leaving a retry notice with no work to send.
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

    /// Current-revision in-flight entries suppress redundant submissions. Changing one
    /// line's revision submits a new batch containing every stale line.
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

    /// Three roots A, B, C: strikes 5000, 4000, 3000.
    const THREE_LINES: [&str; 3] = ["SPX Z26 5000 C", "SPX Z26 4000 P", "SPX Z26 3000 P"];

    // ---- clicks while the bar is open ----

    #[gpui::test]
    fn a_click_on_a_row_while_the_bar_is_open_closes_it_and_lands_there(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "add_below", None);
        let at = centre_of(&mut vcx, "pricer-cell-2-2");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2));
    }

    #[gpui::test]
    fn a_double_click_on_a_row_while_the_bar_is_open_edits_that_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &THREE_LINES);
        h.dispatch(&mut vcx, "add_below", None);
        let at = centre_of(&mut vcx, "pricer-cell-2-2");
        click_at(&mut vcx, at, 2);
        h.draw(&mut vcx);
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(2));
        assert_eq!(h.mode(&mut vcx), "insert", "the cell editor opened");
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.entry.is_none() && t.editor.is_some())
        );
    }

    // Empty state, labels, action menu, and pointer trigger.

    fn empty_text(h: &Harness, vcx: &VisualTestContext) -> &'static str {
        h.tile
            .read_with(vcx, |t, cx| t.table.read(cx).delegate().empty_text())
    }

    fn painted(vcx: &mut VisualTestContext, selector: &'static str) -> bool {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.debug_bounds(selector).is_some()
    }

    /// Empty-table text distinguishes a pending load from a sheet ready for entry.
    /// Installing a model mirrors loading state.
    #[gpui::test]
    fn an_empty_table_names_the_next_action_or_that_it_is_loading(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert_eq!(empty_text(&h, &vcx), "No lines — press o to add one");
        assert!(painted(&mut vcx, "pricer-empty"));
        h.dispatch(&mut vcx, "add_below", None);
        assert!(
            painted(&mut vcx, "pricer-empty"),
            "the bar is not a row; the empty text stays"
        );

        let (store, record) = seeded(&["SPX Z26 5000 C"]);
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        assert_eq!(empty_text(&h, &vcx), "Loading sheet…");
        assert!(painted(&mut vcx, "pricer-empty"));
        h.tile
            .update(&mut vcx, |t, cx| t.loaded(Ok(Some(rows)), cx));
        assert_eq!(empty_text(&h, &vcx), "No lines — press o to add one");
        assert!(!painted(&mut vcx, "pricer-empty"), "the row arrived");
    }

    /// Headers read as words carrying their unit; the table keys stay
    /// the vocabulary's names; both bundled views end in `status`.
    #[gpui::test]
    fn columns_are_labelled_in_words_and_the_views_show_status(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.command(&mut vcx, "view barrier").unwrap();
        let labels = h.labels(&vcx);
        for want in ["barrier type", "spot %", "vol pt", "status"] {
            assert!(labels.contains(&want.to_string()), "{want}: {labels:?}");
        }
        assert!(!labels.iter().any(|l| l.contains('_')), "{labels:?}");
        assert_eq!(h.columns(&vcx).last().map(String::as_str), Some("status"));
        assert_eq!(h.cell(&vcx, 0, "status"), "pricing…");
    }

    fn menu_rows(h: &Harness, vcx: &VisualTestContext) -> Vec<String> {
        h.tile.read_with(vcx, |t, _| {
            t.menu
                .as_ref()
                .expect("the menu is open")
                .items
                .iter()
                .map(|i| match i {
                    MenuItem::Action {
                        title,
                        hint,
                        enabled,
                        ..
                    } => match enabled {
                        Ok(()) => format!("{title} | {hint}"),
                        Err(why) => format!("{title} | ({why})"),
                    },
                    MenuItem::View { name, current } => {
                        format!("{} {name}", if *current { "✓" } else { " " })
                    }
                    MenuItem::Separator => "—".into(),
                    MenuItem::Section(s) => format!("[{s}]"),
                })
                .collect()
        })
    }

    /// Menu actions share palette titles, with default keys or disabled reasons in the
    /// trailing lane. Separators and the View heading are skipped by selection; current
    /// views show a leading tick.
    #[gpui::test]
    fn the_menu_groups_its_rows_names_keys_and_says_why_a_row_is_disabled(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(
            menu_rows(&h, &vcx),
            vec![
                "Reprice all lines | :price",
                "—",
                "Group into package | g p",
                "Ungroup package | (not in a package)",
                "—",
                "Undo | (nothing to undo)",
                "Redo | (nothing to redo)",
                "—",
                "Delete row | d d",
                "—",
                "[View]",
                "✓ vanilla",
                "  barrier",
            ]
        );
        for (id, title) in crate::content::ACTIONS {
            if ["pricer::price", "pricer::group", "pricer::ungroup"].contains(id) {
                assert!(
                    menu_rows(&h, &vcx)
                        .iter()
                        .any(|r| r.starts_with(&format!("{title} |"))),
                    "{id}: the menu says the palette's '{title}'"
                );
            }
        }
        h.dispatch(&mut vcx, "menu_down", Some(1));
        let at = h
            .tile
            .read_with(&vcx, |t, _| t.menu.as_ref().map(|m| m.highlighted));
        assert_eq!(at, Some(2), "over the separator onto Group");
        h.dispatch(&mut vcx, "menu_down", Some(1));
        let at = h
            .tile
            .read_with(&vcx, |t, _| t.menu.as_ref().map(|m| m.highlighted));
        assert_eq!(at, Some(8), "over the greyed Ungroup, Undo and Redo");
        h.dispatch(&mut vcx, "menu_down", Some(1));
        let at = h
            .tile
            .read_with(&vcx, |t, _| t.menu.as_ref().map(|m| m.highlighted));
        assert_eq!(at, Some(11), "over the section header onto a view");
    }

    /// A real pointer move over a menu row moves the highlight there (the
    /// mouse form of `j`/`k`), so the pointer's row and the highlighted
    /// row are one row; `enter` then picks it.
    #[gpui::test]
    fn a_pointer_move_over_a_menu_row_moves_the_highlight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "menu", None);
        let at = centre_of(&mut vcx, "pricer-menu-row-12"); // barrier
        vcx.simulate_mouse_move(at, None, gpui::Modifiers::default());
        let highlighted = h
            .tile
            .read_with(&vcx, |t, _| t.menu.as_ref().map(|m| m.highlighted));
        assert_eq!(highlighted, Some(12));
        h.dispatch(&mut vcx, "menu_pick", None);
        assert!(h.columns(&vcx).contains(&"barrier".to_string()));
    }

    /// A pointer move over a typeahead row moves its highlight — and
    /// does not count as the trader choosing it (`moved` stays unset, so
    /// a free list's `enter` still commits the typed text: the `HSI` →
    /// `HSCEI` guard).
    #[gpui::test]
    fn a_pointer_move_over_a_typeahead_row_moves_its_highlight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "right", Some(4)); // type: C, P
        h.dispatch(&mut vcx, "edit", None);
        let at = centre_of(&mut vcx, "pricer-choice-row-1");
        vcx.simulate_mouse_move(at, None, gpui::Modifiers::default());
        let (highlighted, moved) = h.tile.read_with(&vcx, |t, _| match &t.editor {
            Some(Editor::Choice { list, moved, .. }) => (list.highlighted(), *moved),
            _ => panic!("the typeahead is open"),
        });
        assert_eq!(highlighted, 1);
        assert!(!moved, "a hover is not a choice");
        let painted = h.tile.read_with(&vcx, |t, cx| {
            t.table
                .read(cx)
                .delegate()
                .editor
                .as_ref()
                .and_then(|e| e.choice.as_ref().map(|c| c.highlighted))
        });
        assert_eq!(painted, Some(1), "the delegate's copy follows");
    }

    /// The header's `⋯` is the pointer's `.`: a click opens the menu, a
    /// second click closes it (the capture-phase toggle runs ahead of
    /// the menu's own outside-click close).
    #[gpui::test]
    fn the_header_trigger_toggles_the_menu(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let at = centre_of(&mut vcx, "pricer-menu-button");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "menu");
        let at = centre_of(&mut vcx, "pricer-menu-button");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.mode(&mut vcx), "normal", "a second click closes it");
    }

    /// A pointer-highlighted disabled action paints no fill. Picking it reports its
    /// reason and keeps the menu open; keyboard stepping skips disabled actions.
    #[gpui::test]
    fn a_pointer_over_a_disabled_menu_row_lands_without_a_fill(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.dispatch(&mut vcx, "menu", None);
        let at = centre_of(&mut vcx, "pricer-menu-row-3"); // Ungroup: A is a root line
        vcx.simulate_mouse_move(at, None, gpui::Modifiers::default());
        let (highlighted, enabled, paint, paints) = h.tile.read_with(&vcx, |t, cx| {
            let m = t.menu.as_ref().expect("the menu is open");
            let enabled = matches!(
                m.items[m.highlighted],
                MenuItem::Action {
                    enabled: Ok(()),
                    ..
                }
            );
            let paints = t.table.read(cx).delegate().paints;
            let paint = crate::popup::menu_row_paint(true, enabled, &paints, cx.theme().accent);
            (m.highlighted, enabled, paint, paints)
        });
        assert_eq!(highlighted, 3, "the pointer's row takes the highlight");
        assert!(!enabled, "fixture: Ungroup is disabled here");
        assert_eq!(paint.fill, None, "no fill on a disabled row");
        assert_eq!(paint.text, paints.menu_muted);
        let at = centre_of(&mut vcx, "pricer-menu-row-3");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.footer(&vcx).as_deref(), Some("not in a package"));
        assert_eq!(h.mode(&mut vcx), "menu", "a refused pick keeps the menu");
    }

    // ---- the load and save lifecycle over the store's answers ----

    /// A load answer as the data tier delivers it: `rows` in the shape a
    /// document read returns, addressed to this tile under `tag`.
    fn answer_load(
        h: &Harness,
        vcx: &mut VisualTestContext,
        tag: u64,
        snapshot: Result<geode_core::snapshot::Snapshot, String>,
    ) {
        let outcome = geode_core::query::QueryOutcome {
            key: QueryKey(TILE),
            tag,
            snapshot: snapshot.map(std::sync::Arc::new),
            submitted: std::time::Instant::now(),
        };
        vcx.update(|window, cx| h.content.deliver(Delivery::Query(outcome), window, cx));
    }

    fn snapshot(rows: &geode_core::document::DocumentRows) -> geode_core::snapshot::Snapshot {
        crate::core::storage::tests::snapshot_of(rows, |_| {})
    }

    /// Ids 1 (A), 2 (the package), 3–4 (legs), 5 (B); the record holds
    /// the cursor on B and the package open. The store answers `Pending`.
    fn pending_book() -> (
        MemorySheetStore,
        toml::Table,
        geode_core::document::DocumentRows,
    ) {
        let (store, mut record) = seeded(&[
            "SPX Z26 5000 C",
            "-5 SPX Z26 4800/5200 CS",
            "SPX Z26 4000 P",
        ]);
        record.insert("cursor".into(), toml::Value::Integer(5));
        record.insert(
            "expanded".into(),
            toml::Value::Array(vec![toml::Value::Integer(2)]),
        );
        let rows = store.get("book").unwrap();
        store.set_pending(true);
        (store, record, rows)
    }

    #[gpui::test]
    fn a_query_answer_under_the_latest_load_tag_installs_the_sheet_and_an_older_one_is_ignored(
        cx: &mut gpui::TestAppContext,
    ) {
        let (store, record, rows) = pending_book();
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        assert_eq!(
            h.store.loads(),
            vec![("book".to_string(), QueryKey(TILE), 1)],
            "one load, and the first show does not ask again"
        );
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));
        assert!(h.prices().is_empty(), "nothing prices while loading");

        // An answer under any other tag is not this load's.
        answer_load(&h, &mut vcx, 0, Ok(snapshot(&rows)));
        answer_load(&h, &mut vcx, 2, Ok(snapshot(&rows)));
        assert_eq!(h.sheet_len(&vcx), 0);
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));

        answer_load(&h, &mut vcx, 1, Ok(snapshot(&rows)));
        assert_eq!(h.sheet_len(&vcx), 5, "decoded and installed");
        assert!(h.notice(&vcx).is_none());
        assert_eq!(h.tree(&vcx).len(), 5, "the held expansion applied");
        assert_eq!(h.cursor(&vcx).map(|c| c.0), Some(4), "the held cursor");
        assert!(!h.prices().is_empty(), "the installed sheet reprices");
        assert_eq!(h.save_notice(&vcx), None);

        // A repeat of the answered tag, even a failure, changes nothing.
        answer_load(&h, &mut vcx, 1, Err("late".into()));
        assert_eq!(h.save_notice(&vcx), None);
        assert_eq!(h.sheet_len(&vcx), 5);
    }

    #[gpui::test]
    fn a_failed_or_undecodable_load_answer_blocks_saves(cx: &mut gpui::TestAppContext) {
        let (store, record, rows) = pending_book();
        let (h, mut vcx) = open_full(cx, Some(record.clone()), store, PricerSettings::default());
        answer_load(&h, &mut vcx, 1, Err("boom".into()));
        assert_eq!(
            h.save_notice(&vcx).as_deref(),
            Some("sheet 'book' did not load (boom); edits are not saved")
        );

        let store = MemorySheetStore::default();
        store.set_pending(true);
        let (h2, mut vcx2) = open_full(cx, Some(record), store, PricerSettings::default());
        let bad = crate::core::storage::tests::snapshot_of(&rows, |cols| {
            cols.retain(|(m, _)| m.name != "strike")
        });
        answer_load(&h2, &mut vcx2, 1, Ok(bad));
        let notice = h2.save_notice(&vcx2).expect("a decode failure says so");
        assert!(
            notice.starts_with("sheet 'book' did not load (") && notice.contains("strike"),
            "{notice}"
        );
        assert_eq!(h2.sheet_len(&vcx2), 0, "never a half-sheet");
        assert!(h2.tile.read_with(&vcx2, |t, _| t.save_blocked));
    }

    /// A hide cancels by key, which cancels the pending load too: the
    /// next show asks again under a fresh tag, and only that tag installs.
    #[gpui::test]
    fn a_load_cancelled_by_a_hide_is_resubmitted_on_show_under_a_fresh_tag(
        cx: &mut gpui::TestAppContext,
    ) {
        let (store, record, rows) = pending_book();
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        h.visible(&mut vcx, false);
        assert!(
            h.requests()
                .iter()
                .any(|r| matches!(r, Request::Cancel { key } if *key == QueryKey(TILE))),
            "the hide cancelled by key"
        );
        assert_eq!(h.store.loads().len(), 1, "a hide asks for nothing");
        h.visible(&mut vcx, true);
        assert_eq!(
            h.store.loads(),
            vec![
                ("book".to_string(), QueryKey(TILE), 1),
                ("book".to_string(), QueryKey(TILE), 2),
            ]
        );
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));
        answer_load(&h, &mut vcx, 1, Ok(snapshot(&rows)));
        assert_eq!(h.sheet_len(&vcx), 0, "the cancelled load's answer is stale");
        answer_load(&h, &mut vcx, 2, Ok(snapshot(&rows)));
        assert_eq!(h.sheet_len(&vcx), 5);
        assert_eq!(
            h.tree(&vcx).len(),
            5,
            "the held expansion survives the resubmit"
        );
        // Loaded: a later hide and show asks for nothing.
        h.visible(&mut vcx, false);
        h.visible(&mut vcx, true);
        assert_eq!(h.store.loads().len(), 2);
    }

    /// Decision 15: nothing prices while loading, so a standing refusal
    /// streak would say `REFUSED` over a load with no retry to end it —
    /// proved through `:e`, the production route into a load with a
    /// streak standing.
    #[gpui::test]
    fn a_load_starting_clears_a_refusal_streak(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 1.0);
        h.close_channel();
        h.dispatch(&mut vcx, "price", None);
        assert_eq!(h.notice(&vcx).as_deref(), Some(REFUSED));
        h.store.set_pending(true);
        h.command(&mut vcx, "e other").unwrap();
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));
        assert!(h.tile.read_with(&vcx, |t, _| t.retry_task.is_none()));
    }

    #[gpui::test]
    fn a_refused_load_at_restore_blocks_saves_and_names_the_refusal(cx: &mut gpui::TestAppContext) {
        let (store, record) = seeded(&BOOK);
        let good = store.get("book").unwrap();
        store.set_load_refused(true);
        let base = store.save_count();
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        assert_eq!(
            h.save_notice(&vcx),
            Some(blocked_notice("book", LOAD_REFUSED).to_string())
        );
        assert_ne!(
            h.notice(&vcx).as_deref(),
            Some(LOADING),
            "nothing is coming"
        );
        h.command(&mut vcx, "refresh off").unwrap();
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(
            h.store.save_count(),
            base,
            "the fallback is never published"
        );
        assert_eq!(h.store.get("book"), Some(good));
    }

    fn save_answered(
        h: &Harness,
        vcx: &mut VisualTestContext,
        sheet: &str,
        answer: Result<(), String>,
    ) {
        vcx.update(|_, cx| h.factory.save_answered(sheet, answer, cx));
    }

    fn dirty(h: &Harness, vcx: &VisualTestContext) -> bool {
        h.tile.read_with(vcx, |t, _| t.dirty || t.save_failed)
    }

    #[gpui::test]
    fn a_save_is_settled_by_its_answer_not_by_being_queued(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let base = h.store.save_count();
        h.store.set_refusing(true);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
        h.store.set_refusing(false);

        // Queued: nothing to flush, but nothing confirmed either.
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 3 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base + 1);
        assert!(!dirty(&h, &vcx), "the write is queued");
        assert_eq!(
            h.save_notice(&vcx).as_deref(),
            Some(NOT_SAVED),
            "a queued save is not a landed one"
        );
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert_eq!(h.save_notice(&vcx), None);

        // A failure: dirty again, the reason painted, the next burst retries.
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 4 });
        settle(&mut vcx, SAVE_IDLE);
        save_answered(&h, &mut vcx, "book", Err("disk full".into()));
        assert!(dirty(&h, &vcx));
        assert_eq!(
            h.save_notice(&vcx).as_deref(),
            Some("sheet not saved: disk full; the next edit retries")
        );
        // Another sheet's answer is not this tile's.
        save_answered(&h, &mut vcx, "other", Ok(()));
        assert!(dirty(&h, &vcx));
        assert!(h.save_notice(&vcx).is_some());
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 5 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base + 3, "the next burst retried");
        assert_eq!(stored(&h).qty(0), 5);

        // Outcomes arrive in the writer's order, the last describing the
        // latest queued save, so ok → fail → ok ends clean.
        save_answered(&h, &mut vcx, "book", Ok(()));
        save_answered(&h, &mut vcx, "book", Err("blip".into()));
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert!(!dirty(&h, &vcx));
        assert_eq!(h.save_notice(&vcx), None);
    }

    #[gpui::test]
    fn an_ok_for_an_earlier_save_keeps_a_newer_edit_dirty_and_a_failure_is_flushed_on_close(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let base = h.store.save_count();
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, SAVE_IDLE);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 3 });
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert!(
            dirty(&h, &vcx),
            "the newer edit is not in the confirmed save"
        );
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), base + 2);
        save_answered(&h, &mut vcx, "book", Err("disk full".into()));
        let store = h.store.clone();
        drop(h);
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        drop(vcx);
        assert_eq!(
            store.save_count(),
            base + 3,
            "the failed save ran again on close"
        );
    }

    #[gpui::test]
    fn a_close_with_a_save_queued_writes_nothing_extra(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let base = h.store.save_count();
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, SAVE_IDLE);
        let store = h.store.clone();
        drop(h);
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
        drop(vcx);
        assert_eq!(store.save_count(), base + 1);
    }

    /// The tile that queued `book`'s save leaves it and comes back; the
    /// load behind that save fails. A later outcome for the same name
    /// reaches the tile as the sheet's own, and must
    /// not clear the failed load's block.
    #[gpui::test]
    fn a_save_answer_never_clears_a_failed_loads_block(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        assert_eq!(h.command(&mut vcx, "e book"), Ok(()));
        h.store.set_load_refused(true);
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert!(h.tile.read_with(&vcx, |t, _| t.save_blocked), "the premise");
        let blocked = h.save_notice(&vcx);
        assert!(blocked.is_some());
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert_eq!(h.save_notice(&vcx), blocked);
    }

    /// The tile comes back to `book` while its save is queued, the save
    /// fails, and the load behind it fails too: the failed load's block
    /// does not hide that the tile's last edits were never stored.
    #[gpui::test]
    fn a_waiting_loads_failure_keeps_the_lost_edits_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        assert_eq!(h.command(&mut vcx, "e book"), Ok(()));
        h.store.set_load_refused(true);
        save_answered(&h, &mut vcx, "book", Err("disk full".into()));
        assert!(h.tile.read_with(&vcx, |t, _| t.save_blocked), "the premise");
        let notice = h.save_notice(&vcx).unwrap_or_default();
        assert!(
            notice.contains("did not load"),
            "the block is shown: {notice}"
        );
        assert!(
            notice
                .contains("sheet 'book' was not saved: disk full; its last edits were not stored"),
            "the lost edits are still shown: {notice}"
        );
    }

    /// `:name fresh` while `book`'s save is still queued: the sheet's
    /// edits travel under `fresh`, so a failure of `book`'s save only
    /// means the old name's copy is stale (and about to be forgotten) —
    /// the tile is not told its edits were lost.
    #[gpui::test]
    fn a_failed_save_of_the_renamed_from_name_claims_no_lost_edits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        vcx.update(|_, cx| h.factory.flush_all(cx));
        assert_eq!(h.command(&mut vcx, "name fresh"), Ok(()));
        save_answered(&h, &mut vcx, "book", Err("disk full".into()));
        assert_eq!(h.notice(&vcx), None);
        assert_eq!(h.save_notice(&vcx), None);
        save_answered(&h, &mut vcx, "fresh", Ok(()));
        assert_eq!(h.store.forgets(), vec!["book".to_string()]);
        let fresh = crate::core::from_rows("fresh", &h.store.get("fresh").expect("stored"));
        assert_eq!(fresh.unwrap().qty(0), 7);
    }

    /// A confirmed outcome reaches the store's known names; a submission
    /// or a failure never does.
    #[gpui::test]
    fn confirmed_outcomes_update_the_stores_known_names(cx: &mut gpui::TestAppContext) {
        let (data, _rx) = DataHandle::for_tests();
        let store = Rc::new(crate::store::DuckSheetStore::new(data.clone()));
        let factory = PricerFactory::new(
            data,
            store.clone(),
            Views::builtin(),
            PricerSettings::default(),
        );
        cx.update(|cx| {
            factory.save_answered("a", Err("no".into()), cx);
            factory.save_answered("b", Ok(()), cx);
            factory.forget_answered("gone", Ok(()), cx);
        });
        assert!(!store.contains("a"));
        assert!(store.contains("b"));
        store.note_saved("gone");
        cx.update(|cx| factory.forget_answered("gone", Err("no".into()), cx));
        assert!(store.contains("gone"), "a failed forget leaves it known");
        cx.update(|cx| factory.forget_answered("gone", Ok(()), cx));
        assert!(!store.contains("gone"));
    }

    // ---- known names, `:e`, `:new`, `:name`, `:rm` (Task 5) ----

    fn catalog(names: &[&str]) -> geode_core::query::CatalogSnapshot {
        use geode_core::query::{CatalogSnapshot, DatasetCatalog, PartitionCatalog};
        CatalogSnapshot {
            datasets: vec![DatasetCatalog {
                name: crate::core::storage::PRICER_SHEETS_DATASET.into(),
                partitions: names
                    .iter()
                    .map(|n| PartitionCatalog {
                        batch: (*n).to_string(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// A document for a sheet named `name`, from shorthand lines.
    fn sheet_rows(name: &str, lines: &[&str]) -> geode_core::document::DocumentRows {
        let mut s = Sheet::new(name);
        s.apply(Edit::Insert {
            place: Place::Root { at: 0 },
            rows: lines.iter().map(|l| parse(l).unwrap()).collect(),
        })
        .unwrap();
        to_rows(&s).unwrap()
    }

    fn set_catalog(h: &Harness, vcx: &mut VisualTestContext, names: &[&str]) {
        h.diagnostics.update(vcx, |d, cx| {
            d.set_catalog(catalog(names));
            cx.notify();
        });
        vcx.run_until_parked();
    }

    /// A second tile in the harness window, built by the same factory.
    fn second_tile(h: &Harness, vcx: &mut VisualTestContext, id: u64) -> Box<dyn TileContent> {
        vcx.update(|window, cx| {
            h.factory
                .create(
                    TileId(id),
                    None,
                    h.frame.clone(),
                    h.diagnostics.clone(),
                    window,
                    cx,
                )
                .content
        })
    }

    fn title_of(content: &dyn TileContent, vcx: &mut VisualTestContext) -> String {
        vcx.update(|_, cx| content.title(cx).to_string())
    }

    fn command_on(
        content: &dyn TileContent,
        vcx: &mut VisualTestContext,
        line: &str,
    ) -> Result<(), String> {
        vcx.update(|window, cx| content.command(line, window, cx))
    }

    fn completions(h: &Harness, vcx: &mut VisualTestContext, line: &str) -> Vec<String> {
        vcx.update(|_, cx| h.content.completions(line, line.len(), cx))
    }

    fn close(h: Harness, mut vcx: VisualTestContext) {
        drop(h);
        vcx.update(|window, _| window.remove_window());
        vcx.run_until_parked();
    }

    /// Planning decision 12 and the controller's ruling: the catalog's
    /// `pricer_sheets` partitions seed the known names, a later catalog
    /// only adds, `untitled-N` skips them, and `:e`/`:rm` offer them.
    #[gpui::test]
    fn the_catalog_seeds_the_known_names_and_a_later_one_adds(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        set_catalog(&h, &mut vcx, &["alpha", "untitled-2"]);
        assert_eq!(
            h.store.names(),
            vec!["alpha".to_string(), "untitled-2".to_string()]
        );
        let second = second_tile(&h, &mut vcx, TILE + 1);
        assert_eq!(
            title_of(&*second, &mut vcx),
            "Pricer · untitled-3",
            "untitled-1 is open and untitled-2 is a document"
        );
        set_catalog(&h, &mut vcx, &["beta"]);
        assert_eq!(
            h.store.names(),
            vec![
                "alpha".to_string(),
                "beta".to_string(),
                "untitled-2".to_string()
            ],
            "a later catalog adds and drops nothing"
        );
        assert_eq!(
            completions(&h, &mut vcx, "e "),
            vec!["alpha", "beta", "untitled-2"]
        );
        assert_eq!(
            completions(&h, &mut vcx, "rm "),
            vec!["alpha", "beta", "untitled-2"]
        );
        // Another dataset's partitions are not sheets.
        h.diagnostics.update(&mut vcx, |d, cx| {
            let mut c = catalog(&["gamma"]);
            c.datasets[0].name = "cvi_params".into();
            d.set_catalog(c);
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(!h.store.contains("gamma"));
    }

    /// A factory created with no catalog held asks for one, once, and
    /// notifies in the same update (the bridge's drain wakes only on a
    /// notify).
    #[gpui::test]
    fn a_factory_with_no_catalog_asks_for_one_exactly_once(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(geode_shell::shell::dialog::init_reclaimed_keybindings);
        cx.update(crate::init);
        let (data, _rx) = DataHandle::for_tests();
        let store = MemorySheetStore::default();
        let factory = Rc::new(PricerFactory::new(
            data,
            Rc::new(store.clone()),
            Views::builtin(),
            PricerSettings::default(),
        ));
        let notified = Rc::new(std::cell::Cell::new(0));
        let asked = Rc::new(std::cell::Cell::new(0));
        // The bridge's stand-in: it wakes on a notify and takes the request.
        let diagnostics = cx.update(|cx| {
            let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
            let (notified, asked) = (notified.clone(), asked.clone());
            cx.observe(&diagnostics, move |d, cx| {
                notified.set(notified.get() + 1);
                if d.update(cx, |d, _| d.take_catalog_request()).is_some() {
                    asked.set(asked.get() + 1);
                }
            })
            .detach();
            diagnostics
        });
        cx.run_until_parked();
        let frame = cx.update(|cx| {
            cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None))
        });
        let window = cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let first = factory.create(
                    TileId(1),
                    None,
                    frame.clone(),
                    diagnostics.clone(),
                    window,
                    cx,
                );
                cx.new(|cx| gpui_component::Root::new(first.view, window, cx))
            })
            .unwrap()
        });
        cx.run_until_parked();
        assert!(notified.get() >= 1, "the request was notified");
        assert_eq!(asked.get(), 1);
        // A second tile, in its own update once the first request was
        // taken, still with no catalog held: it asks nothing more.
        window
            .update(cx, |_, window, cx| {
                let _ = factory.create(
                    TileId(2),
                    None,
                    frame.clone(),
                    diagnostics.clone(),
                    window,
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(asked.get(), 1, "asked once, not per tile");
    }

    /// The controller's ruling: a closed tile's save that is queued but
    /// not yet confirmed keeps its name taken — a new sheet under it
    /// would save on top of that document.
    #[gpui::test]
    fn a_queued_unconfirmed_save_keeps_its_name_taken(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.store.set_confirming(true);
        h.visible(&mut vcx, true);
        edit(
            &h,
            &mut vcx,
            Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![parse("SPX Z26 5000 C").unwrap()],
            },
        );
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.store.save_count(), 1, "queued");
        assert!(!h.store.contains("untitled-1"), "not confirmed");
        let (factory, frame, diagnostics) =
            (h.factory.clone(), h.frame.clone(), h.diagnostics.clone());
        close(h, vcx);
        let create = |cx: &mut gpui::TestAppContext, id: u64| {
            cx.update(|cx| {
                let mut title = String::new();
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let o = factory.create(
                        TileId(id),
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
            })
        };
        assert_eq!(create(cx, TILE + 1), "Pricer · untitled-2");
        // Either outcome ends the wait; a failed one frees the name.
        cx.update(|cx| factory.save_answered("untitled-1", Err("no".into()), cx));
        assert_eq!(create(cx, TILE + 2), "Pricer · untitled-1");
    }

    /// `:name` treats a queued save's name as taken too.
    #[gpui::test]
    fn colon_name_refuses_a_name_with_a_queued_save(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.store.set_confirming(true);
        assert_eq!(h.command(&mut vcx, "new"), Ok(()));
        assert_eq!(h.title(&mut vcx), "Pricer · untitled-1");
        edit(
            &h,
            &mut vcx,
            Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![parse("SPX Z26 5000 C").unwrap()],
            },
        );
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.command(&mut vcx, "new"), Ok(()));
        assert!(!h.store.contains("untitled-1"), "queued, not confirmed");
        assert_eq!(
            h.command(&mut vcx, "name untitled-1"),
            Err("sheet 'untitled-1' already exists".into())
        );
        assert_eq!(
            completions(&h, &mut vcx, "e "),
            vec!["untitled-1"],
            "its document is on its way"
        );
    }

    #[gpui::test]
    fn colon_e_saves_the_sheet_it_leaves_and_loads_the_other(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert!(
            h.store
                .save("other", sheet_rows("other", &["NKY Z26 30000 C"]))
        );
        answer_all(&h, &mut vcx, 1.0);
        let base = h.store.save_count();
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        h.store.set_pending(true);

        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        assert_eq!(h.store.save_count(), base + 1, "the dirty sheet was saved");
        assert_eq!(stored(&h).qty(0), 7);
        assert_eq!(h.title(&mut vcx), "Pricer · other");
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));
        assert_eq!(
            h.store.loads().last(),
            Some(&("other".to_string(), QueryKey(TILE), 2))
        );
        assert!(
            h.requests()
                .iter()
                .any(|r| matches!(r, Request::Cancel { key } if *key == QueryKey(TILE))),
            "the old sheet's pricing is cancelled"
        );
        let rows = h.store.get("other").unwrap();
        answer_load(&h, &mut vcx, 2, Ok(snapshot(&rows)));
        assert_eq!(h.sheet_len(&vcx), 1);
        assert!(h.notice(&vcx).is_none());
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("nothing to undo"),
            "the history stayed with the sheet it belonged to"
        );
        // The name was given back: another tile can open it.
        h.store.set_pending(false);
        let second = second_tile(&h, &mut vcx, TILE + 1);
        assert_eq!(command_on(&*second, &mut vcx, "e book"), Ok(()));
        assert_eq!(title_of(&*second, &mut vcx), "Pricer · book");
        assert_eq!(
            h.command(&mut vcx, "e book"),
            Err("sheet 'book' is open in another tile".into())
        );
        // The tile's own name: nothing happens.
        let loads = h.store.loads().len();
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        assert_eq!(h.store.loads().len(), loads);
        assert_eq!(h.sheet_len(&vcx), 1);
    }

    #[gpui::test]
    fn colon_e_stays_when_the_sheet_it_leaves_cannot_be_saved(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        h.store.set_refusing(true);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        assert_eq!(
            h.command(&mut vcx, "e other"),
            Err("sheet 'book' was not saved; still open here".into())
        );
        assert_eq!(h.title(&mut vcx), "Pricer · book");
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
        assert!(dirty(&h, &vcx));
    }

    /// The per-sheet save state goes with the sheet: a failed save's
    /// notice, a failed load's block.
    #[gpui::test]
    fn a_switch_starts_the_save_state_over(cx: &mut gpui::TestAppContext) {
        let (store, record, _) = pending_book();
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        h.visible(&mut vcx, true);
        answer_load(&h, &mut vcx, 1, Err("boom".into()));
        assert!(h.tile.read_with(&vcx, |t, _| t.save_blocked));
        h.store.set_pending(false);
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        assert_eq!(h.save_notice(&vcx), None);
        assert!(!h.tile.read_with(&vcx, |t, _| t.save_blocked));
        edit(
            &h,
            &mut vcx,
            Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![parse("SPX Z26 5000 C").unwrap()],
            },
        );
        settle(&mut vcx, SAVE_IDLE);
        assert!(h.store.get("other").is_some(), "the new sheet saves");
        save_answered(&h, &mut vcx, "other", Err("disk full".into()));
        assert!(h.save_notice(&vcx).is_some());
        assert_eq!(h.command(&mut vcx, "new"), Ok(()));
        assert_eq!(h.save_notice(&vcx), None);
        assert!(!dirty(&h, &vcx));
    }

    #[gpui::test]
    fn colon_new_opens_the_next_untitled_sheet_empty(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        answer_all(&h, &mut vcx, 1.0);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        let loads = h.store.loads().len();
        assert_eq!(h.command(&mut vcx, "new"), Ok(()));
        assert_eq!(h.title(&mut vcx), "Pricer · untitled-1");
        assert_eq!(h.sheet_len(&vcx), 0);
        assert_eq!(h.store.loads().len(), loads, "nothing to load");
        assert!(h.notice(&vcx).is_none());
        assert_eq!(stored(&h).qty(0), 7, "the sheet left behind was saved");
        h.dispatch(&mut vcx, "undo", None);
        assert_eq!(h.footer(&vcx).as_deref(), Some("nothing to undo"));
        // Twice: `untitled-1` is this tile's own, so the next is 2.
        assert_eq!(h.command(&mut vcx, "new"), Ok(()));
        assert_eq!(h.title(&mut vcx), "Pricer · untitled-2");
    }

    #[gpui::test]
    fn colon_name_renames_and_forgets_the_old_name_only_once_saved(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert!(
            h.store
                .save("taken", sheet_rows("taken", &["NKY Z26 30000 C"]))
        );
        let _second = second_tile(&h, &mut vcx, TILE + 1);
        let base = h.store.save_count();
        assert_eq!(
            h.command(&mut vcx, "name taken"),
            Err("sheet 'taken' already exists".into())
        );
        assert_eq!(
            h.command(&mut vcx, "name untitled-1"),
            Err("sheet 'untitled-1' already exists".into()),
            "open in another tile"
        );
        assert_eq!(h.store.save_count(), base);

        assert_eq!(h.command(&mut vcx, "name fresh"), Ok(()));
        assert_eq!(h.title(&mut vcx), "Pricer · fresh");
        assert_eq!(h.store.save_count(), base + 1, "saved under the new name");
        assert_eq!(
            crate::core::from_rows("fresh", &h.store.get("fresh").unwrap())
                .unwrap()
                .len(),
            5
        );
        assert!(h.store.forgets().is_empty(), "not before the save lands");
        save_answered(&h, &mut vcx, "fresh", Ok(()));
        assert_eq!(h.store.forgets(), vec!["book".to_string()]);
    }

    /// The controller's ruling on a failed save under the new name: the
    /// tile keeps the new name, the old document stays, the notice says so.
    #[gpui::test]
    fn a_failed_save_after_colon_name_keeps_the_new_name_and_the_old_document(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert_eq!(h.command(&mut vcx, "name fresh"), Ok(()));
        save_answered(&h, &mut vcx, "fresh", Err("disk full".into()));
        assert_eq!(h.title(&mut vcx), "Pricer · fresh");
        assert!(h.store.forgets().is_empty());
        assert!(h.store.get("book").is_some());
        assert_eq!(
            h.save_notice(&vcx).as_deref(),
            Some("sheet not saved: disk full; the next edit retries")
        );
        // The retry's confirmation retires the old name then.
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        settle(&mut vcx, SAVE_IDLE);
        save_answered(&h, &mut vcx, "fresh", Ok(()));
        assert_eq!(h.store.forgets(), vec!["book".to_string()]);
    }

    #[gpui::test]
    fn colon_name_refuses_a_sheet_that_did_not_load(cx: &mut gpui::TestAppContext) {
        let (store, record, _) = pending_book();
        let (h, mut vcx) = open_full(cx, Some(record), store, PricerSettings::default());
        assert_eq!(
            h.command(&mut vcx, "name fresh"),
            Err("the sheet is still loading".into())
        );
        answer_load(&h, &mut vcx, 1, Err("boom".into()));
        assert_eq!(
            h.command(&mut vcx, "name fresh"),
            Err("sheet 'book' did not load; it cannot be renamed".into())
        );
        assert!(h.store.forgets().is_empty());
    }

    fn prompt(h: &Harness, vcx: &VisualTestContext) -> Option<String> {
        h.tile
            .read_with(vcx, |t, _| t.confirm.as_ref().map(|c| c.prompt.to_string()))
    }

    /// `old` in the store, `book` open here, `untitled-1` in another tile.
    fn rm_fixture(
        cx: &mut gpui::TestAppContext,
    ) -> (Harness, VisualTestContext, Box<dyn TileContent>) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        vcx.update(|window, _cx| window.activate_window());
        vcx.run_until_parked();
        assert!(h.store.save("old", sheet_rows("old", &["NKY Z26 30000 C"])));
        let second = second_tile(&h, &mut vcx, TILE + 1);
        (h, vcx, second)
    }

    #[gpui::test]
    fn colon_rm_refuses_open_and_unknown_sheets(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _second) = rm_fixture(cx);
        assert_eq!(
            h.command(&mut vcx, "rm book"),
            Err("sheet 'book' is open here: close it or `:e` another sheet first".into())
        );
        assert_eq!(
            h.command(&mut vcx, "rm untitled-1"),
            Err("sheet 'untitled-1' is open in another tile".into())
        );
        assert_eq!(
            h.command(&mut vcx, "rm nope"),
            Err("no sheet 'nope'".into())
        );
        assert_eq!(prompt(&h, &vcx), None);
        assert_eq!(h.mode(&mut vcx), "normal");
    }

    #[gpui::test]
    fn colon_rm_asks_and_y_forgets(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _second) = rm_fixture(cx);
        h.store.set_confirming(true);
        h.store.set_known(vec!["old".into()]);
        assert_eq!(h.command(&mut vcx, "rm old"), Ok(()));
        h.draw(&mut vcx);
        let question = "remove sheet 'old' and all its history? (y/n)";
        assert_eq!(prompt(&h, &vcx).as_deref(), Some(question));
        assert!(h.header(&vcx).contains(&question.to_string()));
        assert!(painted(&mut vcx, "pricer-remove-confirm-5"));
        assert_eq!(h.mode(&mut vcx), "insert", "the confirm holds the keyboard");
        assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
        assert!(h.store.forgets().is_empty(), "nothing before the answer");

        vcx.simulate_keystrokes("y");
        assert_eq!(h.store.forgets(), vec!["old".to_string()]);
        assert_eq!(prompt(&h, &vcx), None);
        assert_eq!(h.mode(&mut vcx), "normal");
        assert!(!focused(&mut vcx), "blurred before it dropped");
        assert!(h.store.contains("old"), "known until the forget lands");
        vcx.update(|_, cx| h.factory.forget_answered("old", Ok(()), cx));
        assert!(!h.store.contains("old"));
        assert!(!completions(&h, &mut vcx, "e ").contains(&"old".to_string()));
    }

    /// A removed sheet stays removed when the catalog the diagnostics
    /// entity holds is stale: with the diagnostics tile closed, nothing
    /// refreshes it after the forget, and any later publish (another
    /// sheet's autosave, a feed) re-reads it. Only a confirmed save of the
    /// name makes it known again.
    #[gpui::test]
    fn a_removed_sheet_is_not_revived_by_a_stale_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _second) = rm_fixture(cx);
        h.store.set_confirming(true);
        set_catalog(&h, &mut vcx, &["gone"]);
        assert!(h.store.contains("gone"), "fixture: the catalog lists it");
        h.command(&mut vcx, "rm gone").unwrap();
        h.draw(&mut vcx);
        vcx.simulate_keystrokes("y");
        assert_eq!(h.store.forgets(), vec!["gone".to_string()]);
        vcx.update(|_, cx| h.factory.forget_answered("gone", Ok(()), cx));
        let publish = |vcx: &mut VisualTestContext| {
            h.diagnostics.update(vcx, |d, cx| {
                d.note_published("cvi_params");
                cx.notify();
            });
            vcx.run_until_parked();
        };
        publish(&mut vcx);
        assert!(!h.store.contains("gone"), "the stale catalog revived it");
        assert!(!completions(&h, &mut vcx, "e ").contains(&"gone".to_string()));
        assert_eq!(
            h.command(&mut vcx, "rm gone"),
            Err("no sheet 'gone'".into())
        );
        // A confirmed save of the name makes it a document again.
        assert_eq!(h.command(&mut vcx, "name gone"), Ok(()));
        save_answered(&h, &mut vcx, "gone", Ok(()));
        publish(&mut vcx);
        assert!(h.store.contains("gone"));
    }

    /// `y` re-checks the name: a sheet another tile opened, or one another
    /// tile's `:name` is retiring, while the question stood is not removed.
    #[gpui::test]
    fn y_refuses_a_sheet_opened_or_retiring_since_the_rm_armed(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, second) = rm_fixture(cx);
        h.command(&mut vcx, "rm old").unwrap();
        h.draw(&mut vcx);
        assert_eq!(command_on(&*second, &mut vcx, "e old"), Ok(()));
        assert!(prompt(&h, &vcx).is_some(), "fixture: the question stands");
        vcx.simulate_keystrokes("y");
        assert!(h.store.forgets().is_empty());
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("sheet 'old' not removed: it is open in another tile")
        );

        // The second tile moves on, freeing `old`; the question is asked
        // again, and meanwhile the second tile opens `old` and renames it
        // away, so `old` is retiring and open nowhere.
        assert!(
            h.store
                .save("mine", sheet_rows("mine", &["NKY Z26 30000 C"]))
        );
        assert_eq!(command_on(&*second, &mut vcx, "e mine"), Ok(()));
        h.command(&mut vcx, "rm old").unwrap();
        h.draw(&mut vcx);
        assert_eq!(command_on(&*second, &mut vcx, "e old"), Ok(()));
        assert_eq!(command_on(&*second, &mut vcx, "name renamed"), Ok(()));
        assert!(prompt(&h, &vcx).is_some(), "fixture: the question stands");
        vcx.simulate_keystrokes("y");
        assert!(h.store.forgets().is_empty());
        assert_eq!(
            h.footer(&vcx).as_deref(),
            Some("sheet 'old' not removed: it is being removed")
        );
    }

    /// A sheet whose load failed (here, refused at submission) is retried
    /// in place by `:e` of its own name; nothing on it was ever saved.
    #[gpui::test]
    fn colon_e_of_a_blocked_sheets_own_name_reloads_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        h.store.set_load_refused(true);
        assert_eq!(h.command(&mut vcx, "e book"), Ok(()));
        assert!(h.tile.read_with(&vcx, |t, _| t.save_blocked), "the premise");
        h.store.set_load_refused(false);
        let before = loads_of(&h.store, "book");
        assert_eq!(h.command(&mut vcx, "e book"), Ok(()));
        assert_eq!(loads_of(&h.store, "book"), before + 1, "asked again");
        assert!(!h.tile.read_with(&vcx, |t, _| t.save_blocked));
        assert_eq!(h.save_notice(&vcx), None);
        assert_eq!(h.sheet_len(&vcx), 5);
    }

    #[gpui::test]
    fn a_failed_forget_is_painted_on_the_tile_that_asked(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _second) = rm_fixture(cx);
        h.command(&mut vcx, "rm old").unwrap();
        h.draw(&mut vcx);
        vcx.simulate_keystrokes("y");
        vcx.update(|_, cx| h.factory.forget_answered("elsewhere", Err("x".into()), cx));
        assert_eq!(h.notice(&vcx), None, "not this tile's forget");
        vcx.update(|_, cx| h.factory.forget_answered("old", Err("locked".into()), cx));
        assert_eq!(
            h.notice(&vcx).as_deref(),
            Some("sheet 'old' not removed: locked")
        );
    }

    #[gpui::test]
    fn any_other_key_cancels_the_rm_confirm_and_is_consumed(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _second) = rm_fixture(cx);
        for key in ["n", "escape", "j", "shift-y"] {
            let cursor = h.cursor(&vcx);
            h.command(&mut vcx, "rm old").unwrap();
            h.draw(&mut vcx);
            // Held here so a handle dropped still focused stays visible.
            let focus = h.tile.read_with(&vcx, |t, _| {
                t.confirm.as_ref().expect("armed").focus.clone()
            });
            assert!(vcx.update(|window, _| focus.is_focused(window)));
            vcx.simulate_keystrokes(key);
            assert!(
                !vcx.update(|window, _| focus.is_focused(window)),
                "{key}: blurred before it dropped"
            );
            assert_eq!(prompt(&h, &vcx), None, "{key}");
            assert_eq!(h.footer(&vcx).as_deref(), Some(NOT_REMOVED), "{key}");
            assert_eq!(h.mode(&mut vcx), "normal", "{key}");
            assert!(!focused(&mut vcx), "{key}: blurred before it dropped");
            assert_eq!(h.cursor(&vcx), cursor, "{key} was the confirm's alone");
        }
        assert!(h.store.forgets().is_empty());
        assert!(h.store.get("old").is_some());
    }

    #[gpui::test]
    fn focus_leaving_or_a_pointer_press_cancels_the_rm_confirm(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _second) = rm_fixture(cx);
        h.command(&mut vcx, "rm old").unwrap();
        h.draw(&mut vcx);
        vcx.update(|window, cx| window.blur(cx));
        h.draw(&mut vcx);
        assert_eq!(prompt(&h, &vcx), None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(NOT_REMOVED));
        vcx.simulate_keystrokes("y");
        assert!(h.store.forgets().is_empty(), "a later y forgets nothing");

        h.command(&mut vcx, "rm old").unwrap();
        let at = centre_of(&mut vcx, "pricer-remove-confirm-5");
        click_at(&mut vcx, at, 1);
        assert_eq!(prompt(&h, &vcx), None);
        assert_eq!(h.footer(&vcx).as_deref(), Some(NOT_REMOVED));
        assert!(h.store.forgets().is_empty());

        // A palette dispatch under the question answers "no" too.
        h.command(&mut vcx, "rm old").unwrap();
        h.draw(&mut vcx);
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(prompt(&h, &vcx), None);
        assert!(!focused(&mut vcx));
        assert!(h.store.forgets().is_empty());
    }

    /// A save refused after an earlier one was queued: the earlier one's
    /// `Ok` does not describe the refused attempt, so `NOT_SAVED` stays.
    #[gpui::test]
    fn an_ok_for_an_earlier_save_keeps_a_later_refusals_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 2 });
        settle(&mut vcx, SAVE_IDLE);
        h.store.set_refusing(true);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 3 });
        settle(&mut vcx, SAVE_IDLE);
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert_eq!(h.save_notice(&vcx).as_deref(), Some(NOT_SAVED));
    }

    /// Line ids restart per sheet: a pricing answer for the sheet left
    /// behind must never land on the new sheet's line with the same id.
    #[gpui::test]
    fn an_old_sheets_pricing_answer_never_lands_on_the_new_one(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let batch = h.prices().pop().expect("the shown sheet priced");
        assert!(
            h.store
                .save("other", sheet_rows("other", &["NKY Z26 30000 C"]))
        );
        // Hidden, so the new sheet submits nothing of its own that would
        // retire the old batch's tag by itself.
        h.visible(&mut vcx, false);
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        assert_eq!(h.sheet_len(&vcx), 1);
        assert!(h.prices().is_empty(), "the premise: nothing newer asked");
        h.answer(&mut vcx, &batch, 42.0);
        let priced = h.tile.read_with(&vcx, |t, _| t.sheet.priced_at(0));
        assert_eq!(priced, None, "the old batch's answer is not this sheet's");
    }

    // ---- fix round 1: retiring names, deferred loads, save origins ----

    /// A tile restoring `sheet` in a window of its own, from the harness's
    /// factory — the production route a session restore takes.
    fn restore_tile(
        h: &Parts,
        cx: &mut gpui::TestAppContext,
        id: u64,
        sheet: &str,
    ) -> (Entity<PricerTile>, Box<dyn TileContent>, VisualTestContext) {
        let mut record = toml::Table::new();
        record.insert("sheet".into(), sheet.into());
        type Slot = Option<(Entity<PricerTile>, Box<dyn TileContent>)>;
        let slot: Rc<RefCell<Slot>> = Rc::new(RefCell::new(None));
        let window = cx
            .update(|cx| {
                let slot = slot.clone();
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let o = h.factory.create(
                        TileId(id),
                        Some(&record),
                        h.frame.clone(),
                        h.diagnostics.clone(),
                        window,
                        cx,
                    );
                    let tile = o.view.clone().downcast::<PricerTile>().unwrap();
                    *slot.borrow_mut() = Some((tile.clone(), o.content));
                    cx.new(|cx| gpui_component::Root::new(tile, window, cx))
                })
            })
            .unwrap();
        let vcx = VisualTestContext::from_window(window.into(), cx);
        let (tile, content) = slot.borrow_mut().take().unwrap();
        (tile, content, vcx)
    }

    /// What outlives a closed harness tile.
    struct Parts {
        factory: Rc<PricerFactory>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
    }

    fn parts(h: &Harness) -> Parts {
        Parts {
            factory: h.factory.clone(),
            frame: h.frame.clone(),
            diagnostics: h.diagnostics.clone(),
        }
    }

    /// `second_tile`, with the tile entity.
    fn second_with_entity(
        h: &Harness,
        vcx: &mut VisualTestContext,
        id: u64,
    ) -> (Box<dyn TileContent>, Entity<PricerTile>) {
        vcx.update(|window, cx| {
            let o = h.factory.create(
                TileId(id),
                None,
                h.frame.clone(),
                h.diagnostics.clone(),
                window,
                cx,
            );
            let tile = o.view.clone().downcast::<PricerTile>().unwrap();
            (o.content, tile)
        })
    }

    fn loads_of(h: &MemorySheetStore, sheet: &str) -> usize {
        h.loads().iter().filter(|(n, _, _)| n == sheet).count()
    }

    /// The race the rename test once hid: the old name is reserved from
    /// the rename until its forget is answered, so no tile opens (and
    /// then loses) a sheet that is about to be deleted.
    #[gpui::test]
    fn a_retiring_name_is_reserved_until_its_forget_is_answered(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        let third = second_tile(&h, &mut vcx, TILE + 2);
        assert_eq!(h.command(&mut vcx, "name fresh"), Ok(()));
        let removing = Err("sheet 'book' is being removed".to_string());
        assert_eq!(command_on(&*third, &mut vcx, "e book"), removing);
        assert_eq!(command_on(&*third, &mut vcx, "rm book"), removing);
        assert_eq!(command_on(&*third, &mut vcx, "name book"), removing);
        save_answered(&h, &mut vcx, "fresh", Ok(()));
        assert_eq!(h.store.forgets(), vec!["book".to_string()]);
        assert_eq!(
            command_on(&*third, &mut vcx, "e book"),
            removing,
            "reserved until the forget is answered"
        );
        vcx.update(|_, cx| h.factory.forget_answered("book", Ok(()), cx));
        assert_eq!(command_on(&*third, &mut vcx, "e book"), Ok(()));
    }

    /// `:rm`'s forget reserves its name the same way.
    #[gpui::test]
    fn a_name_removed_by_rm_is_reserved_until_answered(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _second) = rm_fixture(cx);
        h.command(&mut vcx, "rm old").unwrap();
        h.draw(&mut vcx);
        vcx.simulate_keystrokes("y");
        assert_eq!(
            h.command(&mut vcx, "e old"),
            Err("sheet 'old' is being removed".into())
        );
        vcx.update(|_, cx| h.factory.forget_answered("old", Err("locked".into()), cx));
        assert_eq!(h.command(&mut vcx, "e old"), Ok(()));
    }

    /// A restore of a retiring name opens a fresh sheet instead, as `:e`
    /// refuses it: the document is about to be forgotten, and a tile
    /// holding it would show a removed sheet (and its next save would
    /// recreate it). The rename's forget still runs.
    #[gpui::test]
    fn a_restore_of_a_retiring_name_opens_a_fresh_sheet(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert_eq!(h.command(&mut vcx, "name fresh"), Ok(()));
        let loads = loads_of(&h.store, "book");
        let (restored, _content, vcx2) = restore_tile(&parts(&h), cx, TILE + 3, "book");
        let (name, notice) = restored.read_with(&vcx2, |t, _| {
            (
                t.sheet.name.clone(),
                t.notice.as_ref().map(|n| n.to_string()),
            )
        });
        assert_eq!(name, "untitled-1");
        assert_eq!(
            notice.as_deref(),
            Some("sheet 'book' is being removed; opened untitled-1")
        );
        assert_eq!(loads_of(&h.store, "book"), loads, "book was not read");
        save_answered(&h, &mut vcx, "fresh", Ok(()));
        assert_eq!(h.store.forgets(), vec!["book".to_string()]);
    }

    /// If a tile holds the old name when the rename's save is confirmed,
    /// the forget is skipped. No route opens a retiring name today (`:e`
    /// and a restore both refuse it); the guard keeps a future one from
    /// deleting a sheet in use, so the test opens the name directly.
    #[gpui::test]
    fn a_rename_never_forgets_a_sheet_a_tile_has_open(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert_eq!(h.command(&mut vcx, "name fresh"), Ok(()));
        h.tile.read_with(&vcx, |t, _| {
            t.shared.open.borrow_mut().insert("book".into());
        });
        save_answered(&h, &mut vcx, "fresh", Ok(()));
        assert!(h.store.forgets().is_empty(), "book is open in a tile");
        assert!(h.store.get("book").is_some());
        assert!(h.tile.read_with(&vcx, |t, _| t.rename_from.is_none()));
        assert!(
            h.tile
                .read_with(&vcx, |t, _| !t.shared.retiring.borrow().contains("book")),
            "the reservation is released"
        );
    }

    /// Reads and saves run on different lanes: a load of a name whose save
    /// is queued waits for that save's answer, then asks once.
    #[gpui::test]
    fn colon_e_back_to_a_sheet_with_a_queued_save_waits_for_its_answer(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert!(
            h.store
                .save("other", sheet_rows("other", &["NKY Z26 30000 C"]))
        );
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        let before = loads_of(&h.store, "book");
        assert_eq!(h.command(&mut vcx, "e book"), Ok(()));
        assert_eq!(
            loads_of(&h.store, "book"),
            before,
            "no read until the save answers"
        );
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));
        assert_eq!(h.sheet_len(&vcx), 0);
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert_eq!(loads_of(&h.store, "book"), before + 1, "asked once");
        assert_eq!(h.sheet_len(&vcx), 5);
        assert_eq!(stored(&h).qty(0), 7);
        assert!(h.notice(&vcx).is_none());
    }

    /// Two saves of one name queued: the load waits for both answers, not
    /// the first — a read after only the first could return that older
    /// generation, and the next save would make it the latest.
    #[gpui::test]
    fn a_load_behind_two_queued_saves_waits_for_both_answers(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert!(
            h.store
                .save("other", sheet_rows("other", &["NKY Z26 30000 C"]))
        );
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 6 });
        vcx.update(|_, cx| h.factory.flush_all(cx));
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        // Leaving flushes the second save of `book`.
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        let before = loads_of(&h.store, "book");
        assert_eq!(h.command(&mut vcx, "e book"), Ok(()));
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert_eq!(
            loads_of(&h.store, "book"),
            before,
            "one save of `book` is still queued"
        );
        assert_eq!(h.notice(&vcx).as_deref(), Some(LOADING));
        save_answered(&h, &mut vcx, "book", Ok(()));
        assert_eq!(
            loads_of(&h.store, "book"),
            before + 1,
            "asked once, after both"
        );
        assert_eq!(h.sheet_len(&vcx), 5);
        assert_eq!(stored(&h).qty(0), 7);
    }

    /// A save the store refused at submission is never counted as queued:
    /// nothing will answer it, so a load of the name must not wait for it.
    #[gpui::test]
    fn a_save_refused_at_submission_leaves_no_load_waiting(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 9 });
        let store = h.store.clone();
        store.set_refusing(true);
        let before = loads_of(&store, "book");
        let p = parts(&h);
        // Close the tile: its flush is refused.
        close(h, vcx);
        store.set_refusing(false);
        let (restored, _content, vcx2) = restore_tile(&p, cx, TILE + 5, "book");
        assert_eq!(loads_of(&store, "book"), before + 1, "read at once");
        assert!(!restored.read_with(&vcx2, |t, _| t.loading));
    }

    /// A tile closed with its flush queued: a new tile restoring the name
    /// waits for that save's answer the same way.
    #[gpui::test]
    fn a_restore_of_a_name_with_a_queued_save_waits_for_its_answer(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 9 });
        let store = h.store.clone();
        let before = loads_of(&store, "book");
        let p = parts(&h);
        // Close the first tile: its flush queues a save of `book`.
        close(h, vcx);
        let (restored, _content, mut vcx2) = restore_tile(&p, cx, TILE + 4, "book");
        assert_eq!(
            loads_of(&store, "book"),
            before,
            "no read until the save answers"
        );
        assert!(restored.read_with(&vcx2, |t, _| t.loading));
        vcx2.update(|_, cx| p.factory.save_answered("book", Ok(()), cx));
        assert_eq!(loads_of(&store, "book"), before + 1);
        assert!(!restored.read_with(&vcx2, |t, _| t.loading));
        assert_eq!(restored.read_with(&vcx2, |t, _| t.sheet.qty(0)), 9);
    }

    /// A save's outcome reaches the tile that queued it, not whichever
    /// tile holds the name now; one that moved on is told its last edits
    /// were not stored.
    #[gpui::test]
    fn a_save_outcome_reaches_the_tile_that_queued_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_seeded(cx, &BOOK);
        assert!(
            h.store
                .save("other", sheet_rows("other", &["NKY Z26 30000 C"]))
        );
        let (second, second_tile) = second_with_entity(&h, &mut vcx, TILE + 1);
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 7 });
        assert_eq!(h.command(&mut vcx, "e other"), Ok(()));
        assert_eq!(command_on(&*second, &mut vcx, "e book"), Ok(()));
        assert!(
            second_tile.read_with(&vcx, |t, _| t.loading),
            "the premise: it waits on book's queued save"
        );
        save_answered(&h, &mut vcx, "book", Err("disk full".into()));
        assert_eq!(
            h.notice(&vcx).as_deref(),
            Some("sheet 'book' was not saved: disk full; its last edits were not stored")
        );
        assert_eq!(
            h.save_notice(&vcx),
            None,
            "other's own save state is untouched"
        );
        assert!(!dirty(&h, &vcx));
        second_tile.read_with(&vcx, |t, _| {
            assert!(!t.save_failed, "not the holder's save");
            assert!(!t.loading, "its waiting load started");
        });
        // An `Ok` for a moved-on tile paints nothing.
        edit(&h, &mut vcx, Edit::SetQty { row: 0, qty: 3 });
        h.dispatch(&mut vcx, "escape", None);
        assert_eq!(h.command(&mut vcx, "new"), Ok(()));
        save_answered(&h, &mut vcx, "other", Ok(()));
        assert!(h.notice(&vcx).is_none());
    }
}
