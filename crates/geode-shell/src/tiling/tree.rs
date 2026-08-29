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

    #[allow(dead_code)]
    pub(crate) fn right(&self) -> f32 {
        self.x + self.w
    }

    #[allow(dead_code)]
    pub(crate) fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Edge-adjacency tolerance for unit-space geometry comparisons.
#[allow(dead_code)]
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
        if let Some(fs) = self.fullscreen
            && self.contains(fs)
        {
            return vec![(fs, bounds)];
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

fn remove_leaf(node: Node, target: TileId) -> Option<Node> {
    match node {
        Node::Leaf(id) if id == target => None,
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
}
