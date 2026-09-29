//! The kit tile modules are built from: a tile mechanism two modules would
//! otherwise each write lives here, interaction behavior (keys, focus, open
//! and close, precedence) as well as paint. The shell hosts tiles and never
//! depends on this crate; the menu and popover are the shell's own,
//! re-exported here.

pub mod colour;
pub mod confirm;
pub mod following;
pub mod header;
pub mod motion;
pub mod notice;

/// The menu and popover live in the shell (it paints the row menu over a
/// tile whose module answers `dimension_context` or `press_context`);
/// re-exported so module code keeps naming `geode_tile::menu`.
pub use geode_shell::{menu, popover};
