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

/// A rectangle in whatever space the caller works in. The tree computes
/// unit-space geometry; 1b-ui passes pixel bounds.
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

/// Stable name for one divider in a tree (drag-splitters task): the path
/// of child indices from the root down to the owning `Split`, plus the
/// index of the boundary's left/top child — the same `(index, index + 1)`
/// adjacent-pair convention [`Tree::move_divider`] operates in. An address
/// is captured at mouse-down and applied on every mouse-move, and the tree
/// can change in between (a keyboard split mid-drag, a session reload), so
/// it deliberately names *structure* rather than borrowing into it:
/// [`Tree::drag_divider`] re-validates the whole path on every application
/// and treats anything stale as a no-op, never a panic. Accepted limit of
/// name-by-structure (review round): a same-tree structural mutation
/// mid-drag can leave an address that still *validates* but names a
/// different boundary than the one grabbed (e.g. a split inserted before
/// it renumbers siblings). The shell cancels drags on every guarded path
/// (overlay open, workspace switch, fullscreen), so the remaining exposure
/// is a keyboard split/close raced against a held button — worst case a
/// benign misresize of a neighboring, still-clamped pair, never a panic or
/// an invariant break.
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
        Node::Split { children, .. } => {
            for child in children {
                swap_leaves(child, a, b);
            }
        }
    }
}

