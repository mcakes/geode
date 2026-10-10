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
//! New, Rename, Delete and Revert are registered actions (the palette and
//! the `⋯` menu). New and Rename ask in a prompt bar under the header
//! (`editor::PromptField`), validated before anything is written; Rename,
//! Delete and Revert then ask y/n on the confirm bar, Rename and Delete
//! saying how many groupings, views, scopes and expressions still name the
//! classification (they are not rewritten). Each writes through the config door as one
//! batch; the tile shows where it will land at once and goes back if the
//! shell refuses the write.
//!
//! Export CSV asks for a path in the platform's save dialog, awaited off
//! the update, then asks the data tier to write the classification as the
//! tile shows it; only the latest file operation's answer is acted on.
//! Import CSV asks for a file in the open dialog, reads it through the data
//! tier, plans it over the shown object and asks y/n with the counts (a
//! plan landing while the keyboard is elsewhere waits for focus); `y`
//! applies the plan over the object shown then as one write and one undo
//! step. Each step is bound to the classification the import began on.
//!
//! What paint reads is prepared in `Chrome` and the table's `Prepared`
//! whenever the configuration, the values, the filter or the sort change,
//! never in render.

mod editor;
mod header;
mod table;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use geode_core::classification;
use geode_core::classification::import::{ImportPlan, MAX_IMPORT_BYTES, plan_import};
use geode_core::classification::validate::{references, source_columns, validate_source};
use geode_core::config::{DIMENSIONS_DOC, Layer};
use geode_core::dimensions::DerivedDimension;
use geode_core::query::{AsOf, DistinctOutcome, DistinctParams, QueryKey};
use geode_core::scope::Scope;
use geode_core::sort::SortOrder;
use geode_core::textfile::{TextFileOp, TextFileOutcome, TextFileParams, TextFileResult};
use geode_data::{DataHandle, Refusal};
use geode_shell::actions::ActionId;
use geode_shell::frame::{ConfigEdit, FrameRef, TileNotice};
use geode_shell::keymap::{Binding, KeyContext};
use geode_shell::module::{CloseHandle, FindEvent, StackHandle};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::vimnav::NavCommand;
use geode_tile::confirm::{self, Confirm, ConfirmHost};
use geode_tile::edit::EditCaret;
use geode_tile::header::{HEADER_HEIGHT, Mode, link_chips};
use geode_tile::menu::{self, ActionRow, Hint, Menu, MenuHost, MenuIds, MenuPick, Row};
use geode_tile::notice::{self, Dismissals, Notice};
use gpui::prelude::*;
use gpui::{
    AnchoredPositionMode, AnyWindowHandle, App, Context, Entity, Focusable as _, PathPromptOptions,
    Pixels, Point, SharedString, Window, anchored, div,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::content::{ClassificationsConfig, Shared, action_title};
use crate::core::files::{self, FileOp};
use crate::core::grid::{GridModel, label_text};
use crate::core::history::History;
use crate::core::prompt::{self, Prompt, Step};
use crate::core::session::{self, SortCol, State};
use editor::{ChoiceKind, EditorPaint, LabelEditor, PromptField};
use header::HeaderModel;
use table::{GridDelegate, Prepared, RowContext, RowPressed, SortClicked};

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
const RENAME_ACTION: &str = "classifications::rename";
const DELETE_ACTION: &str = "classifications::delete";
const REVERT_ACTION: &str = "classifications::revert";
const REFRESH_ACTION: &str = "classifications::refresh";
const EXPORT_ACTION: &str = "classifications::export";
const EXPORT_ALL_ACTION: &str = "classifications::export_all";
const IMPORT_ACTION: &str = "classifications::import";

/// What an armed confirm does on `y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    Rename {
        from: String,
        to: String,
    },
    Delete {
        name: String,
    },
    Revert {
        name: String,
    },
    /// Apply `plan`, read from `file`, to classification `name`, whose
    /// sources were `from`'s values when it was planned.
    Import {
        name: String,
        from: String,
        file: String,
        plan: ImportPlan,
    },
}

/// A create, rename or delete the tile has written and is showing ahead
/// of the reload that carries it. A refusal from the shell puts back what
/// was shown before.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Awaiting {
    /// The classification the tile shows meanwhile (created or renamed to),
    /// `None` after a delete.
    shows: Option<String>,
    /// The one it showed before, to go back to on a refusal.
    restores: Option<String>,
    /// The one renamed or deleted: kept out of the switcher meanwhile.
    removed: Option<String>,
}

/// The switcher's refusal while there is nothing to list.
const NOTHING_TO_SWITCH: &str = "no classifications to switch to";

/// A planned import waiting for the keyboard: asked about when the tile is
/// next the focused tile with nothing open in it. Arming takes focus, so a
/// plan landing while the trader types elsewhere would take the keys, and a
/// stray `y` would apply it.
struct HeldImport {
    tag: u64,
    name: String,
    from: String,
    file: String,
    plan: ImportPlan,
    /// The window the plan landed in, which the confirm focuses.
    window: AnyWindowHandle,
}

/// The header's notices, by how long each lives. None stays for good: a
/// notice that outlived its cause would read as a standing fault.
#[derive(Default)]
struct Notices {
    /// What the session restore dropped. Cleared by the trader's first
    /// action in the tile (a key or a press): by then it has been seen,
    /// and the restore is over.
    restore: Vec<String>,
    /// The switcher refused with nothing to list. Cleared once a
    /// configuration defines something to switch to.
    nothing_to_switch: bool,
    /// What the last label verb, or the shell about its write, said: a
    /// fork, a refusal, rows skipped by undo, a disabled menu row. Cleared
    /// when the next verb starts and when another classification is shown.
    outcome: Vec<Notice>,
}

impl Notices {
    fn outcome(&mut self, notice: Notice) {
        if !self.outcome.contains(&notice) {
            self.outcome.push(notice);
        }
    }
}

