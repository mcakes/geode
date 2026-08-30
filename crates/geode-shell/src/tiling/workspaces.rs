use super::docks::{DockSide, Docks, FocusRegion};
use super::tree::{Direction, Orientation, TileId, Tree};
use crate::actions::ActionId;
use std::collections::BTreeMap;

#[cfg(test)]
use super::tree::Rect;

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

    /// Empty means *really* empty: no tiles in the tree AND none parked in
    /// any dock (a workspace whose only tile is hidden in a dock still
    /// counts as non-empty — the sidebar indicator must not pretend the
    /// tile is gone).
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty() && self.docks.tiles().next().is_none()
    }

    /// The tile that currently has focus, wherever it lives — the tree's
    /// focused tile while `region` is `Main`, the focused dock's occupant
    /// otherwise. `None` only on a workspace with nothing focusable.
    pub fn focused_tile(&self) -> Option<TileId> {
        match self.region {
            FocusRegion::Main => self.tree.focused(),
            FocusRegion::Dock(side) => self.docks.get(side).tile(),
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

    /// Click-to-focus on a dock: only a focusable (visible + occupied)
    /// dock can take focus — an empty or hidden dock never can.
    pub fn focus_dock(&mut self, side: DockSide) -> bool {
        if self.docks.get(side).focusable() {
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
    /// painted at all, so focus never crosses into them. From a dock, only
    /// the direction back toward center does anything: it lands on the
    /// tree's focused tile, or — when the tree is empty — crosses straight
    /// through to the opposite side dock if that is focusable (the bottom
    /// dock has no opposite, so from it an inward Up over an empty tree
    /// stays put). Every other direction from a dock is a no-op.
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

    /// Move-tile (`ctrl+shift+arrows`) stays a tree concept: swap with the
    /// geometric neighbor while `Main` is focused, no-op from a dock (a
    /// dock has no neighbors to swap with — `dock::move_*` is the verb
    /// that moves tiles between regions).
    pub fn move_direction(&mut self, dir: Direction) {
        if self.region == FocusRegion::Main {
            self.tree.move_direction(dir);
        }
    }

    /// Resize, region-aware. While `Main` is focused this is exactly the
    /// old behavior: move the divider adjacent to the focused tile by
    /// [`RESIZE_STEP`]. While a dock is focused, the same step adjusts
    /// that dock's `size` — [`RESIZE_STEP`] is a fraction of the
    /// containing split there and a fraction of the content area here,
    /// the same kind of unit, so the one constant serves both (recorded
    /// choice: no separate dock step; 0.03 of the content area per press
    /// feels the same as 0.03 of a split). The key names the direction the
    /// dock's *inner* edge moves: the left dock grows on Right and shrinks
    /// on Left, the right dock mirrors that, the bottom dock grows on Up
    /// and shrinks on Down; the two arrows along the dock's own axis are
    /// no-ops. Clamping to the dock size range happens in
    /// [`super::docks::Dock::set_size`].
    pub fn resize(&mut self, dir: Direction) {
        match self.region {
            FocusRegion::Main => {
                self.tree.move_divider(dir, RESIZE_STEP);
            }
            FocusRegion::Dock(side) => {
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

    /// Close the focused tile, wherever it lives. In `Main` this is the
    /// tree's own close (refocus rule unchanged). In a dock the tile is
    /// removed entirely, the now-empty dock auto-hides, and focus falls
    /// back per [`Workspace::fallback_region`].
    pub fn close_tile(&mut self) {
        match self.region {
            FocusRegion::Main => self.tree.close(),
            FocusRegion::Dock(side) => {
                let dock = self.docks.get_mut(side);
                dock.set_tile(None);
                dock.set_visible(false);
                self.region = self.fallback_region();
            }
        }
    }

    /// Fullscreen stays tree-only: while a dock is focused this is a no-op
    /// (still claimed as handled by the router — the keystroke must not
    /// fall through).
    pub fn toggle_fullscreen(&mut self) {
        if self.region == FocusRegion::Main {
            self.tree.toggle_fullscreen();
        }
    }

    /// Split-orientation toggling is a tree concept like the splits
    /// themselves: no-op while a dock is focused.
    pub fn toggle_split_orientation(&mut self) {
        if self.region == FocusRegion::Main {
            self.tree.toggle_split_orientation();
        }
    }

    /// Toggle one dock's visibility. Hidden→visible always works, even on
    /// an empty dock (it shows and renders its "move a tile here" hint).
    /// Visible→hidden keeps the dock's tile parked (toggle back and it's
    /// still there); if the hidden dock held focus, focus falls back per
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

    /// `dock::move_<side>` — move the currently focused tile (wherever it
    /// lives) with respect to dock `side`:
    ///
    /// - Focused in `Main`: the tile leaves the tree and parks in the
    ///   dock, which auto-shows and takes focus. If the dock was occupied
    ///   the two tiles *swap* — the dock's old tile takes the moved tile's
    ///   exact slot in the tree (a leaf-replacement via
    ///   [`Tree::replace_leaf`], deliberately not a remove-then-split,
    ///   which would re-equalize ratios); otherwise the tree refocuses a
    ///   neighbor exactly like close does ([`Tree::remove_focused`]). A
    ///   fullscreen tile exits fullscreen first (see
    ///   [`Tree::exit_fullscreen`]).
    /// - Focused in dock `side` already: the move is "send it back" — the
    ///   tile re-enters the tree at the tree's focused leaf as a split
    ///   (drift note: the brief named a new `Tree::insert_at_focus` API
    ///   for this; [`Tree::split`] already *is* that operation — inserts
    ///   at the focused leaf, becomes root on an empty tree, moves focus
    ///   to the inserted tile — so no second API was added). Orientation:
    ///   Horizontal returning from left/right (the tile arrives side by
    ///   side), Vertical from the bottom (it arrives stacked). The
    ///   now-empty dock auto-hides; focus follows the tile into `Main`.
    /// - Focused in another dock: the tile moves dock-to-dock; if the
    ///   target was occupied the two dock tiles swap (the old occupant
    ///   lands in the source dock, which stays visible), else the source
    ///   empties and auto-hides. The target auto-shows and takes focus.
    /// - Nothing focused anywhere (empty workspace): no-op.
    pub fn move_to_dock(&mut self, side: DockSide) {
        match self.region {
            FocusRegion::Main => {
                let Some(moved) = self.tree.focused() else {
                    return;
                };
                self.tree.exit_fullscreen();
                match self.docks.get(side).tile() {
                    Some(displaced) => {
                        // `moved` is the tree's focused tile (a leaf by
                        // definition) and `displaced` lives only in the
                        // dock (one-place-per-TileId invariant), so this
                        // cannot fail; the debug_assert keeps the swap
                        // honest if that invariant ever broke — a silent
                        // false here followed by `set_tile(Some(moved))`
                        // below would drop the displaced tile and
                        // duplicate the moved one (review nit: match the
                        // defensive posture of the dock arms below).
                        let replaced = self.tree.replace_leaf(moved, displaced);
                        debug_assert!(
                            replaced,
                            "swap leaf-replacement failed: {moved:?} not a leaf or \
                             {displaced:?} already in the tree"
                        );
                    }
                    None => {
                        self.tree.remove_focused();
                    }
                }
                let dock = self.docks.get_mut(side);
                dock.set_tile(Some(moved));
                dock.set_visible(true);
                self.region = FocusRegion::Dock(side);
            }
            FocusRegion::Dock(from) if from == side => {
                let Some(moved) = self.docks.get(side).tile() else {
                    // Unreachable while the region invariant holds; heal
                    // rather than trust it blindly.
                    self.region = self.fallback_region();
                    return;
                };
                let orientation = match side {
                    DockSide::Left | DockSide::Right => Orientation::Horizontal,
                    DockSide::Bottom => Orientation::Vertical,
                };
                self.tree.split(moved, orientation);
                let dock = self.docks.get_mut(side);
                dock.set_tile(None);
                dock.set_visible(false);
                self.region = FocusRegion::Main;
            }
            FocusRegion::Dock(from) => {
                let Some(moved) = self.docks.get(from).tile() else {
                    self.region = self.fallback_region();
                    return;
                };
                let displaced = self.docks.get(side).tile();
                let target = self.docks.get_mut(side);
                target.set_tile(Some(moved));
                target.set_visible(true);
                let source = self.docks.get_mut(from);
                match displaced {
                    Some(displaced) => source.set_tile(Some(displaced)),
                    None => {
                        source.set_tile(None);
                        source.set_visible(false);
                    }
                }
                self.region = FocusRegion::Dock(side);
            }
        }
    }

    /// Reconstruct one workspace from raw session parts, healing what a
    /// hostile/corrupted session file could break *locally* (cross-
    /// workspace duplicate dock claims need the whole set and are healed
    /// in [`Workspaces::from_parts`]): a dock tile that also appears in
    /// this workspace's own tree or an earlier of its own docks loses the
    /// dock's claim (the tree/earlier dock wins — a `TileId` lives in
    /// exactly one place), and a `region` pointing at a dock that isn't
    /// focusable falls back to `Main`. Each healing step contributes a
    /// warning string; none is ever an error.
    pub fn from_parts(tree: Tree, docks: Docks, region: FocusRegion) -> (Workspace, Vec<String>) {
        let mut warnings = Vec::new();
        let mut docks = docks;
        let mut claimed: Vec<TileId> = tree.tiles();
        for side in DockSide::ALL {
            let dock = docks.get_mut(side);
            if let Some(tile) = dock.tile() {
                if claimed.contains(&tile) {
                    warnings.push(format!(
                        "dock tile {} is already placed elsewhere; dropping the {side:?} dock's claim",
                        tile.0
                    ));
                    dock.set_tile(None);
                } else {
                    claimed.push(tile);
                }
            }
        }
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

    /// Split the active workspace's tree, allocating the new tile's id —
    /// unless a dock currently holds focus: splits are a tree concept, so
    /// the verb is then a claimed no-op, and crucially no id is allocated
    /// for a tile that will never exist.
    pub fn split_active(&mut self, orientation: Orientation) {
        if self.active().region() != FocusRegion::Main {
            return;
        }
        let id = self.alloc_tile();
        self.active_mut().tree.split(id, orientation);
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
    /// its own tree/docks) — a dock tile that also appears in any other
    /// workspace's tree, or in an earlier (index, then side) dock claim,
    /// is dropped with a warning, and a `region` left pointing at a
    /// no-longer-focusable dock falls back. Healing, never failure: the
    /// `Ok` variant carries the warnings.
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

        // Cross-workspace dock-claim healing: every tree tile (all
        // workspaces) is claimed first — the tree always wins over a dock
        // — then dock claims are walked in (index, side) order, first
        // claim wins.
        let mut warnings = Vec::new();
        let mut claimed: Vec<TileId> = spaces.values().flat_map(|ws| ws.tree.tiles()).collect();
        for (ix, ws) in spaces.iter_mut() {
            for side in DockSide::ALL {
                let dock = ws.docks.get_mut(side);
                if let Some(tile) = dock.tile() {
                    if claimed.contains(&tile) {
                        warnings.push(format!(
                            "workspace {ix}: dock tile {} is already placed elsewhere; \
                             dropping the {side:?} dock's claim",
                            tile.0
                        ));
                        dock.set_tile(None);
                    } else {
                        claimed.push(tile);
                    }
                }
            }
            // A dropped claim can invalidate the region that pointed at it.
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
        // or, while a dock is focused, move that dock's inner edge (see
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
        dock.set_tile(Some(TileId(20)));
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
        // Workspace 1's tree holds tile 7; workspace 2's left dock claims
        // the same id. The tree wins; the dock claim is dropped with a
        // warning, and workspace 2's region (which pointed at that dock)
        // falls back to Main.
        let mut tree = Tree::default();
        tree.split(TileId(7), Orientation::Horizontal);
        let mut docks = Docks::default();
        let dock = docks.get_mut(DockSide::Left);
        dock.set_tile(Some(TileId(7)));
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
        assert_eq!(warnings.len(), 2, "{warnings:?}"); // dropped claim + region fallback
        assert_eq!(ws.active().docks().get(DockSide::Left).tile(), None);
        assert_eq!(ws.active().region(), FocusRegion::Main);
    }

    #[test]
    fn workspace_from_parts_drops_a_dock_claim_duplicated_in_its_own_tree() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        let mut docks = Docks::default();
        docks.get_mut(DockSide::Right).set_tile(Some(TileId(1)));
        docks.get_mut(DockSide::Right).set_visible(true);
        let (ws, warnings) = Workspace::from_parts(tree, docks, FocusRegion::Main);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(ws.docks().get(DockSide::Right).tile(), None);
        assert_eq!(ws.tree().tiles(), vec![TileId(1)]);
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
        docks.get_mut(DockSide::Left).set_tile(Some(TileId(2)));
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
    fn a_hidden_dock_keeps_its_tile() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let parked = ws.active().docks().get(DockSide::Left).tile();
        assert!(parked.is_some());
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert!(!ws.active().docks().get(DockSide::Left).visible());
        assert_eq!(
            ws.active().docks().get(DockSide::Left).tile(),
            parked,
            "hiding must not evict the parked tile"
        );
        apply_workspace_action(&mut ws, &act("dock::toggle_left"));
        assert!(ws.active().docks().get(DockSide::Left).visible());
        assert_eq!(ws.active().docks().get(DockSide::Left).tile(), parked);
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
        assert_eq!(dock.tile(), Some(moved));
        assert!(dock.visible(), "the dock auto-shows");
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
        assert_eq!(ws.active().tree().tiles().len(), 1, "tile left the tree");
        assert!(
            ws.active().tree().focused().is_some(),
            "the tree refocused a neighbor like close does"
        );
    }

    #[test]
    fn move_from_main_into_an_occupied_dock_swaps_via_leaf_replacement() {
        let mut ws = two_tiles();
        // Park the right tile in the left dock, then move focus to the
        // remaining tree tile and unbalance the layout so a naive
        // remove-then-split would be detectable.
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let parked = ws.active().docks().get(DockSide::Left).tile().unwrap();
        // Return to the tree and split it so it has structure: [a | b].
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        assert_eq!(ws.active().region(), FocusRegion::Main);
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::resize_left"));
        let moved = ws.active().tree().focused().unwrap();
        let slot_before = ws
            .active()
            .tree()
            .layout(Rect::UNIT)
            .into_iter()
            .find(|(id, _)| *id == moved)
            .unwrap()
            .1;

        apply_workspace_action(&mut ws, &act("dock::move_left"));

        // The dock now holds the moved tile; the displaced tile occupies
        // the moved tile's exact old slot (leaf replacement, not a fresh
        // split that would re-equalize ratios).
        assert_eq!(ws.active().docks().get(DockSide::Left).tile(), Some(moved));
        let slot_after = ws
            .active()
            .tree()
            .layout(Rect::UNIT)
            .into_iter()
            .find(|(id, _)| *id == parked)
            .unwrap()
            .1;
        assert_eq!(
            slot_before, slot_after,
            "the displaced dock tile must take the moved tile's exact slot"
        );
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
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
        assert_eq!(dock.tile(), None);
        assert!(!dock.visible(), "the emptied dock auto-hides");
        // Side-dock return is a horizontal (side by side) split.
        let rects = ws.active().tree().layout(Rect::UNIT);
        assert!(rects.iter().all(|(_, r)| approx(r.h, 1.0)));
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
            ws.active().docks().get(DockSide::Bottom).tile(),
            Some(moved)
        );
        assert!(ws.active().docks().get(DockSide::Bottom).visible());
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Bottom));
        let left = ws.active().docks().get(DockSide::Left);
        assert_eq!(left.tile(), None);
        assert!(!left.visible(), "the emptied source dock auto-hides");
    }

    #[test]
    fn move_between_docks_swaps_when_the_target_is_occupied() {
        let mut ws = both_side_docks_occupied_tree_empty();
        let a = ws.active().docks().get(DockSide::Left).tile().unwrap();
        let b = ws.active().docks().get(DockSide::Right).tile().unwrap();
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Right));
        // b (focused, in the right dock) moves to the left dock; a lands
        // in the right dock (the source), which stays visible.
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert_eq!(ws.active().docks().get(DockSide::Left).tile(), Some(b));
        assert_eq!(ws.active().docks().get(DockSide::Right).tile(), Some(a));
        assert!(ws.active().docks().get(DockSide::Right).visible());
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    #[test]
    fn move_on_an_empty_workspace_is_claimed_but_a_noop() {
        let mut ws = Workspaces::new();
        assert!(apply_workspace_action(&mut ws, &act("dock::move_left")));
        assert!(ws.active().is_empty());
        assert_eq!(ws.active().docks().get(DockSide::Left).tile(), None);
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
        apply_workspace_action(&mut ws, &act("dock::move_left")); // swap
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
    fn close_while_dock_focused_removes_the_tile_and_hides_the_dock() {
        let mut ws = two_tiles();
        let moved = ws.active().tree().focused().unwrap();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::close_tile")
        ));
        let dock = ws.active().docks().get(DockSide::Left);
        assert_eq!(dock.tile(), None);
        assert!(!dock.visible());
        assert_eq!(ws.active().region(), FocusRegion::Main);
        assert!(
            !ws.active().tree().contains(moved),
            "close removes the tile entirely — it does not return to the tree"
        );
    }

    #[test]
    fn tree_only_verbs_are_claimed_noops_while_a_dock_is_focused() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        let tree_before = ws.active().tree().layout(Rect::UNIT);
        let tiles_before = ws.active().tree().tiles();

        for verb in [
            "workspace::split_right",
            "workspace::split_down",
            "workspace::move_left",
            "workspace::move_right",
            "workspace::move_up",
            "workspace::move_down",
            "workspace::fullscreen_tile",
            "workspace::toggle_split_orientation",
        ] {
            assert!(apply_workspace_action(&mut ws, &act(verb)), "{verb}");
        }

        assert_eq!(ws.active().tree().layout(Rect::UNIT), tree_before);
        assert_eq!(ws.active().tree().tiles(), tiles_before);
        assert_eq!(ws.active().tree().fullscreen(), None);
        assert_eq!(ws.active().region(), FocusRegion::Dock(DockSide::Left));
    }

    #[test]
    fn split_while_dock_focused_does_not_leak_a_tile_id() {
        let mut ws = two_tiles();
        apply_workspace_action(&mut ws, &act("dock::move_left"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        // Back to Main and split for real: the id allocated must be the
        // next consecutive one — the refused split must not have burned one.
        apply_workspace_action(&mut ws, &act("workspace::focus_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        assert_eq!(
            ws.active().tree().focused(),
            Some(TileId(3)),
            "ids 1 and 2 exist; the dock-focused split must not have consumed 3"
        );
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
            ws.active().docks().get(DockSide::Bottom).tile()
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
        assert_eq!(Dock::default().tile(), None);
        assert!(!Dock::default().visible());
        assert!(approx(Dock::default().size(), DOCK_DEFAULT_SIZE));
    }
}