/// One workspace's layout: an i3-style split tree. Pure data — every verb
/// is a plain method, and [`Tree::layout`] is the only geometry authority
/// (rendering and hjkl navigation both consume it).
/// (`PartialEq` is derived for the dock-trees task: `session.rs` skips
/// writing a dock table when the whole `Dock` — tree included — still
/// equals `Dock::default()`, keeping pre-dock session files byte-identical.)
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

    pub fn contains(&self, id: TileId) -> bool {
        self.tiles().contains(&id)
    }

    /// Set focus to an existing tile (used by click-focus in 1b-ui).
    pub fn focus(&mut self, id: TileId) -> bool {
        if self.contains(id) {
            self.focused = Some(id);
            true
        } else {
            false
        }
    }

    /// Split the focused tile, placing `new` adjacent to it. On an empty
    /// tree this creates the first tile (the split verbs double as "open a
    /// tile"). Sibling ratios equalize on insert (documented v1
    /// simplification). Focus moves to the new tile. Splitting a non-empty
    /// tree exits fullscreen.
    ///
    /// Invariant (dock-trees review fix): split never discards the id it
    /// was given. Callers like `Workspace::move_to_dock` remove a tile
    /// from one tree and hand it to another's `split` — a split that
    /// silently returned would lose that tile forever.
    pub fn split(&mut self, new: TileId, orientation: Orientation) {
        match (self.root.take(), self.focused) {
            (None, _) => {
                self.root = Some(Node::Leaf(new));
            }
            (Some(root), Some(focused)) => {
                self.fullscreen = None;
                self.root = Some(split_at(root, focused, new, orientation));
            }
            // Degenerate: root present but nothing focused. Live verbs
            // keep focused Some whenever root is Some, and restore heals
            // it (`Dock::from_parts` / `Workspace::from_parts` refocus
            // the first tile) — but if some future path reconstructs this
            // state anyway, the never-discard invariant above must hold.
            // Recorded choice: insert at the first tree-order leaf
            // (equivalent to focus-then-split), rather than dropping the
            // id or panicking.
            (Some(root), None) => {
                let mut leaves = Vec::new();
                collect_leaves(&root, &mut leaves);
                match leaves.first().copied() {
                    Some(anchor) => {
                        self.fullscreen = None;
                        self.root = Some(split_at(root, anchor, new, orientation));
                    }
                    // Unreachable (every constructible root bottoms out in
                    // at least one leaf), but even then the id survives.
                    None => self.root = Some(Node::Leaf(new)),
                }
            }
        }
        self.focused = Some(new);
    }

    /// Close the focused tile. Single-child splits collapse; sibling ratios
    /// renormalize. Focus moves to the tree-order neighbor of the closed tile:
    /// the leaf that was immediately after it in the pre-close leaf order,
    /// or the previous one if the last leaf was closed. If no tiles remain,
    /// focus becomes None.
    pub fn close(&mut self) {
        self.remove_focused();
    }

    /// Remove the focused tile from the tree and return its id (dock-regions
    /// task: `dock::move_*` needs the removed id back so it can park it in a
    /// dock — this is `close` exactly, refocus rule and fullscreen-clearing
    /// included, except the id is handed to the caller instead of being
    /// forgotten; `close` is now a thin wrapper over this). Returns `None`
    /// on an empty tree. The focused-is-Some-whenever-root-is-Some
    /// invariant is preserved the same way `close` always preserved it:
    /// focus moves to the pre-removal tree-order neighbor, or `None` only
    /// when the tree emptied.
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

        // Remove the leaf from the tree (exactly one — see `remove_leaf`).
        let mut done = false;
        self.root = self
            .root
            .take()
            .and_then(|n| remove_leaf(n, focused, &mut done));

        // Focus the leaf that was immediately after the closed one, or the
        // previous one if the closed tile was last.
        let post_close_tiles = self.tiles();
        let focus_index = std::cmp::min(k, post_close_tiles.len().saturating_sub(1));
        self.focused = post_close_tiles.get(focus_index).copied();
        Some(focused)
    }

    /// Remove an arbitrary tile by id, wherever it is (dock-trees task:
    /// session healing's seam — a duplicate leaf claim in a dock tree is
    /// healed by removing that leaf, which `remove_focused` alone can't
    /// express without disturbing focus). Removes exactly ONE leaf — the
    /// first in tree order — even when a hostile file duplicated the id
    /// *within* one tree (review fix: an all-copies prune deleted both
    /// copies, losing the tile entirely instead of letting the first
    /// claim win; see `remove_leaf`). Same structural rules as
    /// `close`/`remove_focused`: single-child splits collapse, sibling
    /// ratios renormalize, fullscreen on the removed tile clears. Focus:
    /// if the removed tile *was* focused, the usual tree-order-neighbor
    /// refocus applies; otherwise the existing focus is untouched. Returns
    /// false (tree untouched) when `id` isn't a leaf here. Crate-private on
    /// purpose — live verbs move tiles through `remove_focused`/`split`,
    /// which keep the one-place-per-TileId invariant at the `Workspace`
    /// seam; this exists only for restore-time healing.
    ///
    /// (Replaces the dock-regions task's `replace_leaf`, which existed
    /// solely for the move-to-occupied-dock *swap* rule; dock trees killed
    /// that rule — a move now inserts into the dock's tree — leaving
    /// `replace_leaf` with no caller, so it was removed rather than kept
    /// as dead API.)
    pub(crate) fn remove(&mut self, id: TileId) -> bool {
        if !self.contains(id) {
            return false;
        }
        let prev_focused = self.focused;
        self.focused = Some(id);
        self.remove_focused();
        if let Some(prev) = prev_focused
            && prev != id
            && self.contains(prev)
        {
            self.focused = Some(prev);
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
                self.focused = Some(id);
                true
            }
            None => false,
        }
    }

    /// Swap the focused tile with its geometric neighbor. Focus stays on
    /// the same TileId, which now occupies the neighbor's position.
    pub fn move_direction(&mut self, dir: Direction) -> bool {
        let Some(focused) = self.focused else {
            return false;
        };
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
    /// either would drop below [`MIN_RATIO`] nothing changes and this
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

    /// Set the ratio pair at `address` so the divider lands under an
    /// absolute cursor position (drag-splitters task — the mouse
    /// counterpart of [`Tree::move_divider`], which stays byte-identical
    /// for the keyboard path). `bounds` is the rect this tree is laid out
    /// in (the same one the render pass gives [`Tree::layout`]) and
    /// `(x, y)` is the cursor in that space; the walk down `address.path`
    /// re-derives the owning split's sub-rect from the ratios exactly the
    /// way `layout_node` does, then picks the coordinate matching the
    /// split's orientation — the caller never needs to know which axis a
    /// divider moves along.
    ///
    /// Same invariants as `move_divider`, expressed absolutely instead of
    /// incrementally: only the adjacent pair `(index, index + 1)` changes,
    /// their sum is preserved (so every other sibling and the normalized
    /// total are untouched), and the new position clamps into
    /// `MIN_RATIO..=(pair total − MIN_RATIO)` — dragging past the clamp
    /// pins the divider at the clamp rather than failing, because during
    /// a live drag "stop at the limit" is the behavior the hand expects
    /// (the keyboard's discrete step rejects instead; both end at the same
    /// boundary).
    ///
    /// Returns `false` — tree untouched — for anything stale or
    /// degenerate: a path that runs through a leaf or off the end of a
    /// split's children (the layout changed mid-drag), a boundary index
    /// with no right-hand sibling, a non-finite cursor coordinate, a
    /// zero-extent bounds, or a pair whose total is already below
    /// `2 × MIN_RATIO` (constructible via `from_parts`, which renormalizes
    /// but doesn't enforce `MIN_RATIO`; a clamp range would be inverted).
    /// Also `false` — the review-round no-change contract — when the
    /// clamped result equals the ratio the pair already has (a repeated
    /// position, or a drag pinned at a clamp it's already sitting at):
    /// "true" strictly means "the layout changed", so the caller can key
    /// re-renders and dirty bookkeeping off it directly.
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
        // No-change detection (review fix): without it, every move pinned
        // at a clamp the divider is already sitting at would report true
        // and trigger a re-render for an identical layout. 1e-6 epsilon in
        // ratio space: far below any perceptible change (one pixel on an
        // 8K-wide split is ~1e-4 of it), far above f32 noise at this
        // scale — and the common no-op cases (same cursor position, same
        // clamp bound) reproduce bit-identical values anyway.
        if (new_a - ratios[i]).abs() < 1e-6 {
            return false;
        }
        ratios[i] = new_a;
        ratios[i + 1] = total - new_a;
        true
    }

    /// Insert `new` as `anchor`'s split sibling on a chosen side (tile-drag
    /// task — the drop verbs' insert primitive; `split` stays byte-identical
    /// for the keyboard path). Exactly `split`'s structural rules, anchored
    /// by id instead of by focus and with an explicit side: when `anchor`'s
    /// parent split already has `orientation`, `new` becomes a flat sibling
    /// immediately before/after it with ratios equalized (the same v1
    /// equalize-on-insert simplification `split` documents); otherwise the
    /// anchor leaf wraps into a new 2-way split of `orientation` occupying
    /// its old footprint, `new` on the requested side at 0.5/0.5. Focus
    /// moves to `new` (drop semantics: focus follows the moved tile) and —
    /// mirroring `split`'s rule that an explicit layout operation trumps a
    /// stale fullscreen — any fullscreen clears.
    ///
    /// Returns `false`, tree untouched, when `anchor` isn't a leaf here,
    /// `new` already is, or the two are the same id. Unlike `split`, a
    /// refusal can NOT lose the id being inserted, because refusal happens
    /// before anything is removed anywhere — callers (the `Workspace` drop
    /// verbs) verify the anchor exists *before* removing the dragged tile
    /// from its source tree, and fall back to a plain `split` if this
    /// somehow still refuses, so a tile can never vanish mid-move.
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
        self.focused = Some(new);
        true
    }

    /// Rename one leaf in place: the leaf holding `old` becomes `new`, with
    /// structure, ratios, and every other leaf untouched (tile-drag task —
    /// one half of a cross-tree center-drop swap; the other tree runs the
    /// mirror-image replace). Focus and fullscreen references *follow the
    /// position*, not the id: if `old` was focused (or fullscreen), the
    /// tile now in that spot — `new` — inherits it, so the source tree of a
    /// swap keeps its focus memory pointing at the same on-screen slot.
    /// Returns `false`, untouched, when `old` isn't a leaf here or `new`
    /// already is (the one-place-per-TileId invariant is the caller's to
    /// uphold across trees; within one tree this check enforces it).
    pub(crate) fn replace_tile(&mut self, old: TileId, new: TileId) -> bool {
        if old == new || !self.contains(old) || self.contains(new) {
            return false;
        }
        fn rename(node: &mut Node, old: TileId, new: TileId) {
            match node {
                Node::Leaf(id) => {
                    if *id == old {
                        *id = new;
                    }
                }
                Node::Split { children, .. } => {
                    for child in children {
                        rename(child, old, new);
                    }
                }
            }
        }
        if let Some(root) = &mut self.root {
            rename(root, old, new);
        }
        if self.focused == Some(old) {
            self.focused = Some(new);
        }
        if self.fullscreen == Some(old) {
            self.fullscreen = Some(new);
        }
        true
    }

    /// Swap two leaves of *this* tree in place (tile-drag task — the
    /// same-tree center-drop; [`Tree::move_direction`] uses the identical
    /// mechanism for its geometric-neighbor swap). Structure and ratios
    /// are untouched; focus is deliberately not moved here — both ids are
    /// still present, and which one the drop focuses is `Workspace`'s
    /// decision, not the tree's. Returns `false`, untouched, unless both
    /// ids are distinct leaves of this tree.
    pub(crate) fn swap_tiles(&mut self, a: TileId, b: TileId) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        if let Some(root) = &mut self.root {
            swap_leaves(root, a, b);
        }
        true
    }

    /// Clear any fullscreen state without touching focus (dock-regions
    /// task). Exists for `Workspace::move_to_dock`: moving a tile into a
    /// dock must exit fullscreen first — a fullscreen layout covers the
    /// whole surface and would hide the very dock the moved tile just
    /// landed in (and `remove_focused` only clears fullscreen when the
    /// *removed* tile held it, which a hostile restore can decouple).
    /// Mirrors `split`'s own "splitting a non-empty tree exits fullscreen"
    /// rule: an explicit layout operation trumps a stale fullscreen. Also
    /// the seam `Dock::from_parts` uses to enforce "dock trees never have
    /// fullscreen".
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
            let Some(i) = children.iter().position(|c| *c == Node::Leaf(focused)) else {
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

    /// Construct a `Tree` from raw parts (session restore, Task 3): the
    /// fields are private everywhere else, so this is the one place a
    /// hostile/corrupted session file's data gets turned back into a `Tree`,
    /// with validation instead of blind trust.
    ///
    /// Structural invalidity in `root` is `Err` (the shape genuinely can't
    /// be interpreted as a layout): any `Split` with fewer than 2 children,
    /// a `ratios` vec whose length doesn't match `children`, or any ratio
    /// that is non-finite (NaN/infinite) or non-positive. A structurally
    /// valid split's ratios are then renormalized to sum to exactly 1.0 —
    /// this heals small drift (e.g. from a TOML float round-trip) rather
    /// than rejecting it, since the brief only asks non-positive/NaN ratios
    /// to be rejected.
    ///
    /// `focused`/`fullscreen` are a different kind of problem: a `TileId`
    /// that doesn't exist in `root` as a leaf. That's harmless (nothing
    /// downstream trusts them beyond "is this id currently a leaf",
    /// per `contains`), so it's healed by clearing to `None` rather than
    /// rejecting the whole tree over a dangling reference.
    pub fn from_parts(
        root: Option<Node>,
        focused: Option<TileId>,
        fullscreen: Option<TileId>,
    ) -> Result<Tree, String> {
        let root = root.map(validate_node).transpose()?;

        let mut leaves = Vec::new();
        if let Some(root) = &root {
            collect_leaves(root, &mut leaves);
        }
        let focused = focused.filter(|id| leaves.contains(id));
        let fullscreen = fullscreen.filter(|id| leaves.contains(id));

        Ok(Tree {
            root,
            focused,
            fullscreen,
        })
    }
}

