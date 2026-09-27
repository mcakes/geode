/// Stable identity of a tile (one module instance in one pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId(pub u64);

/// Split orientation. `Horizontal`: children laid out left→right.
/// `Vertical`: children laid out top→bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn orientation(self) -> Orientation {
        match self {
            Direction::Left | Direction::Right => Orientation::Horizontal,
            Direction::Up | Direction::Down => Orientation::Vertical,
        }
    }
}

/// A rectangle in the caller's coordinate space. Navigation uses unit
/// bounds; rendering supplies pixel bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const UNIT: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    pub(crate) fn right(&self) -> f32 {
        self.x + self.w
    }

    pub(crate) fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Edge-adjacency tolerance for unit-space geometry comparisons.
/// MIN_RATIO bounds ratios, not absolute size, so deeply nested layouts can in principle produce tiles thinner than EPS whose adjacency checks then fail; unreachable in realistic layouts (measured clean at <= 12 tiles), revisit if tile counts grow far beyond that.
pub(crate) const EPS: f32 = 1e-3;

/// Smallest fraction any split child may occupy.
pub const MIN_RATIO: f32 = 0.05;

/// Structural address of a split divider: child indices from the root to
/// its owning split, then the index of the left/top child in the adjacent pair.
/// A drag captures this value and revalidates it on every application.
///
/// This is not a node identity or generation. A structural edit can leave a
/// valid address pointing at a different boundary; that drag may resize the
/// new pair. Callers must cancel gestures when their workspace or layout
/// context is no longer applicable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DividerAddress {
    pub path: Vec<usize>,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(TileId),
    Split {
        orientation: Orientation,
        children: Vec<Node>,
        /// Fractions of this split's extent, same length as `children`,
        /// summing to 1.0.
        ratios: Vec<f32>,
    },
    /// A single layout slot with multiple retained tiles and one active member.
    /// Members are IDs, not nested nodes. Valid stacks have at least two
    /// members and `active < children.len()`; layout emits only the active ID.
    Stack {
        children: Vec<TileId>,
        active: usize,
    },
}

fn v_overlap(a: &Rect, b: &Rect) -> f32 {
    (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0.0)
}

fn h_overlap(a: &Rect, b: &Rect) -> f32 {
    (a.right().min(b.right()) - a.x.max(b.x)).max(0.0)
}

fn swap_leaves(node: &mut Node, a: TileId, b: TileId) {
    match node {
        Node::Leaf(id) => {
            if *id == a {
                *id = b;
            } else if *id == b {
                *id = a;
            }
        }
        Node::Stack { children, .. } => {
            for c in children {
                if *c == a {
                    *c = b;
                } else if *c == b {
                    *c = a;
                }
            }
        }
        Node::Split { children, .. } => {
            for child in children {
                swap_leaves(child, a, b);
            }
        }
    }
}

/// Does this node, without recursing into a split, hold `id` — a leaf of
/// that id, or a stack with `id` among its members? The one test every
/// structural verb (`insert_beside`, `toggle_split_orientation`) makes
/// when it asks "which child is the focused tile's slot".
fn node_holds(node: &Node, id: TileId) -> bool {
    match node {
        Node::Leaf(leaf) => *leaf == id,
        Node::Stack { children, .. } => children.contains(&id),
        Node::Split { .. } => false,
    }
}

/// Check leaf and stack membership recursively without allocating. Used by
/// per-frame stack lookup as well as tree mutation guards.
fn holds_anywhere(node: &Node, id: TileId) -> bool {
    match node {
        Node::Leaf(_) | Node::Stack { .. } => node_holds(node, id),
        Node::Split { children, .. } => children.iter().any(|c| holds_anywhere(c, id)),
    }
}

/// The stack node holding `id` as a member, if any.
fn find_stack_mut(node: &mut Node, id: TileId) -> Option<&mut Node> {
    match node {
        Node::Leaf(_) => None,
        Node::Stack { children, .. } if children.contains(&id) => Some(node),
        Node::Stack { .. } => None,
        Node::Split { children, .. } => children.iter_mut().find_map(|c| find_stack_mut(c, id)),
    }
}

fn find_stack(node: &Node, id: TileId) -> Option<&Node> {
    match node {
        Node::Leaf(_) => None,
        Node::Stack { children, .. } if children.contains(&id) => Some(node),
        Node::Stack { .. } => None,
        Node::Split { children, .. } => children.iter().find_map(|c| find_stack(c, id)),
    }
}

fn collect_visible(node: &Node, out: &mut Vec<TileId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Stack { children, active } => out.push(children[*active]),
        Node::Split { children, .. } => {
            for child in children {
                collect_visible(child, out);
            }
        }
    }
}

/// A main or dock split tree with structural focus and optional fullscreen.
/// Pure mutations and [`Tree::layout`] share one geometry model. Equality
/// includes all state, allowing session serialization to omit default docks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tree {
    root: Option<Node>,
    focused: Option<TileId>,
    fullscreen: Option<TileId>,
}

impl Tree {
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn focused(&self) -> Option<TileId> {
        self.focused
    }

    pub fn fullscreen(&self) -> Option<TileId> {
        self.fullscreen
    }

    pub fn root(&self) -> Option<&Node> {
        self.root.as_ref()
    }

