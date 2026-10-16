//! The tile's chrome: the header model, the notices and their
//! dismissal, the empty state, and the watchlist switcher's menu.

use geode_core::watchlist::state::{Status, WatchlistSnapshot};
use geode_shell::keymap::Keystroke;
use geode_shell::tips;
use geode_tile::menu::{ActionRow, Menu, MenuHost, MenuPick, Row as MenuRow};
use geode_tile::notice::{Notice, Tone};
use gpui::{App, Context, SharedString, Window};

use super::{SWITCH_ACTION, VIEW_NEXT_ACTION, VolwatchTile, snapshot};
use crate::core::model::header;

/// Which menu is up. Menu mode, its keys and its pick door are shared by
/// every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Switch,
}

/// What a menu row does when picked.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pick {
    /// Show this watchlist.
    Show(SharedString),
}

impl MenuPick for Pick {
    fn element_name(&self) -> SharedString {
        match self {
            Pick::Show(name) => name.clone(),
        }
    }
}

pub(crate) struct MenuState {
    pub(crate) kind: MenuKind,
    pub(crate) menu: Menu<Pick>,
}

/// The header's notices, by how long each lives.
#[derive(Default)]
pub(crate) struct Notices {
    /// What the session restore dropped; cleared by the trader's first
    /// action in the tile.
    pub(crate) restore: Vec<String>,
    /// The switcher refused with nothing to list; cleared once a snapshot
    /// defines something to switch to.
    pub(crate) nothing_to_switch: bool,
    /// The last submission was refused; cleared by the next admitted one.
    pub(crate) refusal: Option<String>,
    /// The last board failed; stands until a board lands.
    pub(crate) failure: Option<String>,
    /// A key's refusal (a sort on an unsortable column); cleared by the
    /// trader's next action in the tile.
    pub(crate) refused: Option<String>,
}

/// A chord in the keymap's own spelling (`g w`, `shift+r`).
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

impl MenuHost for VolwatchTile {
    fn menu_pick(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(picked) = self.menu.as_ref().and_then(|m| m.menu.pick(index)) else {
            return;
        };
        match picked {
            Err(_) => {}
            Ok(Pick::Show(name)) => {
                self.menu = None;
                self.show(&name, cx);
            }
        }
    }

    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.menu.as_mut().is_some_and(|m| m.menu.highlight(index)) {
            cx.notify();
        }
    }
}

/// The switcher's refusal while there is nothing to list.
const NOTHING_TO_SWITCH: &str = "no watchlists to switch to";

impl VolwatchTile {
    pub(super) fn settle_menu(&mut self, shown: bool, snapshot: &WatchlistSnapshot) {
        let listed = !snapshot.lists.is_empty();
        let first = listed && !self.settled;
        let gone = self.was_shown && !shown;
        self.settled |= listed;
        self.was_shown = shown;
        match self.menu.as_ref().map(|m| m.kind) {
            Some(MenuKind::Switch) => {
                let rows = self.switch_rows(snapshot);
                if rows.is_empty() {
                    self.menu = None;
                } else if let Some(m) = self.menu.as_mut() {
                    m.menu.replace_rows(rows, &self.chords);
                }
            }
            None if !shown && (first || gone) => {
                let rows = self.switch_rows(snapshot);
                if !rows.is_empty() {
                    self.menu = Some(MenuState {
                        kind: MenuKind::Switch,
                        menu: Menu::new(rows, &self.chords),
                    });
                }
            }
            None => {}
        }
    }

    /// One row per watchlist, alphabetical, the shown one ticked.
    pub(super) fn switch_rows(&self, snapshot: &WatchlistSnapshot) -> Vec<MenuRow<Pick>> {
        snapshot
            .lists
            .keys()
            .map(|name| {
                let checked = self.state.list.as_deref() == Some(name.as_str());
                let label = SharedString::from(name.clone());
                MenuRow::Action(ActionRow::new(Pick::Show(label.clone()), label).checked(checked))
            })
            .collect()
    }

