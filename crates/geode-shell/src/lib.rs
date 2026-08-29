//! The Geode shell: tiling window management, workspaces, keymap engine,
//! command palette, shared frame state (scope/grouping/as-of), theming.
//! See docs/superpowers/specs/ §2–§4.
//!
//! Dependency rule: this crate never depends on geode-data or on modules.

pub mod actions;
pub mod defaults;
pub mod keymap;
pub mod palette;
pub mod reload;
pub mod session;
pub mod shell;
pub mod theme;
pub mod tiling;
