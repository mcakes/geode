//! The tile's chrome: the header model, the notices and their
//! dismissal, the empty state, and the watchlist switcher's menu.

use geode_core::volviews::Cells;
use geode_core::watchlist::state::{Status, WatchlistSnapshot};
use geode_shell::actions::ActionId;
use geode_shell::keymap::Keystroke;
use geode_shell::tips;
use geode_tile::menu::{ActionRow, Hint, Menu, MenuHost, MenuPick, Row as MenuRow};
use geode_tile::notice::{Notice, Tone};
use gpui::{App, Context, SharedString, Window};

use super::table::sortable;
use super::{
    COLLAPSE_ALL_ACTION, EXPAND_ALL_ACTION, INSPECT_ACTION, LENS_ACTION, LOOKBACK_ACTION,
    METRIC_ACTION, PAINT_ACTION, REFRESH_ACTION, SHOWN_ACTION, SORT_ACTION, SWITCH_ACTION,
    TIMESERIES_ACTION, TOGGLE_ACTION, VIEW_ACTIONS, VolwatchTile, WINDOW_ACTION, YANK_ACTION,
    snapshot,
};
use crate::core::grouping;
use crate::core::model::header;

/// Which menu is up. Menu mode, its keys and its pick door are shared by
/// every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Switch,
    /// The `.` / `\u{22ef}` / right-press menu of the tile's verbs.
    Actions,
}

/// What a menu row does when picked.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pick {
    /// Show this watchlist.
    Show(SharedString),
    /// Show this tab, as its `g N` chord would; a tab past the ninth has
    /// no chord and only this door.
    View(SharedString),
    /// Run this action id through the tile's own door, as its key would.
    Verb(&'static str),
}