/// A label write: give these sources this label (`None` clears), or replay
/// the history one step.
enum Write {
    Assign(Vec<String>, Option<String>),
    Undo,
    Redo,
}

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
    /// Whether `observed` is an answer for the shown source column: an
    /// export with unclassified rows written before one would leave every
    /// unmapped value out of the file without saying so.
    values_loaded: bool,
    /// The file operation waiting for its answer; any other is stale.
    file_op: Option<FileOp>,
    /// An import's file operation tag and file while its plan is made off
    /// the UI thread.
    planning: Option<(u64, String)>,
    /// A planned import waiting until the tile may take the keyboard.
    held: Option<HeldImport>,
    /// Whether this is the focused tile, as the shell last said.
    focused: bool,
    /// The last file operation's tag.
    file_tag: u64,
    /// The directory of the last file exported or imported here, where the
    /// next save dialog opens. In memory only.
    file_dir: Option<PathBuf>,
    /// Why the values are not on screen: a refused or failed read.
    values_notice: Option<String>,
    /// The filter in force when a `/` search began, which a cancelled
    /// search restores; `None` while no search is open.
    find_entry: Option<String>,
    /// The rem the table's column widths were last scaled to.
    last_rem: f32,
    menu: Option<(MenuKind, Menu<Pick>)>,
    /// Where a right-click opened the `⋯` menu, in window coordinates; it
    /// hangs from that point instead of the header's `⋯` control. `None`
    /// for a menu the key, the palette or the control opened.
    menu_at: Option<Point<Pixels>>,
    /// The live keymap the menus' hints are resolved against.
    chords: Arc<Vec<Binding>>,
    stack: Option<StackHandle>,
    close: Option<CloseHandle>,
    /// Painted in the header's cluster.
    notices: Notices,
    /// The shown classification's label history and its optimistic edit.
    history: History,
    /// The open label editor; the tile is in insert mode while it is.
    editor: Option<LabelEditor>,
    /// The open prompt (New, Rename); insert mode too.
    prompt: Option<PromptField>,
    /// The armed y/n question; it holds the keyboard, insert mode too.
    confirm: Option<Confirm<Pending>>,
    /// A create, rename or delete written and not yet reloaded.
    awaiting: Option<Awaiting>,
    /// A classification whose revert is queued and not yet reloaded. Its
    /// label verbs are refused meanwhile: the tile cannot see the lower
    /// copy the revert will show, and an edit built on the user copy would
    /// replace the revert's removal in the shell's batch, undoing it.
    reverting: Option<String>,
    /// Whether a values read is on its way: the header says so until it
    /// answers or is refused.
    loading: bool,
    /// The window the editor or the prompt opened in, to blur its field
    /// where no window is at hand (a reload removing the classification, a
    /// close).
    field_window: Option<AnyWindowHandle>,
    /// The label `y y` copied; `Some(None)` copied an unclassified row.
    register: Option<Option<String>>,
    /// Whether a configuration has been settled yet: the first one is
    /// the tile's construction, as far as the switcher is concerned.
    settled: bool,
    /// Whether the last settle showed a classification, so the next can
    /// tell a classification going away from one never shown.
    was_shown: bool,
    chrome: Chrome,
    /// The header notices the trader dismissed (a click, or `escape` with
    /// nothing else to do), each hidden while `chrome.notices` still
    /// reports it. Pruned in `rebuild_chrome`, never in render.
    dismissed: Dismissals,
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
        let (state, restore) = restored.map(session::from_table).unwrap_or_default();
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
        cx.subscribe_in(&table, window, |this, _, e: &RowContext, window, cx| {
            this.row_context(*e, window, cx)
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
        // The shell tells the tile about its config writes (a fork, a
        // refusal) by posting on the frame and notifying it.
        cx.observe(frame.entity(), |this, _, cx| this.frame_changed(cx))
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
            values_loaded: false,
            file_op: None,
            planning: None,
            held: None,
            focused: false,
            file_tag: 0,
            file_dir: None,
            values_notice: None,
            find_entry: None,
            last_rem: 0.0,
            menu: None,
            menu_at: None,
            chords: menu::live_bindings(cx),
            stack: None,
            close: None,
            notices: Notices {
                restore,
                ..Notices::default()
            },
            history: History::default(),
            editor: None,
            prompt: None,
            confirm: None,
            awaiting: None,
            reverting: None,
            loading: false,
            field_window: None,
            register: None,
            settled: false,
            was_shown: false,
            chrome: Chrome::default(),
            dismissed: Dismissals::default(),
            menu_selector: format!("classifications-menu-button-{}", id.0).into(),
            menu_tip: format!("tip-classifications-menu-{}", id.0).into(),
            switch_tip: format!("tip-classifications-switch-{}", id.0).into(),
            #[cfg(test)]
            refreshes: 0,
        };
        tile.settle(cx);
        tile
    }

    /// The factory stored a new configuration snapshot. When it carries the
    /// optimistic edit (or a revert, or someone else's change to this
    /// classification) the history drops its pending copy; the rows are
    /// rebuilt from what is current either way.
    pub fn config_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(dim) = self.config_dim() {
            self.history.reloaded(&dim);
        }
        self.settle(cx);
        cx.notify();
    }

    /// Bring the tile in line with the snapshot: the switcher opens on the
    /// first snapshot when nothing is shown, and again when the shown
    /// classification goes away, never on a reload that leaves a
    /// nothing-shown tile as it was (the trader closed the switcher). An
    /// open switcher takes the new names. The values are asked for again
    /// when the shown classification or its source column changed, and the
    /// rows rebuilt from the map either way.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let shown = self.shown().is_some();
        if !shown {
            // Its history means nothing now, and its editor writes nowhere;
            // a rename or a question about it asks about nothing.
            self.history.forget();
            self.release_editor(cx);
            if self
                .prompt
                .as_ref()
                .is_some_and(|p| matches!(p.prompt, Prompt::Rename { .. }))
            {
                self.release_prompt(cx);
            }
            if confirm::withdraw(self, cx).is_some() {
                self.notices
                    .outcome(Notice::status("the classification asked about is gone"));
            }
        }
        if self.awaiting.as_ref().is_some_and(|a| self.landed(a)) {
            self.awaiting = None;
        }
        if self
            .reverting
            .as_ref()
            .is_some_and(|n| self.revert_landed(n))
        {
            self.reverting = None;
        }
        self.settle_menu(shown);
        if self.notices.nothing_to_switch && !self.switch_rows().is_empty() {
            self.notices.nothing_to_switch = false;
        }
        self.ask_values(false);
        self.rebuild_rows(false, cx);
    }

    fn settle_menu(&mut self, shown: bool) {
        let configured = self.shared.config.borrow().is_some();
        let first = configured && !self.settled;
        let gone = self.was_shown && !shown;
        self.settled |= configured;
        self.was_shown = shown;
        match self.menu.as_ref().map(|(k, _)| *k) {
            Some(MenuKind::Switch) => {
                let rows = self.switch_rows();
                if rows.is_empty() {
                    self.menu = None;
                } else if let Some((_, m)) = self.menu.as_mut() {
                    m.replace_rows(rows, &self.chords);
                }
            }
            _ if !shown && (first || gone) => {
                let rows = self.switch_rows();
                if !rows.is_empty() {
                    self.menu = Some((MenuKind::Switch, Menu::new(rows, &self.chords)));
                }
            }
            _ => {}
        }
    }

    /// The frame notified: take what the shell said about this tile's
    /// config writes. A fork is news; a refusal means the optimistic edit
    /// never landed, so the rows go back to the configuration's object.
    fn frame_changed(&mut self, cx: &mut Context<Self>) {
        let id = self.id;
        let told = self
            .frame
            .entity()
            .update(cx, |f, _| f.take_tile_notices(id));
        if told.is_empty() {
            return;
        }
        let refused = told.iter().any(|n| matches!(n, TileNotice::Refused(_)));
        // A create, rename or delete that never landed: back to what was
        // shown before it, ahead of the notices, which showing another
        // classification would clear.
        if refused
            && let Some(a) = self.awaiting.take()
            && self.state.name == a.shows
        {
            self.menu = None;
            match a.restores {
                Some(name) => self.show(&name, cx),
                None => {
                    self.state.name = None;
                    self.was_shown = false;
                }
            }
        }
        for notice in told {
            match notice {
                TileNotice::Forked(text) => self.notices.outcome(Notice::status(text)),
                TileNotice::Refused(text) => self.notices.outcome(Notice::danger(text)),
            }
        }
        if refused {
            self.history.refused();
            self.reverting = None;
            self.rebuild_rows(false, cx);
        } else {
            self.rebuild_chrome();
        }
        cx.notify();
    }

    /// Whether the configuration no longer has the user copy of `name` over
    /// a lower one: the revert's reload has landed (or the classification
    /// went away).
    fn revert_landed(&self, name: &str) -> bool {
        let config = self.shared.config.borrow();
        config
            .as_ref()
            .is_none_or(|c| !c.shadowed.contains_key(name))
    }

    /// Whether `name`'s revert is still on its way.
    fn reverting(&self) -> Option<&str> {
        self.reverting
            .as_deref()
            .filter(|n| self.state.name.as_deref() == Some(*n))
    }

    /// Refuse a label verb while the shown classification's revert is on
    /// its way (see `reverting`); `true` when refused.
    fn refuse_while_reverting(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(name) = self.reverting().map(str::to_string) else {
            return false;
        };
        self.refuse(Notice::status(format!("reverting {name}\u{2026}")), cx);
        true
    }

    /// Whether the configuration now carries what `a` wrote.
    fn landed(&self, a: &Awaiting) -> bool {
        let config = self.shared.config.borrow();
        let Some(config) = config.as_ref() else {
            return false;
        };
        a.shows
            .as_ref()
            .is_none_or(|n| config.dims.get(n).is_some())
            && a.removed
                .as_ref()
                .is_none_or(|n| config.dims.get(n).is_none())
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
            self.values_loaded = false;
        }
        self.asked = now.clone();
        self.values_notice = None;
        self.loading = false;
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
            dataset: None,
        };
        match self.data.distinct(params) {
            Ok(()) => self.loading = true,
            Err(refusal) => {
                self.values_notice = Some(match refusal {
                    Refusal::Busy => {
                        format!("values not loaded: {refusal} \u{2014} shift+r retries")
                    }
                    Refusal::Stopped => format!("values not loaded: {refusal}"),
                });
            }
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
        self.loading = false;
        let answered = match outcome.values {
            Ok(values) => {
                self.observed = values;
                self.values_loaded = true;
                self.values_notice = None;
                true
            }
            Err(why) => {
                self.values_notice =
                    Some(format!("values not loaded: {why} \u{2014} shift+r retries"));
                false
            }
        };
        self.rebuild_rows(false, cx);
        // A restored cursor the values did not hold has nowhere left to
        // arrive from. A failed read is not that answer: `R` may still
        // bring its row.
        if answered {
            self.grid.forget_seed();
        }
        cx.notify();
    }

    /// Rebuild the grid's rows from the shown classification's map (the
    /// history's pending edit while one waits for its reload) and the
    /// observed values, then everything painted from them. `relabelled`
    /// after a label verb: the cursor keeps its shown index.
    fn rebuild_rows(&mut self, relabelled: bool, cx: &mut Context<Self>) {
        let rows = {
            let config = self.shared.config.borrow();
            let dim = self
                .state
                .name
                .as_deref()
                .and_then(|n| config.as_ref()?.dims.get(n));
            dim.map(|d| classification::rows(self.history.current(d), &self.observed))
                .unwrap_or_default()
        };
        if relabelled {
            self.grid.relabelled(rows);
        } else {
            self.grid.set_rows(rows);
        }
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
        let editor = self.editor_paint(cx);
        let refreshed = self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.set_selected(selected);
            d.set_editor(editor);
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

    /// The open editor as the delegate paints it, on the cursor's row.
    fn editor_paint(&self, cx: &Context<Self>) -> Option<EditorPaint> {
        let e = self.editor.as_ref()?;
        Some(EditorPaint {
            row: self.grid.cursor()?,
            input: e.input.clone(),
            choice: Rc::new(editor::choice_paint(&e.list, ChoiceKind::Labels)),
            tile: cx.weak_entity(),
            tile_id: self.id.0,
        })
    }

    /// Mirror the editor alone into the delegate: a keystroke in the field
    /// re-ranks the list without touching the rows.
    fn sync_editor(&mut self, cx: &mut Context<Self>) {
        let editor = self.editor_paint(cx);
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set_editor(editor);
            cx.notify();
        });
    }

    /// One header notice dismissed, as a click on it does and `escape`
    /// does to each notice showing. A transient one-shot notice — a verb's
    /// outcome (a refusal, a fork, a skipped row), `g c`'s nothing to
    /// switch to, or what the session restore dropped — is cleared, so the
    /// key repeated says it again. The standing one (the values notice) is
    /// hidden through `dismissed` until it changes. Whether anything did.
    pub(crate) fn dismiss_notice(&mut self, n: &Notice) -> bool {
        if !n.dismissable() {
            return false;
        }
        if let Some(i) = self.notices.outcome.iter().position(|o| o == n) {
            self.notices.outcome.remove(i);
            self.rebuild_chrome();
            return true;
        }
        // `g c`'s refusal: a key set it, so it is transient too.
        if self.notices.nothing_to_switch && *n == Notice::danger(NOTHING_TO_SWITCH) {
            self.notices.nothing_to_switch = false;
            self.rebuild_chrome();
            return true;
        }
        if n.tone() == notice::Tone::Danger
            && self.notices.restore.iter().any(|r| r == n.text().as_ref())
        {
            self.notices.restore.retain(|r| r != n.text().as_ref());
            self.rebuild_chrome();
            return true;
        }
        self.dismissed.dismiss(n)
    }

    fn rebuild_chrome(&mut self) {
        let config = self.shared.config.borrow();
        let dim = self
            .state
            .name
            .as_deref()
            .and_then(|n| config.as_ref()?.dims.get(n));
        let layer = dim.and_then(|d| config.as_ref()?.layers.get(&d.name).copied());
        self.chrome.header = HeaderModel::prepare(dim, layer, self.grid.counts())
            .loading(dim.is_some() && self.loading);
        let none_defined = config
            .as_ref()
            .is_none_or(|c| c.dims.all().next().is_none());
        let new = action_title(NEW_ACTION);
        let awaited = self.awaiting.as_ref().and_then(|a| a.shows.as_ref());
        self.chrome.empty = match (&self.state.name, dim) {
            (_, Some(_)) => None,
            (Some(name), None) if awaited == Some(name) => {
                Some(format!("Saving {name}\u{2026}").into())
            }
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
        let n = &self.notices;
        self.chrome.notices = n
            .restore
            .iter()
            .cloned()
            .map(Notice::danger)
            .chain(
                n.nothing_to_switch
                    .then(|| Notice::danger(NOTHING_TO_SWITCH)),
            )
            .chain(n.outcome.iter().cloned())
            .chain(self.values_notice.iter().cloned().map(Notice::warning))
            .collect();
        // A dismissed notice no longer reported shows again when it returns.
        self.dismissed.prune(&self.chrome.notices);
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
        let removed = self.awaiting.as_ref().and_then(|a| a.removed.as_deref());
        let mut names: Vec<&str> = config
            .iter()
            .flat_map(|c: &ClassificationsConfig| c.dims.all().map(|d| d.name.as_str()))
            .filter(|n| Some(*n) != removed)
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

    /// The `⋯` menu: the label verbs, then the classification's own, each
    /// running the palette's action, with its live chord. A row that cannot
    /// act says why in its lane, and in full when picked; Revert is listed
    /// only over a user copy shadowing a lower layer's.
    fn action_rows(&self) -> Vec<Row<Pick>> {
        let row = |id: &'static str, enabled: Result<(), Blocked>| {
            let title = action_title(id);
            let title = title.strip_prefix("Classification: ").unwrap_or(title);
            let row = ActionRow::new(Pick::Action(id), title).hint(Hint::chord(id));
            Row::Action(match enabled {
                Ok(()) => row,
                Err(b) => row.enabled(Err(b.long.into())).short_reason(b.short),
            })
        };
        let shown = self.shown();
        let labelled = || match (&shown, self.grid.targets().is_empty()) {
            (None, _) => Err(Blocked::same(NOTHING_SHOWN)),
            (Some(_), true) => Err(Blocked::same("no row")),
            (Some(_), false) => Ok(()),
        };
        let paste = match (labelled(), &self.register) {
            (Ok(()), None) => Err(Blocked {
                short: "nothing copied",
                long: NOTHING_COPIED.into(),
            }),
            (other, _) => other,
        };
        let mut rows = vec![
            row("classifications::edit", labelled()),
            row("classifications::clear", labelled()),
            row("classifications::yank", labelled()),
            row("classifications::paste", paste),
            Row::Separator,
            row(NEW_ACTION, Ok(())),
            row(RENAME_ACTION, self.own(Verb::Rename).map(|_| ())),
            row(DELETE_ACTION, self.own(Verb::Delete).map(|_| ())),
        ];
        if self.revertible().is_ok() {
            rows.push(row(REVERT_ACTION, Ok(())));
        }
        rows.push(row(
            REFRESH_ACTION,
            shown.map(|_| ()).ok_or(Blocked::same(NOTHING_SHOWN)),
        ));
        rows.push(Row::Separator);
        rows.push(row(EXPORT_ACTION, self.exportable(false).map(|_| ())));
        rows.push(row(EXPORT_ALL_ACTION, self.exportable(true).map(|_| ())));
        rows.push(row(IMPORT_ACTION, self.importable().map(|_| ())));
        rows
    }

    /// The shown classification when the user layer owns it outright, so
    /// a rename or delete can remove it; else why not. A desk or builtin
    /// definition cannot be removed from the user layer, and removing a
    /// user copy over a lower layer's would leave that copy standing under
    /// the old name.
    fn own(&self, verb: Verb) -> Result<String, Blocked> {
        let Some(name) = self.shown() else {
            return Err(Blocked::same(NOTHING_SHOWN));
        };
        let config = self.shared.config.borrow();
        let config = config.as_ref().expect("shown, so configured");
        let (verb_does, verb_name) = match verb {
            Verb::Rename => ("rename it there", "rename"),
            Verb::Delete => ("remove it from there", "delete"),
        };
        let lower = |layer: Layer, short| Blocked {
            short,
            long: format!(
                "{name} is defined in {} config; Geode cannot {verb_does}",
                layer.name()
            ),
        };
        // No recorded layer: a removal from the user layer might remove
        // nothing, and the tile would wait for a reload that never comes.
        match config.layers.get(&name).copied() {
            None => {
                return Err(Blocked {
                    short: "defined where unknown",
                    long: format!("can't tell where {name} is defined"),
                });
            }
            Some(Layer::Desk) => return Err(lower(Layer::Desk, "defined in desk config")),
            Some(Layer::Builtin) => {
                return Err(lower(Layer::Builtin, "defined in builtin config"));
            }
            Some(Layer::User) => {}
        }
        if let Some(&under) = config.shadowed.get(&name) {
            let short = match under {
                Layer::Builtin => "a builtin copy stands under it",
                Layer::Desk => "a desk copy stands under it",
                // A user copy shadows only a lower layer; kept neutral
                // rather than misname a layer if that ever changes.
                Layer::User => "another copy stands under it",
            };
            return Err(Blocked {
                short,
                long: format!(
                    "{name} has a {} copy under yours, which a {verb_name} would leave in place \u{2014} Revert\u{2026} removes yours",
                    under.name()
                ),
            });
        }
        Ok(name)
    }

    /// The shown classification when it can be exported, `all` with a
    /// blank row for every observed value it does not map; else why not.
    /// Those rows need the values: without them the file would leave every
    /// unmapped value out and look complete.
    fn exportable(&self, all: bool) -> Result<String, Blocked> {
        let Some(name) = self.shown() else {
            return Err(Blocked {
                short: NOTHING_SHOWN,
                long: "choose a classification to export".into(),
            });
        };
        if all && !self.values_loaded {
            return Err(Blocked {
                short: "values not loaded",
                long: "values not loaded yet \u{2014} shift+r loads them".into(),
            });
        }
        // Values from an earlier answer while a newer read is on its way,
        // or after one refused or failed, may miss what the data now holds.
        if all && (self.loading || self.values_notice.is_some()) {
            return Err(Blocked {
                short: "values not current",
                long: "values not current \u{2014} shift+r reloads them".into(),
            });
        }
        Ok(name)
    }

    /// Export: ask where to save, in the directory of the last file this
    /// tile wrote or read, else the home directory. The dialog is awaited
    /// off the update; its answer writes what is shown then, so an edit
    /// made while it stood is in the file. A cancelled dialog does nothing.
    fn ask_export_path(&mut self, all: bool, cx: &mut Context<Self>) {
        let name = match self.exportable(all) {
            Ok(name) => name,
            Err(b) => return self.refuse(b.long, cx),
        };
        self.notices.outcome.clear();
        self.rebuild_chrome();
        cx.notify();
        let dir = self.file_dir.clone().unwrap_or_else(home_dir);
        let answer = cx.prompt_for_new_path(&dir, Some(&format!("{name}.csv")));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = answer.await {
                this.update(cx, |t, cx| t.export_to(name, path, all, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Queue the write of classification `name`, as the tile shows it (a
    /// label edit not yet reloaded included), to `path`. Checked again: the
    /// save dialog is modeless, so another classification may be shown by
    /// now (its rows would go out under `name`'s file), or the values may
    /// have gone stale.
    fn export_to(&mut self, name: String, path: PathBuf, all: bool, cx: &mut Context<Self>) {
        if self.shown().as_deref() != Some(name.as_str()) {
            return self.refuse(
                format!("{name} is no longer shown \u{2014} nothing exported"),
                cx,
            );
        }
        if let Err(b) = self.exportable(all) {
            return self.refuse(b.long, cx);
        }
        let Some(config_dim) = self.config_dim() else {
            return;
        };
        let dim = self.history.current(&config_dim);
        let observed = all.then_some(self.observed.as_slice());
        let text = classification::export(dim, observed);
        let rows = files::export_rows(dim, observed);
        let replaced = self.overtake_import();
        self.file_tag += 1;
        let params = TextFileParams {
            key: QueryKey(self.id.0),
            tag: self.file_tag,
            path: path.clone(),
            op: TextFileOp::Write { text },
        };
        match self.data.text_file(params) {
            Ok(()) => {
                self.file_dir = path.parent().map(Path::to_path_buf);
                self.file_op = Some(FileOp::Export {
                    tag: self.file_tag,
                    name,
                    path,
                    rows,
                });
            }
            Err(refusal) => self.refuse(busy_text(refusal), cx),
        }
        self.say_replaced(replaced, cx);
    }

    /// Take the import a newer file operation overtakes, whichever step it
    /// is at (its read, its plan, or held for the keyboard), and return its
    /// file: it must not vanish without a word.
    fn overtake_import(&mut self) -> Option<String> {
        let waiting = match self.file_op.take() {
            Some(FileOp::Import { path, .. }) => Some(files::file_name(&path)),
            Some(FileOp::Export { .. }) | None => None,
        };
        let planning = self.planning.take().map(|(_, file)| file);
        let held = self.held.take().map(|h| h.file);
        waiting.or(planning).or(held)
    }

    fn say_replaced(&mut self, replaced: Option<String>, cx: &mut Context<Self>) {
        if let Some(file) = replaced {
            self.notices
                .outcome
                .retain(|n| *n != Notice::status(files::ready(&file)));
            self.notices.outcome(Notice::status(files::replaced(&file)));
            self.rebuild_chrome();
            cx.notify();
        }
    }

    /// A file operation's answer. One carrying any tag but the waiting
    /// operation's was overtaken (or already answered) and is dropped.
    pub fn on_file(
        &mut self,
        outcome: TextFileOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_op.as_ref().map(FileOp::tag) != Some(outcome.tag) {
            return;
        }
        let Some(op) = self.file_op.take() else {
            return;
        };
        let notice = match (op, outcome.result) {
            (FileOp::Export { rows, path, .. }, TextFileResult::Written(Ok(()))) => {
                Notice::status(files::exported(rows, &path))
            }
            (FileOp::Export { .. }, TextFileResult::Written(Err(why))) => {
                Notice::danger(format!("export failed: {why}"))
            }
            (FileOp::Import { name, path, .. }, TextFileResult::Read(read)) => {
                return self.import_read(outcome.tag, name, &path, read, window, cx);
            }
            // A write answered as a read or a read as a write: not the
            // answer to what was asked.
            (FileOp::Export { .. }, TextFileResult::Read(_))
            | (FileOp::Import { .. }, TextFileResult::Written(_)) => {
                return;
            }
        };
        self.notices.outcome.clear();
        self.notices.outcome(notice);
        self.rebuild_chrome();
        cx.notify();
    }

    /// The shown classification when a file can be imported into it; else
    /// why not.
    fn importable(&self) -> Result<String, Blocked> {
        self.shown().ok_or_else(|| Blocked {
            short: NOTHING_SHOWN,
            long: "choose a classification to import into".into(),
        })
    }

    /// Import: ask which file to read. The dialog is awaited off the
    /// update, and its answer is bound to the classification shown now. A
    /// cancelled dialog does nothing.
    fn ask_import_path(&mut self, cx: &mut Context<Self>) {
        let name = match self.importable() {
            Ok(name) => name,
            Err(b) => return self.refuse(b.long, cx),
        };
        self.notices.outcome.clear();
        self.rebuild_chrome();
        cx.notify();
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = answer.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |t, cx| t.import_from(name, path, cx)).ok();
            }
        })
        .detach();
    }

    /// Ask the data tier to read `path` for classification `name`. The
    /// open dialog is modeless: another classification may be shown by
    /// now, and the file is not read into it.
    fn import_from(&mut self, name: String, path: PathBuf, cx: &mut Context<Self>) {
        if self.shown().as_deref() != Some(name.as_str()) {
            let file = files::file_name(&path);
            return self.refuse(Notice::status(files::not_applied(&file, &name)), cx);
        }
        let replaced = self.overtake_import();
        self.file_tag += 1;
        let params = TextFileParams {
            key: QueryKey(self.id.0),
            tag: self.file_tag,
            path: path.clone(),
            op: TextFileOp::Read {
                max_bytes: MAX_IMPORT_BYTES,
            },
        };
        match self.data.text_file(params) {
            Ok(()) => {
                self.file_dir = path.parent().map(Path::to_path_buf);
                self.file_op = Some(FileOp::Import {
                    tag: self.file_tag,
                    name,
                    path,
                });
            }
            Err(refusal) => self.refuse(busy_text(refusal), cx),
        }
        self.say_replaced(replaced, cx);
    }

    /// The file read for classification `name`: planned off the UI thread
    /// over a copy of the object the tile shows now (a file at the size
    /// limit takes tens of milliseconds to plan), then asked about in
    /// [`Self::import_planned`].
    fn import_read(
        &mut self,
        tag: u64,
        name: String,
        path: &Path,
        read: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file = files::file_name(path);
        if self.shown().as_deref() != Some(name.as_str()) {
            return self.refuse(Notice::status(files::not_applied(&file, &name)), cx);
        }
        let text = match read {
            Ok(text) => text,
            Err(why) => return self.refuse(format!("import failed: {why}"), cx),
        };
        let Some(config_dim) = self.config_dim() else {
            return self.refuse(Notice::status(files::not_applied(&file, &name)), cx);
        };
        let dim = self.history.current(&config_dim).clone();
        let from = dim.from.clone();
        self.planning = Some((tag, file.clone()));
        let planning = cx.background_spawn(async move { plan_import(&dim, &text) });
        cx.spawn_in(window, async move |this, cx| {
            let planned = planning.await;
            this.update_in(cx, |t, window, cx| {
                t.import_planned(tag, name, from, file, planned, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// A plan has landed. Asked about only while it is still the latest
    /// file operation's, for the classification and source column it was
    /// made over. Nothing is written until `y`; a file that changes nothing
    /// or is refused whole asks nothing. Rejected rows are listed whatever
    /// the plan. The question takes the keyboard, so it is armed only while
    /// the tile may take it ([`Self::keyboard_free`]); otherwise the plan is
    /// held and asked about when the tile next may.
    #[allow(clippy::too_many_arguments)]
    fn import_planned(
        &mut self,
        tag: u64,
        name: String,
        from: String,
        file: String,
        planned: Result<ImportPlan, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tag != self.file_tag {
            return;
        }
        self.planning = None;
        if let Err(notice) = self.import_current(&name, &from, &file) {
            return self.refuse(notice, cx);
        }
        let plan = match planned {
            Ok(plan) => plan,
            Err(why) => return self.refuse(format!("import refused: {why}"), cx),
        };
        self.notices.outcome.clear();
        if plan.is_noop() {
            self.notices
                .outcome(Notice::status(files::nothing_to_change(
                    plan.rejected.len(),
                )));
        }
        if !plan.rejected.is_empty() {
            self.notices
                .outcome(Notice::warning(files::rejected_notice(&plan.rejected)));
        }
        if !plan.is_noop() {
            let held = HeldImport {
                tag,
                name,
                from,
                file,
                plan,
                window: window.window_handle(),
            };
            if self.keyboard_free(window, cx) {
                self.ask_import(held, window, cx);
            } else {
                self.notices
                    .outcome(Notice::status(files::ready(&held.file)));
                self.held = Some(held);
            }
        }
        self.rebuild_chrome();
        cx.notify();
    }

    /// Whether an import planned for classification `name` over source
    /// column `from` may still be asked about; else the notice refusing it.
    fn import_current(&self, name: &str, from: &str, file: &str) -> Result<(), Notice> {
        if self.shown().as_deref() != Some(name) {
            return Err(Notice::status(files::not_applied(file, name)));
        }
        if self.config_dim().is_none_or(|d| d.from != from) {
            return Err(Notice::status(files::not_applied_from(file, from)));
        }
        Ok(())
    }

    /// Whether the tile may take the keyboard for a question nobody just
    /// asked for: it is the focused tile, nothing in it holds the keys (a
    /// `/` search, the label editor, a prompt, a question), and no input
    /// outside it does (focus is on the shell root or within the tile).
    fn keyboard_free(&self, window: &Window, cx: &mut Context<Self>) -> bool {
        if !self.focused
            || self.find_entry.is_some()
            || self.editor.is_some()
            || self.prompt.is_some()
            || self.confirm.is_some()
        {
            return false;
        }
        window.focused(cx).is_none() || self.table.focus_handle(cx).within_focused(window, cx)
    }

    /// Arm the import's question.
    fn ask_import(&mut self, held: HeldImport, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(cx);
        let question = files::import_question(&held.file, &held.plan);
        confirm::arm(
            self,
            Pending::Import {
                name: held.name,
                from: held.from,
                file: held.file,
                plan: held.plan,
            },
            question,
            window,
            cx,
        );
    }

    /// Offer a held import once the current update is over: the tile may
    /// have just gained focus or closed what held the keys. Deferred, as
    /// arming focuses and notifies, and `set_focused` arrives during the
    /// shell's draw, where a notify is dropped.
    fn offer_held_later(&self, cx: &mut Context<Self>) {
        let Some(handle) = self.held.as_ref().map(|h| h.window) else {
            return;
        };
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                this.update(cx, |t, cx| t.offer_held(window, cx)).ok();
            });
        });
    }

    /// Ask about the held import if the tile may take the keyboard now,
    /// checked again first: the classification, its source column or the
    /// latest file operation may have changed while it waited.
    fn offer_held(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.held.is_none() || !self.keyboard_free(window, cx) {
            return;
        }
        let Some(held) = self.held.take() else {
            return;
        };
        if held.tag != self.file_tag {
            return;
        }
        if let Err(notice) = self.import_current(&held.name, &held.from, &held.file) {
            return self.refuse(notice, cx);
        }
        self.notices
            .outcome
            .retain(|n| *n != Notice::status(files::ready(&held.file)));
        self.ask_import(held, window, cx);
        self.rebuild_chrome();
        cx.notify();
    }

    /// `y` on an import: the plan applied over the object shown now (a
    /// label written meanwhile on a row the file does not name stays), as
    /// one write and one undo step, through the label verbs' gates.
    /// A reload that changed the source column meanwhile is refused: the
    /// plan's sources are the old column's values.
    fn apply_import(
        &mut self,
        name: String,
        from: String,
        file: String,
        plan: ImportPlan,
        cx: &mut Context<Self>,
    ) {
        if self.shown().as_deref() != Some(name.as_str()) {
            return self.refuse(Notice::status(files::not_applied(&file, &name)), cx);
        }
        if self.refuse_while_reverting(cx) {
            return;
        }
        let Some(config_dim) = self.config_dim() else {
            return self.refuse(Notice::status(files::not_applied(&file, &name)), cx);
        };
        if config_dim.from != from {
            return self.refuse(Notice::status(files::not_applied_from(&file, &from)), cx);
        }
        self.notices.outcome.clear();
        let checked = self.source_writable(&config_dim);
        if let Err(why) = checked {
            return self.refuse(format!("not saved: {why}"), cx);
        }
        let (next, entry) = plan.apply(self.history.current(&config_dim));
        let rows = entry.changes.len();
        if rows == 0 {
            // Every row the file names already says what it says.
            return self.refuse(Notice::status(files::nothing_to_change(0)), cx);
        }
        self.history.record(&config_dim, next.clone(), entry);
        self.frame.queue_config_edits(
            vec![ConfigEdit {
                doc: DIMENSIONS_DOC,
                object: config_dim.name.clone(),
                value: Some(classification::to_toml(&next)),
                origin: None,
            }],
            cx,
        );
        self.notices
            .outcome(Notice::status(files::imported(rows, &name)));
        self.rebuild_rows(true, cx);
        cx.notify();
    }

    /// The shown classification and the layer of the copy under the
    /// user's, when one stands there.
    fn revertible(&self) -> Result<(String, Layer), String> {
        let Some(name) = self.shown() else {
            return Err(NOTHING_SHOWN.into());
        };
        let config = self.shared.config.borrow();
        match config.as_ref().and_then(|c| c.shadowed.get(&name)) {
            Some(&under) => Ok((name, under)),
            None => Err(format!("{name} has no copy beneath yours to revert to")),
        }
    }

    /// Open the `kind` menu, or close it when it is the one open. The other
    /// kind's open menu is replaced, not stacked. A switcher with nothing to
    /// list refuses into the header rather than paint an empty list.
    fn toggle_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        self.menu_at = None;
        if self.menu.as_ref().is_some_and(|(k, _)| *k == kind) {
            self.menu = None;
        } else {
            let rows = match kind {
                MenuKind::Switch => self.switch_rows(),
                MenuKind::Actions => self.action_rows(),
            };
            if rows.is_empty() {
                self.menu = None;
                self.notices.nothing_to_switch = true;
                self.rebuild_chrome();
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
        self.menu_at = None;
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Show `name`. The cursor, the filter, a selection and the label
    /// history belonged to the previous classification, so they are
    /// dropped; the sort is the tile's and stays.
    fn show(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.state.name.as_deref() != Some(name) {
            self.state.name = Some(name.to_string());
            self.state.cursor = None;
            self.grid = GridModel::new();
            self.grid.set_sort(self.state.sort);
            self.find_entry = None;
            self.history.forget();
            self.notices.outcome.clear();
        }
        self.was_shown = self.shown().is_some();
        self.ask_values(false);
        self.rebuild_rows(false, cx);
        cx.notify();
    }

    /// The configuration's object for the shown classification.
    fn config_dim(&self) -> Option<DerivedDimension> {
        let name = self.state.name.as_deref()?;
        let config = self.shared.config.borrow();
        config.as_ref()?.dims.get(name).cloned()
    }

    /// The label of the cursor's row, trimmed; `None` when unclassified.
    /// The outer `None`: no cursor.
    fn cursor_label(&self) -> Option<Option<String>> {
        let at = self.grid.cursor()?;
        let row = self.grid.row(self.grid.visible()[at]);
        Some(label_text(row).map(str::to_string))
    }

    /// Whether the shown classification's source column is one a
    /// classification may map. A hand-written definition over another
    /// column is never written: the door would write it even when the
    /// reload then rejected it.
    fn source_writable(&self, config_dim: &DerivedDimension) -> Result<(), String> {
        let config = self.shared.config.borrow();
        let config = config.as_ref().expect("config_dim found it");
        validate_source(&config_dim.from, &config.schema, &config.dims)
    }

    /// Write one label change through the config door: validated, recorded
    /// in the history, queued whole-object, and shown at once (the pending
    /// edit) ahead of the reload that carries it. A classification whose
    /// source column is not one a classification may map (a hand-written
    /// definition) is never written: the door would write it even when the
    /// reload then rejected it.
    fn write(&mut self, write: Write, cx: &mut Context<Self>) {
        if self.refuse_while_reverting(cx) {
            return;
        }
        self.notices.outcome.clear();
        let Some(config_dim) = self.config_dim() else {
            return;
        };
        let checked = self.source_writable(&config_dim);
        if let Err(why) = checked {
            self.notices
                .outcome(Notice::danger(format!("not saved: {why}")));
            self.rebuild_chrome();
            cx.notify();
            return;
        }
        let before = self.history.current(&config_dim).clone();
        let mut changed = false;
        let done = match write {
            Write::Assign(sources, label) => self
                .history
                .apply(&config_dim, &sources, label.as_deref())
                .map(|next| (next, Vec::new())),
            Write::Undo => self.history.undo(&config_dim).or_else(|| {
                self.notices.outcome(Notice::status("nothing to undo"));
                None
            }),
            Write::Redo => self.history.redo(&config_dim).or_else(|| {
                self.notices.outcome(Notice::status("nothing to redo"));
                None
            }),
        };
        if let Some((next, skipped)) = done {
            // The tile's own refused write left its rows as they were; only
            // the rest were changed by another surface.
            let unsaved = self.history.unsaved(&skipped);
            match unsaved {
                0 => {}
                1 => self
                    .notices
                    .outcome(Notice::warning("1 row was not saved and is left as it is")),
                n => self.notices.outcome(Notice::warning(format!(
                    "{n} rows were not saved and are left as they are"
                ))),
            }
            match skipped.len() - unsaved {
                0 => {}
                1 => self
                    .notices
                    .outcome(Notice::warning("1 row changed elsewhere was left as it is")),
                n => self.notices.outcome(Notice::warning(format!(
                    "{n} rows changed elsewhere were left as they are"
                ))),
            }
            // A replay that skipped every row changes nothing to write.
            if next != before {
                self.frame.queue_config_edits(
                    vec![ConfigEdit {
                        doc: DIMENSIONS_DOC,
                        object: config_dim.name.clone(),
                        value: Some(classification::to_toml(&next)),
                        origin: None,
                    }],
                    cx,
                );
                changed = true;
            }
        }
        // Only a change is a relabel: one ends the selection and drops a
        // waiting cursor, which a verb that did nothing must not do.
        if changed {
            self.rebuild_rows(true, cx);
        } else {
            self.rebuild_chrome();
        }
        cx.notify();
    }

    /// `enter`/`c`: open the label editor over the verb's targets,
    /// prefilled with their label when they all share one, the text
    /// selected so typing replaces it.
    fn open_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_while_reverting(cx) {
            return;
        }
        let targets = self.grid.targets();
        if targets.is_empty() {
            return;
        }
        // The editor paints on the cursor's row and writes `targets`: a
        // resting cursor keeps its index across a rebuild, so a reload or a
        // values answer reordering the rows would leave the field painted
        // on a row it does not write. Put on its row, the cursor follows it.
        self.grid.put_cursor();
        let Some(config_dim) = self.config_dim() else {
            return;
        };
        let dim = self.history.current(&config_dim);
        let labels = classification::labels(dim);
        let prefill = editor::prefill(targets.iter().map(|s| {
            dim.values
                .get(s)
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
        }))
        .map(str::to_string);
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |s, cx| {
            EditCaret::Select.seed(s, prefill.clone().unwrap_or_default(), window, cx)
        });
        // Every keystroke re-ranks; typing after a moved highlight makes
        // it a guess again. The subscription dies with the field.
        cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                let query = input.read(cx).value().to_string();
                let changed = match &mut this.editor {
                    Some(e) if &e.input == input => {
                        let changed = e.list.set_query(&query);
                        if changed {
                            e.moved = false;
                        }
                        changed
                    }
                    _ => false,
                };
                if changed {
                    this.sync_editor(cx);
                }
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.editor = Some(LabelEditor::new(input, labels, prefill.as_deref(), targets));
        self.field_window = Some(window.window_handle());
        self.sync_editor(cx);
        cx.notify();
    }

    /// `enter` in the editor: close it and write what the commit rule
    /// picks (`editor::commit_value`). The text is re-read: `set_value`
    /// emits no change event.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut e) = self.editor.take() else {
            return;
        };
        let text = e.input.read(cx).value().to_string();
        let label = e.commit(&text);
        let targets = std::mem::take(&mut e.targets);
        self.drop_editor(e, window, cx);
        self.write(Write::Assign(targets, label), cx);
    }

    /// Close the editor with nothing written: `escape`, a press outside its
    /// list, any other verb.
    pub(crate) fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = self.editor.take() {
            self.drop_editor(e, window, cx);
            cx.notify();
        }
    }

    /// Blur the field if it owns focus before it is dropped: otherwise the
    /// shell cannot restore focus once the field is gone.
    fn drop_editor(&mut self, e: LabelEditor, window: &mut Window, cx: &mut Context<Self>) {
        if e.input.read(cx).focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
        drop(e);
        self.sync_editor(cx);
        self.offer_held_later(cx);
    }

    /// Drop the editor where no window is at hand, blurring it later
    /// through the window it opened in if it still owns focus (a newer
    /// field is never blurred).
    fn release_editor(&mut self, cx: &mut App) {
        let Some(e) = self.editor.take() else {
            return;
        };
        self.release_field(e.input, cx);
    }

    /// [`Self::release_editor`] for the prompt.
    fn release_prompt(&mut self, cx: &mut App) {
        if let Some(p) = self.prompt.take() {
            self.release_field(p.input, cx);
        }
    }

    fn release_field(&self, input: Entity<InputState>, cx: &mut App) {
        let focus = input.read(cx).focus_handle(cx);
        drop(input);
        if let Some(handle) = self.field_window {
            App::defer(cx, move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    if focus.is_focused(window) {
                        window.blur(cx);
                    }
                });
            });
        }
    }

    /// `up`/`down` in the editor move the highlight: a choice now, which
    /// `enter` takes whatever is typed.
    fn choice_step(&mut self, delta: i64, cx: &mut Context<Self>) {
        if let Some(e) = self.editor.as_mut() {
            e.list.nav(NavCommand::Move(delta));
            e.moved = true;
            self.sync_editor(cx);
        } else if let Some(p) = self.prompt.as_mut()
            && let Some(list) = p.list.as_mut()
        {
            list.nav(NavCommand::Move(delta));
            p.repaint();
            cx.notify();
        }
    }

    /// Hover lights a list row without making it a choice: a pointer
    /// passing over the list must not change what `enter` writes.
    pub(crate) fn choice_hover(&mut self, row: usize, cx: &mut Context<Self>) {
        let changed = self
            .editor
            .as_mut()
            .is_some_and(|e| e.list.highlighted() != row && e.list.set_highlighted(row));
        if changed {
            self.sync_editor(cx);
        } else if let Some(p) = self.prompt.as_mut()
            && let Some(list) = p.list.as_mut()
            && list.highlighted() != row
            && list.set_highlighted(row)
        {
            p.repaint();
            cx.notify();
        }
    }

    /// A press on a list row: that label, written at once.
    pub(crate) fn choice_pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let picked = self.editor.as_mut().and_then(|e| {
            e.list.set_highlighted(row).then(|| {
                e.moved = true;
                (
                    e.input.clone(),
                    e.list.highlighted_text().unwrap_or_default().to_string(),
                )
            })
        });
        if let Some((input, text)) = picked {
            input.update(cx, |s, cx| s.set_value(text, window, cx));
            self.commit_edit(window, cx);
            return;
        }
        // The prompt's column list: that column answers the step.
        let picked = self.prompt.as_mut().and_then(|p| {
            let list = p.list.as_mut()?;
            list.set_highlighted(row).then(|| {
                (
                    p.input.clone(),
                    list.highlighted_text().unwrap_or_default().to_string(),
                )
            })
        });
        if let Some((input, text)) = picked {
            input.update(cx, |s, cx| s.set_value(text, window, cx));
            self.commit_prompt(window, cx);
        }
    }

    /// Open the prompt asking `prompt`, the field focused. A rename seeds
    /// the current name, selected, so typing replaces it.
    fn open_prompt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        self.close_prompt(window, cx);
        let seed = match &prompt {
            Prompt::Rename { from } => from.clone(),
            Prompt::NewName | Prompt::NewColumn { .. } => String::new(),
        };
        let placeholder = editor::placeholder(&prompt);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |s, cx| EditCaret::Select.seed(s, seed, window, cx));
        // The column step's list re-ranks as the column is typed. The
        // subscription dies with the field.
        cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                let query = input.read(cx).value().to_string();
                if let Some(p) = this.prompt.as_mut().filter(|p| &p.input == input)
                    && let Some(list) = p.list.as_mut()
                    && list.set_query(&query)
                {
                    p.repaint();
                    cx.notify();
                }
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.prompt = Some(PromptField::new(input, prompt, self.columns()));
        self.field_window = Some(window.window_handle());
        cx.notify();
    }

    /// The columns a new classification may map.
    fn columns(&self) -> Vec<String> {
        let config = self.shared.config.borrow();
        config
            .as_ref()
            .map(|c| source_columns(&c.schema, &c.dims))
            .unwrap_or_default()
    }

    /// `enter` in the prompt: the answer through `prompt::submit`. A
    /// refusal stays on the bar with the field open; the name step moves
    /// on to the column; the last step closes the prompt and creates, or
    /// asks y/n to rename.
    fn commit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.as_mut() else {
            return;
        };
        let text = p.input.read(cx).value().to_string();
        let answer = p.answer(&text);
        let step = {
            let config = self.shared.config.borrow();
            match config.as_ref() {
                Some(c) => prompt::submit(&p.prompt, &answer, &c.schema, &c.dims),
                None => Step::Refuse("the configuration has not loaded yet".into()),
            }
        };
        match step {
            Step::Refuse(why) => {
                p.error = Some(why.into());
                p.repaint();
            }
            Step::Next(next) => {
                let input = p.input.clone();
                let placeholder = editor::placeholder(&next);
                input.update(cx, |s, cx| {
                    s.set_value("", window, cx);
                    s.set_placeholder(placeholder, window, cx);
                });
                let columns = self.columns();
                if let Some(p) = self.prompt.as_mut() {
                    p.ask(next, columns);
                }
            }
            Step::Create { name, from } => {
                self.close_prompt(window, cx);
                self.create(name, from, cx);
            }
            Step::Rename { from, to } => {
                self.close_prompt(window, cx);
                self.ask_rename(from, to, window, cx);
            }
        }
        cx.notify();
    }

    /// Close the prompt with nothing written: `escape`, a press on the
    /// grid, any other verb.
    fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.take() else {
            return;
        };
        if p.input.read(cx).focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
        self.offer_held_later(cx);
        cx.notify();
    }

    /// Queue `edits` as one batch and show `awaiting.shows` meanwhile. The
    /// shown classification's history ends with its name.
    fn write_objects(
        &mut self,
        edits: Vec<ConfigEdit>,
        awaiting: Awaiting,
        cx: &mut Context<Self>,
    ) {
        self.frame.queue_config_edits(edits, cx);
        self.history.forget();
        self.awaiting = Some(awaiting.clone());
        match &awaiting.shows {
            Some(name) => self.show(name, cx),
            None => {
                self.state.name = None;
                self.state.cursor = None;
                self.was_shown = false;
                let rows = self.switch_rows();
                self.menu =
                    (!rows.is_empty()).then(|| (MenuKind::Switch, Menu::new(rows, &self.chords)));
                self.ask_values(false);
                self.rebuild_rows(false, cx);
            }
        }
        cx.notify();
    }

    /// Write an empty classification `name` over `from` and show it.
    fn create(&mut self, name: String, from: String, cx: &mut Context<Self>) {
        self.notices.outcome.clear();
        let dim = DerivedDimension {
            name: name.clone(),
            from,
            values: Default::default(),
        };
        let restores = self.state.name.clone();
        self.write_objects(
            vec![set_edit(&dim)],
            Awaiting {
                shows: Some(name),
                restores,
                removed: None,
            },
            cx,
        );
    }

    /// Who still names `name`: groupings, views, saved scopes and named
    /// expressions. None of them is rewritten by a rename or a delete.
    fn references_summary(&self, name: &str, cx: &App) -> Option<(String, usize)> {
        let frame = self.frame.entity().read(cx);
        let config = self.shared.config.borrow();
        let views = config.as_ref().map(|c| c.views.as_slice()).unwrap_or(&[]);
        let refs = references(
            name,
            frame.slots(),
            views,
            frame.saved_scopes(),
            frame.named_expressions(),
        );
        let n =
            refs.groupings.len() + refs.views.len() + refs.scopes.len() + refs.expressions.len();
        (!refs.is_empty()).then(|| (refs.summary(), n))
    }

    /// `{summary} still say(s) '{name}'`, or `nothing refers to it`.
    fn reference_clause(&self, name: &str, cx: &App) -> String {
        match self.references_summary(name, cx) {
            None => "nothing refers to it".to_string(),
            Some((summary, 1)) => format!("{summary} still says '{name}'"),
            Some((summary, _)) => format!("{summary} still say '{name}'"),
        }
    }

    fn ask_rename(
        &mut self,
        from: String,
        to: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let clause = self.reference_clause(&from, cx);
        let question = format!("rename {from} \u{2192} {to}: {clause} \u{2014} y renames");
        confirm::arm(self, Pending::Rename { from, to }, question, window, cx);
        cx.notify();
    }

    /// Say why a verb cannot act, in the header: a danger notice unless
    /// `why` is already one.
    fn refuse(&mut self, why: impl Into<Why>, cx: &mut Context<Self>) {
        self.notices.outcome.clear();
        self.notices.outcome(why.into().0);
        self.rebuild_chrome();
        cx.notify();
    }

    /// `y` on a rename: the new object and the old one's removal in one
    /// batch, validated again (the configuration may have moved while the
    /// question stood).
    fn rename(&mut self, from: String, to: String, cx: &mut Context<Self>) {
        let checked = {
            let config = self.shared.config.borrow();
            config.as_ref().and_then(|c| {
                // The shown object as the tile has it: a label edit still
                // on its way to the old name goes with the rename, not
                // into the old name's removal.
                let dim = c.dims.get(&from)?;
                let dim = if self.state.name.as_deref() == Some(from.as_str()) {
                    self.history.current(dim).clone()
                } else {
                    dim.clone()
                };
                Some(prompt::submit(
                    &Prompt::Rename { from: from.clone() },
                    &to,
                    &c.schema,
                    &c.dims,
                ))
                .map(|step| (dim, step))
            })
        };
        let dim = match checked {
            Some((dim, Step::Rename { .. })) => dim,
            Some((_, Step::Refuse(why))) => return self.refuse(format!("not renamed: {why}"), cx),
            _ => return self.refuse(format!("not renamed: {from} no longer exists"), cx),
        };
        self.notices.outcome.clear();
        let renamed = DerivedDimension {
            name: to.clone(),
            ..dim
        };
        self.write_objects(
            vec![set_edit(&renamed), remove_edit(&from)],
            Awaiting {
                shows: Some(to),
                restores: Some(from.clone()),
                removed: Some(from),
            },
            cx,
        );
    }

    /// Whether the editor's field owns window focus.
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        let field = self
            .editor
            .as_ref()
            .map(|e| &e.input)
            .into_iter()
            .chain(self.prompt.as_ref().map(|p| &p.input))
            .any(|i| i.read(cx).focus_handle(cx).is_focused(window));
        field || self.confirm.as_ref().is_some_and(|c| c.holds_focus(window))
    }

    /// A press on shown row `row`: a double-click opens the label editor
    /// on it, shift extends a row selection to it, a plain press moves the
    /// cursor there and ends a selection.
    fn row_pressed(&mut self, e: RowPressed, window: &mut Window, cx: &mut Context<Self>) {
        self.user_acted(cx);
        // A press on the grid leaves the editor or the prompt unwritten, as
        // escape would.
        self.close_editor(window, cx);
        self.close_prompt(window, cx);
        self.grid.click(e.row, e.shift);
        self.sync_table(false, cx);
        if e.clicks >= 2 && !e.shift {
            let edit = ActionId("classifications::edit".to_string());
            self.dispatch(&edit, None, window, cx);
        }
        cx.notify();
    }

    /// A right press on shown row `row`: the cursor moves there (a row of a
    /// live selection keeps the selection, which the menu then acts on),
    /// and the `⋯` menu opens hung from the pointer, as `.` opens it from
    /// the header.
    fn row_context(&mut self, e: RowContext, window: &mut Window, cx: &mut Context<Self>) {
        self.user_acted(cx);
        confirm::cancel(self, window, cx);
        self.close_editor(window, cx);
        self.close_prompt(window, cx);
        let in_selection = self.grid.selected().is_some_and(|s| s.contains(&e.row));
        if !in_selection {
            self.grid.click(e.row, false);
        }
        // The table's own right-press row outline: the tile paints the
        // cursor and the selection itself.
        self.table
            .update(cx, |t, cx| t.set_right_clicked_row(None, cx));
        self.sync_table(false, cx);
        self.menu = Some((
            MenuKind::Actions,
            Menu::new(self.action_rows(), &self.chords).open_at(None),
        ));
        self.menu_at = Some(e.position);
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
                self.offer_held_later(cx);
                q
            }
            FindEvent::Cancelled => match self.find_entry.take() {
                Some(entry) => {
                    self.offer_held_later(cx);
                    entry
                }
                None => return,
            },
        };
        self.grid.set_filter(&query);
        self.sync_table(true, cx);
        cx.notify();
    }

    /// `insert` while the label editor or the prompt is open (the shell
    /// then routes bare keys to its field) or a y/n question holds the
    /// keyboard, `menu` while a menu is up, `visual` while a row selection
    /// is live, `normal` otherwise.
    fn mode(&self) -> &'static str {
        if self.editor.is_some() || self.prompt.is_some() || self.confirm.is_some() {
            "insert"
        } else if self.menu.is_some() {
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
        self.user_acted(cx);
        // A verb arriving under an armed question (the palette; a key never
        // gets here, the question takes every key) answers no first. For
        // `cancel` that answer is the whole of it: the notices wait.
        let answered = confirm::cancel(self, window, cx);
        if answered && action.0 == "classifications::cancel" {
            return true;
        }
        // The prompt's own keys; any other verb closes it unwritten first.
        if self.prompt.is_some() {
            match action.0.as_str() {
                "classifications::commit" => {
                    self.commit_prompt(window, cx);
                    return true;
                }
                "classifications::cancel" => {
                    self.close_prompt(window, cx);
                    return true;
                }
                "classifications::choice_up" => {
                    self.choice_step(-1, cx);
                    return true;
                }
                "classifications::choice_down" => {
                    self.choice_step(1, cx);
                    return true;
                }
                _ => self.close_prompt(window, cx),
            }
        }
        // The editor's own keys; any other verb closes it unwritten first.
        if self.editor.is_some() {
            match action.0.as_str() {
                "classifications::commit" => {
                    self.commit_edit(window, cx);
                    return true;
                }
                "classifications::cancel" => {
                    self.close_editor(window, cx);
                    return true;
                }
                "classifications::choice_up" => {
                    self.choice_step(-1, cx);
                    return true;
                }
                "classifications::choice_down" => {
                    self.choice_step(1, cx);
                    return true;
                }
                _ => self.close_editor(window, cx),
            }
        }
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
            // Last in line: nothing above had anything to cancel, so
            // `escape` dismisses every warning and danger notice showing,
            // each as a click on it would. Nothing to dismiss: unhandled.
            "classifications::cancel" => {
                let shown = self.dismissed.visible(self.chrome.notices.iter().cloned());
                let mut any = false;
                for n in &shown {
                    any |= self.dismiss_notice(n);
                }
                if !any {
                    return false;
                }
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
            EXPORT_ACTION | EXPORT_ALL_ACTION => {
                self.close_menu(cx);
                self.ask_export_path(action.0 == EXPORT_ALL_ACTION, cx);
            }
            IMPORT_ACTION => {
                self.close_menu(cx);
                self.ask_import_path(cx);
            }
            // The classification's own verbs, from the palette or the menu.
            NEW_ACTION => {
                self.close_menu(cx);
                self.notices.outcome.clear();
                self.rebuild_chrome();
                self.open_prompt(Prompt::NewName, window, cx);
            }
            RENAME_ACTION => {
                self.close_menu(cx);
                match self.own(Verb::Rename) {
                    Ok(from) => {
                        self.notices.outcome.clear();
                        self.rebuild_chrome();
                        self.open_prompt(Prompt::Rename { from }, window, cx);
                    }
                    Err(refusal) => self.refuse(refusal.long, cx),
                }
            }
            DELETE_ACTION => {
                self.close_menu(cx);
                match self.own(Verb::Delete) {
                    Ok(name) => {
                        let clause = self.reference_clause(&name, cx);
                        let question = format!("delete {name}: {clause} \u{2014} y deletes");
                        self.notices.outcome.clear();
                        self.rebuild_chrome();
                        confirm::arm(self, Pending::Delete { name }, question, window, cx);
                        cx.notify();
                    }
                    Err(refusal) => self.refuse(refusal.long, cx),
                }
            }
            REVERT_ACTION => {
                self.close_menu(cx);
                match self.revertible() {
                    Ok((name, under)) => {
                        let question = format!(
                            "revert {name} to the {} copy \u{2014} y reverts",
                            under.name()
                        );
                        self.notices.outcome.clear();
                        self.rebuild_chrome();
                        confirm::arm(self, Pending::Revert { name }, question, window, cx);
                        cx.notify();
                    }
                    Err(why) => self.refuse(why, cx),
                }
            }
            // The label verbs act on the selection, else the cursor's row.
            "classifications::edit" if self.menu.is_none() => self.open_editor(window, cx),
            "classifications::clear" if self.menu.is_none() => {
                self.write(Write::Assign(self.grid.targets(), None), cx)
            }
            "classifications::yank" if self.menu.is_none() => {
                if let Some(label) = self.cursor_label() {
                    self.register = Some(label);
                }
            }
            "classifications::paste" if self.menu.is_none() => match self.register.clone() {
                Some(label) => self.write(Write::Assign(self.grid.targets(), label), cx),
                None => {
                    self.notices.outcome.clear();
                    self.notices.outcome(Notice::status(NOTHING_COPIED));
                    self.rebuild_chrome();
                    cx.notify();
                }
            },
            "classifications::undo" if self.menu.is_none() => self.write(Write::Undo, cx),
            "classifications::redo" if self.menu.is_none() => self.write(Write::Redo, cx),
            _ => return false,
        }
        true
    }

    /// The trader acted in the tile (a key or a press): the session
    /// restore's notices have been seen, and go.
    fn user_acted(&mut self, cx: &mut Context<Self>) {
        if !self.notices.restore.is_empty() {
            self.notices.restore.clear();
            self.rebuild_chrome();
            cx.notify();
        }
    }

    /// A values read answers whether the tile is shown or not, so being
    /// shown or hidden changes nothing.
    pub fn set_visible(&mut self, _visible: bool) {}

    /// The shell's word on whether this is the focused tile, given from its
    /// render. No notify here: one sent during the draw is dropped. A held
    /// import is offered once the draw is over.
    pub fn set_focused(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.focused = focused;
        if focused {
            self.offer_held_later(cx);
        }
    }

    pub fn closed(&mut self, cx: &mut Context<Self>) {
        self.held = None;
        self.planning = None;
        self.menu = None;
        self.release_editor(cx);
        self.release_prompt(cx);
        let _ = confirm::withdraw(self, cx);
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

    /// The open editor's text, its labels in ranked order, and the
    /// highlighted one.
    #[cfg(test)]
    fn editor_state(&self, cx: &App) -> Option<(String, Vec<String>, Option<String>)> {
        let e = self.editor.as_ref()?;
        Some((
            e.input.read(cx).value().to_string(),
            e.list
                .ranked()
                .iter()
                .map(|r| e.list.options()[r.row].clone())
                .collect(),
            e.list.highlighted_text().map(str::to_string),
        ))
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
                self.notices.outcome(Notice::danger(why.to_string()));
                self.rebuild_chrome();
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
                // From the header's `⋯` its top-right corner hangs under the
                // control; from a right press its top-left at the pointer.
                let corner = if self.menu_at.is_some() {
                    gpui::Anchor::TopLeft
                } else {
                    gpui::Anchor::TopRight
                };
                menu::render_menu(m, &ids, corner, &tile, close(MenuKind::Actions), cx)
            });
        let (actions, row_actions) = match self.menu_at {
            Some(at) => (
                None,
                actions.map(|m| {
                    anchored()
                        .position_mode(AnchoredPositionMode::Window)
                        .position(at)
                        .child(m)
                }),
            ),
            None => (actions, None),
        };
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
                notices: self.dismissed.visible(self.chrome.notices.iter().cloned()),
                on_dismiss: notice::on_dismiss_with(&tile, |t: &mut ClassificationsTile, n, _| {
                    t.dismiss_notice(n)
                }),
                actions_open: actions.is_some() || row_actions.is_some(),
                switcher,
                menu_selector: self.menu_selector.clone(),
                menu_tip: self.menu_tip.clone(),
                switch_tip: self.switch_tip.clone(),
            },
            theme,
        );
        // The prompt or the y/n question, on its own bar under the header,
        // whole at any tile width.
        let question = self.confirm.as_ref().map(|pending| {
            confirm::bar(
                pending,
                &tile,
                move || format!("classifications-confirm-{id}"),
                theme,
            )
        });
        let prompt = self
            .prompt
            .as_ref()
            .map(|p| editor::render_prompt(p, &tile, id, cx));
        let theme = cx.theme();
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
        let root = v_flex()
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
            .children(question)
            .children(prompt)
            .child(body)
            .children(row_actions);
        // A pointer press anywhere on the tile answers an armed question no.
        confirm::cancel_on_press(root, self.confirm.is_some(), &tile)
    }
}

