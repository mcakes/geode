# Tile Stacks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a slot in the tiling tree hold several tiles with one painted, switched by `mod+[`/`mod+]`, a marker chip in the module's header, and a transient list, with no permanent tab chrome.

**Architecture:** A third `Node` variant, `Stack { children: Vec<TileId>, active: usize }`, in the pure tree; `Tree::layout` emits only the active member's rect so every existing slot verb treats a stack as one tile. The shell delivers each member a `StackHandle` through a new required `TileContent::set_stack`, and each module paints the `2/4` chip first in its own header; the list overlay is shell-owned, painted in the palette's mould. Entry is a fourth `{Kind}: Stack` palette row per kind and the centre drop zone, which changes meaning from swap to add-to-stack.

**Tech Stack:** Rust, gpui (`gpui-pre` 0.3.5) + gpui-component 0.6.2 (both `=`-pinned), `toml` for the session, criterion untouched.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-tile-stacks-design.md`

## Global Constraints

- The trader-facing word is **stack** and **member**; never "tab" in a title, notice or doc string.
- Members are always leaves: `Node::Stack` holds `TileId`s, never `Node`s.
- `children.len() >= 2` and `active < children.len()` on every live stack; restore heals rather than refuses.
- A tile id appears once in a tree. `Tree::remove` stays the one leaf-removal seam session healing uses.
- The focused tile of a workspace is a member only when that member is active: every assignment of `focused` goes through `Tree::set_focus`, which activates.
- `mod+]` = `stack::next`, `mod+[` = `stack::prev`, both `workspace` context; `stack::pick` and `stack::unstack` have no default binding.
- `TileContent::set_stack` and `TileContent::title` are **required** trait methods with no default body.
- The marker chip is `Tone::Neutral` through `geode_shell::shell::chip::chip_paint`, mono face, `theme.radius_tokens().sm`, painted only while `len > 1`, first in the module's header, `debug_selector` `stack-marker-{tile}`.
- No per-frame heap churn: the chip's text is a `SharedString` prepared once in `StackHandle`, never `format!`ed in `render`.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo check -p geode-shell --features test-support --all-targets` must pass at the end of every task. Commit at the end of every task.
- Run `zsh scripts/mutation-check.sh --anchors-only` before the final merge (Task 9).
- Do not build a tab bar, a hover reveal, or the unsent-edits dot (spec §5.2).

---

## File map

| File | Responsibility |
|---|---|
| `crates/geode-shell/src/tiling/tree.rs` | `Node::Stack`, layout, `visible_tiles`, `set_focus`/`activate`, `stack_after`, `stack_step`, `pop_out`, `stack_position`, healing in `validate_node` |
| `crates/geode-shell/src/tiling/workspaces.rs` | `Workspace::{stack_step, unstack_focused, drop_stack, stack_position}`, `Workspaces::{stack_active, stack_position}` |
| `crates/geode-shell/src/tiling/dropzones.rs` | module doc: centre drop is add-to-stack |
| `crates/geode-shell/src/session.rs` | `kind = "stack"` node encoding |
| `crates/geode-shell/src/module.rs` | `StackHandle`, `TileContent::{set_stack, title}`, placeholder and recording impls |
| `crates/geode-shell/src/defaults.rs` | `stack::*` actions and bindings, `tile::add_{kind}_stacked`, `AddPlacement`, `parse_add_action` |
| `crates/geode-shell/src/shell/mod.rs` | `notice`, `stack_sent`, `stack_list` fields |
| `crates/geode-shell/src/shell/input.rs` | `stack::*` dispatch arms, list key handling |
| `crates/geode-shell/src/shell/occupants.rs` | `set_stack` delivery, visible set from `visible_tiles` |
| `crates/geode-shell/src/shell/stacklist.rs` (new) | `StackList` state + `render` |
| `crates/geode-shell/src/shell/render.rs` | list overlay paint, `notice` to the status bar |
| `crates/geode-shell/src/shell/status.rs` | `notice` segment |
| `crates/geode-shell/src/shell/add_tile.rs` | `AddPlacement::Stacked` |
| `crates/geode-shell/src/shell/drag.rs` | centre drop → `drop_stack` |
| `crates/geode-shell/src/shell/tests/stacks.rs` (new) | window tests |
| `crates/geode-blotter/src/{tile,content}.rs`, `crates/geode-marketdata/src/{tile,header,content}.rs`, `crates/geode-diagnostics/src/{tile,lib}.rs` | marker chip + `title` |
| `scripts/mutation-check.sh`, `CLAUDE.md`, the spec | Task 9 |

---

### Task 1: `Node::Stack` in the pure tree — shape, layout, visibility, healing

**Files:**
- Modify: `crates/geode-shell/src/tiling/tree.rs`
- Test: `crates/geode-shell/src/tiling/tree.rs` (`mod tests`)

**Interfaces:**
- Produces: `Node::Stack { children: Vec<TileId>, active: usize }`; `Tree::visible_tiles(&self) -> Vec<TileId>`; `Tree::stack_position(&self, id: TileId) -> Option<(usize, usize)>` (one-based index, len); `pub(crate) fn stack_after(&mut self, anchor: TileId, new: TileId) -> bool`; private `set_focus`, `activate`, `node_holds`, `find_stack_mut`.

- [ ] **Step 1: Write the failing tests** (append inside `mod tests` in `tree.rs`)

```rust
    // --- stacks (tile-stacks spec §3) ------------------------------------

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
        assert!(rects.iter().all(|(id, _)| *id != TileId(2)), "hidden member has no rect");
        let r3 = rect_of(&tree, 3);
        assert!(approx(r3.x, 0.5) && approx(r3.w, 0.5) && approx(r3.h, 1.0));
    }

    #[test]
    fn stack_after_appends_after_the_anchor_inside_an_existing_stack() {
        let mut tree = two_tiles_then_stack();
        assert!(tree.stack_after(TileId(2), TileId(4)));
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2), TileId(4), TileId(3)]);
        assert_eq!(tree.focused(), Some(TileId(4)));
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(4)]);
    }

    #[test]
    fn stack_after_refuses_a_missing_anchor_a_present_new_or_a_self_anchor() {
        let mut tree = two_tiles_then_stack();
        assert!(!tree.stack_after(TileId(9), TileId(4)));
        assert!(!tree.stack_after(TileId(2), TileId(1)));
        assert!(!tree.stack_after(TileId(2), TileId(2)));
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2), TileId(3)], "untouched");
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
                Node::Stack { children: vec![TileId(1), TileId(2), TileId(2)], active: 7 },
            ],
            ratios: vec![0.5, 0.5],
        };
        let tree = Tree::from_parts(Some(root), Some(TileId(2)), None).unwrap();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2)]);
        assert_eq!(tree.stack_position(TileId(2)), None, "one survivor collapses to a leaf");

        let root = Node::Stack { children: vec![TileId(4), TileId(5), TileId(6)], active: 9 };
        let tree = Tree::from_parts(Some(root), Some(TileId(6)), None).unwrap();
        assert_eq!(tree.visible_tiles(), vec![TileId(6)], "focused member is activated on restore");

        let root = Node::Stack { children: vec![TileId(4), TileId(5)], active: 9 };
        let tree = Tree::from_parts(Some(root), None, None).unwrap();
        assert_eq!(tree.visible_tiles(), vec![TileId(4)], "out-of-range active clamps to 0");

        let root = Node::Split {
            orientation: Orientation::Vertical,
            children: vec![Node::Leaf(TileId(1)), Node::Stack { children: vec![TileId(1)], active: 0 }],
            ratios: vec![0.5, 0.5],
        };
        let tree = Tree::from_parts(Some(root), None, None).unwrap();
        assert_eq!(tree.tiles(), vec![TileId(1)], "a stack emptied by healing vanishes and the split collapses");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell tiling::tree::tests::stack_after -- --nocapture 2>&1 | tail -20`
Expected: compile error, `no variant named Stack` / `no method named stack_after`.

- [ ] **Step 3: Add the variant and the helpers**

In `tree.rs`, extend `Node`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(TileId),
    Split {
        orientation: Orientation,
        children: Vec<Node>,
        ratios: Vec<f32>,
    },
    /// A slot holding several tiles with one painted (tile-stacks spec
    /// §3). Members are leaves by construction — the variant holds ids,
    /// not nodes — and `Tree::layout` emits only `children[active]`, so
    /// every slot verb sees a stack as one tile. Invariants:
    /// `children.len() >= 2`, `active < children.len()`.
    Stack { children: Vec<TileId>, active: usize },
}
```

Add helpers after `swap_leaves`:

```rust
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
```

Give every existing `match` on `Node` a `Stack` arm: `swap_leaves` (rename members: `for c in children { if *c == a { *c = b } else if *c == b { *c = a } }`), `collect_leaves` (`Node::Stack { children, .. } => out.extend(children)`), `layout_node` (`Node::Stack { children, active } => out.push((children[*active], rect))`), `path_to` (`Node::Stack { children, .. } => children.contains(&target)`), `node_at_mut` (`Node::Leaf(_) | Node::Stack { .. } => unreachable!("path indexes into splits")`), `remove_leaf` (below), `insert_beside` (below), `validate_node` (below).

`remove_leaf`'s Stack arm, placed before the `Split` arm:

```rust
        Node::Stack { mut children, active } => {
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
                    // The next member takes the closed one's slot; the
                    // previous one when the closed member was last
                    // (spec §3 "Close").
                    let active = if ix < active {
                        active - 1
                    } else {
                        active.min(n - 1)
                    };
                    Some(Node::Stack { children, active })
                }
            }
        }
