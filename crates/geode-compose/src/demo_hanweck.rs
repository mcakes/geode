//! Deterministic Hanweck/KDB-style history for the demo: constant-maturity
//! implied-vol surface points (`surface`), realized vol (`realized`),
//! variance-swap strikes (`varswap`), the skew-stickiness ratio (`ssr`)
//! and realized spot/vol covariance (`spotvol_cov`) for the demo
//! underlyings.
//!
//! Every value is a pure function of `(seed, key, day)`, so overlapping
//! asks agree and the service's coverage subtraction is sound. Each
//! underlying's vol level walks an AR(1) in log space from [`EPOCH`], one
//! step per weekday, with two shocks a year (the 3rd Monday of March and
//! the 2nd Monday of October). There is no holiday calendar.
//!
//! One close per weekday at 21:00 UTC. The current business day (the
//! adapter's clock on a weekday) also has a point at every five-minute mark
//! from 14:30 up to now, before 21:00: that day's close with its vol nudged
//! by a seeded per-minute draw. Past days have their close only. `Latest`
//! and `At` answer the last observation at or before the instant, so
//! inside today's session the last five-minute mark, and on a past day the
//! last close. `Between` answers every observation in `[from, to)`, the
//! closes and today's marks, so a cached `Between` answers an `At` the way
//! the source would; it ignores `step`. Nothing after the adapter's clock
//! is answered: `At` a later instant is `Latest`, and `Between` stops at
//! now.
//!
//! An unknown table is an error; an unknown underlying, tenor, delta or
//! window is a missing key and answers no rows for it. Changing the model
//! changes values already cached in the demo store: delete
//! `$TMPDIR/geode-demo/<rows>-<seed>/` after changing it.

use crate::demo_series::{fnv, is_weekday, step, weekdays_between};
use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc, Weekday,
};
use geode_core::query::{KeySet, Span};
use geode_core::schema::order::tenor_days;
use geode_data::adapter::{
    Adapter, AdapterError, Egress, HistoryQuery, HistoryRequest, HistoryRows, Subscription,
};
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::collections::HashMap;
use std::sync::Arc;

pub use geode_demo_data::UNDERLYINGS;

/// The adapter name the `[hanweck_*]` demo sources select.
pub const DEMO_HANWECK: &str = "demo_hanweck";
pub const TENORS: [&str; 10] = ["1W", "1M", "2M", "3M", "6M", "1Y", "18M", "2Y", "3Y", "5Y"];
/// Call deltas: 0.95 is the low-strike (downside) wing, 0.05 the upside.
pub const DELTAS: [f64; 7] = [0.05, 0.10, 0.25, 0.50, 0.75, 0.90, 0.95];
pub const WINDOWS: [&str; 3] = ["5D", "20D", "60D"];

/// A Monday, and the first day with a close. Far enough back that an
/// `800d` history window (the demo datasets' `history`, covering the vol
/// watchlist's `2Y` window) is full from the first run.
const EPOCH: NaiveDate = match NaiveDate::from_ymd_opt(2024, 6, 3) {
    Some(d) => d,
    None => unreachable!(),
};
const OPEN: NaiveTime = match NaiveTime::from_hms_opt(14, 30, 0) {
    Some(t) => t,
    None => unreachable!(),
};
const CLOSE: NaiveTime = match NaiveTime::from_hms_opt(21, 0, 0) {
    Some(t) => t,
    None => unreachable!(),
};

/// Mean reversion per weekday of the log vol level, its daily volatility,
/// and the size of each yearly shock in log space.
const REVERSION: f64 = 0.03;
const VOL_OF_VOL: f64 = 0.035;
const SHOCK: f64 = 0.25;

/// The long-run ATM level each underlying's walk reverts to.
fn base(underlying: &str) -> Option<f64> {
    Some(match underlying {
        "SPX" => 14.8,
        "NDX" => 19.6,
        "RTY" => 21.9,
        "SX5E" => 15.9,
        "DAX" => 16.4,
        "UKX" => 12.7,
        "SMI" => 12.1,
        "NKY" => 18.3,
        "HSI" => 23.4,
        "KOSPI2" => 20.2,
        _ => return None,
    })
}

/// Seed-free, so the forward is fixed per underlying.
fn spot(underlying: &str) -> f64 {
    match underlying {
        "SPX" => 7650.0,
        "NDX" => 22000.0,
        other => 1000.0 + (fnv(0, other, -2) % 39_000) as f64,
    }
}

fn years(tenor: &str) -> f64 {
    tenor_days(tenor).unwrap_or(0) as f64 / 365.0
}

/// ATM for `tenor` over the log level `x`: an upward term structure that
/// flattens past about a year.
fn atm(x: f64, tenor: &str) -> f64 {
    x.exp() * (1.0 + 0.18 * (1.0 - (-years(tenor) / 0.9).exp()))
}

