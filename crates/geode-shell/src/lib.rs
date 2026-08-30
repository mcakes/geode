//! The Geode shell: tiling window management, workspaces, keymap engine,
//! command palette, shared frame state (scope/grouping/as-of), theming.
//! See docs/superpowers/specs/ §2–§4.
//!
//! Dependency rule: this crate never depends on geode-data or on modules.

pub mod actions;
pub mod dataprobe;
pub mod defaults;
pub mod fonts;
pub mod fontsize;
pub mod keymap;
pub mod keymap_edit;
pub mod palette;
pub mod perf;
pub mod reload;
pub mod session;
pub mod shell;
pub mod theme;
pub mod tiling;
pub mod vimfind;
pub mod vimnav;
