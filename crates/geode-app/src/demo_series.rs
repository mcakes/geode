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

/// `(identity, level, drift per day, daily vol)`.
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

/// The level at the open of `day`, walked from `EPOCH` one weekday at a
/// time with a per-(seed, identity) rng, so it depends on nothing but
/// the calendar day.
fn open_level(seed: u64, identity: &str, level: f64, drift: f64, vol: f64, day: NaiveDate) -> f64 {
    let mut rng = StdRng::seed_from_u64(fnv(seed, identity, -1));
    let mut x = level.ln();
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
    let last = to.date_naive();
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
