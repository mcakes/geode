//! Re-export of [`geode_core::health::Health`] (Phase 4b Task 4): the
//! type moved to `geode-core` so `geode-shell`'s `Diagnostics` entity can
//! name it without `geode-shell` depending on `geode-data` (CLAUDE.md:
//! shell and data never depend on each other). Kept under this path so
//! every existing `geode_data::health::Health` / `crate::health::Health`
//! reference in this crate and its callers keeps compiling unchanged.
pub use geode_core::health::Health;

/// The severity ordering `HealthTracker` compares by, and the only one
/// (round 4, NEW-5).
///
/// [`Health`]'s own derived `Ord` must never be used for this. Its
/// variant order IS severity order, but once two values share a variant
/// it falls through to comparing the `reason` STRING — so between a
/// discovery `Degraded { reason: "expected value at line 1" }` and a
/// load `Degraded { reason: "currency varies within instrument key" }`
/// the winner was whichever reason sorted later, and the loser was
/// dropped without ever reaching the surface. That is the same
/// "a real problem is never shown" failure MAJ-3 and NEW-4 were raised
/// for, arriving through the tie-break instead.
///
/// Task 3 (health follow-ups) moved this here from `service.rs`, where
/// `HealthTracker`'s worst-of-two-lanes rollup was its only user, once
/// `geode-data::ingest::scheduler`'s own `worst_health` needed the same
/// rank for its worst-of-candidates rollup — the same "same variant,
/// different reason" hazard, this time between two discovery candidates
/// rather than two health lanes.
pub(crate) fn severity_rank(h: &Health) -> u8 {
    match h {
        Health::Failed { .. } => 4,
        Health::Degraded { .. } => 3,
        Health::PendingTooLong => 2,
        Health::Pending => 1,
        Health::Ok => 0,
    }
}
