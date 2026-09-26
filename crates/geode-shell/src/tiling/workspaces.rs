use super::docks::{DockSide, Docks, FocusRegion};
use super::tree::{Direction, DividerAddress, Orientation, Rect, TileId, Tree};
use crate::actions::ActionId;
use std::collections::BTreeMap;

/// One workspace's main tree, three dock trees, and focused region.
/// Region-aware verbs live here so the same action reaches the correct tree
/// and preserves visibility/focus invariants. [`apply_workspace_action`] maps
/// action IDs to these pure operations.
#[derive(Debug, Default)]
pub struct Workspace {
    tree: Tree,
    docks: Docks,
    /// The focused region. A focused dock must be visible, but may be empty
    /// so a subsequent add can fill it. Hiding that dock selects a fallback;
    /// restoration repairs references to hidden docks.
    region: FocusRegion,
}

impl From<Tree> for Workspace {
    /// A workspace that is just a tree — default (hidden, empty) docks,
    /// focus in the main region. The shape every pre-dock session file
    /// restores to.
    fn from(tree: Tree) -> Self {
        Workspace {
            tree,
            docks: Docks::default(),
            region: FocusRegion::Main,
        }
    }
}

impl Workspace {
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn docks(&self) -> &Docks {
        &self.docks
    }

    pub fn region(&self) -> FocusRegion {
        self.region
    }

