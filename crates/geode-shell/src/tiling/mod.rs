//! Pure workspace layout, structural focus, navigation, and movement.
//!
//! [`Tree::layout`] supplies tile geometry for rendering and navigation. Each
//! [`Workspace`] combines a main tree with three dock trees and routes verbs
//! through its [`FocusRegion`]. [`dock_layout`] carves visible dock frames;
//! each region's tree then places its visible tiles. This module has no GPUI
//! dependency.

mod dividers;
mod docks;
mod dropzones;
mod tree;
mod workspaces;

pub use dividers::{
    DIVIDER_HIT_WIDTH, DividerStrip, DockEdgeStrip, divider_strips, dock_edge_strips,
    dock_size_from_position,
};
pub use docks::{
    DOCK_DEFAULT_SIZE, DOCK_MAX_SIZE, DOCK_MIN_SIZE, Dock, DockSide, Docks, FocusRegion,
    layout as dock_layout,
};
pub use dropzones::{
    DROP_EDGE_BAND, DropTarget, DropZone, classify_drop_zone, drop_highlight_rect, hit_tile,
    locate_drop_target, rect_contains, resolve_drop_target,
};
pub use tree::{Direction, DividerAddress, Node, Orientation, Rect, TileId, Tree};
pub use workspaces::{RESIZE_STEP, Workspace, WorkspaceIx, Workspaces, apply_workspace_action};
