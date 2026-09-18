//! Fixed dock regions (dock-regions task, generalized by the dock-trees
//! task): three per-workspace regions — left, right, bottom — each holding
//! a full tiling [`Tree`] alongside the main i3-style tree, for the
//! "always there" panes a desk keeps pinned (a watchlist on the left, a
//! detail pane on the right, a log strip on the bottom) without giving up
//! the tree for the main working set. Originally a dock held at most one
//! tile; the dock-trees generalization swapped that `Option<TileId>` for a
//! `Tree`, so a dock gets splits, orientations, directional focus/move,
//! divider resize, and focus memory by *reusing* the tree — no parallel
//! layout logic exists here, and none may be added. Pure data, no gpui
//! (spec §10.3) — the [`Workspace`](super::Workspace) verbs consult and
//! mutate these; rendering consumes [`layout`] for the dock frames and each
//! dock's own `Tree::layout` for the tiles within.
//!
//! **Inventory decision (not gpui-component's `dock` module):** the
//! pinned release ships the dock framework across two crates —
//! `gpui-component-0.6.2/src/dock/` (`Panel`/`PanelView`/`TabPanel`) and
//! `gpui-base-0.6.2/src/dock/` (`DockArea`, `PaneTree`, `DockAreaState`,
//! drag-and-drop) — that is a competing layout *and persistence* system:
//! it owns its own split tree, its own drag-and-drop docking, its own
//! serde-based `DockAreaState`
//! save/restore, and its own notion of which panel is active. Geode already
//! has all of those seams, deliberately elsewhere: the split tree is
//! `tiling::Tree` (the single geometry authority `Tree::layout`, which hjkl
//! navigation shares — "what you see is what hjkl navigates"), persistence
//! is `session.rs`'s hand-built tolerant TOML (heal-and-warn, never panic —
//! serde derives reject exactly the hostile shapes it must heal), and focus
//! is the keymap engine's job, not a widget's. Adopting `DockArea` would
//! mean either running two layout trees with two persistence formats side
//! by side or rebasing the whole Phase-1 shell onto gpui-component's, and
//! its panel chrome (tabs, toolbars, drag handles) is mouse-first where
//! Geode is keyboard-first (PHILOSOPHY.md: every action keyboard-reachable).
//! So docks are a couple hundred lines of our own pure state here, rendered
//! with the exact same tile chrome the tree already uses — the same kind of
//! decision `shell/sidebar.rs` records for gpui-component's `Sidebar<E>`.
//! The dock-trees generalization only *strengthened* that reasoning: once a
//! dock IS a `Tree`, every tree behavior arrives for free through the one
//! layout engine, where `DockArea` would have demanded a second one.

use super::tree::{Rect, TileId, Tree};

/// Which fixed dock region. There are exactly three — no top dock: the
/// toolbar owns the top edge, and a top dock would fight it visually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSide {
    Left,
    Right,
    Bottom,
}

impl DockSide {
    /// All sides, in a stable order (left, right, bottom) — iteration,
    /// session save, and healing walk docks in this order so "first claim
    /// wins" tie-breaks are deterministic.
    pub const ALL: [DockSide; 3] = [DockSide::Left, DockSide::Right, DockSide::Bottom];
}

/// Default fraction of the content area a dock occupies (width for
/// left/right, height for bottom).
pub const DOCK_DEFAULT_SIZE: f32 = 0.25;
/// Smallest fraction a dock may be resized to.
pub const DOCK_MIN_SIZE: f32 = 0.10;
/// Largest fraction a dock may be resized to.
pub const DOCK_MAX_SIZE: f32 = 0.50;

/// One dock region's state: a full tiling [`Tree`] plus the dock frame's
/// own `visible`/`size`. `visible` and the tree's emptiness vary
/// independently — a hidden dock keeps its whole tree (toggle it back and
/// every tile, split, and its focus memory are still there), and a visible
/// dock may be empty (it renders a hint inviting a `dock::move_*`).
///
/// Invariant: a dock tree never has a fullscreen tile — fullscreen is a
/// main-tree concept (`mod+f` is a claimed no-op while a dock holds focus),
/// and [`Dock::from_parts`] clears any fullscreen a hostile session file
/// smuggles in (the session layer warns; this just enforces).
#[derive(Debug, Clone, PartialEq)]
pub struct Dock {
    /// The dock's own tiling tree. Every `TileId` here lives in exactly one
    /// of the workspace's four trees (main + 3 docks) — enforced by the
    /// `Workspace` verbs (which only ever *move* ids, never duplicate them)
    /// and healed on session restore (`Workspace::from_parts` /
    /// `Workspaces::from_parts` drop duplicate leaves, tree-wins /
    /// first-claim-wins).
    tree: Tree,
    visible: bool,
    /// Fraction of the content area this dock occupies when visible: width
    /// for left/right, height (of the remaining center column) for bottom.
    /// Always within [`DOCK_MIN_SIZE`]..=[`DOCK_MAX_SIZE`] — every write
    /// goes through [`Dock::set_size`], which clamps. `Default` is a
    /// manual impl (not derived) solely because this defaults to
    /// [`DOCK_DEFAULT_SIZE`], not 0.0.
    size: f32,
}

