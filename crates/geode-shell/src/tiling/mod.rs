//! Tiling window-management core (spec §3.1, §3.6): the pure tree that is
//! the single source of truth for workspace layout. Rendering (Phase 1b-ui)
//! consumes [`Tree::layout`]; directional navigation uses the same geometry,
//! so what you see is what hjkl navigates. No gpui here (spec §10.3).
//! Dock-regions task (generalized by dock-trees): each workspace also
//! carries three fixed docks ([`Docks`]) beside its tree — each dock
//! holding a full [`Tree`] of its own — unified under [`Workspace`], whose
//! verbs route by [`FocusRegion`]; [`docks::layout`] carves the dock
//! frames, and each visible dock's own `Tree::layout` places its tiles.

mod dividers;
mod docks;
mod dropzones;
mod tree;
mod workspaces;

pub use dividers::{
    DIVIDER_HIT_WIDTH, DividerStrip, DockEdgeStrip, divider_strips, dock_edge_strips,
    dock_size_from_position,
};
pub use dropzones::{
    DROP_EDGE_BAND, DropTarget, DropZone, classify_drop_zone, drop_highlight_rect, hit_tile,
    locate_drop_target,
};
pub use docks::{
    DOCK_DEFAULT_SIZE, DOCK_MAX_SIZE, DOCK_MIN_SIZE, Dock, DockSide, Docks, FocusRegion,
    layout as dock_layout,
};
pub use tree::{Direction, DividerAddress, Node, Orientation, Rect, TileId, Tree};
pub use workspaces::{RESIZE_STEP, Workspace, Workspaces, apply_workspace_action};
