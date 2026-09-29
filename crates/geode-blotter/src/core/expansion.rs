//! Snapshot-reading helpers over the path-keyed `Expansion` in
//! `geode_core::expansion`: the path of a snapshot row and the query depth an
//! expansion needs.

use crate::core::plan::ColumnPlan;
use geode_core::expansion::{Expansion, Path};
use geode_core::snapshot::Snapshot;

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

/// The query depth needed to show children of the deepest open node, capped
/// at the grouping depth. With all nodes closed, request the first level;
/// with `open_all`, request every grouping level.
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
}