impl Default for Dock {
    fn default() -> Self {
        Dock {
            tree: Tree::default(),
            visible: false,
            size: DOCK_DEFAULT_SIZE,
        }
    }
}

impl Dock {
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Mutable access to the dock's tree, for the `Workspace` verbs (and
    /// only them — crate-private so the one-place-per-TileId invariant
    /// stays enforceable at the `Workspace` seam).
    pub(crate) fn tree_mut(&mut self) -> &mut Tree {
        &mut self.tree
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    /// Visible AND occupied — the state in which a dock is a
    /// directional-focus target and a `fallback_region` candidate. Not
    /// the region invariant any more: a visible empty dock may hold the
    /// focus region (spec 2026-09-08 add-tile §8) so an add can fill it.
    pub fn focusable(&self) -> bool {
        self.visible && !self.tree.is_empty()
    }

    /// Set the dock's size, clamped into [`DOCK_MIN_SIZE`]..=
    /// [`DOCK_MAX_SIZE`]; a non-finite input (NaN from a hostile session
    /// float) resets to the default rather than poisoning the clamp
    /// (`f32::clamp` propagates NaN).
    pub(crate) fn set_size(&mut self, size: f32) {
        self.size = if size.is_finite() {
            size.clamp(DOCK_MIN_SIZE, DOCK_MAX_SIZE)
        } else {
            DOCK_DEFAULT_SIZE
        };
    }

    pub(crate) fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Reconstruct one dock from raw session parts. Local healing happens
    /// here: size (clamp, NaN → default, via [`Dock::set_size`]); any
    /// fullscreen the tree arrived with is cleared (dock trees never have
    /// fullscreen — the session layer warns about the hostile key, this
    /// enforces the invariant even for paths that skip the warning); and a
    /// non-empty tree whose `focused` was healed away (dangling reference
    /// → `None` in `Tree::from_parts`) refocuses its first tile — the
    /// `Workspace` verbs lean on "a focusable dock has a focused tile"
    /// (e.g. `move_to_dock` drains via `remove_focused`), where the main
    /// tree's long-standing tolerate-`None` behavior is left as is.
    /// *Cross*-dock healing (duplicate tile claims) needs the whole
    /// workspace set and lives in `Workspace::from_parts` /
    /// `Workspaces::from_parts`.
    pub(crate) fn from_parts(mut tree: Tree, visible: bool, size: f32) -> Dock {
        tree.exit_fullscreen();
        if tree.focused().is_none()
            && let Some(&first) = tree.tiles().first()
        {
            tree.focus(first);
        }
        let mut dock = Dock {
            tree,
            visible,
            size: DOCK_DEFAULT_SIZE,
        };
        dock.set_size(size);
        dock
    }
}

/// The three fixed docks of one workspace.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Docks {
    left: Dock,
    right: Dock,
    bottom: Dock,
}

impl Docks {
    pub fn get(&self, side: DockSide) -> &Dock {
        match side {
            DockSide::Left => &self.left,
            DockSide::Right => &self.right,
            DockSide::Bottom => &self.bottom,
        }
    }

    pub fn get_mut(&mut self, side: DockSide) -> &mut Dock {
        match side {
            DockSide::Left => &mut self.left,
            DockSide::Right => &mut self.right,
            DockSide::Bottom => &mut self.bottom,
        }
    }

    /// All docks with their sides, in [`DockSide::ALL`] order.
    pub fn iter(&self) -> impl Iterator<Item = (DockSide, &Dock)> {
        DockSide::ALL
            .into_iter()
            .map(move |side| (side, self.get(side)))
    }

