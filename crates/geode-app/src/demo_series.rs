//! The demo's fetch adapter (timeseries spec §5.6): a seeded,
//! deterministic, span-independent generator of one-minute bars on
//! weekdays 14:30–21:00 UTC (New York's session, without a calendar) for
//! two dozen identities. Two sources share it under `--demo`: `demo_kdb`
//! offers a catalogue, `demo_rest` does not, so both picker paths are
//! exercised.
//!
//! Span-independence is the property that matters: a request for
//! `[a, b)` returns exactly the bars a wider request would return inside
//! `[a, b)`, so an overlapping refetch appends nothing (spec §4.4 step 2)
//! and the service's coverage subtraction is honest. It comes from
//! seeding per `(seed, identity, day)` and walking each day from a daily
//! level that is itself walked from a fixed epoch — never from "the last
//! bar this process generated".

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use geode_data::adapter::{
    Adapter, AdapterError, Egress, Fetch, FetchRequest, SeriesRows, Subscription,
};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::Arc;

/// `(identity, level, drift per day, daily vol)`, where `level` is the
/// identity's level at the open of [`ANCHOR`] — not at [`EPOCH`]. The
/// walk still starts at `EPOCH`; [`open_level`] simply starts it low
/// enough that the drift has compounded back to `level` by `ANCHOR`.
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

/// A Monday, and the day [`IDENTITIES`]' `level` describes. The walk is
/// anchored here rather than at `EPOCH` because six years of compounding
/// `drift` between the two would otherwise put `SPX.close` near 10,000 on
/// a trader's screen today.
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

/// An approximately normal step: Irwin–Hall over twelve uniforms, mean 0,
/// unit variance. Enough for a demo walk; `rand_distr` is not a dep.
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

/// The level at the open of `day`, walked from `EPOCH` one weekday at a
/// time with a per-(seed, identity) rng, so it depends on nothing but
/// the calendar day.
///
/// The walk starts from `level.ln()` less the drift it will accumulate
/// between `EPOCH` and [`ANCHOR`], so `level` is the identity's level on
/// the ANCHOR day rather than six years before it. Only the drift term is
/// subtracted — the random term's expectation is zero — so the ANCHOR-day
/// open is `level` up to the walk's own noise, which is the point of a
/// random walk and is not corrected for.
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

    /// A characterisation test: these are whatever the walk produced when
    /// it was written. A change here means the generator changed and
    /// every --demo chart looks different — update the values
    /// deliberately, never to make a refactor pass.
    ///
    /// The span pinned here starts on `ANCHOR` itself, the day
    /// `IDENTITIES`' `level` describes, so the band is now a tight one:
    /// `open_level` subtracts the drift accumulated since `EPOCH`, and
    /// the only thing left between `level` (5600.0) and this bar is the
    /// random walk's own noise — about 1,565 weekday steps of
    /// `vol = 0.010`, a standard deviation of `sqrt(1565) * 0.010 ≈ 0.40`
    /// in log space, so 0.5×–2.0× is a couple of sigma either side and
    /// 3.0× is no longer needed to accommodate the drift. What it still
    /// catches: a swapped `drift`/`vol` (used as a per-day drift, 0.010
    /// compounds to `exp(1565 * 0.010) ≈ e^15.65`, many orders of
    /// magnitude off), a dropped `sqrt(BARS_PER_DAY)` divisor on the
    /// intraday step, and now also an anchor that drifts away from
    /// `level` — the defect this replaced, where six years of compounding
    /// put `SPX.close` near 10,000.
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

    /// `to` at exactly midnight excludes `to`'s own day: every bar of a
    /// session starts at 14:30, so that day can contribute nothing, and
    /// visiting it costs a full `open_level` walk. The counts below are
    /// the contract — the day is skipped, not merely emitted empty — and
    /// a `to` inside a session still visits its day.
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
