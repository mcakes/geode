//! The query path: scope compilation, the grain-aware view compiler,
//! the read pool, and as-of routing. See `docs/current/data-path.md`.

pub mod scope_sql;

#[cfg(test)]
mod eval_parity;

// Use `Era` to select grain relations and apply their generation filters,
// including in membership probes. This keeps every part of an as-of query
// on the same resolved generations.
//
// Dictionary caching stays internal to compilation. `compile_scope` creates
// its own cache; the view and distinct compilers share one while building
// a statement. Publication rebuilds ENUM types, so caches must not outlive
// that statement.
pub use scope_sql::{Era, ScopeSql, compile_scope};
pub mod compile;

pub use compile::{CompiledColumn, CompiledQuery, compile_view};
pub mod distinct;

pub use distinct::compile_distinct;
pub mod as_of;

pub use as_of::{AsOf, generation_predicate, resolve_generations};
pub mod document;
pub mod pool;

pub use pool::{
    Payload, QueryId, QueryPool, QueryRequest, QueryResult, RequestKind, ResultSink, ViewId, Work,
};
pub mod catalog;

pub use catalog::build_catalog;
pub mod series;

pub use series::{SeriesPlan, Statement, compile_series, run_series};

pub mod read;
pub mod watchlist;

pub use watchlist::WatchlistQuery;