    /// All tiles, in tree order (left→right, top→bottom).
    pub fn tiles(&self) -> Vec<TileId> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            collect_leaves(root, &mut out);
        }
        out
    }

    /// Visible leaves and active stack members in tree order. `tiles()` also
    /// includes hidden members for retention and persistence.
    pub fn visible_tiles(&self) -> Vec<TileId> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            collect_visible(root, &mut out);
        }
        out
    }

    /// `(one-based index, member count)` when `id` is a stack member —
    /// what the marker chip paints — else `None`.
    pub fn stack_position(&self, id: TileId) -> Option<(usize, usize)> {
        let Node::Stack { children, .. } = find_stack(self.root.as_ref()?, id)? else {
            return None;
        };
        let ix = children.iter().position(|c| *c == id)?;
        Some((ix + 1, children.len()))
    }

    /// All members of `id`'s stack in order, or `None` for a plain or absent tile.
    pub fn stack_members(&self, id: TileId) -> Option<Vec<TileId>> {
        let Node::Stack { children, .. } = find_stack(self.root.as_ref()?, id)? else {
            return None;
        };
        Some(children.clone())
    }

    /// Activate `id` within its stack. If the outgoing active member was
    /// fullscreen, transfer fullscreen to `id`. A plain leaf needs no change.
    fn activate(&mut self, id: TileId) {
        let Some(root) = self.root.as_mut() else {
            return;
        };
        let Some(Node::Stack { children, active }) = find_stack_mut(root, id) else {
            return;
        };
        let Some(ix) = children.iter().position(|c| *c == id) else {
            return;
        };
        let outgoing = children[*active];
        *active = ix;
        if self.fullscreen == Some(outgoing) {
            self.fullscreen = Some(id);
        }
    }

    /// The one door every focus assignment goes through: a focused
    /// member is always its stack's active member, so focusing activates.
    fn set_focus(&mut self, id: TileId) {
        self.activate(id);
        self.focused = Some(id);
    }

    /// Insert `new` after `anchor`, creating a stack if the anchor is a leaf.
    /// Activate and focus `new`. Refuse an absent anchor, an existing `new`,
    /// or identical IDs without changing the tree.
    pub(crate) fn stack_after(&mut self, anchor: TileId, new: TileId) -> bool {
        if anchor == new || !self.contains(anchor) || self.contains(new) {
            return false;
        }
        fn insert(node: &mut Node, anchor: TileId, new: TileId) -> bool {
            match node {
                Node::Leaf(id) if *id == anchor => {
                    *node = Node::Stack {
                        children: vec![anchor, new],
                        active: 1,
                    };
                    true
                }
                Node::Leaf(_) => false,
                Node::Stack { children, active } => {
                    match children.iter().position(|c| *c == anchor) {
                        Some(ix) => {
                            children.insert(ix + 1, new);
                            *active = ix + 1;
                            true
                        }
                        None => false,
                    }
                }
                Node::Split { children, .. } => children.iter_mut().any(|c| insert(c, anchor, new)),
            }
        }
        let root = self.root.as_mut().expect("contains(anchor) implies a root");
        insert(root, anchor, new);
        // Same rule as `split`/`pop_out`: an explicit layout operation
        // trumps a stale fullscreen. Without this, `insert`'s own `active`
        // write already points at `new` by the time `set_focus` calls
        // `activate`, so `activate`'s outgoing-member check never fires
        // and a fullscreen held by the stack's old active member survives
        // hidden — `Tree::layout` paints it (any tile `contains(fs)`).
        self.fullscreen = None;
        self.set_focus(new);
        true
    }

    /// Cycle the focused stack member by `delta`, wrapping at either end.
    /// Return false without mutation if focus is not a stack member.
    pub fn stack_step(&mut self, delta: i64) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        let Some(Node::Stack { children, active }) = find_stack_mut(root, focused) else {
            return false;
        };
        let len = children.len() as i64;
        let next = (*active as i64 + delta).rem_euclid(len) as usize;
        let id = children[next];
        self.set_focus(id);
        true
    }

    /// Remove the focused stack member and split it beside the remaining
    /// stack: right/bottom when `after`, left/top otherwise. A one-member
    /// remainder becomes a leaf. Return false for a plain tile.
    fn pop_out(&mut self, orientation: Orientation, after: bool) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        let survivor = {
            let Some(root) = self.root.as_ref() else {
                return false;
            };
            let Some(Node::Stack { children, .. }) = find_stack(root, focused) else {
                return false;
            };
            // Any other member names the stack for `insert_beside`, which
            // asks `node_holds`; the collapsed-to-leaf case is that one
            // member itself.
            *children
                .iter()
                .find(|c| **c != focused)
                .expect("a stack has two members")
        };
        let mut done = false;
        let root = self
            .root
            .take()
            .and_then(|n| remove_leaf(n, focused, &mut done));
        let root = root.expect("removing one member of a stack never empties the tree");
        self.root = Some(insert_beside(root, survivor, focused, orientation, after));
        // Same rule as `split`: an explicit layout operation trumps a
        // stale fullscreen.
        self.fullscreen = None;
        self.set_focus(focused);
        true
    }

    /// Pop the focused stack member after its stack in `orientation`.
    /// Return false for a plain tile.
    pub fn unstack_focused(&mut self, orientation: Orientation) -> bool {
        self.pop_out(orientation, true)
    }

    /// Turn the focused member's stack into a split of `orientation` in the
    /// stack's own slot: members in order, equal ratios, focus unchanged.
    /// Clears fullscreen on success. Return false without mutation for a
    /// plain tile or an empty tree.
    pub fn split_stack(&mut self, orientation: Orientation) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        let Some(node) = self
            .root
            .as_mut()
            .and_then(|root| find_stack_mut(root, focused))
        else {
            return false;
        };
        let Node::Stack { children, .. } = node else {
            unreachable!("find_stack_mut returns a stack");
        };
        let share = 1.0 / children.len() as f32;
        *node = Node::Split {
            orientation,
            ratios: vec![share; children.len()],
            children: children.iter().map(|id| Node::Leaf(*id)).collect(),
        };
        // Same rule as `split`/`pop_out`/`stack_after`: an explicit layout
        // operation trumps a stale fullscreen.
        self.fullscreen = None;
        self.set_focus(focused);
        true
    }

    /// Pull the visible tile beside focus in `dir` into the focused tile's
    /// slot, making a stack from a plain tile. Focus stays on the focused
    /// tile, which stays active. Stack order follows the screen: a tile from
    /// the left or above goes first, one from the right or below goes last,
    /// so repeated pulls sweep a row in reading order either way. A stacked
    /// neighbour gives up only its visible member. Return false without
    /// mutation when there is no neighbour that way (an edge, a lone tile,
    /// or a fullscreen tile, which has no visible neighbour).
    pub fn pull(&mut self, dir: Direction) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        let Some(pulled) = self.neighbor(dir) else {
            return false;
        };
        let mut done = false;
        let root = self
            .root
            .take()
            .and_then(|n| remove_leaf(n, pulled, &mut done))
            .expect("the focused tile survives removing its neighbour");
        let first = matches!(dir, Direction::Left | Direction::Up);
        fn insert(node: &mut Node, focused: TileId, pulled: TileId, first: bool) -> bool {
            match node {
                Node::Leaf(id) if *id == focused => {
                    let children = if first {
                        vec![pulled, focused]
                    } else {
                        vec![focused, pulled]
                    };
                    // `set_focus` below makes `focused` active.
                    *node = Node::Stack {
                        children,
                        active: 0,
                    };
                    true
                }
                Node::Leaf(_) => false,
                Node::Stack { children, .. } => {
                    if !children.contains(&focused) {
                        return false;
                    }
                    if first {
                        children.insert(0, pulled);
                    } else {
                        children.push(pulled);
                    }
                    true
                }
                Node::Split { children, .. } => children
                    .iter_mut()
                    .any(|c| insert(c, focused, pulled, first)),
            }
        }
        let mut root = root;
        insert(&mut root, focused, pulled, first);
        self.root = Some(root);
        self.set_focus(focused);
        true
    }

    pub fn contains(&self, id: TileId) -> bool {
        self.root
            .as_ref()
            .is_some_and(|root| holds_anywhere(root, id))
    }

    /// Set focus to an existing tile (used by click-focus in 1b-ui).
    pub fn focus(&mut self, id: TileId) -> bool {
        if self.contains(id) {
            self.set_focus(id);
            true
        } else {
            false
        }
    }

    /// Insert `new` beside focus, or create the first leaf in an empty tree.
    /// Use the first leaf when a nonempty tree has no focus, so a tile moved
    /// from another tree still has an insertion point. Matching-orientation
    /// siblings receive equal ratios; otherwise the anchor is wrapped in a
    /// half-and-half split. Focus moves to `new` and a nonempty tree exits
    /// fullscreen. The caller must supply an ID not already in this tree.
    pub fn split(&mut self, new: TileId, orientation: Orientation) {
        match (self.root.take(), self.focused) {
            (None, _) => {
                self.root = Some(Node::Leaf(new));
            }
            (Some(root), Some(focused)) => {
                self.fullscreen = None;
                // Share the same insertion rule as ID-addressed edge drops.
                self.root = Some(insert_beside(root, focused, new, orientation, true));
            }
            // A nonempty tree without focus still needs an insertion point.
            // Use the first leaf, preserving both the existing tiles and the new
            // ID. Normal actions and session restoration maintain focus already.
            (Some(root), None) => {
                let mut leaves = Vec::new();
                collect_leaves(&root, &mut leaves);
                match leaves.first().copied() {
                    Some(anchor) => {
                        self.fullscreen = None;
                        self.root = Some(insert_beside(root, anchor, new, orientation, true));
                    }
                    // Unreachable (every constructible root bottoms out in
                    // at least one leaf), but even then the id survives.
                    None => self.root = Some(Node::Leaf(new)),
                }
            }
        }
        self.set_focus(new);
    }

    /// Close focus, collapsing single-child containers and renormalizing
    /// split ratios. Prefer the surviving stack's active member for focus;
    /// otherwise choose the next tree-order tile, or the previous tile when
    /// closing the last. An emptied tree has no focus.
    pub fn close(&mut self) {
        self.remove_focused();
    }

    /// Remove and return the focused tile. Refocus the stack's new active
    /// member when possible; otherwise use the surviving tree-order neighbor.
    /// Clear fullscreen when removing its tile. Return `None` without focus;
    /// a tree emptied by removal also clears focus.
    pub fn remove_focused(&mut self) -> Option<TileId> {
        let focused = self.focused?;
        if self.fullscreen == Some(focused) {
            self.fullscreen = None;
        }

        // Find the position of the focused tile in the pre-close leaf order.
        let pre_close_tiles = self.tiles();
        let k = pre_close_tiles
            .iter()
            .position(|&id| id == focused)
            .unwrap_or(0);

        // A stack member other than the one being closed — its own stack
        // refocus rule (below) takes priority over the tree-order-neighbor
        // rule once the closed tile leaves a live sibling behind.
        let sibling = self
            .root
            .as_ref()
            .and_then(|r| find_stack(r, focused))
            .and_then(|n| match n {
                Node::Stack { children, .. } => children.iter().copied().find(|c| *c != focused),
                _ => None,
            });

        // Remove the leaf from the tree (exactly one — see `remove_leaf`).
        let mut done = false;
        self.root = self
            .root
            .take()
            .and_then(|n| remove_leaf(n, focused, &mut done));

        // Keep focus in the surviving stack after closing a member. Other
        // closes use the tree-order neighbor.
        let next = match sibling {
            Some(s) if self.contains(s) => {
                let root = self.root.as_ref().expect("contains(s) implies a root");
                match find_stack(root, s) {
                    Some(Node::Stack { children, active }) => Some(children[*active]),
                    _ => Some(s),
                }
            }
            _ => {
                let post_close_tiles = self.tiles();
                let focus_index = std::cmp::min(k, post_close_tiles.len().saturating_sub(1));
                post_close_tiles.get(focus_index).copied()
            }
        };
        match next {
            Some(id) => self.set_focus(id),
            None => self.focused = None,
        }
        Some(focused)
    }

    /// Remove one occurrence of `id`, the first in tree order. Collapse
    /// single-child containers, renormalize split ratios, and clear fullscreen
    /// on the removed tile. Restore the prior focus if it was another surviving
    /// ID. Return false for an absent ID.
    ///
    /// Session healing uses one-at-a-time removal for duplicate dock claims.
    /// Live drop operations pair removal with insertion into the destination
    /// before returning, so a move retains its tile ID.
    pub(crate) fn remove(&mut self, id: TileId) -> bool {
        if !self.contains(id) {
            return false;
        }
        let prev_focused = self.focused;
        self.set_focus(id);
        self.remove_focused();
        if let Some(prev) = prev_focused
            && prev != id
            && self.contains(prev)
        {
            self.set_focus(prev);
        }
        true
    }

    /// Compute every visible tile's rectangle within `bounds`. When a tile
    /// is fullscreen it is the only visible tile and fills `bounds`.
    pub fn layout(&self, bounds: Rect) -> Vec<(TileId, Rect)> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        if let Some(fs) = self.fullscreen
            && self.contains(fs)
        {
            return vec![(fs, bounds)];
        }
        let mut out = Vec::new();
        layout_node(root, bounds, &mut out);
        out
    }

    /// The number of slots [`Tree::layout`] paints when no tile is
    /// fullscreen: one per leaf and one per stack (only its active member
    /// shows). Counted without allocating, so render may ask every frame.
    pub fn slot_count(&self) -> usize {
        self.root.as_ref().map_or(0, count_slots)
    }

    /// The geometric neighbor of the focused tile in `dir`, per the visible
    /// layout: nearest facing edge within EPS, positive perpendicular
    /// overlap, ties broken by larger overlap.
    pub fn neighbor(&self, dir: Direction) -> Option<TileId> {
        let focused = self.focused?;
        let rects = self.layout(Rect::UNIT);
        let f = rects.iter().find(|(id, _)| *id == focused)?.1;
        let mut best: Option<(TileId, f32, f32)> = None; // (id, edge_key, overlap)
        for (id, r) in &rects {
            if *id == focused {
                continue;
            }
            // edge_key is oriented so that larger = nearer to the focused
            // tile's facing edge.
            let candidate = match dir {
                Direction::Left => (r.right() <= f.x + EPS).then(|| (r.right(), v_overlap(r, &f))),
                Direction::Right => (r.x >= f.right() - EPS).then(|| (-r.x, v_overlap(r, &f))),
                Direction::Up => (r.bottom() <= f.y + EPS).then(|| (r.bottom(), h_overlap(r, &f))),
                Direction::Down => (r.y >= f.bottom() - EPS).then(|| (-r.y, h_overlap(r, &f))),
            };
            let Some((edge_key, overlap)) = candidate else {
                continue;
            };
            if overlap <= EPS {
                continue;
            }
            let better = match &best {
                None => true,
                Some((_, best_key, best_overlap)) => {
                    edge_key > *best_key + EPS
                        || ((edge_key - *best_key).abs() <= EPS && overlap > *best_overlap + EPS)
                }
            };
            if better {
                best = Some((*id, edge_key, overlap));
            }
        }
        best.map(|(id, _, _)| id)
    }

    pub fn focus_direction(&mut self, dir: Direction) -> bool {
        match self.neighbor(dir) {
            Some(id) => {
                self.set_focus(id);
                true
            }
            None => false,
        }
    }

    /// Swap a plain focused tile with its geometric neighbor while retaining
    /// focus on its ID. A stack member instead pops out in that direction.
    pub fn move_direction(&mut self, dir: Direction) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        if self.stack_position(focused).is_some() {
            let after = matches!(dir, Direction::Right | Direction::Down);
            return self.pop_out(dir.orientation(), after);
        }
        let Some(neighbor) = self.neighbor(dir) else {
            return false;
        };
        if let Some(root) = &mut self.root {
            swap_leaves(root, focused, neighbor);
        }
        true
    }

    /// Move the divider adjacent to the focused tile in `dir` by `delta`
    /// (a fraction of the containing split; `delta` is always given
    /// positive — the direction itself carries the sign, vim-style: each of
    /// the four resize keys moves *a* divider that direction, never "grows
    /// the focused tile" as a fixed idea).
    ///
    /// Walks ancestors from the focused tile outward (same `path_to` walk
    /// as before) to the deepest split whose orientation matches `dir`'s
    /// axis; within it, at the focused child's index `i`, always prefers
    /// the divider on the focused child's right/bottom side (positive axis
    /// direction), falling back to the left/top side only when no right/
    /// bottom divider exists:
    /// - If a next sibling exists (`i + 1 < len`), that divider moves —
    ///   `ratios[i] += sign * delta`, `ratios[i + 1] -= sign * delta`,
    ///   where `sign` is +1 for Right/Down and -1 for Left/Up.
    /// - Otherwise (focused is the split's rightmost/bottommost child),
    ///   the divider on the *left/top* side moves instead — `ratios[i - 1] += sign * delta`,
    ///   `ratios[i] -= sign * delta` — which reverses the effect on the
    ///   focused tile (edge-flip: the key always moves a divider that
    ///   direction, not always the tile the same way).
    ///
    /// Result: `h`/`l` always operate the split on the focused tile's RIGHT
    /// (h moves it left = narrower, l right = wider); `j`/`k` always the
    /// BOTTOM split (j down = taller, k up = shorter); only a tile at the
    /// right/bottom edge of its split falls back to its left/top divider
    /// (where h then widens and l narrows — the edge flip, unchanged).
    ///
    /// If neither side has a divider (a lone child — excluded by the >=2
    /// invariant on real splits, but guards degenerate input), the walk
    /// continues to a shallower matching-orientation ancestor; if none is
    /// found, returns false.
    ///
    /// Never partially applies: both new ratios are computed first, and if
    /// either would drop below `MIN_RATIO` nothing changes and this
    /// returns false.
    pub fn move_divider(&mut self, dir: Direction, delta: f32) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        let Some(root) = &mut self.root else {
            return false;
        };
        let mut path = Vec::new();
        if !path_to(root, focused, &mut path) {
            return false;
        }
        // Walk ancestors from deepest to shallowest looking for a split
        // whose orientation matches the resize direction.
        for depth in (0..path.len()).rev() {
            let node = node_at_mut(root, &path[..depth]);
            let Node::Split {
                orientation,
                ratios,
                ..
            } = node
            else {
                continue;
            };
            if *orientation != dir.orientation() {
                continue;
            }
            let i = path[depth];
            let sign = match dir {
                Direction::Right | Direction::Down => 1.0,
                Direction::Left | Direction::Up => -1.0,
            };
            // Always prefer the divider on the focused child's right/bottom
            // side (i, i+1), falling back to the left/top side (i-1, i) only
            // when no right/bottom divider exists.
            let (a, b) = if i + 1 < ratios.len() {
                (i, i + 1)
            } else if i > 0 {
                (i - 1, i)
            } else {
                continue;
            };
            let new_a = ratios[a] + sign * delta;
            let new_b = ratios[b] - sign * delta;
            if new_a < MIN_RATIO || new_b < MIN_RATIO {
                return false;
            }
            ratios[a] = new_a;
            ratios[b] = new_b;
            return true;
        }
        false
    }

    /// Move an addressed divider to an absolute cursor position within `bounds`.
    /// Recompute the owning split rectangle along the address path, then adjust
    /// only its adjacent ratio pair. Preserve that pair's sum and clamp each
    /// side to at least `MIN_RATIO`.
    ///
    /// Return false for an invalid path/boundary, non-finite position on the
    /// relevant axis, nonpositive extent, or pair total below `2 * MIN_RATIO`.
    /// Also return false when the ratio change is below 1e-6. Unlike discrete
    /// keyboard resize, dragging beyond the limit clamps to it. Bounds must use
    /// the same coordinate space as the cursor and rendered tree.
    pub fn drag_divider(&mut self, address: &DividerAddress, x: f32, y: f32, bounds: Rect) -> bool {
        let Some(root) = &mut self.root else {
            return false;
        };
        let mut node = root;
        let mut rect = bounds;
        for &ix in &address.path {
            let Node::Split {
                orientation,
                children,
                ratios,
            } = node
            else {
                return false;
            };
            if ix >= children.len() {
                return false;
            }
            let before: f32 = ratios[..ix].iter().sum();
            rect = match orientation {
                Orientation::Horizontal => Rect {
                    x: rect.x + rect.w * before,
                    y: rect.y,
                    w: rect.w * ratios[ix],
                    h: rect.h,
                },
                Orientation::Vertical => Rect {
                    x: rect.x,
                    y: rect.y + rect.h * before,
                    w: rect.w,
                    h: rect.h * ratios[ix],
                },
            };
            node = &mut children[ix];
        }
        let Node::Split {
            orientation,
            ratios,
            ..
        } = node
        else {
            return false;
        };
        let i = address.index;
        if i + 1 >= ratios.len() {
            return false;
        }
        let (origin, extent, pos) = match orientation {
            Orientation::Horizontal => (rect.x, rect.w, x),
            Orientation::Vertical => (rect.y, rect.h, y),
        };
        if !pos.is_finite() || extent <= 0.0 {
            return false;
        }
        let start: f32 = ratios[..i].iter().sum();
        let total = ratios[i] + ratios[i + 1];
        if total < 2.0 * MIN_RATIO {
            return false;
        }
        let new_a = ((pos - origin) / extent - start).clamp(MIN_RATIO, total - MIN_RATIO);
        // Ignore sub-1e-6 ratio changes so a repeated position or a drag pinned
        // at its limit does not trigger redundant dirty state and rendering.
        if (new_a - ratios[i]).abs() < 1e-6 {
            return false;
        }
        ratios[i] = new_a;
        ratios[i + 1] = total - new_a;
        true
    }

    /// Insert `new` beside `anchor` on the chosen side. If the enclosing split
    /// has the requested orientation, add a sibling and equalize its ratios;
    /// otherwise wrap the anchor slot in a half-and-half split. A stack member
    /// anchors its whole stack. Focus `new` and clear fullscreen.
    ///
    /// Refuse identical IDs, an absent anchor, or a `new` already in this tree
    /// before mutation. Workspace drop callers check both endpoints before
    /// removal and retain a plain-split fallback if insertion is refused.
    pub(crate) fn insert_at_leaf(
        &mut self,
        anchor: TileId,
        new: TileId,
        orientation: Orientation,
        after: bool,
    ) -> bool {
        if anchor == new || !self.contains(anchor) || self.contains(new) {
            return false;
        }
        // `contains(anchor)` just passed, so the root exists.
        let root = self.root.take().expect("contains(anchor) implies a root");
        self.fullscreen = None;
        self.root = Some(insert_beside(root, anchor, new, orientation, after));
        self.set_focus(new);
        true
    }

    /// Clear fullscreen without changing focus. Moving into a dock uses this
    /// so the destination remains visible; dock restoration also uses it to
    /// enforce the absence of dock fullscreen state.
    pub fn exit_fullscreen(&mut self) {
        self.fullscreen = None;
    }

    /// Toggle fullscreen on the focused tile. Returns false on an empty tree.
    pub fn toggle_fullscreen(&mut self) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        self.fullscreen = if self.fullscreen == Some(focused) {
            None
        } else {
            Some(focused)
        };
        true
    }

    /// Reorient the split around the focused tile (pairwise, user
    /// direction — NOT i3's whole-container "layout toggle split", which
    /// restacks every sibling in a flat split at once). In a 2-child split
    /// the orientation simply flips, children and ratios reinterpreted
    /// along the other axis — a 70/30 row becomes a 70/30 stack. In a
    /// wider (flat) split, only the focused tile and one adjacent sibling
    /// reorient: the pair is extracted into a new sub-split of the flipped
    /// orientation occupying exactly their old combined footprint, and
    /// every other sibling keeps its slot untouched. The partner is the
    /// next sibling, falling back to the previous one when the focused
    /// tile is the split's last child — the same right/bottom preference
    /// [`Tree::move_divider`] uses. Returns false when there is nothing to
    /// reorient (empty tree, or the focused tile is the lone root leaf).
    pub fn toggle_split_orientation(&mut self) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
        fn flipped(orientation: Orientation) -> Orientation {
            match orientation {
                Orientation::Horizontal => Orientation::Vertical,
                Orientation::Vertical => Orientation::Horizontal,
            }
        }
        fn toggle_at(node: &mut Node, focused: TileId) -> bool {
            let Node::Split {
                orientation,
                children,
                ratios,
            } = node
            else {
                return false;
            };
            let Some(i) = children.iter().position(|c| node_holds(c, focused)) else {
                return children.iter_mut().any(|c| toggle_at(c, focused));
            };
            if children.len() == 2 {
                *orientation = flipped(*orientation);
                return true;
            }
            // Pair the focused child with its next sibling (previous when
            // focused is last); `a` is the pair's leftmost/topmost index.
            let a = if i + 1 < children.len() { i } else { i - 1 };
            let pair: Vec<Node> = children.drain(a..=a + 1).collect();
            let pair_ratios: Vec<f32> = ratios.drain(a..=a + 1).collect();
            let total = pair_ratios[0] + pair_ratios[1];
            children.insert(
                a,
                Node::Split {
                    orientation: flipped(*orientation),
                    children: pair,
                    ratios: pair_ratios.iter().map(|r| r / total).collect(),
                },
            );
            ratios.insert(a, total);
            true
        }
        self.root
            .as_mut()
            .is_some_and(|root| toggle_at(root, focused))
    }

    /// Validate and restore raw tree parts. Reject splits with fewer than two
    /// children, mismatched ratio counts, or nonpositive/non-finite ratios.
    /// Normalize accepted ratios; heal stack membership and active indices.
    ///
    /// Drop focus/fullscreen references to IDs absent from the restored tree.
    /// A retained focus activates its stack member. Workspace and dock
    /// constructors supply a first-tile focus when a nonempty tree lacks one.
    /// Plain duplicate leaf IDs are not rejected here; dock claim healing is a
    /// separate workspace operation.
    pub fn from_parts(
        root: Option<Node>,
        focused: Option<TileId>,
        fullscreen: Option<TileId>,
    ) -> Result<Tree, String> {
        let root = root
            .map(|n| validate_node(n, &mut Vec::new()))
            .transpose()?
            .flatten();

        let mut leaves = Vec::new();
        if let Some(root) = &root {
            collect_leaves(root, &mut leaves);
        }
        let focused = focused.filter(|id| leaves.contains(id));
        let fullscreen = fullscreen.filter(|id| leaves.contains(id));

        let mut tree = Tree {
            root,
            focused,
            fullscreen,
        };
        if let Some(f) = focused {
            tree.activate(f);
        }
        Ok(tree)
    }
}

