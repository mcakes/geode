//! Shared health vocabulary and severity ranking.
//!
//! [`Health`] lives in geode-core so data producers and shell diagnostics can
//! share it without depending on each other.
pub use geode_core::health::Health;

/// Rank health by severity only, ignoring reason text. Health aggregation
/// uses this for both source lanes and discovery candidates. The derived
/// `Health::Ord` also compares reasons within a variant, which would make
/// equal-severity selection depend on message spelling. Callers own tie
/// handling, including retaining multiple discovery details.
pub(crate) fn severity_rank(h: &Health) -> u8 {
    match h {
        Health::Failed { .. } => 4,
        Health::Degraded { .. } => 3,
        Health::PendingTooLong => 2,
        Health::Pending => 1,
        Health::Ok => 0,
    }
}
