//! The shell-hosted watchlist entity. It shows one list from the resolved
//! snapshot in `WatchlistGlobal`, answers the shell's door
//! (`crate::content::WatchlistContent`) and paints the tile: the header
//! (`header`), the members grid (`table`), the switcher hung beneath the
//! name, the `⋯` menu, the rules popup (`rules`) hung beneath the rules
//! count, the prompt field (`field`) and the y/n confirm bar (`objects`)
//! under the header, and an empty state while there is nothing to show.
//!
//! The tile issues no data request: the bridge's cache resolves every list
//! and replaces the global, which wakes the tile. The factory's
//! configuration snapshot (schema, dimensions, saved scopes, named
//! expressions) is what a rule is validated against.
//!
//! What paint reads is prepared whenever the snapshot, the reference
//! tables, the clock, the shown list, the sort or the filter changes
//! (`Chrome` for the header and the empty state, the grid's `Prepared`
//! rows for the table), never in render.

mod field;
mod header;
mod objects;
mod rules;
mod table;
#[cfg(test)]
mod tests;
mod verbs;

use std::rc::Rc;
use std::sync::Arc;

use geode_core::clock::Clock;
use geode_core::reference::ReferenceData;
use geode_core::sort::SortOrder;
use geode_core::watchlist::state::{Status, WatchlistSnapshot};
use geode_shell::actions::ActionId;
use geode_shell::clock::AppClock;
use geode_shell::frame::{FrameRef, TileNotice};
use geode_shell::keymap::{Binding, KeyContext, Keystroke};
use geode_shell::module::{CloseHandle, FindEvent, StackHandle};
use geode_shell::reference::ReferenceGlobal;
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use geode_shell::watchlist::WatchlistGlobal;
use geode_tile::confirm::{self, Confirm};
use geode_tile::header::{HEADER_HEIGHT, Mode, link_chips};
use geode_tile::menu::{self, ActionRow, Hint, Menu, MenuHost, MenuIds, MenuPick, Row};
use geode_tile::notice::{self, Dismissals, Notice};
use gpui::prelude::*;
use gpui::{
    AnchoredPositionMode, AnyWindowHandle, App, Context, Entity, Focusable as _, Pixels, Point,
    SharedString, Window, anchored, div,
};
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::content::{Shared, action_title};
use crate::core::grid::GridModel;
use crate::core::history::{History, Way};
use crate::core::prompt::Prompt;
use crate::core::rows;
use crate::core::rules::{RuleRow, RulesPopup, rule_rows};
use crate::core::session::{self, SortCol, State};
use field::PromptField;
use header::HeaderModel;
use objects::{Awaiting, Blocked, Pending, Verb};
use table::{GridDelegate, Prepared, RowContext, RowPressed, SortClicked};

/// Which menu is up: the switcher, hung under the header's name, or the
/// `⋯` action menu, hung from the header's right edge or from the pointer
/// of the right press that opened it. Both share menu mode, its keys and
/// its pick door; opening one replaces the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Switch,
    Actions,
}

/// What a menu row does when picked.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pick {
    /// Show this watchlist.
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

const ADD_ACTION: &str = "watchlist::add";
const REMOVE_ACTION: &str = "watchlist::remove";
const UNDO_ACTION: &str = "watchlist::undo";
const REDO_ACTION: &str = "watchlist::redo";
const REFRESH_ACTION: &str = "watchlist::refresh";
const RULES_ACTION: &str = "watchlist::rules";
const RULE_ADD_ACTION: &str = "watchlist::rule_add";
const RULE_REMOVE_ACTION: &str = "watchlist::rule_remove";
const RULE_EDIT_ACTION: &str = "watchlist::rule_edit";
const SWITCH_ACTION: &str = "watchlist::switch";
const MENU_ACTION: &str = "watchlist::menu";
const COMMIT_ACTION: &str = "watchlist::commit";
const CANCEL_ACTION: &str = "watchlist::cancel";
const VISUAL_ACTION: &str = "watchlist::visual_rows";
const CHOICE_UP_ACTION: &str = "watchlist::choice_up";
const CHOICE_DOWN_ACTION: &str = "watchlist::choice_down";
const NEW_ACTION: &str = "watchlist::new";
const CLONE_ACTION: &str = "watchlist::clone";
const RENAME_ACTION: &str = "watchlist::rename";
const DELETE_ACTION: &str = "watchlist::delete";
const REVERT_ACTION: &str = "watchlist::revert";

/// The switcher's refusal while there is nothing to list.
const NOTHING_TO_SWITCH: &str = "no watchlists to switch to";

/// Why a member verb's `⋯` row is disabled while no list is shown.
const NOTHING_SHOWN: &str = "no watchlist shown";

/// Why the Remove row is disabled with no row under the cursor.
const NO_ROW: &str = "no row";

/// Why Undo and Redo are disabled, and what the key says, while the
/// history has nothing that way.
const NOTHING_TO_UNDO: &str = "nothing to undo";
const NOTHING_TO_REDO: &str = "nothing to redo";

/// What `x` says when its targets changed nothing (no row, or names the
/// list does not hold).
const NOTHING_TO_REMOVE: &str = "nothing to remove";

/// What `shift+r` says on a tile hosted without the bridge's refresh hook.
const NOT_WIRED: &str = "resolve now is not wired";

/// What the standing resolution notices say the remedy is.
const RETRIES: &str = "\u{2014} shift+r retries";

