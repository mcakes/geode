//! The query path (spec §6): scope compilation, the grain-aware view
//! compiler, the read pool, and as-of routing.

pub mod scope_sql;

// `Era` is re-exported deliberately. Every site that names a relation or
// builds a WHERE clause must go through it, and five review rounds found
// defects at sites that had reached for `TableKind::Live` instead — the
// single most repeated defect class in phase 2b. Leaving it reachable only
// via `scope_sql` made the wrong thing the convenient one.
pub use scope_sql::{DictionaryCache, Era, ScopeSql, compile_scope, compile_scope_cached};
pub mod compile;

pub use compile::{CompiledColumn, CompiledQuery, compile_view};
pub mod distinct;

pub use distinct::compile_distinct;
pub mod as_of;

pub use as_of::{AsOf, generation_predicate, resolve_generations};
pub mod pool;

pub use pool::{QueryId, QueryPool, QueryRequest, QueryResult, RequestKind, ResultSink, ViewId};