/// Skew slope, flattening with term.
fn skew(tenor: &str) -> f64 {
    0.42 / (1.0 + 0.6 * years(tenor))
}

fn is_shock_day(d: NaiveDate) -> bool {
    d.weekday() == Weekday::Mon
        && ((d.month() == 3 && (15..=21).contains(&d.day()))
            || (d.month() == 10 && (8..=14).contains(&d.day())))
}

/// The log vol level of `underlying` on every weekday from [`EPOCH`]
/// through `through`, indexed by weekdays since the epoch. Restarting at
/// the epoch every call is what makes a value independent of what was
/// asked before.
fn walk(seed: u64, underlying: &str, level: f64, through: NaiveDate) -> Vec<f64> {
    let mut rng = StdRng::seed_from_u64(fnv(seed, underlying, -1));
    let mean = level.ln();
    let mut x = mean;
    let mut out = Vec::new();
    let mut d = EPOCH;
    while d <= through {
        if is_weekday(d) {
            if !out.is_empty() {
                x += REVERSION * (mean - x) + VOL_OF_VOL * step(&mut rng);
            }
            if is_shock_day(d) {
                x += SHOCK;
            }
            out.push(x);
        }
        d += Duration::days(1);
    }
    out
}

/// The weekday index of `d` since [`EPOCH`]; `None` on a weekend or
/// before the epoch.
fn weekday_index(d: NaiveDate) -> Option<usize> {
    (is_weekday(d) && d >= EPOCH).then(|| weekdays_between(EPOCH, d) as usize)
}

/// The five tables' key columns, in key order.
fn key_columns(table: &str) -> Option<&'static [&'static str]> {
    match table {
        "surface" => Some(&["underlying_ref", "tenor", "delta"]),
        "realized" => Some(&["underlying_ref", "window"]),
        "varswap" => Some(&["underlying_ref", "tenor"]),
        "ssr" => Some(&["underlying_ref", "tenor"]),
        "spotvol_cov" => Some(&["underlying_ref", "tenor"]),
        _ => None,
    }
}

/// Every value of one key column, in display order; f64 values in the
/// canonical `{}` spelling the store compares.
fn column_values(column: &str) -> Vec<String> {
    match column {
        "underlying_ref" => UNDERLYINGS.iter().map(|s| s.to_string()).collect(),
        "tenor" => TENORS.iter().map(|s| s.to_string()).collect(),
        "delta" => DELTAS.iter().map(|d| format!("{d}")).collect(),
        "window" => WINDOWS.iter().map(|s| s.to_string()).collect(),
        _ => Vec::new(),
    }
}

/// Every full key tuple of `table`, underlying-major.
fn tuples(table: &str) -> Option<Vec<Vec<String>>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for column in key_columns(table)? {
        let values = column_values(column);
        out = out
            .into_iter()
            .flat_map(|prefix| {
                values.iter().map(move |v| {
                    let mut t = prefix.clone();
                    t.push(v.clone());
                    t
                })
            })
            .collect();
    }
    Some(out)
}

/// Whether an asked value names a known one: f64 columns compare by value,
/// so `0.50` names `0.5`.
fn names(column: &str, asked: &str, known: &str) -> bool {
    if column == "delta" {
        matches!(
            (asked.trim().parse::<f64>(), known.parse::<f64>()),
            (Ok(a), Ok(k)) if a == k
        )
    } else {
        asked.trim() == known
    }
}

/// The known tuples `keys` names, each once, in [`tuples`] order; `None`
/// for an unknown table. An asked value naming nothing is a missing key,
/// not an error.
fn asked_tuples(table: &str, keys: &KeySet) -> Option<Vec<Vec<String>>> {
    let columns = key_columns(table)?;
    Some(
        tuples(table)?
            .into_iter()
            .filter(|known| {
                keys.tuples().iter().any(|asked| {
                    let arity_fits = match keys {
                        KeySet::Exact(_) => asked.len() == known.len(),
                        KeySet::Prefix(_) => !asked.is_empty() && asked.len() <= known.len(),
                    };
                    arity_fits
                        && asked
                            .iter()
                            .zip(known)
                            .zip(columns)
                            .all(|((a, k), c)| names(c, a, k))
                })
            })
            .collect(),
    )
}

