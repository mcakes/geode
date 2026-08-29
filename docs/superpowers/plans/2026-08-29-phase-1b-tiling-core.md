# Geode Phase 1b-core (Tiling Tree) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The pure-logic tiling core: the i3-style split tree (split/close/focus/move/resize/fullscreen, geometric layout computation) and the global workspace set, plus the pure dispatcher mapping shell actions onto tiling verbs — all window-free tested.

**Architecture:** Per the recorded DockArea decision: our own tree is the single source of truth for layout; Phase 1b-ui renders `Tree::layout` output with gpui-component primitives. Geometry is computed in unit space (`Rect::UNIT`), so the same function drives both directional navigation (this plan) and pixel layout (1b-ui multiplies through real bounds). Everything lives in `geode-shell::tiling`; no gpui anywhere in this plan.

**Tech Stack:** Rust, std only. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-28-geode-foundation-design.md` — §3.1 (tiling verbs), §3.6 (workspaces are app-global), §10.3 (tiling tree is pure logic testable without a window).

## Global Constraints

- No gpui; no new dependencies (dev or regular).
- Crate placement: everything in `geode-shell` (`src/tiling/`). geode-shell continues to depend only on geode-core.
- Pure logic: no I/O, no clocks, no randomness. Same inputs, same tree.
- Panics must not be reachable from user input. Internal-invariant `unreachable!`/`expect` on programmer-guaranteed paths is acceptable with a comment naming the invariant.
- Geometry is `f32` in unit space with `const EPS: f32 = 1e-3` for edge-adjacency comparisons; every comparison against EPS is a deliberate choice, not a float-equality accident.
- All commits end with the project's standard co-author trailer (as in existing git history).
- TDD: tests first, watched failing, then implemented.

## Documented v1 simplifications (encode in doc comments, don't fight them)

- Splitting equalizes sibling ratios (custom ratios within that split reset to 1/n on insert). Custom ratios survive splits *elsewhere* in the tree.
- `split` on an empty tree creates the first tile (bootstrap semantic: the split verbs are also "open a tile").
- After `close`, focus jumps to the first leaf of the tree (leftmost/topmost), not the geometric neighbor.
- `move_direction` swaps the focused tile with its geometric neighbor (no cross-container re-parenting).
- No focus wrap at edges; a failed navigation returns `false` and changes nothing.
- Tabbed/stacked container modes (spec §3.1) are deferred to a later phase; the `Node` enum will grow a variant then.

## File Structure

```
crates/geode-shell/src/tiling/mod.rs          module wiring + re-exports
crates/geode-shell/src/tiling/tree.rs         TileId, Orientation, Direction, Rect, Node, Tree + all verbs
crates/geode-shell/src/tiling/workspaces.rs   Workspaces (global set, spec §3.6) + apply_workspace_action
crates/geode-shell/tests/tiling_integration.rs  keymap → matcher → action → tree, end to end
```

`tree.rs` will be the largest pure-logic file in the crate (~450 lines with tests, across Tasks 1–3). That is intended — the tree's verbs are one cohesive unit; do not split the file.

---

### Task 1: Tree structure, split, close, and layout

**Files:**
- Modify: `crates/geode-shell/src/lib.rs` (declare module)
- Create: `crates/geode-shell/src/tiling/mod.rs`
- Create: `crates/geode-shell/src/tiling/tree.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (used by Tasks 2–4 and Phase 1b-ui):
  - `TileId(pub u64)` (Copy, Ord, Hash)
  - `Orientation { Horizontal, Vertical }` — Horizontal: children left→right; Vertical: top→bottom
  - `Direction { Left, Right, Up, Down }` with `orientation(self) -> Orientation`
  - `Rect { x, y, w, h: f32 }` with `Rect::UNIT`
  - `Node { Leaf(TileId), Split { orientation, children: Vec<Node>, ratios: Vec<f32> } }` (ratios sum to 1.0, len == children.len())
  - `Tree` (Default = empty) with: `is_empty()`, `focused() -> Option<TileId>`, `fullscreen() -> Option<TileId>`, `root() -> Option<&Node>`, `tiles() -> Vec<TileId>`, `contains(TileId) -> bool`, `focus(TileId) -> bool`, `split(new: TileId, Orientation)`, `close()`, `layout(bounds: Rect) -> Vec<(TileId, Rect)>`

