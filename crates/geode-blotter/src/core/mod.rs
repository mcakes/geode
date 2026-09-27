//! The blotter's pure core (Phase 3 §6.1): no `gpui`, tested without a
//! window. Each module lands with the Plan 3c task that needs it.

pub mod cache;
pub mod commands;
pub mod cursor;
pub mod expansion;
pub mod find;
pub mod flatten;
pub mod format;
pub mod launch;
pub mod plan;
pub mod select;
pub mod yank;