impl MenuPick for Pick {
    fn element_name(&self) -> SharedString {
        match self {
            Pick::Show(name) | Pick::View(name) => name.clone(),
            Pick::Verb(id) => SharedString::new_static(id),
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
    /// The shown view went away (from the session or a reload); stands
    /// until the trader picks a view.
    pub(crate) warning: Option<String>,
}

/// A chord in the keymap's own spelling (`g w`, `shift+r`).
pub(crate) fn spell(keystrokes: &[Keystroke]) -> String {
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
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picked) = self.menu.as_ref().and_then(|m| m.menu.pick(index)) else {
            return;
        };
        match picked {
            Err(reason) if self.is_actions() => {
                self.close_menu();
                self.notices.refused = Some(reason.to_string());
                self.rebuild_chrome(cx);
                cx.notify();
            }
            Err(_) => {}
            Ok(Pick::Show(name)) => {
                self.close_menu();
                self.show(&name, cx);
            }
            Ok(Pick::View(name)) => {
                self.close_menu();
                self.user_acted(cx);
                let _ = self.set_view_by_name(&name, cx);
            }
            Ok(Pick::Verb(id)) => {
                self.close_menu();
                self.dispatch(&ActionId(id.to_string()), None, window, cx);
                cx.notify();
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
/// The configuration action that edits `vol_views.toml`, and its palette
/// title, which the empty tab names when no key is bound to it.
const VOL_VIEWS_ACTION: &str = "config::vol_views";
const VOL_VIEWS_LABEL: &str = "Edit vol views\u{2026}";

impl VolwatchTile {
    pub(super) fn settle_menu(&mut self, shown: bool, snapshot: &WatchlistSnapshot) {
        let listed = !snapshot.lists.is_empty();
        let first = listed && !self.settled;
        let gone = self.was_shown && !shown;
        self.settled |= listed;
        self.was_shown = shown;
        match self.menu.as_ref().map(|m| m.kind) {
            Some(MenuKind::Actions) => {}
            Some(MenuKind::Switch) => {
                let rows = self.switch_rows(snapshot);
                if rows.is_empty() {
                    self.close_menu();
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
            self.close_menu();
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

    /// Close whichever menu is up, and release the pointer anchor.
    pub(super) fn close_menu(&mut self) {
        self.menu = None;
        self.menu_at = None;
    }

    pub(super) fn is_actions(&self) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|m| m.kind == MenuKind::Actions)
    }

    /// `.` and the header's `\u{22ef}`: the actions menu hung under the
    /// header; a second press (or a switcher in the way) replaces or closes it.
    pub(super) fn toggle_menu(&mut self, cx: &mut Context<Self>) {
        if self.is_actions() {
            self.close_menu();
        } else {
            self.open_actions(None);
        }
        cx.notify();
    }

    /// Open the actions menu, hung from `at` when it came from a pointer.
    pub(super) fn open_actions(&mut self, at: Option<gpui::Point<gpui::Pixels>>) {
        self.menu = Some(MenuState {
            kind: MenuKind::Actions,
            menu: Menu::new(self.actions_rows(), &self.chords),
        });
        self.menu_at = at;
    }

    fn verb(&self, id: &'static str, title: impl Into<SharedString>) -> ActionRow<Pick> {
        ActionRow::new(Pick::Verb(id), title).hint(Hint::chord(id))
    }

    /// The verbs for the current state: the active view and paint are
    /// ticked; a verb its key would refuse, or whose effect the view
    /// cannot show, is disabled with the reason. The fold verbs need a
    /// grouped tile, and the toggle a group row under the cursor.
    pub(super) fn actions_rows(&self) -> Vec<MenuRow<Pick>> {
        // `m` swaps the number a single cell shows; a pair view shows both.
        let swap = if self.cells == Cells::Single {
            Ok(())
        } else {
            Err(SharedString::new_static("a pair view shows both"))
        };
        let column = self.columns.get(self.cursor_col);
        let sort = match column {
            Some(c) if sortable(&c.kind) => Ok(()),
            Some(c) => Err(SharedString::from(format!("{} is not sortable", c.head))),
            None => Err(SharedString::new_static("no column")),
        };
        let inspect = match self.cursor_row() {
            None => Err(SharedString::new_static("no row")),
            Some(r) if r.group.is_some() => {
                Err(SharedString::new_static("inspect needs an underlying"))
            }
            Some(r) if self.board.is_some() && !r.present => Err(SharedString::from(format!(
                "{} is not in the surface",
                r.name
            ))),
            Some(_) => Ok(()),
        };
        let grouped = if self.grouping.kept.is_empty() {
            Err(SharedString::new_static("the tile is not grouped"))
        } else {
            Ok(())
        };
        let on_group = if self.cursor_row().is_some_and(|r| r.group.is_some()) {
            Ok(())
        } else {
            Err(SharedString::new_static("the cursor is not on a group"))
        };
        let mut rows = vec![
            MenuRow::Action(self.verb(SWITCH_ACTION, "Switch list\u{2026}")),
            MenuRow::Separator,
        ];
        rows.extend(self.strip_tabs.iter().enumerate().map(|(i, (name, _))| {
            let hint = VIEW_ACTIONS.get(i).map_or(Hint::None, |id| Hint::chord(id));
            MenuRow::Action(
                ActionRow::new(Pick::View(name.clone()), format!("{name} view"))
                    .hint(hint)
                    .checked(self.shown_name.as_ref() == Some(name)),
            )
        }));
        rows.extend([
            MenuRow::Separator,
            MenuRow::Action(self.verb(LENS_ACTION, "Toggle moves / rich-cheap")),
            MenuRow::Action(self.verb(LOOKBACK_ACTION, "Cycle lookback")),
            MenuRow::Action(self.verb(METRIC_ACTION, "Toggle z-score / percentile")),
            MenuRow::Action(self.verb(WINDOW_ACTION, "Cycle window")),
            MenuRow::Action(
                self.verb(SHOWN_ACTION, "Swap level / companion")
                    .enabled(swap),
            ),
            MenuRow::Action(self.verb(PAINT_ACTION, "Paint").checked(self.paint)),
            MenuRow::Action(self.verb(SORT_ACTION, "Sort by column").enabled(sort)),
            MenuRow::Action(self.verb(TOGGLE_ACTION, "Toggle group").enabled(on_group)),
            MenuRow::Action(
                self.verb(EXPAND_ALL_ACTION, "Expand all groups")
                    .enabled(grouped.clone()),
            ),
            MenuRow::Action(
                self.verb(COLLAPSE_ALL_ACTION, "Collapse all groups")
                    .enabled(grouped),
            ),
            MenuRow::Separator,
            MenuRow::Action(self.verb(INSPECT_ACTION, "Inspect row").enabled(inspect)),
            MenuRow::Action(self.verb(TIMESERIES_ACTION, "Open timeseries")),
            MenuRow::Action(self.verb(YANK_ACTION, "Yank cell")),
            MenuRow::Separator,
            MenuRow::Action(self.verb(REFRESH_ACTION, "Refresh now")),
        ]);
        rows
    }

    /// Ticks and enablement follow the state while the actions menu is up.
    fn refresh_actions(&mut self) {
        if !self.is_actions() {
            return;
        }
        let rows = self.actions_rows();
        if let Some(m) = self.menu.as_mut() {
            m.menu.replace_rows(rows, &self.chords);
        }
    }

    /// The open menu's action titles in order.
    #[cfg(test)]
    pub(crate) fn menu_titles(&self) -> Vec<String> {
        self.menu
            .iter()
            .flat_map(|m| m.menu.rows())
            .filter_map(MenuRow::action)
            .map(|a| a.title().to_string())
            .collect()
    }

    /// The header's title, count, lens chips, grouping chip and board
    /// counts, from the shown list's name: rebuilt with the chrome, so a
    /// list switch or a restore whose snapshot has not arrived names its
    /// list at once. The grouping chip is muted while nothing groups.
    pub(crate) fn rebuild_header(&mut self) {
        let sort = self.sort_label();
        self.header = header(
            self.state.list.as_deref(),
            self.members.len(),
            &self.state.lens,
            sort.as_deref(),
            self.board.as_deref(),
            &grouping::chip_text(&self.state.pin, &self.grouping),
            self.grouping.kept.is_empty(),
        );
    }

    pub(crate) fn rebuild_chrome(&mut self, cx: &App) {
        self.rebuild_header();
        self.refresh_actions();
        self.rebuild_strip_hints();
        let snapshot = snapshot(cx);
        let list = self.state.list.as_deref();
        let state = list.and_then(|n| snapshot.lists.get(n));
        let none_defined = snapshot.lists.is_empty();
        let switch = tips::chord_for(&self.chords, SWITCH_ACTION)
            .map(|ks| spell(&ks))
            .unwrap_or_else(|| "Vol: Switch list".to_string());
        let no_columns = self
            .shown_view()
            .filter(|v| v.columns.is_empty())
            .map(|v| v.name);
        self.empty = match (list, state.is_some()) {
            (_, true) => no_columns.map(|name| {
                let edit = tips::chord_for(&self.chords, VOL_VIEWS_ACTION)
                    .map(|ks| spell(&ks))
                    .unwrap_or_else(|| VOL_VIEWS_LABEL.to_string());
                format!("{name} has no columns \u{2014} {edit} adds one").into()
            }),
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
            .chain(n.warning.iter())
            .cloned()
            .map(Notice::warning)
            .chain(
                self.vega
                    .refused
                    .iter()
                    .map(|e| Notice::warning(format!("[volwatch] vega: {e}"))),
            )
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
        } else if self.notices.warning.as_deref() == Some(text) {
            self.notices.warning = None;
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