- [ ] **Step 1: Declare modules**

In `crates/geode-shell/src/lib.rs` add (keeping alphabetical order with `actions`, `defaults`, `keymap`):
```rust
pub mod tiling;
```

`crates/geode-shell/src/tiling/mod.rs`:
```rust
//! Tiling window-management core (spec §3.1, §3.6): the pure tree that is
//! the single source of truth for workspace layout. Rendering (Phase 1b-ui)
//! consumes [`Tree::layout`]; directional navigation uses the same geometry,
//! so what you see is what hjkl navigates. No gpui here (spec §10.3).

mod tree;

pub use tree::{Direction, Node, Orientation, Rect, TileId, Tree};
```

- [ ] **Step 2: Write the failing tests**

Create `crates/geode-shell/src/tiling/tree.rs` containing only the test module for now:
```rust
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
            assert!(approx(rect_of(&tree, id).w, 1.0 / 3.0), "tile {id} not a third wide");
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
        assert!(approx(r.w, 1.0) && approx(r.h, 1.0), "collapse must restore full size");
        assert!(matches!(tree.root(), Some(Node::Leaf(_))), "single-child split must collapse");
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
        let bounds = Rect { x: 10.0, y: 20.0, w: 100.0, h: 50.0 };
        let out = tree.layout(bounds);
        let r2 = out.iter().find(|(t, _)| *t == TileId(2)).unwrap().1;
        assert!(approx(r2.x, 60.0) && approx(r2.y, 20.0) && approx(r2.w, 50.0) && approx(r2.h, 50.0));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-shell tiling`
Expected: compile error — the types are not defined.

- [ ] **Step 4: Implement**

Insert above the tests in `tree.rs`:
```rust
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
    pub const UNIT: Rect = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    pub(crate) fn right(&self) -> f32 {
        self.x + self.w
    }

    pub(crate) fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Edge-adjacency tolerance for unit-space geometry comparisons.
pub(crate) const EPS: f32 = 1e-3;

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

/// One workspace's layout: an i3-style split tree. Pure data — every verb
/// is a plain method, and [`Tree::layout`] is the only geometry authority
/// (rendering and hjkl navigation both consume it).
#[derive(Debug, Clone, Default)]
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
    /// simplification). Focus moves to the new tile.
    pub fn split(&mut self, new: TileId, orientation: Orientation) {
        match (self.root.take(), self.focused) {
            (None, _) => {
                self.root = Some(Node::Leaf(new));
            }
            (Some(root), Some(focused)) => {
                self.root = Some(split_at(root, focused, new, orientation));
            }
            // Invariant: focused is Some whenever root is Some.
            (Some(root), None) => {
                self.root = Some(root);
                return;
            }
        }
        self.focused = Some(new);
    }

    /// Close the focused tile. Single-child splits collapse; sibling ratios
    /// renormalize. Focus falls back to the tree's first leaf (documented
    /// v1 simplification).
    pub fn close(&mut self) {
        let Some(focused) = self.focused else { return };
        if self.fullscreen == Some(focused) {
            self.fullscreen = None;
        }
        self.root = self.root.take().and_then(|n| remove_leaf(n, focused));
        self.focused = self.root.as_ref().map(first_leaf);
    }

    /// Compute every visible tile's rectangle within `bounds`. When a tile
    /// is fullscreen it is the only visible tile and fills `bounds`.
    pub fn layout(&self, bounds: Rect) -> Vec<(TileId, Rect)> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        if let Some(fs) = self.fullscreen {
            if self.contains(fs) {
                return vec![(fs, bounds)];
            }
        }
        let mut out = Vec::new();
        layout_node(root, bounds, &mut out);
        out
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

fn first_leaf(node: &Node) -> TileId {
    match node {
        Node::Leaf(id) => *id,
        Node::Split { children, .. } => first_leaf(&children[0]),
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
        Node::Split { orientation: existing, mut children, ratios } => {
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
                    return Node::Split { orientation: existing, children, ratios };
                }
            }
            let children = children
                .into_iter()
                .map(|c| split_at(c, focused, new, orientation))
                .collect();
            Node::Split { orientation: existing, children, ratios }
        }
    }
}

fn remove_leaf(node: Node, target: TileId) -> Option<Node> {
    match node {
        Node::Leaf(id) if id == target => None,
        leaf @ Node::Leaf(_) => Some(leaf),
        Node::Split { orientation, children, ratios } => {
            let mut kept_children = Vec::new();
            let mut kept_ratios = Vec::new();
            let mut removed = false;
            for (child, ratio) in children.into_iter().zip(ratios) {
                match remove_leaf(child, target) {
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
        Node::Split { orientation, children, ratios } => {
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p geode-shell tiling`
Expected: 12 tests PASS.

