//! The member verbs: what each acts on, what it writes through the
//! frame's config door, and what it says. Every write is the whole object
//! (`ConfigEdit { doc: watchlists, object, value: to_toml(next) }`; the
//! frame stamps the tile as its origin), checked here first, and shown at
//! once through the history's pending copy.

use std::collections::BTreeSet;

use geode_core::watchlist::edit::{self, UndoEntry};
use geode_core::watchlist::members::{Member, Origin};
use geode_core::watchlist::state::{WatchlistSnapshot, WatchlistState};
use geode_core::watchlist::{self as watchlist, WATCHLISTS_DOC, Watchlist};
use geode_shell::frame::ConfigEdit;
use geode_tile::notice::Notice;
use gpui::{Context, SharedString, Window};

use super::{
    NOT_WIRED, NOTHING_SHOWN, NOTHING_TO_REDO, NOTHING_TO_REMOVE, NOTHING_TO_UNDO, WatchlistTile,
    header, reference, snapshot,
};
use crate::core::history::Way;
use crate::core::prompt::Prompt;
use crate::core::rows;

/// The member verbs: what each acts on, what it writes, and what it says.
impl WatchlistTile {
    /// The shown list and its snapshot entry, for a verb; `None` with
    /// nothing shown.
    pub(super) fn shown<'a>(
        &self,
        snapshot: &'a WatchlistSnapshot,
    ) -> Option<(String, &'a WatchlistState)> {
        let name = self.state.name.as_deref()?;
        let state = snapshot.lists.get(name)?;
        Some((name.to_string(), state))
    }

    /// Whether the member verbs may act now. Always, so far: a queued
    /// revert will refuse them, since the tile cannot see the lower copy
    /// the revert will show.
    fn verbs_allowed(&self) -> Result<(), String> {
        Ok(())
    }

    /// A verb refused: say why, as a danger, in place of the last word.
    pub(super) fn refuse(&mut self, why: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.close_menu(cx);
        self.say(Notice::danger(why), cx);
    }

    /// Say `notice` in place of the last verb's word, writing nothing.
    fn say(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.notices.outcome.clear();
        self.notices.outcome(notice);
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// `o`/`enter`: open the add field, a free typeahead over every name
    /// the reference table and any list knows. A name already a member is
    /// refused at commit, naming where it comes from, not hidden here.
    pub(super) fn open_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        if self.shown(&snapshot).is_none() {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        }
        if let Err(why) = self.verbs_allowed() {
            self.refuse(why, cx);
            return;
        }
        self.close_menu(cx);
        let reference = reference(cx);
        let options: BTreeSet<String> = reference
            .keys(rows::REFERENCE_DATASET)
            .chain(snapshot.all_names())
            .map(str::to_string)
            .collect();
        self.open_prompt(Prompt::AddName, options.into_iter().collect(), window, cx);
    }

    /// `x`: remove the selection, else the cursor's row. A manual name
    /// leaves `include`, a rule-supplied one is excluded, one that is both
    /// does both in one write, an excluded one is restored
    /// (`edit::remove`). The cursor keeps its shown index: the next row
    /// lands under it.
    pub(super) fn remove(&mut self, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let Some((name, state)) = self.shown(&snapshot) else {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        };
        if let Err(why) = self.verbs_allowed() {
            self.refuse(why, cx);
            return;
        }
        self.close_menu(cx);
        let targets = self.grid.targets();
        let members = rows::members(state, self.history.pending());
        let config = state.definition.clone();
        let current = self.history.current(&config).clone();
        let (next, entry) = edit::remove(&current, &members, &targets);
        if entry.is_empty() {
            self.say(Notice::status(NOTHING_TO_REMOVE), cx);
            return;
        }
        let said = remove_notice(&targets, &members);
        self.commit(&name, &config, next, entry, Notice::status(said), cx);
    }

    /// `u`/`ctrl+r`: replay the history one step over the current object
    /// and write the result. A change another surface made since is
    /// skipped and said so; one the tile's own refused write left is said
    /// as not saved, not blamed on another surface.
    pub(super) fn replay(&mut self, way: Way, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let Some((name, state)) = self.shown(&snapshot) else {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        };
        if let Err(why) = self.verbs_allowed() {
            self.refuse(why, cx);
            return;
        }
        self.close_menu(cx);
        let config = state.definition.clone();
        let Some(replay) = self.history.peek(&config, way) else {
            let why = match way {
                Way::Undo => NOTHING_TO_UNDO,
                Way::Redo => NOTHING_TO_REDO,
            };
            self.say(Notice::status(why), cx);
            return;
        };
        let unsaved = self.history.unsaved(&replay.skipped.names)
            + usize::from(replay.skipped.rules && self.history.rules_unsaved());
        let elsewhere = replay.skipped.count() - unsaved;
        let verb = match way {
            Way::Undo => "undid",
            Way::Redo => "redid",
        };
        let mut text = format!("{verb} {}", header::plural(replay.applied, "change"));
        let mut tails = Vec::new();
        if elsewhere > 0 {
            tails.push(format!("{elsewhere} changed elsewhere"));
        }
        if unsaved > 0 {
            tails.push(format!("{unsaved} not saved"));
        }
        let notice = if tails.is_empty() {
            Notice::status(text)
        } else {
            text = format!("{text} \u{2014} {}", tails.join(", "));
            Notice::warning(text)
        };
        // Nothing applied: the entry is spent (the history drops it) and
        // there is nothing to write.
        if replay.applied == 0 {
            self.history.step(&config, way);
            self.say(notice, cx);
            return;
        }
        // The gate before the stacks move: a refused write leaves the
        // history as it was.
        if let Err(why) = self.queue_write(&name, &replay.next, cx) {
            self.say(Notice::danger(why), cx);
            return;
        }
        let stepped = self.history.step(&config, way);
        debug_assert_eq!(
            stepped.as_ref(),
            Some(&replay),
            "the step is what was peeked"
        );
        self.notices.outcome.clear();
        self.notices.outcome(notice);
        self.rebuild_rows(true, cx);
        cx.notify();
    }

    /// `shift+r`: ask the bridge to resolve the shown list now, through
    /// the factory's hook; the header shows `resolving…` with the next
    /// snapshot. A tile hosted without the hook says so.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let Some((name, _)) = self.shown(&snapshot) else {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        };
        self.close_menu(cx);
        self.notices.outcome.clear();
        let hook = self.shared.refresh.borrow().clone();
        match hook {
            Some(hook) => hook(&name, cx),
            None => self.notices.outcome(Notice::status(NOT_WIRED)),
        }
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// Record `entry` and write `next` for `name` through the config door:
    /// checked, queued whole-object, held pending (shown at once, ahead of
    /// the reload that carries it), and said.
    pub(super) fn commit(
        &mut self,
        name: &str,
        config: &Watchlist,
        next: Watchlist,
        entry: UndoEntry,
        notice: Notice,
        cx: &mut Context<Self>,
    ) {
        self.notices.outcome.clear();
        if let Err(why) = self.queue_write(name, &next, cx) {
            self.notices.outcome(Notice::danger(why));
            self.rebuild_chrome(cx);
            cx.notify();
            return;
        }
        self.history.push(config, next, entry);
        self.notices.outcome(notice);
        self.rebuild_rows(true, cx);
        cx.notify();
    }

    /// Queue the whole object through the frame's config door, which
    /// stamps this tile as the origin. Checked first: no name may be both
    /// included and excluded. No verb produces one (each moves a name to
    /// one manual state), so this is a last gate rather than a path; the
    /// door would write it even when the reload then rejected it.
    fn queue_write(
        &mut self,
        name: &str,
        next: &Watchlist,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        debug_assert!(
            next.include.iter().all(|n| !next.exclude.contains(n)),
            "no verb includes and excludes one name"
        );
        if let Some(both) = next.include.iter().find(|n| next.exclude.contains(n)) {
            return Err(format!("not saved: {both} is both included and excluded"));
        }
        self.frame.queue_config_edits(
            vec![ConfigEdit {
                doc: WATCHLISTS_DOC,
                object: name.to_string(),
                value: Some(watchlist::to_toml(next)),
                origin: None,
            }],
            cx,
        );
        Ok(())
    }
}

