//! DataService: sources, ingestion, DuckDB storage, archive, and the
//! query API. The current contracts are in `docs/current/data-path.md`.
//! This crate is the application's door to stored data; modules ask through
//! `DataHandle` instead of owning a connection or source transport.

pub mod adapter;
pub mod documents;
pub mod egress;
pub mod handle;
pub mod health;
pub mod ingest;
pub mod pricing;
pub mod query;
pub mod service;
pub mod source;
pub mod store;

pub use egress::{UploadOutcome, UploadParams};
pub use handle::{DataHandle, REQUEST_BOUND, Request};
pub use pricing::{PricerConfig, PricerRegistry};
pub use service::{
    DataEvent, DataService, DataServiceConfig, EventSink, FetchParams, LocalForget, QueryParams,
};

#[cfg(test)]
mod consistency_tests;