- [ ] **Step 6: Lint, format, commit**

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-shell
git commit -m "feat: tiling tree with split, close, and unit-space layout"
```

---

### Task 2: Geometric navigation — directional focus and move

**Files:**
- Modify: `crates/geode-shell/src/tiling/tree.rs`

**Interfaces:**
- Consumes: Task 1's `Tree`, `Rect`, `Direction`, `EPS`.
- Produces (used by Task 4 and 1b-ui): `Tree::focus_direction(Direction) -> bool`, `Tree::move_direction(Direction) -> bool`, `Tree::neighbor(Direction) -> Option<TileId>` (public — 1b-ui uses it for hover hints later).

Semantics (encode in tests): a neighbor is a tile whose facing edge touches the focused tile's edge in that direction (within `EPS`) with positive perpendicular overlap; the nearest such edge wins, ties broken by larger overlap. No wrap. `move_direction` swaps the two leaves' ids; focus stays on the moved tile (which now sits in the new position). While fullscreen, navigation finds no neighbor (layout shows one tile) and returns false.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `tests` module in `tree.rs`:
```rust
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
        assert!(approx(r1.x, 0.5) && approx(r1.y, 0.0), "1 moved to top-right");
        assert!(approx(r2.x, 0.0) && approx(r2.y, 0.0), "2 moved to top-left");
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
```

Note: `fullscreen_blocks_navigation` requires Task 3's `toggle_fullscreen`; add the test now with the others but expect it to fail-to-compile until Task 3 — OR (preferred, to keep this task self-contained) comment it out with a `// Task 3:` marker and un-comment it in Task 3. Choose the comment-out approach and note it in the commit message.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p geode-shell tiling`
Expected: compile error — `focus_direction`, `move_direction` not defined (with the fullscreen test commented out).

- [ ] **Step 3: Implement**

Add to `impl Tree` in `tree.rs`:
```rust
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
```

Add the free functions:
```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-shell tiling`
Expected: prior 12 + 6 new tests PASS (fullscreen test still commented).

