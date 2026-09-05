//! Which nodes are open (Phase 3 spec §6.1): a set of *paths* — the
//! grouping values from the root to the node — never row indices, so
//! expansion survives a requery, a regroup, and a snapshot that
//! reorders siblings. Built at keypress time; never touched per frame.

use crate::core::plan::ColumnPlan;
use geode_core::snapshot::Snapshot;
use std::collections::HashSet;

/// Grouping values root → node. `None` is NULL, which is its own value
/// (a blanked ENUM, P2 §3.6), distinct from the empty string.
pub type Path = Vec<Option<String>>;

#[derive(Debug, Default, Clone)]
pub struct Expansion {
    open: HashSet<Path>,
    /// Consulted only while `all` is set: nodes `zc`'d shut after `zR`.
    /// Vim closes just the named fold and leaves the rest of `zR` open
    /// (spec §6.1), so "all open" needs its own carve-out set rather
    /// than degrading to "all closed" the moment one node is closed.
    closed: HashSet<Path>,
    /// `zR`: every materialised node is open until `close_all`, except
    /// what `closed` names.
    all: bool,
}

impl Expansion {
    pub fn is_open(&self, path: &[Option<String>]) -> bool {
        if self.all {
            !self.closed.contains(path)
        } else {
            self.open.contains(path)
        }
    }

    pub fn open(&mut self, path: Path) -> bool {
        if self.all {
            self.closed.remove(&path)
        } else {
            self.open.insert(path)
        }
    }

    pub fn close(&mut self, path: &[Option<String>]) -> bool {
        if self.all {
            self.closed.insert(path.to_vec())
        } else {
            self.open.remove(path)
        }
    }

    /// `true` when the node is open afterwards.
    pub fn toggle(&mut self, path: Path) -> bool {
        if self.is_open(&path) {
            self.close(&path);
            false
        } else {
            self.open(path);
            true
        }
    }

    pub fn open_all(&mut self) {
        self.all = true;
        self.open.clear();
        self.closed.clear();
    }

    pub fn close_all(&mut self) {
        self.all = false;
        self.open.clear();
        self.closed.clear();
    }

    /// The deepest open node's depth; `usize::MAX` under `open_all`.
    pub fn deepest_open_depth(&self) -> usize {
        if self.all {
            return usize::MAX;
        }
        self.open.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// After a regroup, paths deeper than the new grouping cannot exist.
    pub fn prune_to(&mut self, grouping_len: usize) {
        self.open.retain(|p| p.len() <= grouping_len);
        self.closed.retain(|p| p.len() <= grouping_len);
    }
}

/// The path of `row`: its ancestors' tree texts, root excluded.
pub fn path_of(snapshot: &Snapshot, plan: &ColumnPlan, row: usize) -> Path {
    let tree = snapshot.tree();
    let depth = tree.depth(row);
    let mut path: Path = vec![None; depth];
    let mut at = Some(row);
    let mut d = depth;
    while let (Some(r), true) = (at, d > 0) {
        path[d - 1] = plan.tree_text(snapshot, r).map(str::to_string);
        at = tree.parent(r);
        d -= 1;
    }
    path
}

/// One more than the deepest open node, so a single expand is already in
/// hand; never past the grouping (§6.1, `docs/perf.md`).
pub fn depth_bound(expansion: &Expansion, grouping_len: usize) -> usize {
    expansion
        .deepest_open_depth()
        .saturating_add(1)
        .min(grouping_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(parts: &[Option<&str>]) -> Path {
        parts.iter().map(|s| s.map(str::to_string)).collect()
    }

    #[test]
    fn open_close_toggle_and_deepest() {
        let mut e = Expansion::default();
        assert_eq!(e.deepest_open_depth(), 0);
        assert!(e.open(p(&[Some("L1")])));
        assert!(!e.open(p(&[Some("L1")])), "already open");
        assert!(e.is_open(&p(&[Some("L1")])));
        assert!(e.toggle(p(&[Some("L1"), Some("SPX")])));
        assert_eq!(e.deepest_open_depth(), 2);
        assert!(!e.toggle(p(&[Some("L1"), Some("SPX")])));
        assert!(!e.is_open(&p(&[Some("L1"), Some("SPX")])));
        assert!(e.close(&p(&[Some("L1")])));
        assert!(!e.close(&p(&[Some("L1")])));
        assert_eq!(e.deepest_open_depth(), 0);
    }

    #[test]
    fn a_null_and_an_empty_string_are_different_paths() {
        let mut e = Expansion::default();
        e.open(p(&[None]));
        assert!(!e.is_open(&p(&[Some("")])));
    }

    #[test]
    fn open_all_opens_every_node_until_close_all() {
        let mut e = Expansion::default();
        e.open_all();
        assert!(e.is_open(&p(&[Some("anything"), Some("at"), Some("all")])));
        assert_eq!(e.deepest_open_depth(), usize::MAX);
        e.close_all();
        assert!(!e.is_open(&p(&[Some("anything")])));
        assert_eq!(e.deepest_open_depth(), 0);
    }

    #[test]
    fn close_under_open_all_closes_only_that_node() {
        // vim's own behaviour: `zR` then `zc` on one fold closes just
        // that fold, not every fold `zR` opened (spec §6.1).
        let mut e = Expansion::default();
        e.open_all();
        assert!(e.close(&p(&[Some("L1")])));
        assert!(!e.is_open(&p(&[Some("L1")])));
        assert!(e.is_open(&p(&[Some("L2")])), "a sibling stays open");
        assert!(!e.close(&p(&[Some("L1")])), "already closed");
        assert!(e.open(p(&[Some("L1")])), "reopens under all");
        assert!(e.is_open(&p(&[Some("L1")])));
        e.close_all();
        assert!(!e.is_open(&p(&[Some("L1")])));
        assert!(!e.is_open(&p(&[Some("L2")])));
    }

    #[test]
    fn the_depth_bound_is_one_past_the_deepest_open_node_capped_at_the_grouping() {
        let mut e = Expansion::default();
        assert_eq!(depth_bound(&e, 3), 1, "collapsed: the first level only");
        e.open(p(&[Some("L1")]));
        assert_eq!(depth_bound(&e, 3), 2);
        e.open(p(&[Some("L1"), Some("SPX")]));
        assert_eq!(depth_bound(&e, 3), 3);
        e.open_all();
        assert_eq!(depth_bound(&e, 3), 3, "never past the grouping");
        assert_eq!(depth_bound(&Expansion::default(), 0), 0, "a flat view");
    }

    #[test]
    fn regrouping_prunes_paths_deeper_than_the_new_grouping() {
        let mut e = Expansion::default();
        e.open(p(&[Some("L1")]));
        e.open(p(&[Some("L1"), Some("SPX")]));
        e.prune_to(1);
        assert!(e.is_open(&p(&[Some("L1")])));
        assert!(!e.is_open(&p(&[Some("L1"), Some("SPX")])));
    }
}
