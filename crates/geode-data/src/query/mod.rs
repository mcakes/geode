//! The query path (spec §6): scope compilation, the grain-aware view
//! compiler, the read pool, and as-of routing.

pub mod scope_sql;

pub use scope_sql::{ScopeSql, compile_scope};
pub mod compile;

pub use compile::{CompiledColumn, CompiledQuery, compile_view, depth_for_mask, mask_for_depth};