```

`insert_beside`: change the first arm's guard and the flat-insert position test so a stack is wrapped or joined exactly as a leaf is:

```rust
        node if node_holds(&node, anchor) => {
            let children = if after {
                vec![node, Node::Leaf(new)]
            } else {
                vec![Node::Leaf(new), node]
            };
            Node::Split { orientation, children, ratios: vec![0.5, 0.5] }
        }
        leaf @ Node::Leaf(_) => leaf,
        stack @ Node::Stack { .. } => stack,
        Node::Split { orientation: existing, mut children, ratios } => {
            if existing == orientation
                && let Some(ix) = children.iter().position(|c| node_holds(c, anchor))
            {
                // unchanged body
```

`validate_node` becomes healing and threads a `seen` list; a node can vanish:

```rust
/// Recursively validate one `Node` for [`Tree::from_parts`]. A `Split`
/// must have >= 2 children with a matching-length `ratios` vec of finite,
/// positive values (renormalized on success). A `Stack` is HEALED rather
/// than refused (tile-stacks spec §7): a member already claimed by an
/// earlier node in document order (`seen`) or repeated within the stack
/// is dropped, an out-of-range `active` clamps to 0, one survivor
/// collapses to a leaf and none vanishes — `Ok(None)`, which a parent
/// split then drops from its own children (collapsing to its survivor
/// when one remains) exactly as `remove_leaf` would.
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
                n => Some(Node::Stack { active: if active < n { active } else { 0 }, children: kept }),
            })
        }
        Node::Split { orientation, children, ratios } => {
            if children.len() < 2 {
                return Err(format!("split has {} children, need at least 2", children.len()));
            }
            if children.len() != ratios.len() {
                return Err(format!("split has {} children but {} ratios", children.len(), ratios.len()));
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
                    Ok(Some(Node::Split { orientation, children: kept_children, ratios }))
                }
            }
        }
    }
}
```

In `from_parts`, call it as `let root = root.map(|n| validate_node(n, &mut Vec::new())).transpose()?.flatten();` and, after computing `focused`, build the tree then `if let Some(f) = focused { tree.activate(f); }` (so a restored focused hidden member is painted).

Add to `impl Tree`, next to `tiles`:

```rust
    /// The tiles painted right now: every leaf plus each stack's active
    /// member, in tree order (tile-stacks spec §3). `tiles()` still lists
    /// hidden members — retention and session dirt need them.
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

    /// Make `id` the painted member of its stack (a no-op for a plain
    /// leaf). Fullscreen follows: if the stack's outgoing active member
    /// held it, `id` holds it now (spec §3 "Fullscreen").
    fn activate(&mut self, id: TileId) {
        let Some(root) = self.root.as_mut() else { return };
        let Some(Node::Stack { children, active }) = find_stack_mut(root, id) else { return };
        let Some(ix) = children.iter().position(|c| *c == id) else { return };
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

    /// Insert `new` after `anchor` in the anchor's stack — a leaf anchor
    /// becomes a two-member stack of the two (spec §6.1). `new` becomes
    /// active and focused. Refuses, untouched, when `anchor` is not a
    /// leaf here, `new` already is, or the two are one id.
    pub(crate) fn stack_after(&mut self, anchor: TileId, new: TileId) -> bool {
        if anchor == new || !self.contains(anchor) || self.contains(new) {
            return false;
        }
        fn insert(node: &mut Node, anchor: TileId, new: TileId) -> bool {
            match node {
                Node::Leaf(id) if *id == anchor => {
                    *node = Node::Stack { children: vec![anchor, new], active: 1 };
                    true
                }
                Node::Leaf(_) => false,
                Node::Stack { children, active } => match children.iter().position(|c| *c == anchor) {
                    Some(ix) => {
                        children.insert(ix + 1, new);
                        *active = ix + 1;
                        true
                    }
                    None => false,
                },
                Node::Split { children, .. } => children.iter_mut().any(|c| insert(c, anchor, new)),
            }
        }
        let root = self.root.as_mut().expect("contains(anchor) implies a root");
        insert(root, anchor, new);
        self.set_focus(new);
        true
    }
```

Replace every `self.focused = Some(x)` in `split`, `remove_focused`, `focus`, `focus_direction`, `insert_at_leaf` and `replace_tile` with `self.set_focus(x)` (`remove_focused`'s `self.focused = post_close_tiles.get(..).copied()` becomes `match … { Some(id) => self.set_focus(id), None => self.focused = None }`).

- [ ] **Step 4: Run the tree tests**

Run: `cargo test -p geode-shell tiling:: 2>&1 | tail -15`
Expected: all pass, including every pre-existing tree, dividers, dropzones and workspaces test.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings
git add crates/geode-shell/src/tiling/tree.rs
git commit -m "tiling: Node::Stack — layout, visible_tiles, stack_after, healing restore

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: Stack verbs in the pure tree — cycle, close, move out, split beside, fullscreen

**Files:**
- Modify: `crates/geode-shell/src/tiling/tree.rs`

**Interfaces:**
- Consumes: Task 1's `set_focus`, `activate`, `node_holds`, `find_stack_mut`.
- Produces: `pub fn stack_step(&mut self, delta: i64) -> bool`; `pub fn unstack_focused(&mut self, orientation: Orientation) -> bool`; `move_direction` pops a member out; `remove_focused` refocuses the stack's new active member; `toggle_split_orientation` treats a stack as one child.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn stack_step_cycles_with_wrap_and_a_count() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(tree.stack_after(TileId(1), TileId(2)));
        assert!(tree.stack_after(TileId(2), TileId(3))); // [1, 2, 3], 3 active
        assert!(tree.stack_step(1));
        assert_eq!(tree.focused(), Some(TileId(1)), "next past the end wraps to the first");
        assert!(tree.stack_step(-1));
        assert_eq!(tree.focused(), Some(TileId(3)), "prev before the first wraps to the last");
        assert!(tree.stack_step(2));
        assert_eq!(tree.focused(), Some(TileId(2)), "a count steps N with the same wrap");
        assert_eq!(tree.visible_tiles(), vec![TileId(2)]);
    }

    #[test]
    fn stack_step_is_refused_on_a_plain_leaf() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(!tree.stack_step(1));
        assert_eq!(tree.focused(), Some(TileId(1)));
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
        assert_eq!(tree.focused(), Some(TileId(3)), "the next member, not the tile after the stack");
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
        assert_eq!(tree.stack_position(TileId(2)), None, "one survivor collapsed to a leaf");
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(2), TileId(3)]);
        assert_eq!(tree.focused(), Some(TileId(3)));
        let r3 = rect_of(&tree, 3);
        let r2 = rect_of(&tree, 2);
        assert!(r3.x > r2.x, "popped out to the right of the stack it left");
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
        assert!(tree.stack_after(TileId(2), TileId(3)));
        tree.focus(TileId(2));
        assert!(tree.unstack_focused(Orientation::Vertical));
        assert_eq!(tree.stack_position(TileId(2)), None);
        assert_eq!(tree.stack_position(TileId(1)), Some((1, 2)));
        assert!(rect_of(&tree, 2).y > rect_of(&tree, 1).y, "below the stack");
        assert_eq!(tree.focused(), Some(TileId(2)));
        assert!(!tree.unstack_focused(Orientation::Vertical), "refused on a plain leaf");
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
        assert_eq!(tree.stack_position(TileId(3)), Some((2, 2)), "the stack itself is untouched");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell tiling::tree::tests::stack_step 2>&1 | tail -5`
Expected: compile error, `no method named stack_step`.

- [ ] **Step 3: Implement**

In `impl Tree`:

```rust
    /// Cycle the focused member by `delta` with wrap (spec §4:
    /// `stack::next`/`prev`, a count prefix steps N). `false`, untouched,
    /// when the focused tile is not a member.
    pub fn stack_step(&mut self, delta: i64) -> bool {
        let Some(focused) = self.focused else { return false };
        let Some(root) = self.root.as_mut() else { return false };
        let Some(Node::Stack { children, active }) = find_stack_mut(root, focused) else {
            return false;
        };
        let len = children.len() as i64;
        let next = (*active as i64 + delta).rem_euclid(len) as usize;
        let id = children[next];
        self.set_focus(id);
        true
    }

    /// Pop the focused member out of its stack and place it beside the
    /// stack: `after` on the right/bottom side, else left/top (spec §3
    /// "Move", §4 `stack::unstack`). The stack collapses to a leaf when
    /// one member remains. `false`, untouched, on a plain leaf.
    fn pop_out(&mut self, orientation: Orientation, after: bool) -> bool {
        let Some(focused) = self.focused else { return false };
        let survivor = {
            let Some(root) = self.root.as_ref() else { return false };
            let Some(Node::Stack { children, .. }) = find_stack(root, focused) else {
                return false;
            };
            // Any other member names the stack for `insert_beside`, which
            // asks `node_holds`; the collapsed-to-leaf case is that one
            // member itself.
            *children.iter().find(|c| **c != focused).expect("a stack has two members")
        };
        let fullscreen = self.fullscreen;
        let mut done = false;
        let root = self.root.take().and_then(|n| remove_leaf(n, focused, &mut done));
        let root = root.expect("removing one member of a stack never empties the tree");
        self.root = Some(insert_beside(root, survivor, focused, orientation, after));
        self.fullscreen = fullscreen.filter(|f| *f == focused);
        self.set_focus(focused);
        true
    }

    pub fn unstack_focused(&mut self, orientation: Orientation) -> bool {
        self.pop_out(orientation, true)
    }
```

`move_direction` becomes:

```rust
    pub fn move_direction(&mut self, dir: Direction) -> bool {
        let Some(focused) = self.focused else { return false };
        if self.stack_position(focused).is_some() {
            // A member does not swap: it leaves its stack in that
            // direction (spec §3 "Move" — i3's move-out-of-container).
            let after = matches!(dir, Direction::Right | Direction::Down);
            return self.pop_out(dir.orientation(), after);
        }
        let Some(neighbor) = self.neighbor(dir) else { return false };
        if let Some(root) = &mut self.root {
            swap_leaves(root, focused, neighbor);
        }
        true
    }
```

`remove_focused`: before the removal, record `let sibling = self.root.as_ref().and_then(|r| find_stack(r, focused)).and_then(|n| match n { Node::Stack { children, .. } => children.iter().copied().find(|c| *c != focused), _ => None });`. After the removal, replace the refocus block with:

```rust
        // A closed member refocuses its own stack's new active member
        // (spec §3 "Close"), never the tile after the stack; every other
        // close keeps the tree-order-neighbour rule.
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
```

`toggle_split_orientation`'s `toggle_at`: replace `children.iter().position(|c| *c == Node::Leaf(focused))` with `children.iter().position(|c| node_holds(c, focused))`.

- [ ] **Step 4: Run the whole shell crate's pure tests**

Run: `cargo test -p geode-shell tiling:: 2>&1 | tail -8`
Expected: all pass.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings
git add crates/geode-shell/src/tiling/tree.rs
git commit -m "tiling: stack verbs — cycle with wrap, close refocus, move-out, split wraps the stack

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Workspace-level stack verbs and the centre drop

**Files:**
- Modify: `crates/geode-shell/src/tiling/workspaces.rs`
- Modify: `crates/geode-shell/src/tiling/dropzones.rs` (module doc only)

**Interfaces:**
- Consumes: `Tree::{stack_step, unstack_focused, stack_after, stack_position}`.
- Produces: `Workspace::stack_step(&mut self, delta: i64) -> bool`; `Workspace::unstack_focused(&mut self, orientation: Orientation) -> bool`; `Workspace::stack_position(&self, id: TileId) -> Option<(usize, usize)>`; `Workspace::drop_stack(&mut self, dragged: TileId, target: TileId) -> bool` (replaces `drop_swap`); `Workspaces::stack_active(&mut self) -> Option<TileId>`; `Workspaces::stack_position(&self, id: TileId) -> Option<(usize, usize)>`.

- [ ] **Step 1: Write the failing tests** (in `workspaces.rs`'s `mod tests`, next to the drop tests)

```rust
    #[test]
    fn stack_active_stacks_onto_the_focused_tile_in_whichever_region_holds_focus() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.stack_active().expect("a focused tile to stack onto");
        assert_eq!(ws.active().tree().visible_tiles(), vec![b]);
        assert_eq!(ws.active().stack_position(a), Some((1, 2)));
        assert_eq!(ws.active().focused_tile(), Some(b));
        assert!(Workspaces::new().stack_active().is_none(), "nothing focused, nothing stacked");
    }

    #[test]
    fn stack_step_and_unstack_go_to_the_focused_region() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.stack_active().unwrap();
        assert!(ws.active_mut().stack_step(1));
        assert_eq!(ws.active().focused_tile(), Some(a));
        assert!(ws.active_mut().unstack_focused(Orientation::Horizontal));
        assert_eq!(ws.active().stack_position(a), None);
        assert_eq!(ws.active().stack_position(b), None);
        assert_eq!(ws.active().tree().visible_tiles().len(), 2);
        assert!(!ws.active_mut().stack_step(1), "no longer a member");
    }

    #[test]
    fn drop_stack_adds_the_dragged_tile_after_the_target_and_focuses_it() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.split_active(Orientation::Horizontal);
        assert!(ws.active_mut().drop_stack(a, b));
        assert_eq!(ws.active().tree().tiles(), vec![b, a]);
        assert_eq!(ws.active().stack_position(a), Some((2, 2)));
        assert_eq!(ws.active().focused_tile(), Some(a));
        assert!(!ws.active_mut().drop_stack(a, a), "self-drop is refused");
    }

    #[test]
    fn drop_stack_within_one_stack_reorders() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.stack_active().unwrap();
        let c = ws.stack_active().unwrap(); // [a, b, c]
        assert!(ws.active_mut().drop_stack(a, c));
        assert_eq!(ws.active().tree().tiles(), vec![b, c, a]);
        assert_eq!(ws.active().stack_position(a), Some((3, 3)));
        assert_eq!(ws.active().focused_tile(), Some(a));
    }

    #[test]
    fn drop_stack_across_regions_lands_in_the_targets_dock() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.split_active(Orientation::Horizontal);
        ws.active_mut().move_to_dock(DockSide::Left); // b into the left dock
        assert_eq!(ws.active().region_of(b), Some(FocusRegion::Dock(DockSide::Left)));
        assert!(ws.active_mut().drop_stack(a, b));
        assert!(ws.active().tree().is_empty(), "a left the main tree");
        assert_eq!(ws.active().region_of(a), Some(FocusRegion::Dock(DockSide::Left)));
        assert_eq!(ws.active().stack_position(a), Some((2, 2)));
        assert_eq!(ws.active().focused_tile(), Some(a));
    }

    #[test]
    fn workspaces_stack_position_searches_every_workspace_and_dock() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.stack_active().unwrap();
        ws.switch(2);
        assert_eq!(ws.stack_position(a), Some((1, 2)));
        assert_eq!(ws.stack_position(b), Some((2, 2)));
        assert_eq!(ws.stack_position(TileId(99)), None);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell tiling::workspaces::tests::stack_active 2>&1 | tail -5`
Expected: compile error, `no method named stack_active`.

- [ ] **Step 3: Implement**

In `impl Workspace` (beside `close_tile`):

```rust
    /// `stack::next`/`prev` (tile-stacks spec §4): cycle the focused
    /// member in whichever tree holds focus. `false` when it is not a
    /// member.
    pub fn stack_step(&mut self, delta: i64) -> bool {
        let region = self.region;
        self.tree_for_mut(region).stack_step(delta)
    }

    /// `stack::unstack` (spec §4): pop the focused member out beside its
    /// stack. `false` when it is not a member.
    pub fn unstack_focused(&mut self, orientation: Orientation) -> bool {
        let region = self.region;
        self.tree_for_mut(region).unstack_focused(orientation)
    }

    /// `id`'s `(index, len)` in whichever of this workspace's trees holds
    /// it as a member (spec §5.1's marker).
    pub fn stack_position(&self, id: TileId) -> Option<(usize, usize)> {
        let region = self.region_of(id)?;
        self.tree_for(region).stack_position(id)
    }

    /// Centre-zone drop (tile-stacks spec §6.2, replacing the swap):
    /// move `dragged` out of whichever tree holds it and add it to
    /// `target`'s stack, after `target` — a leaf target becomes a
    /// two-member stack — in whichever region the target lives. Dropping
    /// a member onto another member of its own stack reorders it. Focus
    /// and region follow the dragged tile; an emptied source dock
    /// auto-hides; a destination dock auto-shows. Returns `true` iff the
    /// layout changed: a self-drop or an unknown id is `false`.
    pub fn drop_stack(&mut self, dragged: TileId, target: TileId) -> bool {
        if dragged == target {
            return false;
        }
        let Some(source) = self.region_of(dragged) else { return false };
        let Some(destination) = self.region_of(target) else { return false };
        if destination != source && self.tree_for(destination).contains(dragged) {
            debug_assert!(
                false,
                "one-place-per-TileId invariant pre-broken \
                 (dragged {dragged:?} duplicated into the destination tree); \
                 refusing the stack drop untouched"
            );
            return false;
        }
        self.remove_tile_anywhere(dragged);
        if destination == FocusRegion::Main {
            self.tree.exit_fullscreen();
        }
        let tree = self.tree_for_mut(destination);
        if !tree.stack_after(target, dragged) {
            // Unreachable given the pre-checks; the never-lose-a-tile
            // invariant outranks trusting them.
            tree.split(dragged, Orientation::Horizontal);
        }
        self.enter_region(destination);
        true
    }
```

Delete `Workspace::drop_swap` and `Tree::replace_tile`/`Tree::swap_tiles` if nothing else calls them (`grep -rn "replace_tile\|swap_tiles\|drop_swap" crates/` — `move_direction` uses `swap_leaves`, not these; their unit tests go with them). In `impl Workspaces` beside `split_active`:

```rust
    /// `{Kind}: Stack` (tile-stacks spec §6.1): allocate a tile and add it
    /// after the focused tile in that tile's stack, in whichever region
    /// holds focus. `None` when nothing is focused — the caller then
    /// falls back to the split path, since a stack of one is meaningless.
    pub fn stack_active(&mut self) -> Option<TileId> {
        let focused = self.active().focused_tile()?;
        let id = self.alloc_tile();
        let ws = self.active_mut();
        let region = ws.region;
        if !ws.tree_for_mut(region).stack_after(focused, id) {
            // Unreachable: `focused_tile` is a leaf of that tree and `id`
            // is fresh. Never lose the id.
            ws.tree_for_mut(region).split(id, Orientation::Horizontal);
        }
        Some(id)
    }

    /// `id`'s stack position in whichever workspace holds it (the shell
    /// delivers markers for every tile, not only the active workspace's).
    pub fn stack_position(&self, id: TileId) -> Option<(usize, usize)> {
        self.spaces.values().find_map(|ws| ws.stack_position(id))
    }
```

Rewrite the **Center-drop semantics note** in `dropzones.rs`'s module doc:

```rust
//! **Center-drop semantics (tile-stacks spec §6.2)**: a center drop adds
//! the dragged tile to the target's stack, after the target — the
//! meaning change the original tile-drag design accepted in advance.
//! Keyboard `workspace::move_*` keeps its swap on a plain leaf and pops a
//! member out of its stack.
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell tiling:: 2>&1 | tail -8`
Expected: all pass. If `drop_swap`'s own unit tests remain, delete them with the verb.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings
git add crates/geode-shell/src/tiling/
git commit -m "tiling: workspace stack verbs, stack_active, drop_stack replaces drop_swap

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

(The shell's `drag.rs` still calls `drop_swap` at this point; Task 8 switches it. If the crate does not compile between Tasks 3 and 8, change `drag.rs`'s centre arm to `ws.drop_stack(drag.tile, id)` in this task and leave the drag test's assertion update to Task 8.)

---

### Task 4: Session encoding for stacks

**Files:**
- Modify: `crates/geode-shell/src/session.rs` (`node_to_toml`, `node_from_toml`, module doc)
- Modify: spec §7 (one correction)

**Interfaces:**
- Consumes: `Node::Stack`, `Tree::from_parts` healing (Task 1).
- Produces: `kind = "stack"` with `members = [..]`, `active = n`.

- [ ] **Step 1: Write the failing tests** (in `session.rs`'s `mod tests`)

```rust
    #[test]
    fn round_trips_a_stack_in_the_main_tree_and_in_a_dock() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.stack_active().unwrap();
        let _c = ws.stack_active().unwrap();
        ws.active_mut().focus_main_tile(b); // b active, hidden c and a
        let d = ws.split_active(Orientation::Horizontal);
        ws.active_mut().move_to_dock(crate::tiling::DockSide::Left);
        let _e = ws.stack_active().unwrap(); // stack in the dock
        let _ = (a, d);

        let table = to_toml(&ws, &TileRecords::new(), None, &no_usage());
        let text = toml::to_string(&table).unwrap();
        assert!(text.contains("kind = \"stack\""), "{text}");
        assert!(text.contains("members = ["), "{text}");
        let Restored { workspaces: restored, warnings, .. } = from_toml(&table).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored.active().tree().tiles(), ws.active().tree().tiles());
        assert_eq!(restored.active().tree().visible_tiles(), ws.active().tree().visible_tiles());
        assert_eq!(restored.active().tree().focused(), ws.active().tree().focused());
        let side = crate::tiling::DockSide::Left;
        assert_eq!(
            restored.active().docks().get(side).tree().visible_tiles(),
            ws.active().docks().get(side).tree().visible_tiles()
        );
    }

    #[test]
    fn a_hostile_stack_node_is_healed_not_refused() {
        let text = r#"
config_version = 1
active = 1
[workspaces.1]
focused = 2
[workspaces.1.node]
kind = "split"
orientation = "horizontal"
ratios = [0.5, 0.5]
[[workspaces.1.node.children]]
kind = "leaf"
id = 1
[[workspaces.1.node.children]]
kind = "stack"
members = [1, 2, 3]
active = 12
"#;
        let table: toml::Table = toml::from_str(text).unwrap();
        let Restored { workspaces, .. } = from_toml(&table).unwrap();
        let tree = workspaces.active().tree();
        assert_eq!(tree.tiles(), vec![TileId(1), TileId(2), TileId(3)]);
        assert_eq!(tree.stack_position(TileId(2)), Some((1, 2)), "the leaf's claim on 1 won");
        assert_eq!(tree.visible_tiles(), vec![TileId(1), TileId(2)], "active clamped to 0; focused 2 activated");
    }

    #[test]
    fn a_stack_node_with_a_bad_member_list_is_an_error_like_a_bad_leaf() {
        let text = r#"
config_version = 1
active = 1
[workspaces.1]
[workspaces.1.node]
kind = "stack"
members = [1, -4]
"#;
        let table: toml::Table = toml::from_str(text).unwrap();
        let err = from_toml(&table).unwrap_err();
        assert!(err.contains("negative"), "{err}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell session::tests::round_trips_a_stack 2>&1 | tail -5`
Expected: FAIL (`assert!(text.contains("kind = \"stack\""))` fails, or a match is non-exhaustive at compile time).

- [ ] **Step 3: Implement**

`node_to_toml` gains the arm:

```rust
        Node::Stack { children, active } => {
            let mut t = toml::Table::new();
            t.insert("kind".to_string(), toml::Value::String("stack".to_string()));
            t.insert(
                "members".to_string(),
                toml::Value::Array(children.iter().map(|id| toml::Value::Integer(tile_id_to_i64(*id))).collect()),
            );
            t.insert("active".to_string(), toml::Value::Integer(*active as i64));
            toml::Value::Table(t)
        }
```

`node_from_toml` gains, before the `other =>` arm:

```rust
        Some("stack") => {
            let members = table
                .get("members")
                .and_then(|v| v.as_array())
                .ok_or("stack missing 'members' array")?
                .iter()
                .map(|v| {
                    let id = v.as_integer().ok_or_else(|| "stack member is not an integer".to_string())?;
                    if id < 0 {
                        return Err(format!("stack member id {id} is negative"));
                    }
                    Ok(TileId(id as u64))
                })
                .collect::<Result<Vec<_>, String>>()?;
            // Healing — a short list, a repeated or already-claimed member,
            // an out-of-range `active` — is `Tree::from_parts`'s job
            // (`validate_node`); this reader only refuses what it cannot
            // read at all, exactly as the `leaf` arm does.
            let active = table
                .get("active")
                .and_then(|v| v.as_integer())
                .map(|a| a.max(0) as usize)
                .unwrap_or(0);
            Ok(Node::Stack { children: members, active })
        }
```

Extend the module doc's node-encoding block with a `kind = "stack"` example and a sentence: "A `stack` holds `members` (tile ids, two or more once healed) and `active` (index into `members`); an older build meets `kind = "stack"` as an unknown kind and refuses that workspace's layout as it refuses any unknown kind." Then correct spec §7's last sentence to say the same (the spec claimed the older reader drops only the child; `node_from_toml` errors on an unknown kind and the workspace parse propagates it).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell session:: 2>&1 | tail -8`
Expected: all pass.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy -p geode-shell --all-targets -- -D warnings
git add crates/geode-shell/src/session.rs docs/superpowers/specs/2026-09-19-geode-tile-stacks-design.md
git commit -m "session: kind = \"stack\" nodes with members and active; spec §7 corrected

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: `StackHandle`, the two trait methods, and every occupant's minimal answer

**Files:**
- Modify: `crates/geode-shell/src/module.rs` (trait, `StackHandle`, placeholder, recording fixture, `Recorded`)
- Modify: `crates/geode-shell/src/shell/tests/occupants.rs` (`WatchingContent`)
- Modify: `crates/geode-blotter/src/content.rs`, `crates/geode-blotter/src/tile.rs`
- Modify: `crates/geode-marketdata/src/content.rs`, `crates/geode-marketdata/src/tile.rs`
- Modify: `crates/geode-diagnostics/src/lib.rs`, `crates/geode-diagnostics/src/tile.rs`

**Interfaces:**
- Produces: `geode_shell::module::StackHandle { pub index: usize, pub len: usize, pub text: SharedString, open: Rc<dyn Fn(&mut Window, &mut App)> }` with `StackHandle::new(index, len, open)` and `open_list(&self, window, cx)`; `TileContent::set_stack(&self, stack: Option<StackHandle>, cx: &mut App)`; `TileContent::title(&self, cx: &App) -> SharedString`; `Recorded::Stack(TileId, Option<(usize, usize)>)`. Each module's tile stores the handle in a `stack: Option<StackHandle>` field (painted in Task 9).

- [ ] **Step 1: Write the failing tests**

In `module.rs`'s existing test module (or a new `#[cfg(test)] mod stack_handle_tests`):

```rust
    #[test]
    fn stack_handle_prepares_its_text_once_and_runs_its_closure() {
        use std::cell::Cell;
        use std::rc::Rc;
        let ran = Rc::new(Cell::new(0));
        let r = ran.clone();
        let h = StackHandle::new(2, 4, move |_w, _cx| r.set(r.get() + 1));
        assert_eq!(h.index, 2);
        assert_eq!(h.len, 4);
        assert_eq!(h.text.as_ref(), "2/4");
        let _ = ran; // `open_list` needs a Window; the closure is exercised in shell/tests/stacks.rs
    }
```

In `crates/geode-diagnostics/src/tile.rs` tests (near `set_visible_false_unwatches_and_notifies`):

```rust
    #[gpui::test]
    fn title_names_the_section(cx: &mut gpui::TestAppContext) {
        let (h, vcx) = open(cx);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.title()).as_ref(), "diagnostics · sources");
    }
```

(Replace `sources` with whatever `Section::default().name()` is — check `commands.rs` line 27.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell module:: 2>&1 | tail -5`
Expected: compile error, `StackHandle` not found.

- [ ] **Step 3: Implement the shell side**

In `module.rs`, after `Delivery`:

```rust
/// What the shell hands a stack member (tile-stacks spec §5.1): its
/// one-based `index` and the stack's `len`, `text` prepared once
/// (`"2/4"`) so no module formats it per frame, and `open_list`, a
/// closure over the shell's own weak entity, so a module opens the
/// shell's list without a path to `ShellView`.
#[derive(Clone)]
pub struct StackHandle {
    pub index: usize,
    pub len: usize,
    pub text: SharedString,
    open: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl StackHandle {
    pub fn new(index: usize, len: usize, open: impl Fn(&mut Window, &mut App) + 'static) -> StackHandle {
        StackHandle { index, len, text: format!("{index}/{len}").into(), open: Rc::new(open) }
    }

    /// Open the shell's transient member list on this tile (spec §5.2).
    pub fn open_list(&self, window: &mut Window, cx: &mut App) {
        (self.open)(window, cx)
    }
}

impl std::fmt::Debug for StackHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StackHandle({}/{})", self.index, self.len)
    }
}
```

(Add `use std::rc::Rc;` and `use gpui::SharedString;` if missing.) Add to `TileContent`, after `set_visible`:

```rust
    /// This tile's place in its stack, or `None` when it is not a member
    /// (tile-stacks spec §5.1). Delivered by `ShellView::ensure_occupants`
    /// on the first render after creation and on every change of
    /// `(index, len)` thereafter, never on an unrelated render. The
    /// module paints `stack.text` first in its header while `len > 1` and
    /// calls `open_list` from the chip's click. Required, not defaulted:
    /// a module that forgot would ship a stack a trader cannot see.
    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App);
    /// The row this tile paints as in the stack list (spec §5.2): the
    /// same words its own header leads with (`risk · book, lhu`,
    /// `CVI · SPX.Z`, `diagnostics · log`).
    fn title(&self, cx: &App) -> SharedString;
```

Placeholder: give `PlaceholderView` a `stack: Option<StackHandle>` field and `PlaceholderContent` a `view: Entity<PlaceholderView>` field (construct it with the view in `create`), then:

```rust
        fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
            self.view.update(cx, |v, cx| {
                v.stack = stack;
                cx.notify();
            });
        }
        fn title(&self, _: &App) -> SharedString {
            SharedString::new_static("empty")
        }
```

Recording fixture: add `Stack(TileId, Option<(usize, usize)>)` to `Recorded`; `RecordingContent` gains `pub stack: RefCell<Option<StackHandle>>`:

```rust
        fn set_stack(&self, stack: Option<StackHandle>, _: &mut App) {
            self.log
                .borrow_mut()
                .push(Recorded::Stack(self.tile, stack.as_ref().map(|s| (s.index, s.len))));
            *self.stack.borrow_mut() = stack;
        }
        fn title(&self, _: &App) -> SharedString {
            format!("rec {}", self.tile.0).into()
        }
```

`WatchingContent` in `shell/tests/occupants.rs`: `fn set_stack(&self, _: Option<StackHandle>, _: &mut App) {}` and `fn title(&self, _: &App) -> SharedString { "watching".into() }`.

- [ ] **Step 4: Implement each module's minimal answer**

Blotter (`tile.rs`): field `stack: Option<StackHandle>` (init `None` in `new`), `pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) { self.stack = stack; cx.notify(); }`, `pub fn title(&self) -> SharedString { format!("{} · {}", self.view_name, GroupingSlots::label_of(&self.last_grouping)).into() }`. `content.rs`: forward both (`self.tile.update(cx, |t, cx| t.set_stack(stack, cx))`, `self.tile.read(cx).title()`).

Market-data (`tile.rs`): field `stack: Option<StackHandle>`, `set_stack` as above, `pub fn title(&self) -> SharedString { match &self.key { Some(k) => format!("{} · {}", self.spec.title, display_key(k)).into(), None => self.spec.title.into() } }`. `content.rs`: forward both.

Diagnostics (`tile.rs`): field `stack: Option<StackHandle>`, `set_stack`, `pub fn title(&self) -> SharedString { format!("diagnostics · {}", self.section.name()).into() }`. `lib.rs`: forward both.

`use geode_shell::module::StackHandle;` in each.

- [ ] **Step 5: Build the whole workspace and run the tests**

Run: `cargo test --workspace 2>&1 | tail -8 && cargo check -p geode-shell --features test-support --all-targets`
Expected: green. Every `impl TileContent` compiles only because it answered both methods.

- [ ] **Step 6: Lint and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/
git commit -m "module: StackHandle and the required set_stack/title on every occupant

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Shell delivery — visible set from `visible_tiles`, `set_stack` once per change, the `notice` segment, and the four actions

**Files:**
- Modify: `crates/geode-shell/src/shell/mod.rs` (fields `notice: Option<&'static str>`, `stack_sent: HashMap<TileId, Option<(usize, usize)>>`)
- Modify: `crates/geode-shell/src/shell/occupants.rs`
- Modify: `crates/geode-shell/src/shell/input.rs` (dispatch arms)
- Modify: `crates/geode-shell/src/shell/status.rs`, `crates/geode-shell/src/shell/render.rs` (the segment)
- Modify: `crates/geode-shell/src/defaults.rs` (actions + bindings)
- Create: `crates/geode-shell/src/shell/tests/stacks.rs`; register `mod stacks;` in `shell/tests/mod.rs`

**Interfaces:**
- Consumes: `Workspaces::{stack_position, stack_active}`, `Workspace::{stack_step, unstack_focused}`, `StackHandle::new`, `Recorded::Stack`.
- Produces: actions `stack::next`, `stack::prev`, `stack::pick`, `stack::unstack`; `ShellView::open_stack_list(&mut self, tile: TileId, window, cx)` is Task 7's — this task calls a stub `pub(super) fn open_stack_list(&mut self, _tile: TileId, _window: &mut Window, _cx: &mut Context<Self>) {}` that Task 7 fills; `ShellView::notice` painted with selector `shell-notice`.

- [ ] **Step 1: Write the failing tests** (`shell/tests/stacks.rs`)

```rust
//! Tile stacks (spec 2026-09-19): delivery of the stack position, the
//! visible set, the four verbs and their notice.

use super::*;
use crate::module::recording::Recorded;
use crate::tiling::TileId;

/// Two tiles side by side, then a "rec" stacked onto the right one:
/// [left | stack(right, top active)]. Returns (cx, shell, log, left, right, top).
pub(super) fn stacked_shell(
    cx: &mut gpui::TestAppContext,
) -> (
    gpui::VisualTestContext,
    Entity<ShellView>,
    std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>,
    TileId,
    TileId,
    TileId,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("tile::add_rec_stacked".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let tiles: Vec<TileId> =
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 3, "{tiles:?}");
    (cx, shell, log, tiles[0], tiles[1], tiles[2])
}

fn stack_events(log: &std::rc::Rc<std::cell::RefCell<Vec<Recorded>>>, tile: TileId) -> Vec<Option<(usize, usize)>> {
    log.borrow()
        .iter()
        .filter_map(|r| match r {
            Recorded::Stack(t, p) if *t == tile => Some(*p),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn a_stacked_add_tells_both_members_their_position_once_and_hides_the_old_one(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, log, left, right, top) = stacked_shell(cx);
    assert_eq!(stack_events(&log, right), vec![None, Some((1, 2))]);
    assert_eq!(stack_events(&log, top), vec![Some((2, 2))]);
    assert_eq!(stack_events(&log, left), vec![None]);
    assert!(
        log.borrow().iter().any(|r| matches!(r, Recorded::Visible(t, false) if *t == right)),
        "the hidden member is told it left the screen: {:?}",
        log.borrow()
    );
    // An unrelated render re-notifies nobody.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
        let _ = window.draw(cx);
    });
    assert_eq!(stack_events(&log, right), vec![None, Some((1, 2))]);
    let _ = shell;
}

#[gpui::test]
fn mod_bracket_cycles_and_the_ring_follows(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, log, _left, right, top) = stacked_shell(cx);
    cx.simulate_keystrokes("alt-]");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right)
    );
    assert!(log.borrow().iter().any(|r| matches!(r, Recorded::Visible(t, true) if *t == right)));
    assert!(log.borrow().iter().any(|r| matches!(r, Recorded::Visible(t, false) if *t == top)));
    assert!(shell.read_with(&cx, |s, _| s.session_dirty));
    cx.simulate_keystrokes("alt-[");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(top)
    );
}

#[gpui::test]
fn a_count_prefix_steps_n_members(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("tile::add_rec_stacked".to_string()), None, window, cx);
        });
    });
    // [right, top, newest]; newest (index 2) focused. 2 × next is
    // (2 + 2) mod 3 = 1: `top`, wrapping past `right`.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::next".to_string()), Some(2), window, cx);
        });
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(top)
    );
    let _ = right;
}

#[gpui::test]
fn a_stack_verb_on_a_plain_tile_leaves_a_notice_the_next_action_clears(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-]");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(shell.read_with(&cx, |s, _| s.notice), Some("not in a stack"));
    assert!(cx.debug_bounds("shell-notice").is_some(), "painted in the status bar");
    cx.simulate_keystrokes("alt-l");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(shell.read_with(&cx, |s, _| s.notice), None);
}

#[gpui::test]
fn unstack_pops_the_focused_member_out_and_the_survivors_are_re_notified(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, log, _left, right, top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::unstack".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.stack_position(top)),
        None
    );
    assert_eq!(stack_events(&log, top).last(), Some(&None));
    assert_eq!(stack_events(&log, right).last(), Some(&None), "a one-member stack collapsed");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().visible_tiles().len()),
        3
    );
}

#[gpui::test]
fn a_cycle_re_arms_the_focus_restore_while_an_abandoned_editor_holds_the_keyboard(cx: &mut gpui::TestAppContext) {
    // Same shape as `input.rs`'s I-3 test: focus a module input, then
    // move which tile has focus by a stack verb; the shell must re-arm.
    let (services, focus) = services_with_recorder_focus();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("tile::add_rec_stacked".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    cx.update(|window, cx| {
        if let Some(handle) = focus.borrow().clone() {
            window.focus(&handle, cx);
        }
    });
    cx.simulate_keystrokes("alt-]");
    assert!(shell.read_with(&cx, |s, _| s.pending_focus_restore));
}
```

Add `mod stacks;` to `shell/tests/mod.rs`. (`services_with_recorder_focus`'s `RecFocus` is the recorder view's own focus handle, filled when its view is created — check `services_with_rec_roster_shipping` for how `last_focus` is populated and adapt the focus step if the handle is filled differently.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support shell::tests::stacks 2>&1 | tail -10`
Expected: `tile::add_rec_stacked` is not registered (three tiles assertion fails) and `stack_events` finds nothing.

- [ ] **Step 3: Register the actions and bindings** (`defaults.rs`)

In `register_builtin_actions`, after the dock actions:

```rust
    // Tile stacks (spec 2026-09-19 §4): cycle the focused member, open the
    // member list, pop the member out. `pick`/`unstack` are palette-only.
    action(reg, "stack::next", "Stack: Next", "Workspace");
    action(reg, "stack::prev", "Stack: Previous", "Workspace");
    action(reg, "stack::pick", "Stack: Pick…", "Workspace");
    action(reg, "stack::unstack", "Stack: Unstack", "Workspace");
```

In `BUILTIN_KEYMAP`'s `workspace` table, after `"ctrl+/" = "dock::toggle_bottom"`:

```toml
"mod+]" = "stack::next"
"mod+[" = "stack::prev"
```

In `register_add_actions`, a fourth row per kind:

```rust
        action(
            reg,
            &format!("tile::add_{kind}_stacked"),
            &format!("{title}: Stack"),
            "Tiles",
        );
```

Replace `parse_add_action`'s return type with an enum and extend it:

```rust
/// How an add row places its tile (spec 2026-09-08 §4.2, tile-stacks
/// spec §6.1): a split in an explicit or setting-resolved direction, or
/// stacked onto the focused tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddPlacement {
    Split(Option<crate::tiling::Orientation>),
    Stacked,
}

pub fn parse_add_action(id: &str) -> Option<(&str, AddPlacement)> {
    use crate::tiling::Orientation;
    let rest = id.strip_prefix("tile::add_")?;
    let (kind, placement) = if let Some(k) = rest.strip_suffix("_horizontal") {
        (k, AddPlacement::Split(Some(Orientation::Horizontal)))
    } else if let Some(k) = rest.strip_suffix("_vertical") {
        (k, AddPlacement::Split(Some(Orientation::Vertical)))
    } else if let Some(k) = rest.strip_suffix("_stacked") {
        (k, AddPlacement::Stacked)
    } else {
        (rest, AddPlacement::Split(None))
    };
    (!kind.is_empty()).then_some((kind, placement))
}
```

Update `register_add_actions_registers_three_rows_per_kind_in_the_tiles_category` to expect the fourth row (`tile::add_blotter_stacked` → `Blotter: Stack`) and rename it `..._four_rows_...`; update `parse_add_action`'s tests for the new type. Fix the two callers of `parse_add_action` in `input.rs` (Task 8 uses the placement; for now map `AddPlacement::Split(d)` to `add_tile(kind, d, ..)` and `Stacked` to the same `add_tile` with `None`, so the repo compiles; Task 8 makes `Stacked` real — **do not skip Task 8**).

- [ ] **Step 4: Deliver `set_stack` and derive the visible set from `visible_tiles`** (`occupants.rs`)

`fill_active_tiles` and `visible_tile_keys`: replace `tree().tiles()` with `tree().visible_tiles()` in the main-tree and dock branches (four sites).

Add the field to `ShellView` (`mod.rs`, near `visible_tiles`): `stack_sent: HashMap<TileId, Option<(usize, usize)>>` (init `HashMap::new()`), and `notice: Option<&'static str>` (init `None`). At the end of `ensure_occupants`, after the visibility diff loops and before the focus backstop:

```rust
        // Stack positions (tile-stacks spec §5.1): every occupant is told
        // its `(index, len)` on its first render and on every change,
        // never on an unrelated render — `stack_sent` remembers the last
        // value sent per tile, and a missing entry means "unsent", so a
        // fresh occupant always hears once, `None` included.
        let weak = cx.entity().downgrade();
        for id in &creation_order {
            let now = self.services.workspaces.stack_position(*id);
            if self.stack_sent.get(id) == Some(&now) {
                continue;
            }
            self.stack_sent.insert(*id, now);
            let Some(o) = self.occupants.get(id) else { continue };
            let handle = now.map(|(index, len)| {
                let weak = weak.clone();
                let tile = *id;
                crate::module::StackHandle::new(index, len, move |window, cx| {
                    let _ = weak.update(cx, |view, cx| view.open_stack_list(tile, window, cx));
                })
            });
            o.content.set_stack(handle, cx);
        }
        self.stack_sent.retain(|id, _| all.contains(id));
```

(`creation_order` is the sorted list of every tile; it is computed above the creation loop and is still in scope. `all` is reassigned to `self.scratch_all_tiles` before this point — read `self.scratch_all_tiles` instead if so.) Add the stub `pub(super) fn open_stack_list(&mut self, _tile: TileId, _window: &mut Window, _cx: &mut Context<Self>) {}` in `occupants.rs` for Task 7 to replace.

- [ ] **Step 5: Dispatch arms and the notice** (`input.rs`, `status.rs`, `render.rs`)

At the top of `dispatch`, right after the tracing line: `self.notice = None;`. Before `let handled = apply_workspace_action(..)`:

```rust
        if action.0 == "stack::next" || action.0 == "stack::prev" {
            // Tile stacks (spec §4): count-aware, so not in the router.
            let n = i64::from(count.unwrap_or(1).max(1));
            let delta = if action.0 == "stack::next" { n } else { -n };
            if self.services.workspaces.active_mut().stack_step(delta) {
                self.session_dirty = true;
                self.note_keyboard_focus_move(window, cx);
            } else {
                self.notice = Some(NOT_IN_A_STACK);
            }
            return;
        }
        if action.0 == "stack::unstack" {
            let rect = self
                .services
                .workspaces
                .active()
                .focused_tile_rect(super::render::content_area(window));
            let orientation = self.add_direction.resolve(None, rect);
            if self.services.workspaces.active_mut().unstack_focused(orientation) {
                self.session_dirty = true;
                self.note_keyboard_focus_move(window, cx);
            } else {
                self.notice = Some(NOT_IN_A_STACK);
            }
            return;
        }
        if action.0 == "stack::pick" {
            match self.services.workspaces.active().focused_tile() {
                Some(tile) => self.open_stack_list(tile, window, cx),
                None => self.notice = Some(NOT_IN_A_STACK),
            }
            return;
        }
```

with `pub(super) const NOT_IN_A_STACK: &str = "not in a stack";` in `input.rs`. `status_bar` gains a parameter `notice: Option<&str>` (after `restart_message`), painted:

```rust
    if let Some(message) = notice {
        // A verb's one-line refusal (tile stacks spec §4): muted, cleared
        // by the next dispatch.
        bar = bar.left(
            div()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "shell-notice".to_string())
                .child(message.to_string()),
        );
    }
```

and `render.rs` passes `self.notice`.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-shell --features test-support 2>&1 | tail -10`
Expected: all pass, including `shell::tests::stacks`.

- [ ] **Step 7: Lint and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-shell
git commit -m "shell: stack verbs, mod+[ / mod+], set_stack delivery once per change, visible set from visible_tiles, notice segment

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: The transient member list

**Files:**
- Create: `crates/geode-shell/src/shell/stacklist.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (`pub mod stacklist;`, field `stack_list: Option<stacklist::StackList>`)
- Modify: `crates/geode-shell/src/shell/occupants.rs` (replace the stub `open_stack_list`)
- Modify: `crates/geode-shell/src/shell/input.rs` (key branch)
- Modify: `crates/geode-shell/src/shell/render.rs` (overlay)
- Test: `crates/geode-shell/src/shell/tests/stacks.rs`

**Interfaces:**
- Consumes: `TileContent::title`, `Workspace::{region_of, focus_main_tile, focus_dock_tile}`, `Tree::stack_position`, `listrow::row_paint`, `dialog::overlay_panel_shadow`, `scale::design_px`.
- Produces: `stacklist::StackList { tile: TileId, members: Vec<TileId>, highlighted: usize }`, `stacklist::{step, jump}` (pure), `stacklist::render(..)`; `ShellView::{open_stack_list, close_stack_list, activate_stack_member, handle_stack_list_key}`.

- [ ] **Step 1: Write the failing tests** (append to `shell/tests/stacks.rs`)

```rust
#[gpui::test]
fn stack_pick_opens_the_list_highlighting_the_active_member(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let list = shell.read_with(&cx, |s, _| s.stack_list.clone()).expect("open");
    assert_eq!(list.members, vec![right, top]);
    assert_eq!(list.highlighted, 1, "the showing member");
    assert!(cx.debug_bounds("stack-list").is_some());
    assert!(cx.debug_bounds("stack-list-row-0").is_some());
}

#[gpui::test]
fn a_digit_enter_and_escape_do_what_the_spec_says(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, top) = stacked_shell(cx);
    let focused = |cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |s, _| s.services.workspaces.active().tree().focused())
    };
    cx.simulate_keystrokes("ctrl-k"); // palette, then the pick row by action
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    cx.simulate_keystrokes("1");
    assert_eq!(focused(&cx), Some(right), "a digit activates at once");
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()), "and closes the list");

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    cx.simulate_keystrokes("j");
    cx.simulate_keystrokes("enter");
    assert_eq!(focused(&cx), Some(top), "j then enter activates the highlighted row");

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    cx.simulate_keystrokes("k");
    cx.simulate_keystrokes("escape");
    assert_eq!(focused(&cx), Some(top), "escape changes nothing");
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));
}

#[gpui::test]
fn a_row_click_activates_and_a_click_outside_closes(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, _top) = stacked_shell(cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let row = cx.debug_bounds("stack-list-row-0").unwrap();
    cx.simulate_click(row.center(), gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right)
    );
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let catcher = cx.debug_bounds("stack-list-click-catcher").unwrap();
    cx.simulate_click(gpui::point(catcher.right() - px(4.0), catcher.bottom() - px(4.0)), gpui::Modifiers::none());
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right),
        "an outside click changes nothing"
    );
}

#[gpui::test]
fn the_handle_opens_the_list_on_its_own_tile_and_ctrl_k_closes_it(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, log, left, right, _top) = stacked_shell(cx);
    // Focus `left`, then open through `right`'s handle: the list must be
    // about `right`'s stack and `right`'s tile must take focus first.
    cx.simulate_keystrokes("alt-h");
    let handle = {
        let log = log.borrow();
        let _ = &log; // the recorder keeps the handle on its content; reach it through the shell
        shell.read_with(&cx, |s, _| {
            s.occupants
                .get(&right)
                .and_then(|o| o.content.stack_handle_for_test())
        })
    };
    let handle = handle.expect("right holds a handle");
    cx.update(|window, cx| handle.open_list(window, cx));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()),
        Some(right)
    );
    assert_eq!(shell.read_with(&cx, |s, _| s.stack_list.as_ref().map(|l| l.tile)), Some(right));
    assert_ne!(left, right);
    cx.simulate_keystrokes("ctrl-k");
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()), "ctrl+k closes it for the palette");
}

#[gpui::test]
fn pick_on_a_plain_tile_refuses_with_the_notice(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("stack::pick".to_string()), None, window, cx);
        });
    });
    assert!(shell.read_with(&cx, |s, _| s.stack_list.is_none()));
    assert_eq!(shell.read_with(&cx, |s, _| s.notice), Some("not in a stack"));
}
```

`stack_handle_for_test` is a `#[cfg(any(test, feature = "test-support"))]` default method on `TileContent` returning `Option<StackHandle>` (`None` by default), overridden by `RecordingContent` to return `self.stack.borrow().clone()`. Add it to the trait in this task.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support shell::tests::stacks::stack_pick 2>&1 | tail -5`
Expected: compile error, no field `stack_list`.

- [ ] **Step 3: Write the pure core and the renderer** (`shell/stacklist.rs`)

```rust
//! The transient stack-member list (tile-stacks spec §5.2): shell-owned,
//! painted in the palette's mould under the focused tile's header, no
//! `Input`, so no focus dance. The pure state is [`StackList`]; the two
//! motion rules are [`step`] and [`jump`]; [`render`] paints from
//! prepared rows only.

use gpui::prelude::*;
use gpui::{App, MouseButton, Pixels, SharedString, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::listrow::row_paint;
use super::scale;
use crate::fonts;
use crate::tiling::{Rect, TileId};

/// Row height on the design scale (PopupMenu's geometry).
pub const ROW_HEIGHT: f32 = 26.0;
/// Where the panel hangs below the tile's top edge: the module header
/// strips share a 22 px height, plus the ring.
pub const TOP_INSET: f32 = 24.0;
pub const MAX_WIDTH: f32 = 320.0;
pub const MIN_WIDTH: f32 = 160.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackList {
    /// The tile whose stack this lists (the focused tile while open).
    pub tile: TileId,
    /// Every member in stack order.
    pub members: Vec<TileId>,
    /// The row `enter` activates; opens on the active member.
    pub highlighted: usize,
}

/// A prepared row: title from `TileContent::title`, kind dimmed.
pub struct Row {
    pub title: SharedString,
    pub kind: &'static str,
}

/// `j`/`k`/arrows: a bare ±1 wraps (the one motion rule, spec §20).
pub fn step(list: &mut StackList, delta: i64) {
    let len = list.members.len() as i64;
    if len == 0 {
        return;
    }
    list.highlighted = (list.highlighted as i64 + delta).rem_euclid(len) as usize;
}

/// A digit `1`–`9`: the member at that one-based index, if any.
pub fn jump(list: &StackList, digit: u32) -> Option<TileId> {
    list.members.get(digit.checked_sub(1)? as usize).copied()
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    list: &StackList,
    rows: &[Row],
    tile_rect: Rect,
    rem_size: Pixels,
    on_row_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let paint = row_paint(theme);
    let row_height = scale::design_px(ROW_HEIGHT, rem_size);
    let width = scale::design_px(MAX_WIDTH, rem_size)
        .min((tile_rect.w - 8.0).max(scale::design_px(MIN_WIDTH, rem_size)));
    let left = tile_rect.x + 2.0;
    let top = tile_rect.y + scale::design_px(TOP_INSET, rem_size);

    let mut panel = v_flex()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .shadow(super::dialog::overlay_panel_shadow())
        .debug_selector(|| "stack-list".to_string())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

    for (i, row) in rows.iter().enumerate() {
        let is_highlighted = i == list.highlighted;
        let on_click = on_row_click.clone();
        let mut el = h_flex()
            .id(("stack-list-row", i))
            .w_full()
            .h(px(row_height))
            .items_center()
            .gap_3()
            .px_2()
            .rounded(theme.radius)
            .debug_selector(move || format!("stack-list-row-{i}"))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                on_click(i, window, cx);
            });
        if is_highlighted {
            el = el.bg(paint.active).text_color(paint.text);
        } else {
            el = el.hover(|s| s.bg(paint.hover));
        }
        panel = panel.child(
            el.child(
                div()
                    .font_family(fonts::MONO)
                    .text_color(theme.muted_foreground)
                    .w(px(row_height / 2.0))
                    .child(SharedString::from((i + 1).to_string())),
            )
            .child(div().flex_1().child(row.title.clone()))
            .child(div().text_xs().text_color(theme.muted_foreground).child(row.kind)),
        );
    }
    panel
}
```

(The index digit string is one small allocation per row per frame while the list is open, on a list of at most nine rows; prepare it in `Row` if the reviewer objects.)

- [ ] **Step 4: Wire the shell** (`occupants.rs` replaces the stub; `input.rs`; `render.rs`)

```rust
    /// Open the member list on `tile` (spec §5.2): focus that tile first
    /// (a marker click on an unfocused tile must open THAT tile's list),
    /// refuse with the notice if it is not a member, close the palette
    /// and any command line, and highlight the active member.
    pub(super) fn open_stack_list(&mut self, tile: TileId, window: &mut Window, cx: &mut Context<Self>) {
        let ws = self.services.workspaces.active_mut();
        match ws.region_of(tile) {
            Some(crate::tiling::FocusRegion::Main) => {
                ws.focus_main_tile(tile);
            }
            Some(crate::tiling::FocusRegion::Dock(side)) => {
                ws.focus_dock_tile(side, tile);
            }
            None => return,
        }
        let Some((index, _)) = self.services.workspaces.active().stack_position(tile) else {
            self.notice = Some(super::input::NOT_IN_A_STACK);
            cx.notify();
            return;
        };
        let members = self.stack_members_of(tile);
        self.close_palette(window, cx);
        self.leave_command_line(window, cx);
        self.stack_list = Some(super::stacklist::StackList { tile, members, highlighted: index - 1 });
        self.note_keyboard_focus_move(window, cx);
        cx.notify();
    }

    /// The members of `tile`'s stack in stack order (`Tree::tiles` walks
    /// members in order, and `stack_position` says which are members of
    /// the same stack by sharing `len` and being contiguous — but the
    /// honest way is to ask the tree). Add `Tree::stack_members(&self, id)
    /// -> Option<Vec<TileId>>` in `tree.rs` (a `find_stack` lookup
    /// returning `children.clone()`) and `Workspace::stack_members` over
    /// `region_of`, and call that here.
    fn stack_members_of(&self, tile: TileId) -> Vec<TileId> {
        self.services.workspaces.active().stack_members(tile).unwrap_or_default()
    }

    pub(super) fn close_stack_list(&mut self, cx: &mut Context<Self>) {
        if self.stack_list.take().is_some() {
            cx.notify();
        }
    }

    /// Make `id` the painted, focused member and close the list.
    pub(super) fn activate_stack_member(&mut self, id: TileId, window: &mut Window, cx: &mut Context<Self>) {
        let ws = self.services.workspaces.active_mut();
        let moved = match ws.region_of(id) {
            Some(crate::tiling::FocusRegion::Main) => ws.focus_main_tile(id),
            Some(crate::tiling::FocusRegion::Dock(side)) => ws.focus_dock_tile(side, id),
            None => false,
        };
        if moved {
            self.session_dirty = true;
            self.note_keyboard_focus_move(window, cx);
        }
        self.close_stack_list(cx);
    }
