//! The tile's edit history: undo and redo stacks over one watchlist, and
//! the optimistic object an edit produced before the snapshot carrying it
//! arrives.
//!
//! Every verb works over [`History::current`]: the pending object while one
//! is waiting for its reload, else the snapshot's definition. Two quick
//! edits before a reload therefore compose (the second builds on the
//! first), and undo replays change by change over what is current,
//! skipping a change another surface made since (`edit::undo`).
//!
//! The pending object outlives a reload that is not its answer. Each edit
//! the tile queues is in flight until a reload carries it, and the shell
//! writes them in order, so a reload carrying an earlier one (the second
//! edit was queued after the first write fired), or still carrying the
//! object the edits started from (another list or document changed, or a
//! resolution answered under the same definition), is a step behind the
//! pending object: dropping it then would flash the later edits off, and
//! the next edit, built over the reload, would overwrite them. Any other
//! object is someone else's write, and is the truth now.
//!
//! A refused write leaves its names as the configuration has them, so undo
//! later skips them like names changed elsewhere; [`History::unsaved`]
//! tells the two apart, so the notice does not blame another surface for
//! the tile's own write that never landed.

use std::collections::BTreeSet;

use geode_core::watchlist::Watchlist;
use geode_core::watchlist::edit::{self, Change, UndoEntry, manual_of};

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    pending: Option<Watchlist>,
    /// The configuration's object the in-flight edits were made over.
    base: Option<Watchlist>,
    /// Every object an edit produced since `base`, oldest first; the last
    /// is `pending`.
    in_flight: Vec<Watchlist>,
    /// Names a refused write would have changed, until another list is
    /// shown: an undo that skips one skips the tile's own unsaved edit, not
    /// another surface's.
    unsaved: BTreeSet<String>,
    /// A refused write would have changed the rules.
    rules_unsaved: bool,
}

/// What an undo or redo left as it was: the names whose manual state had
/// changed since the entry was made, and whether the rules had.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Skipped {
    pub names: Vec<String>,
    pub rules: bool,
}

impl Skipped {
    pub fn count(&self) -> usize {
        self.names.len() + usize::from(self.rules)
    }
}

/// What an undo or redo did: the next object, how many changes it
/// replayed, and what it skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    pub next: Watchlist,
    pub applied: usize,
    pub skipped: Skipped,
}

/// Which way the history steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    Undo,
    Redo,
}

impl History {
    /// The object the tile shows and edits: the optimistic pending one if
    /// any, else the configuration's.
    pub fn current<'a>(&'a self, config: &'a Watchlist) -> &'a Watchlist {
        self.pending.as_ref().unwrap_or(config)
    }

