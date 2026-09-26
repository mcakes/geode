//! Shared source-health states for ingestion and diagnostics. A failed load
//! leaves the last good generation live and records its failure reason.
//!
//! Variants are declared in increasing severity, but derived `Ord` also compares
//! reason strings within a variant. Health rollups must compare explicit severity
//! ranks and preserve simultaneous findings; taking `max` over `Health` values
//! would choose one reason by lexical order. The data layer uses its internal
//! `health::severity_rank` for this purpose.
//!
//! This type lives below both the shell and data crates so diagnostics can carry
//! health without a dependency between those crates.

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

    /// Variants sort in severity order. This does not establish a valid rollup:
    /// same-variant values also compare their reason text.
    #[test]
    fn variants_are_declared_in_severity_order() {
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
