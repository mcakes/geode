//! The vol board request: one request per tile, one row per watchlist
//! member, every cell computed from history observations the data layer
//! read. `measure` defines what a cell measures and its arithmetic,
//! `plan` what a request asks of the history family, `reduce` the pure
//! step from observations to rows. `geode-data` lowers the asks to
//! history calls and the read to SQL; the tile never assembles a row.

pub mod measure;
pub mod plan;
pub mod reduce;

pub use measure::{Input, Measure, Role, delta_text};

use crate::query::QueryKey;
use chrono::{DateTime, Utc};
use std::time::{Duration, Instant};

/// How far back a change looks, in business days: one, five, twenty-one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lookback {
    D1,
    W1,
    M1,
}

impl Lookback {
    pub const ALL: [Lookback; 3] = [Lookback::D1, Lookback::W1, Lookback::M1];

    pub fn business_days(self) -> u32 {
        match self {
            Lookback::D1 => 1,
            Lookback::W1 => 5,
            Lookback::M1 => 21,
        }
    }

    /// The calendar span a lookback can reach back over weekends and a
    /// long weekend; a percentile window must hold at least this.
    pub fn calendar_days(self) -> u32 {
        match self {
            Lookback::D1 => 4,
            Lookback::W1 => 8,
            Lookback::M1 => 31,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lookback::D1 => "1d",
            Lookback::W1 => "1w",
            Lookback::M1 => "1m",
        }
    }
}

/// One board ask. `window` is the rank-percentile window (`None`: no
/// percentiles); `sparkline` names one measure and the span of daily
/// values to return for it.
#[derive(Debug, Clone, PartialEq)]
pub struct VolBoardParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub surface: String,
    pub realized: Option<String>,
    pub varswap: Option<String>,
    pub members: Vec<String>,
    pub measures: Vec<Measure>,
    pub lookbacks: Vec<Lookback>,
    pub window: Option<Duration>,
    pub sparkline: Option<(Measure, Duration)>,
}

const DAY: u64 = 86_400;

impl VolBoardParams {
    /// Members trimmed with repeats removed in order, lookbacks with
    /// repeats removed in order: a repeated member would be two rows for
    /// one name and a repeated lookback two identical columns.
    pub fn normalized(mut self) -> Self {
        let mut members: Vec<String> = Vec::with_capacity(self.members.len());
        for m in &self.members {
            let m = m.trim().to_string();
            if !members.contains(&m) {
                members.push(m);
            }
        }
        self.members = members;
        let mut lookbacks = Vec::with_capacity(self.lookbacks.len());
        for lb in &self.lookbacks {
            if !lookbacks.contains(lb) {
                lookbacks.push(*lb);
            }
        }
        self.lookbacks = lookbacks;
        self
    }

    /// Refuses what no dataset could answer. Checked before any history
    /// call: a request that would blank every cell is the caller's error.
    pub fn validate(&self) -> Result<(), String> {
        if self.members.is_empty() {
            return Err("a board names at least one member".to_string());
        }
        if self.members.iter().any(|m| m.trim().is_empty()) {
            return Err("a member name is blank".to_string());
        }
        if self.measures.is_empty() {
            return Err("a board names at least one measure".to_string());
        }
        let asked = self
            .measures
            .iter()
            .chain(self.sparkline.iter().map(|(m, _)| m));
        for measure in asked {
            for role in measure.roles() {
                if self.dataset(role).is_none() {
                    return Err(format!(
                        "{} needs a {} dataset",
                        measure.name(),
                        role.name()
                    ));
                }
            }
        }
        if let Some(window) = self.window {
            if window.is_zero() {
                return Err("a window is longer than zero".to_string());
            }
            let days = window.as_secs() / DAY;
            if let Some(lb) = self
                .lookbacks
                .iter()
                .find(|lb| u64::from(lb.calendar_days()) > days)
            {
                return Err(format!(
                    "a {days}-day window cannot hold a {} lookback ({} days)",
                    lb.label(),
                    lb.calendar_days()
                ));
            }
        }
        if let Some((_, span)) = &self.sparkline
            && span.is_zero()
        {
            return Err("a sparkline span is longer than zero".to_string());
        }
        Ok(())
    }

    pub fn dataset(&self, role: Role) -> Option<&str> {
        match role {
            Role::Surface => Some(self.surface.as_str()),
            Role::Realized => self.realized.as_deref(),
            Role::Varswap => self.varswap.as_deref(),
        }
    }
}

/// One change cell: `value` = current − `from`, where `from` is the last
/// observation at or before the lookback instant, observed at `at`.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub value: f64,
    pub from: f64,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub measure: Measure,
    pub current: Option<f64>,
    /// Parallel to the request's `lookbacks`.
    pub changes: Vec<Option<Change>>,
    /// Rank in `[0, 1]` (the share of the window's daily observations
    /// strictly below the current) and the observation count.
    pub pctl: Option<(f64, usize)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VolRow {
    pub underlying: String,
    /// The latest surface observation among this row's inputs.
    pub as_of: Option<DateTime<Utc>>,
    /// Parallel to the request's `measures`.
    pub cells: Vec<Cell>,
    /// The sparkline measure's daily values, oldest first.
    pub spark: Option<Vec<f64>>,
}

