//! Tiling window-management core (spec §3.1, §3.6): the pure tree that is
//! the single source of truth for workspace layout. Rendering (Phase 1b-ui)
//! consumes [`Tree::layout`]; directional navigation uses the same geometry,
//! so what you see is what hjkl navigates. No gpui here (spec §10.3).

mod tree;

pub use tree::{Direction, Node, Orientation, Rect, TileId, Tree};
