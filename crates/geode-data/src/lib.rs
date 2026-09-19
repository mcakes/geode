//! DataService: sources, ingestion, DuckDB storage, archive, and the
//! query API. See docs/superpowers/specs/ §5. The only door to data —
//! no other crate opens files or sockets.

pub mod adapter;
pub mod documents;
pub mod handle;
pub mod health;
pub mod ingest;
pub mod pricing;
pub mod query;
pub mod service;
pub mod source;
pub mod store;

pub use handle::{DataHandle, REQUEST_BOUND, Request};
pub use service::{DataEvent, DataService, DataServiceConfig, EventSink, QueryParams};
