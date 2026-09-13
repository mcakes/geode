//! Ingestion: discovery, readiness, the load pipeline, and the priority
//! ladder (spec §5).

pub mod coalesce;
pub mod load;
pub mod plan;
pub mod runner;
pub mod scheduler;
pub mod split;

pub use runner::{IngestEvent, IngestHandle, IngestRunner, IngestSink};

pub use load::{LoadError, LoadOutcome, LoadRequest, load_file};
pub use plan::{WorkItem, WorkPlan, build_plan};
pub use split::{Conflict, SplitRequest, SplitResult, split_by_grain};
