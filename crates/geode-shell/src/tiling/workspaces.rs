use super::docks::{DockSide, Docks, FocusRegion};
use super::tree::{Direction, DividerAddress, Orientation, Rect, TileId, Tree};
use crate::actions::ActionId;
use std::collections::BTreeMap;

/// One workspace's complete layout state (dock-regions task): the i3-style
/// split [`Tree`] for the main working set, the three fixed [`Docks`], and
/// which of those regions currently holds focus. The navigation verbs live
/// here rather than on `Tree` directly because every one of them must
/// consult `region` first — the same keystroke means "move the tree's
/// focus" or "adjust the focused dock" depending on where focus lives, and
/// this struct is the single place that decision is made (the
/// [`apply_workspace_action`] router below just names the verb).
#[derive(Debug, Default)]
pub struct Workspace {
    tree: Tree,
    docks: Docks,
    /// Where focus lives. Invariant: `Dock(side)` only while that dock is
    /// [`focusable`](super::docks::Dock::focusable) (visible AND occupied)
    /// — every verb that can hide or empty a dock re-derives this via
    /// [`Workspace::fallback_region`], and session restore heals
    /// violations back to `Main`.
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

    /// The tile that currently has focus, wherever it lives — the tree's
    /// focused tile while `region` is `Main`, the focused dock's own
    /// tree's focused tile otherwise. `None` only on a workspace with
    /// nothing focusable.
    pub fn focused_tile(&self) -> Option<TileId> {
        match self.region {
            FocusRegion::Main => self.tree.focused(),
            FocusRegion::Dock(side) => self.docks.get(side).tree().focused(),
        }
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

    /// Click-to-focus on a specific tile inside a dock (dock-trees task —
    /// the dock counterpart of [`Workspace::focus_main_tile`]): focus that
    /// tile within the dock's own tree AND move the region there. Refused
    /// (false) when the dock is hidden or the tile isn't in its tree —
    /// a failed click must not move the region.
    pub fn focus_dock_tile(&mut self, side: DockSide, id: TileId) -> bool {
        if self.docks.get(side).visible() && self.docks.get_mut(side).tree_mut().focus(id) {
            self.region = FocusRegion::Dock(side);
            true
        } else {
            false
        }
    }

    /// Directional focus, region-aware. From `Main`, the tree's own
    /// geometric navigation runs first; only at an edge (no neighbor) does
    /// focus cross into the dock on that side — Left/Right into the
    /// left/right dock, Down into the bottom one, Up never (the toolbar
    /// owns the top). While a tree tile is fullscreen the docks are not
    /// painted at all, so focus never crosses into them. From a dock, the
    /// dock's *own* tree navigates first, exactly like the main tree
    /// (dock-trees task); only when it has no neighbor that way does the
    /// region cross — and region crossing stays edge-triggered and
    /// inward-only: the direction back toward center lands on the tree's
    /// focused tile, or — when the tree is empty — crosses straight
    /// through to the opposite side dock if that is focusable (the bottom
    /// dock has no opposite, so from it an inward Up over an empty tree
    /// stays put). Every other direction at a dock tree's edge is a no-op.
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

    /// Move-tile (`ctrl+shift+arrows`), region-aware (dock-trees task):
    /// swap with the geometric neighbor within whichever tree holds focus
    /// — the main tree while `Main` is focused, the focused dock's own
    /// tree otherwise. A directional move never crosses regions: at the
    /// dock tree's edge it is a no-op, exactly like the main tree at its
    /// own edge (`dock::move_*` is the verb that moves tiles between
    /// regions).
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

    /// Resize, region-aware. While `Main` is focused this is exactly the
    /// old behavior: move the divider adjacent to the focused tile by
    /// [`RESIZE_STEP`]. While a dock is focused: dividers first, frame
    /// fallback (dock-trees task) — the dock's own tree gets first claim
    /// via its `move_divider`, so a split dock resizes internally exactly
    /// like the main tree; only when no divider moves (a lone tile, no
    /// split along that axis, or the step was clamped out by `MIN_RATIO` —
    /// i.e. `move_divider` reported no change) does the *same press* fall
    /// back to resizing the dock frame itself, adjusting the dock's
    /// `size`. [`RESIZE_STEP`] is a fraction of the containing split there
    /// and a fraction of the content area here, the same kind of unit, so
    /// the one constant serves both (recorded choice: no separate dock
    /// step). For the frame fallback the key names the direction the
    /// dock's *inner* edge moves: the left dock grows on Right and shrinks
    /// on Left, the right dock mirrors that, the bottom dock grows on Up
    /// and shrinks on Down; the two arrows along the dock's own axis are
    /// no-ops (a divider along that axis inside the dock tree still moves,
    /// though — the fallback is per-press, not per-direction-class).
    /// Clamping to the dock size range happens in
    /// [`super::docks::Dock::set_size`].
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

    /// Drag one of the main tree's dividers to an absolute cursor position
    /// (drag-splitters task): a thin forwarding verb to
    /// [`Tree::drag_divider`], which owns all the validation and clamp
    /// semantics. Lives here (like [`Workspace::resize`]) because the
    /// tree field is private — but unlike `resize` it is NOT region-aware:
    /// a drag names its divider by address, not by where keyboard focus
    /// happens to live, and deliberately never moves focus or the region.
    pub fn drag_main_divider(
        &mut self,
        address: &DividerAddress,
        x: f32,
        y: f32,
        bounds: Rect,
    ) -> bool {
        self.tree.drag_divider(address, x, y, bounds)
    }

    /// [`Workspace::drag_main_divider`]'s dock counterpart: drag a divider
    /// *inside* one dock's own tree. `bounds` is that dock's laid-out
    /// frame rect (the drag re-derives sub-rects from it, same as the main
    /// tree from the tree area). No visibility check on purpose: a dock
    /// hidden mid-drag (ctrl+[ while the button is down) keeps its tree,
    /// and applying the remaining drag to the hidden tree is harmless
    /// where refusing it would make the already-applied part of the drag
    /// final in a surprising place. Focus and region stay untouched.
    pub fn drag_dock_divider(
        &mut self,
        side: DockSide,
        address: &DividerAddress,
        x: f32,
        y: f32,
        bounds: Rect,
    ) -> bool {
        self.docks
            .get_mut(side)
            .tree_mut()
            .drag_divider(address, x, y, bounds)
    }

    /// Drag a dock's frame edge (the boundary between the dock and the
    /// main area) to an absolute cursor position: projects the cursor onto
    /// a size fraction of `area` (the content area `dock_layout` carves —
    /// the same rect keyboard resize's `RESIZE_STEP` is a fraction of) via
    /// [`super::dividers::dock_size_from_position`], then routes it
    /// through [`super::docks::Dock::set_size`], which owns the
    /// 0.10..=0.50 clamp — so dragging past the range pins at the clamp,
    /// mirroring [`Tree::drag_divider`]'s clamp-not-fail behavior. `false`
    /// (untouched) for a degenerate area/cursor or a hidden dock — a
    /// hidden dock has no visible edge, so an edge drag reaching it can
    /// only be stale.
    pub fn drag_dock_edge(&mut self, side: DockSide, x: f32, y: f32, area: Rect) -> bool {
        let Some(frac) = super::dividers::dock_size_from_position(side, x, y, area) else {
            return false;
        };
        let dock = self.docks.get_mut(side);
        if !dock.visible() {
            return false;
        }
        dock.set_size(frac);
        true
    }

    /// Close the focused tile, wherever it lives. In `Main` this is the
    /// tree's own close (refocus rule unchanged). In a dock it is the dock
    /// tree's own close just the same (dock-trees task — the tree
    /// refocuses a neighbor within the dock); only when that close empties
    /// the dock's tree does the dock auto-hide and focus fall back per
    /// [`Workspace::fallback_region`].
    pub fn close_tile(&mut self) {
        match self.region {
            FocusRegion::Main => self.tree.close(),
            FocusRegion::Dock(side) => {
                let dock = self.docks.get_mut(side);
                dock.tree_mut().close();
                if dock.tree().is_empty() {
                    dock.set_visible(false);
                    self.region = self.fallback_region();
                }
            }
        }
    }

    /// Fullscreen stays main-tree-only, even now that docks are trees:
    /// while a dock is focused this is a no-op (still claimed as handled
    /// by the router — the keystroke must not fall through). A fullscreen
    /// dock tile would cover only its dock's little frame, a state with no
    /// meaning the dock frame toggle (`dock::toggle_*`) doesn't already
    /// serve better.
    pub fn toggle_fullscreen(&mut self) {
        if self.region == FocusRegion::Main {
            self.tree.toggle_fullscreen();
        }
    }

    /// Reorient the split around the focused tile, in whichever tree holds
    /// focus (dock-trees task: orientations come with the tree — a dock's
    /// stack of tiles can be turned into a row and back just like the main
    /// tree's).
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

    /// Toggle one dock's visibility. Hidden→visible always works, even on
    /// an empty dock (it shows and renders its "move a tile here" hint).
    /// Visible→hidden keeps the dock's whole tree parked — splits, ratios,
    /// and focus memory included (toggle back and it's all still there);
    /// if the hidden dock held focus, focus falls back per
    /// [`Workspace::fallback_region`].
    pub fn toggle_dock(&mut self, side: DockSide) {
        let dock = self.docks.get_mut(side);
        if dock.visible() {
            dock.set_visible(false);
            if self.region == FocusRegion::Dock(side) {
                self.region = self.fallback_region();
            }
        } else {
            dock.set_visible(true);
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

    /// `dock::move_<side>` — move the currently focused tile (wherever it
    /// lives) with respect to dock `side`. Dock-trees task: the old
    /// "at most one tile per dock, occupied target swaps" rule is gone —
    /// a dock holds a whole tree, so moves *insert*:
    ///
    /// - Focused in `Main`: the tile leaves the tree (the tree refocuses a
    ///   neighbor exactly like close does — [`Tree::remove_focused`]) and
    ///   enters dock `side`'s tree at that tree's focused leaf via
    ///   [`Tree::split`] (which is precisely "insert at focus": becomes
    ///   root on an empty tree, focus moves to the inserted tile),
    ///   orientation per [`Workspace::dock_insert_orientation`]. The dock
    ///   auto-shows and takes focus. A fullscreen tile exits fullscreen
    ///   first (see [`Tree::exit_fullscreen`]).
    /// - Focused in dock `side` already: the move is "send it back" — the
    ///   dock tree's focused tile (only that one; repeated presses drain
    ///   the dock one tile per press) re-enters the main tree at its
    ///   focused leaf as a split, same orientation convention. The dock
    ///   auto-hides only when its tree empties; focus follows the tile
    ///   into `Main` either way.
    /// - Focused in another dock: the source dock tree's focused tile
    ///   moves dock-to-dock, inserted into the target's tree likewise; the
    ///   source auto-hides only when it empties. The target auto-shows and
    ///   takes focus.
    /// - Nothing focused anywhere (empty workspace): no-op.
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
                let dock = self.docks.get_mut(side);
                dock.tree_mut()
                    .split(moved, Self::dock_insert_orientation(side));
                dock.set_visible(true);
                self.region = FocusRegion::Dock(side);
            }
            FocusRegion::Dock(from) if from == side => {
                let Some(moved) = self.docks.get_mut(side).tree_mut().remove_focused() else {
                    // Unreachable while the region invariant holds (a
                    // focused dock is occupied); heal rather than trust
                    // it blindly.
                    self.region = self.fallback_region();
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
                    self.region = self.fallback_region();
                    return;
                };
                if self.docks.get(from).tree().is_empty() {
                    self.docks.get_mut(from).set_visible(false);
                }
                let target = self.docks.get_mut(side);
                target
                    .tree_mut()
                    .split(moved, Self::dock_insert_orientation(side));
                target.set_visible(true);
                self.region = FocusRegion::Dock(side);
            }
        }
    }