```

In `handle_key_down`, after the palette branch (`if self.palette.is_some() { … return; }`):

```rust
        if let Some(list) = self.stack_list.clone() {
            // The member list owns the keyboard while open (spec §5.2).
            let key = event.keystroke.key.as_str();
            match key {
                "escape" => self.close_stack_list(cx),
                "j" | "down" => {
                    let mut l = list;
                    stacklist::step(&mut l, 1);
                    self.stack_list = Some(l);
                }
                "k" | "up" => {
                    let mut l = list;
                    stacklist::step(&mut l, -1);
                    self.stack_list = Some(l);
                }
                "enter" => {
                    if let Some(id) = list.members.get(list.highlighted).copied() {
                        self.activate_stack_member(id, window, cx);
                    }
                }
                d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => {
                    if let Some(id) = stacklist::jump(&list, u32::from(d.as_bytes()[0] - b'0')) {
                        self.activate_stack_member(id, window, cx);
                    }
                }
                _ => {}
            }
            cx.notify();
            return;
        }
```

This branch must sit AFTER the `is_palette_toggle` check so `ctrl+k` still reaches `toggle_palette`; in `toggle_palette`'s open arm call `self.close_stack_list(cx)` first. In `dispatch`, next to `self.notice = None;`, add `self.stack_list = None;` (any action closes it — the list's own keys never reach `dispatch`). In `render`, beside the generic command-line check at the top: drop the list when `self.stack_list.as_ref().is_some_and(|l| self.services.workspaces.active().focused_tile() != Some(l.tile) || self.services.workspaces.active().stack_position(l.tile).is_none())`.

In `render.rs`, after the command-line `when_some` and before the palette block:

```rust
            .when_some(
                self.stack_list.as_ref().zip(focused_rect),
                |el, (list, rect)| {
                    let rows: Vec<stacklist::Row> = list
                        .members
                        .iter()
                        .map(|id| match self.occupants.get(id) {
                            Some(o) => stacklist::Row { title: o.content.title(cx), kind: o.kind },
                            None => stacklist::Row { title: "empty".into(), kind: "placeholder" },
                        })
                        .collect();
                    let weak = cx.entity().downgrade();
                    let members = list.members.clone();
                    let on_row_click = move |i: usize, window: &mut Window, cx: &mut App| {
                        let Some(id) = members.get(i).copied() else { return };
                        let _ = weak.update(cx, |view, cx| view.activate_stack_member(id, window, cx));
                    };
                    let panel = stacklist::render(list, &rows, rect, rem_size, on_row_click, cx);
                    el.child(
                        div()
                            .id("stack-list-click-catcher")
                            .absolute()
                            .left(px(0.))
                            .top(px(0.))
                            .w(px(width))
                            .h(px(viewport_height))
                            .debug_selector(|| "stack-list-click-catcher".to_string())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, _event, _window, cx| view.close_stack_list(cx)),
                            )
                            .child(panel),
                    )
                },
            )
