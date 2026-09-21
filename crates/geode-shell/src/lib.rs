//! The Geode shell: tiling window management, workspaces, keymap engine,
//! command palette, shared frame state (scope/grouping/as-of), theming.
//! See docs/superpowers/specs/ §2–§4.
//!
//! Dependency rule: this crate never depends on geode-data or on modules.

pub mod actions;
pub mod choice;
pub mod clock;
pub mod commandline;
pub mod config_write;
pub mod defaults;
pub mod diagnostics;
pub mod dialogmode;
pub mod fonts;
pub mod fontsize;
pub mod footer;
pub mod frame;
pub mod keymap;
pub mod keymap_edit;
pub mod linenumbers;
pub mod listfilter;
pub mod log_persist;
pub mod module;
pub mod palette;
pub mod palette_usage;
pub mod perf;
pub mod reload;
pub mod scopebar;
pub mod series;
pub mod session;
pub mod shell;
pub mod theme;
pub mod tileadd;
pub mod tiling;
pub mod tips;
pub mod vimfind;
pub mod vimnav;
