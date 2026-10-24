//! What a board request asks of the history family, as pure decisions:
//! the instant each lookback compares against, the key tuples the
//! measures read, and how far back the daily series goes. `geode-data`
//! turns these into history calls and SQL.

use super::{Input, Lookback, Role, VolBoardParams};
use crate::clock::{Clock, ClockError, business_days_back};
use crate::query::END_OF_DAY;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::time::Duration;

/// The instant a lookback compares against: the end of the business day
/// `n` back from the clock's today, in the clock's zone. The clock's `eod`
/// is a display boundary (18:00 by default) that a 21:00 close would fall
/// after, so the calendar day's end is used instead. "Previous business
/// day's last observation" is therefore `At(this instant)`.
pub fn lookback_instant(
    clock: &Clock,
    now: DateTime<Utc>,
    lb: Lookback,
) -> Result<DateTime<Utc>, ClockError> {
    let day = business_days_back(clock.today(now), lb.business_days());
    clock.resolve_local(day, END_OF_DAY)
}

/// Every input the request reads: the measures' in order, then the
/// sparklines', repeats removed.
pub fn inputs(params: &VolBoardParams) -> Vec<Input> {
    let mut out: Vec<Input> = Vec::new();
    let measures = params
        .measures
        .iter()
        .chain(params.sparklines.iter().map(|(m, _)| m));
    for measure in measures {
        for input in measure.inputs() {
            if !out.contains(&input) {
                out.push(input);
            }
        }
    }
    out
}

/// The exact key suffixes each role reads, distinct, in first-seen
/// order: what the `At` and `Between` asks name after the member.
pub fn suffixes(params: &VolBoardParams) -> BTreeMap<Role, Vec<Vec<String>>> {
    let mut out: BTreeMap<Role, Vec<Vec<String>>> = BTreeMap::new();
    for input in inputs(params) {
        let tuples = out.entry(input.role.clone()).or_default();
        if !tuples.contains(&input.suffix) {
            tuples.push(input.suffix);
        }
    }
    out
}

/// How far back the daily series reaches: the longer of the percentile
/// window and every sparkline span, one `Between` per dataset serving all;
/// `None` when neither is asked.
pub fn daily_span(params: &VolBoardParams) -> Option<Duration> {
    params
        .sparklines
        .iter()
        .map(|(_, d)| *d)
        .chain(params.window)
        .max()
}