impl ConfirmHost for ClassificationsTile {
    type Payload = Pending;

    fn confirm_slot(&mut self) -> &mut Option<Confirm<Pending>> {
        &mut self.confirm
    }

    fn confirmed(&mut self, pending: Pending, _: &mut Window, cx: &mut Context<Self>) {
        match pending {
            Pending::Rename { from, to } => self.rename(from, to, cx),
            Pending::Delete { name } => {
                self.notices.outcome.clear();
                self.write_objects(
                    vec![remove_edit(&name)],
                    Awaiting {
                        shows: None,
                        restores: Some(name.clone()),
                        removed: Some(name),
                    },
                    cx,
                );
            }
            Pending::Import {
                name,
                from,
                file,
                plan,
            } => self.apply_import(name, from, file, plan, cx),
            Pending::Revert { name } => {
                self.notices.outcome.clear();
                self.frame.queue_config_edits(vec![remove_edit(&name)], cx);
                self.reverting = Some(name);
                self.history.forget();
                self.rebuild_rows(false, cx);
                cx.notify();
            }
        }
        self.offer_held_later(cx);
    }

    fn cancelled(&mut self, pending: Pending, _: &mut Window, cx: &mut Context<Self>) {
        let said = match pending {
            Pending::Rename { from, .. } => format!("{from} not renamed"),
            Pending::Delete { name } => format!("{name} not deleted"),
            Pending::Revert { name } => format!("{name} not reverted"),
            Pending::Import { file, .. } => format!("{file} not imported"),
        };
        self.notices.outcome.clear();
        self.notices.outcome(Notice::status(said));
        self.rebuild_chrome();
        cx.notify();
        self.offer_held_later(cx);
    }
}

