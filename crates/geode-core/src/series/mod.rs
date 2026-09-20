//! The timeseries viewer's shared vocabulary (timeseries spec §6.1,
//! §6.4): what a tile asks for and what the query answers. Below both
//! `geode-shell` and `geode-data` for the reason `query.rs` gives — the
//! two may never depend on each other, and the module builds these
//! while the compiler consumes them.

pub mod expr;

use crate::health::Health;
use crate::query::{AsOf, QueryKey};
use chrono::{DateTime, Utc};
use std::time::Instant;

/// The display frequency a series is bucketed to (spec ruling 2: applied
/// by DuckDB on query, never sent to the source).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Frequency {
    M1,
    M5,
    M15,
    H1,
    D1,
    W1,
}

impl Frequency {
    pub const ALL: [Frequency; 6] = [
        Frequency::M1,
        Frequency::M5,
        Frequency::M15,
        Frequency::H1,
        Frequency::D1,
        Frequency::W1,
    ];

    pub fn parse(s: &str) -> Option<Frequency> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::M1 => "1m",
            Frequency::M5 => "5m",
            Frequency::M15 => "15m",
            Frequency::H1 => "1h",
            Frequency::D1 => "1d",
            Frequency::W1 => "1w",
        }
    }

    pub fn seconds(self) -> i64 {
        match self {
            Frequency::M1 => 60,
            Frequency::M5 => 300,
            Frequency::M15 => 900,
            Frequency::H1 => 3_600,
            Frequency::D1 => 86_400,
            Frequency::W1 => 604_800,
        }
    }

    /// The `time_bucket` interval, a literal the compiler formats into
    /// SQL text — the one place a frequency becomes SQL.
    pub fn interval_sql(self) -> &'static str {
        match self {
            Frequency::M1 => "interval '1 minute'",
            Frequency::M5 => "interval '5 minutes'",
            Frequency::M15 => "interval '15 minutes'",
            Frequency::H1 => "interval '1 hour'",
            Frequency::D1 => "interval '1 day'",
            Frequency::W1 => "interval '1 week'",
        }
    }

    /// How many buckets `[from, to)` spans at this frequency, a partial
    /// bucket counting as one. What the cap (spec §6.3) is measured on.
    pub fn buckets_in(self, from: DateTime<Utc>, to: DateTime<Utc>) -> u64 {
        let secs = (to - from).num_seconds();
        if secs <= 0 {
            return 0;
        }
        (secs as u64).div_ceil(self.seconds() as u64)
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|f| *f == self)
            .expect("every frequency is in ALL")
    }

    pub fn next(self) -> Frequency {
        Self::ALL[(self.index() + 1).min(Self::ALL.len() - 1)]
    }

    pub fn prev(self) -> Frequency {
        Self::ALL[self.index().saturating_sub(1)]
    }
}

/// How the rows inside one bucket become one value (spec ruling 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BucketRule {
    #[default]
    Last,
    First,
    Mean,
    Min,
    Max,
}

impl BucketRule {
    pub const ALL: [BucketRule; 5] = [
        BucketRule::Last,
        BucketRule::First,
        BucketRule::Mean,
        BucketRule::Min,
        BucketRule::Max,
    ];

