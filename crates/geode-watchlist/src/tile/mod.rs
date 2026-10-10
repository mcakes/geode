//! The shell-hosted watchlist entity. It shows one list from the resolved
//! snapshot in `WatchlistGlobal`, answers the shell's door
//! (`crate::content::WatchlistContent`) and paints the tile: the header
//! (`header`), the switcher hung beneath its name, the `⋯` menu, and an
//! empty state while there is nothing to show.
//!
//! The tile issues no data request: the bridge's cache resolves every list
//! and replaces the global, which wakes the tile. The factory's
//! configuration snapshot (schema, dimensions, saved scopes, named
//! expressions) is what a rule is validated against.
//!
//! What paint reads is prepared in `Chrome` whenever the snapshot, the
//! clock or the shown list changes, never in render.

mod header;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use geode_core::clock::Clock;
use geode_core::watchlist::state::WatchlistSnapshot;
use geode_shell::actions::ActionId;
use geode_shell::clock::AppClock;
use geode_shell::frame::{FrameRef, TileNotice};
use geode_shell::keymap::{Binding, KeyContext};
use geode_shell::module::{CloseHandle, FindEvent, StackHandle};
use geode_shell::reference::ReferenceGlobal;
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::watchlist::WatchlistGlobal;
use geode_tile::header::{HEADER_HEIGHT, Mode, link_chips};
use geode_tile::menu::{self, ActionRow, Hint, Menu, MenuHost, MenuIds, MenuPick, Row};
use geode_tile::notice::{self, Dismissals, Notice};
use gpui::prelude::*;
use gpui::{
    AnchoredPositionMode, App, Context, Pixels, Point, SharedString, Window, anchored, div,
};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::content::action_title;
use crate::core::session::{self, State};
use header::HeaderModel;

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

const SWITCH_ACTION: &str = "watchlist::switch";
const MENU_ACTION: &str = "watchlist::menu";
const COMMIT_ACTION: &str = "watchlist::commit";
const CANCEL_ACTION: &str = "watchlist::cancel";
const NEW_ACTION: &str = "watchlist::new";
const CLONE_ACTION: &str = "watchlist::clone";
const RENAME_ACTION: &str = "watchlist::rename";
const DELETE_ACTION: &str = "watchlist::delete";
const REVERT_ACTION: &str = "watchlist::revert";

/// The switcher's refusal while there is nothing to list.
const NOTHING_TO_SWITCH: &str = "no watchlists to switch to";

/// What a `⋯` row says while its verb is not built yet.
pub(crate) const NOT_YET: &str = "not yet available";

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
}

/// The resolved lists, as the bridge last published them; empty for a
/// tile hosted without the global.
fn snapshot(cx: &App) -> Arc<WatchlistSnapshot> {
    cx.try_global::<WatchlistGlobal>()
        .map(|g| g.0.clone())
        .unwrap_or_default()
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
}