    /// Empty means *really* empty: no tiles in the tree AND none in any
    /// dock's tree (a workspace whose only tile is hidden in a dock still
    /// counts as non-empty — the sidebar indicator must not pretend the
    /// tile is gone).
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty() && self.docks.tiles().next().is_none()
    }

    /// Focus in the selected region's tree. An empty focused region returns
    /// `None` even when another region contains tiles.
    pub fn focused_tile(&self) -> Option<TileId> {
        self.tree_for(self.region).focused()
    }

    /// Find the focused tile's rectangle using dock and tree layout. Return
    /// `None` for an empty focused region. `AddDirection::Auto` uses this shape.
    /// When the main tree is fullscreen, it sees the fullscreen tile's rectangle
    /// inside the dock-carved tree area, not the underlying split slot.
    pub fn focused_tile_rect(&self, area: Rect) -> Option<Rect> {
        let focused = self.focused_tile()?;
        let (tree_area, dock_rects) = super::docks::layout(&self.docks, area);
        let region_area = match self.region {
            FocusRegion::Main => tree_area,
            FocusRegion::Dock(side) => dock_rects.iter().find(|(s, _)| *s == side)?.1,
        };
        self.tree_for(self.region)
            .layout(region_area)
            .into_iter()
            .find(|(id, _)| *id == focused)
            .map(|(_, r)| r)
    }

    /// Where focus should land when it can no longer stay where it is
    /// (the focused dock was hidden, emptied, or closed): the tree if it
    /// has tiles (its own focused tile is untouched), else any focusable
    /// dock (first in [`DockSide::ALL`] order — deterministic), else
    /// `Main` (the empty-workspace resting state).
    fn fallback_region(&self) -> FocusRegion {
        if !self.tree.is_empty() {
            return FocusRegion::Main;
        }
        self.docks
            .iter()
            .find(|(_, dock)| dock.focusable())
            .map(|(side, _)| FocusRegion::Dock(side))
            .unwrap_or(FocusRegion::Main)
    }

    /// Select the main or named dock tree independently of current focus.
    fn tree_for(&self, region: FocusRegion) -> &Tree {
        match region {
            FocusRegion::Main => &self.tree,
            FocusRegion::Dock(side) => self.docks.get(side).tree(),
        }
    }

    /// [`Workspace::tree_for`], mutably.
    fn tree_for_mut(&mut self, region: FocusRegion) -> &mut Tree {
        match region {
            FocusRegion::Main => &mut self.tree,
            FocusRegion::Dock(side) => self.docks.get_mut(side).tree_mut(),
        }
    }

    /// Enter a region, showing a destination dock to keep focus visible.
    /// Source-dock hiding belongs to the move/removal operation that knows
    /// whether that source emptied.
    fn enter_region(&mut self, region: FocusRegion) {
        if let FocusRegion::Dock(side) = region {
            self.docks.get_mut(side).set_visible(true);
        }
        self.region = region;
    }

    /// Click-to-focus on a tree tile: focus it and return focus to the
    /// main region.
    pub fn focus_main_tile(&mut self, id: TileId) -> bool {
        if self.tree.focus(id) {
            self.region = FocusRegion::Main;
            true
        } else {
            false
        }
    }

    /// Focus a dock as a region: only a focusable (visible + occupied)
    /// dock can take focus — an empty or hidden dock never can. The dock's
    /// own tree keeps whatever tile it last had focused (focus memory,
    /// straight from `Tree`).
    pub fn focus_dock(&mut self, side: DockSide) -> bool {
        if self.docks.get(side).focusable() {
            self.region = FocusRegion::Dock(side);
            true
        } else {
            false
        }
    }

    /// Focus a visible empty dock and exit main fullscreen so a subsequent
    /// add lands there. Refuse hidden, occupied, or already-focused docks.
    /// Directional navigation instead uses [`Self::focus_dock`], which requires
    /// an occupied dock; tile clicks use [`Self::focus_dock_tile`].
    pub fn focus_empty_dock(&mut self, side: DockSide) -> bool {
        let dock = self.docks.get(side);
        if !dock.visible() || !dock.tree().is_empty() || self.region == FocusRegion::Dock(side) {
            return false;
        }
        self.tree.exit_fullscreen();
        self.region = FocusRegion::Dock(side);
        true
    }

    /// Focus a tile inside a visible dock and select that region. Refuse a
    /// hidden dock or an ID absent from its tree without moving the region.
    pub fn focus_dock_tile(&mut self, side: DockSide, id: TileId) -> bool {
        if self.docks.get(side).visible() && self.docks.get_mut(side).tree_mut().focus(id) {
            self.region = FocusRegion::Dock(side);
            true
        } else {
            false
        }
    }

    /// Navigate within the current tree first. At a main-tree edge, enter the
    /// visible occupied dock on that side; there is no upper dock, and main
    /// fullscreen prevents crossing. At a dock edge, only the inward direction
    /// crosses to main. If main is empty, left/right can cross to the opposite
    /// occupied visible dock; bottom has no opposite. Other directions stop.
    pub fn focus_direction(&mut self, dir: Direction) {
        match self.region {
            FocusRegion::Main => {
                if self.tree.focus_direction(dir) {
                    return;
                }
                if self.tree.fullscreen().is_some() {
                    return;
                }
                let target = match dir {
                    Direction::Left => Some(DockSide::Left),
                    Direction::Right => Some(DockSide::Right),
                    Direction::Down => Some(DockSide::Bottom),
                    Direction::Up => None,
                };
                if let Some(side) = target
                    && self.docks.get(side).focusable()
                {
                    self.region = FocusRegion::Dock(side);
                }
            }
            FocusRegion::Dock(side) => {
                if self.docks.get_mut(side).tree_mut().focus_direction(dir) {
                    return;
                }
                let inward = match side {
                    DockSide::Left => Direction::Right,
                    DockSide::Right => Direction::Left,
                    DockSide::Bottom => Direction::Up,
                };
                if dir != inward {
                    return;
                }
                if !self.tree.is_empty() {
                    self.region = FocusRegion::Main;
                    return;
                }
                let opposite = match side {
                    DockSide::Left => Some(DockSide::Right),
                    DockSide::Right => Some(DockSide::Left),
                    DockSide::Bottom => None,
                };
                if let Some(opposite) = opposite
                    && self.docks.get(opposite).focusable()
                {
                    self.region = FocusRegion::Dock(opposite);
                }
            }
        }
    }

    /// Move within the focused region's tree: swap a plain tile with its
    /// neighbor or pop a stack member out. Directional moves do not cross
    /// regions; [`Self::move_to_dock`] handles region transfers.
    pub fn move_direction(&mut self, dir: Direction) {
        match self.region {
            FocusRegion::Main => {
                self.tree.move_direction(dir);
            }
            FocusRegion::Dock(side) => {
                self.docks.get_mut(side).tree_mut().move_direction(dir);
            }
        }
    }

    /// Resize the focused tree's divider by [`RESIZE_STEP`]. In a dock, if no
    /// internal divider moves, resize its frame on the same press. This fallback
    /// also occurs when an internal divider refuses a step at its minimum.
    ///
    /// Frame resizing moves the inner edge: left grows rightward, right grows
    /// leftward, and bottom grows upward. Directions along the frame's other
    /// axis do nothing. The dock setter clamps size to its allowed range.
    pub fn resize(&mut self, dir: Direction) {
        match self.region {
            FocusRegion::Main => {
                self.tree.move_divider(dir, RESIZE_STEP);
            }
            FocusRegion::Dock(side) => {
                if self
                    .docks
                    .get_mut(side)
                    .tree_mut()
                    .move_divider(dir, RESIZE_STEP)
                {
                    return;
                }
                let grow = match (side, dir) {
                    (DockSide::Left, Direction::Right) => 1.0,
                    (DockSide::Left, Direction::Left) => -1.0,
                    (DockSide::Right, Direction::Left) => 1.0,
                    (DockSide::Right, Direction::Right) => -1.0,
                    (DockSide::Bottom, Direction::Up) => 1.0,
                    (DockSide::Bottom, Direction::Down) => -1.0,
                    _ => return,
                };
                let dock = self.docks.get_mut(side);
                let size = dock.size();
                dock.set_size(size + grow * RESIZE_STEP);
            }
        }
    }

    /// Drag a main-tree divider by structural address, using
    /// [`Tree::drag_divider`]'s validation and clamping. Does not change focus
    /// or select a region; keyboard focus does not determine the target.
    pub fn drag_main_divider(
        &mut self,
        address: &DividerAddress,
        x: f32,
        y: f32,
        bounds: Rect,
    ) -> bool {
        self.tree.drag_divider(address, x, y, bounds)
    }

    /// Drag a divider inside a visible dock using its frame as `bounds`.
    /// A hidden dock refuses the drag without mutation. Focus and region stay
    /// unchanged; already-applied resize steps are retained.
    pub fn drag_dock_divider(
        &mut self,
        side: DockSide,
        address: &DividerAddress,
        x: f32,
        y: f32,
        bounds: Rect,
    ) -> bool {
        let dock = self.docks.get_mut(side);
        if !dock.visible() {
            return false;
        }
        dock.tree_mut().drag_divider(address, x, y, bounds)
    }

    /// Project an absolute cursor position to a dock size within `area`, then
    /// clamp to the dock's allowed range. Refuse a hidden dock or invalid
    /// projection. Return true only when size changes by at least 1e-6;
    /// repeated positions at the clamp do not dirty the session.
    pub fn drag_dock_edge(&mut self, side: DockSide, x: f32, y: f32, area: Rect) -> bool {
        let Some(frac) = super::dividers::dock_size_from_position(side, x, y, area) else {
            return false;
        };
        let dock = self.docks.get_mut(side);
        if !dock.visible() {
            return false;
        }
        let before = dock.size();
        dock.set_size(frac);
        // Compare post-clamp against pre-clamp — `set_size` owns the
        // 0.10..=0.50 range, so this is the one honest way to know whether
        // the clamp actually let anything through. Same 1e-6 epsilon (in
        // content-area fraction space) as `Tree::drag_divider`.
        (dock.size() - before).abs() >= 1e-6
    }

    /// Close focus in its region's tree. If removal empties a dock, hide it
    /// and choose a fallback region. Closing an already-empty focused dock is
    /// a no-op: its visible, empty focus remains available for adding a tile.
    pub fn close_tile(&mut self) {
        match self.region {
            FocusRegion::Main => self.tree.close(),
            FocusRegion::Dock(side) => {
                let dock = self.docks.get_mut(side);
                if dock.tree().is_empty() {
                    return;
                }
                dock.tree_mut().close();
                if dock.tree().is_empty() {
                    dock.set_visible(false);
                    self.region = self.fallback_region();
                }
            }
        }
    }

    /// Cycle the focused region's stack member; return false for a plain tile.
    pub fn stack_step(&mut self, delta: i64) -> bool {
        let region = self.region;
        self.tree_for_mut(region).stack_step(delta)
    }

    /// Pop the focused member beside its stack; return false for a plain tile.
    pub fn unstack_focused(&mut self, orientation: Orientation) -> bool {
        let region = self.region;
        self.tree_for_mut(region).unstack_focused(orientation)
    }

    /// Find `id`'s member index and stack length across this workspace's trees.
    pub fn stack_position(&self, id: TileId) -> Option<(usize, usize)> {
        let region = self.region_of(id)?;
        self.tree_for(region).stack_position(id)
    }

    /// Return ordered members of the stack containing `id` in any region.
    pub fn stack_members(&self, id: TileId) -> Option<Vec<TileId>> {
        let region = self.region_of(id)?;
        self.tree_for(region).stack_members(id)
    }

    /// While a main-tree tile is fullscreen, how many other tiles the
    /// unmaximised picture would show: the main tree's other slots plus
    /// every visible dock's slots (fullscreen paints no dock). A stack is
    /// one slot, as in [`Tree::layout`]. `None` when nothing is fullscreen,
    /// including a dangling fullscreen id, which `Tree::layout` also
    /// ignores. Feeds the status bar's fullscreen segment, so a lone
    /// maximised tile reads differently from a lone tile.
    pub fn fullscreen_hidden(&self) -> Option<usize> {
        let fs = self.tree.fullscreen()?;
        if !self.tree.contains(fs) {
            return None;
        }
        let docked: usize = self
            .docks
            .iter()
            .filter(|(_, dock)| dock.visible())
            .map(|(_, dock)| dock.tree().slot_count())
            .sum();
        Some(self.tree.slot_count() - 1 + docked)
    }

    /// Toggle fullscreen only in the main tree. With a dock focused this is
    /// a no-op, but the router still consumes the action. Dock visibility is
    /// controlled separately by `dock::toggle_*`.
    pub fn toggle_fullscreen(&mut self) {
        if self.region == FocusRegion::Main {
            self.tree.toggle_fullscreen();
        }
    }

    /// Reorient the split around focus in the selected region's tree.
    pub fn toggle_split_orientation(&mut self) {
        match self.region {
            FocusRegion::Main => {
                self.tree.toggle_split_orientation();
            }
            FocusRegion::Dock(side) => {
                self.docks
                    .get_mut(side)
                    .tree_mut()
                    .toggle_split_orientation();
            }
        }
    }

    /// Show a hidden dock, exit main fullscreen, and focus the dock even if
    /// empty. Hide a visible dock while retaining its tree, ratios, and focus
    /// memory; select a fallback only if that dock held the focused region.
    pub fn toggle_dock(&mut self, side: DockSide) {
        if self.docks.get(side).visible() {
            self.docks.get_mut(side).set_visible(false);
            if self.region == FocusRegion::Dock(side) {
                self.region = self.fallback_region();
            }
        } else {
            // Showing focuses the dock so the next add lands in it.
            self.tree.exit_fullscreen();
            self.enter_region(FocusRegion::Dock(side));
        }
    }

    /// The orientation a tile arrives with when inserted into a region by
    /// a `dock::move_*` (both directions — into a dock's tree and back
    /// into the main tree — use the side it crossed): Horizontal for a
    /// left/right dock (the tile arrives side by side), Vertical for the
    /// bottom one (it arrives stacked).
    fn dock_insert_orientation(side: DockSide) -> Orientation {
        match side {
            DockSide::Left | DockSide::Right => Orientation::Horizontal,
            DockSide::Bottom => Orientation::Vertical,
        }
    }

    /// Transfer the focused tile to `side`, inserting at that dock's focus.
    /// Left/right use horizontal insertion; bottom uses vertical. The target
    /// shows and receives focus, and an emptied source dock hides.
    ///
    /// If already focused in `side`, send its focused tile back to main instead.
    /// Main fullscreen clears when moving a tile into a dock. With no focused
    /// tile, leave layout and region unchanged, including a visible empty dock.
    pub fn move_to_dock(&mut self, side: DockSide) {
        match self.region {
            FocusRegion::Main => {
                let Some(moved) = self.tree.remove_focused() else {
                    return; // empty workspace: claimed no-op
                };
                // `remove_focused` only clears fullscreen when the removed
                // tile held it; clear it unconditionally — a lingering
                // fullscreen (possible only via a hostile restore that
                // decoupled focus from fullscreen) would cover the whole
                // surface and hide the dock the tile just landed in.
                self.tree.exit_fullscreen();
                self.docks
                    .get_mut(side)
                    .tree_mut()
                    .split(moved, Self::dock_insert_orientation(side));
                self.enter_region(FocusRegion::Dock(side));
            }
            FocusRegion::Dock(from) if from == side => {
                let Some(moved) = self.docks.get_mut(side).tree_mut().remove_focused() else {
                    // An empty focused dock has no tile to transfer; retain its region.
                    return;
                };
                self.tree.split(moved, Self::dock_insert_orientation(side));
                let dock = self.docks.get_mut(side);
                if dock.tree().is_empty() {
                    dock.set_visible(false);
                }
                self.region = FocusRegion::Main;
            }
            FocusRegion::Dock(from) => {
                let Some(moved) = self.docks.get_mut(from).tree_mut().remove_focused() else {
                    // An empty focused dock has no tile to transfer; retain its region.
                    return;
                };
                if self.docks.get(from).tree().is_empty() {
                    self.docks.get_mut(from).set_visible(false);
                }
                self.docks
                    .get_mut(side)
                    .tree_mut()
                    .split(moved, Self::dock_insert_orientation(side));
                self.enter_region(FocusRegion::Dock(side));
            }
        }
    }

    /// Locate `id` across main and dock trees, independent of keyboard focus.
    /// Drop operations resolve their endpoints this way before mutation.
    pub fn region_of(&self, id: TileId) -> Option<FocusRegion> {
        if self.tree.contains(id) {
            return Some(FocusRegion::Main);
        }
        self.docks
            .iter()
            .find(|(_, dock)| dock.tree().contains(id))
            .map(|(side, _)| FocusRegion::Dock(side))
    }

    /// Remove an ID from its owning tree and hide a dock emptied by removal.
    /// Return its former region, or `None` without mutation if absent. Do not
    /// repair the focused region here: drop callers finish by entering the
    /// destination after reinserting the tile.
    fn remove_tile_anywhere(&mut self, id: TileId) -> Option<FocusRegion> {
        let region = self.region_of(id)?;
        match region {
            FocusRegion::Main => {
                self.tree.remove(id);
            }
            FocusRegion::Dock(side) => {
                let dock = self.docks.get_mut(side);
                dock.tree_mut().remove(id);
                if dock.tree().is_empty() {
                    dock.set_visible(false);
                }
            }
        }
        Some(region)
    }

    /// Move `dragged` beside `target` on the requested edge, within or across
    /// regions. Focus follows the tile; an emptied source dock hides and a
    /// destination dock shows. Self-drops and absent IDs return false.
    ///
    /// Validate endpoints before removal and fall back to insertion at destination
    /// focus if the target insertion refuses. Return true for an accepted move;
    /// the result is not a structural equality check of the old and new layout.
    pub fn drop_split(&mut self, dragged: TileId, target: TileId, edge: Direction) -> bool {
        if dragged == target {
            return false;
        }
        // Verify BOTH ends before removing anything: refusal must never
        // strand the dragged tile outside every tree. (The destination
        // derived here stays valid across the removal below — removal of
        // `dragged` can never move `target`.)
        let Some(source) = self.region_of(dragged) else {
            return false;
        };
        let Some(destination) = self.region_of(target) else {
            return false;
        };
        // Check destination uniqueness before removing anything. Otherwise a
        // refused ID-based insertion followed by the plain-split fallback could
        // add another copy. Assert the broken invariant in debug builds and
        // return an untouched refusal in release builds.
        if destination != source && self.tree_for(destination).contains(dragged) {
            debug_assert!(
                false,
                "one-place-per-TileId invariant pre-broken \
                 (dragged {dragged:?} duplicated into the destination tree); \
                 refusing the edge drop untouched"
            );
            return false;
        }
        self.remove_tile_anywhere(dragged);
        let after = match edge {
            Direction::Left | Direction::Up => false,
            Direction::Right | Direction::Down => true,
        };
        let orientation = edge.orientation();
        if destination == FocusRegion::Main {
            // A stale fullscreen would cover the tile that just moved
            // (same reasoning as `move_to_dock`'s unconditional exit);
            // `insert_at_leaf` also clears it, but only on success.
            self.tree.exit_fullscreen();
        }
        let tree = self.tree_for_mut(destination);
        if !tree.insert_at_leaf(target, dragged, orientation, after) {
            // Unreachable given the pre-checks — but the
            // never-lose-a-tile invariant outranks trusting them:
            // fall back to a plain focused-leaf split.
            tree.split(dragged, orientation);
        }
        // Focus/region follow the moved tile; a destination dock
        // auto-shows (`enter_region` — drop targets only come from
        // visible layout, but the verb enforces the region invariant on
        // its own rather than trusting the caller).
        self.enter_region(destination);
        true
    }

    /// Move `dragged` after `target` in its stack, making a stack from a plain
    /// target if needed. Members of the same stack are reordered. Focus follows
    /// the tile; an emptied source dock hides and a destination dock shows.
    /// Return false for a self-drop or absent ID; true means the operation was
    /// accepted, even if reinsertion reproduces the previous member order.
    pub fn drop_stack(&mut self, dragged: TileId, target: TileId) -> bool {
        if dragged == target {
            return false;
        }
        let Some(source) = self.region_of(dragged) else {
            return false;
        };
        let Some(destination) = self.region_of(target) else {
            return false;
        };
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

    /// Move `dragged` into `side` at its focused slot, using horizontal
    /// insertion for left/right and vertical for bottom. Show and focus the
    /// destination, hiding a source dock emptied by removal. Refuse an absent
    /// ID or a tile already in this dock; background drops do not send it back
    /// to main. Return true for an accepted transfer.
    pub fn drop_to_dock(&mut self, dragged: TileId, side: DockSide) -> bool {
        let Some(source) = self.region_of(dragged) else {
            return false;
        };
        if source == FocusRegion::Dock(side) {
            return false;
        }
        // Refuse a duplicate destination claim before mutation. The plain-split
        // insertion below assumes the incoming ID is absent from that tree.
        if self.docks.get(side).tree().contains(dragged) {
            debug_assert!(
                false,
                "one-place-per-TileId invariant pre-broken \
                 (dragged {dragged:?} duplicated into the target dock's tree); \
                 refusing the dock drop untouched"
            );
            return false;
        }
        self.remove_tile_anywhere(dragged);
        if source == FocusRegion::Main {
            // Same unconditional clear as `move_to_dock`: a lingering
            // fullscreen would cover the dock the tile just landed in.
            self.tree.exit_fullscreen();
        }
        self.docks
            .get_mut(side)
            .tree_mut()
            .split(dragged, Self::dock_insert_orientation(side));
        // The target dock auto-shows and takes focus+region
        // (`enter_region`; focus followed the moved tile via `split`).
        self.enter_region(FocusRegion::Dock(side));
        true
    }

    /// Prune dock claims against `claimed`, visiting sides in [`DockSide::ALL`]
    /// order and tiles in tree order. Callers seed claims with one workspace's
    /// main tree or all main trees, so main wins and subsequent dock claims are
    /// removed one at a time. Prefix each warning with `label`.
    fn heal_duplicate_dock_claims(
        docks: &mut Docks,
        claimed: &mut Vec<TileId>,
        label: &str,
        warnings: &mut Vec<String>,
    ) {
        for side in DockSide::ALL {
            let dock = docks.get_mut(side);
            for tile in dock.tree().tiles() {
                if claimed.contains(&tile) {
                    warnings.push(format!(
                        "{label}dock tile {} is already placed elsewhere; \
                         removing it from the {side:?} dock's tree",
                        tile.0
                    ));
                    dock.tree_mut().remove(tile);
                } else {
                    claimed.push(tile);
                }
            }
        }
    }

    /// Restore a workspace with local recovery: supply missing main-tree focus,
    /// prune duplicate dock claims, and replace a hidden focused dock with a
    /// fallback region. A visible empty dock can retain focus. Clear main
    /// fullscreen when dock focus survives. Return recovery warnings; global
    /// dock conflicts are handled by [`Workspaces::from_parts`].
    pub fn from_parts(tree: Tree, docks: Docks, region: FocusRegion) -> (Workspace, Vec<String>) {
        let mut warnings = Vec::new();
        let mut tree = tree;
        let mut docks = docks;
        // Give a nonempty main tree focus so region transfers can insert at it.
        // This mirrors dock restoration and requires no warning.
        if tree.focused().is_none()
            && let Some(&first) = tree.tiles().first()
        {
            tree.focus(first);
        }
        let mut claimed: Vec<TileId> = tree.tiles();
        Self::heal_duplicate_dock_claims(&mut docks, &mut claimed, "", &mut warnings);
        let mut ws = Workspace {
            tree,
            docks,
            region,
        };
        if let FocusRegion::Dock(side) = ws.region
            && !ws.docks.get(side).visible()
        {
            warnings.push(format!(
                "focus region points at the {side:?} dock, which is hidden; falling back"
            ));
            ws.region = ws.fallback_region();
        }
        // Main fullscreen would hide the focused dock and disable its fullscreen
        // toggle. Keep valid dock focus and clear fullscreen. Check after region
        // recovery so a fallback to main can retain its fullscreen tile.
        if matches!(ws.region, FocusRegion::Dock(_)) && ws.tree.fullscreen().is_some() {
            warnings.push(
                "fullscreen is set while focus is in a dock (contradictory); \
                 clearing fullscreen"
                    .to_string(),
            );
            ws.tree.exit_fullscreen();
        }
        (ws, warnings)
    }
}

