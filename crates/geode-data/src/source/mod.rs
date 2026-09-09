//! Sources: configured origins of data (spec §5.1). A source is an adapter
//! plus a list of directory globs, a refresh interval, a readiness
//! strategy, a priority, and a column map.

pub mod discovery;
pub mod sentinel;

// `SourceSpec::from_doc` and `parse_duration` (the `sources.toml` reader)
// now live in geode-core alongside the type they build — see the
// re-export and doc comment in `discovery.rs` (Phase 4c §2.2).
pub use discovery::{Candidate, CandidateState, Priority, Readiness, SourceSpec, discover};
pub use geode_core::source_config::parse_duration;
pub use sentinel::{Sentinel, SentinelError, parse_sentinel};
