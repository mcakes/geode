use super::tree::{Direction, Orientation, TileId, Tree};
use crate::actions::ActionId;
use std::collections::BTreeMap;

#[cfg(test)]
use super::tree::Rect;

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
        Workspaces {
            spaces,
            active: 1,
            next_tile: 0,
        }
    }

    pub fn active_index(&self) -> u8 {
        self.active
    }

    pub fn active(&self) -> &Tree {
        // Invariant: `active` is always a key (established in new/switch).
        &self.spaces[&self.active]
    }

    pub fn active_mut(&mut self) -> &mut Tree {
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

    /// Workspace indices that currently hold at least one tile.
    pub fn non_empty_indices(&self) -> Vec<u8> {
        self.spaces
            .iter()
            .filter(|(_, tree)| !tree.is_empty())
            .map(|(ix, _)| *ix)
            .collect()
    }

    /// Every workspace's index and tree, in index order (session save,
    /// Task 3).
    pub fn spaces(&self) -> impl Iterator<Item = (u8, &Tree)> {
        self.spaces.iter().map(|(ix, tree)| (*ix, tree))
    }

    /// Construct a `Workspaces` from raw parts (session restore, Task 3),
    /// validating and healing what a hostile/corrupted session file could
    /// break: `active` must be in 1..=9, else `Err`; any workspace index
    /// outside 1..=9 present in `spaces` is silently dropped (workspace
    /// indices are always 1..=9, same range `switch` enforces); the active
    /// workspace is inserted empty if it was missing from `spaces`
    /// (mirrors `new`'s own invariant that `active` is always a key).
    ///
    /// `next_tile` is computed to resume past the maximum `TileId` found
    /// across every restored tree, so a subsequent `alloc_tile` can never
    /// collide with a restored id — that's the whole reason this
    /// constructor exists rather than just handing `spaces`/`active` to a
    /// struct literal (the fields are private everywhere else for exactly
    /// this reason: `next_tile` must never be set independently of the ids
    /// actually present).
    pub fn from_parts(spaces: BTreeMap<u8, Tree>, active: u8) -> Result<Workspaces, String> {
        if !(1..=9).contains(&active) {
            return Err(format!("active workspace {active} is out of range 1..=9"));
        }

        let mut spaces: BTreeMap<u8, Tree> = spaces
            .into_iter()
            .filter(|(ix, _)| (1..=9).contains(ix))
            .collect();
        spaces.entry(active).or_default();

        let next_tile = spaces
            .values()
            .flat_map(Tree::tiles)
            .map(|id| id.0)
            .max()
            .unwrap_or(0);

        Ok(Workspaces {
            spaces,
            active,
            next_tile,
        })
    }
}

