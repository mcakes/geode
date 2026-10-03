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
//! The pending object outlives a reload that leaves this classification as
//! it was (another classification or document changed inside the write's
//! debounce): dropping it then would flash the old labels back and build
//! the next edit over a stale base.

use geode_core::classification::{self, UndoEntry};
use geode_core::dimensions::DerivedDimension;

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    pending: Option<DerivedDimension>,
    /// The configuration's object the pending one was made over: a reload
    /// still carrying exactly this has not answered the write yet.
    base: Option<DerivedDimension>,
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
        self.undo.push(entry);
        self.redo.clear();
        self.hold(config, &next);
        Some(next)
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

    /// Keep `next` as the pending object over `config`. A pending object
    /// already held keeps its base: `config` is still that base, or a
    /// reload would have dropped it.
    fn hold(&mut self, config: &DerivedDimension, next: &DerivedDimension) {
        if self.pending.is_none() {
            self.base = Some(config.clone());
        }
        self.pending = Some(next.clone());
    }

    /// A reload brought `config` for this classification. It carries the
    /// pending object, or something else changed this classification (the
    /// configuration's word is the truth now): the optimistic copy goes. A
    /// reload leaving the classification as the pending object found it
    /// has not answered the write yet, and the copy stays.
    pub fn reloaded(&mut self, config: &DerivedDimension) {
        if self.base.as_ref() != Some(config) {
            self.pending = None;
            self.base = None;
        }
    }

    /// The write was refused and the configuration's object stands.
    pub fn refused(&mut self) {
        self.pending = None;
        self.base = None;
    }

    /// Another classification is shown, or this one was renamed or deleted:
    /// its entries mean nothing any more.
    pub fn forget(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.pending = None;
        self.base = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