    /// Heal duplicate tile claims across one workspace-or-app-wide set of
    /// dock trees against an already-`claimed` list (dock-trees task —
    /// shared by [`Workspace::from_parts`], which seeds `claimed` with its
    /// own tree, and [`Workspaces::from_parts`], which seeds it with every
    /// workspace's tree): docks are walked in [`DockSide::ALL`] order and
    /// each dock's tiles in tree order, so "tree wins, then first claim
    /// wins" stays deterministic. A duplicate leaf is *removed from the
    /// dock's tree* (the tree-shaped generalization of the old "drop the
    /// dock's claim"; [`Tree::remove`] collapses/renormalizes like close),
    /// with `label` prefixing each warning ("" for the local pass).
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

    /// Reconstruct one workspace from raw session parts, healing what a
    /// hostile/corrupted session file could break *locally* (cross-
    /// workspace duplicate dock claims need the whole set and are healed
    /// in [`Workspaces::from_parts`]): a dock-tree tile that also appears
    /// in this workspace's own tree or an earlier of its own docks is
    /// removed from the dock's tree (the tree/earlier dock wins — a
    /// `TileId` lives in exactly one of the four trees), and a `region`
    /// pointing at a dock that isn't focusable falls back to `Main`. Each
    /// healing step contributes a warning string; none is ever an error.
    pub fn from_parts(tree: Tree, docks: Docks, region: FocusRegion) -> (Workspace, Vec<String>) {
        let mut warnings = Vec::new();
        let mut tree = tree;
        let mut docks = docks;
        // Mirror `Dock::from_parts`'s focus heal for the main tree (review
        // fix): a non-empty tree whose `focused` was healed away (dangling
        // reference → `None` in `Tree::from_parts`) refocuses its first
        // tile, silently — the verbs lean on "focused is Some whenever
        // root is Some" (e.g. `move_to_dock`'s same-side arm removes a
        // tile from a dock and hands it to this tree's `split`; without a
        // focus, the split would have to fall back to its degenerate
        // first-leaf anchor, and before that guard existed it silently
        // dropped the tile).
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
            && !ws.docks.get(side).focusable()
        {
            warnings.push(format!(
                "focus region points at the {side:?} dock, which is hidden or empty; falling back"
            ));
            ws.region = ws.fallback_region();
        }
        // Fullscreen-on-the-tree while focus lives in a dock is a
        // contradiction no live path can produce (fullscreen blocks focus
        // from crossing into docks, and `toggle_fullscreen` is a no-op
        // while dock-focused) but a hand-edited session file can claim
        // both — restored as-is it would render the fullscreen tile with
        // no focus ring anywhere and leave mod+f dead until focus returned
        // inward. Heal by clearing fullscreen and keeping the dock focus
        // the file asked for (review should-fix). Checked after the
        // region heal above so it only fires when the dock focus actually
        // survives — a region that fell back to `Main` may keep its
        // fullscreen, an ordinary state.
        //
        // `Workspaces::from_parts`'s cross-workspace pass cannot
        // reintroduce this combination: its healing only ever drops dock
        // claims and moves regions *toward* `Main` (and a fullscreen tree
        // is by definition non-empty, so its fallback is always `Main`),
        // so this local heal is the single seam that needs the check.
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

/// The app-global workspace set (spec §3.6: workspaces are global; windows
/// are viewports). Indices 1..=9; workspaces materialize lazily on first
/// switch and persist (empty ones stay listed as empty).
#[derive(Debug)]
pub struct Workspaces {
    spaces: BTreeMap<u8, Workspace>,
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
        spaces.insert(1, Workspace::default());
        Workspaces {
            spaces,
            active: 1,
            next_tile: 0,
        }
    }

    pub fn active_index(&self) -> u8 {
        self.active
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

    /// Split at the focus, wherever it lives (dock-trees task: the old
    /// "splits are refused while a dock holds focus" rule is gone — a dock
    /// holds a full tree, so `ctrl+h`/`ctrl+v` split within the focused
    /// dock's tree exactly as they do the main tree), allocating the new
    /// tile's id from the single app-wide allocator either way.
    pub fn split_active(&mut self, orientation: Orientation) {
        let id = self.alloc_tile();
        let ws = self.active_mut();
        match ws.region {
            FocusRegion::Main => ws.tree.split(id, orientation),
            FocusRegion::Dock(side) => ws.docks.get_mut(side).tree_mut().split(id, orientation),
        }
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

    /// Every workspace's index and state, in index order (session save,
    /// Task 3).
    pub fn spaces(&self) -> impl Iterator<Item = (u8, &Workspace)> {
        self.spaces.iter().map(|(ix, ws)| (*ix, ws))
    }

    /// Construct a `Workspaces` from raw parts (session restore, Task 3),
    /// validating and healing what a hostile/corrupted session file could
    /// break: `active` must be in 1..=9, else `Err`; any workspace index
    /// outside 1..=9 present in `spaces` is silently dropped (workspace
    /// indices are always 1..=9, same range `switch` enforces); the active
    /// workspace is inserted empty if it was missing from `spaces`
    /// (mirrors `new`'s own invariant that `active` is always a key).
    ///
    /// Dock-regions task: dock claims are healed *across* workspaces here
    /// (the per-workspace pass in [`Workspace::from_parts`] can only see
    /// its own tree/docks) — a dock-tree tile that also appears in any
    /// other workspace's main tree, or in an earlier (index, then side,
    /// then tree order) dock claim, is removed from its dock's tree with a
    /// warning, and a `region` left pointing at a no-longer-focusable dock
    /// falls back. Healing, never failure: the `Ok` variant carries the
    /// warnings.
    ///
    /// `next_tile` is computed to resume past the maximum `TileId` found
    /// across every restored tree AND dock, so a subsequent `alloc_tile`
    /// can never collide with a restored id — that's the whole reason this
    /// constructor exists rather than just handing `spaces`/`active` to a
    /// struct literal (the fields are private everywhere else for exactly
    /// this reason: `next_tile` must never be set independently of the ids
    /// actually present).
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
            // A removed claim can empty the dock the region pointed at.
            if let FocusRegion::Dock(side) = ws.region
                && !ws.docks.get(side).focusable()
            {
                warnings.push(format!(
                    "workspace {ix}: focus region points at the {side:?} dock, \
                     which is hidden or empty; falling back"
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
            },
            warnings,
        ))
    }
}

/// Fraction of the containing split moved per direct resize keystroke
/// (`shift+h/j/k/l`, spec/brief: resize is a direct binding, not a mode).
/// Dock-regions task: also the per-keystroke dock resize step, as a
/// fraction of the content area (see [`Workspace::resize`]).
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
        // Vim naming (user direction): split_right = Orientation::Horizontal
        // (side by side, vim :vsplit, bound ctrl+v); split_down =
        // Orientation::Vertical (stacked, vim :split, bound ctrl+h). The
        // direction-named ids exist so the vim bindings don't read backwards
        // (see BUILTIN_KEYMAP).
        "workspace::split_right" => {
            ws.split_active(Orientation::Horizontal);
            true
        }
        "workspace::split_down" => {
            ws.split_active(Orientation::Vertical);
            true
        }
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        let focused = ws.active().tree().focused();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::focus_left")
        ));
        assert_eq!(ws.active().tree().focused(), focused);
    }

    #[test]
    fn split_down_creates_a_stacked_tile() {
        let mut ws = Workspaces::new();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::split_down")
        ));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::split_down")
        ));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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

    // --- Workspaces::from_parts (Task 3: session restore) --------------

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
        // Workspace 1's tree holds tile 7; workspace 2's left dock tree
        // claims the same id. The tree wins; the duplicate leaf is removed
        // from the dock's tree with a warning, and workspace 2's region
        // (which pointed at that now-empty dock) falls back to Main.
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
        assert_eq!(warnings.len(), 2, "{warnings:?}"); // removed leaf + region fallback
        assert!(ws.active().docks().get(DockSide::Left).tree().is_empty());
        assert_eq!(ws.active().region(), FocusRegion::Main);
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
        // Review should-fix: fullscreen-on-the-tree plus focus-in-a-dock is
        // unreachable live (mod+f is a no-op while dock-focused; focus
        // can't cross into docks under fullscreen) but a hand-edited
        // session file can claim both. Restored as-is it renders the
        // fullscreen tile with no focus ring anywhere and mod+f dead. The
        // heal keeps the dock focus the user asked for and clears
        // fullscreen, with a warning.
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
        // Review fix: `Tree::from_parts` heals a dangling `focused` to
        // None while keeping the root — restored as-is, the main tree
        // then violated "focused is Some whenever root is Some" and the
        // move-back verb could drop a tile into `split`'s degenerate arm.
        // The workspace-level heal mirrors `Dock::from_parts`: refocus
        // the first tile, silently.
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
        // Review repro: main-tree node with a dangling/missing `focused`,
        // focus in an occupied left dock. The move-back chord removes the
        // tile from the dock and hands it to the main tree's split —
        // before the fixes that split silently dropped it. Both ends now
        // guard: restore heals the main tree's focus, and even without
        // focus, split anchors at the first leaf.
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
    fn workspace_from_parts_heals_a_region_pointing_at_an_unfocusable_dock() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        // Region points at the (empty, hidden) bottom dock.
        let (ws, warnings) =
            Workspace::from_parts(tree, Docks::default(), FocusRegion::Dock(DockSide::Bottom));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            ws.region(),
            FocusRegion::Main,
            "an unfocusable-dock region must heal to Main"
        );
    }

    #[test]
    fn resize_with_no_matching_split_is_claimed_but_changes_nothing() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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
        apply_workspace_action(&mut lone, &act("workspace::split_right"));
        assert!(apply_workspace_action(
            &mut lone,
            &act("workspace::toggle_split_orientation")
        ));
    }

    // --- divider / dock-edge drags (drag-splitters task) ----------------

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
        assert!(
            ws.active_mut()
                .drag_dock_edge(DockSide::Left, 10.0, 0.0, area)
        );
        assert!(approx(
            ws.active().docks().get(DockSide::Left).size(),
            DOCK_MIN_SIZE
        ));
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

    // --- dock toggling (dock-regions task) ------------------------------

    #[test]
    fn toggle_shows_then_hides_a_dock() {
        let mut ws = Workspaces::new();
        assert!(apply_workspace_action(&mut ws, &act("dock::toggle_left")));
        assert!(ws.active().docks().get(DockSide::Left).visible());
        assert!(apply_workspace_action(&mut ws, &act("dock::toggle_left")));
        assert!(!ws.active().docks().get(DockSide::Left).visible());
    }

    #[test]
    fn toggling_an_empty_dock_shows_it_without_taking_focus() {
        let mut ws = two_tiles();
        let focused = ws.active().tree().focused();
        apply_workspace_action(&mut ws, &act("dock::toggle_bottom"));
        assert!(ws.active().docks().get(DockSide::Bottom).visible());
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert_eq!(ws.active().tree().focused(), focused);
    }

    #[test]
    fn a_hidden_dock_keeps_its_whole_tree() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        // Split inside the dock so hiding has real structure to preserve.
        apply_workspace_action(&mut ws, &act("workspace::split_down"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right")); // third tile
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

    #[test]
    fn focus_does_not_enter_a_hidden_or_empty_dock() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        // Left dock hidden+empty: staying put.
        apply_workspace_action(&mut ws, &act("workspace::focus_left"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        // Visible but empty: still not focusable.
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_down"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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
        // "Dividers, frame fallback" (dock-trees task), pinned: with a
        // split inside the dock along the pressed axis, the press moves
        // the dock tree's divider and leaves the frame size alone.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("workspace::split_down")); // vertical split inside
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
        apply_workspace_action(&mut ws, &act("workspace::split_down"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_down")); // second dock tile
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
        // rule is gone — ctrl+h/ctrl+v split within the focused dock's
        // tree, allocating from the same app-wide id counter.
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let main_before = ws.active().tree().layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::split_down")
        ));
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
        apply_workspace_action(&mut ws, &act("workspace::split_down")); // dock: two stacked tiles
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
        apply_workspace_action(&mut ws, &act("workspace::split_down"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_down")); // dock: two tiles
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
}