```

(`rows` is built per frame only while the list is open — nine `SharedString` clones at most; `title()` on the blotter formats a `String`, so cache it: have `BlotterTile::title` return a `SharedString` field refreshed where `view_name`/`last_grouping` change, in Task 9.)

- [ ] **Step 5: Run the tests**

Run: `cargo test -p geode-shell --features test-support shell::tests::stacks 2>&1 | tail -10`
Expected: all pass.

- [ ] **Step 6: Lint and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-shell
git commit -m "shell: the transient stack-member list — pick, digits, enter, click, escape

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: Entry doors — `{Kind}: Stack` placement and the centre drop

**Files:**
- Modify: `crates/geode-shell/src/shell/add_tile.rs`
- Modify: `crates/geode-shell/src/shell/input.rs` (the `parse_add_action` arm, `duplicate_tile` callers)
- Modify: `crates/geode-shell/src/shell/drag.rs`
- Modify: `crates/geode-shell/src/shell/tests/drag.rs` (the centre-drop test)
- Test: `crates/geode-shell/src/shell/tests/stacks.rs`

**Interfaces:**
- Consumes: `AddPlacement`, `Workspaces::stack_active`, `Workspace::drop_stack`.
- Produces: `ShellView::add_tile(&mut self, kind: &str, placement: AddPlacement, state: Option<toml::Table>, window, cx)`.

- [ ] **Step 1: Write the failing tests**

Append to `shell/tests/stacks.rs`:

```rust
#[gpui::test]
fn a_stacked_add_on_a_placeholder_or_empty_region_fills_or_roots_like_a_split(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("tile::add_rec_stacked".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 1, "empty region: the tile is the root");
    assert_eq!(shell.read_with(&cx, |s, _| s.services.workspaces.stack_position(tiles[0])), None);
    assert!(log.borrow().iter().any(|r| matches!(r, Recorded::Created(t, _) if *t == tiles[0])));
}

