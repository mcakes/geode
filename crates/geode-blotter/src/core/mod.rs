//! Pure blotter models and transformations, tested without a GPUI window.
//! The core resolves columns, tracks expansion and navigation, prepares
//! visible rows and formatted cells, and builds selection summaries and TSV.

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