/// The header's notices, by how long each lives. None stays for good: a
/// notice that outlived its cause would read as a standing fault.
#[derive(Default)]
struct Notices {
    /// What the session restore dropped. Cleared by the trader's first
    /// action in the tile (a key or a press): by then it has been seen,
    /// and the restore is over.
    restore: Vec<String>,
    /// The switcher refused with nothing to list. Cleared once a snapshot
    /// defines something to switch to.
    nothing_to_switch: bool,
    /// What the last verb, or the shell about its write, said. Cleared
    /// when the next verb starts and when another list is shown.
    outcome: Vec<Notice>,
}

impl Notices {
    fn outcome(&mut self, notice: Notice) {
        if !self.outcome.contains(&notice) {
            self.outcome.push(notice);
        }
    }
}

/// Prepared paint input.
#[derive(Default)]
struct Chrome {
    header: HeaderModel,
    /// The body's empty state, `None` while a watchlist is shown.
    empty: Option<SharedString>,
    notices: Vec<Notice>,
    /// The rules popup's rows: the shown list's rules as they are now
    /// (the pending object while an edit awaits its reload), with the
    /// snapshot's errors while those rules are the snapshot's.
    rules: Vec<RuleRow>,
}

/// The resolved lists, as the bridge last published them; empty for a
/// tile hosted without the global.
fn snapshot(cx: &App) -> Arc<WatchlistSnapshot> {
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

/// A chord in the keymap's own spelling (`g w`, `ctrl+r`): what names a
/// key inside a sentence, as `shell::kbd` documents.
fn spell(keystrokes: &[Keystroke]) -> String {
    keystrokes
        .iter()
        .map(|k| {
            let mut parts = Vec::new();
            if k.mods.ctrl {
                parts.push("ctrl");
            }
            if k.mods.alt {
                parts.push("alt");
            }
            if k.mods.shift {
                parts.push("shift");
            }
            if k.mods.cmd {
                parts.push("cmd");
            }
            parts.push(k.key.as_str());
            parts.join("+")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The installed display clock, or the machine's for a tile hosted
/// without `AppClock`.
fn clock(cx: &App) -> Clock {
    cx.try_global::<AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| Clock::machine().0)
}

pub struct WatchlistTile {
    id: TileId,
    frame: FrameRef,
    state: State,
    grid: GridModel,
    table: Entity<TableState<GridDelegate>>,
    /// The filter in force when a `/` search began, which a cancelled
    /// search restores; `None` while no search is open.
    find_entry: Option<String>,
    /// The rem the table's column widths were last scaled to.
    last_rem: f32,
    /// The live keymap the menus' hints are resolved against.
    chords: Arc<Vec<Binding>>,
    stack: Option<StackHandle>,
    close: Option<CloseHandle>,
    /// Painted in the header's cluster.
    notices: Notices,
    menu: Option<(MenuKind, Menu<Pick>)>,
    /// Where a right-click opened the `⋯` menu, in window coordinates; it
    /// hangs from that point instead of the header's `⋯` control. `None`
    /// for a menu the key, the palette or the control opened.
    menu_at: Option<Point<Pixels>>,
    /// The header notices the trader dismissed (a click, or `escape` with
    /// nothing else to do), each hidden while `chrome.notices` still
    /// reports it. Pruned in `rebuild_chrome`, never in render.
    dismissed: Dismissals,
    chrome: Chrome,
    menu_selector: SharedString,
    menu_tip: SharedString,
    switch_tip: SharedString,
    /// Whether a snapshot with lists has been settled yet: the first one
    /// is the tile's construction, as far as the switcher is concerned. A
    /// restored tile is built before the bridge publishes the lists, so an
    /// empty snapshot does not count.
    settled: bool,
    /// Whether the last settle showed a watchlist, so the next can tell a
    /// list going away from one never shown.
    was_shown: bool,
    /// The factory's shared state: the refresh hook `shift+r` calls.
    shared: Rc<Shared>,
    /// The shown list's edit history and its optimistic pending object.
    history: History,
    /// The open prompt field (the add field, or a rule step); the tile is
    /// in insert mode while it is.
    prompt: Option<PromptField>,
    /// The open rules popup; the tile is in `rules` mode while it is and
    /// no field is open.
    rules: Option<RulesPopup>,
    /// The window the prompt opened in, to blur its field where no window
    /// is at hand (a reload removing the list, a close).
    field_window: Option<AnyWindowHandle>,
    /// The armed y/n question (a rename, delete or revert); it holds the
    /// keyboard until answered.
    confirm: Option<Confirm<Pending>>,
    /// A create, clone, rename or delete written and shown ahead of the
    /// reload that carries it.
    awaiting: Option<Awaiting>,
    /// A confirmed revert on its way: the list whose user copy was
    /// removed, until the snapshot no longer shows a user copy of it.
    /// The member and rules verbs on it are refused meanwhile.
    reverting: Option<String>,
}

impl WatchlistTile {
    pub(crate) fn new(
        id: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        shared: Rc<Shared>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WatchlistTile {
        let (state, restore) = restored.map(session::from_table).unwrap_or_default();
        let mut grid = GridModel::new();
        grid.set_sort(state.sort);
        // A restored cursor lands on its name once the snapshot holds it.
        if let Some(name) = &state.cursor {
            grid.seed_cursor(name.clone());
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
        // refresh (a sort, the rem) keeps them. The table's other events
        // are its own: the cursor and selection come through `RowPressed`,
        // and the tile sets the table's selected row itself.
        cx.subscribe(&table, |this, _, e: &TableEvent, cx| {
            if let TableEvent::ColumnWidthsChanged(widths) = e {
                this.table
                    .update(cx, |t, _| t.delegate_mut().record_widths(widths));
            }
        })
        .detach();
        // The bridge replaces the global on every change to a list's
        // definition, members or state.
        cx.observe_global::<WatchlistGlobal>(|this, cx| this.snapshot_changed(cx))
            .detach();
        cx.observe_global::<ReferenceGlobal>(|this, cx| this.reference_changed(cx))
            .detach();
        // `as of` is on the display clock: a zone change re-prepares it.
        cx.observe_global::<AppClock>(|this, cx| this.clock_changed(cx))
            .detach();
        // A keymap reload re-resolves an open menu's hints and the empty
        // state's switch chord at once.
        cx.observe_global::<tips::Chords>(|this, cx| {
            this.chords = menu::live_bindings(cx);
            if let Some((_, m)) = this.menu.as_mut() {
                m.rehint(&this.chords);
            }
            this.rebuild_chrome(cx);
            cx.notify();
        })
        .detach();
        // The shell tells the tile about its config writes (a fork, a
        // refusal) by posting on the frame and notifying it.
        cx.observe(frame.entity(), |this, _, cx| this.frame_changed(cx))
            .detach();
        let mut tile = WatchlistTile {
            id,
            frame,
            state,
            grid,
            table,
            find_entry: None,
            last_rem: 0.0,
            chords: menu::live_bindings(cx),
            stack: None,
            close: None,
            notices: Notices {
                restore,
                ..Notices::default()
            },
            menu: None,
            menu_at: None,
            dismissed: Dismissals::default(),
            chrome: Chrome::default(),
            menu_selector: format!("watchlist-menu-button-{}", id.0).into(),
            menu_tip: format!("tip-watchlist-menu-{}", id.0).into(),
            switch_tip: format!("tip-watchlist-switch-{}", id.0).into(),
            settled: false,
            was_shown: false,
            shared,
            history: History::default(),
            prompt: None,
            rules: None,
            field_window: None,
            confirm: None,
            awaiting: None,
            reverting: None,
        };
        tile.settle(cx);
        tile
    }

    /// The factory stored a new configuration snapshot: what a rule is
    /// validated against. The lists themselves come with the global, so
    /// nothing shown changes.
    pub fn config_changed(&mut self, cx: &mut Context<Self>) {
        self.settle(cx);
        cx.notify();
    }

    /// The bridge published a new snapshot: a list's definition, members,
    /// resolution state or provenance changed, or a list came or went. The
    /// shown list's definition is the reload the history waits for: its
    /// own write landing drops the pending copy, another surface's write
    /// drops it too, and a members answer under the same definition keeps
    /// it (`History::reloaded`).
    fn snapshot_changed(&mut self, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        if let Some((_, state)) = self.shown(&snapshot) {
            self.history.reloaded(&state.definition);
        }
        self.settle(cx);
        cx.notify();
    }

    /// The reference tables changed: the grid's reference names come from
    /// them.
    fn reference_changed(&mut self, cx: &mut Context<Self>) {
        self.rebuild_rows(false, cx);
        cx.notify();
    }

    fn clock_changed(&mut self, cx: &mut Context<Self>) {
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// Bring the tile in line with the snapshot: the switcher opens on the
    /// first snapshot with lists when nothing is shown, and again when the
    /// shown list goes away, never on a change that leaves a nothing-shown
    /// tile as it was (the trader closed the switcher). An open switcher
    /// takes the new names; the grid takes the new members. A restored
    /// cursor still waiting once the shown list's resolution is current
    /// names a member the list no longer holds: the rows that could hold
    /// it have answered, so it stops waiting (a later re-add must not snap
    /// the cursor under a trader who has not touched the tile). A
    /// resolving or failed status is not that answer.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let shown = self.shown_in(&snapshot);
        if !shown {
            // Its history means nothing now, and its field would write
            // nowhere: the field goes, its text with it, nothing written
            // (a new list's name asks about no list, and stays); the rules
            // popup listed its rules; a question about it asks about
            // nothing.
            self.history.forget();
            if !self
                .prompt
                .as_ref()
                .is_some_and(|p| p.prompt == Prompt::NewName)
            {
                self.release_prompt(cx);
            }
            self.rules = None;
            if confirm::withdraw(self, cx).is_some() {
                self.notices
                    .outcome(Notice::status("the watchlist asked about is gone"));
            }
        }
        if self
            .awaiting
            .as_ref()
            .is_some_and(|a| Self::landed(a, &snapshot))
        {
            self.awaiting = None;
        }
        if self
            .reverting
            .as_ref()
            .is_some_and(|n| Self::revert_landed(n, &snapshot))
        {
            self.reverting = None;
        }
        self.settle_menu(shown, &snapshot);
        if self.notices.nothing_to_switch && !snapshot.lists.is_empty() {
            self.notices.nothing_to_switch = false;
        }
        self.rebuild_rows(false, cx);
        let current = self
            .state
            .name
            .as_deref()
            .and_then(|n| snapshot.lists.get(n))
            .is_some_and(|s| s.status == Status::Current);
        if current {
            self.grid.forget_seed();
        }
    }

    fn settle_menu(&mut self, shown: bool, snapshot: &WatchlistSnapshot) {
        let listed = !snapshot.lists.is_empty();
        let first = listed && !self.settled;
        let gone = self.was_shown && !shown;
        self.settled |= listed;
        self.was_shown = shown;
        match self.menu.as_ref().map(|(k, _)| *k) {
            Some(MenuKind::Switch) => {
                let rows = self.switch_rows(snapshot);
                if rows.is_empty() {
                    self.menu = None;
                } else if let Some((_, m)) = self.menu.as_mut() {
                    m.replace_rows(rows, &self.chords);
                }
            }
            _ if !shown && (first || gone) => {
                let rows = self.switch_rows(snapshot);
                if !rows.is_empty() {
                    self.menu = Some((MenuKind::Switch, Menu::new(rows, &self.chords)));
                }
            }
            _ => {}
        }
    }

    /// The frame notified: take what the shell said about this tile's
    /// config writes. A fork is news; a refusal means the optimistic edit
    /// never landed, so the rows go back to the snapshot's object.
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
        // The verb's word (`removed NDX`) is no longer true: the refusal
        // replaces it rather than standing beside it.
        if refused {
            self.notices.outcome.clear();
        }
        // A create, clone, rename or delete that never landed: back to
        // what was shown before it, ahead of the notices, which showing
        // another list would clear.
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
            // A refused revert is over: the user copy stands.
            self.reverting = None;
            self.rebuild_rows(false, cx);
        } else {
            self.rebuild_chrome(cx);
        }
        cx.notify();
    }

    pub(crate) fn dismiss_notice(&mut self, n: &Notice, cx: &App) -> bool {
        if !n.dismissable() {
            return false;
        }
        if let Some(i) = self.notices.outcome.iter().position(|o| o == n) {
            self.notices.outcome.remove(i);
            self.rebuild_chrome(cx);
            return true;
        }
        // `g w`'s refusal: a key set it, so it is transient too.
        if self.notices.nothing_to_switch && *n == Notice::danger(NOTHING_TO_SWITCH) {
            self.notices.nothing_to_switch = false;
            self.rebuild_chrome(cx);
            return true;
        }
        if n.tone() == notice::Tone::Danger
            && self.notices.restore.iter().any(|r| r == n.text().as_ref())
        {
            self.notices.restore.retain(|r| r != n.text().as_ref());
            self.rebuild_chrome(cx);
            return true;
        }
        self.dismissed.dismiss(n)
    }

    /// Rebuild the grid's rows from the shown list's snapshot entry, the
    /// history's pending definition (an edit awaiting its reload) and the
    /// reference tables, then everything painted from them. `after_verb`
    /// after a member verb: the cursor keeps its shown index.
    fn rebuild_rows(&mut self, after_verb: bool, cx: &mut Context<Self>) {
        let rows = {
            let snapshot = snapshot(cx);
            let reference = reference(cx);
            self.state
                .name
                .as_deref()
                .and_then(|n| snapshot.lists.get(n))
                .map(|s| rows::rows(s, self.history.pending(), &reference))
                .unwrap_or_default()
        };
        if after_verb {
            self.grid.after_verb(rows);
        } else {
            self.grid.set_rows(rows);
        }
        self.rebuild_chrome(cx);
        self.sync_table(true, cx);
    }

    /// Hand the table what it paints. `rows` re-prepares the shown rows
    /// (and the columns' sort marks); a cursor or selection change alone
    /// moves only the painted cursor and tint.
    fn sync_table(&mut self, rows: bool, cx: &mut Context<Self>) {
        let prepared = rows.then(|| Rc::new(Prepared::build(&self.grid)));
        let (title, help) = if self.grid.query().is_empty() {
            (
                "No names",
                "The list has no members: add a name or a rule from the \u{22ef} menu.",
            )
        } else {
            (
                "No matching names",
                "Escape restores the filter in force before the search.",
            )
        };
        let selected = self.grid.selected();
        let sort = self.grid.sort();
        let cursor = self.grid.cursor();
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.set_selected(selected);
            // The table reads sort marks only on a refresh, which also
            // re-lays every column: refresh only when the sort changed,
            // never for a filter keystroke or a new snapshot.
            let refresh = d.set_sort(sort);
            if let Some(prepared) = prepared {
                d.set(prepared);
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
        });
    }

    fn rebuild_chrome(&mut self, cx: &App) {
        let snapshot = snapshot(cx);
        let clock = clock(cx);
        let name = self.state.name.as_deref();
        let state = name.and_then(|n| snapshot.lists.get(n));
        // The rules as they are now; the snapshot's errors apply to the
        // snapshot's rules, not to a pending edit awaiting its fold.
        self.chrome.rules = match state {
            Some(s) => {
                let current = self.history.current(&s.definition);
                let errors: &[_] = if current.rules == s.definition.rules {
                    &s.rule_errors
                } else {
                    &[]
                };
                rule_rows(current, errors)
            }
            None => Vec::new(),
        };
        if let Some(p) = self.rules.as_mut() {
            p.clamp(self.chrome.rules.len());
        }
        // The counts are what the rows and the popup show: with a pending
        // edit, not what the snapshot last resolved.
        let (live, _) = self.grid.counts();
        let rules = (
            self.chrome.rules.len(),
            self.chrome.rules.iter().any(|r| r.error.is_some()),
        );
        self.chrome.header = HeaderModel::prepare(name, state, live, rules, &clock);
        let none_defined = snapshot.lists.is_empty();
        let new = action_title(NEW_ACTION);
        // The switch chord as the keymap binds it now; the palette's title
        // names the route when it binds none.
        let switch = tips::chord_for(&self.chords, SWITCH_ACTION)
            .map(|ks| spell(&ks))
            .unwrap_or_else(|| action_title(SWITCH_ACTION).to_string());
        let saving = self.awaiting.as_ref().and_then(|a| a.shows.as_deref());
        self.chrome.empty = match (name, state.is_some()) {
            (_, true) => None,
            // Written, not yet reloaded: the name is on its way.
            (Some(name), false) if saving == Some(name) => {
                Some(format!("saving {name}\u{2026}").into())
            }
            (Some(gone), false) if none_defined => {
                Some(format!("{gone} no longer exists \u{2014} {new} creates one").into())
            }
            (Some(gone), false) => {
                Some(format!("{gone} no longer exists \u{2014} {switch} switches").into())
            }
            (None, false) if none_defined => {
                Some(format!("no watchlists defined \u{2014} {new} creates one").into())
            }
            (None, false) => Some(format!("no watchlist shown \u{2014} {switch} switches").into()),
        };
        // The standing resolution notices, from the snapshot: a failed
        // resolution whole, then each bad rule. They stand until the next
        // resolution changes them; dismissed, they hide until they do.
        let standing = state.into_iter().flat_map(|s| {
            let failed = match &s.status {
                Status::Failed(err) => {
                    Some(Notice::danger(format!("not resolved: {err} {RETRIES}")))
                }
                Status::Resolving | Status::Current => None,
            };
            failed.into_iter().chain(s.rule_errors.iter().map(|e| {
                Notice::warning(format!(
                    "rule {} failed: {} {RETRIES}",
                    e.index + 1,
                    e.reason
                ))
            }))
        });
        // The verb's own word first, then what stands, then the restore's.
        let n = &self.notices;
        self.chrome.notices = n
            .outcome
            .iter()
            .cloned()
            .chain(
                n.nothing_to_switch
                    .then(|| Notice::danger(NOTHING_TO_SWITCH)),
            )
            .chain(standing)
            .chain(n.restore.iter().cloned().map(Notice::danger))
            .collect();
        // A dismissed notice no longer reported shows again when it returns.
        self.dismissed.prune(&self.chrome.notices);
    }

    /// Whether the tile names a list the snapshot still holds.
    fn shown_in(&self, snapshot: &WatchlistSnapshot) -> bool {
        self.state
            .name
            .as_deref()
            .is_some_and(|n| snapshot.lists.contains_key(n))
    }

    /// One row per watchlist, alphabetical, the shown one ticked. A list
    /// whose removal is on its way (renamed or deleted) is left out.
    fn switch_rows(&self, snapshot: &WatchlistSnapshot) -> Vec<Row<Pick>> {
        let removed = self.awaiting.as_ref().and_then(|a| a.removed.as_deref());
        snapshot
            .lists
            .keys()
            .filter(|name| Some(name.as_str()) != removed)
            .map(|name| {
                let checked = self.state.name.as_deref() == Some(name.as_str());
                let label = SharedString::from(name.clone());
                Row::Action(ActionRow::new(Pick::Show(label.clone()), label).checked(checked))
            })
            .collect()
    }

    /// The `⋯` menu: the member verbs, the switcher, then the watchlist's
    /// own verbs, each running the palette's action, with its live chord.
    /// A row that cannot act says why in its lane, and in full when
    /// picked; Revert… is listed only over a user copy with a lower copy
    /// beneath it. The switcher's row reads `Switch…`: like `New…`, it
    /// opens a further pick.
    fn action_rows(&self, cx: &App) -> Vec<Row<Pick>> {
        let row = |id: &'static str, title: &str, enabled: Result<(), Blocked>| {
            let title = title.strip_prefix("Watchlist: ").unwrap_or(title);
            let row = ActionRow::new(Pick::Action(id), title.to_string()).hint(Hint::chord(id));
            Row::Action(match enabled {
                Ok(()) => row,
                Err(b) => row.enabled(Err(b.long.into())).short_reason(b.short),
            })
        };
        let verb =
            |id: &'static str, enabled: Result<(), Blocked>| row(id, action_title(id), enabled);
        let snapshot = snapshot(cx);
        let shown = self.chrome.empty.is_none();
        // What every member verb needs: a list shown, and no revert of it
        // on its way.
        let listed = || match (shown, self.verbs_allowed()) {
            (false, _) => Err(Blocked::same(NOTHING_SHOWN)),
            (true, Err(why)) => Err(Blocked {
                short: "reverting",
                long: why,
            }),
            (true, Ok(())) => Ok(()),
        };
        let targeted = match (listed(), self.grid.cursor().is_none()) {
            (Err(b), _) => Err(b),
            (Ok(()), true) => Err(Blocked::same(NO_ROW)),
            (Ok(()), false) => Ok(()),
        };
        let mut rows = vec![
            verb(ADD_ACTION, listed()),
            verb(REMOVE_ACTION, targeted),
            verb(RULES_ACTION, listed()),
            verb(
                REFRESH_ACTION,
                if shown {
                    Ok(())
                } else {
                    Err(Blocked::same(NOTHING_SHOWN))
                },
            ),
            verb(
                UNDO_ACTION,
                match (listed(), self.history.can_undo()) {
                    (Err(b), _) => Err(b),
                    (Ok(()), false) => Err(Blocked::same(NOTHING_TO_UNDO)),
                    (Ok(()), true) => Ok(()),
                },
            ),
            verb(
                REDO_ACTION,
                match (listed(), self.history.can_redo()) {
                    (Err(b), _) => Err(b),
                    (Ok(()), false) => Err(Blocked::same(NOTHING_TO_REDO)),
                    (Ok(()), true) => Ok(()),
                },
            ),
            Row::Separator,
            row(
                SWITCH_ACTION,
                &format!("{}\u{2026}", action_title(SWITCH_ACTION)),
                Ok(()),
            ),
            Row::Separator,
            verb(NEW_ACTION, Ok(())),
            verb(
                CLONE_ACTION,
                if shown {
                    Ok(())
                } else {
                    Err(Blocked::same(NOTHING_SHOWN))
                },
            ),
            verb(RENAME_ACTION, self.own(&snapshot, Verb::Rename).map(|_| ())),
            verb(DELETE_ACTION, self.own(&snapshot, Verb::Delete).map(|_| ())),
        ];
        if self.revertible(&snapshot).is_ok() {
            rows.push(verb(REVERT_ACTION, Ok(())));
        }
        rows
    }

    fn toggle_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        self.menu_at = None;
        if self.menu.as_ref().is_some_and(|(k, _)| *k == kind) {
            self.menu = None;
        } else {
            let rows = match kind {
                MenuKind::Switch => self.switch_rows(&snapshot(cx)),
                MenuKind::Actions => self.action_rows(cx),
            };
            if rows.is_empty() {
                self.menu = None;
                self.notices.nothing_to_switch = true;
                self.rebuild_chrome(cx);
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

    /// Show `name`. The cursor, the filter, a selection and the last
    /// verb's notices belonged to the previous list, so they are dropped;
    /// the sort is the tile's and stays.
    fn show(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.state.name.as_deref() != Some(name) {
            self.state.name = Some(name.to_string());
            self.state.cursor = None;
            self.grid = GridModel::new();
            self.grid.set_sort(self.state.sort);
            self.find_entry = None;
            self.notices.outcome.clear();
            // The history, an open field and the rules popup were the
            // previous list's.
            self.history.forget();
            self.release_prompt(cx);
            self.rules = None;
        }
        self.was_shown = self.shown_in(&snapshot(cx));
        self.rebuild_rows(false, cx);
        cx.notify();
    }

    /// A press on shown row `row`: shift extends a row selection to it, a
    /// plain press moves the cursor there and ends a selection. A
    /// double-click is two presses and nothing more: a members row has no
    /// field to open. A press on the grid leaves an open field unwritten,
    /// as escape would, and closes the rules popup (it is outside it).
    fn row_pressed(&mut self, e: RowPressed, window: &mut Window, cx: &mut Context<Self>) {
        self.user_acted(cx);
        self.close_prompt(window, cx);
        self.close_rules(cx);
        self.grid.click(e.row, e.shift);
        self.sync_table(false, cx);
        cx.notify();
    }

    /// A right press on shown row `row`: the cursor moves there (a row of a
    /// live selection keeps the selection, which the menu then acts on),
    /// and the `⋯` menu opens hung from the pointer, as `.` opens it from
    /// the header.
    fn row_context(&mut self, e: RowContext, window: &mut Window, cx: &mut Context<Self>) {
        self.user_acted(cx);
        self.close_prompt(window, cx);
        self.close_rules(cx);
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
            Menu::new(self.action_rows(cx), &self.chords).open_at(None),
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

    /// `:sort <name|origin|reference> [asc|desc]`, tile-local; a bare
    /// `:sort` restores the default order (by name, excluded rows last).
    pub fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("sort") => {}
            Some(verb) => return Err(format!("not a watchlist command: {verb}")),
            None => return Err("not a watchlist command".into()),
        }
        const USAGE: &str = "sort takes name, origin or reference, then optionally asc or desc";
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
            ["sort"] => strs(&["name", "origin", "reference"]),
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

    /// `insert` while a field or the y/n question holds the keys, `menu`
    /// while a menu is up, `rules` while the rules popup is (a rule prompt
    /// over it is a field: insert wins), `visual` while a row selection is
    /// live, `normal` otherwise.
    fn mode(&self) -> &'static str {
        if self.prompt.is_some() || self.confirm.is_some() {
            "insert"
        } else if self.menu.is_some() {
            "menu"
        } else if self.rules.is_some() {
            "rules"
        } else if self.grid.selecting() {
            "visual"
        } else {
            "normal"
        }
    }

    /// `grid` in every mode (the shell's motions bind only in normal and
    /// visual), with counts; `tilelist` only while a menu or the rules
    /// popup is up and no field is (a field's bare keys are its text), so
    /// the shell's shared `j`/`k` and arrows step the rows instead.
    pub fn key_context(&self) -> KeyContext {
        let ctx = KeyContext::new(crate::KIND)
            .grid()
            .pair("mode", self.mode())
            .counts();
        if self.prompt.is_none() && (self.menu.is_some() || self.rules.is_some()) {
            ctx.tilelist()
        } else {
            ctx
        }
    }

    /// Whether `action` is the rules popup's own, so the popup stays open
    /// for it; any other verb closes the popup first.
    fn keeps_rules(action: &str) -> bool {
        matches!(
            action,
            geode_tile::motion::MENU_DOWN
                | geode_tile::motion::MENU_UP
                | RULES_ACTION
                | RULE_ADD_ACTION
                | RULE_REMOVE_ACTION
                | RULE_EDIT_ACTION
                | COMMIT_ACTION
                | CANCEL_ACTION
        )
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
        // `cancel` that answer is the whole of it: the layers beneath wait.
        let answered = confirm::cancel(self, window, cx);
        if answered && action.0 == CANCEL_ACTION {
            return true;
        }
        // The field's own keys; any other verb closes it unwritten first.
        if self.prompt.is_some() {
            match action.0.as_str() {
                COMMIT_ACTION => {
                    self.commit_prompt(window, cx);
                    return true;
                }
                CANCEL_ACTION => {
                    self.close_prompt(window, cx);
                    return true;
                }
                CHOICE_UP_ACTION => {
                    self.choice_step(-1, cx);
                    return true;
                }
                CHOICE_DOWN_ACTION => {
                    self.choice_step(1, cx);
                    return true;
                }
                _ => self.close_prompt(window, cx),
            }
        }
        // The popup's own keys keep it; a verb that leaves it closes it.
        if self.rules.is_some() && !Self::keeps_rules(&action.0) {
            self.close_rules(cx);
        }
        // The grid's motions, while no menu or popup holds the keys. A
        // column motion has no column to move in this rows-only grid.
        if self.menu.is_none()
            && self.rules.is_none()
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
            // The shared list steps: the menu's rows while one is up (it
            // is on top), else the rules popup's.
            geode_tile::motion::MENU_DOWN | geode_tile::motion::MENU_UP
                if self.menu.is_some() || self.rules.is_some() =>
            {
                let down = action.0 == geode_tile::motion::MENU_DOWN;
                if let Some((_, m)) = self.menu.as_mut() {
                    m.step(if down { 1 } else { -1 });
                    cx.notify();
                } else {
                    self.rules_step(if down { 1 } else { -1 }, cx);
                }
            }
            SWITCH_ACTION => self.toggle_menu(MenuKind::Switch, cx),
            MENU_ACTION => self.toggle_menu(MenuKind::Actions, cx),
            COMMIT_ACTION if self.menu.is_some() => {
                let Some(at) = self.menu.as_ref().and_then(|(_, m)| m.highlighted()) else {
                    return false;
                };
                self.menu_pick(at, window, cx);
            }
            COMMIT_ACTION if self.rules.is_some() => self.rule_edit(window, cx),
            COMMIT_ACTION => return false,
            // `escape` peels one layer at a time: the menu (the surface on
            // top, which may be acting on the selection), then the rules
            // popup, then a live selection, then the notices.
            CANCEL_ACTION if self.menu.is_some() => self.close_menu(cx),
            CANCEL_ACTION if self.rules.is_some() => {
                self.close_rules(cx);
            }
            CANCEL_ACTION if self.grid.selecting() => {
                self.grid.clear_selection();
                self.sync_table(false, cx);
                cx.notify();
            }
            // Last in line: nothing above had anything to cancel, so
            // `escape` dismisses every warning and danger notice showing,
            // each as a click on it would. Nothing to dismiss: unhandled.
            CANCEL_ACTION => {
                let shown = self.dismissed.visible(self.chrome.notices.iter().cloned());
                let mut any = false;
                for n in &shown {
                    any |= self.dismiss_notice(n, cx);
                }
                if !any {
                    return false;
                }
                cx.notify();
            }
            VISUAL_ACTION if self.menu.is_none() => {
                if self.grid.selecting() {
                    self.grid.clear_selection();
                } else {
                    self.grid.start_selection();
                }
                self.sync_table(false, cx);
                cx.notify();
            }
            // The member verbs, from a key, the palette or the menu.
            ADD_ACTION => self.open_add(window, cx),
            REMOVE_ACTION => self.remove(cx),
            UNDO_ACTION => self.replay(Way::Undo, cx),
            REDO_ACTION => self.replay(Way::Redo, cx),
            REFRESH_ACTION => self.refresh(cx),
            // The rules popup and its verbs.
            RULES_ACTION => self.toggle_rules(cx),
            RULE_ADD_ACTION => self.rule_add(window, cx),
            RULE_REMOVE_ACTION => self.rule_remove(cx),
            RULE_EDIT_ACTION => self.rule_edit(window, cx),
            // The watchlist's own verbs, from the palette or the menu.
            NEW_ACTION => self.open_new(window, cx),
            CLONE_ACTION => self.open_clone(window, cx),
            RENAME_ACTION => self.open_rename(window, cx),
            DELETE_ACTION => self.ask_delete(window, cx),
            REVERT_ACTION => self.ask_revert(window, cx),
            _ => return false,
        }
        true
    }

    /// The trader acted in the tile (a key or a press): the session
    /// restore's notices have been seen, and go.
    fn user_acted(&mut self, cx: &mut Context<Self>) {
        if !self.notices.restore.is_empty() {
            self.notices.restore.clear();
            self.rebuild_chrome(cx);
            cx.notify();
        }
    }

    /// Whether the open field or the armed question owns window focus.
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.prompt
            .as_ref()
            .is_some_and(|p| p.input.read(cx).focus_handle(cx).is_focused(window))
            || self.confirm.as_ref().is_some_and(|c| c.holds_focus(window))
    }

    /// The snapshot arrives whether the tile is shown or not, so being
    /// shown or hidden changes nothing.
    pub fn set_visible(&mut self, _visible: bool) {}

    /// The shell's word on whether this is the focused tile, given from its
    /// render. Nothing here needs it yet; no notify, one sent during the
    /// draw is dropped.
    pub fn set_focused(&mut self, _focused: bool, _cx: &mut Context<Self>) {}

    pub fn closed(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        self.rules = None;
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

    /// The header's name, or the one written and on its way.
    pub fn title(&self) -> SharedString {
        let saving = self.awaiting.as_ref().and_then(|a| a.shows.as_deref());
        match (&self.chrome.header.name, saving) {
            (Some(name), _) => format!("Watchlist: {name}").into(),
            (None, Some(name)) if self.state.name.as_deref() == Some(name) => {
                format!("Watchlist: {name}").into()
            }
            (None, _) => SharedString::new_static(header::NONE_SHOWN),
        }
    }

    /// The cursor is saved by name: a restored one still waiting for its
    /// row, else the grid's.
    pub fn serialize(&self) -> toml::Table {
        session::to_table(&State {
            cursor: self.grid.saved_cursor().map(str::to_string),
            ..self.state.clone()
        })
    }

    /// The names a member verb acts on.
    #[cfg(test)]
    fn targets(&self) -> Vec<String> {
        self.grid.targets()
    }

    /// The shown rows' names, in painted order.
    #[cfg(test)]
    fn shown_names(&self) -> Vec<String> {
        self.grid
            .visible()
            .iter()
            .map(|&i| self.grid.row(i).name.clone())
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

    /// The origin column of the shown row `name`, as painted.
    #[cfg(test)]
    fn origin_of(&self, name: &str) -> Option<String> {
        self.grid
            .visible()
            .iter()
            .map(|&i| self.grid.row(i))
            .find(|r| r.name == name)
            .map(|r| rows::origin_text(&r.origin, r.pending))
    }

    /// The notices showing: reported and not dismissed.
    #[cfg(test)]
    fn visible_notice_texts(&self) -> Vec<String> {
        self.dismissed
            .visible(self.chrome.notices.iter().cloned())
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

    /// The open `⋯` menu's row titles; `None` while it is closed.
    #[cfg(test)]
    fn action_titles(&self) -> Option<Vec<String>> {
        let (MenuKind::Actions, m) = self.menu.as_ref()? else {
            return None;
        };
        Some(
            m.rows()
                .iter()
                .filter_map(Row::action)
                .map(|a| a.title().to_string())
                .collect(),
        )
    }

    /// The open `⋯` menu's rows with each disabled row's reason.
    #[cfg(test)]
    fn action_reasons(&self) -> Option<Vec<(String, Option<String>)>> {
        let (MenuKind::Actions, m) = self.menu.as_ref()? else {
            return None;
        };
        Some(
            m.rows()
                .iter()
                .filter_map(Row::action)
                .map(|a| (a.title().to_string(), a.reason().map(|r| r.to_string())))
                .collect(),
        )
    }

    /// The highlighted row of whichever menu is open.
    #[cfg(test)]
    fn highlighted(&self) -> Option<usize> {
        self.menu.as_ref().and_then(|(_, m)| m.highlighted())
    }

    #[cfg(test)]
    fn empty_text(&self) -> Option<String> {
        self.chrome.empty.as_ref().map(|s| s.to_string())
    }
}

impl MenuHost for WatchlistTile {
    /// A disabled row says why and keeps the menu open; an enabled one
    /// closes it and acts.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picked) = self.menu.as_ref().and_then(|(_, m)| m.pick(index)) else {
            return;
        };
        match picked {
            Err(why) => {
                self.notices.outcome(Notice::danger(why.to_string()));
                self.rebuild_chrome(cx);
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

impl Render for WatchlistTile {
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
        let ids = MenuIds::new("watchlist-menu", "watchlist-menu-row");
        // Each menu's outside press closes only that menu: a press on the
        // other menu's trigger has already swapped it in (the triggers act
        // in the capture phase), and must not see it closed again.
        let close = |kind: MenuKind| {
            move |t: &mut WatchlistTile, _: &mut Window, cx: &mut Context<Self>| {
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
                    .debug_selector(|| "watchlist-switcher".into())
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
        // The rules popup hangs from the header's rules item; while a rule
        // prompt is open it sits inline under the prompt bar instead, so
        // the field is not covered.
        let (rules_hung, rules_inline) = match (self.rules, self.prompt.is_some()) {
            (None, _) => (None, None),
            (Some(p), false) => (
                Some(rules::render_hung(
                    &self.chrome.rules,
                    p.cursor,
                    &tile,
                    id,
                    cx,
                )),
                None,
            ),
            (Some(p), true) => (
                None,
                Some(rules::render_inline(
                    &self.chrome.rules,
                    p.cursor,
                    &tile,
                    id,
                    cx,
                )),
            ),
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
                on_dismiss: notice::on_dismiss_with(&tile, |t: &mut WatchlistTile, n, cx| {
                    t.dismiss_notice(n, cx)
                }),
                actions_open: actions.is_some() || row_actions.is_some(),
                switcher,
                rules_popup: rules_hung,
                menu_selector: self.menu_selector.clone(),
                menu_tip: self.menu_tip.clone(),
                switch_tip: self.switch_tip.clone(),
            },
            theme,
        );
        // The y/n question or the prompt, on its own bar under the header,
        // whole at any tile width.
        let question = self.confirm.as_ref().map(|pending| {
            confirm::bar(
                pending,
                &tile,
                move || format!("watchlist-confirm-{id}"),
                theme,
            )
        });
        let prompt = self
            .prompt
            .as_ref()
            .map(|p| field::render_prompt(p, &tile, id, cx));
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
                        .debug_selector(move || format!("watchlist-empty-{id}"))
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
            .children(rules_inline)
            .child(body)
            .children(row_actions);
        // A pointer press anywhere on the tile answers an armed question no.
        confirm::cancel_on_press(root, self.confirm.is_some(), &tile)
    }
}
