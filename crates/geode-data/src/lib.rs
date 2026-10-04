//! DataService: sources, ingestion, DuckDB storage, archive, and the
//! query API. The current contracts are in `docs/current/data-path.md`.
//! This crate is the application's door to stored data; modules ask through
//! `DataHandle` instead of owning a connection or source transport.

pub mod adapter;
pub mod documents;
pub mod egress;
pub mod files;
pub mod handle;
pub mod health;
pub mod ingest;
pub mod lease;
pub mod positions;
pub mod pricing;
pub mod query;
pub mod service;
pub mod source;
pub mod store;
pub mod supervise;
pub mod vol;

pub use egress::{UploadOutcome, UploadParams};
pub use handle::{DataHandle, REQUEST_BOUND, Refusal, Request};
pub use pricing::{PricerConfig, PricerRegistry};
pub use service::{
    ContextColumns, DataEvent, DataService, DataServiceConfig, EventSink, FetchParams, LocalForget,
    QueryParams, StoreRole,
};
pub use store::stamp::STORE_FORMAT;
pub use vol::{VolConfig, VolModelRegistry};

#[cfg(test)]
mod consistency_tests;