/// One answered board: a row per member in request order (a member the
/// surface lacks keeps its row, `as_of: None` and its surface cells
/// `None`, and is named in `missing`), the datasets that failed and
/// blanked their cells, and notes (`<dataset>: history starts <date>`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VolBoard {
    pub rows: Vec<VolRow>,
    pub missing: Vec<String>,
    pub dataset_errors: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// The board request's answer, addressed to the key that asked. `Err` is
/// the failure text: a surface failure or a refusal.
#[derive(Debug, Clone, PartialEq)]
pub struct VolBoardOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub result: Result<VolBoard, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn params() -> VolBoardParams {
        VolBoardParams {
            key: QueryKey(7),
            tag: 1,
            submitted: Instant::now(),
            surface: "vol_surface".into(),
            realized: Some("realized_vol".into()),
            varswap: Some("varswap".into()),
            members: vec!["SPX".into(), "SX5E".into()],
            measures: vec![Measure::atfm("3M"), Measure::Skew { tenor: "3M".into() }],
            lookbacks: vec![Lookback::D1, Lookback::M1],
            window: Some(Duration::from_secs(365 * 86_400)),
            sparkline: Some((Measure::atfm("3M"), Duration::from_secs(30 * 86_400))),
        }
    }

    #[test]
    fn lookbacks_count_business_days_and_bound_the_window() {
        assert_eq!(Lookback::D1.business_days(), 1);
        assert_eq!(Lookback::W1.business_days(), 5);
        assert_eq!(Lookback::M1.business_days(), 21);
        assert_eq!(Lookback::M1.calendar_days(), 31);
        assert_eq!(Lookback::ALL.map(Lookback::label), ["1d", "1w", "1m"]);
    }

    #[test]
    fn normalized_trims_members_and_drops_repeats_in_order() {
        let mut p = params();
        p.members = vec![" SX5E".into(), "SPX".into(), "SX5E ".into()];
        p.lookbacks = vec![Lookback::M1, Lookback::D1, Lookback::M1];
        let p = p.normalized();
        assert_eq!(p.members, vec!["SX5E", "SPX"]);
        assert_eq!(p.lookbacks, vec![Lookback::M1, Lookback::D1]);
    }

    /// A refusal case: its name, the edit that breaks a good request, and
    /// the refusal text.
    type Case = (&'static str, Box<dyn Fn(&mut VolBoardParams)>, &'static str);

    #[test]
    fn a_well_formed_request_validates_and_each_refusal_names_its_rule() {
        assert_eq!(params().validate(), Ok(()));
        let cases: Vec<Case> = vec![
            (
                "no members",
                Box::new(|p| p.members.clear()),
                "a board names at least one member",
            ),
            (
                "blank member",
                Box::new(|p| p.members.push("  ".into())),
                "a member name is blank",
            ),
            (
                "no measures",
                Box::new(|p| p.measures.clear()),
                "a board names at least one measure",
            ),
            (
                "realized without dataset",
                Box::new(|p| {
                    p.realized = None;
                    p.measures.push(Measure::Realized {
                        window: "5D".into(),
                    });
                }),
                "realized 5D needs a realized dataset",
            ),
            (
                "varswap without dataset",
                Box::new(|p| {
                    p.varswap = None;
                    p.measures.push(Measure::VsSpread { tenor: "3M".into() });
                }),
                "vs spread 3M needs a varswap dataset",
            ),
            (
                "window shorter than M1",
                Box::new(|p| p.window = Some(Duration::from_secs(20 * 86_400))),
                "a 20-day window cannot hold a 1m lookback (31 days)",
            ),
            (
                "zero window",
                Box::new(|p| p.window = Some(Duration::ZERO)),
                "a window is longer than zero",
            ),
            (
                "zero sparkline",
                Box::new(|p| p.sparkline = Some((Measure::atfm("3M"), Duration::ZERO))),
                "a sparkline span is longer than zero",
            ),
            (
                "sparkline needs a missing dataset",
                Box::new(|p| {
                    p.varswap = None;
                    p.sparkline = Some((
                        Measure::Varswap { tenor: "3M".into() },
                        Duration::from_secs(86_400),
                    ));
                }),
                "varswap 3M needs a varswap dataset",
            ),
        ];
        for (name, mutate, message) in cases {
            let mut p = params();
            mutate(&mut p);
            assert_eq!(p.validate(), Err(message.to_string()), "{name}");
        }
    }

    #[test]
    fn a_window_only_has_to_hold_the_lookbacks_asked() {
        let mut p = params();
        p.lookbacks = vec![Lookback::D1];
        p.window = Some(Duration::from_secs(5 * 86_400));
        assert_eq!(p.validate(), Ok(()));
        p.window = None;
        p.lookbacks = vec![Lookback::M1];
        assert_eq!(
            p.validate(),
            Ok(()),
            "no window, no percentile, nothing to hold"
        );
    }

    #[test]
    fn dataset_by_role() {
        let p = params();
        assert_eq!(p.dataset(Role::Surface), Some("vol_surface"));
        assert_eq!(p.dataset(Role::Realized), Some("realized_vol"));
        let mut p = p;
        p.varswap = None;
        assert_eq!(p.dataset(Role::Varswap), None);
    }
}
