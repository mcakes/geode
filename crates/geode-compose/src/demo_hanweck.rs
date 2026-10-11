//! Deterministic Hanweck/KDB-style history for the demo: constant-maturity
//! implied-vol surface points (`surface`), realized vol (`realized`) and
//! variance-swap strikes (`varswap`) for the demo underlyings.
//!
//! Every value is a pure function of `(seed, key, day)`, so overlapping
//! asks agree and the service's coverage subtraction is sound. Each
//! underlying's vol level walks an AR(1) in log space from [`EPOCH`], one
//! step per weekday, with two shocks a year (the 3rd Monday of March and
//! the 2nd Monday of October). There is no holiday calendar.
//!
//! One close per weekday at 21:00 UTC. `Latest` and `At` answer the last
//! close at or before the instant, except inside a weekday session
//! (14:30 to 21:00 UTC), where they answer a point at the last five-minute
//! mark: that day's close nudged by a seeded per-minute draw. `Between`
//! answers the daily closes in `[from, to)` and ignores `step`, since the
//! simulator has one point per day.
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
pub const TENORS: [&str; 8] = ["1M", "2M", "3M", "6M", "1Y", "18M", "2Y", "3Y"];
/// Put deltas: 0.9 is the downside wing, 0.1 the upside.
pub const DELTAS: [f64; 5] = [0.10, 0.25, 0.50, 0.75, 0.90];
pub const WINDOWS: [&str; 3] = ["5D", "20D", "60D"];

/// A Monday, and the first day with a close. Far enough back that a
/// `400d` history window is full from the first run.
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

/// The three tables' key columns, in key order.
fn key_columns(table: &str) -> Option<&'static [&'static str]> {
    match table {
        "surface" => Some(&["underlying_ref", "tenor", "delta"]),
        "realized" => Some(&["underlying_ref", "window"]),
        "varswap" => Some(&["underlying_ref", "tenor"]),
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
            // Tracks the 1M implied its window ago, cycling around it.
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
        _ => unreachable!("key_columns knows only the three tables"),
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

/// What `At(t)` answers: the last close at or before `t`, or inside a
/// weekday session the point at the last five-minute mark. `None` before
/// the first close.
fn observation_at(t: DateTime<Utc>) -> Option<Observation> {
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
        if time >= OPEN {
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
        let observations: Vec<Observation> = match req.span {
            Span::Latest => observation_at((self.now)()).into_iter().collect(),
            Span::At(t) => observation_at(t).into_iter().collect(),
            Span::Between { from, to, .. } => closes_between(from, to),
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
        // One walk per underlying per call: a 400-day prefix ask over the
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
                    let nudge = (0.004 * step(&mut rng)).exp();
                    values.iter_mut().for_each(|v| *v *= nudge);
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

        let latest =
            |now: fn() -> DateTime<Utc>| query(now, &ask("surface", spx_3m_atm(), Span::Latest));
        assert_eq!(latest(tuesday_evening).ts, [t("2026-01-13T21:00:00Z")]);
        assert_eq!(latest(tuesday_evening).values[0][0], tuesday[0]);
        assert_eq!(latest(tuesday_session).ts, [t("2026-01-13T15:05:00Z")]);
    }

    #[test]
    fn keys_enumerate_the_constants_and_unknown_tables_and_symbols_behave() {
        let adapter = DemoHanweck::new(42);
        assert_eq!(adapter.name(), DEMO_HANWECK);
        assert!(adapter.subscription().is_none() && adapter.egress().is_none());
        let mut h = adapter.history().unwrap();
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(h.keys("surface", "tenor"), Some(strings(&TENORS)));
        assert_eq!(h.keys("varswap", "tenor"), Some(strings(&TENORS)));
        assert_eq!(
            h.keys("surface", "delta"),
            Some(strings(&["0.1", "0.25", "0.5", "0.75", "0.9"]))
        );
        assert_eq!(h.keys("realized", "window"), Some(strings(&WINDOWS)));
        for table in ["surface", "realized", "varswap"] {
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
