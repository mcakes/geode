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
pub mod history_dispatch;
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
pub use handle::{DataHandle, REQUEST_BOUND, Refusal, Request, StopMode};
pub use pricing::{PricerConfig, PricerRegistry};
pub use service::{
    ContextColumns, DEFAULT_STORE_DEADLINE, DataEvent, DataService, DataServiceConfig, EventSink,
    FetchParams, LocalForget, QueryParams, StoreRole,
};
pub use store::stamp::STORE_FORMAT;
pub use vol::{VolConfig, VolModelRegistry};

/// How long a release keeps running the feeds' queued work before it drops
/// the rest: the drain a background collector grants when the app takes the
/// store over.
pub const HANDOFF_DRAIN: std::time::Duration = std::time::Duration::from_secs(2);

/// How far behind its fetch time a history answer is trusted to be
/// complete: coverage recorded from an answer ends this long before the
/// adapter was called, so an observation the vendor stamps before the ask
/// but publishes after it is fetched by the next ask instead of lost
/// behind coverage. A guess at the vendors' publication lag; tune it
/// against the real Hanweck and KDB sources.
pub const HISTORY_SETTLE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[cfg(test)]
mod consistency_tests;