#[gpui::test]
fn a_stacked_add_on_a_member_lands_after_it(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _log, _left, right, top) = stacked_shell(cx);
    cx.simulate_keystrokes("alt-]"); // right active
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("tile::add_rec_stacked".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 4);
    let new = tiles[2];
    assert_eq!(&tiles[1..], &[right, new, top]);
    assert_eq!(shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().focused()), Some(new));
}

#[gpui::test]
fn a_centre_drop_across_regions_stacks_into_the_targets_dock(cx: &mut gpui::TestAppContext) {
    // Pure verb coverage lives in workspaces.rs; this pins the shell's
    // arm: the drop handler calls `drop_stack`, not `drop_swap`.
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    let (a, b) = (tiles[0], tiles[1]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("dock::move_left".to_string()), None, window, cx);
        });
        let _ = window.draw(cx);
    });
    // b is in the left dock and focused; drop a onto b's centre.
    let grab = super::drag::main_tile_point_pub(&mut cx, &shell, a, 0.5, 0.5);
    let drop = super::drag::dock_tile_point_pub(&mut cx, &shell, crate::tiling::DockSide::Left, b, 0.5, 0.5);
    cx.simulate_mouse_down(grab, gpui::MouseButton::Left, super::drag::alt_held_pub());
    cx.simulate_mouse_move(drop, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(drop, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(shell.read_with(&cx, |s, _| s.services.workspaces.stack_position(a)), Some((2, 2)));
    assert!(shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().is_empty()));
}
```

Expose `main_tile_point`, `alt_held` and a `dock_tile_point` (same math over `dock_rects` from `dock_layout`) as `pub(super)` from `tests/drag.rs` under the `_pub` names, or move them into `tests/mod.rs`. Rewrite `mod_dragging_onto_a_tiles_center_swaps_the_pair` as `mod_dragging_onto_a_tiles_center_stacks_the_pair`: expect `tiles() == vec![right, left]`, `stack_position(left) == Some((2, 2))`, focus on `left`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support shell::tests::stacks::a_stacked_add_on_a_member 2>&1 | tail -5`
Expected: FAIL (`tiles.len() == 4` holds but the new tile split beside the stack rather than joining it, or the test panics on the placement).

