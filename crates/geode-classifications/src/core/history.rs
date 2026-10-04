//! The tile's label history: undo and redo stacks over one classification,
//! and the optimistic object an edit produced before the configuration
//! reload carries it.
//!
//! Every verb works over [`History::current`]: the pending object while one
//! is waiting for its reload, else the configuration's. Two quick edits
//! before a reload therefore compose (the second builds on the first), and
//! undo replays row by row over what is current, skipping a row another
//! surface changed since (`classification::undo`).
//!
//! The pending object outlives a reload that is not its answer. Each edit
//! the tile queues is in flight until a reload carries it, and the shell
//! writes them in order, so a reload carrying an earlier one (the second
//! edit was queued after the first write fired), or still carrying the
//! object the edits started from (another classification or document
//! changed), is a step behind the pending object: dropping it then would
//! flash the later labels off, and the next edit, built over the reload,
//! would overwrite them. Any other object is someone else's write, and is
//! the truth now.
//!
//! A refused write leaves its rows as the configuration has them, so undo
//! later skips them like rows changed elsewhere; [`History::unsaved`] tells
//! the two apart, so the notice does not blame another surface for the
//! tile's own write that never landed.

use std::collections::BTreeSet;

use geode_core::classification::{self, UndoEntry};
use geode_core::dimensions::DerivedDimension;

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    pending: Option<DerivedDimension>,
    /// The configuration's object the in-flight edits were made over.
    base: Option<DerivedDimension>,
    /// Every object an edit produced since `base`, oldest first; the last
    /// is `pending`.
    in_flight: Vec<DerivedDimension>,
    /// Rows a refused write would have changed, until another
    /// classification is shown: an undo that skips one skips the tile's own
    /// unsaved edit, not another surface's.
    unsaved: BTreeSet<String>,
}

impl History {
    /// The object the tile shows and edits: the optimistic pending one if
    /// any, else the configuration's.
    pub fn current<'a>(&'a self, config: &'a DerivedDimension) -> &'a DerivedDimension {
        self.pending.as_ref().unwrap_or(config)
    }

    /// Give `sources` the label (`None` or blank clears) over the current
    /// object. Records the entry, clears redo, and holds the next object as
    /// pending; `None` when nothing changed, and then nothing is recorded.
    pub fn apply(
        &mut self,
        config: &DerivedDimension,
        sources: &[String],
        label: Option<&str>,
    ) -> Option<DerivedDimension> {
        let (next, entry) = classification::assign(self.current(config), sources, label);
        if entry.is_empty() {
            return None;
        }
        self.push(config, &next, entry);
        Some(next)
    }

    /// Record `next`, made over the current object by a whole plan (an
    /// import), as one entry: one undo step however many rows it changed.
    /// Bookkeeping as [`History::apply`]; an empty entry records nothing.
    pub fn record(&mut self, config: &DerivedDimension, next: DerivedDimension, entry: UndoEntry) {
        if entry.is_empty() {
            return;
        }
        self.push(config, &next, entry);
    }

    /// A new edit: its entry goes on the undo stack, redo is cleared (it
    /// was undone over an object this edit replaces), and `next` is held
    /// pending behind any edit already in flight.
    fn push(&mut self, config: &DerivedDimension, next: &DerivedDimension, entry: UndoEntry) {
        self.undo.push(entry);
        self.redo.clear();
        self.hold(config, next);
    }

    /// Revert the last entry over the current object: the next object and
    /// the rows skipped because they changed elsewhere since.
    pub fn undo(&mut self, config: &DerivedDimension) -> Option<(DerivedDimension, Vec<String>)> {
        let entry = self.undo.pop()?;
        let (next, skipped) = classification::undo(self.current(config), &entry);
        self.redo.push(entry);
        self.hold(config, &next);
        Some((next, skipped))
    }

    /// Re-apply the last undone entry over the current object.
    pub fn redo(&mut self, config: &DerivedDimension) -> Option<(DerivedDimension, Vec<String>)> {
        let entry = self.redo.pop()?;
        let (next, skipped) = classification::redo(self.current(config), &entry);
        self.undo.push(entry);
        self.hold(config, &next);
        Some((next, skipped))
    }

    /// Keep `next` as the pending object over `config`, in flight behind
    /// any earlier edit. A pending object already held keeps its base:
    /// `config` is still that base or one of the in-flight objects, or a
    /// reload would have dropped it.
    fn hold(&mut self, config: &DerivedDimension, next: &DerivedDimension) {
        if self.pending.is_none() {
            self.base = Some(config.clone());
            self.in_flight.clear();
        }
        self.in_flight.push(next.clone());
        self.pending = Some(next.clone());
    }