/// The daily close of `table` at the known full `tuple` on weekday
/// `index`, given its underlying's `walk` through at least `index`.
fn close(seed: u64, table: &str, tuple: &[String], walk: &[f64], index: usize) -> Vec<f64> {
    let x = walk[index];
    match table {
        "surface" => {
            let tenor = tuple[1].as_str();
            let delta: f64 = tuple[2].parse().unwrap_or(0.5);
            let wing = if delta > 0.5 { 2.0 } else { 1.0 };
            let vol = atm(x, tenor) * (1.0 + skew(tenor) * (delta - 0.5) * wing);
            let forward = spot(&tuple[0]) * (0.03 * years(tenor)).exp();
            vec![vol, forward]
        }
        "realized" => {
            // Tracks the 1M implied W weekdays ago for a window `<W>D`,
            // cycling around it.
            let lag = tenor_days(&tuple[1]).unwrap_or(0) as usize;
            let lagged = atm(walk[index.saturating_sub(lag)], "1M");
            let phase = (fnv(seed, &tuple[0], -3) % 6283) as f64 / 1000.0;
            let cycle = 0.92 + 0.12 * (index as f64 / 17.0 + phase).sin();
            vec![(lagged * cycle).max(4.0)]
        }
        "varswap" => {
            let tenor = tuple[1].as_str();
            let a = atm(x, tenor);
            vec![a + 1.4 * skew(tenor) * a / 10.0]
        }
        "ssr" => {
            // Skew-stickiness ratio: how far the ATM moves per unit of spot
            // move in skew terms; around 1.5 in calm markets, rising with
            // the walk's level and flattening with term, clamped to a
            // plausible band.
            let tenor = tuple[1].as_str();
            let a = atm(x, tenor);
            vec![(1.0 + 0.8 * skew(tenor) + 0.02 * (a - 15.0)).clamp(0.5, 3.0)]
        }
        "spotvol_cov" => {
            // Realized spot/vol covariance: negative in calm markets (spot
            // down, vol up), deeper with the walk's level and the skew,
            // nearer zero as the tenor lengthens; clamped to a plausible
            // band in vol points × percent.
            let tenor = tuple[1].as_str();
            let a = atm(x, tenor);
            let fade = 1.0 / (1.0 + years(tenor));
            vec![(-(0.6 + 0.5 * skew(tenor) + 0.03 * (a - 15.0)) * fade).clamp(-3.0, 0.5)]
        }
        _ => unreachable!("key_columns knows only the five tables"),
    }
}

/// The daily close of `table` at the full key `key` on `day`: `None` for
/// an unknown table or key, a weekend, or a day before [`EPOCH`].
pub fn point(seed: u64, table: &str, key: &[&str], day: NaiveDate) -> Option<Vec<f64>> {
    let asked = KeySet::Exact(vec![key.iter().map(|s| s.to_string()).collect()]);
    let tuple = asked_tuples(table, &asked)?.into_iter().next()?;
    let index = weekday_index(day)?;
    let walk = walk(seed, &tuple[0], base(&tuple[0])?, day);
    Some(close(seed, table, &tuple, &walk, index))
}

/// One observation: a weekday, its instant, and whether it is an intraday
/// point (that day's close nudged) rather than the close itself.
#[derive(Clone, Copy)]
struct Observation {
    day: NaiveDate,
    ts: DateTime<Utc>,
    intraday: bool,
}

fn on(day: NaiveDate, time: NaiveTime) -> DateTime<Utc> {
    Utc.from_utc_datetime(&day.and_time(time))
}

/// The one day with intraday points: the date of `now` when it is a
/// weekday from [`EPOCH`] on. Confining them to today keeps a `Between`
/// bounded (at most 78 marks per tuple) while it still holds every point an
/// `At` can answer.
fn session_day(now: DateTime<Utc>) -> Option<NaiveDate> {
    let day = now.date_naive();
    (is_weekday(day) && day >= EPOCH).then_some(day)
}

/// What `At(t)` answers for `t <= now`: the last observation at or before
/// `t`, so inside today's session the point at the last five-minute mark,
/// otherwise the last close. `None` before the first close.
fn observation_at(t: DateTime<Utc>, now: DateTime<Utc>) -> Option<Observation> {
    let mut day = t.date_naive();
    let time = t.time();
    if is_weekday(day) && day >= EPOCH {
        if time >= CLOSE {
            return Some(Observation {
                day,
                ts: on(day, CLOSE),
                intraday: false,
            });
        }
        if time >= OPEN && session_day(now) == Some(day) {
            let mark = NaiveTime::from_hms_opt(time.hour(), time.minute() / 5 * 5, 0)?;
            return Some(Observation {
                day,
                ts: on(day, mark),
                intraday: true,
            });
        }
    }
    loop {
        day = day.pred_opt()?;
        if day < EPOCH {
            return None;
        }
        if is_weekday(day) {
            return Some(Observation {
                day,
                ts: on(day, CLOSE),
                intraday: false,
            });
        }
    }
}

/// Today's five-minute marks with `from <= ts < to` and `ts <= now`.
fn marks_between(from: DateTime<Utc>, to: DateTime<Utc>, now: DateTime<Utc>) -> Vec<Observation> {
    let Some(day) = session_day(now) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut ts = on(day, OPEN);
    while ts < on(day, CLOSE) && ts < to && ts <= now {
        if ts >= from {
            out.push(Observation {
                day,
                ts,
                intraday: true,
            });
        }
        ts += Duration::minutes(5);
    }
    out
}