/// Validate splits and normalize their ratios. For stacks, discard members
/// already encountered in tree order or repeated within the stack; reset an
/// out-of-range active index to zero. Collapse one survivor to a leaf and
/// remove an empty stack, collapsing any parent split left with one child.
fn validate_node(node: Node, seen: &mut Vec<TileId>) -> Result<Option<Node>, String> {
    match node {
        Node::Leaf(id) => {
            seen.push(id);
            Ok(Some(Node::Leaf(id)))
        }
        Node::Stack { children, active } => {
            let mut kept: Vec<TileId> = Vec::with_capacity(children.len());
            for id in children {
                if !seen.contains(&id) && !kept.contains(&id) {
                    kept.push(id);
                }
            }
            seen.extend(kept.iter().copied());
            Ok(match kept.len() {
                0 => None,
                1 => Some(Node::Leaf(kept[0])),
                n => Some(Node::Stack {
                    active: if active < n { active } else { 0 },
                    children: kept,
                }),
            })
        }
        Node::Split {
            orientation,
            children,
            ratios,
        } => {
            if children.len() < 2 {
                return Err(format!(
                    "split has {} children, need at least 2",
                    children.len()
                ));
            }
            if children.len() != ratios.len() {
                return Err(format!(
                    "split has {} children but {} ratios",
                    children.len(),
                    ratios.len()
                ));
            }
            for ratio in &ratios {
                if !ratio.is_finite() || *ratio <= 0.0 {
                    return Err(format!("ratio {ratio} is not finite and positive"));
                }
            }
            let mut kept_children = Vec::new();
            let mut kept_ratios = Vec::new();
            for (child, ratio) in children.into_iter().zip(ratios) {
                if let Some(child) = validate_node(child, seen)? {
                    kept_children.push(child);
                    kept_ratios.push(ratio);
                }
            }
            match kept_children.len() {
                0 => Ok(None),
                1 => Ok(kept_children.pop()),
                _ => {
                    let sum: f32 = kept_ratios.iter().sum();
                    let ratios = kept_ratios.iter().map(|r| r / sum).collect();
                    Ok(Some(Node::Split {
                        orientation,
                        children: kept_children,
                        ratios,
                    }))
                }
            }
        }
    }
}

fn collect_leaves(node: &Node, out: &mut Vec<TileId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Stack { children, .. } => out.extend(children),
        Node::Split { children, .. } => {
            for child in children {
                collect_leaves(child, out);
            }
        }
    }
}

/// Insert beside an anchor slot. A matching-orientation split gets a flat
/// sibling with equalized ratios; otherwise wrap the slot in a half-and-half
/// split. A stack containing the anchor is one slot. `after` selects the side.
/// Shared by focus-based splitting and ID-based edge insertion.
fn insert_beside(
    node: Node,
    anchor: TileId,
    new: TileId,
    orientation: Orientation,
    after: bool,
) -> Node {
    match node {
        node if node_holds(&node, anchor) => {
            let children = if after {
                vec![node, Node::Leaf(new)]
            } else {
                vec![Node::Leaf(new), node]
            };
            Node::Split {
                orientation,
                children,
                ratios: vec![0.5, 0.5],
            }
        }
        leaf @ Node::Leaf(_) => leaf,
        stack @ Node::Stack { .. } => stack,
        Node::Split {
            orientation: existing,
            mut children,
            ratios,
        } => {
            if existing == orientation
                && let Some(ix) = children.iter().position(|c| node_holds(c, anchor))
            {
                let at = if after { ix + 1 } else { ix };
                children.insert(at, Node::Leaf(new));
                let n = children.len() as f32;
                let ratios = vec![1.0 / n; children.len()];
                return Node::Split {
                    orientation: existing,
                    children,
                    ratios,
                };
            }
            let children = children
                .into_iter()
                .map(|c| insert_beside(c, anchor, new, orientation, after))
                .collect();
            Node::Split {
                orientation: existing,
                children,
                ratios,
            }
        }
    }
}

/// Remove the first occurrence of `target` in tree order and rebuild the
/// container, returning `None` if empty. The threaded `done` flag prevents
/// removing every duplicate at once: dock healing must be able to remove
/// claims one at a time while retaining a surviving tile.
fn remove_leaf(node: Node, target: TileId, done: &mut bool) -> Option<Node> {
    match node {
        Node::Leaf(id) if id == target && !*done => {
            *done = true;
            None
        }
        leaf @ Node::Leaf(_) => Some(leaf),
        Node::Stack {
            mut children,
            active,
        } => {
            let Some(ix) = (!*done)
                .then(|| children.iter().position(|c| *c == target))
                .flatten()
            else {
                return Some(Node::Stack { children, active });
            };
            *done = true;
            children.remove(ix);
            match children.len() {
                0 => None,
                1 => Some(Node::Leaf(children[0])),
                n => {
                    // Select the next member at the removed index, or the previous one
                    // when the removed member was last.
                    let active = if ix < active {
                        active - 1
                    } else {
                        active.min(n - 1)
                    };
                    Some(Node::Stack { children, active })
                }
            }
        }
        Node::Split {
            orientation,
            children,
            ratios,
        } => {
            let mut kept_children = Vec::new();
            let mut kept_ratios = Vec::new();
            let mut removed = false;
            for (child, ratio) in children.into_iter().zip(ratios) {
                match remove_leaf(child, target, done) {
                    Some(child) => {
                        kept_children.push(child);
                        kept_ratios.push(ratio);
                    }
                    None => removed = true,
                }
            }
            match kept_children.len() {
                0 => None,
                1 => kept_children.pop(),
                _ => {
                    if removed {
                        let total: f32 = kept_ratios.iter().sum();
                        for ratio in &mut kept_ratios {
                            *ratio /= total;
                        }
                    }
                    Some(Node::Split {
                        orientation,
                        children: kept_children,
                        ratios: kept_ratios,
                    })
                }
            }
        }
    }
}