/// What `x` says of `targets`, by their origins among `members`: one name
/// by what happened to it, a selection by counts.
fn remove_notice(targets: &[String], members: &[Member]) -> String {
    let origin = |t: &str| members.iter().find(|m| m.name == t).map(|m| &m.origin);
    if let [one] = targets
        && let Some(o) = origin(one)
    {
        return match o {
            Origin::Manual => format!("removed {one}"),
            Origin::Rules(rules) => format!(
                "excluded {one} \u{2014} {} still supplies it; x again restores",
                rows::rules_text(rules)
            ),
            Origin::Both(_) => format!("removed and excluded {one}"),
            Origin::Excluded { .. } => format!("restored {one}"),
        };
    }
    let (mut removed, mut excluded, mut both, mut restored) = (0, 0, 0, 0);
    for t in targets {
        match origin(t) {
            Some(Origin::Manual) => removed += 1,
            Some(Origin::Rules(_)) => excluded += 1,
            Some(Origin::Both(_)) => both += 1,
            Some(Origin::Excluded { .. }) => restored += 1,
            None => {}
        }
    }
    [
        ("removed", removed),
        ("excluded", excluded),
        ("removed and excluded", both),
        ("restored", restored),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .map(|(verb, n)| format!("{verb} {}", header::plural(n, "name")))
    .collect::<Vec<_>>()
    .join(", ")
}