    /// Every tile currently living in any dock's tree, in
    /// [`DockSide::ALL`] order (tree order within each dock).
    pub fn tiles(&self) -> impl Iterator<Item = TileId> + '_ {
        self.iter().flat_map(|(_, dock)| dock.tree.tiles())
    }

    /// Reconstruct from raw per-side parts (session restore).
    pub(crate) fn from_parts(left: Dock, right: Dock, bottom: Dock) -> Docks {
        Docks {
            left,
            right,
            bottom,
        }
    }
}

/// Where focus lives within one workspace: the main tiling tree, or one of
/// the docks. `Dock(side)` is only ever valid while that dock is
/// [`Dock::focusable`] (visible and occupied) — the `Workspace` verbs
/// maintain that invariant and session restore heals violations to `Main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusRegion {
    #[default]
    Main,
    Dock(DockSide),
}

/// Pixel-space carve-up of one workspace's content `area` (the same
/// single-pass geometry authority role `Tree::layout` plays for the tree —
/// rendering must call this once and lay both docks and tree out of the
/// result, never re-derive it; each visible dock's rect then feeds that
/// dock's own `Tree::layout`, one call per region). Visible left/right
/// docks take full-height columns of `size * area.w` off their edge; the
/// visible bottom dock takes `size * area.h` off the bottom of the
/// *remaining center column* (so the side docks always run the full height
/// — a deliberate look: side docks frame, the bottom dock tucks between
/// them); the tree gets what's left. Hidden docks take nothing. Returns
/// each visible dock's rect plus the tree's remaining rect. Widths/heights
/// never go negative (clamped at 0 — only reachable in degenerate
/// over-small windows, since sizes cap at [`DOCK_MAX_SIZE`] each).
pub fn layout(docks: &Docks, area: Rect) -> (Rect, Vec<(DockSide, Rect)>) {
    let mut out = Vec::new();
    let mut x = area.x;
    let mut w = area.w;

    if docks.left.visible {
        let dw = docks.left.size * area.w;
        out.push((
            DockSide::Left,
            Rect {
                x,
                y: area.y,
                w: dw,
                h: area.h,
            },
        ));
        x += dw;
        w = (w - dw).max(0.0);
    }
    if docks.right.visible {
        let dw = docks.right.size * area.w;
        out.push((
            DockSide::Right,
            Rect {
                x: (area.x + area.w - dw).max(x),
                y: area.y,
                w: dw,
                h: area.h,
            },
        ));
        w = (w - dw).max(0.0);
    }

    let mut h = area.h;
    if docks.bottom.visible {
        let dh = docks.bottom.size * area.h;
        out.push((
            DockSide::Bottom,
            Rect {
                x,
                y: area.y + area.h - dh,
                w,
                h: dh,
            },
        ));
        h = (h - dh).max(0.0);
    }

    (Rect { x, y: area.y, w, h }, out)
}

#[cfg(test)]
mod tests {
    use super::super::tree::Orientation;
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn default_dock_is_hidden_empty_quarter_sized() {
        let dock = Dock::default();
        assert!(dock.tree().is_empty());
        assert!(!dock.visible());
        assert!(approx(dock.size(), DOCK_DEFAULT_SIZE));
        assert!(!dock.focusable());
    }

    #[test]
    fn set_size_clamps_into_range() {
        let mut dock = Dock::default();
        dock.set_size(0.05);
        assert!(approx(dock.size(), DOCK_MIN_SIZE));
        dock.set_size(0.9);
        assert!(approx(dock.size(), DOCK_MAX_SIZE));
        dock.set_size(0.3);
        assert!(approx(dock.size(), 0.3));
    }

    #[test]
    fn set_size_heals_nan_to_default() {
        let mut dock = Dock::default();
        dock.set_size(f32::NAN);
        assert!(approx(dock.size(), DOCK_DEFAULT_SIZE));
        dock.set_size(f32::INFINITY);
        assert!(approx(dock.size(), DOCK_DEFAULT_SIZE));
    }

    #[test]
    fn focusable_requires_visible_and_a_non_empty_tree() {
        let mut dock = Dock::default();
        dock.set_visible(true);
        assert!(!dock.focusable(), "visible but empty");
        dock.tree_mut().split(TileId(1), Orientation::Horizontal);
        assert!(dock.focusable());
        dock.set_visible(false);
        assert!(!dock.focusable(), "occupied but hidden");
    }

    #[test]
    fn from_parts_clears_a_smuggled_fullscreen() {
        // Dock trees never have fullscreen — a hostile session file that
        // claims one gets it cleared here (the session layer warns).
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.toggle_fullscreen();
        assert!(tree.fullscreen().is_some());
        let dock = Dock::from_parts(tree, true, 0.25);
        assert_eq!(dock.tree().fullscreen(), None);
        assert_eq!(dock.tree().tiles(), vec![TileId(1)], "the tree survives");
    }

