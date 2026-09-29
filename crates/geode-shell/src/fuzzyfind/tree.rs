//! Immutable topology and background ordering for hierarchy-preserving search.
use super::Rows;
use std::{
    collections::HashSet,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

const NONE: u32 = u32::MAX;
type RankedTree = (Arc<Vec<usize>>, Arc<[bool]>);
#[derive(Clone, Copy)]
struct Node {
    parent: u32,
    end: u32,
    depth: u32,
}

/// Topology for source rows in depth-first order. Row indices are local to the
/// search dataset; ancestors must precede their descendants.
pub struct FindTree {
    nodes: OnceLock<Vec<Node>>,
    len: usize,
    source: Option<Source>,
}

struct Source {
    depth: Box<dyn Fn(usize) -> usize + Send + Sync>,
    branch: Box<dyn Fn(usize) -> bool + Send + Sync>,
}

impl FindTree {
    pub fn from_depths(depths: impl IntoIterator<Item = usize>) -> Self {
        let mut nodes: Vec<Node> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for depth in depths {
            let row = nodes.len();
            assert!(row < NONE as usize, "too many search rows");
            while stack
                .last()
                .is_some_and(|&p| nodes[p].depth as usize >= depth)
            {
                nodes[stack.pop().unwrap()].end = row as u32;
            }
            nodes.push(Node {
                parent: stack.last().map_or(NONE, |&p| p as u32),
                end: NONE,
                depth: depth as u32,
            });
            stack.push(row);
        }
        let end = nodes.len() as u32;
        for row in stack {
            nodes[row].end = end;
        }
        Self {
            len: nodes.len(),
            nodes: OnceLock::from(nodes),
            source: None,
        }
    }

    /// Share an existing tree's depth and branch lookup for immediate display.
    /// Call `prepare` on the indexing worker to cache topology before ranking.
    pub fn deferred(
        len: usize,
        depth: impl Fn(usize) -> usize + Send + Sync + 'static,
        branch: impl Fn(usize) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            nodes: OnceLock::new(),
            len,
            source: Some(Source {
                depth: Box::new(depth),
                branch: Box::new(branch),
            }),
        }
    }

    pub fn prepare(&self) {
        self.nodes();
    }

    fn nodes(&self) -> &[Node] {
        self.nodes.get_or_init(|| {
            let source = self.source.as_ref().unwrap();
            Self::from_depths((0..self.len).map(|row| (source.depth)(row)))
                .nodes
                .into_inner()
                .unwrap()
        })
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }
    pub(super) fn branch(&self, row: usize) -> bool {
        match self.nodes.get() {
            Some(nodes) => nodes[row].end as usize > row + 1,
            None => (self.source.as_ref().unwrap().branch)(row),
        }
    }

    /// Propagate each direct match's rank to its ancestors. Each retained node
    /// is discovered once; link siblings by their best descendant, then emit DFS.
    pub(super) fn rank(
        &self,
        matches: &[usize],
        current: &AtomicU64,
        revision: u64,
    ) -> Option<RankedTree> {
        let nodes = self.nodes();
        let mut seen = vec![false; self.len()];
        let mut hits = vec![false; self.len()];
        let mut included = Vec::new();
        for &row in matches {
            if current.load(Ordering::Relaxed) != revision {
                return None;
            }
            hits[row] = true;
            let mut at = row as u32;
            while at != NONE && !seen[at as usize] {
                seen[at as usize] = true;
                included.push(at);
                at = nodes[at as usize].parent;
            }
        }
        // Discovery is already in best-match order. Link siblings in that order
        // without sorting all matches or hashing each leaf during traversal.
        let mut first = vec![NONE; self.len()];
        let mut next = vec![NONE; self.len()];
        let mut root = NONE;
        for &row in included.iter().rev() {
            if current.load(Ordering::Relaxed) != revision {
                return None;
            }
            let parent = nodes[row as usize].parent;
            let head = if parent == NONE {
                &mut root
            } else {
                &mut first[parent as usize]
            };
            next[row as usize] = *head;
            *head = row;
        }
        let mut order = Vec::with_capacity(included.len());
        let mut stack = vec![root];
        while let Some(row) = stack.pop() {
            if current.load(Ordering::Relaxed) != revision {
                return None;
            }
            if row == NONE {
                continue;
            }
            let row = row as usize;
            order.push(row);
            if next[row] != NONE {
                stack.push(next[row]);
            }
            if first[row] != NONE {
                stack.push(first[row]);
            }
        }
        Some((Arc::new(order), hits.into()))
    }

    pub(super) fn visible(
        &self,
        order: &Rows,
        folded: &HashSet<usize>,
        current: &AtomicU64,
        revision: u64,
    ) -> Option<Rows> {
        if folded.is_empty() {
            return Some(order.clone());
        }
        let nodes = self.nodes();
        let mut out = Vec::with_capacity(order.len());
        let mut hidden = 0..0;
        for row in order.iter() {
            if current.load(Ordering::Relaxed) != revision {
                return None;
            }
            if hidden.contains(&row) {
                continue;
            }
            out.push(row);
            hidden = if folded.contains(&row) {
                row + 1..nodes[row].end as usize
            } else {
                0..0
            };
        }
        Some(Rows::Ranked(Arc::new(out)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fzf_deferred_topology_does_not_walk_depths_for_visible_chevrons() {
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = reads.clone();
        let tree = FindTree::deferred(
            3,
            move |row| {
                counter.fetch_add(1, Ordering::Relaxed);
                usize::from(row != 0)
            },
            |row| row == 0,
        );
        assert!(tree.branch(0));
        assert!(!tree.branch(1));
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        tree.prepare();
        tree.prepare();
        assert_eq!(reads.load(Ordering::Relaxed), 3);
        let (order, _) = tree.rank(&[2, 1], &AtomicU64::new(1), 1).unwrap();
        assert_eq!(*order, vec![0, 2, 1]);
        assert!(tree.branch(0));
        assert!(!tree.branch(1));
    }

    #[test]
    fn fzf_ranks_branches_by_best_match_and_keeps_each_ancestor_once() {
        let tree = FindTree::from_depths([0, 1, 2, 3, 3, 1, 2, 3, 3, 2, 3]);
        let current = AtomicU64::new(1);
        let (order, hits) = tree.rank(&[7, 3, 8], &current, 1).unwrap();
        assert_eq!(*order, vec![0, 5, 6, 7, 8, 1, 2, 3]);
        assert_eq!(
            hits.iter()
                .enumerate()
                .filter_map(|(r, &hit)| hit.then_some(r))
                .collect::<Vec<_>>(),
            vec![3, 7, 8]
        );
        let order = Rows::Ranked(order);
        assert_eq!(
            tree.visible(&order, &HashSet::from([5]), &current, 1)
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            vec![0, 5, 1, 2, 3]
        );
        assert_eq!(
            tree.visible(&order, &HashSet::from([5, 2]), &current, 1)
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            vec![0, 5, 1, 2]
        );
        assert_eq!(
            tree.visible(&order, &HashSet::from([0]), &current, 1)
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            tree.visible(&order, &HashSet::new(), &current, 1)
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            order.iter().collect::<Vec<_>>()
        );
        assert!(tree.rank(&[], &current, 1).unwrap().0.is_empty());
        current.store(2, Ordering::Relaxed);
        assert!(tree.rank(&[7], &current, 1).is_none());
        assert!(
            tree.visible(&order, &HashSet::from([0]), &current, 1)
                .is_none()
        );
    }
}
