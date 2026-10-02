//! Shared health vocabulary and severity ranking.
//!
//! [`Health`] lives in geode-core so data producers and shell diagnostics can
//! share it without depending on each other.
pub use geode_core::health::Health;

/// Rank health by severity only, ignoring reason text — [`Health::severity`].
/// Health aggregation uses this for both source lanes and discovery
/// candidates. The derived `Health::Ord` also compares reasons within a
/// variant, which would make equal-severity selection depend on message
/// spelling. Callers own tie handling, including retaining multiple discovery
/// details.
pub(crate) fn severity_rank(h: &Health) -> u8 {
    h.severity()
}

/// The load-lane key for a source-wide condition that is not one batch's
/// outcome: `<source>:<condition>`. A source's batches are document keys,
/// raw topics, `identity@source` pairs and file batches; a condition keyed
/// here is reported and cleared on its own and never overwrites one of them,
/// nor another condition. The `DataEvent::Health` it produces still carries
/// the plain source name, which is what `Diagnostics` maps to a dataset.
pub fn condition_key(source: &str, condition: &str) -> String {
    format!("{source}:{condition}")
}

/// A subscription's receiver dropping messages (`ingest::subscribe`).
pub const QUEUE: &str = "queue";
/// A source's documents and series queued past `BACKLOG_DEPTH` (`ingest::runner`).
pub const BACKLOG: &str = "backlog";