/// Fraction of the containing split moved per direct resize keystroke
/// (`shift+h/j/k/l`, spec/brief: resize is a direct binding, not a mode).
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
            let id = ws.alloc_tile();
            ws.active_mut().split(id, Orientation::Horizontal);
            true
        }
        "workspace::split_down" => {
            let id = ws.alloc_tile();
            ws.active_mut().split(id, Orientation::Vertical);
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
        // "Resize: grow <dir>" (brief) — shift+h/j/k/l lean the focused
        // tile's edge toward that direction by RESIZE_STEP; shrinking is
        // growing the opposite way (Tree::resize takes a signed delta, but
        // these direct bindings are always the "grow toward dir" case).
        "workspace::resize_left" => {
            ws.active_mut().resize(Direction::Left, RESIZE_STEP);
            true
        }
        "workspace::resize_down" => {
            ws.active_mut().resize(Direction::Down, RESIZE_STEP);
            true
        }
        "workspace::resize_up" => {
            ws.active_mut().resize(Direction::Up, RESIZE_STEP);
            true
        }
        "workspace::resize_right" => {
            ws.active_mut().resize(Direction::Right, RESIZE_STEP);
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
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::split_right")
        ));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::split_right")
        ));
        assert_eq!(ws.active().tiles().len(), 2);
        let rects = ws.active().layout(Rect::UNIT);
        assert!((rects[0].1.w - 0.5).abs() < 1e-4);
    }

    #[test]
    fn focus_and_fullscreen_and_close_actions_route() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::focus_left")
        ));
        let left = ws.active().focused().unwrap();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::fullscreen_tile")
        ));
        assert_eq!(ws.active().fullscreen(), Some(left));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::fullscreen_tile")
        ));
        assert_eq!(ws.active().fullscreen(), None);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::close_tile")
        ));
        assert_eq!(ws.active().tiles().len(), 1);
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
        let focused = ws.active().focused();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::focus_left")
        ));
        assert_eq!(ws.active().focused(), focused);
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
        assert_eq!(ws.active().tiles().len(), 2);
        let rects = ws.active().layout(Rect::UNIT);
        // Vertical (stacked) split: both tiles half-height, not half-width.
        assert!((rects[0].1.h - 0.5).abs() < 1e-4);
        assert!((rects[0].1.w - 1.0).abs() < 1e-4);
    }

    #[test]
    fn move_actions_route_to_tree_move_direction() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        // Two tiles side by side; focus is on the second (rightmost).
        let before = ws.active().layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::move_left")
        ));
        let after = ws.active().layout(Rect::UNIT);
        assert_ne!(before, after, "move_left should swap the two tiles");
    }

    #[test]
    fn move_with_no_neighbor_is_claimed_but_changes_nothing() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        let before = ws.active().layout(Rect::UNIT);
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::move_left")
        ));
        assert_eq!(ws.active().layout(Rect::UNIT), before);
    }

    #[test]
    fn resize_actions_grow_the_focused_tile_toward_the_named_direction() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        // Two tiles side by side (0.5/0.5); focus is the second (rightmost).
        // resize_left grows the focused tile's edge leftward, i.e. its
        // width grows and the left neighbor shrinks.
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_left")
        ));
        let rects = ws.active().layout(Rect::UNIT);
        let focused_w = rects
            .iter()
            .find(|(id, _)| Some(*id) == ws.active().focused())
            .unwrap()
            .1
            .w;
        assert!(
            (focused_w - (0.5 + RESIZE_STEP)).abs() < 1e-4,
            "resize_left should grow the focused tile by RESIZE_STEP, got {focused_w}"
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
        let ws = Workspaces::from_parts(BTreeMap::new(), 3).unwrap();
        assert_eq!(ws.active_index(), 3);
        assert!(ws.active().is_empty());
    }

    #[test]
    fn from_parts_drops_out_of_range_workspace_keys() {
        let mut spaces = BTreeMap::new();
        spaces.insert(1, Tree::default());
        spaces.insert(0, Tree::default());
        spaces.insert(200, Tree::default());
        let ws = Workspaces::from_parts(spaces, 1).unwrap();
        assert_eq!(ws.spaces().map(|(ix, _)| ix).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn from_parts_resumes_next_tile_past_the_max_restored_id() {
        let mut tree = Tree::default();
        tree.split(TileId(5), Orientation::Horizontal);
        tree.split(TileId(12), Orientation::Horizontal);
        let mut spaces = BTreeMap::new();
        spaces.insert(1, tree);
        let mut ws = Workspaces::from_parts(spaces, 1).unwrap();

        let next = ws.alloc_tile();
        assert_eq!(
            next,
            TileId(13),
            "alloc_tile after restore must not collide with a restored id"
        );
    }

    #[test]
    fn from_parts_with_empty_spaces_starts_next_tile_at_one() {
        let mut ws = Workspaces::from_parts(BTreeMap::new(), 1).unwrap();
        assert_eq!(ws.alloc_tile(), TileId(1));
    }

    #[test]
    fn resize_with_no_matching_split_is_claimed_but_changes_nothing() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_right"));
        let before = ws.active().layout(Rect::UNIT);
        // Single tile: no ancestor split to resize against.
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::resize_right")
        ));
        assert_eq!(ws.active().layout(Rect::UNIT), before);
    }
}
