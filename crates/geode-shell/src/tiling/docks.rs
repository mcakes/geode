//! Fixed dock regions (dock-regions task): three per-workspace slots —
//! left, right, bottom — each holding at most one tile alongside the main
//! i3-style tiling tree, for the "always there" panes a desk keeps pinned
//! (a watchlist on the left, a detail pane on the right, a log strip on the
//! bottom) without giving up the tree for the main working set. Pure data,
//! no gpui (spec §10.3) — the [`Workspace`](super::Workspace) verbs consult
//! and mutate these; rendering consumes [`layout`].
//!
//! **Inventory decision (not gpui-component's `dock` module):** the pinned
//! checkout ships a whole `crates/ui/src/dock/` framework — `DockArea`,
//! `PaneTree`, `Panel`/`PanelView`, `TabPanel`, `StackPanel` — that is a
//! competing layout *and persistence* system: it owns its own split tree,
//! its own drag-and-drop docking, its own serde-based `DockAreaState`
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
//! So docks are ~a hundred lines of our own pure state here, rendered with
//! the exact same tile chrome the tree already uses — the same kind of
//! decision `shell/sidebar.rs` records for gpui-component's `Sidebar<E>`.

use super::tree::{Rect, TileId};

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

/// One dock region's state. A dock holds at most one tile; `visible` and
/// `tile` vary independently — a hidden dock keeps its tile (toggle it back
/// and the tile is still there), and a visible dock may be empty (it
/// renders a hint inviting a `dock::move_*`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dock {
    /// The tile parked here, if any. A `TileId` lives in exactly one place
    /// — the tree XOR one dock — enforced by the `Workspace` verbs (which
    /// only ever *move* ids, never duplicate them) and healed on session
    /// restore (`Workspaces::from_parts` drops duplicate dock claims).
    tile: Option<TileId>,
    visible: bool,
    /// Fraction of the content area this dock occupies when visible: width
    /// for left/right, height (of the remaining center column) for bottom.
    /// Always within [`DOCK_MIN_SIZE`]..=[`DOCK_MAX_SIZE`] — every write
    /// goes through [`Dock::set_size`], which clamps.
    size: f32,
}

impl Default for Dock {
    fn default() -> Self {
        Dock {
            tile: None,
            visible: false,
            size: DOCK_DEFAULT_SIZE,
        }
    }
}

impl Dock {
    pub fn tile(&self) -> Option<TileId> {
        self.tile
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    /// Visible AND occupied — the only state in which a dock can hold
    /// focus or be a directional-focus target.
    pub fn focusable(&self) -> bool {
        self.visible && self.tile.is_some()
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

    pub(crate) fn set_tile(&mut self, tile: Option<TileId>) {
        self.tile = tile;
    }

    /// Reconstruct one dock from raw session parts. Size healing (clamp,
    /// NaN → default) happens here via [`Dock::set_size`]; *cross*-dock
    /// healing (duplicate tile claims) needs the whole workspace set and
    /// lives in `Workspaces::from_parts`.
    pub(crate) fn from_parts(tile: Option<TileId>, visible: bool, size: f32) -> Dock {
        let mut dock = Dock {
            tile,
            visible,
            size: DOCK_DEFAULT_SIZE,
        };
        dock.set_size(size);
        dock
    }
}

/// The three fixed docks of one workspace.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
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

    /// Every tile currently parked in a dock, in [`DockSide::ALL`] order.
    pub fn tiles(&self) -> impl Iterator<Item = TileId> + '_ {
        self.iter().filter_map(|(_, dock)| dock.tile())
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
/// result, never re-derive it). Visible left/right docks take full-height
/// columns of `size * area.w` off their edge; the visible bottom dock takes
/// `size * area.h` off the bottom of the *remaining center column* (so the
/// side docks always run the full height — a deliberate look: side docks
/// frame, the bottom dock tucks between them); the tree gets what's left.
/// Hidden docks take nothing. Returns each visible dock's rect plus the
/// tree's remaining rect. Widths/heights never go negative (clamped at 0 —
/// only reachable in degenerate over-small windows, since sizes cap at
/// [`DOCK_MAX_SIZE`] each).
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
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn default_dock_is_hidden_empty_quarter_sized() {
        let dock = Dock::default();
        assert_eq!(dock.tile(), None);
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
    fn focusable_requires_visible_and_occupied() {
        let mut dock = Dock::default();
        dock.set_visible(true);
        assert!(!dock.focusable(), "visible but empty");
        dock.set_tile(Some(TileId(1)));
        assert!(dock.focusable());
        dock.set_visible(false);
        assert!(!dock.focusable(), "occupied but hidden");
    }

    #[test]
    fn docks_get_and_iter_cover_all_sides() {
        let mut docks = Docks::default();
        docks.get_mut(DockSide::Right).set_tile(Some(TileId(7)));
        assert_eq!(docks.get(DockSide::Right).tile(), Some(TileId(7)));
        let sides: Vec<_> = docks.iter().map(|(s, _)| s).collect();
        assert_eq!(sides, DockSide::ALL.to_vec());
        assert_eq!(docks.tiles().collect::<Vec<_>>(), vec![TileId(7)]);
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
