//! The query path (spec §6): scope compilation, the grain-aware view
//! compiler, the read pool, and as-of routing.

pub mod scope_sql;

pub use scope_sql::{ScopeSql, compile_scope};
pub mod compile;

pub use compile::{CompiledColumn, CompiledQuery, compile_view};
pub mod as_of;

pub use as_of::{AsOf, generation_predicate, resolve_generations};
pub mod pool;

pub use pool::{QueryId, QueryPool, QueryRequest, QueryResult, ViewId};
