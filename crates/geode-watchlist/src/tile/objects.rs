//! The watchlist's own verbs: New, Clone, Rename, Delete and Revert, each
//! reached from the palette and the `⋯` menu (no default chord). A name is
//! asked in the prompt field and checked by `prompt::submit_object` before
//! anything is written; Rename, Delete and Revert then ask y/n on the
//! in-tile confirm bar (`geode_tile::confirm`), which holds the keyboard
//! until it is answered. Every write is a whole-object `ConfigEdit`
//! through the frame's config door; a rename is one batch of two (set the
//! new name, remove the old), so the shell writes both or neither.
//!
//! The gates: Rename and Delete act only on a list the user layer owns
//! outright (`own`): a desk or builtin one cannot be removed from the
//! user layer, and removing a user copy over a lower layer's would leave
//! that copy standing under the old name, which Revert… is for
//! (`revertible`). A confirmed revert is awaited (`reverting`): until the
//! reload no longer shows a user copy, the member and rules verbs on that
//! list are refused, since an edit built on the user copy would replace
//! the removal in the shell's batch.

use geode_core::config::Layer;
use geode_core::watchlist::state::WatchlistSnapshot;
use geode_core::watchlist::{self as watchlist, WATCHLISTS_DOC, Watchlist, Watchlists};
use geode_shell::frame::ConfigEdit;
use geode_tile::confirm::{self, Confirm, ConfirmHost};
use geode_tile::menu::Menu;
use geode_tile::notice::Notice;
use gpui::{Context, Window};

use super::{MenuKind, NOTHING_SHOWN, WatchlistTile, snapshot};
use crate::core::prompt::{self, Prompt, Step};

/// What an armed confirm does on `y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    Rename { from: String, to: String },
    Delete { name: String },
    Revert { name: String },
}

/// A create, clone, rename or delete the tile has written and is showing
/// ahead of the reload that carries it. A refusal from the shell puts
/// back what was shown before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Awaiting {
    /// The list the tile shows meanwhile (created, cloned or renamed to);
    /// `None` after a delete.
    pub(crate) shows: Option<String>,
    /// The one it showed before, to go back to on a refusal.
    pub(crate) restores: Option<String>,
    /// The one renamed or deleted: kept out of the switcher meanwhile.
    pub(crate) removed: Option<String>,
}

/// The verbs that remove the shown list's user definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Rename,
    Delete,
}

/// Why a verb cannot act: a few words for a menu row's lane, a sentence
/// for the header.
pub(crate) struct Blocked {
    pub(crate) short: &'static str,
    pub(crate) long: String,
}

impl Blocked {
    pub(crate) fn same(text: &'static str) -> Blocked {
        Blocked {
            short: text,
            long: text.to_string(),
        }
    }
}

/// The whole object `list`, written under `name`.
fn set_edit(name: &str, list: &Watchlist) -> ConfigEdit {
    ConfigEdit {
        doc: WATCHLISTS_DOC,
        object: name.to_string(),
        value: Some(watchlist::to_toml(list)),
        origin: None,
    }
}

/// `name`'s user definition, removed.
fn remove_edit(name: &str) -> ConfigEdit {
    ConfigEdit {
        doc: WATCHLISTS_DOC,
        object: name.to_string(),
        value: None,
        origin: None,
    }
}

/// Every list the snapshot defines, for the name check.
pub(super) fn defined(snapshot: &WatchlistSnapshot) -> Watchlists {
    let mut lists = Watchlists::default();
    for (name, state) in &snapshot.lists {
        lists.insert(name.clone(), state.definition.clone());
    }
    lists
}