- [ ] **Step 5: Lint, format, commit**

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-shell
git commit -m "feat: geometric directional focus and move for the tiling tree"
```
(Note in the commit body that `fullscreen_blocks_navigation` is committed commented-out pending Task 3.)

---

### Task 3: Resize and fullscreen

**Files:**
- Modify: `crates/geode-shell/src/tiling/tree.rs`

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces (used by Task 4 and 1b-ui): `Tree::resize(Direction, delta: f32) -> bool` (grow the focused tile's edge toward `dir` by `delta` of the containing split, taking from the adjacent sibling; clamped so no ratio drops below `MIN_RATIO` = 0.05; negative `delta` shrinks), `Tree::toggle_fullscreen() -> bool`.

- [ ] **Step 1: Write the failing tests**

Un-comment `fullscreen_blocks_navigation` from Task 2 and append inside the `tests` module:
```rust
    #[test]
    fn resize_transfers_ratio_to_neighbor() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(tree.resize(Direction::Right, 0.1)); // grow 1 rightward
        assert!(approx(rect_of(&tree, 1).w, 0.6));
        assert!(approx(rect_of(&tree, 2).w, 0.4));
        assert!(approx(rect_of(&tree, 2).x, 0.6));
    }

    #[test]
    fn resize_shrinks_with_negative_delta() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(tree.resize(Direction::Right, -0.1));
        assert!(approx(rect_of(&tree, 1).w, 0.4));
    }

    #[test]
    fn resize_clamps_at_min_ratio() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(!tree.resize(Direction::Right, 0.9), "would push neighbor below MIN_RATIO");
        assert!(approx(rect_of(&tree, 1).w, 0.5), "failed resize must change nothing");
    }

    #[test]
    fn resize_finds_matching_orientation_ancestor() {
        let mut tree = grid(); // focused: 1 (top-left)
        // Up/Down resizing of tile 1 must adjust the left column's inner
        // vertical split (1 over 4), not the outer horizontal one.
        assert!(tree.resize(Direction::Down, 0.2));
        assert!(approx(rect_of(&tree, 1).h, 0.7));
        assert!(approx(rect_of(&tree, 4).h, 0.3));
        // And Left/Right resizing adjusts the outer horizontal split.
        assert!(tree.resize(Direction::Right, 0.1));
        assert!(approx(rect_of(&tree, 1).w, 0.6));
        assert!(approx(rect_of(&tree, 2).w, 0.4));
    }

    #[test]
    fn resize_with_no_matching_split_is_noop() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(!tree.resize(Direction::Right, 0.1), "single tile has nothing to resize");
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p geode-shell tiling`
Expected: compile error — `resize`, `toggle_fullscreen` not defined.

- [ ] **Step 3: Implement**

Add to `tree.rs`:
```rust
/// Smallest fraction any split child may occupy.
pub const MIN_RATIO: f32 = 0.05;
```

Add to `impl Tree`:
```rust
    /// Grow the focused tile's edge toward `dir` by `delta` (a fraction of
    /// the containing split), taking the space from the adjacent sibling in
    /// that direction (or the opposite sibling when at the split's edge).
    /// Negative `delta` shrinks. Returns false (changing nothing) when no
    /// ancestor split matches the direction's orientation or the transfer
    /// would push either ratio below [`MIN_RATIO`].
    pub fn resize(&mut self, dir: Direction, delta: f32) -> bool {
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
            let Node::Split { orientation, ratios, .. } = node else {
                continue;
            };
            if *orientation != dir.orientation() {
                continue;
            }
            let i = path[depth];
            let j = match dir {
                Direction::Right | Direction::Down => {
                    if i + 1 < ratios.len() {
                        i + 1
                    } else if i > 0 {
                        i - 1
                    } else {
                        continue;
                    }
                }
                Direction::Left | Direction::Up => {
                    if i > 0 {
                        i - 1
                    } else if i + 1 < ratios.len() {
                        i + 1
                    } else {
                        continue;
                    }
                }
            };
            let grown = ratios[i] + delta;
            let shrunk = ratios[j] - delta;
            if grown < MIN_RATIO || shrunk < MIN_RATIO {
                return false;
            }
            ratios[i] = grown;
            ratios[j] = shrunk;
            return true;
        }
        false
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
```

Add the free functions:
```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p geode-shell tiling`
Expected: all tiling tests PASS (Task 1's 12 + Task 2's 6 + the re-enabled fullscreen-navigation test + 7 new = 26).

- [ ] **Step 5: Lint, format, commit**

Run: `cargo clippy -p geode-shell --all-targets -- -D warnings && cargo fmt`

```bash
git add crates/geode-shell
git commit -m "feat: tiling resize with ratio clamping and fullscreen toggle"
```

---

### Task 4: Workspaces and the action dispatcher

**Files:**
- Modify: `crates/geode-shell/src/tiling/mod.rs` (declare module)
- Create: `crates/geode-shell/src/tiling/workspaces.rs`
- Create: `crates/geode-shell/tests/tiling_integration.rs`

**Interfaces:**
- Consumes: Tasks 1–3; `ActionId` from `crate::actions`; (integration test) `defaults`, `keymap` from Phase 1a.
- Produces (used by 1b-ui as its entire mutation surface):
  - `Workspaces` (Default) with `new()`, `active_index() -> u8`, `active() -> &Tree`, `active_mut() -> &mut Tree`, `switch(n: u8) -> bool` (1..=9, creates lazily), `alloc_tile() -> TileId` (globally unique across workspaces), `non_empty_indices() -> Vec<u8>`
  - `apply_workspace_action(&mut Workspaces, &ActionId) -> bool` — true when the action was recognized as a workspace action (even if it changed nothing, e.g. focus at an edge); false means "not mine, try another handler"

- [ ] **Step 1: Declare the module**

In `tiling/mod.rs` add:
```rust
mod workspaces;