/// Days since 1970-01-01 UTC, floored: the day bucket the board SQL uses
/// (`epoch_us(ts) // 86_400_000_000`), so Rust and SQL agree on a day.
pub fn day_index(t: DateTime<Utc>) -> i64 {
    t.timestamp().div_euclid(86_400)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;
    use crate::query::QueryKey;
    use crate::volboard::{Measure, VolBoardParams};
    use std::time::Instant;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_lookback_ends_at_the_end_of_the_business_day_n_back_in_the_clocks_zone() {
        let utc = Clock::utc();
        // Tuesday 2026-01-13 10:00: D1 is the end of Monday the 12th.
        let now = ts("2026-01-13T10:00:00Z");
        assert_eq!(
            lookback_instant(&utc, now, Lookback::D1).unwrap(),
            ts("2026-01-12T23:59:59.999999Z")
        );
        assert_eq!(
            lookback_instant(&utc, now, Lookback::W1).unwrap(),
            ts("2026-01-06T23:59:59.999999Z")
        );
        // 21 business days back from Tuesday the 13th: Mon 12, 9, 8, 7, 6, 5, 2, 1,
        // Dec 31, 30, 29, 26, 25, 24, 23, 22, 19, 18, 17, 16, 15.
        assert_eq!(
            lookback_instant(&utc, now, Lookback::M1).unwrap(),
            ts("2025-12-15T23:59:59.999999Z")
        );
        // Monday 10:00: D1 is Friday's end, not Thursday's.
        let monday = ts("2026-01-12T10:00:00Z");
        assert_eq!(
            lookback_instant(&utc, monday, Lookback::D1).unwrap(),
            ts("2026-01-09T23:59:59.999999Z")
        );
        // Saturday: today snaps to Friday, D1 is Thursday.
        let saturday = ts("2026-01-10T12:00:00Z");
        assert_eq!(
            lookback_instant(&utc, saturday, Lookback::D1).unwrap(),
            ts("2026-01-08T23:59:59.999999Z")
        );
        // New York: 2026-01-13 02:00 UTC is still Monday evening there, so
        // D1 is the end of Friday the 9th in New York (05:00 UTC Saturday).
        let ny = Clock::in_zone_named("America/New_York");
        assert_eq!(
            lookback_instant(&ny, ts("2026-01-13T02:00:00Z"), Lookback::D1).unwrap(),
            ts("2026-01-10T04:59:59.999999Z")
        );
    }

    #[test]
    fn the_clocks_eod_is_not_the_lookback_boundary() {
        // A 21:00 UTC close lies after the default 18:00 eod and before the
        // instant: it is the D1 comparison, as the desk expects.
        let utc = Clock::utc();
        let instant = lookback_instant(&utc, ts("2026-01-13T10:00:00Z"), Lookback::D1).unwrap();
        assert!(ts("2026-01-12T21:00:00Z") <= instant);
        assert!(instant < ts("2026-01-13T00:00:00Z"));
    }

    fn params() -> VolBoardParams {
        VolBoardParams {
            key: QueryKey(1),
            tag: 0,
            submitted: Instant::now(),
            surface: "vol_surface".into(),
            realized: Some("realized_vol".into()),
            varswap: Some("varswap".into()),
            tables: Vec::new(),
            members: vec!["SPX".into()],
            measures: vec![
                Measure::atfm("3M"),
                Measure::Skew { tenor: "3M".into() },
                Measure::IvRv {
                    tenor: "3M".into(),
                    window: "20D".into(),
                },
                Measure::VsSpread { tenor: "3M".into() },
            ],
            lookbacks: vec![Lookback::D1],
            window: Some(Duration::from_secs(365 * 86_400)),
            sparklines: vec![(Measure::atfm("1M"), Duration::from_secs(30 * 86_400))],
        }
    }

    #[test]
    fn inputs_are_distinct_in_first_seen_order_with_the_sparkline_last() {
        let ins = inputs(&params());
        let names: Vec<String> = ins
            .iter()
            .map(|i| format!("{:?} {} {}", i.role, i.suffix.join("/"), i.column))
            .collect();
        assert_eq!(
            names,
            vec![
                "Surface 3M/0.5 vol",
                "Surface 3M/0.75 vol",
                "Surface 3M/0.25 vol",
                "Realized 20D vol",
                "Varswap 3M strike",
                "Surface 1M/0.5 vol",
            ]
        );
    }

    #[test]
    fn suffixes_group_tuples_by_role_without_the_value_column() {
        let s = suffixes(&params());
        assert_eq!(
            s[&Role::Surface],
            vec![
                vec!["3M", "0.5"],
                vec!["3M", "0.75"],
                vec!["3M", "0.25"],
                vec!["1M", "0.5"]
            ]
            .into_iter()
            .map(|t| t.into_iter().map(String::from).collect::<Vec<_>>())
            .collect::<Vec<_>>()
        );
        assert_eq!(s[&Role::Realized], vec![vec!["20D".to_string()]]);
        assert_eq!(s[&Role::Varswap], vec![vec!["3M".to_string()]]);
        let mut p = params();
        p.measures = vec![Measure::atfm("3M"), Measure::Forward { tenor: "3M".into() }];
        p.sparklines.clear();
        assert_eq!(
            suffixes(&p)[&Role::Surface].len(),
            1,
            "vol and forward share one tuple"
        );
        assert!(!suffixes(&p).contains_key(&Role::Realized));
    }

    #[test]
    fn the_daily_span_is_the_longer_of_window_and_sparkline() {
        assert_eq!(
            daily_span(&params()),
            Some(Duration::from_secs(365 * 86_400))
        );
        let mut p = params();
        p.window = None;
        assert_eq!(daily_span(&p), Some(Duration::from_secs(30 * 86_400)));
        p.sparklines.clear();
        assert_eq!(daily_span(&p), None);
    }

    #[test]
    fn the_daily_span_is_the_longest_of_window_and_every_sparkline() {
        let day = |n: u64| Duration::from_secs(n * 86_400);
        let mut p = params();
        p.window = Some(day(100));
        p.sparklines = vec![
            (Measure::atfm("3M"), day(30)),
            (Measure::Skew { tenor: "3M".into() }, day(365)),
        ];
        assert_eq!(daily_span(&p), Some(day(365)));
    }

    #[test]
    fn day_index_floors_to_the_utc_day() {
        assert_eq!(day_index(ts("1970-01-01T00:00:00Z")), 0);
        assert_eq!(day_index(ts("1970-01-02T23:59:59Z")), 1);
        assert_eq!(day_index(ts("2026-01-10T21:00:00Z")), 20_463);
        assert_eq!(day_index(ts("1969-12-31T12:00:00Z")), -1);
    }
}