/// The object verbs, their gates, and the confirm's answers.
impl WatchlistTile {
    /// The shown list when the user layer owns it outright, so a rename or
    /// delete can remove it; else why not. A list with no recorded layer
    /// is refused too: a removal from the user layer might remove nothing,
    /// and the tile would wait for a reload that never comes.
    pub(super) fn own(&self, snapshot: &WatchlistSnapshot, verb: Verb) -> Result<String, Blocked> {
        let Some((name, state)) = self.shown(snapshot) else {
            return Err(Blocked::same(NOTHING_SHOWN));
        };
        let verb_name = match verb {
            Verb::Rename => "rename",
            Verb::Delete => "delete",
        };
        let lower = |layer: Layer, short| Blocked {
            short,
            long: format!(
                "{name} is defined in {} config; Geode cannot {verb_name} it",
                layer.name()
            ),
        };
        match state.layer {
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
        if let Some(under) = state.shadowed {
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
                    "{name} shadows the {} copy \u{2014} Revert\u{2026} removes it",
                    under.name()
                ),
            });
        }
        Ok(name)
    }

    /// The shown list and the layer of the copy under the user's, when
    /// one stands there.
    pub(super) fn revertible(
        &self,
        snapshot: &WatchlistSnapshot,
    ) -> Result<(String, Layer), String> {
        let Some((name, state)) = self.shown(snapshot) else {
            return Err(NOTHING_SHOWN.into());
        };
        match state.shadowed {
            Some(under) => Ok((name, under)),
            None => Err(format!("{name} has no copy beneath yours to revert to")),
        }
    }

    /// Whether the shown list's revert is still on its way: the list is
    /// the one reverted and the snapshot still shows a user copy of it.
    pub(super) fn reverting(&self) -> Option<&str> {
        self.reverting
            .as_deref()
            .filter(|n| self.state.name.as_deref() == Some(*n))
    }

    /// What a verb refused while the revert is on its way says.
    pub(super) fn reverting_text(name: &str) -> String {
        format!("reverting {name}\u{2026}")
    }

    /// Whether the snapshot no longer shows a user copy of `name`: the
    /// revert's reload has landed (or the list went away).
    pub(super) fn revert_landed(name: &str, snapshot: &WatchlistSnapshot) -> bool {
        snapshot
            .lists
            .get(name)
            .is_none_or(|s| s.layer != Some(Layer::User))
    }

    /// Whether the snapshot now carries what `a` wrote.
    pub(super) fn landed(a: &Awaiting, snapshot: &WatchlistSnapshot) -> bool {
        a.shows
            .as_ref()
            .is_none_or(|n| snapshot.lists.contains_key(n))
            && a.removed
                .as_ref()
                .is_none_or(|n| !snapshot.lists.contains_key(n))
    }

    /// A verb is starting: the menu closes and the last verb's word goes.
    fn begin(&mut self, cx: &mut Context<Self>) {
        self.close_menu(cx);
        self.notices.outcome.clear();
        self.rebuild_chrome(cx);
    }

    /// `Watchlist: New…`: ask a name, with or without a list shown.
    pub(super) fn open_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin(cx);
        self.open_prompt(Prompt::NewName, Vec::new(), window, cx);
    }

    /// `Watchlist: Clone…`: ask the name a copy of the shown list is
    /// written under. Nothing shown: refused.
    pub(super) fn open_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((from, _)) = self.shown(&snapshot(cx)) else {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        };
        self.begin(cx);
        self.open_prompt(Prompt::CloneName { from }, Vec::new(), window, cx);
    }

    /// `Watchlist: Rename…`: ask the new name of a list the user layer
    /// owns outright; else refused with why.
    pub(super) fn open_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.own(&snapshot(cx), Verb::Rename) {
            Ok(from) => {
                self.begin(cx);
                self.open_prompt(Prompt::Rename { from }, Vec::new(), window, cx);
            }
            Err(refusal) => self.refuse(refusal.long, cx),
        }
    }

    /// `Watchlist: Delete…`: ask y/n over a list the user layer owns
    /// outright; else refused with why.
    pub(super) fn ask_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.own(&snapshot(cx), Verb::Delete) {
            Ok(name) => {
                self.begin(cx);
                let question = format!("delete {name} \u{2014} y deletes");
                confirm::arm(self, Pending::Delete { name }, question, window, cx);
                cx.notify();
            }
            Err(refusal) => self.refuse(refusal.long, cx),
        }
    }

    /// `Watchlist: Revert…`: ask y/n over a user copy with a lower copy
    /// beneath it, naming that copy's layer; else refused with why.
    pub(super) fn ask_revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.revertible(&snapshot(cx)) {
            Ok((name, under)) => {
                self.begin(cx);
                let question = format!(
                    "revert {name} to the {} copy \u{2014} y reverts",
                    under.name()
                );
                confirm::arm(self, Pending::Revert { name }, question, window, cx);
                cx.notify();
            }
            Err(why) => self.refuse(why, cx),
        }
    }

    /// A checked rename's y/n. No reference count: nothing names a
    /// watchlist yet, so there is nothing a rename would leave pointing at
    /// the old name.
    pub(super) fn ask_rename(
        &mut self,
        from: String,
        to: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let question = format!("rename {from} \u{2192} {to} \u{2014} y renames");
        confirm::arm(self, Pending::Rename { from, to }, question, window, cx);
        cx.notify();
    }

    /// Queue `edits` as one batch and show `awaiting.shows` meanwhile; the
    /// shown list's history ends with its name. After a delete the tile
    /// shows the empty state and opens the switcher without the deleted
    /// name.
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
                self.grid = crate::core::grid::GridModel::new();
                self.grid.set_sort(self.state.sort);
                self.find_entry = None;
                self.was_shown = false;
                let rows = self.switch_rows(&snapshot(cx));
                self.menu =
                    (!rows.is_empty()).then(|| (MenuKind::Switch, Menu::new(rows, &self.chords)));
                self.menu_at = None;
                self.rebuild_rows(false, cx);
            }
        }
        cx.notify();
    }

    /// Write an empty list `name` and show it (`saving <name>…` until the
    /// reload carries it).
    pub(super) fn create(&mut self, name: String, cx: &mut Context<Self>) {
        self.notices.outcome.clear();
        let restores = self.state.name.clone();
        self.write_objects(
            vec![set_edit(&name, &Watchlist::default())],
            Awaiting {
                shows: Some(name),
                restores,
                removed: None,
            },
            cx,
        );
    }

    /// Write the shown list as it is now (the pending object while an
    /// edit awaits its reload, so what the trader sees is what is copied)
    /// under `to`, and show it. `from` is not touched: no edit is queued
    /// for it, so a desk-layer list is not forked by its clone.
    pub(super) fn clone_list(&mut self, from: String, to: String, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let Some(state) = snapshot.lists.get(&from) else {
            self.refuse(format!("not cloned: {from} no longer exists"), cx);
            return;
        };
        let current = if self.state.name.as_deref() == Some(from.as_str()) {
            self.history.current(&state.definition).clone()
        } else {
            state.definition.clone()
        };
        self.notices.outcome.clear();
        self.write_objects(
            vec![set_edit(&to, &current)],
            Awaiting {
                shows: Some(to),
                restores: Some(from),
                removed: None,
            },
            cx,
        );
    }

    /// `y` on a rename: the new object and the old one's removal in one
    /// batch, validated again (the lists may have moved while the question
    /// stood). The object written is the shown one as the tile has it: an
    /// edit still on its way to the old name goes with the rename, not
    /// into the old name's removal.
    fn rename(&mut self, from: String, to: String, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let Some(state) = snapshot.lists.get(&from) else {
            self.refuse(format!("not renamed: {from} no longer exists"), cx);
            return;
        };
        let asked = Prompt::Rename { from: from.clone() };
        match prompt::submit_object(&asked, &to, &defined(&snapshot)) {
            Step::Rename { .. } => {}
            Step::Refuse(why) => {
                self.refuse(format!("not renamed: {why}"), cx);
                return;
            }
            Step::Add(_)
            | Step::Next(_)
            | Step::Rules(_)
            | Step::Create { .. }
            | Step::Clone { .. } => {
                unreachable!("a rename step leads to a rename or a refusal")
            }
        }
        let current = if self.state.name.as_deref() == Some(from.as_str()) {
            self.history.current(&state.definition).clone()
        } else {
            state.definition.clone()
        };
        self.notices.outcome.clear();
        self.write_objects(
            vec![set_edit(&to, &current), remove_edit(&from)],
            Awaiting {
                shows: Some(to),
                restores: Some(from.clone()),
                removed: Some(from),
            },
            cx,
        );
    }

    /// The armed question, for tests.
    #[cfg(test)]
    pub(super) fn question(&self) -> Option<String> {
        self.confirm.as_ref().map(|c| c.prompt_text().to_string())
    }
}

impl ConfirmHost for WatchlistTile {
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
            Pending::Revert { name } => {
                self.notices.outcome.clear();
                self.frame.queue_config_edits(vec![remove_edit(&name)], cx);
                self.reverting = Some(name);
                self.history.forget();
                self.close_rules(cx);
                self.rebuild_rows(false, cx);
                cx.notify();
            }
        }
    }

    fn cancelled(&mut self, pending: Pending, _: &mut Window, cx: &mut Context<Self>) {
        let said = match pending {
            Pending::Rename { from, .. } => format!("{from} not renamed"),
            Pending::Delete { name } => format!("{name} not deleted"),
            Pending::Revert { name } => format!("{name} not reverted"),
        };
        self.notices.outcome.clear();
        self.notices.outcome(Notice::status(said));
        self.rebuild_chrome(cx);
        cx.notify();
    }
}
