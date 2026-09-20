//! The query path (spec §6): scope compilation, the grain-aware view
//! compiler, the read pool, and as-of routing.

pub mod scope_sql;

// `Era` is re-exported deliberately. Every site that names a relation or
// builds a WHERE clause must go through it, and five review rounds found
// defects at sites that had reached for `TableKind::Live` instead — the
// single most repeated defect class in phase 2b. Leaving it reachable only
// via `scope_sql` made the wrong thing the convenient one.
//
// `DictionaryCache` and `compile_scope_cached` are deliberately NOT
// re-exported here: nothing outside `geode-data` compiles several grains
// of one statement, so nothing outside it needs a cache that can outlive
// a single call — and `DictionaryCache`'s own doc warns against holding
// one across statements at all (ingest rebuilds the ENUM types on every
// publish). `compile_scope`'s unchanged public signature is the seam
// every other crate uses; `compile_view` and `compile_distinct` (below)
// reach the cache through `crate::query::scope_sql::` directly.
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
