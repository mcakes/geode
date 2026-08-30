//! DataService: sources, ingestion, DuckDB storage, archive, and the
//! query API. See docs/superpowers/specs/ §5. The only door to data —
//! no other crate opens files or sockets.

pub mod health;
pub mod source;
pub mod store;