- [ ] **Step 3: Implement**

`add_tile.rs`:

```rust
    pub fn add_tile(
        &mut self,
        kind: &str,
        placement: AddPlacement,
        state: Option<toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.services.workspaces.active().focused_tile();
        if let Some(tile) = focused
            && self.occupant_kind(tile) == Some(PLACEHOLDER_KIND)
        {
            // unchanged placeholder-fill body
            return;
        }
        // Tile stacks (spec §6.1): stacked onto the focused tile; with
        // nothing focused (an empty region) fall through to the split
        // path, which makes the tile the root — a stack of one is
        // meaningless.
        if placement == AddPlacement::Stacked
            && let Some(id) = self.services.workspaces.stack_active()
        {
            self.pending_tiles.insert(id, PendingTile { kind: kind.to_string(), state });
            self.session_dirty = true;
            cx.notify();
            return;
        }
        let direction = match placement {
            AddPlacement::Split(d) => d,
            AddPlacement::Stacked => None,
        };
        // unchanged split body from `let rect = …` on, using `direction`
    }
```

`duplicate_tile` passes `AddPlacement::Split(Some(direction))`; `open_module` passes `AddPlacement::Split(None)`; `input.rs`'s add arm passes the parsed placement straight through. `drag.rs`: the `DropZone::Center` arm becomes `ws.drop_stack(drag.tile, id)`. Update the drag-arm doc comments that say "swap".

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell --features test-support 2>&1 | tail -10`
Expected: all pass, the renamed drag test included.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-shell
git commit -m "shell: {Kind}: Stack placement and the centre drop adds to the target's stack

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: The marker chip in every module header

**Files:**
- Modify: `crates/geode-blotter/src/tile.rs`
- Modify: `crates/geode-marketdata/src/header.rs`, `crates/geode-marketdata/src/tile.rs`
- Modify: `crates/geode-diagnostics/src/tile.rs`
- Modify: `crates/geode-shell/src/module.rs` (placeholder view)
- Tests: each crate's existing header tests plus one per module below.

**Interfaces:**
- Consumes: `StackHandle { text, open_list }` stored in Task 5's `stack` fields; `geode_shell::shell::chip::{chip_paint, Tone}`.
- Produces: a `stack-marker-{tile}` element in each header while `len > 1`.

- [ ] **Step 1: Write the failing tests**

Blotter (`tile.rs` tests, where a `BlotterTile` is opened in a `VisualTestContext` — follow the nearest existing header test):

```rust
    #[gpui::test]
    fn the_stack_marker_paints_only_while_a_member(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_blotter(cx); // the file's existing fixture name
        assert!(vcx.debug_bounds(&format!("stack-marker-{}", h.id.0)).is_none());
        h.tile.update(&mut vcx, |t, cx| {
            t.set_stack(Some(geode_shell::module::StackHandle::new(2, 4, |_, _| {})), cx);
        });
        vcx.update(|window, cx| { let _ = window.draw(cx); });
        let marker = vcx.debug_bounds(&format!("stack-marker-{}", h.id.0)).expect("painted");
        let header = vcx.debug_bounds(&format!("blotter-header-{}", h.id.0)).unwrap();
        assert!(marker.left() - header.left() < px(20.0), "first in the strip");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.title()).as_ref(), "risk · book, lhu"); // adapt to the fixture's view/grouping
    }