/// The daily closes with `from <= ts < to`.
fn closes_between(from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Observation> {
    let mut out = Vec::new();
    let mut day = from.date_naive().max(EPOCH);
    while on(day, CLOSE) < to {
        let ts = on(day, CLOSE);
        if is_weekday(day) && ts >= from {
            out.push(Observation {
                day,
                ts,
                intraday: false,
            });
        }
        day += Duration::days(1);
    }
    out
}

/// The demo Hanweck history service behind the `[hanweck_*]` sources.
/// Instances with the same seed answer identically; `now` decides what
/// `Latest` answers.
pub struct DemoHanweck {
    seed: u64,
    now: fn() -> DateTime<Utc>,
}

impl DemoHanweck {
    pub fn new(seed: u64) -> Arc<DemoHanweck> {
        Arc::new(DemoHanweck {
            seed,
            now: Utc::now,
        })
    }
}

struct DemoHistory {
    seed: u64,
    now: fn() -> DateTime<Utc>,
}

impl HistoryQuery for DemoHistory {
    fn query(&mut self, req: &HistoryRequest) -> Result<HistoryRows, AdapterError> {
        let unknown = || AdapterError {
            message: format!("unknown table '{}'", req.table),
        };
        let columns = key_columns(&req.table).ok_or_else(unknown)?;
        let wanted = asked_tuples(&req.table, &req.keys).ok_or_else(unknown)?;
        // Nothing after `now` has been observed yet: a source that answered
        // tomorrow's close would be fiction, and an unbounded `to` would
        // walk the calendar to its end.
        let now = (self.now)();
        let observations: Vec<Observation> = match req.span {
            Span::Latest => observation_at(now, now).into_iter().collect(),
            Span::At(t) => observation_at(t.min(now), now).into_iter().collect(),
            Span::Between { from, to, .. } => {
                let through_now = now + Duration::microseconds(1);
                let mut all = closes_between(from, to.min(through_now));
                all.extend(marks_between(from, to, now));
                all.sort_by_key(|o| o.ts);
                all
            }
        };
        let value_arity = if req.table == "surface" { 2 } else { 1 };
        let mut rows = HistoryRows {
            ts: Vec::new(),
            keys: vec![Vec::new(); columns.len()],
            values: vec![Vec::new(); value_arity],
        };
        let Some(last) = observations.iter().map(|o| o.day).max() else {
            return Ok(rows);
        };
        // One walk per underlying per call: an 800-day prefix ask over the
        // surface is 40 tuples sharing it.
        let mut walks: HashMap<&str, Vec<f64>> = HashMap::new();
        for tuple in &wanted {
            let underlying = tuple[0].as_str();
            let Some(level) = base(underlying) else {
                continue;
            };
            let levels = walks
                .entry(underlying)
                .or_insert_with(|| walk(self.seed, underlying, level, last));
            for o in &observations {
                let Some(index) = weekday_index(o.day) else {
                    continue;
                };
                let mut values = close(self.seed, &req.table, tuple, levels, index);
                if o.intraday {
                    let identity = format!("{}/{}", req.table, tuple.join("/"));
                    let minute = o.ts.timestamp() / 60;
                    let mut rng = StdRng::seed_from_u64(fnv(self.seed, &identity, minute));
                    // The vol moves intraday; a forward beside it stays put.
                    values[0] *= (0.004 * step(&mut rng)).exp();
                }
                rows.ts.push(o.ts);
                for (c, value) in tuple.iter().enumerate() {
                    rows.keys[c].push(value.clone());
                }
                for (c, value) in values.into_iter().enumerate() {
                    rows.values[c].push(value);
                }
            }
        }
        Ok(rows)
    }

    fn keys(&mut self, table: &str, column: &str) -> Option<Vec<String>> {
        key_columns(table)?
            .contains(&column)
            .then(|| column_values(column))
    }
}

impl Adapter for DemoHanweck {
    fn name(&self) -> &'static str {
        DEMO_HANWECK
    }
    fn subscription(&self) -> Option<Box<dyn Subscription>> {
        None
    }
    fn egress(&self) -> Option<Box<dyn Egress>> {
        None
    }
    fn history(&self) -> Option<Box<dyn HistoryQuery>> {
        Some(Box::new(DemoHistory {
            seed: self.seed,
            now: self.now,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, NaiveDate, Utc};
    use geode_core::query::{KeySet, Span};
    use geode_data::adapter::{Adapter, HistoryRequest, HistoryRows};

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn keys(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter()
            .map(|t| t.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    fn ask(table: &str, keys: KeySet, span: Span) -> HistoryRequest {
        HistoryRequest {
            table: table.to_string(),
            keys,
            span,
        }
    }

    fn between(from: &str, to: &str) -> Span {
        Span::Between {
            from: t(from),
            to: t(to),
            step: None,
        }
    }

    /// Tuesday 2026-01-13, after the close.
    fn tuesday_evening() -> DateTime<Utc> {
        t("2026-01-13T22:00:00Z")
    }

    /// Tuesday 2026-01-13, inside the session, off a five-minute mark.
    fn tuesday_session() -> DateTime<Utc> {
        t("2026-01-13T15:07:00Z")
    }

    fn query(now: fn() -> DateTime<Utc>, req: &HistoryRequest) -> HistoryRows {
        let adapter = DemoHanweck { seed: 42, now };
        adapter.history().unwrap().query(req).unwrap()
    }

    fn spx_3m_atm() -> KeySet {
        KeySet::Exact(keys(&[&["SPX", "3M", "0.5"]]))
    }

    /// Pinned values keep the seeded model stable across refactors.
    /// Changing them changes the demo's history and requires deleting the
    /// persisted demo store to avoid mixing incompatible values.
    #[test]
    fn the_first_spx_3m_atm_point_for_seed_42_is_pinned() {
        let v = point(42, "surface", &["SPX", "3M", "0.5"], day(2026, 1, 5)).unwrap();
        let expected_vol = 17.792975456681315_f64;
        let expected_forward = 7706.79886007149_f64;
        assert!((v[0] - expected_vol).abs() < 1e-9, "vol = {}", v[0]);
        assert!((v[1] - expected_forward).abs() < 1e-9, "forward = {}", v[1]);
        assert!(
            (10.0..30.0).contains(&v[0]),
            "vol {} is a vol, not a spot",
            v[0]
        );
        assert!(
            (7000.0..8500.0).contains(&v[1]),
            "forward {} is near the SPX spot",
            v[1]
        );
        let other = point(43, "surface", &["SPX", "3M", "0.5"], day(2026, 1, 5)).unwrap();
        assert_ne!(v[0], other[0], "the seed drives the walk");
        assert_eq!(v[1], other[1], "the forward is the seed-free spot carried");
    }

    #[test]
    fn the_put_wing_is_above_atm_and_the_call_wing_below_and_the_term_rises() {
        let d = day(2026, 1, 5);
        let at =
            |delta: &str, tenor: &str| point(42, "surface", &["SPX", tenor, delta], d).unwrap()[0];
        assert!(at("0.9", "3M") > at("0.5", "3M") && at("0.5", "3M") > at("0.1", "3M"));
        assert!(at("0.5", "2Y") > at("0.5", "1M"));
        assert!(
            at("0.9", "3M") - at("0.5", "3M") > at("0.5", "3M") - at("0.1", "3M"),
            "the put wing is steeper"
        );
        let atm: Vec<f64> = TENORS.iter().map(|tenor| at("0.5", tenor)).collect();
        assert!(
            atm.windows(2).all(|w| w[0] < w[1]),
            "ATM rises along the term: {atm:?}"
        );
    }

    #[test]
    fn the_surface_ladders_1w_to_5y_at_seven_deltas_and_ssr_and_spotvol_cov_are_tables() {
        assert_eq!(TENORS.first(), Some(&"1W"));
        assert_eq!(TENORS.last(), Some(&"5Y"));
        assert_eq!(DELTAS.len(), 7);
        assert_eq!(DELTAS[0], 0.05);
        assert_eq!(DELTAS[6], 0.95);
        let day = day(2026, 1, 13);
        let wing_lo = point(42, "surface", &["SPX", "3M", "0.95"], day).unwrap()[0];
        let put = point(42, "surface", &["SPX", "3M", "0.75"], day).unwrap()[0];
        let atm = point(42, "surface", &["SPX", "3M", "0.5"], day).unwrap()[0];
        let call = point(42, "surface", &["SPX", "3M", "0.25"], day).unwrap()[0];
        let wing_hi = point(42, "surface", &["SPX", "3M", "0.05"], day).unwrap()[0];
        assert!(wing_lo > put && put > atm && atm > call && call > wing_hi);
        let week = point(42, "surface", &["SPX", "1W", "0.5"], day).unwrap()[0];
        let five = point(42, "surface", &["SPX", "5Y", "0.5"], day).unwrap()[0];
        assert!(week < atm && atm < five, "the term rises end to end");
        let ssr = point(42, "ssr", &["SPX", "3M"], day).unwrap();
        assert_eq!(ssr.len(), 1);
        assert!((0.5..=3.0).contains(&ssr[0]), "{ssr:?}");
        assert!(point(42, "ssr", &["SPX", "3M", "0.5"], day).is_none());
        let mut h = DemoHanweck::new(42).history().unwrap();
        assert_eq!(h.keys("ssr", "tenor"), Some(strings(&TENORS)));
        assert!(h.keys("ssr", "delta").is_none());
        let cov = point(42, "spotvol_cov", &["SPX", "3M"], day).unwrap();
        assert_eq!(cov.len(), 1);
        assert!((-3.0..=0.5).contains(&cov[0]), "{cov:?}");
        let long = point(42, "spotvol_cov", &["SPX", "5Y"], day).unwrap()[0];
        assert!(
            long.abs() < cov[0].abs(),
            "nearer zero as the tenor lengthens: {long} vs {cov:?}"
        );
        assert!(point(42, "spotvol_cov", &["SPX", "3M", "0.5"], day).is_none());
        assert_eq!(h.keys("spotvol_cov", "tenor"), Some(strings(&TENORS)));
        assert!(h.keys("spotvol_cov", "delta").is_none());
    }

    #[test]
    fn a_weekend_has_no_point_and_overlapping_between_asks_agree() {
        assert!(point(42, "surface", &["SPX", "3M", "0.5"], day(2026, 1, 10)).is_none());
        let weekend = query(
            tuesday_evening,
            &ask(
                "surface",
                spx_3m_atm(),
                between("2026-01-10T00:00:00Z", "2026-01-12T00:00:00Z"),
            ),
        );
        assert!(weekend.is_empty(), "{weekend:?}");

        let wide_ask = ask(
            "surface",
            spx_3m_atm(),
            between("2026-01-01T00:00:00Z", "2026-01-20T00:00:00Z"),
        );
        let wide = query(tuesday_evening, &wide_ask);
        wide.validate_against(3, 2, &wide_ask.keys).unwrap();
        let narrow = query(
            tuesday_evening,
            &ask(
                "surface",
                spx_3m_atm(),
                between("2026-01-06T00:00:00Z", "2026-01-13T00:00:00Z"),
            ),
        );
        assert_eq!(narrow.len(), 5, "Tue to the next Mon: five closes");
        let shared: Vec<usize> = (0..wide.len())
            .filter(|&i| {
                wide.ts[i] >= t("2026-01-06T00:00:00Z") && wide.ts[i] < t("2026-01-13T00:00:00Z")
            })
            .collect();
        assert_eq!(
            shared.iter().map(|&i| wide.ts[i]).collect::<Vec<_>>(),
            narrow.ts
        );
        for (j, &i) in shared.iter().enumerate() {
            assert_eq!(wide.values[0][i], narrow.values[0][j]);
            assert_eq!(wide.values[1][i], narrow.values[1][j]);
        }
        for (i, ts) in narrow.ts.iter().enumerate() {
            let close = point(42, "surface", &["SPX", "3M", "0.5"], ts.date_naive()).unwrap();
            assert_eq!(ts.format("%H:%M").to_string(), "21:00");
            assert_eq!(vec![narrow.values[0][i], narrow.values[1][i]], close);
        }

        // Half-open at both ends: a close exactly at `from` is in, one
        // exactly at `to` is out.
        let edges = query(
            tuesday_evening,
            &ask(
                "surface",
                spx_3m_atm(),
                between("2026-01-06T21:00:00Z", "2026-01-08T21:00:00Z"),
            ),
        );
        assert_eq!(
            edges.ts,
            [t("2026-01-06T21:00:00Z"), t("2026-01-07T21:00:00Z")]
        );

        // Nothing after now: Tuesday's close is in after it happens, the
        // rest of the week is not, and inside the session not even Tuesday's
        // close, only its five-minute marks so far. Monday, a past day, has
        // its close alone.
        let rest_of_week = ask(
            "surface",
            spx_3m_atm(),
            between("2026-01-12T00:00:00Z", "2026-01-20T00:00:00Z"),
        );
        let marks = |through: &str| {
            let mut out = Vec::new();
            let mut m = t("2026-01-13T14:30:00Z");
            while m <= t(through) {
                out.push(m);
                m += Duration::minutes(5);
            }
            out
        };
        let mut evening = vec![t("2026-01-12T21:00:00Z")];
        evening.extend(marks("2026-01-13T20:55:00Z"));
        evening.push(t("2026-01-13T21:00:00Z"));
        assert_eq!(evening.len(), 2 + 78);
        assert_eq!(query(tuesday_evening, &rest_of_week).ts, evening);
        let mut session = vec![t("2026-01-12T21:00:00Z")];
        session.extend(marks("2026-01-13T15:05:00Z"));
        assert_eq!(query(tuesday_session, &rest_of_week).ts, session);

        // A step is a hint this simulator ignores: one point per day.
        let stepped = query(
            tuesday_evening,
            &ask(
                "surface",
                spx_3m_atm(),
                Span::Between {
                    from: t("2026-01-06T00:00:00Z"),
                    to: t("2026-01-13T00:00:00Z"),
                    step: Some(std::time::Duration::from_secs(3600)),
                },
            ),
        );
        assert_eq!(stepped, narrow);

        // A prefix answers every tuple under it, agreeing with the exact ask.
        let prefix = query(
            tuesday_evening,
            &ask(
                "surface",
                KeySet::Prefix(keys(&[&["SPX"]])),
                between("2026-01-06T00:00:00Z", "2026-01-13T00:00:00Z"),
            ),
        );
        assert_eq!(prefix.len(), TENORS.len() * DELTAS.len() * 5);
        let atm_rows: Vec<usize> = (0..prefix.len())
            .filter(|&i| prefix.key_at(i) == ["SPX", "3M", "0.5"])
            .collect();
        assert_eq!(
            atm_rows.iter().map(|&i| prefix.ts[i]).collect::<Vec<_>>(),
            narrow.ts
        );
        assert_eq!(
            atm_rows
                .iter()
                .map(|&i| prefix.values[0][i])
                .collect::<Vec<_>>(),
            narrow.values[0]
        );

        // Nothing before the epoch.
        let early = query(
            tuesday_evening,
            &ask(
                "surface",
                spx_3m_atm(),
                between("2024-05-01T00:00:00Z", "2024-06-04T00:00:00Z"),
            ),
        );
        assert_eq!(early.ts, [t("2024-06-03T21:00:00Z")]);
    }

    #[test]
    fn latest_and_at_answer_the_last_business_close_or_an_intraday_point() {
        let at = |s: &str| {
            query(
                tuesday_evening,
                &ask("surface", spx_3m_atm(), Span::At(t(s))),
            )
        };
        let friday = point(42, "surface", &["SPX", "3M", "0.5"], day(2026, 1, 9)).unwrap();
        let sunday = at("2026-01-11T12:00:00Z");
        assert_eq!(sunday.ts, [t("2026-01-09T21:00:00Z")]);
        assert_eq!(vec![sunday.values[0][0], sunday.values[1][0]], friday);

        let monday = point(42, "surface", &["SPX", "3M", "0.5"], day(2026, 1, 12)).unwrap();
        let before_open = at("2026-01-13T09:00:00Z");
        assert_eq!(before_open.ts, [t("2026-01-12T21:00:00Z")]);
        assert_eq!(before_open.values[0][0], monday[0]);

        let tuesday = point(42, "surface", &["SPX", "3M", "0.5"], day(2026, 1, 13)).unwrap();
        let session = at("2026-01-13T15:00:00Z");
        assert_eq!(session.ts, [t("2026-01-13T15:00:00Z")]);
        let v = session.values[0][0];
        assert!(
            (v / tuesday[0] - 1.0).abs() < 0.02,
            "{v} vs close {}",
            tuesday[0]
        );
        assert_ne!(v, tuesday[0], "an intraday point moves off the close");
        assert_eq!(
            at("2026-01-13T15:00:00Z"),
            session,
            "deterministic per minute"
        );
        assert_eq!(at("2026-01-13T15:04:59Z").ts, [t("2026-01-13T15:00:00Z")]);
        assert_eq!(at("2026-01-13T21:00:00Z").ts, [t("2026-01-13T21:00:00Z")]);
        assert_eq!(at("2026-01-13T21:00:00Z").values[0][0], tuesday[0]);
        assert!(
            at("2024-06-03T14:00:00Z").is_empty(),
            "before the first session"
        );
        assert_eq!(
            at("2026-01-20T12:00:00Z").ts,
            [t("2026-01-13T21:00:00Z")],
            "an instant after now answers what is known now"
        );

        // A past day has its close only: inside its session, `At` answers
        // the close before it, and after its close that close.
        let wednesday = || t("2026-01-14T22:00:00Z");
        let past = |s: &str| query(wednesday, &ask("surface", spx_3m_atm(), Span::At(t(s))));
        assert_eq!(past("2026-01-13T15:00:00Z").ts, [t("2026-01-12T21:00:00Z")]);
        assert_eq!(past("2026-01-13T15:00:00Z").values[0][0], monday[0]);
        assert_eq!(past("2026-01-13T22:00:00Z").ts, [t("2026-01-13T21:00:00Z")]);
        assert_eq!(past("2026-01-13T22:00:00Z").values[0][0], tuesday[0]);

        let latest =
            |now: fn() -> DateTime<Utc>| query(now, &ask("surface", spx_3m_atm(), Span::Latest));
        assert_eq!(latest(tuesday_evening).ts, [t("2026-01-13T21:00:00Z")]);
        assert_eq!(latest(tuesday_evening).values[0][0], tuesday[0]);
        assert_eq!(latest(tuesday_session).ts, [t("2026-01-13T15:05:00Z")]);
    }

    #[test]
    fn a_between_over_today_holds_the_point_at_and_latest_answer() {
        let close = point(42, "surface", &["SPX", "3M", "0.5"], day(2026, 1, 13)).unwrap();
        let instant = t("2026-01-13T15:03:00Z");
        let at = query(
            tuesday_session,
            &ask("surface", spx_3m_atm(), Span::At(instant)),
        );
        assert_eq!(at.ts, [t("2026-01-13T15:00:00Z")]);
        assert_eq!(at.values[1][0], close[1], "the forward is not nudged");
        let span = Span::Between {
            from: instant - Duration::days(1),
            to: tuesday_session(),
            step: None,
        };
        let cached = query(tuesday_session, &ask("surface", spx_3m_atm(), span));
        let i = cached
            .ts
            .iter()
            .position(|ts| *ts == at.ts[0])
            .expect("the Between holds the point At answers");
        assert_eq!(
            (cached.values[0][i], cached.values[1][i]),
            (at.values[0][0], at.values[1][0])
        );
        // What a cache holding this Between answers for At: the last row at
        // or before the instant, which must be the source's own answer.
        let last_before = (0..cached.len())
            .filter(|&j| cached.ts[j] <= instant)
            .max_by_key(|&j| cached.ts[j])
            .unwrap();
        assert_eq!(cached.ts[last_before], at.ts[0]);
        let latest = query(tuesday_session, &ask("surface", spx_3m_atm(), Span::Latest));
        assert_eq!(cached.ts.last(), latest.ts.first());
        assert_eq!(cached.values[0].last(), latest.values[0].first());
    }

    #[test]
    fn keys_enumerate_the_constants_and_unknown_tables_and_symbols_behave() {
        let adapter = DemoHanweck::new(42);
        assert_eq!(adapter.name(), DEMO_HANWECK);
        assert!(adapter.subscription().is_none() && adapter.egress().is_none());
        let mut h = adapter.history().unwrap();
        assert_eq!(h.keys("surface", "tenor"), Some(strings(&TENORS)));
        assert_eq!(h.keys("varswap", "tenor"), Some(strings(&TENORS)));
        assert_eq!(
            h.keys("surface", "delta"),
            Some(strings(&[
                "0.05", "0.1", "0.25", "0.5", "0.75", "0.9", "0.95"
            ]))
        );
        assert_eq!(h.keys("realized", "window"), Some(strings(&WINDOWS)));
        for table in ["surface", "realized", "varswap", "ssr", "spotvol_cov"] {
            assert_eq!(h.keys(table, "underlying_ref"), Some(strings(&UNDERLYINGS)));
        }
        assert_eq!(h.keys("surface", "vol"), None);
        assert_eq!(h.keys("realized", "tenor"), None);
        assert_eq!(h.keys("nope", "tenor"), None);

        let err = h
            .query(&ask("nope", spx_3m_atm(), Span::Latest))
            .unwrap_err();
        assert_eq!(err.message, "unknown table 'nope'");
        let missing = h
            .query(&ask(
                "surface",
                KeySet::Prefix(keys(&[&["NOPE"]])),
                Span::Latest,
            ))
            .unwrap();
        assert!(missing.is_empty());
        let off_grid = h
            .query(&ask(
                "surface",
                KeySet::Exact(keys(&[&["SPX", "3M", "0.33"]])),
                Span::Latest,
            ))
            .unwrap();
        assert!(off_grid.is_empty());

        let pair = keys(&[&["NKY", "1Y", "0.1"], &["SPX", "3M", "0.5"]]);
        let exact = query(
            tuesday_evening,
            &ask("surface", KeySet::Exact(pair.clone()), Span::Latest),
        );
        let mut answered: Vec<Vec<String>> = (0..exact.len()).map(|i| exact.key_at(i)).collect();
        answered.sort();
        assert_eq!(answered, pair, "an exact ask answers those tuples only");

        let two_columns = query(
            tuesday_evening,
            &ask(
                "surface",
                KeySet::Prefix(keys(&[&["SPX", "3M"]])),
                Span::Latest,
            ),
        );
        assert_eq!(two_columns.len(), DELTAS.len());
    }

    #[test]
    fn realized_and_varswap_tables_have_the_declared_shapes() {
        let spx = || KeySet::Prefix(keys(&[&["SPX"]]));
        let realized = query(tuesday_evening, &ask("realized", spx(), Span::Latest));
        realized.validate_against(2, 1, &spx()).unwrap();
        assert_eq!(realized.len(), WINDOWS.len());
        assert_eq!(realized.keys[1], WINDOWS.to_vec());
        assert!(realized.values[0].iter().all(|v| *v >= 4.0));

        let d = day(2026, 1, 13);
        let varswap = query(tuesday_evening, &ask("varswap", spx(), Span::Latest));
        varswap.validate_against(2, 1, &spx()).unwrap();
        assert_eq!(varswap.len(), TENORS.len());
        for i in 0..varswap.len() {
            let tenor = varswap.keys[1][i].as_str();
            let atm = point(42, "surface", &["SPX", tenor, "0.5"], d).unwrap()[0];
            assert!(varswap.values[0][i] > atm, "{tenor}: strike above ATM");
        }

        // Realized vol tracks the 1M ATM implied, lagged by its window.
        let lagged = point(42, "surface", &["SPX", "1M", "0.5"], day(2025, 12, 16)).unwrap()[0];
        let rv = point(42, "realized", &["SPX", "20D"], d).unwrap()[0];
        assert!(
            (0.80..1.04).contains(&(rv / lagged)),
            "rv {rv} vs lagged ATM {lagged}"
        );
    }
}