impl WatchlistTile {
    pub(crate) fn new(
        id: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WatchlistTile {
        let (state, restore) = restored.map(session::from_table).unwrap_or_default();
        // The bridge replaces the global on every change to a list's
        // definition, members or state.
        cx.observe_global::<WatchlistGlobal>(|this, cx| this.snapshot_changed(cx))
            .detach();
        cx.observe_global::<ReferenceGlobal>(|this, cx| this.reference_changed(cx))
            .detach();
        // `as of` is on the display clock: a zone change re-prepares it.
        cx.observe_global::<AppClock>(|this, cx| this.clock_changed(cx))
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
        let mut tile = WatchlistTile {
            id,
            frame,
            state,
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
    /// resolution state or provenance changed, or a list came or went.
    fn snapshot_changed(&mut self, cx: &mut Context<Self>) {
        self.settle(cx);
        cx.notify();
    }

    /// The reference table changed. The grid's reference names come from
    /// it; nothing here reads it yet, so a change only repaints.
    fn reference_changed(&mut self, cx: &mut Context<Self>) {
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
    /// takes the new names.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let shown = self.shown_in(&snapshot);
        self.settle_menu(shown, &snapshot);
        if self.notices.nothing_to_switch && !snapshot.lists.is_empty() {
            self.notices.nothing_to_switch = false;
        }
        self.rebuild_chrome(cx);
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
    /// config writes. A fork is news; a refusal is a fault.
    fn frame_changed(&mut self, cx: &mut Context<Self>) {
        let id = self.id;
        let told = self
            .frame
            .entity()
            .update(cx, |f, _| f.take_tile_notices(id));
        if told.is_empty() {
            return;
        }
        for notice in told {
            match notice {
                TileNotice::Forked(text) => self.notices.outcome(Notice::status(text)),
                TileNotice::Refused(text) => self.notices.outcome(Notice::danger(text)),
            }
        }
        self.rebuild_chrome(cx);
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

    fn rebuild_chrome(&mut self, cx: &App) {
        let snapshot = snapshot(cx);
        let clock = clock(cx);
        let name = self.state.name.as_deref();
        let state = name.and_then(|n| snapshot.lists.get(n));
        self.chrome.header = HeaderModel::prepare(name, state, &clock);
        let none_defined = snapshot.lists.is_empty();
        let new = action_title(NEW_ACTION);
        self.chrome.empty = match (name, state.is_some()) {
            (_, true) => None,
            (Some(gone), false) if none_defined => {
                Some(format!("{gone} no longer exists \u{2014} {new} creates one").into())
            }
            (Some(gone), false) => {
                Some(format!("{gone} no longer exists \u{2014} g w switches").into())
            }
            (None, false) if none_defined => {
                Some(format!("no watchlists defined \u{2014} {new} creates one").into())
            }
            (None, false) => Some(SharedString::new_static(
                "no watchlist shown \u{2014} g w switches",
            )),
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

    /// One row per watchlist, alphabetical, the shown one ticked.
    fn switch_rows(&self, snapshot: &WatchlistSnapshot) -> Vec<Row<Pick>> {
        snapshot
            .lists
            .keys()
            .map(|name| {
                let checked = self.state.name.as_deref() == Some(name.as_str());
                let label = SharedString::from(name.clone());
                Row::Action(ActionRow::new(Pick::Show(label.clone()), label).checked(checked))
            })
            .collect()
    }

    /// The `⋯` menu: the switcher, then the watchlist's own verbs, each
    /// running the palette's action, with its live chord.
    fn action_rows(&self) -> Vec<Row<Pick>> {
        let row = |id: &'static str| {
            let title = action_title(id);
            let title = title.strip_prefix("Watchlist: ").unwrap_or(title);
            Row::Action(ActionRow::new(Pick::Action(id), title).hint(Hint::chord(id)))
        };
        vec![
            row(SWITCH_ACTION),
            Row::Separator,
            row(NEW_ACTION),
            row(CLONE_ACTION),
            row(RENAME_ACTION),
            row(DELETE_ACTION),
            row(REVERT_ACTION),
        ]
    }

    fn toggle_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        self.menu_at = None;
        if self.menu.as_ref().is_some_and(|(k, _)| *k == kind) {
            self.menu = None;
        } else {
            let rows = match kind {
                MenuKind::Switch => self.switch_rows(&snapshot(cx)),
                MenuKind::Actions => self.action_rows(),
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

    /// Show `name`. The cursor and the last verb's notices belonged to the
    /// previous list, so they are dropped; the sort is the tile's and stays.
    fn show(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.state.name.as_deref() != Some(name) {
            self.state.name = Some(name.to_string());
            self.state.cursor = None;
            self.notices.outcome.clear();
        }
        self.was_shown = self.shown_in(&snapshot(cx));
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// A `⋯` row whose verb lands in a later task: say so, as a status.
    fn not_yet(&mut self, cx: &mut Context<Self>) {
        self.close_menu(cx);
        self.notices.outcome.clear();
        self.notices.outcome(Notice::status(NOT_YET));
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// No `:` command yet: the sort lands with the grid.
    pub fn command(&mut self, line: &str, _cx: &mut Context<Self>) -> Result<(), String> {
        match line.split_whitespace().next() {
            Some(verb) => Err(format!("not a watchlist command: {verb}")),
            None => Err("not a watchlist command".into()),
        }
    }

    pub fn completions(&self, _line: &str, _cursor: usize) -> Vec<String> {
        Vec::new()
    }

    /// `/` has no rows to narrow yet.
    pub fn find(&mut self, _event: FindEvent, _cx: &mut Context<Self>) {}

    fn mode(&self) -> &'static str {
        if self.menu.is_some() {
            "menu"
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
        _count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.user_acted(cx);
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
            SWITCH_ACTION => self.toggle_menu(MenuKind::Switch, cx),
            MENU_ACTION => self.toggle_menu(MenuKind::Actions, cx),
            COMMIT_ACTION => {
                let Some(at) = self.menu.as_ref().and_then(|(_, m)| m.highlighted()) else {
                    return false;
                };
                self.menu_pick(at, window, cx);
            }
            CANCEL_ACTION if self.menu.is_some() => self.close_menu(cx),
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
            // The watchlist's own verbs, from the palette or the menu: not
            // built yet.
            NEW_ACTION | CLONE_ACTION | RENAME_ACTION | DELETE_ACTION | REVERT_ACTION => {
                self.not_yet(cx)
            }
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

    /// No field holds window focus yet.
    pub fn holds_focus(&self, _window: &Window, _cx: &App) -> bool {
        false
    }

    /// The snapshot arrives whether the tile is shown or not, so being
    /// shown or hidden changes nothing.
    pub fn set_visible(&mut self, _visible: bool) {}

    /// The shell's word on whether this is the focused tile, given from its
    /// render. Nothing here needs it yet; no notify, one sent during the
    /// draw is dropped.
    pub fn set_focused(&mut self, _focused: bool, _cx: &mut Context<Self>) {}

    pub fn closed(&mut self, _cx: &mut Context<Self>) {
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
            Some(name) => format!("Watchlist: {name}").into(),
            None => SharedString::new_static(header::NONE_SHOWN),
        }
    }

    pub fn serialize(&self) -> toml::Table {
        session::to_table(&self.state)
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.0;
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
                menu_selector: self.menu_selector.clone(),
                menu_tip: self.menu_tip.clone(),
                switch_tip: self.switch_tip.clone(),
            },
            theme,
        );
        // The grid lands with the rows; until then a shown list paints an
        // empty body under its header.
        let body = v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .px_4()
            .text_color(theme.muted_foreground)
            .when_some(self.chrome.empty.clone(), |el, text| {
                el.child(
                    div()
                        .debug_selector(move || format!("watchlist-empty-{id}"))
                        .child(text),
                )
            });
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
            .children(row_actions)
    }
}
