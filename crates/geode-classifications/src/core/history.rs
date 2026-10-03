//! The tile's label history: undo and redo stacks over one classification,
//! and the optimistic object an edit produced before the configuration
//! reload carries it.
//!
//! Every verb works over [`History::current`]: the pending object while one
//! is waiting for its reload, else the configuration's. Two quick edits
//! before a reload therefore compose (the second builds on the first), and
//! undo replays row by row over what is current, skipping a row another
//! surface changed since (`classification::undo`).

use geode_core::classification::{self, UndoEntry};
use geode_core::dimensions::DerivedDimension;

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    pending: Option<DerivedDimension>,
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
        self.pending = Some(next.clone());
        Some(next)
    }

    /// Revert the last entry over the current object: the next object and
    /// the rows skipped because they changed elsewhere since.
    pub fn undo(&mut self, config: &DerivedDimension) -> Option<(DerivedDimension, Vec<String>)> {
        let entry = self.undo.pop()?;
        let (next, skipped) = classification::undo(self.current(config), &entry);
        self.redo.push(entry);
        self.pending = Some(next.clone());
        Some((next, skipped))
    }

    /// Re-apply the last undone entry over the current object.
    pub fn redo(&mut self, config: &DerivedDimension) -> Option<(DerivedDimension, Vec<String>)> {
        let entry = self.redo.pop()?;
        let (next, skipped) = classification::redo(self.current(config), &entry);
        self.undo.push(entry);
        self.pending = Some(next.clone());
        Some((next, skipped))
    }

    /// The configuration now carries the pending object, or the write was
    /// refused and the configuration's object stands: drop the optimistic
    /// copy either way.
    pub fn reloaded(&mut self) {
        self.pending = None;
    }

    /// Another classification is shown, or this one was renamed or deleted:
    /// its entries mean nothing any more.
    pub fn forget(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.pending = None;
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
        h.reloaded();
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
        h.reloaded();
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
        h.reloaded();
        let cfg = dim(&[("A", "X")]);
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
