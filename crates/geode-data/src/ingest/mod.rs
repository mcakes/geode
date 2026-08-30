//! Ingestion: discovery, readiness, the load pipeline, and the priority
//! ladder (spec §5).

pub mod split;

pub use split::{Conflict, SplitRequest, SplitResult, split_by_grain};
