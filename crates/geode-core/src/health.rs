//! Degradation vocabulary (spec §5.7). Data problems are never modal and
//! never fatal: a failed load leaves live untouched and degrades this
//! file's health. Ord is severity order so a rollup can take the worst.
//!
//! Lives in `geode-core` (Phase 4b Task 4), not `geode-data`, for the
//! same reason `Scope`/`AsOf`/`QueryKey` do (`geode_core::query`'s own
//! module doc): `geode_shell::diagnostics::SourceState` carries a
//! `Health`, and `shell` and `data` may never depend on each other
//! (CLAUDE.md) — so this sits below both. `geode_data::health`
//! re-exports this type under its old path so every existing
//! `geode_data::health::Health` / `crate::health::Health` reference in
//! that crate keeps compiling unchanged.

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    Ok,
    /// A CSV whose sentinel has not landed yet. Expected, not broken.
    Pending,
    /// Pending past the source's configured timeout.
    PendingTooLong,
    /// Loaded, but something was wrong — a required column was missing.
    Degraded {
        reason: String,
    },
    /// The load failed. Last good generation stays live.
    Failed {
        reason: String,
    },
}

impl Health {
    pub fn label(&self) -> &'static str {
        match self {
            Health::Ok => "ok",
            Health::Pending => "pending",
            Health::PendingTooLong => "pending_too_long",
            Health::Degraded { .. } => "degraded",
            Health::Failed { .. } => "failed",
        }
    }

    pub fn to_parts(&self) -> (String, Option<String>) {
        let reason = match self {
            Health::Degraded { reason } | Health::Failed { reason } => Some(reason.clone()),
            _ => None,
        };
        (self.label().to_string(), reason)
    }

    pub fn from_parts(label: &str, reason: Option<&str>) -> Health {
        match label {
            "pending" => Health::Pending,
            "pending_too_long" => Health::PendingTooLong,
            "degraded" => Health::Degraded {
                reason: reason.unwrap_or_default().to_string(),
            },
            "failed" => Health::Failed {
                reason: reason.unwrap_or_default().to_string(),
            },
            _ => Health::Ok,
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Health::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_orders_by_severity_so_rollups_take_the_worst() {
        let mut states = [
            Health::Ok,
            Health::Failed {
                reason: "torn read".into(),
            },
            Health::Pending,
            Health::Degraded {
                reason: "column missing".into(),
            },
        ];
        states.sort();
        assert_eq!(states.first().unwrap().label(), "ok");
        assert_eq!(states.last().unwrap().label(), "failed");
    }

    #[test]
    fn round_trips_through_its_stored_label() {
        for h in [
            Health::Ok,
            Health::Pending,
            Health::PendingTooLong,
            Health::Degraded { reason: "r".into() },
            Health::Failed { reason: "r".into() },
        ] {
            let (label, reason) = h.to_parts();
            assert_eq!(Health::from_parts(&label, reason.as_deref()), h);
        }
    }
}