    /// A reload brought `config` for this classification. The pending
    /// object itself: every edit landed, and the copy goes. The base or an
    /// earlier in-flight object: the later edits are still on their way, so
    /// the copy stays, and the edits it carries are no longer in flight.
    /// Anything else is another surface's write: the copy goes.
    pub fn reloaded(&mut self, config: &DerivedDimension) {
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
    /// rows the in-flight edits changed are remembered as never saved.
    pub fn refused(&mut self) {
        if let (Some(pending), Some(base)) = (&self.pending, &self.base) {
            let changed = pending
                .values
                .keys()
                .chain(base.values.keys())
                .filter(|s| pending.values.get(*s) != base.values.get(*s));
            self.unsaved.extend(changed.cloned());
        }
        self.drop_pending();
    }

    /// How many of `skipped` are rows a refused write of this tile changed:
    /// left as they are because the edit was never saved, not because
    /// another surface changed them.
    pub fn unsaved(&self, skipped: &[String]) -> usize {
        skipped.iter().filter(|s| self.unsaved.contains(*s)).count()
    }

    fn drop_pending(&mut self) {
        self.pending = None;
        self.base = None;
        self.in_flight.clear();
    }

    /// Another classification is shown, or this one was renamed or deleted:
    /// its entries mean nothing any more.
    pub fn forget(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.unsaved.clear();
        self.drop_pending();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::classification::Change;
    use std::collections::BTreeMap;

    fn dim(pairs: &[(&str, &str)]) -> DerivedDimension {
        DerivedDimension {
            name: "region".into(),
            from: "underlying_ref".into(),
            values: pairs
                .iter()
                .map(|(s, l)| (s.to_string(), l.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn an_edit_is_visible_before_the_reload_and_dropped_after() {
        let cfg = dim(&[("A", "X")]);
        let mut h = History::default();
        let next = h.apply(&cfg, &["B".into()], Some("Y")).unwrap();
        assert_eq!(h.current(&cfg), &next);
        h.reloaded(&next);
        assert_eq!(h.current(&next), &next);
        assert!(h.pending.is_none(), "the reload carried it");
    }

    /// A reload that leaves this classification as it was (another
    /// classification or document changed) is not the one carrying the
    /// edit: the edit stays on screen, and the next edit builds on it.
    #[test]
    fn an_unrelated_reload_keeps_the_pending_edit() {
        let cfg = dim(&[]);
        let mut h = History::default();
        let first = h.apply(&cfg, &["A".into()], Some("X")).unwrap();
        h.reloaded(&cfg);
        assert_eq!(h.current(&cfg), &first);
        let second = h.apply(&cfg, &["B".into()], Some("Y")).unwrap();
        assert_eq!(second.values.get("A").map(String::as_str), Some("X"));
        assert_eq!(second.values.get("B").map(String::as_str), Some("Y"));
    }

    /// The second edit was queued after the first write fired: the reload
    /// carrying the first is a step behind, keeps both, and the third edit
    /// builds on both; the reload carrying the last drops the copy.
    #[test]
    fn a_reload_of_an_earlier_write_keeps_the_later_edits() {
        let cfg = dim(&[]);
        let mut h = History::default();
        let e1 = h.apply(&cfg, &["A".into()], Some("X")).unwrap();
        let e2 = h.apply(&cfg, &["B".into()], Some("Y")).unwrap();
        h.reloaded(&e1);
        assert_eq!(h.current(&e1), &e2, "E2 does not flash off");
        let e3 = h.apply(&e1, &["C".into()], Some("Z")).unwrap();
        assert_eq!(
            e3,
            dim(&[("A", "X"), ("B", "Y"), ("C", "Z")]),
            "E3 builds on E1 and E2"
        );
        // E1 is no longer in flight: its reload again is someone's revert.
        h.reloaded(&e2);
        assert_eq!(h.current(&e2), &e3);
        h.reloaded(&e3);
        assert_eq!(h.current(&e3), &e3);
        assert!(h.pending.is_none(), "every edit landed");
    }

    /// A reload that changed this classification otherwise (another tile
    /// wrote it) is the truth now: the optimistic copy goes.
    #[test]
    fn a_reload_changing_this_classification_otherwise_drops_the_pending_edit() {
        let cfg = dim(&[]);
        let mut h = History::default();
        h.apply(&cfg, &["A".into()], Some("X"));
        let foreign = dim(&[("C", "Z")]);
        h.reloaded(&foreign);
        assert_eq!(h.current(&foreign), &foreign);
    }

    #[test]
    fn a_refusal_drops_the_pending_edit() {
        let cfg = dim(&[]);
        let mut h = History::default();
        h.apply(&cfg, &["A".into()], Some("X"));
        h.refused();
        assert_eq!(h.current(&cfg), &cfg);
    }

    #[test]
    fn two_quick_edits_compose_and_undo_reverts_only_the_second() {
        let cfg = dim(&[]);
        let mut h = History::default();
        h.apply(&cfg, &["A".into()], Some("X"));
        h.apply(&cfg, &["B".into()], Some("Y")); // before any reload
        let (after, skipped) = h.undo(&cfg).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(after.values.get("A").map(String::as_str), Some("X"));
        assert!(!after.values.contains_key("B"));
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let cfg = dim(&[]);
        let mut h = History::default();
        h.apply(&cfg, &["A".into()], Some("X"));
        assert!(h.undo(&cfg).is_some());
        h.apply(&cfg, &["B".into()], Some("Y"));
        assert!(h.redo(&cfg).is_none());
    }

    #[test]
    fn an_edit_that_changes_nothing_records_nothing() {
        let cfg = dim(&[("A", "X")]);
        let mut h = History::default();
        assert!(h.apply(&cfg, &["A".into()], Some("X")).is_none());
        assert!(h.apply(&cfg, &["B".into()], None).is_none());
        assert!(h.undo(&cfg).is_none());
        assert_eq!(h.current(&cfg), &cfg, "no pending object either");
    }

    #[test]
    fn undo_after_a_foreign_change_skips_that_row() {
        let cfg = dim(&[]);
        let mut h = History::default();
        h.apply(&cfg, &["A".into()], Some("X"));
        h.refused();
        // Another tile relabelled A since.
        let cfg = dim(&[("A", "Z")]);
        let (after, skipped) = h.undo(&cfg).unwrap();
        assert_eq!(skipped, ["A"]);
        assert_eq!(after.values.get("A").map(String::as_str), Some("Z"));
    }

    /// Undo after a refusal skips the refused rows, and says they were
    /// the tile's own unsaved edit rather than another surface's change.
    #[test]
    fn undo_after_a_refusal_counts_the_refused_rows_as_unsaved() {
        let cfg = dim(&[("A", "X")]);
        let mut h = History::default();
        h.apply(&cfg, &["A".into(), "B".into()], Some("Y"));
        h.refused();
        let (_, skipped) = h.undo(&cfg).unwrap();
        assert_eq!(skipped, ["A", "B"]);
        assert_eq!(h.unsaved(&skipped), 2);
        // A row another surface changed is not the tile's unsaved edit.
        assert_eq!(h.unsaved(&["C".to_string()]), 0);
        h.forget();
        assert_eq!(h.unsaved(&skipped), 0);
    }

    fn change(source: &str, before: Option<&str>, after: Option<&str>) -> Change {
        Change {
            source: source.into(),
            before: before.map(str::to_string),
            after: after.map(str::to_string),
        }
    }

    #[test]
    fn a_recorded_plan_is_one_undo_step() {
        let cfg = dim(&[("A", "X")]);
        let mut h = History::default();
        let next = dim(&[("A", "Y"), ("B", "Z")]);
        let entry = UndoEntry {
            changes: vec![
                change("A", Some("X"), Some("Y")),
                change("B", None, Some("Z")),
            ],
        };
        h.record(&cfg, next.clone(), entry);
        assert_eq!(h.current(&cfg), &next);
        let (back, skipped) = h.undo(&cfg).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(back, cfg);
    }

    #[test]
    fn recording_an_empty_entry_records_nothing() {
        let cfg = dim(&[("A", "X")]);
        let mut h = History::default();
        h.record(&cfg, cfg.clone(), UndoEntry::default());
        assert!(h.undo(&cfg).is_none());
        assert!(h.pending.is_none(), "no pending object either");
    }

    /// A recorded plan is an edit like any other: it clears redo, and it is
    /// in flight behind an earlier edit, so the earlier write's reload keeps
    /// it on screen.
    #[test]
    fn a_recorded_plan_clears_redo_and_is_in_flight() {
        let cfg = dim(&[]);
        let mut h = History::default();
        let e1 = h.apply(&cfg, &["A".into()], Some("X")).unwrap();
        let next = dim(&[("A", "X"), ("B", "Y")]);
        let entry = || UndoEntry {
            changes: vec![change("B", None, Some("Y"))],
        };
        h.record(&cfg, next.clone(), entry());
        h.reloaded(&e1);
        assert_eq!(h.current(&e1), &next, "the plan does not flash off");
        assert!(h.undo(&e1).is_some());
        h.record(&e1, next.clone(), entry());
        assert!(h.redo(&e1).is_none(), "a recorded plan clears redo");
    }

    #[test]
    fn redo_reapplies_and_forget_clears_everything() {
        let cfg = dim(&[]);
        let mut h = History::default();
        h.apply(&cfg, &["A".into()], Some("X"));
        let cfg = dim(&[("A", "X")]);
        h.reloaded(&cfg);
        let (undone, _) = h.undo(&cfg).unwrap();
        assert!(undone.values.is_empty());
        let (redone, skipped) = h.redo(&cfg).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(redone.values.get("A").map(String::as_str), Some("X"));
        h.forget();
        assert!(h.undo(&cfg).is_none());
        assert_eq!(h.current(&cfg), &cfg);
    }
}
