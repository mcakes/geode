//! Deterministic one-minute bars for the demo fetch sources.
//!
//! Twenty-four identities share a fixed weekday session, 14:30–21:00 UTC,
//! with no holiday calendar or daylight-saving adjustment. `demo_kdb`
//! offers a catalogue; `demo_rest` exercises entry without a catalogue.
//!
//! For a fixed seed and identity, `[a, b)` returns the same bars as the
//! matching slice of a wider request. Each day's intraday walk has its own
//! seed and starts from a daily level walked from a fixed epoch. Fetch order
//! and process lifetime do not affect values, so overlapping fetches agree.
//!
//! Covered spans are cached in the demo database. Changes to identities,
//! the anchor, or the walk can mix old and new values in that cache. Delete
//! `$TMPDIR/geode-demo/<rows>-<seed>/` after changing them or the demo schema.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use geode_data::adapter::{
    Adapter, AdapterError, Egress, Fetch, FetchRequest, SeriesRows, Subscription,
};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::Arc;

/// `(identity, level, drift per weekday, daily volatility)`.
/// `level` is the reference value at `ANCHOR`, subject to accumulated
/// random noise. `open_level` compensates for drift between `EPOCH`
/// and the anchor without removing that noise.
pub const IDENTITIES: [(&str, f64, f64, f64); 24] = [
    ("SPX.close", 5600.0, 0.0003, 0.010),
    ("SPX.vol_1m", 14.0, 0.0, 0.060),
    ("SPX.vol_3m", 15.5, 0.0, 0.045),
    ("SPX.skew_3m", 1.8, 0.0, 0.030),
    ("SX5E.close", 5100.0, 0.0002, 0.011),
    ("SX5E.vol_1m", 15.0, 0.0, 0.060),
    ("SX5E.vol_3m", 16.0, 0.0, 0.045),
    ("NKY.close", 39000.0, 0.0003, 0.013),
    ("NKY.vol_1m", 18.0, 0.0, 0.065),
    ("NDX.close", 20000.0, 0.0004, 0.013),
    ("NDX.vol_1m", 18.5, 0.0, 0.060),
    ("RTY.close", 2200.0, 0.0002, 0.014),
    ("RTY.vol_1m", 20.0, 0.0, 0.060),
    ("VIX", 16.0, 0.0, 0.070),
    ("V2X", 17.0, 0.0, 0.070),
    ("VNKY", 19.0, 0.0, 0.070),
    ("SPX.fwd_1y", 5720.0, 0.0003, 0.010),
    ("SX5E.fwd_1y", 5150.0, 0.0002, 0.011),
    ("SPX.div_1y", 1.4, 0.0, 0.004),
    ("SX5E.div_1y", 3.2, 0.0, 0.004),
    ("SPX.repo_1y", 0.35, 0.0, 0.020),
    ("SX5E.repo_1y", 0.55, 0.0, 0.020),
    ("EURUSD", 1.09, 0.0, 0.005),
    ("USDJPY", 152.0, 0.0, 0.006),
];

/// A Monday, and the day the daily walk starts from.
const EPOCH: NaiveDate = match NaiveDate::from_ymd_opt(2020, 1, 6) {
    Some(d) => d,
    None => unreachable!(),
};

/// Reference date for the levels in [`IDENTITIES`]. Drift compensation
/// keeps those reference levels centered in log space at this date rather
/// than allowing all drift since [`EPOCH`] to compound into them.
const ANCHOR: NaiveDate = match NaiveDate::from_ymd_opt(2026, 1, 5) {
    Some(d) => d,
    None => unreachable!(),
};
const OPEN_MINUTE: i64 = 14 * 60 + 30;
const BARS_PER_DAY: i64 = 390;