    /// The optimistic object awaiting its reload, if any.
    pub fn pending(&self) -> Option<&Watchlist> {
        self.pending.as_ref()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// A new edit made over the current object: `entry` goes on the undo
    /// stack, redo is cleared (it was undone over an object this edit
    /// replaces), and `next` is held pending behind any edit already in
    /// flight. An empty entry records nothing: nothing changed.
    pub fn push(&mut self, config: &Watchlist, next: Watchlist, entry: UndoEntry) {
        if entry.is_empty() {
            return;
        }
        self.undo.push(entry);
        self.redo.clear();
        self.hold(config, next);
    }

    /// What stepping `way` would do over the current object, without
    /// doing it: the tile gates the write on it before the stacks move.
    /// `None` with nothing that way.
    pub fn peek(&self, config: &Watchlist, way: Way) -> Option<Replay> {
        let entry = match way {
            Way::Undo => self.undo.last()?,
            Way::Redo => self.redo.last()?,
        };
        Some(Self::replay_over(self.current(config), entry).0)
    }

    /// Step `way`: the top entry replayed over the current object, the
    /// entry that reverses it pushed on the other stack, and the next
    /// object held pending. A replay nothing of which applies (every
    /// change was made elsewhere since, or never saved) is dropped from
    /// its stack and holds nothing: nothing of it can ever apply again,
    /// and crossing as an empty reverse entry would make the other verb
    /// say it did nothing. What [`History::peek`] said.
    pub fn step(&mut self, config: &Watchlist, way: Way) -> Option<Replay> {
        let (from, to) = match way {
            Way::Undo => (&mut self.undo, &mut self.redo),
            Way::Redo => (&mut self.redo, &mut self.undo),
        };
        let entry = from.pop()?;
        let current = self.pending.as_ref().unwrap_or(config);
        let (replay, reverse) = Self::replay_over(current, &entry);
        if replay.applied == 0 {
            return Some(replay);
        }
        to.push(reverse);
        self.hold(config, replay.next.clone());
        Some(replay)
    }

    /// Revert the last entry: [`History::step`] the undo way.
    pub fn undo(&mut self, config: &Watchlist) -> Option<Replay> {
        self.step(config, Way::Undo)
    }

    /// Re-apply the last undone entry: [`History::step`] the redo way.
    pub fn redo(&mut self, config: &Watchlist) -> Option<Replay> {
        self.step(config, Way::Redo)
    }

    /// `edit::undo` over the current object, with the skipped changes
    /// named: a name whose manual state is no longer the entry's `after`,
    /// or rules no longer the entry's `after`, were changed elsewhere.
    /// Also the entry that reverses what was replayed.
    fn replay_over(current: &Watchlist, entry: &UndoEntry) -> (Replay, UndoEntry) {
        let mut skipped = Skipped::default();
        // Backwards, as `edit::undo` replays.
        for c in entry.changes.iter().rev() {
            match c {
                Change::Name { name, after, .. } if manual_of(current, name) != *after => {
                    skipped.names.push(name.clone())
                }
                Change::Rules { after, .. } if current.rules != *after => skipped.rules = true,
                _ => {}
            }
        }
        let (next, reverse, count) = edit::undo(current, entry);
        debug_assert_eq!(
            count,
            skipped.count(),
            "the same rule names the skipped changes"
        );
        let replay = Replay {
            next,
            applied: entry.changes.len() - count,
            skipped,
        };
        (replay, reverse)
    }

    /// Keep `next` as the pending object over `config`, in flight behind
    /// any earlier edit. A pending object already held keeps its base:
    /// `config` is still that base or one of the in-flight objects, or a
    /// reload would have dropped it.
    fn hold(&mut self, config: &Watchlist, next: Watchlist) {
        if self.pending.is_none() {
            self.base = Some(config.clone());
            self.in_flight.clear();
        }
        self.in_flight.push(next.clone());
        self.pending = Some(next);
    }

    /// A reload brought `config` for this list. The pending object itself:
    /// every edit landed, and the copy goes. The base or an earlier
    /// in-flight object: the later edits are still on their way, so the
    /// copy stays, and the edits it carries are no longer in flight.
    /// Anything else is another surface's write: the copy goes.
    pub fn reloaded(&mut self, config: &Watchlist) {
        let Some(pending) = &self.pending else {
            return;
        };
        if pending == config {
            self.drop_pending();
        } else if self.base.as_ref() == Some(config) {
        } else if let Some(at) = self.in_flight.iter().position(|o| o == config) {
            self.in_flight.drain(..=at);
            self.base = Some(config.clone());
        } else {
            self.drop_pending();
        }
    }

    /// The write was refused and the configuration's object stands. The
    /// names (and the rules) the in-flight edits changed are remembered as
    /// never saved.
    pub fn refused(&mut self) {
        if let (Some(pending), Some(base)) = (&self.pending, &self.base) {
            let names = pending
                .include
                .iter()
                .chain(&pending.exclude)
                .chain(&base.include)
                .chain(&base.exclude)
                .filter(|n| manual_of(pending, n) != manual_of(base, n));
            self.unsaved.extend(names.cloned());
            self.rules_unsaved |= pending.rules != base.rules;
        }
        self.drop_pending();
    }

    /// How many of `names` are ones a refused write of this tile changed:
    /// left as they are because the edit was never saved, not because
    /// another surface changed them.
    pub fn unsaved(&self, names: &[String]) -> usize {
        names.iter().filter(|n| self.unsaved.contains(*n)).count()
    }

    /// Whether a refused write of this tile would have changed the rules.
    pub fn rules_unsaved(&self) -> bool {
        self.rules_unsaved
    }

    fn drop_pending(&mut self) {
        self.pending = None;
        self.base = None;
        self.in_flight.clear();
    }

    /// Another list is shown, or this one was renamed or deleted: its
    /// entries mean nothing any more.
    pub fn forget(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.unsaved.clear();
        self.rules_unsaved = false;
        self.drop_pending();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::watchlist::Rule;
    use geode_core::watchlist::members::{Member, Origin};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn list(include: &[&str], exclude: &[&str]) -> Watchlist {
        Watchlist {
            include: s(include),
            exclude: s(exclude),
            rules: vec![],
        }
    }

    /// Add `name` by hand over the current object, as the tile's add verb
    /// does, and return what it wrote.
    fn add(h: &mut History, cfg: &Watchlist, name: &str) -> Watchlist {
        let (next, entry) = edit::add(h.current(cfg), &[], &s(&[name])).unwrap();
        h.push(cfg, next.clone(), entry);
        next
    }

    fn manual(name: &str) -> Member {
        Member {
            name: name.into(),
            origin: Origin::Manual,
        }
    }

    #[test]
    fn an_edit_is_visible_before_the_reload_and_dropped_after() {
        let cfg = list(&["SPX"], &[]);
        let mut h = History::default();
        let next = add(&mut h, &cfg, "NDX");
        assert_eq!(h.current(&cfg), &next);
        assert_eq!(h.pending(), Some(&next));
        h.reloaded(&next);
        assert_eq!(h.current(&next), &next);
        assert!(h.pending().is_none(), "the reload carried it");
    }

    /// A reload that leaves this list as it was (another list or document
    /// changed, or a resolution answered under the same definition) is not
    /// the one carrying the edit: the edit stays on screen, and the next
    /// edit builds on it.
    #[test]
    fn an_unrelated_reload_keeps_the_pending_edit() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        let first = add(&mut h, &cfg, "SPX");
        h.reloaded(&cfg);
        assert_eq!(h.current(&cfg), &first);
        let second = add(&mut h, &cfg, "NDX");
        assert_eq!(second.include, s(&["SPX", "NDX"]));
    }

    /// The second edit was queued after the first write fired: the reload
    /// carrying the first is a step behind, keeps both, and the third edit
    /// builds on both; the reload carrying the last drops the copy.
    #[test]
    fn a_reload_of_an_earlier_write_keeps_the_later_edits() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        let e1 = add(&mut h, &cfg, "SPX");
        let e2 = add(&mut h, &cfg, "NDX");
        h.reloaded(&e1);
        assert_eq!(h.current(&e1), &e2, "E2 does not flash off");
        let e3 = add(&mut h, &e1, "DAX");
        assert_eq!(
            e3,
            list(&["SPX", "NDX", "DAX"], &[]),
            "E3 builds on E1 and E2"
        );
        // E1 is no longer in flight: its reload again is someone's revert.
        h.reloaded(&e2);
        assert_eq!(h.current(&e2), &e3);
        h.reloaded(&e3);
        assert_eq!(h.current(&e3), &e3);
        assert!(h.pending().is_none(), "every edit landed");
    }