pub use workspaces::{apply_workspace_action, Workspaces};
```

- [ ] **Step 2: Write the failing tests**

`crates/geode-shell/src/tiling/workspaces.rs`:
```rust
use super::tree::{Direction, Orientation, TileId, Tree};
use crate::actions::ActionId;
use std::collections::BTreeMap;

#[cfg(test)]
mod tests {
    use super::*;

    fn act(s: &str) -> ActionId {
        ActionId(s.to_string())
    }

    #[test]
    fn starts_on_workspace_one_empty() {
        let ws = Workspaces::new();
        assert_eq!(ws.active_index(), 1);
        assert!(ws.active().is_empty());
        assert_eq!(ws.non_empty_indices(), Vec::<u8>::new());
    }

    #[test]
    fn switch_creates_lazily_and_validates_range() {
        let mut ws = Workspaces::new();
        assert!(ws.switch(5));
        assert_eq!(ws.active_index(), 5);
        assert!(ws.active().is_empty());
        assert!(!ws.switch(0));
        assert!(!ws.switch(10));
        assert_eq!(ws.active_index(), 5);
    }

    #[test]
    fn tile_ids_are_unique_across_workspaces() {
        let mut ws = Workspaces::new();
        let a = ws.alloc_tile();
        ws.switch(2);
        let b = ws.alloc_tile();
        assert_ne!(a, b);
    }

    #[test]
    fn split_actions_create_and_arrange_tiles() {
        let mut ws = Workspaces::new();
        assert!(apply_workspace_action(&mut ws, &act("workspace::split_horizontal")));
        assert!(apply_workspace_action(&mut ws, &act("workspace::split_horizontal")));
        assert_eq!(ws.active().tiles().len(), 2);
        let rects = ws.active().layout(super::super::tree::Rect::UNIT);
        assert!((rects[0].1.w - 0.5).abs() < 1e-4);
    }

    #[test]
    fn focus_and_fullscreen_and_close_actions_route() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_horizontal"));
        apply_workspace_action(&mut ws, &act("workspace::split_horizontal"));
        assert!(apply_workspace_action(&mut ws, &act("workspace::focus_left")));
        let left = ws.active().focused().unwrap();
        assert!(apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile")));
        assert_eq!(ws.active().fullscreen(), Some(left));
        assert!(apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile")));
        assert_eq!(ws.active().fullscreen(), None);
        assert!(apply_workspace_action(&mut ws, &act("workspace::close_tile")));
        assert_eq!(ws.active().tiles().len(), 1);
    }

    #[test]
    fn switch_actions_parse_their_index() {
        let mut ws = Workspaces::new();
        assert!(apply_workspace_action(&mut ws, &act("workspace::switch_3")));
        assert_eq!(ws.active_index(), 3);
        assert!(!apply_workspace_action(&mut ws, &act("workspace::switch_zzz")));
    }

    #[test]
    fn unrecognized_actions_are_not_claimed() {
        let mut ws = Workspaces::new();
        assert!(!apply_workspace_action(&mut ws, &act("palette::toggle")));
        assert!(!apply_workspace_action(&mut ws, &act("blotter::group")));
    }

    #[test]
    fn edge_focus_is_claimed_but_changes_nothing() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_horizontal"));
        let focused = ws.active().focused();
        assert!(apply_workspace_action(&mut ws, &act("workspace::focus_left")));
        assert_eq!(ws.active().focused(), focused);
    }
}
```

`crates/geode-shell/tests/tiling_integration.rs`:
```rust
//! Keyboard to layout, end to end: builtin keymap → Matcher → ActionId →
//! apply_workspace_action → Tree geometry. This is the exact pipeline
//! Phase 1b-ui wires into gpui's key handler.