    pub fn parse(s: &str) -> Option<BucketRule> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BucketRule::Last => "last",
            BucketRule::First => "first",
            BucketRule::Mean => "mean",
            BucketRule::Min => "min",
            BucketRule::Max => "max",
        }
    }

    /// The next rule in `ALL`, wrapping — what a tile's `b` key steps.
    pub fn next(self) -> BucketRule {
        let i = Self::ALL.iter().position(|r| *r == self).expect("in ALL");
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// One slot of a request: a source pair bucketed by a rule, or an
/// expression over other slots (spec §7).
#[derive(Debug, Clone, PartialEq)]
pub enum SlotKind {
    Source {
        source: String,
        identity: String,
        rule: BucketRule,
    },
    Expr(expr::Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesSpec {
    pub slot: u8,
    pub kind: SlotKind,
}

/// One tile's whole question (spec §6.1): every slot in one round trip.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub dataset: String,
    /// Half-open; the buckets the points cover.
    pub range: (DateTime<Utc>, DateTime<Utc>),
    /// Half-open, inside `range`; what percentiles and bins are computed over.
    pub window: (DateTime<Utc>, DateTime<Utc>),
    pub as_of: AsOf,
    pub frequency: Frequency,
    pub series: Vec<SeriesSpec>,
    /// Fractions in (0, 1); empty is off.
    pub percentiles: Vec<f64>,
    /// `None` is density off; `Some(n)` with `MIN_BINS..=MAX_BINS`.
    pub bins: Option<u32>,
}

/// The most buckets one request may ask for (spec §6.3).
pub const SERIES_POINT_CAP: u64 = 500_000;
pub const MIN_BINS: u32 = 4;
pub const MAX_BINS: u32 = 200;

fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn describe_span(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let secs = (to - from).num_seconds().max(0);
    if secs >= 365 * 86_400 {
        format!("{}y", secs / (365 * 86_400))
    } else if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else {
        format!("{}h", secs / 3_600)
    }
}

/// The refusal a capped request is answered with (spec §6.3):
/// `1m over 3y is 1,170,000 points; the cap is 500,000`.
pub fn cap_message(
    frequency: Frequency,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    points: u64,
) -> String {
    format!(
        "{} over {} is {} points; the cap is {}",
        frequency.as_str(),
        describe_span(from, to),
        group_thousands(points),
        group_thousands(SERIES_POINT_CAP)
    )
}

/// What a source slot's data is worth (spec §6.4): the coverage hull,
/// the newest fetch, and the load lane's word. `None`s throughout for an
/// expression slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotProvenance {
    pub loaded: Option<(DateTime<Utc>, DateTime<Utc>)>,
    pub latest_received_at: Option<DateTime<Utc>>,
    pub health: Option<Health>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlotResult {
    pub slot: u8,
    /// `buckets.len()` long; `NaN` where this slot has no bucket.
    pub values: Vec<f64>,
    /// `(fraction, value)`, in request order; empty when off or when the
    /// window held nothing.
    pub percentiles: Vec<(f64, f64)>,
    /// `(lo, hi, count)` per bin, ascending; empty when off or when the
    /// window held fewer than two distinct values.
    pub bins: Vec<(f64, f64, u32)>,
    pub provenance: SlotProvenance,
}

/// Struct-of-arrays, the shape a chart wants: no tree, no grouping, no
/// attribution, so not a `Snapshot`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesResult {
    /// Epoch microseconds, ascending: the union of every source slot's buckets.
    pub buckets: Vec<i64>,
    /// In request order.
    pub slots: Vec<SlotResult>,
}

/// One series query's answer, addressed to the key that asked.
#[derive(Debug)]
pub struct SeriesOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    /// `Err` is the failure text; the tile keeps its last good model.
    pub result: Result<SeriesResult, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn frequencies_round_trip_and_step() {
        for f in Frequency::ALL {
            assert_eq!(Frequency::parse(f.as_str()), Some(f));
        }
        assert_eq!(Frequency::parse("2h"), None);
        assert_eq!(Frequency::M1.seconds(), 60);
        assert_eq!(Frequency::W1.seconds(), 604_800);
        assert_eq!(Frequency::D1.interval_sql(), "interval '1 day'");
        assert_eq!(Frequency::M15.interval_sql(), "interval '15 minutes'");
        assert_eq!(Frequency::M1.prev(), Frequency::M1, "saturates");
        assert_eq!(Frequency::W1.next(), Frequency::W1, "saturates");
        assert_eq!(Frequency::M5.next(), Frequency::M15);
        assert_eq!(Frequency::H1.prev(), Frequency::M15);
    }

    #[test]
    fn buckets_in_rounds_up_and_answers_zero_for_an_empty_span() {
        let from = t("2026-01-05T00:00:00Z");
        assert_eq!(
            Frequency::D1.buckets_in(from, t("2026-01-15T00:00:00Z")),
            10
        );
        assert_eq!(
            Frequency::D1.buckets_in(from, t("2026-01-15T00:00:01Z")),
            11,
            "a partial bucket counts"
        );
        assert_eq!(Frequency::M1.buckets_in(from, t("2026-01-05T00:00:00Z")), 0);
        assert_eq!(
            Frequency::M1.buckets_in(t("2026-01-15T00:00:00Z"), from),
            0,
            "to before from is empty"
        );
        // the spec's own example: 1m over 3y
        let three_years = Utc.with_ymd_and_hms(2029, 1, 5, 0, 0, 0).unwrap();
        assert!(Frequency::M1.buckets_in(from, three_years) > SERIES_POINT_CAP);
    }

    #[test]
    fn the_cap_message_names_the_frequency_the_span_and_the_count() {
        let from = t("2026-01-05T00:00:00Z");
        let to = Utc.with_ymd_and_hms(2029, 1, 4, 0, 0, 0).unwrap();
        let n = Frequency::M1.buckets_in(from, to);
        let m = cap_message(Frequency::M1, from, to, n);
        assert!(m.starts_with("1m over 3y is "), "{m}");
        assert!(m.ends_with(" points; the cap is 500,000"), "{m}");
        assert!(m.contains(','), "thousands are grouped: {m}");
        let m = cap_message(Frequency::M1, from, t("2026-01-25T00:00:00Z"), 28_800);
        assert!(m.starts_with("1m over 20d is 28,800 points"), "{m}");
        let m = cap_message(Frequency::M1, from, t("2026-01-05T06:00:00Z"), 360);
        assert!(m.starts_with("1m over 6h is 360 points"), "{m}");
    }

    #[test]
    fn bucket_rules_round_trip_and_cycle() {
        for r in BucketRule::ALL {
            assert_eq!(BucketRule::parse(r.as_str()), Some(r));
        }
        assert_eq!(BucketRule::default(), BucketRule::Last);
        assert_eq!(BucketRule::Max.next(), BucketRule::Last, "cycles");
        assert_eq!(BucketRule::parse("median"), None);
    }
}
