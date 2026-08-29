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
            &act("workspace::split_horizontal")
        ));
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::split_horizontal")
        ));
        assert_eq!(ws.active().tiles().len(), 2);
        let rects = ws.active().layout(Rect::UNIT);
        assert!((rects[0].1.w - 0.5).abs() < 1e-4);
    }

    #[test]
    fn focus_and_fullscreen_and_close_actions_route() {
        let mut ws = Workspaces::new();
        apply_workspace_action(&mut ws, &act("workspace::split_horizontal"));
        apply_workspace_action(&mut ws, &act("workspace::split_horizontal"));
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
        apply_workspace_action(&mut ws, &act("workspace::split_horizontal"));
        let focused = ws.active().focused();
        assert!(apply_workspace_action(
            &mut ws,
            &act("workspace::focus_left")
        ));
        assert_eq!(ws.active().focused(), focused);
    }
}
