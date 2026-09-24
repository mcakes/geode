//! The line-pricer tile (spec §8): one `Sheet`, an `Rc<GridModel>`
//! installed into a `DataTable`, a header and a footer.
//!
//! **One door per kind of change.** A request-changing edit goes through
//! `apply_edit` (Task 8), which records its undo; a delivery through
//! `deliver`; a tick through `tick`. Each ends at `rebuild`, which builds
//! the grid model (never in `render`), installs it, re-prepares the
//! header and notifies.

use crate::content::{PricerSettings, Shared};
use crate::core::commands::{self, Command};
use crate::core::sheet::{LineId, Sheet};
use crate::core::storage::from_rows;
use crate::core::tree::Expansion;
use crate::core::views::ColumnPlan;
use crate::delegate::{ChevronClicked, SheetDelegate};
use crate::grid::{GridModel, GridRowKind};
use crate::header::{self, HeaderInputs, HeaderModel};
use crate::session::Record;
use crate::store::Loaded;
use geode_core::clock::Clock;
use geode_core::document::DocumentRows;
use geode_core::pricing::PriceOutcome;
use geode_core::query::QueryKey;
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, find_match};
use gpui::prelude::*;
use gpui::{App, Context, Entity, SharedString, Window, div};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, v_flex};
use std::rc::Rc;

pub(crate) const LOADING: &str = "loading…";

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

pub struct PricerTile {
    pub(crate) id: TileId,
    // Read from Task 8 on (the request door and the frame's as-of).
    #[allow(dead_code)]
    frame: Entity<Frame>,
    #[allow(dead_code)]
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
    /// Transient header notice (a load failure, a refused request).
    pub(crate) notice: Option<SharedString>,
    /// The view fallback's standing notice (`resolve_plan`).
    view_notice: Option<SharedString>,
    /// A user error for the footer (spec §8.3); cleared by the next verb.
    pub(crate) footer: Option<SharedString>,
    /// What the footer paints: `footer`, else the cursor row's failure.
    pub(crate) footer_text: Option<SharedString>,
    pub(crate) header: HeaderModel,
    title: SharedString,
    stack: Option<StackHandle>,
    pub(crate) clock: Clock,
    /// What `p`/`shift+p` put (Task 11): the last `y y` or `d d`.
    pub(crate) register: Option<crate::core::RowSpec>,
    find: Option<FindState>,
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
                    notices.push(format!("sheet '{name}' did not load: {e}"));
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

        let delegate = SheetDelegate::new(cx.theme());
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
        cx.subscribe(&table, |this, _, event: &ChevronClicked, cx| {
            this.toggle_grid_row(event.0, cx)
        })
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
        // A closed tile gives its name back (spec §7.4's open set). Tasks
        // 8 and 12 add the cancel and the final save here.
        cx.on_release(|this: &mut PricerTile, _cx| {
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
            view_notice: None,
            footer: None,
            footer_text: None,
            header: HeaderModel::default(),
            title: SharedString::default(),
            stack: None,
            clock: app_clock(cx),
            register: None,
            find: None,
        };
        this.resolve_plan();
        this.rebuild(cx);
        this
    }

    // ---- what the shell reads ----------------------------------------

    /// `normal` until Tasks 9–11 add `entry`, `insert` and `menu`.
    pub fn key_context(&self) -> KeyContext {
        KeyContext::new("pricer").pair("mode", self.mode()).counts()
    }

    pub(crate) fn mode(&self) -> &'static str {
        "normal"
    }

    /// Does one of THIS tile's own fields hold window focus? (Tasks 9–10
    /// add the entry field, the cell editor and the choice field.)
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        let _ = (window, cx);
        false
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
                None => self.expansion.ids().collect(),
            },
        }
        .to_table()
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    /// Task 8 adds the reprice on show and the cancel on hide.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        cx.notify();
    }

    /// `/` (spec §8.5): every keystroke searches from the ORIGIN, so a
    /// lengthening query walks forward and a shortened one walks back
    /// (vim's incsearch); `escape` returns to the origin.
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
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

    /// Task 8 fills this in.
    pub fn deliver(&mut self, outcome: PriceOutcome, cx: &mut Context<Self>) {
        let _ = (outcome, cx);
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
                }
                Err(e) => self.notice = Some(format!("sheet '{name}' did not load: {e}").into()),
            },
            Ok(None) => {
                self.notice = Some(format!("sheet '{name}' was not found; opened empty").into())
            }
            Err(e) => self.notice = Some(format!("sheet '{name}' did not load: {e}").into()),
        }
        if let Some(held) = self.held_expanded.take() {
            self.expansion = Expansion::from_ids(held);
        }
        self.expansion.retain_packages(&self.sheet);
        self.resolve_plan();
        self.rebuild(cx);
    }

    /// A reload reached this tile (planning decision 20).
    pub(crate) fn config_changed(&mut self, cx: &mut Context<Self>) {
        self.resolve_plan();
        self.rebuild(cx);
    }

    // ---- verbs ----------------------------------------------------------

    /// Every normal-mode verb (Tasks 7–11 add arms). A verb this tile
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
        let _ = window;
        let n = count.unwrap_or(1).max(1) as usize;
        self.footer = None;
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
                self.notice = None;
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

    /// The chevron at grid row `row` (spec §8.2: its click is `space`).
    pub(crate) fn toggle_grid_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.set_cursor_row(row);
        self.tree_verb(None, cx);
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
        let _ = window;
        match commands::parse(line)? {
            Command::View(name) => self.set_view(&name, cx),
            // Tasks 8 and 11 replace this arm verb by verb.
            _ => Err("not built yet".into()),
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

    /// Task 9 answers the open entry field's place.
    pub(crate) fn entry_place(&self) -> Option<crate::core::Place> {
        None
    }

    /// The only way a model reaches the table (spec §8.2).
    pub(crate) fn install_model(&mut self, cx: &mut Context<Self>) {
        let model = Rc::clone(&self.model);
        self.table.update(cx, |t, cx| {
            t.delegate_mut().model = model;
            t.refresh(cx);
        });
        self.sync_cursor(cx);
    }

    pub(crate) fn rebuild_chrome(&mut self) {
        let settings: PricerSettings = self.shared.settings.borrow().clone();
        let notice = self.notice.clone().or_else(|| self.view_notice.clone());
        self.header = header::prepare(HeaderInputs {
            sheet: &self.sheet,
            notice,
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

    fn on_table_event(&mut self, event: &TableEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        if let TableEvent::SelectCell(row, col) = event {
            self.set_cursor_row(*row);
            if let Some(c) = SheetDelegate::plan_col(*col) {
                self.cursor.col = c;
            }
            self.sync_cursor(cx);
            self.rebuild_chrome();
            cx.notify();
        }
        // `SelectRow`/`SelectColumn` are what `sync_cursor` itself emits:
        // deliberately unmatched. Task 10 adds `DoubleClickedCell`.
    }
}

impl gpui::Render for PricerTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Staleness is a compare per frame, never a format (spec §9.4).
        let stale_after = self.shared.settings.borrow().stale_after;
        self.header.stale = self.header.last_priced.is_some_and(|at| {
            chrono::Utc::now()
                .signed_duration_since(at)
                .to_std()
                .unwrap_or_default()
                > stale_after
        });
        let theme = cx.theme();
        let header = header::render(&self.header, theme, self.stack.as_ref(), self.id);
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

    // The harness serves Tasks 7–12's tests too; not every field and
    // method has a reader yet.
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
                    // focused `InputState` on it (Tasks 9–10's fields).
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

    // ---- Task 6 ----

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

    // ---- Task 7 ----

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
}