    pub(super) fn toggle_switcher(&mut self, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.menu = None;
        } else {
            let rows = self.switch_rows(&snapshot(cx));
            if rows.is_empty() {
                self.notices.nothing_to_switch = true;
                self.rebuild_chrome(cx);
            } else {
                let at = rows
                    .iter()
                    .position(|r| r.action().is_some_and(|a| a.tick() == Some(true)));
                self.menu = Some(MenuState {
                    kind: MenuKind::Switch,
                    menu: Menu::new(rows, &self.chords).open_at(at),
                });
            }
        }
        cx.notify();
    }

    /// The header's `⋯` press. The action menu is not built yet: the
    /// trigger is wired so its press already reaches this door, and asks
    /// nothing.
    pub(super) fn toggle_menu(&mut self, _cx: &mut Context<Self>) {}

    /// The header's title, count, lookback chips and board counts, from
    /// the shown list's name: rebuilt with the chrome, so a list switch or
    /// a restore whose snapshot has not arrived names its list at once.
    pub(crate) fn rebuild_header(&mut self) {
        let sort = self.sort_label();
        self.header = header(
            self.state.list.as_deref(),
            self.members.len(),
            self.state.lookback,
            sort.as_deref(),
            self.board.as_deref(),
        );
    }

    pub(crate) fn rebuild_chrome(&mut self, cx: &App) {
        self.rebuild_header();
        self.strip_keys = tips::chord_for(&self.chords, VIEW_NEXT_ACTION);
        let snapshot = snapshot(cx);
        let list = self.state.list.as_deref();
        let state = list.and_then(|n| snapshot.lists.get(n));
        let none_defined = snapshot.lists.is_empty();
        let switch = tips::chord_for(&self.chords, SWITCH_ACTION)
            .map(|ks| spell(&ks))
            .unwrap_or_else(|| "Vol: Switch list".to_string());
        self.empty = match (list, state.is_some()) {
            (_, true) => None,
            (_, false) if none_defined => Some("no watchlists defined".into()),
            (Some(gone), false) => {
                Some(format!("{gone} no longer exists \u{2014} {switch} switches").into())
            }
            (None, false) => Some(format!("no watchlist shown \u{2014} {switch} switches").into()),
        };
        let unresolved = state.and_then(|s| match &s.status {
            Status::Failed(err) => Some(Notice::warning(format!(
                "{} not resolved: {err}",
                list.unwrap_or_default()
            ))),
            Status::Resolving | Status::Current => None,
        });
        let n = &self.notices;
        self.reported = n
            .refusal
            .iter()
            .chain(n.failure.iter())
            .chain(n.refused.iter())
            .cloned()
            .map(Notice::warning)
            .chain(unresolved)
            .chain(
                n.nothing_to_switch
                    .then(|| Notice::danger(NOTHING_TO_SWITCH)),
            )
            .chain(n.restore.iter().cloned().map(Notice::danger))
            .collect();
        self.dismissed.prune(&self.reported);
    }

    /// A transient notice goes for good; a standing one hides until it
    /// changes.
    pub(crate) fn dismiss_notice(&mut self, n: &Notice, cx: &App) -> bool {
        if !n.dismissable() {
            return false;
        }
        let text = n.text().as_ref();
        let transient = if self.notices.refusal.as_deref() == Some(text) {
            self.notices.refusal = None;
            true
        } else if self.notices.refused.as_deref() == Some(text) {
            self.notices.refused = None;
            true
        } else if self.notices.nothing_to_switch && text == NOTHING_TO_SWITCH {
            self.notices.nothing_to_switch = false;
            true
        } else if n.tone() == Tone::Danger && self.notices.restore.iter().any(|r| r == text) {
            self.notices.restore.retain(|r| r != text);
            true
        } else {
            false
        };
        if transient {
            self.rebuild_chrome(cx);
            return true;
        }
        self.dismissed.dismiss(n)
    }

    /// The trader acted in the tile: the restore's notices have been seen.
    pub(super) fn user_acted(&mut self, cx: &mut Context<Self>) {
        if !self.notices.restore.is_empty() || self.notices.refused.is_some() {
            self.notices.restore.clear();
            self.notices.refused = None;
            self.rebuild_chrome(cx);
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn notice_texts(&self) -> Vec<String> {
        self.reported.iter().map(|n| n.text().to_string()).collect()
    }
}