```

Write the same shape for the market-data tile (`marketdata-header-{id}`) and the diagnostics tile (`diagnostics-header-{id}`), and for the placeholder a shell window test in `shell/tests/stacks.rs`: stack two tiles via a raw `Workspaces` restore with no `rec` roster (so both are placeholders), draw, and assert `stack-marker-{id}` exists for the active one.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-blotter the_stack_marker 2>&1 | tail -5`
Expected: FAIL, `expect("painted")`.

- [ ] **Step 3: Implement the chip once per module**

Blotter: in `render`, immediately after `let mut header = h_flex()…debug_selector(..)` and BEFORE the `view_name` child:

```rust
        if let Some(stack) = self.stack.as_ref().filter(|s| s.len > 1) {
            let open = stack.clone();
            header = header.child(
                div()
                    .id(ElementId::NamedInteger(SharedString::new_static("stack-marker"), self.tile.0))
                    .text_color(neutral_chip.text)
                    .when_some(neutral_chip.fill, |el, fill| el.bg(fill))
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .debug_selector(|| format!("stack-marker-{}", self.tile.0))
                    .child(stack.text.clone())
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        open.open_list(window, cx);
                    }),
            );
        }
```

Cache the title: add `title: SharedString` to `BlotterTile`, set wherever `view_name` or `last_grouping` changes (grep `last_grouping =`), and return it from `title()`.

