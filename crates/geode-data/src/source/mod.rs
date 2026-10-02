//! Directory discovery and sentinel parsing. Shared configuration types also
//! describe subscription and fetch sources; their workers live in `ingest`.
//! See `docs/current/data-path.md` for readiness and publication contracts.

pub mod discovery;
pub mod sentinel;

// Re-export the I/O-free configuration reader shared with the shell.
pub use discovery::{
    Candidate, CandidateState, Discovered, PathProblem, Priority, Readiness, SourceSpec, discover,
    discover_all,
};
pub use geode_core::source_config::parse_duration;
pub use sentinel::{Sentinel, SentinelError, parse_sentinel};