/// Workspace collection with indices 1–9 and a shared tile-ID allocator.
/// Switching materializes an absent workspace; empty workspaces remain stored.
/// Each shell owns its collection through `ShellServices`.
#[derive(Debug)]
pub struct Workspaces {
    spaces: BTreeMap<u8, Workspace>,
    active: u8,
    next_tile: u64,
    /// Generation of actual workspace switches. Drag state compares this
    /// rather than only `active`, so switching away and back invalidates a
    /// gesture even between renders. Selecting the current index does not
    /// advance it.
    switch_epoch: u64,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspaces {
    pub fn new() -> Self {
        let mut spaces = BTreeMap::new();
        spaces.insert(1, Workspace::default());
        Workspaces {
            spaces,
            active: 1,
            next_tile: 0,
            switch_epoch: 0,
        }
    }

    pub fn active_index(&self) -> u8 {
        self.active
    }

    /// See the field doc: the ABA-proof "which workspace era is this"
    /// pin for the shell's drag re-checks.
    pub fn switch_epoch(&self) -> u64 {
        self.switch_epoch
    }

    pub fn active(&self) -> &Workspace {
        // Invariant: `active` is always a key (established in new/switch).
        &self.spaces[&self.active]
    }

    pub fn active_mut(&mut self) -> &mut Workspace {
        // Invariant: 'active' is always a key — established in new() and switch().
        self.spaces
            .get_mut(&self.active)
            .expect("active workspace always exists")
    }

    /// Switch to workspace `n` (1..=9), creating it empty if needed. An
    /// actual change of active workspace bumps [`Workspaces::switch_epoch`]
    /// (a same-index re-select does not — nothing on screen changes).
    pub fn switch(&mut self, n: u8) -> bool {
        if !(1..=9).contains(&n) {
            return false;
        }
        self.spaces.entry(n).or_default();
        if n != self.active {
            self.switch_epoch += 1;
        }
        self.active = n;
        true
    }

    /// Allocate the next ID in this collection, shared by all its workspaces
    /// and docks. The counter has no exhaustion check.
    pub fn alloc_tile(&mut self) -> TileId {
        self.next_tile += 1;
        TileId(self.next_tile)
    }

    /// Allocate a tile ID and split at focus in the active region, creating a
    /// root if empty. The shell associates its pending occupant request with
    /// the returned ID. All workspaces and docks share this allocator.
    pub fn split_active(&mut self, orientation: Orientation) -> TileId {
        let id = self.alloc_tile();
        let ws = self.active_mut();
        match ws.region {
            FocusRegion::Main => ws.tree.split(id, orientation),
            FocusRegion::Dock(side) => ws.docks.get_mut(side).tree_mut().split(id, orientation),
        }
        id
    }

    /// Allocate and stack a tile after focus in the active region. Return
    /// `None` without focus so callers can use the split path instead.
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

    /// Workspace indices that currently hold at least one tile (in the
    /// tree or parked in a dock — see [`Workspace::is_empty`]).
    pub fn non_empty_indices(&self) -> Vec<u8> {
        self.spaces
            .iter()
            .filter(|(_, ws)| !ws.is_empty())
            .map(|(ix, _)| *ix)
            .collect()
    }

    /// Stored workspace indices and state in index order, including empty ones.
    pub fn spaces(&self) -> impl Iterator<Item = (u8, &Workspace)> {
        self.spaces.iter().map(|(ix, ws)| (*ix, ws))
    }

    /// Restore a workspace collection. Reject an active index outside 1–9,
    /// filter other out-of-range indices, and materialize the active workspace
    /// if missing. Prune duplicate dock claims against all main trees, then
    /// prior docks in workspace/side/tree order, with warnings. Hidden focused
    /// docks use a fallback; empty visible docks may retain focus.
    ///
    /// Seed allocation from the maximum restored tile ID. This does not check
    /// plain duplicate IDs across main trees; callers should supply globally
    /// unique live IDs. Allocation increments the stored maximum without an
    /// exhaustion check.
    pub fn from_parts(
        spaces: BTreeMap<u8, Workspace>,
        active: u8,
    ) -> Result<(Workspaces, Vec<String>), String> {
        if !(1..=9).contains(&active) {
            return Err(format!("active workspace {active} is out of range 1..=9"));
        }

        let mut spaces: BTreeMap<u8, Workspace> = spaces
            .into_iter()
            .filter(|(ix, _)| (1..=9).contains(ix))
            .collect();
        spaces.entry(active).or_default();

        // Cross-workspace dock-claim healing: every main-tree tile (all
        // workspaces) is claimed first — the tree always wins over a dock
        // — then dock trees are walked in (index, side, tree) order, first
        // claim wins; a duplicate leaf is removed from its dock tree.
        let mut warnings = Vec::new();
        let mut claimed: Vec<TileId> = spaces.values().flat_map(|ws| ws.tree.tiles()).collect();
        for (ix, ws) in spaces.iter_mut() {
            Workspace::heal_duplicate_dock_claims(
                &mut ws.docks,
                &mut claimed,
                &format!("workspace {ix}: "),
                &mut warnings,
            );
            // Also repair hidden focused docks for callers that bypass local
            // workspace recovery. Empty visible docks remain valid focus targets.
            if let FocusRegion::Dock(side) = ws.region
                && !ws.docks.get(side).visible()
            {
                warnings.push(format!(
                    "workspace {ix}: focus region points at the {side:?} dock, \
                     which is hidden; falling back"
                ));
                ws.region = ws.fallback_region();
            }
        }

        let next_tile = claimed.iter().map(|id| id.0).max().unwrap_or(0);

        Ok((
            Workspaces {
                spaces,
                active,
                next_tile,
                // A fresh restore starts a fresh switch-era; no drag can
                // predate it (drags never survive across sessions).
                switch_epoch: 0,
            },
            warnings,
        ))
    }
}

/// Resize step as a fraction of the enclosing split, or of the content area
/// when resizing a dock frame. Used by direct resize actions.
pub const RESIZE_STEP: f32 = 0.03;

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
        // Adding a tile needs a module kind and is handled by `ShellView` via
        // `Workspaces::split_active`; this router handles pure layout actions.
        "workspace::close_tile" => {
            ws.active_mut().close_tile();
            true
        }
        "workspace::fullscreen_tile" => {
            ws.active_mut().toggle_fullscreen();
            true
        }
        "workspace::toggle_split_orientation" => {
            ws.active_mut().toggle_split_orientation();
            true
        }
        "workspace::move_left" => {
            ws.active_mut().move_direction(Direction::Left);
            true
        }
        "workspace::move_down" => {
            ws.active_mut().move_direction(Direction::Down);
            true
        }
        "workspace::move_up" => {
            ws.active_mut().move_direction(Direction::Up);
            true
        }
        "workspace::move_right" => {
            ws.active_mut().move_direction(Direction::Right);
            true
        }
        // "Move split <dir>" (vim model) — shift+arrows move a divider
        // adjacent to the focused tile that direction, by RESIZE_STEP —
        // in whichever tree holds focus; while a dock is focused and no
        // divider can move, the same press moves the dock's inner edge
        // instead (dividers first, frame fallback — see
        // `Workspace::resize`).
        "workspace::resize_left" => {
            ws.active_mut().resize(Direction::Left);
            true
        }
        "workspace::resize_down" => {
            ws.active_mut().resize(Direction::Down);
            true
        }
        "workspace::resize_up" => {
            ws.active_mut().resize(Direction::Up);
            true
        }
        "workspace::resize_right" => {
            ws.active_mut().resize(Direction::Right);
            true
        }
        "dock::toggle_left" => {
            ws.active_mut().toggle_dock(DockSide::Left);
            true
        }
        "dock::toggle_right" => {
            ws.active_mut().toggle_dock(DockSide::Right);
            true
        }
        "dock::toggle_bottom" => {
            ws.active_mut().toggle_dock(DockSide::Bottom);
            true
        }
        "dock::move_left" => {
            ws.active_mut().move_to_dock(DockSide::Left);
            true
        }
        "dock::move_right" => {
            ws.active_mut().move_to_dock(DockSide::Right);
            true
        }
        "dock::move_bottom" => {
            ws.active_mut().move_to_dock(DockSide::Bottom);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::Node;
    use crate::tiling::docks::{DOCK_DEFAULT_SIZE, DOCK_MAX_SIZE, DOCK_MIN_SIZE, Dock};

    fn act(s: &str) -> ActionId {
        ActionId(s.to_string())
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// Two tiles side by side in the tree, focus on the second (right).
    fn two_tiles() -> Workspaces {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Horizontal);
        ws
    }

    #[test]
    fn starts_on_workspace_one_empty() {
        let ws = Workspaces::new();
        assert_eq!(ws.active_index(), 1);
        assert!(ws.active().is_empty());
        assert_eq!(ws.active().region(), FocusRegion::Main);
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

    /// Switching away and back advances the epoch twice. Reselecting the
    /// current index or refusing an invalid index leaves it unchanged.
    #[test]
    fn switch_epoch_counts_actual_switches_only() {
        let mut ws = Workspaces::new();
        let start = ws.switch_epoch();
        assert!(ws.switch(1), "re-selecting the active workspace is valid");
        assert_eq!(ws.switch_epoch(), start, "same-index switch: no bump");
        assert!(!ws.switch(0));
        assert_eq!(ws.switch_epoch(), start, "refused switch: no bump");
        assert!(ws.switch(2));
        assert!(ws.switch(1));
        assert_eq!(
            ws.switch_epoch(),
            start + 2,
            "away-and-back is two bumps — the ABA the epoch exists to expose"
        );
        assert_eq!(ws.active_index(), 1);
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
    fn split_active_creates_and_arranges_tiles() {
        let ws = two_tiles();
        assert_eq!(ws.active().tree().tiles().len(), 2);
        let rects = ws.active().tree().layout(Rect::UNIT);
        assert!((rects[0].1.w - 0.5).abs() < 1e-4);
    }

    #[test]
    fn focus_and_fullscreen_and_close_actions_route() {
        let mut ws = two_tiles();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::focus_left")
        ));
        let left = ws.active().tree().focused().unwrap();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::fullscreen_tile")
        ));
        assert_eq!(ws.active().tree().fullscreen(), Some(left));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::fullscreen_tile")
        ));
        assert_eq!(ws.active().tree().fullscreen(), None);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::close_tile")
        ));
        assert_eq!(ws.active().tree().tiles().len(), 1);
    }

    #[test]
    fn switch_actions_parse_their_index() {
        let mut ws = Workspaces::new();
        assert!(apply_workspace_action(&mut ws, &act("workspace::switch_3")));
        assert_eq!(ws.active_index(), 3);
        assert!(!apply_workspace_action(
            &mut ws,
            &act("workspace::switch_zzz")
        ));
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
        ws.split_active(Orientation::Horizontal);
        let focused = ws.active().tree().focused();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::focus_left")
        ));
        assert_eq!(ws.active().tree().focused(), focused);
    }

    #[test]
    fn a_vertical_split_creates_a_stacked_tile() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Vertical);
        ws.split_active(Orientation::Vertical);
        assert_eq!(ws.active().tree().tiles().len(), 2);
        let rects = ws.active().tree().layout(Rect::UNIT);
        // Vertical (stacked) split: both tiles half-height, not half-width.
        assert!((rects[0].1.h - 0.5).abs() < 1e-4);
        assert!((rects[0].1.w - 1.0).abs() < 1e-4);
    }

    #[test]
    fn move_actions_route_to_tree_move_direction() {
        let mut ws = two_tiles();
        // Two tiles side by side; focus is on the second (rightmost).
        let before = ws.active().tree().layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::move_left")
        ));
        let after = ws.active().tree().layout(Rect::UNIT);
        assert_ne!(before, after, "move_left should swap the two tiles");
    }

    #[test]
    fn move_with_no_neighbor_is_claimed_but_changes_nothing() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        let before = ws.active().tree().layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::move_left")
        ));
        assert_eq!(ws.active().tree().layout(Rect::UNIT), before);
    }

    #[test]
    fn resize_left_moves_the_divider_left_widening_the_focused_rightmost_tile() {
        let mut ws = two_tiles();
        // Focus is the second (rightmost, no divider on its right).
        // resize_left moves the only available divider — its left one —
        // leftward, which widens the focused tile (edge-flip: the key
        // always moves a divider that direction).
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_left")
        ));
        let rects = ws.active().tree().layout(Rect::UNIT);
        let focused_w = rects
            .iter()
            .find(|(id, _)| Some(*id) == ws.active().tree().focused())
            .unwrap()
            .1
            .w;
        assert!(
            (focused_w - (0.5 + RESIZE_STEP)).abs() < 1e-4,
            "resize_left should widen the focused (rightmost) tile by RESIZE_STEP, got {focused_w}"
        );
    }

    #[test]
    fn resize_right_widens_the_focused_leftmost_tile_normal_case() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        // Now focus is the leftmost tile, which has a right neighbor:
        // resize_right moves the divider between them rightward, widening
        // the focused tile (the non-flip case).
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_right")
        ));
        let rects = ws.active().tree().layout(Rect::UNIT);
        let focused_w = rects
            .iter()
            .find(|(id, _)| Some(*id) == ws.active().tree().focused())
            .unwrap()
            .1
            .w;
        assert!(
            (focused_w - (0.5 + RESIZE_STEP)).abs() < 1e-4,
            "resize_right should widen the focused (leftmost) tile by RESIZE_STEP, got {focused_w}"
        );
    }

    // --- Workspace restoration ----------------------------------------

    #[test]
    fn from_parts_rejects_active_out_of_range() {
        assert!(Workspaces::from_parts(BTreeMap::new(), 0).is_err());
        assert!(Workspaces::from_parts(BTreeMap::new(), 10).is_err());
    }

    #[test]
    fn from_parts_inserts_the_active_workspace_empty_if_missing() {
        let (ws, warnings) = Workspaces::from_parts(BTreeMap::new(), 3).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(ws.active_index(), 3);
        assert!(ws.active().is_empty());
    }

    #[test]
    fn from_parts_drops_out_of_range_workspace_keys() {
        let mut spaces = BTreeMap::new();
        spaces.insert(1, Workspace::default());
        spaces.insert(0, Workspace::default());
        spaces.insert(200, Workspace::default());
        let (ws, _) = Workspaces::from_parts(spaces, 1).unwrap();
        assert_eq!(ws.spaces().map(|(ix, _)| ix).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn from_parts_resumes_next_tile_past_the_max_restored_id() {
        let mut tree = Tree::default();
        tree.split(TileId(5), Orientation::Horizontal);
        tree.split(TileId(12), Orientation::Horizontal);
        let mut spaces = BTreeMap::new();
        spaces.insert(1, Workspace::from(tree));
        let (mut ws, _) = Workspaces::from_parts(spaces, 1).unwrap();

        let next = ws.alloc_tile();
        assert_eq!(
            next,
            TileId(13),
            "alloc_tile after restore must not collide with a restored id"
        );
    }

    #[test]
    fn from_parts_resumes_next_tile_past_a_dock_tile_id() {
        let mut tree = Tree::default();
        tree.split(TileId(3), Orientation::Horizontal);
        let mut docks = Docks::default();
        let dock = docks.get_mut(DockSide::Left);
        dock.tree_mut().split(TileId(20), Orientation::Horizontal);
        dock.set_visible(true);
        let (workspace, warnings) =
            Workspace::from_parts(tree, docks, FocusRegion::Dock(DockSide::Left));
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut spaces = BTreeMap::new();
        spaces.insert(1, workspace);
        let (mut ws, warnings) = Workspaces::from_parts(spaces, 1).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            ws.alloc_tile(),
            TileId(21),
            "the next_tile rescan must include dock tiles"
        );
    }

    #[test]
    fn from_parts_with_empty_spaces_starts_next_tile_at_one() {
        let (mut ws, _) = Workspaces::from_parts(BTreeMap::new(), 1).unwrap();
        assert_eq!(ws.alloc_tile(), TileId(1));
    }

    #[test]
    fn from_parts_drops_a_dock_claim_duplicated_in_another_workspace() {
        // A main-tree claim wins over another workspace's dock claim. Removing
        // the dock duplicate warns, but the now-empty visible dock keeps focus.
        let mut tree = Tree::default();
        tree.split(TileId(7), Orientation::Horizontal);
        let mut docks = Docks::default();
        let dock = docks.get_mut(DockSide::Left);
        dock.tree_mut().split(TileId(7), Orientation::Horizontal);
        dock.set_visible(true);
        let (dup_ws, local_warnings) =
            Workspace::from_parts(Tree::default(), docks, FocusRegion::Dock(DockSide::Left));
        // Locally the claim looks fine (its own tree is empty) — the dup
        // is only visible across workspaces.
        assert!(local_warnings.is_empty(), "{local_warnings:?}");
        let mut spaces = BTreeMap::new();
        spaces.insert(1, Workspace::from(tree));
        spaces.insert(2, dup_ws);
        let (ws, warnings) = Workspaces::from_parts(spaces, 2).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}"); // the removed leaf, nothing else
        assert!(ws.active().docks().get(DockSide::Left).tree().is_empty());
        assert_eq!(
            ws.active().region(),
            FocusRegion::Dock(DockSide::Left),
            "an emptied but visible dock keeps the region"
        );
    }

    #[test]
    fn from_parts_keeps_a_dock_trees_unique_tiles_when_one_leaf_is_a_duplicate() {
        // A dock tree holding [8, 7] where 7 also lives in another
        // workspace's main tree: only the duplicate leaf is removed — the
        // dock keeps its other tile, stays focusable, and the region
        // pointing at it survives.
        let mut tree = Tree::default();
        tree.split(TileId(7), Orientation::Horizontal);
        let mut docks = Docks::default();
        let dock = docks.get_mut(DockSide::Left);
        dock.tree_mut().split(TileId(8), Orientation::Horizontal);
        dock.tree_mut().split(TileId(7), Orientation::Horizontal);
        dock.set_visible(true);
        let (dup_ws, _) =
            Workspace::from_parts(Tree::default(), docks, FocusRegion::Dock(DockSide::Left));
        let mut spaces = BTreeMap::new();
        spaces.insert(1, Workspace::from(tree));
        spaces.insert(2, dup_ws);
        let (ws, warnings) = Workspaces::from_parts(spaces, 2).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(dock.tree().tiles(), vec![TileId(8)]);
        assert_eq!(
            dock.tree().focused(),
            Some(TileId(8)),
            "removing the focused duplicate refocuses within the dock"
        );
        assert_eq!(
            ws.active().region(),
            FocusRegion::Dock(DockSide::Left),
            "a still-occupied dock keeps the focus the file asked for"
        );
    }

    #[test]
    fn workspace_from_parts_drops_a_dock_claim_duplicated_in_its_own_tree() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        let mut docks = Docks::default();
        docks
            .get_mut(DockSide::Right)
            .tree_mut()
            .split(TileId(1), Orientation::Horizontal);
        docks.get_mut(DockSide::Right).set_visible(true);
        let (ws, warnings) = Workspace::from_parts(tree, docks, FocusRegion::Main);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(ws.docks().get(DockSide::Right).tree().is_empty());
        assert_eq!(ws.tree().tiles(), vec![TileId(1)]);
    }

    #[test]
    fn workspace_from_parts_drops_a_claim_duplicated_across_its_own_docks() {
        // Left dock (earlier in DockSide::ALL) wins over the right dock:
        // first-claim-wins, deterministically.
        let mut docks = Docks::default();
        docks
            .get_mut(DockSide::Left)
            .tree_mut()
            .split(TileId(5), Orientation::Horizontal);
        docks
            .get_mut(DockSide::Right)
            .tree_mut()
            .split(TileId(5), Orientation::Horizontal);
        let (ws, warnings) = Workspace::from_parts(Tree::default(), docks, FocusRegion::Main);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            ws.docks().get(DockSide::Left).tree().tiles(),
            vec![TileId(5)]
        );
        assert!(ws.docks().get(DockSide::Right).tree().is_empty());
    }

    #[test]
    fn workspace_from_parts_clears_fullscreen_when_a_dock_holds_focus() {
        // Restoring main fullscreen with dock focus clears fullscreen and warns,
        // so the selected dock stays visible and keyboard focus remains usable.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.toggle_fullscreen();
        assert_eq!(tree.fullscreen(), Some(TileId(1)));
        let mut docks = Docks::default();
        docks
            .get_mut(DockSide::Left)
            .tree_mut()
            .split(TileId(2), Orientation::Horizontal);
        docks.get_mut(DockSide::Left).set_visible(true);
        let (ws, warnings) = Workspace::from_parts(tree, docks, FocusRegion::Dock(DockSide::Left));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("fullscreen"), "{warnings:?}");
        assert_eq!(
            ws.region(),
            FocusRegion::Dock(DockSide::Left),
            "the dock focus the file asked for is kept"
        );
        assert_eq!(
            ws.tree().fullscreen(),
            None,
            "fullscreen must be cleared so the docks paint and mod+f works"
        );
    }

    #[test]
    fn workspace_from_parts_keeps_fullscreen_while_main_holds_focus() {
        // The heal must only fire on the contradictory combination —
        // fullscreen with Main focus is an ordinary, reachable state.
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.toggle_fullscreen();
        let (ws, warnings) = Workspace::from_parts(tree, Docks::default(), FocusRegion::Main);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(ws.tree().fullscreen(), Some(TileId(1)));
    }

    #[test]
    fn workspace_from_parts_heals_a_missing_main_tree_focus_to_the_first_tile() {
        // Restore missing main-tree focus to the first tile without warning.
        let tree = Tree::from_parts(Some(Node::Leaf(TileId(3))), None, None).unwrap();
        assert_eq!(tree.focused(), None, "fixture sanity");
        let (ws, warnings) = Workspace::from_parts(tree, Docks::default(), FocusRegion::Main);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            ws.tree().focused(),
            Some(TileId(3)),
            "a non-empty restored tree must have a focused tile"
        );
    }

    #[test]
    fn move_back_after_a_hostile_restore_never_loses_the_tile() {
        // A tile moved back from a dock must survive even when the restored main
        // tree had no valid focus. Workspace recovery supplies focus; tree split
        // also has a first-leaf fallback.
        let tree = Tree::from_parts(Some(Node::Leaf(TileId(1))), None, None).unwrap();
        let mut docks = Docks::default();
        let dock = docks.get_mut(DockSide::Left);
        dock.tree_mut().split(TileId(2), Orientation::Horizontal);
        dock.set_visible(true);
        let (workspace, warnings) =
            Workspace::from_parts(tree, docks, FocusRegion::Dock(DockSide::Left));
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut spaces = BTreeMap::new();
        spaces.insert(1, workspace);
        let (mut ws, warnings) = Workspaces::from_parts(spaces, 1).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        apply_workspace_action(&mut ws, &act("dock::move_left")); // move back

        let mut all: Vec<TileId> = ws.active().tree().tiles();
        all.extend(ws.active().docks().tiles());
        all.sort();
        assert_eq!(
            all,
            vec![TileId(1), TileId(2)],
            "the moved tile must land in the main tree, never vanish"
        );
        assert_eq!(ws.active().tree().focused(), Some(TileId(2)));
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn workspace_from_parts_heals_a_region_pointing_at_a_hidden_dock() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        // Region points at the (empty, hidden) bottom dock.
        let (ws, warnings) =
            Workspace::from_parts(tree, Docks::default(), FocusRegion::Dock(DockSide::Bottom));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            ws.region(),
            FocusRegion::Main,
            "a hidden-dock region must heal to Main"
        );
    }

    #[test]
    fn resize_with_no_matching_split_is_claimed_but_changes_nothing() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        let before = ws.active().tree().layout(Rect::UNIT);
        // Single tile: no ancestor split to resize against.
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_right")
        ));
        assert_eq!(ws.active().tree().layout(Rect::UNIT), before);
    }

    #[test]
    fn toggle_split_orientation_action_reorients_the_focused_split() {
        let mut ws = two_tiles();
        let before = ws.active().tree().layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::toggle_split_orientation")
        ));
        let after = ws.active().tree().layout(Rect::UNIT);
        assert_ne!(before, after, "row should have become a stack");
        // Claimed even when it changes nothing (lone tile), same contract
        // as the resize arms.
        let mut lone = Workspaces::new();
        lone.split_active(Orientation::Horizontal);
        assert!(apply_workspace_action(
            &mut lone,
            &act("workspace::toggle_split_orientation")
        ));
    }

    // --- Divider and dock-edge drags ----------------------------------

    #[test]
    fn drag_main_divider_moves_the_pair_without_touching_focus_or_region() {
        let mut ws = two_tiles(); // focus on the right tile
        let focused_before = ws.active().focused_tile();
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 800.0,
        };
        assert!(ws.active_mut().drag_main_divider(&addr, 300.0, 0.0, bounds));
        let rects = ws.active().tree().layout(Rect::UNIT);
        assert!(approx(rects[0].1.w, 0.3) && approx(rects[1].1.w, 0.7));
        assert_eq!(ws.active().focused_tile(), focused_before);
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn drag_dock_divider_resizes_within_the_docks_own_tree() {
        let mut ws = two_tiles();
        // Park both tiles in the left dock so its tree has a split (a
        // second dock::move_left from the dock would move the tile BACK,
        // so refocus the main region's remaining tile between moves).
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let remaining = ws.active().tree().focused().expect("one tile left in main");
        assert!(ws.active_mut().focus_main_tile(remaining));
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().tiles().len(),
            2
        );
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        let dock_rect = Rect {
            x: 0.0,
            y: 0.0,
            w: 250.0,
            h: 800.0,
        };
        // Side docks insert Horizontal (see `dock_insert_orientation`), so
        // the divider is vertical: drag it to x=62.5 within the 250-wide
        // dock frame → 25%.
        assert!(
            ws.active_mut()
                .drag_dock_divider(DockSide::Left, &addr, 62.5, 0.0, dock_rect)
        );
        let rects = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .layout(Rect::UNIT);
        assert!(approx(rects[0].1.w, 0.25), "got {}", rects[0].1.w);
        // The main tree (now empty) and the dock's frame size are untouched.
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            DOCK_DEFAULT_SIZE
        ));
    }

    #[test]
    fn drag_dock_edge_sets_the_frame_size_with_the_existing_clamps() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 800.0,
        };
        // Cursor at x=400 → 0.40 of the area's width.
        assert!(
            ws.active_mut()
                .drag_dock_edge(DockSide::Left, 400.0, 0.0, area)
        );
        assert!(approx(ws.active().docks().get(DockSide::Left).size(), 0.40));
        // Past the max pins at DOCK_MAX_SIZE (set_size's clamp), and past
        // the min pins at DOCK_MIN_SIZE — clamp-not-fail, like tree drags.
        assert!(
            ws.active_mut()
                .drag_dock_edge(DockSide::Left, 900.0, 0.0, area)
        );
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            DOCK_MAX_SIZE
        ));
        // Repeated positions at the clamp report false so unchanged geometry
        // does not trigger rendering or dirty bookkeeping.
        assert!(
            !ws.active_mut()
                .drag_dock_edge(DockSide::Left, 950.0, 0.0, area)
        );
        assert!(
            !ws.active_mut()
                .drag_dock_edge(DockSide::Left, 999.0, 0.0, area)
        );
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            DOCK_MAX_SIZE
        ));
        assert!(
            ws.active_mut()
                .drag_dock_edge(DockSide::Left, 10.0, 0.0, area)
        );
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            DOCK_MIN_SIZE
        ));
        // And a repeat of the exact same in-range position is a no-change
        // too.
        assert!(
            ws.active_mut()
                .drag_dock_edge(DockSide::Left, 300.0, 0.0, area)
        );
        assert!(
            !ws.active_mut()
                .drag_dock_edge(DockSide::Left, 300.0, 0.0, area)
        );
    }

    #[test]
    fn drag_dock_divider_refuses_a_hidden_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let remaining = ws.active().tree().focused().expect("one tile left in main");
        assert!(ws.active_mut().focus_main_tile(remaining));
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let addr = DividerAddress {
            path: vec![],
            index: 0,
        };
        let dock_rect = Rect {
            x: 0.0,
            y: 0.0,
            w: 250.0,
            h: 800.0,
        };
        // Hiding a dock during a drag makes subsequent applications refuse
        // without changing the retained hidden tree.
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert!(!ws.active().docks().get(DockSide::Left).visible());
        let before = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .layout(Rect::UNIT);
        assert!(
            !ws.active_mut()
                .drag_dock_divider(DockSide::Left, &addr, 62.5, 0.0, dock_rect)
        );
        assert_eq!(
            ws.active()
                .docks()
                .get(DockSide::Left)
                .tree()
                .layout(Rect::UNIT),
            before,
            "a hidden dock's tree must not resize"
        );
    }

    #[test]
    fn drag_dock_edge_refuses_hidden_docks_and_junk_geometry() {
        let mut ws = two_tiles();
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 800.0,
        };
        // Hidden dock: an edge drag reaching it can only be stale.
        assert!(
            !ws.active_mut()
                .drag_dock_edge(DockSide::Left, 400.0, 0.0, area)
        );
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            DOCK_DEFAULT_SIZE
        ));
        // Degenerate area / non-finite cursor: no-op, and crucially NOT a
        // set_size(NaN) — that would "heal" the size back to the default.
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert!(
            ws.active_mut()
                .drag_dock_edge(DockSide::Left, 400.0, 0.0, area)
        );
        let flat = Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        assert!(
            !ws.active_mut()
                .drag_dock_edge(DockSide::Left, 400.0, 0.0, flat)
        );
        assert!(
            !ws.active_mut()
                .drag_dock_edge(DockSide::Left, f32::NAN, 0.0, area)
        );
        assert!(approx(ws.active().docks().get(DockSide::Left).size(), 0.40));
    }

    // --- Dock toggling ------------------------------------------------

    #[test]
    fn fullscreen_hidden_is_none_without_a_fullscreen_tile() {
        let ws = two_tiles();
        assert_eq!(ws.active().fullscreen_hidden(), None);
    }

    #[test]
    fn fullscreen_hidden_counts_the_other_main_tree_slots() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        assert_eq!(
            ws.active().fullscreen_hidden(),
            Some(0),
            "a lone maximised tile hides nothing, but still reads as fullscreen"
        );
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Vertical);
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        assert_eq!(ws.active().fullscreen_hidden(), Some(2));
    }

    #[test]
    fn fullscreen_hidden_counts_a_stack_as_one_slot() {
        let mut ws = two_tiles();
        ws.stack_active().expect("focus is on a tile");
        assert_eq!(ws.active().tree().tiles().len(), 3);
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        assert_eq!(
            ws.active().fullscreen_hidden(),
            Some(1),
            "the stack's hidden member would not show unmaximised either"
        );
    }

    #[test]
    fn fullscreen_hidden_counts_visible_dock_slots_only() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical);
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().slot_count(),
            2
        );
        let main = ws.active().tree().tiles()[0];
        assert!(ws.active_mut().focus_main_tile(main));
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        assert_eq!(ws.active().tree().fullscreen(), Some(main));
        assert_eq!(
            ws.active().fullscreen_hidden(),
            Some(2),
            "fullscreen paints no dock, so a visible dock's tiles are hidden too"
        );
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert!(!ws.active().docks().get(DockSide::Left).visible());
        assert_eq!(
            ws.active().fullscreen_hidden(),
            Some(0),
            "a hidden dock's tiles are hidden with or without fullscreen"
        );
    }

    #[test]
    fn toggle_shows_then_hides_a_dock() {
        let mut ws = Workspaces::new();
        assert!(apply_workspace_action(&mut ws, &act("dock::toggle_left")));
        assert!(ws.active().docks().get(DockSide::Left).visible());
        assert!(apply_workspace_action(&mut ws, &act("dock::toggle_left")));
        assert!(!ws.active().docks().get(DockSide::Left).visible());
    }

    /// Showing even an empty dock gives it focus for the next add. Hiding it
    /// selects the main tree in this fixture.
    #[test]
    fn toggling_a_hidden_dock_shows_it_and_focuses_it_even_when_empty() {
        let mut ws = two_tiles();
        let tree_focused = ws.active().tree().focused();
        apply_workspace_action(&mut ws, &act("dock::toggle_bottom"));
        assert!(ws.active().docks().get(DockSide::Bottom).visible());
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Bottom));
        assert_eq!(
            ws.active().focused_tile(),
            None,
            "an empty focused dock has no focused tile"
        );
        assert_eq!(
            ws.active().tree().focused(),
            tree_focused,
            "the tree's own focus memory is untouched"
        );
        apply_workspace_action(&mut ws, &act("dock::toggle_bottom"));
        assert!(!ws.active().docks().get(DockSide::Bottom).visible());
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert_eq!(ws.active().focused_tile(), tree_focused);
    }

    /// Fullscreen and a focused dock is a combination `render` cannot
    /// paint (no docks while a tile is fullscreen; no focus ring outside
    /// `Main`) and `toggle_fullscreen` cannot undo (it no-ops while a
    /// dock is focused) — so showing a dock must exit any main-tree
    /// fullscreen first, same precedent as `move_to_dock`'s `Main` arm.
    #[test]
    fn toggling_a_dock_visible_exits_main_tree_fullscreen() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        assert!(ws.active().tree().fullscreen().is_some());
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert!(
            ws.active().tree().fullscreen().is_none(),
            "showing a dock must exit main-tree fullscreen"
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert!(ws.active().tree().fullscreen().is_none());
    }

    #[test]
    fn a_hidden_dock_keeps_its_whole_tree() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        // Split inside the dock so hiding has real structure to preserve.
        ws.split_active(Orientation::Vertical);
        let parked = ws.active().docks().get(DockSide::Left).tree().clone();
        assert_eq!(parked.tiles().len(), 2);
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert!(!ws.active().docks().get(DockSide::Left).visible());
        assert_eq!(
            *ws.active().docks().get(DockSide::Left).tree(),
            parked,
            "hiding must not evict or disturb the parked tree (focus memory included)"
        );
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert!(ws.active().docks().get(DockSide::Left).visible());
        assert_eq!(*ws.active().docks().get(DockSide::Left).tree(), parked);
    }

    #[test]
    fn hiding_the_focused_dock_returns_focus_to_main() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        let tree_focused = ws.active().tree().focused();
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert_eq!(
            ws.active().tree().focused(),
            tree_focused,
            "the tree's own focused tile must be untouched"
        );
    }

    /// Layout verbs tolerate a visible empty focused dock without panicking
    /// or leaving the focus region on a hidden dock.
    #[test]
    fn verbs_on_an_empty_focused_dock_are_no_ops_that_keep_the_region_valid() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        for verb in [
            "workspace::close_tile",
            "workspace::fullscreen_tile",
            "workspace::toggle_split_orientation",
            "workspace::resize_left",
            "workspace::move_up",
            "dock::move_left",
            "dock::move_bottom",
        ] {
            apply_workspace_action(&mut ws, &act(verb));
            assert_eq!(ws.active().tree().tiles().len(), 2, "{verb} moved a tile");
            let region = ws.active().region();
            if let FocusRegion::Dock(side) = region {
                assert!(
                    ws.active().docks().get(side).visible(),
                    "{verb} left focus on a hidden dock"
                );
            }
        }
        // Directional focus out of the empty dock lands somewhere valid.
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        let region = ws.active().region();
        if let FocusRegion::Dock(side) = region {
            assert!(ws.active().docks().get(side).visible());
        }
    }

    /// `focus_empty_dock`: the mouse's click-to-focus on
    /// an empty visible dock moves the region there so the next add
    /// lands in it; refused on a hidden dock, an occupied one (a click
    /// there is a tile's, `focus_dock_tile`), and when the region is
    /// already that dock (nothing to persist). The directional rule
    /// (`focus_dock`) still refuses an empty dock.
    #[test]
    fn focus_empty_dock_takes_the_region_only_for_a_visible_empty_dock() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        let w = ws.active_mut();
        assert!(!w.focus_empty_dock(DockSide::Left), "hidden: refused");
        assert_eq!(w.region(), FocusRegion::Main);
        w.docks.get_mut(DockSide::Left).set_visible(true);
        assert!(
            !w.focus_dock(DockSide::Left),
            "directional focus still refuses it"
        );
        assert!(w.focus_empty_dock(DockSide::Left));
        assert_eq!(w.region(), FocusRegion::Dock(DockSide::Left));
        assert!(
            !w.focus_empty_dock(DockSide::Left),
            "already there: nothing changed"
        );
        assert_eq!(ws.split_active(Orientation::Horizontal), TileId(2));
        let w = ws.active_mut();
        assert_eq!(w.docks().get(DockSide::Left).tree().tiles().len(), 1);
        w.focus_main_tile(TileId(1));
        assert!(!w.focus_empty_dock(DockSide::Left), "occupied: refused");
        assert_eq!(w.region(), FocusRegion::Main);
    }

    #[test]
    fn split_active_returns_the_new_focused_tile_in_main_and_in_a_dock() {
        let mut ws = Workspaces::new();
        let first = ws.split_active(Orientation::Horizontal);
        assert_eq!(ws.active().tree().focused(), Some(first));
        let second = ws.split_active(Orientation::Vertical);
        assert_ne!(second, first);
        assert_eq!(ws.active().tree().focused(), Some(second));
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        let in_dock = ws.split_active(Orientation::Horizontal);
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().focused(),
            Some(in_dock)
        );
        assert_eq!(ws.active().focused_tile(), Some(in_dock));
    }

    #[test]
    fn focused_tile_rect_follows_the_region_and_is_none_when_nothing_is_focused() {
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 500.0,
        };
        let mut ws = Workspaces::new();
        assert_eq!(ws.active().focused_tile_rect(area), None);
        ws.split_active(Orientation::Horizontal);
        let whole = ws.active().focused_tile_rect(area).unwrap();
        assert!(approx(whole.w, 1000.0) && approx(whole.h, 500.0));
        ws.split_active(Orientation::Horizontal);
        let right = ws.active().focused_tile_rect(area).unwrap();
        assert!(approx(right.w, 500.0), "{right:?}");
        assert!(right.x > 0.0, "the new tile is the right half");
        // A focused (visible, empty) dock has no focused tile → None.
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert_eq!(ws.active().focused_tile_rect(area), None);
        // Add into the dock: its rect is the dock's column, not the tree's.
        ws.split_active(Orientation::Horizontal);
        let docked = ws.active().focused_tile_rect(area).unwrap();
        assert!(docked.w < 500.0 && approx(docked.x, 0.0), "{docked:?}");
    }

    #[test]
    fn from_parts_keeps_focus_on_a_visible_empty_dock_and_heals_a_hidden_one() {
        let mut docks = Docks::default();
        docks.get_mut(DockSide::Right).set_visible(true);
        let (ws, warnings) =
            Workspace::from_parts(Tree::default(), docks, FocusRegion::Dock(DockSide::Right));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(ws.region(), FocusRegion::Dock(DockSide::Right));

        let (ws, warnings) = Workspace::from_parts(
            Tree::default(),
            Docks::default(),
            FocusRegion::Dock(DockSide::Right),
        );
        assert_eq!(ws.region(), FocusRegion::Main);
        assert!(
            warnings.iter().any(|w| w.contains("hidden")),
            "{warnings:?}"
        );
    }

    /// One tile parked in each side dock, nothing left in the tree, focus
    /// on the right dock. (Built by draining a two-tile tree — a split
    /// can't create tiles while a dock holds focus, by design, so the
    /// tiles must exist before the moves.)
    fn both_side_docks_occupied_tree_empty() -> Workspaces {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("workspace::focus_right")); // back to Main
        apply_workspace_action(&mut ws, &act("dock::move_right"));
        ws
    }

    #[test]
    fn hiding_the_focused_dock_with_an_empty_tree_falls_back_to_another_dock() {
        let mut ws = both_side_docks_occupied_tree_empty();
        assert!(ws.active().tree().is_empty());
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Right));
        apply_workspace_action(&mut ws, &act("dock::toggle_right"));
        assert_eq!(
            ws.active().region(),
            FocusRegion::Dock(DockSide::Left),
            "with an empty tree, focus falls back to an occupied visible dock"
        );
    }

    #[test]
    fn hiding_the_focused_dock_with_nothing_else_falls_back_to_main() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    // --- dock::move_* ----------------------------------------------------

    #[test]
    fn move_from_main_parks_the_tile_shows_the_dock_and_focuses_it() {
        let mut ws = two_tiles();
        let moved = ws.active().tree().focused().unwrap();
        assert!(apply_workspace_action(&mut ws, &act("dock::move_left")));
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(dock.tree().tiles(), vec![moved]);
        assert_eq!(
            dock.tree().focused(),
            Some(moved),
            "focus follows into the dock's tree"
        );
        assert!(dock.visible(), "the dock auto-shows");
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_eq!(ws.active().tree().tiles().len(), 1, "tile left the tree");
        assert!(
            ws.active().tree().focused().is_some(),
            "the tree refocused a neighbor like close does"
        );
    }

    #[test]
    fn move_from_main_into_an_occupied_dock_inserts_into_the_docks_tree() {
        // Dock-trees semantics: the old occupied-target *swap* rule is
        // gone — moving into an occupied dock splits the moved tile in at
        // the dock tree's focused leaf, so the dock simply holds both.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let parked = ws.active().docks().get(DockSide::Left).tree().focused();
        apply_workspace_action(&mut ws, &act("workspace::focus_right")); // → Main
        let moved = ws.active().tree().focused().unwrap();

        apply_workspace_action(&mut ws, &act("dock::move_left"));

        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(dock.tree().tiles().len(), 2, "both tiles live in the dock");
        assert!(dock.tree().contains(parked.unwrap()));
        assert!(dock.tree().contains(moved));
        assert_eq!(
            dock.tree().focused(),
            Some(moved),
            "focus lands on the tile that just arrived"
        );
        // Side-dock insert orientation is horizontal (side by side).
        let rects = dock.tree().layout(Rect::UNIT);
        assert!(
            rects.iter().all(|(_, r)| approx(r.h, 1.0)),
            "left-dock insert must be a horizontal split, got {rects:?}"
        );
        assert!(
            ws.active().tree().is_empty(),
            "the main tree drained completely"
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    #[test]
    fn move_into_the_bottom_dock_inserts_stacked() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        apply_workspace_action(&mut ws, &act("workspace::focus_up")); // → Main
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        let dock = ws.active().docks().get(DockSide::Bottom);
        let rects = dock.tree().layout(Rect::UNIT);
        assert_eq!(rects.len(), 2);
        assert!(
            rects
                .iter()
                .all(|(_, r)| approx(r.w, 1.0) && approx(r.h, 0.5)),
            "bottom-dock insert must be a stacked (vertical) split, got {rects:?}"
        );
    }

    #[test]
    fn move_from_the_same_dock_returns_the_tile_to_the_tree_and_hides_the_dock() {
        let mut ws = two_tiles();
        let moved = ws.active().tree().focused().unwrap();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert_eq!(
            ws.active().tree().focused(),
            Some(moved),
            "focus follows the tile back into the tree"
        );
        assert_eq!(ws.active().tree().tiles().len(), 2);
        let dock = ws.active().docks().get(DockSide::Left);
        assert!(dock.tree().is_empty());
        assert!(!dock.visible(), "the emptied dock auto-hides");
        // Side-dock return is a horizontal (side by side) split.
        let rects = ws.active().tree().layout(Rect::UNIT);
        assert!(rects.iter().all(|(_, r)| approx(r.h, 1.0)));
    }

    #[test]
    fn repeated_move_presses_drain_a_multi_tile_dock_one_tile_per_press() {
        // Two tiles into the left dock, then send them back one press at a
        // time: the first press moves only the dock's focused tile (region
        // follows it to Main; the dock stays visible with the remainder),
        // the second — after focusing the dock again — moves the last one
        // and only then does the dock auto-hide.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("workspace::focus_right")); // → Main
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().tiles().len(),
            2
        );
        assert!(ws.active().tree().is_empty());

        apply_workspace_action(&mut ws, &act("dock::move_left")); // drain #1
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(dock.tree().tiles().len(), 1, "one tile per press");
        assert!(dock.visible(), "a non-empty dock must not auto-hide");
        assert_eq!(ws.active().tree().tiles().len(), 1);
        assert_eq!(ws.active().region(), FocusRegion::Main);

        // Focus returns to the dock (edge-cross left), then drain again.
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        apply_workspace_action(&mut ws, &act("dock::move_left")); // drain #2
        let dock = ws.active().docks().get(DockSide::Left);
        assert!(dock.tree().is_empty());
        assert!(!dock.visible(), "the emptied dock auto-hides");
        assert_eq!(ws.active().tree().tiles().len(), 2);
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn move_back_from_the_bottom_dock_splits_vertically() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        let rects = ws.active().tree().layout(Rect::UNIT);
        assert_eq!(rects.len(), 2);
        // The returning tile stacked onto the focused leaf: both tiles in
        // that slot are full width, half height.
        assert!(
            rects
                .iter()
                .all(|(_, r)| approx(r.w, 1.0) && approx(r.h, 0.5)),
            "bottom-dock return must be a stacked (vertical) split, got {rects:?}"
        );
    }

    #[test]
    fn move_back_into_an_empty_tree_makes_the_tile_the_root() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        let tile = ws.active().tree().focused().unwrap();
        apply_workspace_action(&mut ws, &act("dock::move_right"));
        assert!(ws.active().tree().is_empty());
        apply_workspace_action(&mut ws, &act("dock::move_right"));
        assert_eq!(ws.active().tree().tiles(), vec![tile]);
        assert_eq!(ws.active().tree().focused(), Some(tile));
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn move_between_docks_relocates_and_hides_the_emptied_source() {
        let mut ws = two_tiles();
        let moved = ws.active().tree().focused().unwrap();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        assert_eq!(
            ws.active().docks().get(DockSide::Bottom).tree().tiles(),
            vec![moved]
        );
        assert!(ws.active().docks().get(DockSide::Bottom).visible());
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Bottom));
        let left = ws.active().docks().get(DockSide::Left);
        assert!(left.tree().is_empty());
        assert!(!left.visible(), "the emptied source dock auto-hides");
    }

    #[test]
    fn move_between_docks_inserts_into_an_occupied_targets_tree() {
        // Dock-trees semantics: no swap — the source dock's focused tile
        // joins the target dock's tree; the emptied source auto-hides.
        let mut ws = both_side_docks_occupied_tree_empty();
        let a = ws.active().docks().get(DockSide::Left).tree().focused();
        let b = ws.active().docks().get(DockSide::Right).tree().focused();
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Right));
        // b (focused, in the right dock) moves into the left dock's tree,
        // alongside a; the emptied right dock auto-hides.
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let left = ws.active().docks().get(DockSide::Left);
        assert_eq!(left.tree().tiles().len(), 2);
        assert!(left.tree().contains(a.unwrap()));
        assert!(left.tree().contains(b.unwrap()));
        assert_eq!(left.tree().focused(), b, "focus follows the moved tile");
        let right = ws.active().docks().get(DockSide::Right);
        assert!(right.tree().is_empty());
        assert!(!right.visible(), "the emptied source dock auto-hides");
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    #[test]
    fn move_on_an_empty_workspace_is_claimed_but_a_noop() {
        let mut ws = Workspaces::new();
        assert!(apply_workspace_action(&mut ws, &act("dock::move_left")));
        assert!(ws.active().is_empty());
        assert!(ws.active().docks().get(DockSide::Left).tree().is_empty());
        assert!(!ws.active().docks().get(DockSide::Left).visible());
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn moving_a_fullscreen_tile_to_a_dock_exits_fullscreen() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        assert!(ws.active().tree().fullscreen().is_some());
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert_eq!(
            ws.active().tree().fullscreen(),
            None,
            "a dock move is an explicit layout operation — fullscreen must not linger"
        );
    }

    #[test]
    fn tile_ids_stay_unique_across_tree_and_docks_through_moves() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("workspace::focus_right")); // → Main
        ws.split_active(Orientation::Horizontal); // third tile
        apply_workspace_action(&mut ws, &act("dock::move_left")); // insert (dock holds two)
        apply_workspace_action(&mut ws, &act("dock::move_bottom")); // dock→dock
        let mut all: Vec<TileId> = ws.active().tree().tiles();
        all.extend(ws.active().docks().tiles());
        let mut deduped = all.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(all.len(), deduped.len(), "duplicate TileId found: {all:?}");
        assert_eq!(all.len(), 3, "no tile lost: {all:?}");
    }

    // --- directional focus across regions --------------------------------

    #[test]
    fn focus_left_at_the_tree_edge_enters_a_focusable_left_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        // Back to the tree, then walk left past the edge.
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    /// Directional focus skips hidden and empty docks. Explicitly showing or
    /// clicking an empty dock is a separate focus route.
    #[test]
    fn focus_does_not_enter_a_hidden_or_empty_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        // Left dock hidden+empty: staying put.
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        // Visible but empty: `dock::toggle_left` focuses it directly
        // (that's a different path, covered elsewhere) — back out to
        // Main, then `focus_left` must still not enter it.
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn focus_down_at_the_bottom_edge_enters_the_bottom_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        apply_workspace_action(&mut ws, &act("workspace::focus_up"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        apply_workspace_action(&mut ws, &act("workspace::focus_down"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Bottom));
    }

    #[test]
    fn directional_focus_navigates_within_a_docks_tree_before_crossing() {
        // Two stacked tiles in the left dock, focus on the bottom one.
        // Up moves within the dock's own tree; only at the dock tree's
        // edge does the inward direction cross back to Main.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical);
        let bottom = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .focused()
            .unwrap();
        apply_workspace_action(&mut ws, &act("workspace::focus_up"));
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_ne!(
            dock.tree().focused(),
            Some(bottom),
            "Up must move within the dock's tree first"
        );
        // At the top edge now: Up stays put (not the inward direction).
        apply_workspace_action(&mut ws, &act("workspace::focus_up"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        // Right (inward for the left dock, no neighbor in the stack)
        // crosses back to Main.
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn inward_focus_crosses_only_at_the_dock_trees_edge() {
        // A left dock split side-by-side (two columns): from the left
        // column, Right moves to the dock's own right column; a second
        // Right — now at the edge — crosses into Main.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Horizontal);
        // Focus the dock's left column.
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        let left_col = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .focused()
            .unwrap();
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(
            ws.active().region(),
            FocusRegion::Dock(DockSide::Left),
            "the first Right stays inside the dock"
        );
        assert_ne!(
            ws.active().docks().get(DockSide::Left).tree().focused(),
            Some(left_col)
        );
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(
            ws.active().region(),
            FocusRegion::Main,
            "the edge press crosses back to Main"
        );
    }

    #[test]
    fn inward_focus_from_a_dock_lands_on_the_trees_focused_tile() {
        let mut ws = two_tiles();
        let stays = ws.active().tree().focused();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let tree_focused = ws.active().tree().focused();
        assert_ne!(stays, tree_focused, "sanity: a different tile remains");
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert_eq!(ws.active().tree().focused(), tree_focused);
    }

    #[test]
    fn outward_and_lateral_directions_from_a_dock_are_noops() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        for dir in [
            "workspace::focus_left",
            "workspace::focus_up",
            "workspace::focus_down",
        ] {
            assert!(apply_workspace_action(&mut ws, &act(dir)));
            assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        }
    }

    #[test]
    fn inward_focus_over_an_empty_tree_crosses_to_the_opposite_dock() {
        let mut ws = both_side_docks_occupied_tree_empty();
        assert!(ws.active().tree().is_empty());
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Right));
        // Right dock, inward is Left; the tree is empty, so cross to the
        // opposite (left) dock.
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        // And back.
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Right));
    }

    #[test]
    fn inward_focus_from_the_bottom_dock_over_an_empty_tree_stays_put() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        assert!(ws.active().tree().is_empty());
        apply_workspace_action(&mut ws, &act("workspace::focus_up"));
        assert_eq!(
            ws.active().region(),
            FocusRegion::Dock(DockSide::Bottom),
            "the bottom dock has no opposite side to cross to"
        );
    }

    #[test]
    fn fullscreen_blocks_focus_from_crossing_into_docks() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        apply_workspace_action(&mut ws, &act("workspace::fullscreen_tile"));
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        assert_eq!(
            ws.active().region(),
            FocusRegion::Main,
            "docks are not painted while fullscreen — focus must not enter them"
        );
    }

    // --- resize / close / tree verbs while a dock is focused --------------

    #[test]
    fn resize_in_a_split_dock_moves_the_docks_own_divider_not_the_frame() {
        // An internal dock divider gets the resize press before frame resizing.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical); // vertical split inside
        let frame_before = ws.active().docks().get(DockSide::Left).size();
        let layout_before = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_up")
        ));
        let dock = ws.active().docks().get(DockSide::Left);
        assert_ne!(
            dock.tree().layout(Rect::UNIT),
            layout_before,
            "the divider inside the dock must move"
        );
        assert!(
            approx(dock.size(), frame_before),
            "the dock frame must not resize while a divider can move"
        );
    }

    #[test]
    fn resize_falls_back_to_the_frame_when_no_divider_can_move() {
        // The same press, no split along that axis inside the dock (the
        // dock's split is vertical; press Left/Right): no divider moves,
        // so the frame resizes — and the along-axis frame no-op rule still
        // holds for a lone tile.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical);
        let frame_before = ws.active().docks().get(DockSide::Left).size();
        let layout_before = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_right")
        ));
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(
            dock.tree().layout(Rect::UNIT),
            layout_before,
            "no horizontal divider exists inside the vertical stack"
        );
        assert!(
            approx(dock.size(), frame_before + RESIZE_STEP),
            "the same press falls back to growing the left dock's frame"
        );
    }

    #[test]
    fn resize_grows_and_shrinks_the_focused_left_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let start = ws.active().docks().get(DockSide::Left).size();
        assert!(approx(start, DOCK_DEFAULT_SIZE));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_right")
        ));
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            start + RESIZE_STEP
        ));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_left")
        ));
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            start
        ));
        // Along-axis arrows are no-ops for a side dock.
        apply_workspace_action(&mut ws, &act("workspace::resize_up"));
        apply_workspace_action(&mut ws, &act("workspace::resize_down"));
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            start
        ));
    }

    #[test]
    fn resize_directions_mirror_for_the_right_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_right"));
        let start = ws.active().docks().get(DockSide::Right).size();
        apply_workspace_action(&mut ws, &act("workspace::resize_left"));
        assert!(approx(
            ws.active().docks().get(DockSide::Right).size(),
            start + RESIZE_STEP
        ));
        apply_workspace_action(&mut ws, &act("workspace::resize_right"));
        assert!(approx(
            ws.active().docks().get(DockSide::Right).size(),
            start
        ));
    }

    #[test]
    fn resize_directions_for_the_bottom_dock_are_up_grows_down_shrinks() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        let start = ws.active().docks().get(DockSide::Bottom).size();
        apply_workspace_action(&mut ws, &act("workspace::resize_up"));
        assert!(approx(
            ws.active().docks().get(DockSide::Bottom).size(),
            start + RESIZE_STEP
        ));
        apply_workspace_action(&mut ws, &act("workspace::resize_down"));
        assert!(approx(
            ws.active().docks().get(DockSide::Bottom).size(),
            start
        ));
    }

    #[test]
    fn dock_resize_clamps_at_both_ends() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        for _ in 0..50 {
            apply_workspace_action(&mut ws, &act("workspace::resize_right"));
        }
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            DOCK_MAX_SIZE
        ));
        for _ in 0..50 {
            apply_workspace_action(&mut ws, &act("workspace::resize_left"));
        }
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            DOCK_MIN_SIZE
        ));
    }

    #[test]
    fn resize_while_main_focused_still_moves_tree_dividers_with_docks_present() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        ws.split_active(Orientation::Horizontal);
        let dock_size = ws.active().docks().get(DockSide::Left).size();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_left")
        ));
        assert!(
            approx(ws.active().docks().get(DockSide::Left).size(), dock_size),
            "a Main-focused resize must not touch dock sizes"
        );
    }

    #[test]
    fn close_in_a_multi_tile_dock_refocuses_within_and_keeps_the_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical); // second dock tile
        let closed = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .focused()
            .unwrap();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::close_tile")
        ));
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(dock.tree().tiles().len(), 1);
        assert!(!dock.tree().contains(closed));
        assert!(
            dock.tree().focused().is_some(),
            "the dock tree refocused a neighbor, as Tree::close does"
        );
        assert!(dock.visible(), "a still-occupied dock must not hide");
        assert_eq!(
            ws.active().region(),
            FocusRegion::Dock(DockSide::Left),
            "focus stays in the dock while it has tiles"
        );
    }

    #[test]
    fn close_of_a_docks_last_tile_hides_it_and_falls_back() {
        let mut ws = two_tiles();
        let moved = ws.active().tree().focused().unwrap();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::close_tile")
        ));
        let dock = ws.active().docks().get(DockSide::Left);
        assert!(dock.tree().is_empty());
        assert!(!dock.visible());
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert!(
            !ws.active().tree().contains(moved),
            "close removes the tile entirely — it does not return to the tree"
        );
    }

    #[test]
    fn splits_while_a_dock_is_focused_grow_the_docks_tree() {
        // Dock-trees semantics: the old "splits are refused in a dock"
        // rule is gone — a split lands within the focused dock's tree,
        // allocating from the same app-wide id counter.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let main_before = ws.active().tree().layout(Rect::UNIT);
        ws.split_active(Orientation::Vertical);
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(dock.tree().tiles().len(), 2);
        assert_eq!(
            dock.tree().focused(),
            Some(TileId(3)),
            "the new tile came from the single allocator (ids 1 and 2 exist)"
        );
        // Vertical split: the dock's two tiles stack.
        let rects = dock.tree().layout(Rect::UNIT);
        assert!(
            rects
                .iter()
                .all(|(_, r)| approx(r.w, 1.0) && approx(r.h, 0.5)),
            "{rects:?}"
        );
        assert_eq!(
            ws.active().tree().layout(Rect::UNIT),
            main_before,
            "the main tree is untouched by a dock-focused split"
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    #[test]
    fn directional_move_within_a_dock_swaps_and_never_crosses_regions() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical); // dock: two stacked tiles
        let dock_layout_before = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .layout(Rect::UNIT);
        // The focused (bottom) dock tile swaps upward within the dock.
        assert!(apply_workspace_action(&mut ws, &act("workspace::move_up")));
        let dock = ws.active().docks().get(DockSide::Left);
        assert_ne!(dock.tree().layout(Rect::UNIT), dock_layout_before);
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        // At the dock tree's edge, a directional move is a claimed no-op —
        // it never crosses into Main, exactly like the main tree at its
        // own edge.
        let at_edge = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .layout(Rect::UNIT);
        let main_before = ws.active().tree().layout(Rect::UNIT);
        assert!(apply_workspace_action(&mut ws, &act("workspace::move_up")));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::move_right")
        ));
        assert_eq!(
            ws.active()
                .docks()
                .get(DockSide::Left)
                .tree()
                .layout(Rect::UNIT),
            at_edge
        );
        assert_eq!(ws.active().tree().layout(Rect::UNIT), main_before);
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    #[test]
    fn orientation_toggle_while_a_dock_is_focused_reorients_the_docks_split() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical);
        let before = ws
            .active()
            .docks()
            .get(DockSide::Left)
            .tree()
            .layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::toggle_split_orientation")
        ));
        assert_ne!(
            ws.active()
                .docks()
                .get(DockSide::Left)
                .tree()
                .layout(Rect::UNIT),
            before,
            "the dock's stack should have become a row"
        );
    }

    #[test]
    fn fullscreen_stays_a_claimed_noop_while_a_dock_is_focused() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::fullscreen_tile")
        ));
        assert_eq!(ws.active().tree().fullscreen(), None);
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().fullscreen(),
            None,
            "dock trees never go fullscreen"
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    // --- workspace emptiness / focused_tile -------------------------------

    #[test]
    fn a_workspace_whose_only_tile_is_docked_counts_as_non_empty() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert!(ws.active().tree().is_empty());
        assert!(!ws.active().is_empty());
        assert_eq!(ws.non_empty_indices(), vec![1]);
        // Even hidden, the parked tile keeps the workspace non-empty.
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert_eq!(ws.non_empty_indices(), vec![1]);
    }

    #[test]
    fn focused_tile_follows_the_region() {
        let mut ws = two_tiles();
        let in_tree = ws.active().tree().focused();
        assert_eq!(ws.active().focused_tile(), in_tree);
        apply_workspace_action(&mut ws, &act("dock::move_bottom"));
        assert_eq!(
            ws.active().focused_tile(),
            ws.active().docks().get(DockSide::Bottom).tree().focused()
        );
    }

    #[test]
    fn click_focus_helpers_respect_focusability() {
        let mut ws = two_tiles();
        let left_tile = ws.active().tree().tiles()[0];
        assert!(!ws.active_mut().focus_dock(DockSide::Left), "empty dock");
        apply_workspace_action(&mut ws, &act("dock::move_right"));
        assert!(ws.active_mut().focus_main_tile(left_tile));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert!(ws.active_mut().focus_dock(DockSide::Right));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Right));
        assert!(!ws.active_mut().focus_main_tile(TileId(99)));
        assert_eq!(
            ws.active().region(),
            FocusRegion::Dock(DockSide::Right),
            "a failed click-focus must not move the region"
        );
    }

    #[test]
    fn default_dock_is_default() {
        // Ties `Dock::default()` (used by `Workspace::default`) to the
        // documented defaults the session layer relies on when deciding
        // whether a dock table is worth writing at all.
        assert!(Dock::default().tree().is_empty());
        assert!(!Dock::default().visible());
        assert!(approx(Dock::default().size(), DOCK_DEFAULT_SIZE));
    }

    #[test]
    fn click_focus_on_a_dock_tile_focuses_it_within_the_docks_tree() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        ws.split_active(Orientation::Vertical); // dock: two tiles
        let tiles = ws.active().docks().get(DockSide::Left).tree().tiles();
        let unfocused = tiles
            .iter()
            .copied()
            .find(|id| Some(*id) != ws.active().docks().get(DockSide::Left).tree().focused())
            .unwrap();
        // Click from Main back onto the non-focused dock tile.
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert!(ws.active_mut().focus_dock_tile(DockSide::Left, unfocused));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().focused(),
            Some(unfocused)
        );
        // A tile that isn't in the dock's tree is refused without moving
        // the region.
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert!(!ws.active_mut().focus_dock_tile(DockSide::Left, TileId(99)));
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    // --- Drop operations ----------------------------------------------

    /// Assert the one-place-per-TileId invariant plus focused-iff-root
    /// across all four trees of the active workspace.
    fn assert_tile_invariants(ws: &Workspaces) {
        let workspace = ws.active();
        let mut seen: Vec<TileId> = workspace.tree().tiles();
        assert_eq!(
            workspace.tree().is_empty(),
            workspace.tree().focused().is_none(),
            "main tree: focused iff non-empty"
        );
        for (side, dock) in workspace.docks().iter() {
            assert_eq!(
                dock.tree().is_empty(),
                dock.tree().focused().is_none(),
                "{side:?} dock tree: focused iff non-empty"
            );
            for tile in dock.tree().tiles() {
                assert!(!seen.contains(&tile), "tile {tile:?} lives in two trees");
                seen.push(tile);
            }
        }
        // A focused dock must be visible; it may be empty for the next add.
        if let FocusRegion::Dock(side) = workspace.region() {
            assert!(
                workspace.docks().get(side).visible(),
                "region points at a hidden {side:?} dock"
            );
        }
    }

    /// [1 | 2 | 3] in the main tree, focus on 3.
    fn three_row() -> Workspaces {
        let mut ws = Workspaces::new();
        for _ in 0..3 {
            ws.split_active(Orientation::Horizontal);
        }
        ws
    }

    #[test]
    fn drop_split_moves_a_main_tile_beside_another_main_tile() {
        // Drag 3 onto tile 1's LEFT band: 3 leaves its slot and lands
        // leftmost — [3 | 1 | 2], equalized.
        let mut ws = three_row();
        assert!(
            ws.active_mut()
                .drop_split(TileId(3), TileId(1), Direction::Left)
        );
        assert_eq!(
            ws.active().tree().tiles(),
            vec![TileId(3), TileId(1), TileId(2)]
        );
        assert_eq!(ws.active().tree().focused(), Some(TileId(3)));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_split_top_edge_wraps_the_target_vertically() {
        let mut ws = two_tiles();
        // Drag 2 onto tile 1's TOP band: 1's slot becomes a stack with 2
        // on top; the row collapses to just 1's old slot holding both.
        assert!(
            ws.active_mut()
                .drop_split(TileId(2), TileId(1), Direction::Up)
        );
        let rects = ws.active().tree().layout(Rect::UNIT);
        let r2 = rects.iter().find(|(id, _)| *id == TileId(2)).unwrap().1;
        let r1 = rects.iter().find(|(id, _)| *id == TileId(1)).unwrap().1;
        assert!(approx(r2.y, 0.0) && approx(r2.h, 0.5) && approx(r2.w, 1.0));
        assert!(approx(r1.y, 0.5) && approx(r1.h, 0.5) && approx(r1.w, 1.0));
        assert_eq!(ws.active().tree().focused(), Some(TileId(2)));
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_split_from_main_into_a_dock_tiles_edge() {
        // Tile 3 docked left; drag main tile 2 onto the docked tile's
        // BOTTOM band → 2 joins the dock's tree stacked below 3.
        let mut ws = three_row();
        apply_workspace_action(&mut ws, &act("dock::move_left")); // 3 → dock
        assert!(
            ws.active_mut()
                .drop_split(TileId(2), TileId(3), Direction::Down)
        );
        let dock_tree = ws.active().docks().get(DockSide::Left).tree();
        assert_eq!(dock_tree.tiles(), vec![TileId(3), TileId(2)]);
        assert_eq!(dock_tree.focused(), Some(TileId(2)));
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_eq!(ws.active().tree().tiles(), vec![TileId(1)]);
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_split_from_a_dock_back_onto_a_main_tiles_edge_hides_the_emptied_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_right")); // 2 → right dock
        assert!(ws.active().docks().get(DockSide::Right).visible());
        // Drag the docked 2 onto main tile 1's RIGHT band.
        assert!(
            ws.active_mut()
                .drop_split(TileId(2), TileId(1), Direction::Right)
        );
        assert_eq!(ws.active().tree().tiles(), vec![TileId(1), TileId(2)]);
        assert_eq!(ws.active().tree().focused(), Some(TileId(2)));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert!(
            !ws.active().docks().get(DockSide::Right).visible(),
            "the emptied dock auto-hides"
        );
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_split_between_two_docks() {
        let mut ws = three_row();
        apply_workspace_action(&mut ws, &act("dock::move_left")); // 3 → left
        // Move 2 to the right dock via the keyboard verb: focus main tile 2
        // first (region currently Dock(Left)).
        assert!(ws.active_mut().focus_main_tile(TileId(2)));
        apply_workspace_action(&mut ws, &act("dock::move_right")); // 2 → right
        // Drag the right dock's 2 onto the left dock's 3, left band.
        assert!(
            ws.active_mut()
                .drop_split(TileId(2), TileId(3), Direction::Left)
        );
        let left_tree = ws.active().docks().get(DockSide::Left).tree();
        assert_eq!(left_tree.tiles(), vec![TileId(2), TileId(3)]);
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert!(
            !ws.active().docks().get(DockSide::Right).visible(),
            "the emptied source dock auto-hides"
        );
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_split_self_and_unknown_ids_are_noops() {
        let mut ws = two_tiles();
        let before = ws.active().tree().clone();
        assert!(
            !ws.active_mut()
                .drop_split(TileId(1), TileId(1), Direction::Left)
        );
        assert!(
            !ws.active_mut()
                .drop_split(TileId(9), TileId(1), Direction::Left)
        );
        assert!(
            !ws.active_mut()
                .drop_split(TileId(1), TileId(9), Direction::Left)
        );
        assert_eq!(ws.active().tree(), &before, "no-op drops change nothing");
        assert_tile_invariants(&ws);
    }

    /// An edge drop must refuse a duplicate destination claim before removing
    /// the source tile. Otherwise fallback insertion could duplicate it again.
    /// Construct malformed state directly and check the debug assertion.
    #[test]
    #[should_panic(expected = "one-place-per-TileId")]
    fn drop_split_refuses_a_pre_broken_duplicate_id_before_mutating() {
        let mut w = Workspace::default();
        w.tree.split(TileId(1), Orientation::Horizontal);
        w.tree.split(TileId(2), Orientation::Horizontal);
        let dock = w.docks.get_mut(DockSide::Left);
        dock.tree_mut().split(TileId(3), Orientation::Horizontal);
        // The invariant break: tile 1 claimed by BOTH the main tree and
        // the dock tree.
        dock.tree_mut().split(TileId(1), Orientation::Horizontal);
        dock.set_visible(true);
        // dragged 1 resolves to Main (region_of checks the tree first);
        // target 3 lives in the dock, whose tree also holds a duplicate
        // of the dragged id — the pre-check must fire, not the
        // remove-then-fallback-split.
        w.drop_split(TileId(1), TileId(3), Direction::Right);
    }

    #[test]
    fn stack_active_stacks_onto_the_focused_tile_in_whichever_region_holds_focus() {
        let mut ws = Workspaces::new();
        let a = ws.split_active(Orientation::Horizontal);
        let b = ws.stack_active().expect("a focused tile to stack onto");
        assert_eq!(ws.active().tree().visible_tiles(), vec![b]);
        assert_eq!(ws.active().stack_position(a), Some((1, 2)));
        assert_eq!(ws.active().focused_tile(), Some(b));
        assert!(
            Workspaces::new().stack_active().is_none(),
            "nothing focused, nothing stacked"
        );
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
        assert_eq!(
            ws.active().region_of(b),
            Some(FocusRegion::Dock(DockSide::Left))
        );
        assert!(ws.active_mut().drop_stack(a, b));
        assert!(ws.active().tree().is_empty(), "a left the main tree");
        assert_eq!(
            ws.active().region_of(a),
            Some(FocusRegion::Dock(DockSide::Left))
        );
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

    /// A dock-background drop must refuse a duplicate destination claim before
    /// source removal or unconditional split insertion.
    #[test]
    #[should_panic(expected = "one-place-per-TileId")]
    fn drop_to_dock_refuses_a_pre_broken_duplicate_id_before_mutating() {
        let mut w = Workspace::default();
        w.tree.split(TileId(1), Orientation::Horizontal);
        w.tree.split(TileId(2), Orientation::Horizontal);
        let dock = w.docks.get_mut(DockSide::Left);
        dock.tree_mut().split(TileId(3), Orientation::Horizontal);
        // Tile 1 claimed by BOTH the main tree and the target dock.
        dock.tree_mut().split(TileId(1), Orientation::Horizontal);
        dock.set_visible(true);
        w.drop_to_dock(TileId(1), DockSide::Left);
    }

    #[test]
    fn drop_to_dock_inserts_at_the_dock_trees_focused_leaf_with_the_side_convention() {
        let mut ws = three_row();
        // 3 → left dock via the keyboard verb, then drag 2 onto the left
        // dock's background: it must insert beside the dock tree's focused
        // leaf (3) side by side (Horizontal, the left-dock convention).
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert!(ws.active_mut().drop_to_dock(TileId(2), DockSide::Left));
        let dock_tree = ws.active().docks().get(DockSide::Left).tree();
        assert_eq!(dock_tree.tiles(), vec![TileId(3), TileId(2)]);
        assert_eq!(dock_tree.focused(), Some(TileId(2)));
        let rects = dock_tree.layout(Rect::UNIT);
        assert!(
            approx(rects[0].1.w, 0.5) && approx(rects[0].1.h, 1.0),
            "left dock inserts side by side (Horizontal)"
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_eq!(ws.active().tree().tiles(), vec![TileId(1)]);
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_to_dock_on_an_empty_visible_dock_makes_the_tile_its_root() {
        let mut ws = two_tiles();
        ws.active_mut().toggle_dock(DockSide::Bottom); // visible, empty
        assert!(ws.active_mut().drop_to_dock(TileId(2), DockSide::Bottom));
        let dock = ws.active().docks().get(DockSide::Bottom);
        assert_eq!(dock.tree().tiles(), vec![TileId(2)]);
        assert!(dock.visible() && dock.focusable());
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Bottom));
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_to_dock_from_another_dock_moves_and_hides_the_emptied_source() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left")); // 2 → left
        assert!(ws.active_mut().drop_to_dock(TileId(2), DockSide::Right));
        assert!(!ws.active().docks().get(DockSide::Left).visible());
        assert_eq!(
            ws.active().docks().get(DockSide::Right).tree().tiles(),
            vec![TileId(2)]
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Right));
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_to_dock_onto_the_tiles_own_dock_is_a_noop_not_a_move_back() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left")); // 2 → left
        assert!(
            !ws.active_mut().drop_to_dock(TileId(2), DockSide::Left),
            "a background drop into the source dock leaves the tile there"
        );
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tree().tiles(),
            vec![TileId(2)],
            "nothing moved"
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_verbs_from_a_single_tile_main_tree_empty_it_cleanly() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal); // lone tile 1
        ws.active_mut().toggle_dock(DockSide::Left);
        assert!(ws.active_mut().drop_to_dock(TileId(1), DockSide::Left));
        assert!(ws.active().tree().is_empty());
        assert_eq!(
            ws.active().tree().focused(),
            None,
            "an emptied tree has no focus (focused-iff-root)"
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_tile_invariants(&ws);
    }

    #[test]
    fn drop_split_exits_a_stale_fullscreen_on_the_main_tree() {
        let mut ws = three_row();
        apply_workspace_action(&mut ws, &act("dock::move_left")); // 3 → left dock
        // Fullscreen a main tile, then edge-drop the docked tile beside it.
        assert!(ws.active_mut().focus_main_tile(TileId(1)));
        ws.active_mut().toggle_fullscreen();
        assert!(ws.active().tree().fullscreen().is_some());
        assert!(
            ws.active_mut()
                .drop_split(TileId(3), TileId(1), Direction::Right)
        );
        assert_eq!(
            ws.active().tree().fullscreen(),
            None,
            "an explicit drop trumps a stale fullscreen"
        );
        assert_tile_invariants(&ws);
    }
}