    #[test]
    fn docks_get_and_iter_cover_all_sides() {
        let mut docks = Docks::default();
        docks
            .get_mut(DockSide::Right)
            .tree_mut()
            .split(TileId(7), Orientation::Horizontal);
        assert_eq!(docks.get(DockSide::Right).tree().tiles(), vec![TileId(7)]);
        let sides: Vec<_> = docks.iter().map(|(s, _)| s).collect();
        assert_eq!(sides, DockSide::ALL.to_vec());
        assert_eq!(docks.tiles().collect::<Vec<_>>(), vec![TileId(7)]);
    }

    #[test]
    fn docks_tiles_walks_every_dock_tree_in_side_order() {
        let mut docks = Docks::default();
        docks
            .get_mut(DockSide::Bottom)
            .tree_mut()
            .split(TileId(3), Orientation::Horizontal);
        docks
            .get_mut(DockSide::Left)
            .tree_mut()
            .split(TileId(1), Orientation::Horizontal);
        docks
            .get_mut(DockSide::Left)
            .tree_mut()
            .split(TileId(2), Orientation::Horizontal);
        assert_eq!(
            docks.tiles().collect::<Vec<_>>(),
            vec![TileId(1), TileId(2), TileId(3)],
            "left's whole tree first, then bottom's"
        );
    }

    // --- layout ---------------------------------------------------------

    fn area() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 800.0,
        }
    }

    #[test]
    fn layout_with_no_visible_docks_gives_the_tree_everything() {
        let (tree, docks) = layout(&Docks::default(), area());
        assert!(docks.is_empty());
        assert_eq!(tree, area());
    }

    #[test]
    fn layout_left_dock_carves_a_full_height_column() {
        let mut d = Docks::default();
        d.get_mut(DockSide::Left).set_visible(true);
        let (tree, rects) = layout(&d, area());
        assert_eq!(rects.len(), 1);
        let (side, r) = rects[0];
        assert_eq!(side, DockSide::Left);
        assert!(approx(r.x, 0.0) && approx(r.w, 250.0) && approx(r.h, 800.0));
        assert!(approx(tree.x, 250.0) && approx(tree.w, 750.0) && approx(tree.h, 800.0));
    }

    #[test]
    fn layout_right_dock_hugs_the_right_edge() {
        let mut d = Docks::default();
        d.get_mut(DockSide::Right).set_visible(true);
        d.get_mut(DockSide::Right).set_size(0.4);
        let (tree, rects) = layout(&d, area());
        let (_, r) = rects[0];
        assert!(approx(r.x, 600.0) && approx(r.w, 400.0) && approx(r.h, 800.0));
        assert!(approx(tree.x, 0.0) && approx(tree.w, 600.0));
    }

    #[test]
    fn layout_bottom_dock_spans_only_the_center_column() {
        let mut d = Docks::default();
        d.get_mut(DockSide::Left).set_visible(true);
        d.get_mut(DockSide::Right).set_visible(true);
        d.get_mut(DockSide::Bottom).set_visible(true);
        let (tree, rects) = layout(&d, area());
        let bottom = rects
            .iter()
            .find(|(s, _)| *s == DockSide::Bottom)
            .unwrap()
            .1;
        // Side docks each take 250px full height; the bottom band spans the
        // 500px between them, 200px tall, flush with the bottom edge.
        assert!(approx(bottom.x, 250.0) && approx(bottom.w, 500.0));
        assert!(approx(bottom.y, 600.0) && approx(bottom.h, 200.0));
        assert!(approx(tree.x, 250.0) && approx(tree.w, 500.0));
        assert!(approx(tree.y, 0.0) && approx(tree.h, 600.0));
    }

    #[test]
    fn layout_offsets_respect_a_non_origin_area() {
        let mut d = Docks::default();
        d.get_mut(DockSide::Bottom).set_visible(true);
        let shifted = Rect {
            x: 10.0,
            y: 20.0,
            w: 100.0,
            h: 100.0,
        };
        let (tree, rects) = layout(&d, shifted);
        let (_, r) = rects[0];
        assert!(approx(r.x, 10.0) && approx(r.y, 95.0) && approx(r.h, 25.0));
        assert!(approx(tree.y, 20.0) && approx(tree.h, 75.0));
    }
}