Market-data: `header::render` gains a parameter `stack: Option<&StackHandle>` painted as child 0 (before the kind badge) with the identical chip; `tile.rs` passes `self.stack.as_ref()`. Diagnostics: same chip as the first child before `header_text`. Placeholder: `PlaceholderView::render` paints the chip above the hint when `stack.len > 1` (a `v_flex` of the chip row and the hint).

- [ ] **Step 4: Run the workspace tests**

Run: `cargo test --workspace 2>&1 | tail -8`
Expected: green.

- [ ] **Step 5: Lint and commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/
git commit -m "modules: the stack marker chip first in every header, cached blotter title

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 10: Mutation entries, docs, anchors

**Files:**
- Modify: `scripts/mutation-check.sh`
- Modify: `CLAUDE.md`
- Modify: `docs/superpowers/specs/2026-08-28-geode-foundation-design.md` (§3.1 line)
- Modify: `crates/geode-shell/src/tiling/dropzones.rs` (confirm the doc from Task 3)

- [ ] **Step 1: Add one `run_mutation` entry per behaviour**, in a new `# ---- tile stacks (spec 2026-09-19)` section, each naming its test as the 6th argument. Copy each anchor verbatim from the implemented line:

| Name | File | Break | Test |
|---|---|---|---|
| `stacks: layout emits the active member` | tree.rs | `children[*active]` → `children[0]` in `layout_node` | `layout_emits_only_the_active_member_over_the_whole_slot` (needs the fixture's active ≠ 0: it is 1) |
| `stacks: step wraps` | tree.rs | `.rem_euclid(len)` → `.min(len - 1)` in `stack_step` | `stack_step_cycles_with_wrap_and_a_count` |
| `stacks: fullscreen follows a cycle` | tree.rs | delete the `if self.fullscreen == Some(outgoing)` block in `activate` | `fullscreen_follows_a_cycle` |
| `stacks: closing the last member activates the previous` | tree.rs | `active.min(n - 1)` → `active.min(n)` … pick a mutation that keeps it compiling: `if ix < active { active - 1 } else { active.min(n - 1) }` → `active.min(n - 1)` | `closing_the_active_member_activates_and_focuses_the_next_one` |
| `stacks: closing a member refocuses its own stack` | tree.rs | `Some(s) if self.contains(s) =>` → `Some(s) if false && self.contains(s) =>` | `closing_the_active_member_activates_and_focuses_the_next_one` |
| `stacks: move pops a member out` | tree.rs | `if self.stack_position(focused).is_some()` → `if false` | `move_direction_pops_a_member_out_beside_its_stack` |
| `stacks: a focused member is activated` | tree.rs | `self.activate(id);` in `set_focus` → `` | `focusing_a_hidden_member_activates_it` |
| `stacks: restore dedupes members` | tree.rs | `if !seen.contains(&id) && !kept.contains(&id)` → `if true` | `from_parts_heals_a_stack_rather_than_refusing_it` |
| `stacks: restore clamps active` | tree.rs | `if active < n { active } else { 0 }` → `active` | `from_parts_heals_a_stack_rather_than_refusing_it` (expect a panic caught as a failure) |
| `stacks: session writes members` | session.rs | `"members".to_string()` → `"children".to_string()` | `round_trips_a_stack_in_the_main_tree_and_in_a_dock` |
| `stacks: set_stack is sent once per change` | occupants.rs | `if self.stack_sent.get(id) == Some(&now) { continue; }` → `` | `a_stacked_add_tells_both_members_their_position_once_and_hides_the_old_one` |
| `stacks: hidden members leave the visible set` | occupants.rs | the `fill_active_tiles` `visible_tiles()` → `tiles()` (main-tree site) | same test (the `Visible(right, false)` assertion) |
| `stacks: a stacked add joins the stack` | add_tile.rs | `placement == AddPlacement::Stacked` → `false` | `a_stacked_add_on_a_member_lands_after_it` |
| `stacks: a centre drop stacks` | drag.rs | `ws.drop_stack(drag.tile, id)` → `ws.drop_split(drag.tile, id, Direction::Right)` | `mod_dragging_onto_a_tiles_center_stacks_the_pair` |
| `stacks: the list opens on the active member` | occupants.rs | `highlighted: index - 1` → `highlighted: 0` | `stack_pick_opens_the_list_highlighting_the_active_member` |
| `stacks: a digit activates` | input.rs | the digit arm's `activate_stack_member` call → `` | `a_digit_enter_and_escape_do_what_the_spec_says` |
| `stacks: the marker is gated on len > 1` | blotter tile.rs | `.filter(\|s\| s.len > 1)` → `.filter(\|_\| true)` | `the_stack_marker_paints_only_while_a_member` |

- [ ] **Step 2: Run the new entries and the anchor check**

Run: `zsh scripts/mutation-check.sh "stacks:" 2>&1 | tail -25 && zsh scripts/mutation-check.sh --anchors-only`
Expected: every entry `caught`, no `SURVIVED`, anchors clean (exit 0). A `SURVIVED` means the named test cannot see that branch — fix the test, not the entry.

- [ ] **Step 3: Docs**

`CLAUDE.md`: add a **Tile stacks (2026-09-19)** paragraph after the "Adding tiles" one: the `Node::Stack` model and that layout emits only the active member; `set_focus` as the one focus door (a focused member is always active); the two required `TileContent` methods and the once-per-change `stack_sent` rule; the marker chip's placement rule; the list overlay; `mod+[`/`mod+]`; `{Kind}: Stack`; the centre drop's meaning change and that keyboard move pops a member out; the `notice` status segment; the session `kind = "stack"` node and its healing; and that an older build refuses a layout carrying one. Foundation spec §3.1: change the "tabbed/stacked container modes for dense workspaces" line to point at the stacks spec. Confirm the `dropzones.rs` doc from Task 3 no longer says "swap".

- [ ] **Step 4: Full verification and commit**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace 2>&1 | tail -5 && cargo bench --workspace --no-run 2>&1 | tail -2 && cargo check -p geode-shell --features test-support --all-targets
git add scripts/mutation-check.sh CLAUDE.md docs/
git commit -m "docs + harness: tile stacks

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Self-review

**Spec coverage.** §3 model → Tasks 1–2 (variant, layout, focus, fullscreen, split beside, close, move, docks via the shared `Tree`). §4 verbs and keys → Task 6 (actions, bindings, count, notice, focus re-arm); `stack::unstack`'s direction via `AddDirection::resolve` → Task 6. §5.1 marker and trait → Tasks 5 and 9; once-per-change delivery and first-render contract → Task 6. §5.2 list → Task 7 (anchor, rows, keys, click, outside click, `ctrl+k`, closes on any dispatch and when the tile stops being the focused member, refusal notice, titles). §6.1 add row and placement, placeholder/empty fallback → Tasks 6 (registration) and 8. §6.2 centre drop, reorder, cross-region, keyboard move unchanged on a leaf → Tasks 3 and 8. §7 session shape, healing, older-reader note corrected → Task 4. §8 visible set → Task 6. §9 tests → each task; harness → Task 10. §10 docs → Task 10.

**Placeholders.** None: every step carries code or an exact command. Two places delegate a small helper by description with its full signature (`Tree::stack_members`/`Workspace::stack_members` in Task 7 Step 4, and the `_pub` drag helpers in Task 8 Step 1), each with its one-line body stated.

**Type consistency.** `stack_position` returns `Option<(usize, usize)>` one-based everywhere (tree, workspace, workspaces, `Recorded::Stack`, `StackHandle::{index, len}`); `StackList.highlighted` is zero-based, seeded from `index - 1`. `AddPlacement` is defined in Task 6 and consumed in Task 8. `drop_swap` is deleted in Task 3 and its shell caller switched in Task 8 (with the compile note in Task 3). `NOT_IN_A_STACK` lives in `input.rs` and is read by `occupants.rs`.