/// The verbs that remove the shown classification's user definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Rename,
    Delete,
}

/// Why a verb cannot act: a few words for a menu row's lane, a sentence
/// for the header.
struct Blocked {
    short: &'static str,
    long: String,
}

impl Blocked {
    fn same(text: &'static str) -> Blocked {
        Blocked {
            short: text,
            long: text.to_string(),
        }
    }
}

/// What [`ClassificationsTile::refuse`] shows: a sentence is a danger
/// notice; a waiting state passes its own status notice.
struct Why(Notice);

impl From<Notice> for Why {
    fn from(notice: Notice) -> Self {
        Why(notice)
    }
}

impl From<String> for Why {
    fn from(why: String) -> Self {
        Why(Notice::danger(why))
    }
}

/// What the menu and the verbs say while nothing is shown.
const NOTHING_SHOWN: &str = "no classification shown";

/// What `p` says with nothing copied.
const NOTHING_COPIED: &str = "nothing copied: y y copies a label";

/// What a refused file request says.
fn busy_text(refusal: Refusal) -> String {
    match refusal {
        Refusal::Busy => format!("{refusal} \u{2014} try again"),
        Refusal::Stopped => refusal.to_string(),
    }
}

/// Where the first save dialog opens: the home directory, else the
/// working one.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// The whole object `dim`, written.
fn set_edit(dim: &DerivedDimension) -> ConfigEdit {
    ConfigEdit {
        doc: DIMENSIONS_DOC,
        object: dim.name.clone(),
        value: Some(classification::to_toml(dim)),
        origin: None,
    }
}

/// `name`'s user definition, removed.
fn remove_edit(name: &str) -> ConfigEdit {
    ConfigEdit {
        doc: DIMENSIONS_DOC,
        object: name.to_string(),
        value: None,
        origin: None,
    }
}

#[cfg(test)]
mod tests;
