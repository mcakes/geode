//! Ingestion: discovery, readiness, the load pipeline, and the priority
//! ladder (spec §5).

pub mod load;
pub mod split;

pub use load::{LoadError, LoadOutcome, LoadRequest, load_file};
pub use split::{Conflict, SplitRequest, SplitResult, split_by_grain};