    /// A reload that changed this list otherwise (another tile wrote it) is
    /// the truth now: the optimistic copy goes.
    #[test]
    fn a_reload_changing_this_list_otherwise_drops_the_pending_edit() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        add(&mut h, &cfg, "SPX");
        let foreign = list(&["DAX"], &[]);
        h.reloaded(&foreign);
        assert_eq!(h.current(&foreign), &foreign);
    }

    #[test]
    fn a_refusal_drops_the_pending_edit() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        add(&mut h, &cfg, "SPX");
        h.refused();
        assert_eq!(h.current(&cfg), &cfg);
    }

    #[test]
    fn two_quick_edits_compose_and_undo_reverts_only_the_second() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        add(&mut h, &cfg, "SPX");
        add(&mut h, &cfg, "NDX"); // before any reload
        let r = h.undo(&cfg).unwrap();
        assert_eq!(r.skipped, Skipped::default());
        assert_eq!(r.applied, 1);
        assert_eq!(r.next.include, s(&["SPX"]));
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        add(&mut h, &cfg, "SPX");
        assert!(h.undo(&cfg).is_some());
        assert!(h.can_redo());
        add(&mut h, &cfg, "NDX");
        assert!(!h.can_redo());
        assert!(h.redo(&cfg).is_none());
    }

    #[test]
    fn an_edit_that_changes_nothing_records_nothing() {
        let cfg = list(&["SPX"], &[]);
        let mut h = History::default();
        h.push(&cfg, cfg.clone(), UndoEntry::default());
        assert!(!h.can_undo());
        assert!(h.undo(&cfg).is_none());
        assert_eq!(h.current(&cfg), &cfg, "no pending object either");
    }

    #[test]
    fn undo_after_a_foreign_change_skips_that_name() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        add(&mut h, &cfg, "SPX");
        h.refused();
        // Another tile excluded SPX since.
        let cfg = list(&[], &["SPX"]);
        let r = h.undo(&cfg).unwrap();
        assert_eq!(r.skipped.names, s(&["SPX"]));
        assert!(!r.skipped.rules);
        assert_eq!(r.applied, 0);
        assert_eq!(r.next, cfg);
    }

    /// Undo after a refusal skips the refused names, and says they were
    /// the tile's own unsaved edit rather than another surface's change.
    #[test]
    fn undo_after_a_refusal_counts_the_refused_names_as_unsaved() {
        let cfg = list(&["SPX"], &[]);
        let mut h = History::default();
        // One entry removing SPX (manual) and excluding NDX (from a rule).
        let members = vec![
            manual("SPX"),
            Member {
                name: "NDX".into(),
                origin: Origin::Rules(vec![0]),
            },
        ];
        let (next, entry) = edit::remove(&cfg, &members, &s(&["SPX", "NDX"]));
        h.push(&cfg, next, entry);
        h.refused();
        let r = h.undo(&cfg).unwrap();
        assert_eq!(r.skipped.names, s(&["NDX", "SPX"]), "replayed backwards");
        assert_eq!(h.unsaved(&r.skipped.names), 2);
        // A name another surface changed is not the tile's unsaved edit.
        assert_eq!(h.unsaved(&s(&["DAX"])), 0);
        assert!(!h.rules_unsaved());
        h.forget();
        assert_eq!(h.unsaved(&r.skipped.names), 0);
    }

    /// A rules change is one change: refused, it is the rules that were
    /// not saved; changed elsewhere, undo skips it whole.
    #[test]
    fn a_rules_change_is_skipped_whole_and_a_refusal_marks_the_rules_unsaved() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        let rule = Rule {
            dataset: "risk".into(),
            scope: None,
            expression: None,
        };
        let (next, entry) = edit::set_rules(&cfg, vec![rule.clone()]);
        h.push(&cfg, next, entry);
        h.refused();
        assert!(h.rules_unsaved());
        let r = h.undo(&cfg).unwrap();
        assert_eq!(
            r.skipped,
            Skipped {
                names: vec![],
                rules: true
            }
        );
        assert_eq!(r.skipped.count(), 1);
        assert_eq!(r.applied, 0);
    }

    /// A step nothing of which applies is dropped from its stack and holds
    /// nothing: the other verb never sees an empty entry, and no pending
    /// copy of an unchanged object is held. `peek` says the same as the
    /// step, without moving anything.
    #[test]
    fn a_step_that_applies_nothing_is_dropped_and_holds_nothing() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        add(&mut h, &cfg, "SPX");
        h.refused();
        // Another surface excluded SPX since: the add cannot be undone.
        let cfg = list(&[], &["SPX"]);
        let peeked = h.peek(&cfg, Way::Undo).unwrap();
        assert_eq!(peeked.applied, 0);
        assert!(h.can_undo(), "peeking moves nothing");
        let r = h.undo(&cfg).unwrap();
        assert_eq!(r, peeked);
        assert!(!h.can_undo() && !h.can_redo(), "dropped, not crossed");
        assert!(h.pending().is_none(), "nothing held");
        assert_eq!(h.current(&cfg), &cfg);
        assert_eq!(h.peek(&cfg, Way::Undo), None);
        // The redo way too: an undone add, then the name re-added elsewhere.
        let cfg = list(&[], &[]);
        add(&mut h, &cfg, "SPX");
        let cfg = list(&["SPX"], &[]);
        h.reloaded(&cfg);
        assert_eq!(h.undo(&cfg).unwrap().applied, 1);
        // A foreign write (NDX added, SPX put back) is the truth now.
        let cfg = list(&["SPX", "NDX"], &[]);
        h.reloaded(&cfg);
        assert_eq!(h.peek(&cfg, Way::Redo).unwrap().applied, 0);
        assert_eq!(h.redo(&cfg).unwrap().applied, 0);
        assert!(!h.can_redo() && !h.can_undo());
        assert!(h.pending().is_none());
    }

    #[test]
    fn redo_reapplies_and_forget_clears_everything() {
        let cfg = list(&[], &[]);
        let mut h = History::default();
        add(&mut h, &cfg, "SPX");
        let cfg = list(&["SPX"], &[]);
        h.reloaded(&cfg);
        let undone = h.undo(&cfg).unwrap();
        assert!(undone.next.include.is_empty());
        let redone = h.redo(&cfg).unwrap();
        assert_eq!(redone.skipped, Skipped::default());
        assert_eq!(redone.applied, 1);
        assert_eq!(redone.next.include, s(&["SPX"]));
        // Redo went back on the undo stack: it can be undone again.
        assert!(h.can_undo());
        h.forget();
        assert!(h.undo(&cfg).is_none());
        assert!(!h.can_undo() && !h.can_redo());
        assert_eq!(h.current(&cfg), &cfg);
    }
}