/// Recursively validate one `Node` for [`Tree::from_parts`]: every `Split`
/// must have >= 2 children with a matching-length `ratios` vec of finite,
/// positive values; on success the ratios are renormalized to sum to 1.0.
fn validate_node(node: Node) -> Result<Node, String> {
    match node {
        Node::Leaf(id) => Ok(Node::Leaf(id)),
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
            // Every ratio was just checked finite and > 0, so the sum is
            // finite and > 0 too — safe to divide by.
            let sum: f32 = ratios.iter().sum();
            let ratios: Vec<f32> = ratios.iter().map(|r| r / sum).collect();
            let children = children
                .into_iter()
                .map(validate_node)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Node::Split {
                orientation,
                children,
                ratios,
            })
        }
    }
}

fn collect_leaves(node: &Node, out: &mut Vec<TileId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { children, .. } => {
            for child in children {
                collect_leaves(child, out);
            }
        }
    }
}

fn split_at(node: Node, focused: TileId, new: TileId, orientation: Orientation) -> Node {
    match node {
        Node::Leaf(id) if id == focused => Node::Split {
            orientation,
            children: vec![Node::Leaf(id), Node::Leaf(new)],
            ratios: vec![0.5, 0.5],
        },
        leaf @ Node::Leaf(_) => leaf,
        Node::Split {
            orientation: existing,
            mut children,
            ratios,
        } => {
            if existing == orientation {
                // Same orientation and the focused leaf is a direct child:
                // insert as a sibling right after it, equalizing ratios.
                if let Some(ix) = children
                    .iter()
                    .position(|c| matches!(c, Node::Leaf(id) if *id == focused))
                {
                    children.insert(ix + 1, Node::Leaf(new));
                    let n = children.len() as f32;
                    let ratios = vec![1.0 / n; children.len()];
                    return Node::Split {
                        orientation: existing,
                        children,
                        ratios,
                    };
                }
            }
            let children = children
                .into_iter()
                .map(|c| split_at(c, focused, new, orientation))
                .collect();
            Node::Split {
                orientation: existing,
                children,
                ratios,
            }
        }
    }
}