use geode_shell::actions::ActionRegistry;
use geode_shell::defaults;
use geode_shell::keymap::{build_keymap, parse_keystroke, KeyContext, MatchResult, Matcher};
use geode_shell::tiling::{apply_workspace_action, Direction, Rect, Workspaces};
use geode_core::config::LayerDoc;

#[test]
fn keystrokes_drive_the_tiling_tree() {
    let mut registry = ActionRegistry::default();
    defaults::register_builtin_actions(&mut registry);
    let doc = LayerDoc::builtin("keymap", defaults::BUILTIN_KEYMAP).unwrap();
    let mod_alias = defaults::default_mod();
    let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
    assert!(diags.is_empty(), "{diags:?}");

    let stack = vec![KeyContext::new("workspace")];
    let mut matcher = Matcher::default();
    let mut ws = Workspaces::new();
    let mut press = |matcher: &mut Matcher, ws: &mut Workspaces, key: &str| {
        let ks = parse_keystroke(key, mod_alias).unwrap();
        match matcher.press(&keymap, ks, &stack) {
            MatchResult::Matched(action) => {
                assert!(apply_workspace_action(ws, &action), "unhandled action {action}");
            }
            other => panic!("expected a match for {key}, got {other:?}"),
        }
    };

    // mod+s twice: two tiles side by side.
    press(&mut matcher, &mut ws, "mod+s");
    press(&mut matcher, &mut ws, "mod+s");
    assert_eq!(ws.active().tiles().len(), 2);

    // mod+h: focus left tile; mod+v: split it vertically.
    press(&mut matcher, &mut ws, "mod+h");
    press(&mut matcher, &mut ws, "mod+v");
    assert_eq!(ws.active().tiles().len(), 3);
    let rects = ws.active().layout(Rect::UNIT);
    assert_eq!(rects.len(), 3);

    // mod+f: fullscreen the focused tile — only one visible.
    press(&mut matcher, &mut ws, "mod+f");
    assert_eq!(ws.active().layout(Rect::UNIT).len(), 1);
    press(&mut matcher, &mut ws, "mod+f");

    // mod+2: switch to an empty workspace; mod+1: back with tiles intact.
    press(&mut matcher, &mut ws, "mod+2");
    assert!(ws.active().is_empty());
    press(&mut matcher, &mut ws, "mod+1");
    assert_eq!(ws.active().tiles().len(), 3);

    // Directional focus works through the same pipeline.
    let before = ws.active().focused();
    press(&mut matcher, &mut ws, "mod+l");
    assert_ne!(ws.active().focused(), before);
    assert!(ws.active().neighbor(Direction::Left).is_some());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-shell`
Expected: compile error — `Workspaces`, `apply_workspace_action` not defined.

- [ ] **Step 4: Implement**

Insert above the tests in `workspaces.rs`:
```rust
/// The app-global workspace set (spec §3.6: workspaces are global; windows
/// are viewports). Indices 1..=9; workspaces materialize lazily on first
/// switch and persist (empty ones stay listed as empty).
#[derive(Debug)]
pub struct Workspaces {
    spaces: BTreeMap<u8, Tree>,
    active: u8,
    next_tile: u64,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspaces {
    pub fn new() -> Self {
        let mut spaces = BTreeMap::new();
        spaces.insert(1, Tree::default());
        Workspaces { spaces, active: 1, next_tile: 0 }
    }

    pub fn active_index(&self) -> u8 {
        self.active
    }

    pub fn active(&self) -> &Tree {
        // Invariant: `active` is always a key (established in new/switch).
        &self.spaces[&self.active]
    }

    pub fn active_mut(&mut self) -> &mut Tree {
        self.spaces
            .get_mut(&self.active)
            .expect("active workspace always exists")
    }

    /// Switch to workspace `n` (1..=9), creating it empty if needed.
    pub fn switch(&mut self, n: u8) -> bool {
        if !(1..=9).contains(&n) {
            return false;
        }
        self.spaces.entry(n).or_default();
        self.active = n;
        true
    }

    /// Allocate a tile id, unique across all workspaces for the app's life.
    pub fn alloc_tile(&mut self) -> TileId {
        self.next_tile += 1;
        TileId(self.next_tile)
    }

    /// Workspace indices that currently hold at least one tile.
    pub fn non_empty_indices(&self) -> Vec<u8> {
        self.spaces
            .iter()
            .filter(|(_, tree)| !tree.is_empty())
            .map(|(ix, _)| *ix)
            .collect()
    }
}

/// Route a shell action onto the tiling verbs. Returns true when the action
/// was recognized as a workspace action — even if it changed nothing (focus
/// at an edge still counts as handled; the keystroke must not fall through
/// to another handler). False means "not a workspace action".
pub fn apply_workspace_action(ws: &mut Workspaces, action: &ActionId) -> bool {
    match action.0.as_str() {
        "workspace::focus_left" => {
            ws.active_mut().focus_direction(Direction::Left);
            true
        }
        "workspace::focus_down" => {
            ws.active_mut().focus_direction(Direction::Down);
            true
        }
        "workspace::focus_up" => {
            ws.active_mut().focus_direction(Direction::Up);
            true
        }
        "workspace::focus_right" => {
            ws.active_mut().focus_direction(Direction::Right);
            true
        }
        "workspace::split_vertical" => {
            let id = ws.alloc_tile();
            ws.active_mut().split(id, Orientation::Vertical);
            true
        }
        "workspace::split_horizontal" => {
            let id = ws.alloc_tile();
            ws.active_mut().split(id, Orientation::Horizontal);
            true
        }
        "workspace::close_tile" => {
            ws.active_mut().close();
            true
        }
        "workspace::fullscreen_tile" => {
            ws.active_mut().toggle_fullscreen();
            true
        }
        other => match other.strip_prefix("workspace::switch_") {
            Some(n) => match n.parse::<u8>() {
                Ok(n) => ws.switch(n),
                Err(_) => false,
            },
            None => false,
        },
    }
}
```

Note for the implementer: the test module references `super::super::tree::Rect` in one test — if the module tree makes `super::Rect` (via `use super::tree::...` at the top) cleaner, adjust the test's path accordingly and note it; the assertion content is what matters.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p geode-shell`
Expected: 8 new workspaces tests + 1 integration test + all prior tests PASS.

- [ ] **Step 6: Full workspace verification**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && cargo bench --workspace --no-run`
Expected: everything green.

- [ ] **Step 7: Commit**

```bash
git add crates/geode-shell
git commit -m "feat: global workspaces and the workspace action dispatcher"
```

---

## Self-Review Notes

- **Spec coverage:** §3.1 verbs — split (h/v), focus (hjkl), move, resize, fullscreen, close, workspace switch 1..9 — all present; tabbed/stacked modes explicitly deferred (documented simplification, matching the ratchet: they extend `Node` later). §3.6 — workspaces as an app-global set, windows-as-viewports ready (nothing in `Workspaces` assumes one window). §10.3 — everything window-free tested. The keyboard→tree pipeline is proven end to end in the integration test.
- **Type consistency:** `TileId`/`Orientation`/`Direction`/`Rect`/`Tree` identical across Tasks 1–4; `apply_workspace_action` signature matches the integration test's use; action id strings match Phase 1a's `defaults::register_builtin_actions` exactly (focus_left/down/up/right, split_vertical/horizontal, fullscreen_tile, close_tile, switch_N).
- **Known seams for 1b-ui:** `Tree::layout(pixel_bounds)` for rendering, `Tree::focus`/`neighbor` for mouse interaction, `Workspaces` as the single mutation surface, resize verbs awaiting a resize-mode keymap context (bindings come with 1b-ui's mode handling).
- **Test-count expectations are minimums**, as established in 1a.