fn fnv(seed: u64, identity: &str, day: i64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325 ^ seed;
    for b in identity.bytes().chain(day.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// An approximately normal step: twelve independent uniforms summed and
/// centered to give mean zero and unit variance.
fn step(rng: &mut StdRng) -> f64 {
    (0..12).map(|_| rng.random::<f64>()).sum::<f64>() - 6.0
}

fn is_weekday(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

/// Weekdays in `[from, to)` — exactly the days [`open_level`]'s walk
/// applies `drift` on when it reaches `to`. Whole weeks are counted
/// arithmetically and only the ragged tail is stepped.
fn weekdays_between(from: NaiveDate, to: NaiveDate) -> i64 {
    if to <= from {
        return 0;
    }
    let weeks = (to - from).num_days() / 7;
    let mut count = weeks * 5;
    let mut d = from + Duration::days(weeks * 7);
    while d < to {
        if is_weekday(d) {
            count += 1;
        }
        d += Duration::days(1);
    }
    count
}

/// The opening level for a fixed seed, identity, parameters, and day.
/// The daily walk restarts at [`EPOCH`] and advances once per weekday,
/// so earlier fetches cannot affect the result.
///
/// The initial log level subtracts accumulated drift from the epoch to
/// [`ANCHOR`]. Random daily steps remain, so the anchor's opening level
/// can differ from the identity's reference level.
fn open_level(seed: u64, identity: &str, level: f64, drift: f64, vol: f64, day: NaiveDate) -> f64 {
    let mut rng = StdRng::seed_from_u64(fnv(seed, identity, -1));
    let mut x = level.ln() - drift * weekdays_between(EPOCH, ANCHOR) as f64;
    let mut d = EPOCH;
    while d < day {
        if is_weekday(d) {
            x += drift + vol * step(&mut rng);
        }
        d += Duration::days(1);
    }
    x.exp()
}

/// Bars with timestamps in `[from, to)` for a known identity.
/// Unknown identities return `None`; known identities return an empty result
/// when the span contains no session bars. Dates before `EPOCH` have no bars.
pub fn bars(
    seed: u64,
    identity: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Option<SeriesRows> {
    let &(_, level, drift, vol) = IDENTITIES.iter().find(|(n, ..)| *n == identity)?;
    let mut rows = SeriesRows::default();
    let intraday_vol = vol / (BARS_PER_DAY as f64).sqrt();
    let mut day = from.date_naive();
    // The last day whose session can intersect `[from, to)`. `to`'s own
    // date is excluded when `to` is exactly midnight: that day's bars all
    // start at 14:30, so visiting it would pay a full `open_level` walk
    // (one step per weekday since `EPOCH`) to emit nothing.
    let last = (to - Duration::microseconds(1)).date_naive();
    while day <= last {
        if is_weekday(day) && day >= EPOCH {
            let mut rng = StdRng::seed_from_u64(fnv(seed, identity, day.num_days_from_ce() as i64));
            let mut x = open_level(seed, identity, level, drift, vol, day).ln();
            for k in 0..BARS_PER_DAY {
                x += intraday_vol * step(&mut rng);
                let ts = Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight"))
                    + Duration::minutes(OPEN_MINUTE + k);
                if ts >= from && ts < to {
                    rows.ts.push(ts);
                    rows.value.push(x.exp());
                }
            }
        }
        day += Duration::days(1);
    }
    Some(rows)
}

/// Fetch-only adapter with an optional catalogue of [`IDENTITIES`].
/// Instances with the same seed return identical bars regardless of name or
/// catalogue availability. Fetching an unknown identity returns an adapter error.
pub struct DemoSeries {
    name: &'static str,
    seed: u64,
    catalogue: bool,
}

impl DemoSeries {
    pub fn new(name: &'static str, seed: u64, catalogue: bool) -> Arc<DemoSeries> {
        Arc::new(DemoSeries {
            name,
            seed,
            catalogue,
        })
    }
}

struct DemoFetch {
    seed: u64,
    catalogue: bool,
}

impl Fetch for DemoFetch {
    fn fetch(&mut self, req: &FetchRequest) -> Result<SeriesRows, AdapterError> {
        bars(self.seed, &req.identity, req.from, req.to).ok_or_else(|| AdapterError {
            message: format!("unknown identity '{}'", req.identity),
        })
    }

    fn catalogue(&mut self) -> Option<Vec<String>> {
        self.catalogue
            .then(|| IDENTITIES.iter().map(|(n, ..)| n.to_string()).collect())
    }
}

impl Adapter for DemoSeries {
    fn name(&self) -> &'static str {
        self.name
    }
    fn subscription(&self) -> Option<Box<dyn Subscription>> {
        None
    }
    fn egress(&self) -> Option<Box<dyn Egress>> {
        None
    }
    fn fetch(&self) -> Option<Box<dyn Fetch>> {
        Some(Box::new(DemoFetch {
            seed: self.seed,
            catalogue: self.catalogue,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_data::adapter::{Adapter, FetchRequest};

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Pinned values keep the seeded generator stable across refactors.
    /// Changing them changes demo charts and requires clearing persisted demo
    /// series to avoid mixing incompatible values.
    ///
    /// The anchor-day magnitude check allows accumulated daily noise while
    /// catching parameter mixups, missing intraday volatility scaling, or
    /// missing drift compensation between the epoch and anchor.
    #[test]
    fn the_first_bars_of_spx_for_seed_42_are_pinned() {
        let rows = bars(
            42,
            "SPX.close",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-06T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(rows.ts[0], t("2026-01-05T14:30:00Z"));
        let expected_v0 = 6340.015041033459_f64;
        let expected_v1 = 6342.533639352445_f64;
        assert!(
            (rows.value[0] - expected_v0).abs() < 1e-6,
            "v0 = {}",
            rows.value[0]
        );
        assert!(
            (rows.value[1] - expected_v1).abs() < 1e-6,
            "v1 = {}",
            rows.value[1]
        );
        let level = 5600.0_f64;
        for v in [rows.value[0], rows.value[1]] {
            assert!(
                (0.5 * level..2.0 * level).contains(&v),
                "SPX.close bar {v} is not within a sane multiple of its level {level} — \
                 a magnitude bug (a swapped drift/vol, a dropped sqrt divisor, a walk no \
                 longer anchored on ANCHOR) would fail this; the random walk's own noise \
                 would not"
            );
        }
    }

    /// A midnight end excludes that day's session. Moving the end into the
    /// next session adds its first bar while preserving the half-open interval.
    /// The row-count assertions verify emitted bars, not how many daily walks run.
    #[test]
    fn a_midnight_to_does_not_walk_the_excluded_day() {
        let to_midnight = bars(
            42,
            "SPX.close",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-06T00:00:00Z"),
        )
        .unwrap();
        let before_the_close = bars(
            42,
            "SPX.close",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-05T21:00:00Z"),
        )
        .unwrap();
        assert_eq!(to_midnight.len(), before_the_close.len());
        assert_eq!(to_midnight.len(), 390, "one weekday of one-minute bars");
        let into_the_next_session = bars(
            42,
            "SPX.close",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-06T14:31:00Z"),
        )
        .unwrap();
        assert_eq!(
            into_the_next_session.len(),
            391,
            "the 6th's day is visited: its 14:30 bar is inside the span"
        );
    }

    #[test]
    fn the_same_span_always_yields_the_same_bars() {
        let a = bars(
            42,
            "SPX.close",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-07T00:00:00Z"),
        )
        .unwrap();
        let b = bars(
            42,
            "SPX.close",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-07T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.len(),
            2 * 390,
            "two weekdays of one-minute bars, 14:30–21:00 UTC"
        );
        assert!(a.validate().is_ok());
        assert!(a.ts.first().unwrap() >= &t("2026-01-05T00:00:00Z"));
        assert!(a.ts.last().unwrap() < &t("2026-01-07T00:00:00Z"));
    }

    #[test]
    fn overlapping_spans_agree_on_their_shared_bars() {
        let wide = bars(
            42,
            "SPX.close",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-08T00:00:00Z"),
        )
        .unwrap();
        let narrow = bars(
            42,
            "SPX.close",
            t("2026-01-06T00:00:00Z"),
            t("2026-01-07T00:00:00Z"),
        )
        .unwrap();
        let shared: Vec<(DateTime<Utc>, f64)> = wide
            .ts
            .iter()
            .copied()
            .zip(wide.value.iter().copied())
            .filter(|(ts, _)| *ts >= t("2026-01-06T00:00:00Z") && *ts < t("2026-01-07T00:00:00Z"))
            .collect();
        let narrow_pairs: Vec<(DateTime<Utc>, f64)> = narrow
            .ts
            .iter()
            .copied()
            .zip(narrow.value.iter().copied())
            .collect();
        assert_eq!(
            shared, narrow_pairs,
            "a span-independent generator: what the coverage subtraction relies on"
        );
    }

    #[test]
    fn weekends_have_no_bars_and_a_different_seed_differs() {
        let sat = bars(
            42,
            "SPX.close",
            t("2026-01-10T00:00:00Z"),
            t("2026-01-12T00:00:00Z"),
        )
        .unwrap();
        assert!(sat.is_empty(), "Saturday and Sunday");
        let a = bars(
            42,
            "VIX",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-06T00:00:00Z"),
        )
        .unwrap();
        let b = bars(
            43,
            "VIX",
            t("2026-01-05T00:00:00Z"),
            t("2026-01-06T00:00:00Z"),
        )
        .unwrap();
        assert_ne!(a.value, b.value);
        assert!(
            a.value.iter().all(|v| *v > 0.0),
            "a geometric walk stays positive"
        );
    }

    #[test]
    fn the_two_demo_sources_differ_only_in_the_catalogue() {
        let kdb = DemoSeries::new("demo_kdb", 42, true);
        let rest = DemoSeries::new("demo_rest", 42, false);
        assert_eq!(kdb.name(), "demo_kdb");
        assert!(kdb.subscription().is_none() && kdb.egress().is_none());
        let mut k = kdb.fetch().unwrap();
        let mut r = rest.fetch().unwrap();
        let ids = k.catalogue().unwrap();
        assert_eq!(ids.len(), IDENTITIES.len());
        assert!(ids.contains(&"SPX.close".to_string()) && ids.contains(&"VIX".to_string()));
        assert!(r.catalogue().is_none());
        let req = FetchRequest {
            identity: "SX5E.close".into(),
            from: t("2026-01-05T00:00:00Z"),
            to: t("2026-01-06T00:00:00Z"),
        };
        assert_eq!(k.fetch(&req).unwrap(), r.fetch(&req).unwrap());
        let unknown = FetchRequest {
            identity: "NOPE".into(),
            ..req
        };
        assert_eq!(
            k.fetch(&unknown).unwrap_err().message,
            "unknown identity 'NOPE'"
        );
    }
}
