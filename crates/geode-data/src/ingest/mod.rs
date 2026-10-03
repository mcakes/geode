//! Ingestion: discovery, readiness, the load pipeline, and the priority
//! ladder. See `docs/current/data-path.md` for the current flow.

pub mod coalesce;
pub mod fetch;
pub mod load;
pub mod plan;
pub(crate) mod recover;
pub mod runner;
pub mod scheduler;
pub mod snapshot;
pub mod split;
pub mod subscribe;

pub use runner::{
    DocumentJob, ForgetJob, IngestEvent, IngestHandle, IngestRunner, IngestSink,
    LOCAL_KEEP_GENERATIONS, ReferenceJob, SeriesJob,
};

pub use load::{LoadError, LoadOutcome, LoadRequest, load_file};
pub use plan::{WorkItem, WorkPlan, build_plan};
pub use split::{Conflict, SplitRequest, SplitResult, split_by_grain};