/// [`split_at`]'s side-aware sibling (tile-drag task, backing
/// [`Tree::insert_at_leaf`]): identical structural rules — flat sibling
/// insert with equalized ratios when the anchor's parent split already has
/// `orientation`, otherwise wrap the anchor leaf into a new 0.5/0.5 split —
/// except the anchor is named by id and `after` picks which side `new`
/// lands on (`split_at` always inserts after the focused leaf).
fn insert_beside(
    node: Node,
    anchor: TileId,
    new: TileId,
    orientation: Orientation,
    after: bool,
) -> Node {
    match node {
        Node::Leaf(id) if id == anchor => {
            let children = if after {
                vec![Node::Leaf(id), Node::Leaf(new)]
            } else {
                vec![Node::Leaf(new), Node::Leaf(id)]
            };
            Node::Split {
                orientation,
                children,
                ratios: vec![0.5, 0.5],
            }
        }
        leaf @ Node::Leaf(_) => leaf,
        Node::Split {
            orientation: existing,
            mut children,
            ratios,
        } => {
            if existing == orientation
                && let Some(ix) = children
                    .iter()
                    .position(|c| matches!(c, Node::Leaf(id) if *id == anchor))
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

/// Remove exactly ONE leaf holding `target` — the first in tree order —
/// rebuilding the node (returns `None` when the removal emptied it).
/// `done` threads "already removed one" through the recursion. One leaf,
/// not all (dock-trees review fix): a live tree never holds duplicate ids,
/// so for every live caller this is the same operation as before — but
/// session healing removes duplicate leaves from hostile dock trees one
/// claim at a time, and an all-matches prune there deleted every copy of
/// an id duplicated *within* one tree, losing the tile entirely instead of
/// letting the first claim win.
fn remove_leaf(node: Node, target: TileId, done: &mut bool) -> Option<Node> {
    match node {
        Node::Leaf(id) if id == target && !*done => {
            *done = true;
            None
        }
        leaf @ Node::Leaf(_) => Some(leaf),
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

fn layout_node(node: &Node, rect: Rect, out: &mut Vec<(TileId, Rect)>) {
    match node {
        Node::Leaf(id) => out.push((*id, rect)),
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
            Node::Leaf(_) => unreachable!("path indexes into splits"),
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
        // Resize comes in Task 3; emulate asymmetry by focusing and testing
        // overlap tie-break on the symmetric grid instead:
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
        tree.toggle_fullscreen(); // Task 3 provides this; here it gates layout
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

    // --- drag_divider (drag-splitters task) -----------------------------

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
        // Using the 2x2 grid fixture: tiles in tree order are [1, 4, 2, 3]
        // Close tile 1 (at index 0): should focus tile 4 (at new index 0, which was 1)
        // This shows the neighbor rule applies to nested leaves where old behavior
        // would have also focused the first leaf, but now we follow tree order.
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

    // --- remove_focused / remove (dock-regions + dock-trees tasks) ------

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
        // Review fix: a hostile session file can duplicate an id WITHIN
        // one tree (node_from_toml has no duplicate check). remove() must
        // prune exactly one leaf — first in tree order — so healing's
        // first-claim-wins leaves one copy alive instead of deleting both.
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
        // Review fix: the old degenerate arm returned without inserting,
        // so a caller that had already removed the tile from another tree
        // (move_to_dock's move-back) lost it forever. The invariant is
        // "split never discards the id it was given": with no focus, the
        // first tree-order leaf anchors the insert and focus lands on the
        // new tile as usual. (Reachable only through a reconstructed
        // root-Some/focused-None tree — Tree::from_parts heals a dangling
        // focused to None.)
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

    // --- Tree::from_parts (Task 3: session restore reconstruction) -----

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
        // ctrl+v three times: ONE flat horizontal split with three children.
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

    // --- insert_at_leaf / replace_tile / swap_tiles (tile-drag task) ----

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

    #[test]
    fn replace_tile_renames_the_leaf_and_remaps_focus_and_fullscreen() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        let r1_before = rect_of(&tree, 1);
        tree.toggle_fullscreen(); // fullscreen on 1
        assert!(tree.replace_tile(TileId(1), TileId(9)));
        assert!(!tree.contains(TileId(1)));
        assert_eq!(
            tree.fullscreen(),
            Some(TileId(9)),
            "a fullscreen reference follows the renamed slot"
        );
        // Drop fullscreen to compare the underlying slot geometry.
        tree.exit_fullscreen();
        assert_eq!(rect_of(&tree, 9), r1_before, "same slot, new id");
        assert_eq!(tree.focused(), Some(TileId(9)), "focus follows the slot");
    }

    #[test]
    fn replace_tile_refuses_bad_ids_untouched() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        let before = tree.clone();
        assert!(!tree.replace_tile(TileId(9), TileId(3)), "old missing");
        assert!(!tree.replace_tile(TileId(1), TileId(2)), "new present");
        assert!(!tree.replace_tile(TileId(1), TileId(1)), "old == new");
        assert_eq!(tree, before);
    }

    #[test]
    fn swap_tiles_swaps_positions_without_touching_focus() {
        let mut tree = grid();
        let r1 = rect_of(&tree, 1);
        let r3 = rect_of(&tree, 3);
        let focused = tree.focused();
        assert!(tree.swap_tiles(TileId(1), TileId(3)));
        assert_eq!(rect_of(&tree, 1), r3);
        assert_eq!(rect_of(&tree, 3), r1);
        assert_eq!(tree.focused(), focused, "swap alone never moves focus");
        assert!(!tree.swap_tiles(TileId(1), TileId(1)), "self-swap refused");
        assert!(!tree.swap_tiles(TileId(1), TileId(99)), "missing id");
    }
}