/// [`layout_node`]'s slot rule without the geometry: must emit exactly one
/// count wherever `layout_node` pushes one rect.
fn count_slots(node: &Node) -> usize {
    match node {
        Node::Leaf(_) | Node::Stack { .. } => 1,
        Node::Split { children, .. } => children.iter().map(count_slots).sum(),
    }
}

fn layout_node(node: &Node, rect: Rect, out: &mut Vec<(TileId, Rect)>) {
    match node {
        Node::Leaf(id) => out.push((*id, rect)),
        Node::Stack { children, active } => out.push((children[*active], rect)),
        Node::Split {
            orientation,
            children,
            ratios,
        } => {
            let mut offset = 0.0;
            for (child, ratio) in children.iter().zip(ratios) {
                let child_rect = match orientation {
                    Orientation::Horizontal => Rect {
                        x: rect.x + rect.w * offset,
                        y: rect.y,
                        w: rect.w * ratio,
                        h: rect.h,
                    },
                    Orientation::Vertical => Rect {
                        x: rect.x,
                        y: rect.y + rect.h * offset,
                        w: rect.w,
                        h: rect.h * ratio,
                    },
                };
                layout_node(child, child_rect, out);
                offset += ratio;
            }
        }
    }
}

fn path_to(node: &Node, target: TileId, path: &mut Vec<usize>) -> bool {
    match node {
        Node::Leaf(id) => *id == target,
        Node::Stack { children, .. } => children.contains(&target),
        Node::Split { children, .. } => {
            for (ix, child) in children.iter().enumerate() {
                path.push(ix);
                if path_to(child, target, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
    }
}

fn node_at_mut<'a>(node: &'a mut Node, path: &[usize]) -> &'a mut Node {
    let mut current = node;
    for &ix in path {
        match current {
            Node::Split { children, .. } => current = &mut children[ix],
            // Invariant: `path` was produced by `path_to` over this same
            // tree, so every prefix lands on a Split.
            Node::Leaf(_) | Node::Stack { .. } => unreachable!("path indexes into splits"),
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rects(tree: &Tree) -> Vec<(TileId, Rect)> {
        tree.layout(Rect::UNIT)
    }

    fn rect_of(tree: &Tree, id: u64) -> Rect {
        rects(tree)
            .into_iter()
            .find(|(t, _)| *t == TileId(id))
            .map(|(_, r)| r)
            .unwrap_or_else(|| panic!("tile {id} not in layout"))
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn slot_count_matches_the_unmaximised_layout() {
        let mut t = Tree::default();
        assert_eq!(t.slot_count(), 0);
        t.split(TileId(1), Orientation::Horizontal);
        t.split(TileId(2), Orientation::Horizontal);
        t.split(TileId(3), Orientation::Vertical);
        assert!(t.stack_after(TileId(3), TileId(4)));
        assert_eq!(t.tiles().len(), 4);
        assert_eq!(t.slot_count(), 3);
        assert_eq!(t.slot_count(), t.layout(Rect::UNIT).len());
        t.toggle_fullscreen();
        assert_eq!(
            t.slot_count(),
            3,
            "the count ignores fullscreen; layout's fullscreen filter is the caller's"
        );
    }

    #[test]
    fn empty_tree_has_empty_layout() {
        let tree = Tree::default();
        assert!(tree.is_empty());
        assert!(rects(&tree).is_empty());
        assert_eq!(tree.focused(), None);
    }

    #[test]
    fn split_on_empty_creates_first_tile_fullsize() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert_eq!(tree.focused(), Some(TileId(1)));
        let r = rect_of(&tree, 1);
        assert!(approx(r.x, 0.0) && approx(r.y, 0.0) && approx(r.w, 1.0) && approx(r.h, 1.0));
    }

    #[test]
    fn horizontal_split_divides_left_right() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert_eq!(tree.focused(), Some(TileId(2)));
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        assert!(approx(r1.x, 0.0) && approx(r1.w, 0.5) && approx(r1.h, 1.0));
        assert!(approx(r2.x, 0.5) && approx(r2.w, 0.5) && approx(r2.h, 1.0));
    }

    #[test]
    fn vertical_split_divides_top_bottom() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Vertical);
        tree.split(TileId(2), Orientation::Vertical);
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        assert!(approx(r1.y, 0.0) && approx(r1.h, 0.5) && approx(r1.w, 1.0));
        assert!(approx(r2.y, 0.5) && approx(r2.h, 0.5) && approx(r2.w, 1.0));
    }

    #[test]
    fn same_orientation_inserts_sibling_and_equalizes() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        for id in [1, 2, 3] {
            assert!(
                approx(rect_of(&tree, id).w, 1.0 / 3.0),
                "tile {id} not a third wide"
            );
        }
        // 3 was inserted after the focused tile 2: order is 1, 2, 3 left to right.
        assert!(rect_of(&tree, 1).x < rect_of(&tree, 2).x);
        assert!(rect_of(&tree, 2).x < rect_of(&tree, 3).x);
    }

    #[test]
    fn cross_orientation_wraps_focused_leaf() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Vertical); // wraps tile 2's slot
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        let r3 = rect_of(&tree, 3);
        assert!(approx(r1.w, 0.5) && approx(r1.h, 1.0));
        assert!(approx(r2.w, 0.5) && approx(r2.h, 0.5) && approx(r2.y, 0.0));
        assert!(approx(r3.w, 0.5) && approx(r3.h, 0.5) && approx(r3.y, 0.5));
    }

    #[test]
    fn focus_setter_validates_membership() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert!(tree.focus(TileId(1)));
        assert_eq!(tree.focused(), Some(TileId(1)));
        assert!(!tree.focus(TileId(99)));
        assert_eq!(tree.focused(), Some(TileId(1)));
    }

    #[test]
    fn close_collapses_single_child_split() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.close(); // closes focused tile 2
        assert_eq!(tree.tiles(), vec![TileId(1)]);
        assert_eq!(tree.focused(), Some(TileId(1)));
        let r = rect_of(&tree, 1);
        assert!(
            approx(r.w, 1.0) && approx(r.h, 1.0),
            "collapse must restore full size"
        );
        assert!(
            matches!(tree.root(), Some(Node::Leaf(_))),
            "single-child split must collapse"
        );
    }

    #[test]
    fn close_renormalizes_remaining_ratios() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        tree.focus(TileId(2));
        tree.close();
        assert!(approx(rect_of(&tree, 1).w, 0.5));
        assert!(approx(rect_of(&tree, 3).w, 0.5));
    }

    #[test]
    fn close_last_tile_empties_tree() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.close();
        assert!(tree.is_empty());
        assert_eq!(tree.focused(), None);
        assert!(rects(&tree).is_empty());
    }

    #[test]
    fn close_on_empty_tree_is_a_noop() {
        let mut tree = Tree::default();
        tree.close();
        assert!(tree.is_empty());
    }

    #[test]
    fn layout_scales_to_arbitrary_bounds() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        let bounds = Rect {
            x: 10.0,
            y: 20.0,
            w: 100.0,
            h: 50.0,
        };
        let out = tree.layout(bounds);
        let r2 = out.iter().find(|(t, _)| *t == TileId(2)).unwrap().1;
        assert!(
            approx(r2.x, 60.0) && approx(r2.y, 20.0) && approx(r2.w, 50.0) && approx(r2.h, 50.0)
        );
    }

    /// 2x2 grid:  1 | 2      built with splits + focus, ids at positions:
    ///            -----      1 top-left, 4 bottom-left, 2 top-right,
    ///            4 | 3      3 bottom-right.
    fn grid() -> Tree {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal); // [1]
        tree.split(TileId(2), Orientation::Horizontal); // [1 | 2]
        tree.split(TileId(3), Orientation::Vertical); // 2 wraps: [1 | (2 / 3)]
        tree.focus(TileId(1));
        tree.split(TileId(4), Orientation::Vertical); // 1 wraps: [(1 / 4) | (2 / 3)]
        tree.focus(TileId(1));
        tree
    }

    #[test]
    fn grid_geometry_is_as_documented() {
        let tree = grid();
        let r1 = rect_of(&tree, 1);
        let r3 = rect_of(&tree, 3);
        assert!(approx(r1.x, 0.0) && approx(r1.y, 0.0) && approx(r1.w, 0.5) && approx(r1.h, 0.5));
        assert!(approx(r3.x, 0.5) && approx(r3.y, 0.5));
    }

    #[test]
    fn focus_moves_right_and_down_through_grid() {
        let mut tree = grid();
        assert!(tree.focus_direction(Direction::Right));
        assert_eq!(tree.focused(), Some(TileId(2)));
        assert!(tree.focus_direction(Direction::Down));
        assert_eq!(tree.focused(), Some(TileId(3)));
        assert!(tree.focus_direction(Direction::Left));
        assert_eq!(tree.focused(), Some(TileId(4)));
        assert!(tree.focus_direction(Direction::Up));
        assert_eq!(tree.focused(), Some(TileId(1)));
    }

    #[test]
    fn no_wrap_at_edges() {
        let mut tree = grid();
        assert!(!tree.focus_direction(Direction::Left));
        assert_eq!(tree.focused(), Some(TileId(1)));
        assert!(!tree.focus_direction(Direction::Up));
        assert_eq!(tree.focused(), Some(TileId(1)));
    }

    #[test]
    fn nearest_edge_wins_over_overlap() {
        // [a | b] where the right column is split into c over d; from a,
        // focusing Right must land on whichever of c/d overlaps a more —
        // here both overlap equally until we unbalance: c is the top 3/4.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Vertical); // right col: 2 over 3
        // Check focus and overlap tie-breaking on the symmetric grid.
        tree.focus(TileId(1));
        assert!(tree.focus_direction(Direction::Right));
        // 2 and 3 are equidistant (same shared edge); overlap with the
        // full-height tile 1 is equal (0.5 each), so the first-best stands.
        // Pin the documented deterministic outcome:
        assert_eq!(tree.focused(), Some(TileId(2)));
    }

    #[test]
    fn move_swaps_with_neighbor_and_focus_follows() {
        let mut tree = grid();
        assert!(tree.move_direction(Direction::Right)); // swap 1 and 2
        assert_eq!(tree.focused(), Some(TileId(1)));
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        assert!(
            approx(r1.x, 0.5) && approx(r1.y, 0.0),
            "1 moved to top-right"
        );
        assert!(
            approx(r2.x, 0.0) && approx(r2.y, 0.0),
            "2 moved to top-left"
        );
    }

    #[test]
    fn move_with_no_neighbor_is_noop() {
        let mut tree = grid();
        assert!(!tree.move_direction(Direction::Left));
        assert!(approx(rect_of(&tree, 1).x, 0.0));
    }

    #[test]
    fn fullscreen_blocks_navigation() {
        let mut tree = grid();
        tree.toggle_fullscreen(); // Fullscreen replaces the split layout.
        assert!(!tree.focus_direction(Direction::Right));
        assert_eq!(tree.focused(), Some(TileId(1)));
    }

    #[test]
    fn move_divider_widens_focused_left_tile_toward_right() {
        // [1 | 2], focused 1 (left tile). Right moves the divider between
        // them rightward: the focused left tile widens.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(tree.move_divider(Direction::Right, 0.1));
        assert!(approx(rect_of(&tree, 1).w, 0.6));
        assert!(approx(rect_of(&tree, 2).w, 0.4));
        assert!(approx(rect_of(&tree, 2).x, 0.6));
    }

    #[test]
    fn move_divider_narrows_focused_left_tile_toward_left() {
        // Same layout, Left moves the same divider leftward: the focused
        // left tile narrows.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(tree.move_divider(Direction::Left, 0.1));
        assert!(approx(rect_of(&tree, 1).w, 0.4));
        assert!(approx(rect_of(&tree, 2).w, 0.6));
    }

    #[test]
    fn move_divider_edge_flip_narrows_focused_right_tile_toward_right() {
        // [1 | 2], focused 2 (rightmost — no divider on its right side).
        // Right still moves *a* divider rightward: the only one available
        // is 2's left divider, so it moves right and 2 narrows.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert_eq!(tree.focused(), Some(TileId(2)));
        assert!(tree.move_divider(Direction::Right, 0.1));
        assert!(approx(rect_of(&tree, 1).w, 0.6));
        assert!(approx(rect_of(&tree, 2).w, 0.4));
    }

    #[test]
    fn move_divider_edge_flip_widens_focused_right_tile_toward_left() {
        // Mirror: Left on the same focused rightmost tile moves its left
        // divider left, so the focused tile widens.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert_eq!(tree.focused(), Some(TileId(2)));
        assert!(tree.move_divider(Direction::Left, 0.1));
        assert!(approx(rect_of(&tree, 1).w, 0.4));
        assert!(approx(rect_of(&tree, 2).w, 0.6));
    }

    #[test]
    fn move_divider_vertical_stack_down_and_up() {
        // Vertical analogue: [1 / 2] stacked, focused 1 (top). Down widens
        // the top tile; Up (on the same focus) narrows it.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Vertical);
        tree.split(TileId(2), Orientation::Vertical);
        tree.focus(TileId(1));
        assert!(tree.move_divider(Direction::Down, 0.1));
        assert!(approx(rect_of(&tree, 1).h, 0.6));
        assert!(approx(rect_of(&tree, 2).h, 0.4));
        assert!(tree.move_divider(Direction::Up, 0.2));
        assert!(approx(rect_of(&tree, 1).h, 0.4));
        assert!(approx(rect_of(&tree, 2).h, 0.6));
    }

    #[test]
    fn move_divider_middle_tile_in_a_row_always_moves_right_divider() {
        // 3-way row [1 | 2 | 3], focused 2 (middle — a divider exists on
        // BOTH sides). Right and Left both move the divider on 2's right
        // side (the 2/3 divider), just in opposite directions: Right moves
        // it rightward (2 widens into 3's space); Left moves it leftward
        // (2 narrows, 3 widens). Tile 1 is never touched.
        let row = || {
            let mut tree = Tree::default();
            tree.split(TileId(1), Orientation::Horizontal);
            tree.split(TileId(2), Orientation::Horizontal);
            tree.split(TileId(3), Orientation::Horizontal);
            tree.focus(TileId(2));
            tree
        };

        let mut right = row();
        assert!(right.move_divider(Direction::Right, 0.1));
        assert!(approx(rect_of(&right, 1).w, 1.0 / 3.0), "tile 1 untouched");
        assert!(approx(rect_of(&right, 2).w, 1.0 / 3.0 + 0.1));
        assert!(approx(rect_of(&right, 3).w, 1.0 / 3.0 - 0.1));

        let mut left = row();
        assert!(left.move_divider(Direction::Left, 0.1));
        assert!(approx(rect_of(&left, 1).w, 1.0 / 3.0), "tile 1 untouched");
        assert!(approx(rect_of(&left, 2).w, 1.0 / 3.0 - 0.1));
        assert!(approx(rect_of(&left, 3).w, 1.0 / 3.0 + 0.1));
    }

    #[test]
    fn move_divider_middle_tile_in_a_stack_always_moves_bottom_divider() {
        // Vertical analogue: 3-way stack [1 / 2 / 3], focused 2 (middle).
        // Down and Up both move the divider on 2's bottom side (the 2/3 divider),
        // just in opposite directions: Down moves it downward (2 widens into 3's
        // space); Up moves it upward (2 narrows, 3 widens). Tile 1 is never touched.
        let stack = || {
            let mut tree = Tree::default();
            tree.split(TileId(1), Orientation::Vertical);
            tree.split(TileId(2), Orientation::Vertical);
            tree.split(TileId(3), Orientation::Vertical);
            tree.focus(TileId(2));
            tree
        };

        let mut down = stack();
        assert!(down.move_divider(Direction::Down, 0.1));
        assert!(approx(rect_of(&down, 1).h, 1.0 / 3.0), "tile 1 untouched");
        assert!(approx(rect_of(&down, 2).h, 1.0 / 3.0 + 0.1));
        assert!(approx(rect_of(&down, 3).h, 1.0 / 3.0 - 0.1));

        let mut up = stack();
        assert!(up.move_divider(Direction::Up, 0.1));
        assert!(approx(rect_of(&up, 1).h, 1.0 / 3.0), "tile 1 untouched");
        assert!(approx(rect_of(&up, 2).h, 1.0 / 3.0 - 0.1));
        assert!(approx(rect_of(&up, 3).h, 1.0 / 3.0 + 0.1));
    }

    #[test]
    fn move_divider_clamps_at_min_ratio_with_no_partial_mutation() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        // Repeated in-bounds moves approach the clamp...
        assert!(tree.move_divider(Direction::Right, 0.4)); // -> [0.9, 0.1]
        assert!(approx(rect_of(&tree, 1).w, 0.9));
        // ...then one more push that would cross MIN_RATIO is rejected
        // outright, changing nothing.
        assert!(
            !tree.move_divider(Direction::Right, 0.1),
            "would push neighbor below MIN_RATIO"
        );
        assert!(
            approx(rect_of(&tree, 1).w, 0.9),
            "failed move must change nothing"
        );
        assert!(
            approx(rect_of(&tree, 2).w, 0.1),
            "failed move must change nothing"
        );
    }

    #[test]
    fn move_divider_finds_matching_orientation_ancestor() {
        let mut tree = grid(); // focused: 1 (top-left)
        // Up/Down on tile 1 must move the left column's inner vertical
        // divider (between 1 and 4), not the outer horizontal one.
        assert!(tree.move_divider(Direction::Down, 0.2));
        assert!(approx(rect_of(&tree, 1).h, 0.7));
        assert!(approx(rect_of(&tree, 4).h, 0.3));
        // And Left/Right moves the outer horizontal divider.
        assert!(tree.move_divider(Direction::Right, 0.1));
        assert!(approx(rect_of(&tree, 1).w, 0.6));
        assert!(approx(rect_of(&tree, 2).w, 0.4));
    }

    #[test]
    fn move_divider_with_no_split_is_noop() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(
            !tree.move_divider(Direction::Right, 0.1),
            "single tile has no divider to move"
        );
    }

    // --- Divider dragging ---------------------------------------------

    /// Pixel-flavored bounds for drag tests: dragging is defined against
    /// the laid-out rect, so these tests use a non-unit, offset rect to
    /// prove the origin/extent math rather than hiding it behind 0..1.
    const DRAG_BOUNDS: Rect = Rect {
        x: 100.0,
        y: 50.0,
        w: 1000.0,
        h: 800.0,
    };

    #[test]
    fn drag_divider_sets_the_pair_from_an_absolute_position() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        // Cursor at x=400 within x 100..1100 → boundary at 30%.
        assert!(tree.drag_divider(&addr, 400.0, 0.0, DRAG_BOUNDS));
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        assert!(approx(r1.w, 0.3) && approx(r2.w, 0.7) && approx(r2.x, 0.3));
    }

    #[test]
    fn drag_divider_vertical_split_reads_the_y_coordinate() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Vertical);
        tree.split(TileId(2), Orientation::Vertical);
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        // Cursor at y=650 within y 50..850 → boundary at 75%; the x
        // coordinate is junk on purpose — a vertical split must ignore it.
        assert!(tree.drag_divider(&addr, -9999.0, 650.0, DRAG_BOUNDS));
        assert!(approx(rect_of(&tree, 1).h, 0.75));
        assert!(approx(rect_of(&tree, 2).h, 0.25));
    }

    #[test]
    fn drag_divider_clamps_at_min_ratio_instead_of_failing() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        // Dragging way past the left edge pins the pair at MIN_RATIO —
        // the live-drag behavior is "stop at the limit", where the
        // keyboard's discrete move_divider step rejects outright.
        assert!(tree.drag_divider(&addr, -500.0, 0.0, DRAG_BOUNDS));
        assert!(approx(rect_of(&tree, 1).w, MIN_RATIO));
        assert!(approx(rect_of(&tree, 2).w, 1.0 - MIN_RATIO));
        // And past the right edge the other way.
        assert!(tree.drag_divider(&addr, 5000.0, 0.0, DRAG_BOUNDS));
        assert!(approx(rect_of(&tree, 1).w, 1.0 - MIN_RATIO));
        assert!(approx(rect_of(&tree, 2).w, MIN_RATIO));
    }

    #[test]
    fn drag_divider_middle_pair_leaves_other_siblings_untouched() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        // Boundary between 2 and 3 (pair index 1) to 40% of the bounds:
        // tile 1 keeps its third; 2 and 3 split the remaining 2/3 at the
        // new boundary.
        let addr = DividerAddress {
            path: vec![],
            index: 1,
        };
        assert!(tree.drag_divider(&addr, 500.0, 0.0, DRAG_BOUNDS));
        assert!(approx(rect_of(&tree, 1).w, 1.0 / 3.0), "tile 1 untouched");
        assert!(approx(rect_of(&tree, 2).w, 0.4 - 1.0 / 3.0));
        assert!(approx(rect_of(&tree, 3).w, 0.6));
    }

    #[test]
    fn drag_divider_nested_split_maps_through_the_sub_rect() {
        let mut tree = grid(); // [(1 / 4) | (2 / 3)]
        // The left column's inner divider (path [0], boundary 0) dragged
        // to y=250 within the column's full-height y 50..850 → 25%.
        let addr = DividerAddress {
            path: vec![0],
            index: 0,
        };
        assert!(tree.drag_divider(&addr, 0.0, 250.0, DRAG_BOUNDS));
        assert!(approx(rect_of(&tree, 1).h, 0.25));
        assert!(approx(rect_of(&tree, 4).h, 0.75));
        // The right column's pair is untouched.
        assert!(approx(rect_of(&tree, 2).h, 0.5));
        assert!(approx(rect_of(&tree, 3).h, 0.5));
    }

    #[test]
    fn drag_divider_stale_addresses_are_safe_noops() {
        let mut tree = grid();
        let before = rects(&tree);
        // Path runs through a leaf (leaf at [0, 0] has no children).
        let through_leaf = DividerAddress {
            path: vec![0, 0, 0],
            index: 0,
        };
        assert!(!tree.drag_divider(&through_leaf, 500.0, 400.0, DRAG_BOUNDS));
        // Path index off the end of a split's children.
        let past_children = DividerAddress {
            path: vec![7],
            index: 0,
        };
        assert!(!tree.drag_divider(&past_children, 500.0, 400.0, DRAG_BOUNDS));
        // Boundary index with no right-hand sibling.
        let past_boundary = DividerAddress {
            path: vec![],
            index: 1,
        };
        assert!(!tree.drag_divider(&past_boundary, 500.0, 400.0, DRAG_BOUNDS));
        // Address that lands on a leaf rather than a split.
        let at_leaf = DividerAddress {
            path: vec![0, 0],
            index: 0,
        };
        assert!(!tree.drag_divider(&at_leaf, 500.0, 400.0, DRAG_BOUNDS));
        assert_eq!(rects(&tree), before, "failed drags must change nothing");
    }

    #[test]
    fn drag_divider_refuses_junk_geometry() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        assert!(!tree.drag_divider(&addr, f32::NAN, 0.0, DRAG_BOUNDS));
        let flat = Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 800.0,
        };
        assert!(!tree.drag_divider(&addr, 0.0, 0.0, flat));
        assert!(approx(rect_of(&tree, 1).w, 0.5), "tree untouched");
    }

    #[test]
    fn drag_divider_reports_false_when_nothing_changes() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        // A real move reports true; repeating the exact position reports
        // false (nothing changed), so callers can skip re-renders.
        assert!(tree.drag_divider(&addr, 400.0, 0.0, DRAG_BOUNDS));
        assert!(!tree.drag_divider(&addr, 400.0, 0.0, DRAG_BOUNDS));
        // Pinning at a clamp: the move that first hits it is a change...
        assert!(tree.drag_divider(&addr, -500.0, 0.0, DRAG_BOUNDS));
        assert!(approx(rect_of(&tree, 1).w, MIN_RATIO));
        // ...but every further move pinned at the same clamp is not.
        assert!(!tree.drag_divider(&addr, -600.0, 0.0, DRAG_BOUNDS));
        assert!(!tree.drag_divider(&addr, -9999.0, 0.0, DRAG_BOUNDS));
        assert!(approx(rect_of(&tree, 1).w, MIN_RATIO), "still pinned");
    }

    #[test]
    fn drag_divider_on_an_empty_tree_is_a_noop() {
        let mut tree = Tree::default();
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        assert!(!tree.drag_divider(&addr, 500.0, 400.0, DRAG_BOUNDS));
    }

    #[test]
    fn drag_divider_refuses_a_pair_too_small_to_clamp() {
        // from_parts renormalizes but doesn't enforce MIN_RATIO, so a
        // hostile session can leave a pair totalling under 2×MIN_RATIO —
        // the clamp range would be inverted, so the drag must refuse.
        let tree = Tree::from_parts(
            Some(Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    Node::Leaf(TileId(1)),
                    Node::Leaf(TileId(2)),
                    Node::Leaf(TileId(3)),
                ],
                ratios: vec![0.04, 0.04, 0.92],
            }),
            Some(TileId(1)),
            None,
        );
        let mut tree = tree.expect("structurally valid");
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        let before = rects(&tree);
        assert!(!tree.drag_divider(&addr, 500.0, 0.0, DRAG_BOUNDS));
        assert_eq!(rects(&tree), before);
    }

    #[test]
    fn fullscreen_toggles_and_layout_shows_only_that_tile() {
        let mut tree = grid();
        assert!(tree.toggle_fullscreen());
        assert_eq!(tree.fullscreen(), Some(TileId(1)));
        let out = rects(&tree);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, TileId(1));
        assert!(approx(out[0].1.w, 1.0) && approx(out[0].1.h, 1.0));
        assert!(tree.toggle_fullscreen());
        assert_eq!(tree.fullscreen(), None);
        assert_eq!(rects(&tree).len(), 4);
    }

    #[test]
    fn closing_fullscreen_tile_clears_fullscreen() {
        let mut tree = grid();
        tree.toggle_fullscreen();
        tree.close();
        assert_eq!(tree.fullscreen(), None);
        assert_eq!(rects(&tree).len(), 3);
    }

    #[test]
    fn split_while_fullscreen_exits_fullscreen() {
        let mut tree = grid();
        tree.toggle_fullscreen();
        assert_eq!(tree.fullscreen(), Some(TileId(1)));
        tree.split(TileId(9), Orientation::Horizontal);
        assert_eq!(tree.fullscreen(), None, "split must exit fullscreen");
        assert_eq!(tree.focused(), Some(TileId(9)));
        assert_eq!(rects(&tree).len(), 5, "all tiles visible again");
    }

    #[test]
    fn close_three_siblings_focuses_adjacent() {
        // [1 | 2 | 3]: close middle (2) → focus 3
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        tree.focus(TileId(2));
        tree.close();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(3)]);
        assert_eq!(
            tree.focused(),
            Some(TileId(3)),
            "closing middle tile should focus the next sibling"
        );
    }

    #[test]
    fn close_three_siblings_first_focuses_next() {
        // [1 | 2 | 3]: close first (1) → focus 2
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        tree.focus(TileId(1));
        tree.close();
        assert_eq!(tree.tiles(), vec![TileId(2), TileId(3)]);
        assert_eq!(
            tree.focused(),
            Some(TileId(2)),
            "closing first tile should focus the next tile"
        );
    }

    #[test]
    fn close_three_siblings_last_focuses_previous() {
        // [1 | 2 | 3]: close last (3) → focus 2
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        tree.focus(TileId(3));
        tree.close();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2)]);
        assert_eq!(
            tree.focused(),
            Some(TileId(2)),
            "closing last tile should focus the previous tile"
        );
    }

    #[test]
    fn close_nested_tile_focuses_tree_order_neighbor() {
        // Closing the first leaf in the nested grid selects the next leaf in
        // tree order: tile 4 moves from index 1 to index 0.
        let mut tree = grid(); // tiles: [1, 4, 2, 3], focused: 1
        tree.close(); // closes tile 1 at index 0
        assert_eq!(tree.tiles(), vec![TileId(4), TileId(2), TileId(3)]);
        assert_eq!(
            tree.focused(),
            Some(TileId(4)),
            "closing first nested tile should focus the tree-order neighbor"
        );
    }

    #[test]
    fn close_nested_tile_last_position_focuses_previous() {
        // Using the 2x2 grid fixture: tiles in tree order are [1, 4, 2, 3]
        // Focus and close tile 3 (at index 3): should focus tile 2 (at new index 2)
        // This shows the neighbor rule works at nested leaves.
        let mut tree = grid(); // tiles: [1, 4, 2, 3], focused: 1
        tree.focus(TileId(3));
        tree.close(); // closes tile 3 at index 3
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(4), TileId(2)]);
        assert_eq!(
            tree.focused(),
            Some(TileId(2)),
            "closing last nested tile should focus the previous tile"
        );
    }

    // --- Removal ------------------------------------------------------

    #[test]
    fn remove_focused_returns_the_removed_id_and_refocuses_like_close() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        tree.focus(TileId(2));
        assert_eq!(tree.remove_focused(), Some(TileId(2)));
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(3)]);
        assert_eq!(
            tree.focused(),
            Some(TileId(3)),
            "same refocus rule as close: the next tree-order leaf"
        );
    }

    #[test]
    fn remove_focused_on_empty_tree_is_none() {
        let mut tree = Tree::default();
        assert_eq!(tree.remove_focused(), None);
    }

    #[test]
    fn remove_focused_last_tile_empties_the_tree() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert_eq!(tree.remove_focused(), Some(TileId(1)));
        assert!(tree.is_empty());
        assert_eq!(tree.focused(), None);
    }

    #[test]
    fn remove_focused_clears_fullscreen_on_the_removed_tile() {
        let mut tree = grid();
        tree.toggle_fullscreen();
        assert_eq!(tree.remove_focused(), Some(TileId(1)));
        assert_eq!(tree.fullscreen(), None);
    }

    #[test]
    fn remove_takes_out_an_unfocused_tile_without_moving_focus() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        tree.focus(TileId(3));
        assert!(tree.remove(TileId(1)));
        assert_eq!(tree.tiles(), vec![TileId(2), TileId(3)]);
        assert_eq!(
            tree.focused(),
            Some(TileId(3)),
            "removing a non-focused tile must leave focus alone"
        );
    }

    #[test]
    fn remove_of_the_focused_tile_refocuses_like_close() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(tree.remove(TileId(1)));
        assert_eq!(tree.tiles(), vec![TileId(2)]);
        assert_eq!(tree.focused(), Some(TileId(2)));
    }

    #[test]
    fn remove_of_a_missing_tile_is_rejected_untouched() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(!tree.remove(TileId(99)));
        assert_eq!(tree.tiles(), vec![TileId(1)]);
        assert_eq!(tree.focused(), Some(TileId(1)));
    }

    #[test]
    fn remove_last_tile_empties_the_tree() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(tree.remove(TileId(1)));
        assert!(tree.is_empty());
        assert_eq!(tree.focused(), None);
    }

    #[test]
    fn remove_of_a_duplicated_id_takes_only_the_first_occurrence() {
        // A malformed session can duplicate an ID within a tree. Remove one
        // occurrence so claim healing can retain a survivor.
        let dup = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                Node::Leaf(TileId(5)),
                Node::Leaf(TileId(9)),
                Node::Leaf(TileId(5)),
            ],
            ratios: vec![1.0 / 3.0; 3],
        };
        let mut tree = Tree::from_parts(Some(dup), Some(TileId(9)), None).unwrap();
        assert!(tree.remove(TileId(5)));
        assert_eq!(
            tree.tiles(),
            vec![TileId(9), TileId(5)],
            "exactly one copy removed — the first in tree order"
        );
        assert_eq!(tree.focused(), Some(TileId(9)), "focus untouched");
    }

    #[test]
    fn split_with_root_but_no_focus_inserts_at_the_first_leaf_instead_of_dropping() {
        // A restored nonempty tree may lack focus. Splitting must use the first
        // leaf as its anchor and retain the incoming tile; callers may already
        // have removed that tile from another region.
        let mut tree = Tree::from_parts(Some(Node::Leaf(TileId(1))), None, None).unwrap();
        assert_eq!(tree.focused(), None, "fixture sanity: no focus");
        tree.split(TileId(2), Orientation::Horizontal);
        assert_eq!(
            tree.tiles(),
            vec![TileId(1), TileId(2)],
            "the id must never be dropped"
        );
        assert_eq!(tree.focused(), Some(TileId(2)));
    }

    // --- Tree restoration ---------------------------------------------

    #[test]
    fn from_parts_rebuilds_an_equivalent_tree() {
        let tree = grid();
        let rebuilt =
            Tree::from_parts(tree.root().cloned(), tree.focused(), tree.fullscreen()).unwrap();
        assert_eq!(rebuilt.tiles(), tree.tiles());
        assert_eq!(rebuilt.focused(), tree.focused());
        assert_eq!(rebuilt.fullscreen(), tree.fullscreen());
        assert_eq!(rebuilt.layout(Rect::UNIT), tree.layout(Rect::UNIT));
    }

    #[test]
    fn from_parts_on_none_root_is_the_empty_tree() {
        let tree = Tree::from_parts(None, None, None).unwrap();
        assert!(tree.is_empty());
        assert_eq!(tree.focused(), None);
        assert_eq!(tree.fullscreen(), None);
    }

    #[test]
    fn from_parts_rejects_a_split_with_fewer_than_two_children() {
        let bad = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![Node::Leaf(TileId(1))],
            ratios: vec![1.0],
        };
        assert!(Tree::from_parts(Some(bad), None, None).is_err());
    }

    #[test]
    fn from_parts_rejects_a_ratio_length_mismatch() {
        let bad = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![Node::Leaf(TileId(1)), Node::Leaf(TileId(2))],
            ratios: vec![1.0],
        };
        assert!(Tree::from_parts(Some(bad), None, None).is_err());
    }

    #[test]
    fn from_parts_rejects_a_nan_ratio() {
        let bad = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![Node::Leaf(TileId(1)), Node::Leaf(TileId(2))],
            ratios: vec![f32::NAN, 0.5],
        };
        assert!(Tree::from_parts(Some(bad), None, None).is_err());
    }

    #[test]
    fn from_parts_rejects_a_non_positive_ratio() {
        let bad = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![Node::Leaf(TileId(1)), Node::Leaf(TileId(2))],
            ratios: vec![0.0, 1.0],
        };
        assert!(Tree::from_parts(Some(bad), None, None).is_err());

        let negative = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![Node::Leaf(TileId(1)), Node::Leaf(TileId(2))],
            ratios: vec![-0.5, 1.5],
        };
        assert!(Tree::from_parts(Some(negative), None, None).is_err());
    }

    #[test]
    fn from_parts_renormalizes_small_ratio_drift() {
        let drifted = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![Node::Leaf(TileId(1)), Node::Leaf(TileId(2))],
            ratios: vec![0.501, 0.5], // sums to 1.001, not exactly 1.0
        };
        let tree = Tree::from_parts(Some(drifted), Some(TileId(1)), None).unwrap();
        let rects = tree.layout(Rect::UNIT);
        let total: f32 = rects.iter().map(|(_, r)| r.w).sum();
        assert!(
            (total - 1.0).abs() < 1e-4,
            "renormalized ratios must partition the unit square, got {total}"
        );
    }

    #[test]
    fn from_parts_heals_a_dangling_focused_reference() {
        let node = Node::Leaf(TileId(1));
        let tree = Tree::from_parts(Some(node), Some(TileId(99)), None).unwrap();
        assert_eq!(
            tree.focused(),
            None,
            "a focused id not present as a leaf must be healed to None, not rejected"
        );
    }

    #[test]
    fn from_parts_heals_a_dangling_fullscreen_reference() {
        let node = Node::Leaf(TileId(1));
        let tree = Tree::from_parts(Some(node), Some(TileId(1)), Some(TileId(99))).unwrap();
        assert_eq!(tree.focused(), Some(TileId(1)));
        assert_eq!(
            tree.fullscreen(),
            None,
            "a fullscreen id not present as a leaf must be healed to None, not rejected"
        );
    }

    #[test]
    fn from_parts_preserves_a_valid_fullscreen_reference() {
        let node = Node::Leaf(TileId(1));
        let tree = Tree::from_parts(Some(node), Some(TileId(1)), Some(TileId(1))).unwrap();
        assert_eq!(tree.fullscreen(), Some(TileId(1)));
    }

    #[test]
    fn from_parts_validates_nested_splits() {
        // Outer split is fine; the inner one has a ratio-length mismatch.
        let inner = Node::Split {
            orientation: Orientation::Vertical,
            children: vec![Node::Leaf(TileId(2)), Node::Leaf(TileId(3))],
            ratios: vec![1.0], // wrong length
        };
        let outer = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![Node::Leaf(TileId(1)), inner],
            ratios: vec![0.5, 0.5],
        };
        assert!(Tree::from_parts(Some(outer), None, None).is_err());
    }

    #[test]
    fn composed_resize_split_close_keeps_ratios_normalized() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        // Push toward the clamp, then split (equalizes), then close (renormalizes).
        assert!(tree.move_divider(Direction::Right, 0.4)); // ratios [0.9, 0.1]
        tree.split(TileId(3), Orientation::Horizontal); // equalize to thirds
        tree.focus(TileId(2));
        tree.close(); // renormalize the survivors
        let rects = tree.layout(Rect::UNIT);
        assert_eq!(rects.len(), 2);
        let total: f32 = rects.iter().map(|(_, r)| r.w).sum();
        assert!(
            (total - 1.0).abs() < 1e-4,
            "layout must partition the unit square, got {total}"
        );
        for (_, r) in &rects {
            assert!(r.w > 0.0, "no zero/negative-width tiles");
        }
    }

    #[test]
    fn toggle_split_orientation_turns_a_row_into_a_stack_and_back() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert!(tree.toggle_split_orientation());
        // Same 50/50 ratios, now along the other axis: full-width stack.
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        assert!(approx(r1.w, 1.0) && approx(r1.h, 0.5) && approx(r1.y, 0.0));
        assert!(approx(r2.w, 1.0) && approx(r2.h, 0.5) && approx(r2.y, 0.5));
        assert!(tree.toggle_split_orientation());
        assert!(approx(rect_of(&tree, 1).w, 0.5) && approx(rect_of(&tree, 1).h, 1.0));
    }

    #[test]
    fn toggle_split_orientation_keeps_uneven_ratios() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert!(tree.move_divider(Direction::Right, 0.2)); // ratios [0.7, 0.3]
        assert!(tree.toggle_split_orientation());
        assert!(approx(rect_of(&tree, 1).h, 0.7));
        assert!(approx(rect_of(&tree, 2).h, 0.3));
    }

    #[test]
    fn toggle_split_orientation_noop_on_empty_or_lone_tile() {
        let mut tree = Tree::default();
        assert!(!tree.toggle_split_orientation());
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(!tree.toggle_split_orientation());
        // The lone tile still fills the unit square.
        assert!(approx(rect_of(&tree, 1).w, 1.0) && approx(rect_of(&tree, 1).h, 1.0));
    }

    #[test]
    fn toggle_split_orientation_in_a_flat_row_reorients_only_the_focused_pair() {
        // Three horizontal splits: ONE flat row with three children.
        // Toggling on tile 3 (last, so it pairs with its previous sibling 2)
        // must not restack the whole row: tile 1 keeps its slot, and 2/3
        // stack inside their old combined footprint (right two-thirds).
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        assert_eq!(tree.focused(), Some(TileId(3)));
        assert!(tree.toggle_split_orientation());
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        let r3 = rect_of(&tree, 3);
        assert!(
            approx(r1.x, 0.0) && approx(r1.w, 1.0 / 3.0) && approx(r1.h, 1.0),
            "tile 1 must keep its slot, got {r1:?}"
        );
        assert!(approx(r2.x, 1.0 / 3.0) && approx(r2.w, 2.0 / 3.0));
        assert!(approx(r3.x, 1.0 / 3.0) && approx(r3.w, 2.0 / 3.0));
        assert!(approx(r2.y, 0.0) && approx(r2.h, 0.5));
        assert!(approx(r3.y, 0.5) && approx(r3.h, 0.5));
    }

    #[test]
    fn toggle_split_orientation_pairs_a_middle_tile_with_its_next_sibling() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        tree.focus(TileId(2));
        assert!(tree.toggle_split_orientation());
        // 2 pairs rightward with 3 (same preference as resize); 1 untouched.
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        let r3 = rect_of(&tree, 3);
        assert!(approx(r1.x, 0.0) && approx(r1.w, 1.0 / 3.0) && approx(r1.h, 1.0));
        assert!(approx(r2.y, 0.0) && approx(r2.h, 0.5) && approx(r2.w, 2.0 / 3.0));
        assert!(approx(r3.y, 0.5) && approx(r3.h, 0.5) && approx(r3.w, 2.0 / 3.0));
    }

    #[test]
    fn toggle_split_orientation_flips_only_the_focused_tiles_parent() {
        // 1 | (2 over 3): outer horizontal split, inner vertical split.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Vertical); // wraps tile 2's slot
        assert_eq!(tree.focused(), Some(TileId(3)));
        assert!(tree.toggle_split_orientation());
        // Inner split is now horizontal: 2 and 3 sit side by side in the
        // right half; tile 1 (the outer split) is untouched at half width.
        let r1 = rect_of(&tree, 1);
        let r2 = rect_of(&tree, 2);
        let r3 = rect_of(&tree, 3);
        assert!(approx(r1.w, 0.5) && approx(r1.h, 1.0), "outer unchanged");
        assert!(approx(r2.h, 1.0) && approx(r2.w, 0.25));
        assert!(approx(r3.h, 1.0) && approx(r3.w, 0.25));
        assert!(approx(r2.x, 0.5) && approx(r3.x, 0.75));
    }

    // --- ID-based insertion -------------------------------------------

    #[test]
    fn insert_at_leaf_before_and_after_join_a_matching_orientation_split() {
        // [1 | 2]: inserting 3 after 1 lands between them; inserting 4
        // before 1 lands leftmost — all flat siblings, ratios equalized.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert!(tree.insert_at_leaf(TileId(1), TileId(3), Orientation::Horizontal, true));
        assert!(tree.insert_at_leaf(TileId(1), TileId(4), Orientation::Horizontal, false));
        let order: Vec<TileId> = tree.tiles();
        assert_eq!(
            order,
            vec![TileId(4), TileId(1), TileId(3), TileId(2)],
            "before lands left of the anchor, after lands right"
        );
        for id in [1, 2, 3, 4] {
            assert!(
                approx(rect_of(&tree, id).w, 0.25),
                "flat sibling insert equalizes ratios"
            );
        }
        assert_eq!(tree.focused(), Some(TileId(4)), "focus follows the insert");
    }

    #[test]
    fn insert_at_leaf_cross_orientation_wraps_the_anchor_on_the_chosen_side() {
        // [1 | 2]: a Vertical insert before 2 wraps 2's slot with the new
        // tile on top; after would put it below.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert!(tree.insert_at_leaf(TileId(2), TileId(3), Orientation::Vertical, false));
        let r2 = rect_of(&tree, 2);
        let r3 = rect_of(&tree, 3);
        assert!(approx(r3.x, 0.5) && approx(r3.y, 0.0) && approx(r3.h, 0.5));
        assert!(approx(r2.x, 0.5) && approx(r2.y, 0.5) && approx(r2.h, 0.5));
        assert!(
            approx(rect_of(&tree, 1).w, 0.5),
            "the anchor's sibling keeps its slot"
        );
    }

    #[test]
    fn insert_at_leaf_refuses_bad_ids_untouched() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        let before = tree.clone();
        assert!(
            !tree.insert_at_leaf(TileId(9), TileId(3), Orientation::Horizontal, true),
            "missing anchor"
        );
        assert!(
            !tree.insert_at_leaf(TileId(1), TileId(2), Orientation::Horizontal, true),
            "new id already present"
        );
        assert!(
            !tree.insert_at_leaf(TileId(1), TileId(1), Orientation::Horizontal, true),
            "anchor == new"
        );
        assert_eq!(tree, before, "refusals leave the tree untouched");
    }

    #[test]
    fn insert_at_leaf_exits_fullscreen_like_split_does() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.toggle_fullscreen();
        assert!(tree.fullscreen().is_some());
        assert!(tree.insert_at_leaf(TileId(1), TileId(3), Orientation::Vertical, true));
        assert_eq!(
            tree.fullscreen(),
            None,
            "an explicit layout operation trumps a stale fullscreen"
        );
    }

    // --- Stacks -------------------------------------------------------

    /// [1 | stack(2, 3 active)] built through `stack_after`.
    fn two_tiles_then_stack() -> Tree {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert!(tree.stack_after(TileId(2), TileId(3)));
        tree
    }

    #[test]
    fn stack_after_makes_a_two_member_stack_with_the_new_member_active_and_focused() {
        let tree = two_tiles_then_stack();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2), TileId(3)]);
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(3)]);
        assert_eq!(tree.focused(), Some(TileId(3)));
        assert_eq!(tree.stack_position(TileId(2)), Some((1, 2)));
        assert_eq!(tree.stack_position(TileId(3)), Some((2, 2)));
        assert_eq!(tree.stack_position(TileId(1)), None);
    }

    #[test]
    fn layout_emits_only_the_active_member_over_the_whole_slot() {
        let tree = two_tiles_then_stack();
        let rects = rects(&tree);
        assert_eq!(rects.len(), 2);
        assert!(
            rects.iter().all(|(id, _)| *id != TileId(2)),
            "hidden member has no rect"
        );
        let r3 = rect_of(&tree, 3);
        assert!(approx(r3.x, 0.5) && approx(r3.w, 0.5) && approx(r3.h, 1.0));
    }

    #[test]
    fn stack_after_appends_after_the_anchor_inside_an_existing_stack() {
        let mut tree = two_tiles_then_stack();
        assert!(tree.stack_after(TileId(2), TileId(4)));
        assert_eq!(
            tree.tiles(),
            vec![TileId(1), TileId(2), TileId(4), TileId(3)]
        );
        assert_eq!(tree.focused(), Some(TileId(4)));
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(4)]);
    }

    #[test]
    fn stack_after_refuses_a_missing_anchor_a_present_new_or_a_self_anchor() {
        let mut tree = two_tiles_then_stack();
        assert!(!tree.stack_after(TileId(9), TileId(4)));
        assert!(!tree.stack_after(TileId(2), TileId(1)));
        assert!(!tree.stack_after(TileId(2), TileId(2)));
        assert_eq!(
            tree.tiles(),
            vec![TileId(1), TileId(2), TileId(3)],
            "untouched"
        );
    }

    #[test]
    fn focusing_a_hidden_member_activates_it() {
        let mut tree = two_tiles_then_stack();
        assert!(tree.focus(TileId(2)));
        assert_eq!(tree.focused(), Some(TileId(2)));
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(2)]);
    }

    #[test]
    fn from_parts_heals_a_stack_rather_than_refusing_it() {
        // active out of range clamps to 0; a duplicated member is dropped
        // (the first claim, the leaf, wins); a stack left with one member
        // collapses to a leaf; a focused hidden member is activated.
        let root = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                Node::Leaf(TileId(1)),
                Node::Stack {
                    children: vec![TileId(1), TileId(2), TileId(2)],
                    active: 7,
                },
            ],
            ratios: vec![0.5, 0.5],
        };
        let tree = Tree::from_parts(Some(root), Some(TileId(2)), None).unwrap();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2)]);
        assert_eq!(
            tree.stack_position(TileId(2)),
            None,
            "one survivor collapses to a leaf"
        );

        let root = Node::Stack {
            children: vec![TileId(4), TileId(5), TileId(6)],
            active: 9,
        };
        let tree = Tree::from_parts(Some(root), Some(TileId(6)), None).unwrap();
        assert_eq!(
            tree.visible_tiles(),
            vec![TileId(6)],
            "focused member is activated on restore"
        );

        let root = Node::Stack {
            children: vec![TileId(4), TileId(5)],
            active: 9,
        };
        let tree = Tree::from_parts(Some(root), None, None).unwrap();
        assert_eq!(
            tree.visible_tiles(),
            vec![TileId(4)],
            "out-of-range active clamps to 0"
        );

        let root = Node::Split {
            orientation: Orientation::Vertical,
            children: vec![
                Node::Leaf(TileId(1)),
                Node::Stack {
                    children: vec![TileId(1)],
                    active: 0,
                },
            ],
            ratios: vec![0.5, 0.5],
        };
        let tree = Tree::from_parts(Some(root), None, None).unwrap();
        assert_eq!(
            tree.tiles(),
            vec![TileId(1)],
            "a stack emptied by healing vanishes and the split collapses"
        );
    }

    #[test]
    fn stack_step_cycles_with_wrap_and_a_count() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(tree.stack_after(TileId(1), TileId(2)));
        assert!(tree.stack_after(TileId(2), TileId(3))); // [1, 2, 3], 3 active
        assert!(tree.stack_step(1));
        assert_eq!(
            tree.focused(),
            Some(TileId(1)),
            "next past the end wraps to the first"
        );
        assert!(tree.stack_step(-1));
        assert_eq!(
            tree.focused(),
            Some(TileId(3)),
            "prev before the first wraps to the last"
        );
        assert!(tree.stack_step(2));
        assert_eq!(
            tree.focused(),
            Some(TileId(2)),
            "a count steps N with the same wrap"
        );
        assert_eq!(tree.visible_tiles(), vec![TileId(2)]);
    }

    #[test]
    fn stack_step_is_refused_on_a_plain_leaf() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(!tree.stack_step(1));
        assert_eq!(tree.focused(), Some(TileId(1)));
    }

    /// `contains`'s non-allocating core (`holds_anywhere`) must still see
    /// a stack's HIDDEN member, not just its painted one — `visible_
    /// tiles()` alone would miss tile 1 here — and must still refuse an
    /// id nothing holds.
    #[test]
    fn contains_finds_a_hidden_stack_member_and_refuses_an_absent_id() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(tree.stack_after(TileId(1), TileId(2)));
        assert_eq!(
            tree.visible_tiles(),
            vec![TileId(2)],
            "sanity: 1 is the hidden member"
        );
        assert!(
            tree.contains(TileId(1)),
            "a hidden stack member still counts"
        );
        assert!(tree.contains(TileId(2)));
        assert!(!tree.contains(TileId(99)), "an absent id is refused");
    }

    #[test]
    fn fullscreen_follows_a_cycle() {
        let mut tree = two_tiles_then_stack(); // 3 active + focused
        assert!(tree.toggle_fullscreen());
        assert_eq!(tree.fullscreen(), Some(TileId(3)));
        assert!(tree.stack_step(1));
        assert_eq!(tree.fullscreen(), Some(TileId(2)));
        assert_eq!(rects(&tree), vec![(TileId(2), Rect::UNIT)]);
    }

    #[test]
    fn closing_the_active_member_activates_and_focuses_the_next_one() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(9), Orientation::Horizontal); // [1 | 9]
        tree.focus(TileId(1));
        assert!(tree.stack_after(TileId(1), TileId(2)));
        assert!(tree.stack_after(TileId(2), TileId(3))); // [stack(1,2,3 active) | 9]
        tree.focus(TileId(2));
        tree.close();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(3), TileId(9)]);
        assert_eq!(
            tree.focused(),
            Some(TileId(3)),
            "the next member, not the tile after the stack"
        );
        assert_eq!(tree.visible_tiles(), vec![TileId(3), TileId(9)]);
    }

    #[test]
    fn closing_the_last_member_activates_the_previous_one() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(9), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(tree.stack_after(TileId(1), TileId(2)));
        assert!(tree.stack_after(TileId(2), TileId(3))); // 3 active, last
        tree.close();
        assert_eq!(tree.focused(), Some(TileId(2)));
        assert_eq!(tree.visible_tiles(), vec![TileId(2), TileId(9)]);
    }

    #[test]
    fn closing_one_of_two_members_collapses_the_stack_to_a_leaf() {
        let mut tree = two_tiles_then_stack(); // [1 | stack(2, 3 active)]
        tree.close();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2)]);
        assert_eq!(tree.stack_position(TileId(2)), None);
        assert_eq!(tree.focused(), Some(TileId(2)));
    }

    #[test]
    fn removing_a_hidden_member_leaves_the_active_one_alone() {
        let mut tree = two_tiles_then_stack(); // 3 active
        assert!(tree.remove(TileId(2)));
        assert_eq!(tree.focused(), Some(TileId(3)));
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(3)]);
    }

    #[test]
    fn move_direction_pops_a_member_out_beside_its_stack() {
        let mut tree = two_tiles_then_stack(); // [1 | stack(2, 3 active)]
        assert!(tree.move_direction(Direction::Right));
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2), TileId(3)]);
        assert_eq!(tree.stack_position(TileId(3)), None, "3 left the stack");
        assert_eq!(
            tree.stack_position(TileId(2)),
            None,
            "one survivor collapsed to a leaf"
        );
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(2), TileId(3)]);
        assert_eq!(tree.focused(), Some(TileId(3)));
        let r3 = rect_of(&tree, 3);
        let r2 = rect_of(&tree, 2);
        assert!(r3.x > r2.x, "popped out to the right of the stack it left");
    }

    #[test]
    fn popping_a_member_out_exits_fullscreen() {
        let mut tree = two_tiles_then_stack(); // 3 active + focused
        assert!(tree.toggle_fullscreen());
        assert!(tree.move_direction(Direction::Right));
        assert_eq!(tree.fullscreen(), None);
        assert_eq!(rects(&tree).len(), 3, "every tile painted again");
    }

    #[test]
    fn move_direction_on_a_plain_leaf_still_swaps() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(tree.move_direction(Direction::Right));
        assert_eq!(tree.tiles(), vec![TileId(2), TileId(1)]);
    }

    #[test]
    fn unstack_focused_pops_out_after_the_stack_in_the_given_orientation() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(tree.stack_after(TileId(1), TileId(2)));
        assert!(tree.stack_after(TileId(2), TileId(3))); // [1, 2, 3], 3 active
        tree.focus(TileId(2));
        assert!(tree.unstack_focused(Orientation::Vertical));
        assert_eq!(tree.stack_position(TileId(2)), None, "2 left the stack");
        // The stack it left behind is [1, 3] with 3 active — remove_leaf's
        // own "next member takes the popped slot" rule, unchanged by a
        // pop rather than a close.
        assert_eq!(tree.stack_position(TileId(1)), Some((1, 2)));
        assert_eq!(tree.stack_position(TileId(3)), Some((2, 2)));
        assert_eq!(tree.visible_tiles(), vec![TileId(3), TileId(2)]);
        assert!(
            rect_of(&tree, 2).y > rect_of(&tree, 3).y,
            "below the stack's painted member"
        );
        assert_eq!(tree.focused(), Some(TileId(2)));
        assert!(
            !tree.unstack_focused(Orientation::Vertical),
            "refused on a plain leaf"
        );
    }

    #[test]
    fn unstack_focused_exits_fullscreen() {
        let mut tree = two_tiles_then_stack(); // 3 active + focused
        assert!(tree.toggle_fullscreen());
        assert!(tree.unstack_focused(Orientation::Vertical));
        assert_eq!(tree.fullscreen(), None);
    }

    #[test]
    fn stacking_onto_a_fullscreen_tile_exits_fullscreen() {
        // A plain fullscreen leaf stacked onto: without the fix, `insert`
        // sets `active` to the new member's index before `set_focus` ever
        // calls `activate`, so `activate`'s outgoing-member check never
        // sees the old active id and `fullscreen` keeps pointing at a now
        // -hidden member — `Tree::layout` still paints it.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(tree.toggle_fullscreen());
        assert_eq!(tree.fullscreen(), Some(TileId(1)));
        assert!(tree.stack_after(TileId(1), TileId(2)));
        assert_eq!(tree.fullscreen(), None);
        assert!(tree.visible_tiles().contains(&TileId(2)));
    }

    #[test]
    fn split_beside_a_member_wraps_the_whole_stack() {
        let mut tree = two_tiles_then_stack(); // [1 | stack(2,3)]
        tree.split(TileId(4), Orientation::Vertical);
        // The stack and 4 share the right half, stacked vertically.
        assert_eq!(tree.stack_position(TileId(2)), Some((1, 2)));
        assert_eq!(tree.stack_position(TileId(3)), Some((2, 2)));
        let r3 = rect_of(&tree, 3);
        let r4 = rect_of(&tree, 4);
        assert!(approx(r3.x, 0.5) && approx(r4.x, 0.5) && r4.y > r3.y);
        assert_eq!(tree.focused(), Some(TileId(4)));
    }

    #[test]
    fn toggle_split_orientation_above_a_stack_flips_the_split() {
        let mut tree = two_tiles_then_stack(); // horizontal [1 | stack]
        assert!(tree.toggle_split_orientation());
        assert!(rect_of(&tree, 3).y > rect_of(&tree, 1).y, "now vertical");
        assert_eq!(
            tree.stack_position(TileId(3)),
            Some((2, 2)),
            "the stack itself is untouched"
        );
    }

    fn row_of_three() -> Tree {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        tree
    }

    #[test]
    fn pull_right_stacks_the_neighbour_behind_the_focused_tile() {
        let mut tree = row_of_three();
        tree.focus(TileId(1));
        assert!(tree.pull(Direction::Right));
        assert_eq!(tree.stack_position(TileId(1)), Some((1, 2)));
        assert_eq!(tree.stack_position(TileId(2)), Some((2, 2)));
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(3)]);
        assert_eq!(tree.focused(), Some(TileId(1)), "focus stays put");
        assert!(
            approx(rect_of(&tree, 1).w, 0.5),
            "2's room goes to the split"
        );
    }

    #[test]
    fn repeated_pulls_sweep_a_row_in_screen_order_either_way() {
        let mut tree = row_of_three();
        tree.focus(TileId(1));
        assert!(tree.pull(Direction::Right));
        assert!(tree.pull(Direction::Right));
        assert_eq!(
            tree.stack_members(TileId(1)),
            Some(vec![TileId(1), TileId(2), TileId(3)])
        );
        assert_eq!(tree.visible_tiles(), vec![TileId(1)]);

        let mut tree = row_of_three();
        tree.focus(TileId(3));
        assert!(tree.pull(Direction::Left));
        assert!(tree.pull(Direction::Left));
        assert_eq!(
            tree.stack_members(TileId(3)),
            Some(vec![TileId(1), TileId(2), TileId(3)])
        );
        assert_eq!(
            tree.stack_position(TileId(3)),
            Some((3, 3)),
            "3 stays active"
        );
        assert_eq!(tree.visible_tiles(), vec![TileId(3)]);
    }

    #[test]
    fn pull_takes_only_the_visible_member_of_a_stacked_neighbour() {
        let mut tree = two_tiles_then_stack(); // [1 | stack(2, 3 active)]
        tree.focus(TileId(1));
        assert!(tree.pull(Direction::Right));
        assert_eq!(
            tree.stack_members(TileId(1)),
            Some(vec![TileId(1), TileId(3)])
        );
        assert_eq!(tree.stack_position(TileId(2)), None, "2 is left on its own");
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(2)]);
    }

    #[test]
    fn pull_down_crosses_a_nested_split() {
        // [1 | (2 / 3)] with 2 focused: the tile below joins 2's slot.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Vertical);
        tree.focus(TileId(2));
        assert!(tree.pull(Direction::Down));
        assert_eq!(
            tree.stack_members(TileId(2)),
            Some(vec![TileId(2), TileId(3)])
        );
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(2)]);
        let r2 = rect_of(&tree, 2);
        assert!(approx(r2.x, 0.5) && approx(r2.h, 1.0), "the right half");
    }

    #[test]
    fn pull_refuses_an_edge_a_lone_tile_and_an_empty_tree() {
        let mut tree = Tree::default();
        assert!(!tree.pull(Direction::Right));
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(!tree.pull(Direction::Right));
        let mut tree = row_of_three(); // 3 focused, rightmost
        let before = tree.clone();
        assert!(!tree.pull(Direction::Right));
        assert!(!tree.pull(Direction::Up));
        assert_eq!(tree, before);
    }

    #[test]
    fn split_stack_splits_a_stack_in_its_own_slot() {
        let mut tree = two_tiles_then_stack(); // [1 | stack(2, 3 active)]
        assert!(tree.split_stack(Orientation::Vertical));
        assert_eq!(tree.stack_position(TileId(2)), None);
        assert_eq!(tree.stack_position(TileId(3)), None);
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(2), TileId(3)]);
        assert_eq!(tree.focused(), Some(TileId(3)));
        let (r1, r2, r3) = (rect_of(&tree, 1), rect_of(&tree, 2), rect_of(&tree, 3));
        assert!(approx(r1.w, 0.5), "1 keeps its half");
        assert!(
            approx(r2.x, 0.5) && approx(r3.x, 0.5),
            "members share the stack's slot"
        );
        assert!(
            approx(r2.h, 0.5) && approx(r3.y, 0.5),
            "equal, in member order"
        );
    }

    #[test]
    fn split_stack_gives_three_members_equal_room() {
        let mut tree = row_of_three();
        tree.focus(TileId(1));
        assert!(tree.pull(Direction::Right));
        assert!(tree.pull(Direction::Right));
        assert!(tree.split_stack(Orientation::Horizontal));
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(2), TileId(3)]);
        for id in 1..=3 {
            assert!(approx(rect_of(&tree, id).w, 1.0 / 3.0), "tile {id} width");
        }
    }

    #[test]
    fn split_stack_refuses_a_plain_tile() {
        let mut tree = Tree::default();
        assert!(!tree.split_stack(Orientation::Horizontal));
        let mut tree = row_of_three();
        let before = tree.clone();
        assert!(!tree.split_stack(Orientation::Horizontal));
        assert_eq!(tree, before);
    }

    #[test]
    fn split_stack_exits_fullscreen() {
        let mut tree = two_tiles_then_stack(); // 3 active + focused
        assert!(tree.toggle_fullscreen());
        assert!(tree.split_stack(Orientation::Horizontal));
        assert_eq!(tree.fullscreen(), None);
    }
}
