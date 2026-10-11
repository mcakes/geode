//! From observations per input tuple to board rows. Pure: the data layer
//! reads the cache, this decides every number. The rules the tile shows
//! live here, once: a change is current minus the last observation at or
//! before the lookback instant; a rank is the share of the window's daily
//! observations strictly below the current, carried with their count; a
//! derived measure is combined pointwise over the days all its inputs
//! hold.

use super::{Cell, Change, Input, Role, VolBoardParams, VolRow};
use crate::clock::Clock;
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashMap};

/// What the reader found for one input tuple of one member, one value
/// (the input's column) per observation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observed {
    pub current: Option<(DateTime<Utc>, f64)>,
    /// Parallel to the request's lookbacks: the last observation at or
    /// before each lookback instant.
    pub at: Vec<Option<(DateTime<Utc>, f64)>>,
    /// The last observation of each UTC day (`plan::day_index`) inside the
    /// daily span, oldest first.
    pub daily: BTreeMap<i64, f64>,
    /// The earliest cached observation of the tuple, for the history-starts note.
    pub first: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observations {
    /// Keyed by `(member, input)`. A tuple the cache holds nothing for is absent.
    pub by_input: HashMap<(String, Input), Observed>,
    /// The first day index that counts toward the percentile window;
    /// `None` when no window was asked.
    pub window_from_day: Option<i64>,
    /// The first day index the sparkline shows; `None` without a sparkline.
    pub spark_from_day: Option<i64>,
    /// The lookback instants, parallel to the request's lookbacks, for the
    /// "no observation at or before" note.
    pub instants: Vec<DateTime<Utc>>,
}

/// A board's rows, its missing members in request order, and its notes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reduced {
    pub rows: Vec<VolRow>,
    pub missing: Vec<String>,
    pub notes: Vec<String>,
}

/// Share of `values` strictly below `current`, and their count. `(0.0, 0)`
/// for no values.
pub fn rank(values: impl Iterator<Item = f64>, current: f64) -> (f64, usize) {
    let (mut below, mut n) = (0usize, 0usize);
    for v in values {
        n += 1;
        if v < current {
            below += 1;
        }
    }
    if n == 0 {
        (0.0, 0)
    } else {
        (below as f64 / n as f64, n)
    }
}

/// The inputs' observations for one member, in `inputs()` order; `None`
/// when any input is absent from the cache.
fn gather<'a>(obs: &'a Observations, member: &str, inputs: &[Input]) -> Option<Vec<&'a Observed>> {
    inputs
        .iter()
        .map(|i| obs.by_input.get(&(member.to_string(), i.clone())))
        .collect()
}

/// The measure's daily series over the days every input holds, from
/// `from_day`, oldest first.
fn daily(measure: &super::Measure, found: &[&Observed], from_day: i64) -> Vec<f64> {
    let Some(first) = found.first() else {
        return Vec::new();
    };
    first
        .daily
        .range(from_day..)
        .filter_map(|(day, _)| {
            // Every input must hold the day, or a derived value would mix days.
            let values: Option<Vec<f64>> =
                found.iter().map(|o| o.daily.get(day).copied()).collect();
            values.map(|v| measure.combine(&v))
        })
        .collect()
}

fn cell(
    params: &VolBoardParams,
    obs: &Observations,
    member: &str,
    measure: &super::Measure,
) -> Cell {
    let inputs = measure.inputs();
    let blank = Cell {
        measure: measure.clone(),
        current: None,
        current_at: None,
        changes: vec![None; params.lookbacks.len()],
        pctl: None,
    };
    let Some(found) = gather(obs, member, &inputs) else {
        return blank;
    };
    let current = found
        .iter()
        .map(|o| o.current.map(|(_, v)| v))
        .collect::<Option<Vec<f64>>>()
        .map(|v| measure.combine(&v));
    // The oldest of the inputs' current times: a derived cell is only as
    // fresh as its stalest input.
    let current_at =
        current.and_then(|_| found.iter().filter_map(|o| o.current.map(|(t, _)| t)).min());
    let changes = (0..params.lookbacks.len())
        .map(|i| {
            let current = current?;
            let at: Vec<(DateTime<Utc>, f64)> = found
                .iter()
                .map(|o| o.at.get(i).copied().flatten())
                .collect::<Option<_>>()?;
            let from = measure.combine(&at.iter().map(|(_, v)| *v).collect::<Vec<_>>());
            // The later of the inputs' observation times names when the
            // comparison value was last complete.
            let when = at.iter().map(|(t, _)| *t).max()?;
            Some(Change {
                value: current - from,
                from,
                at: when,
            })
        })
        .collect();
    let pctl = match (current, obs.window_from_day) {
        (Some(current), Some(from_day)) => {
            let series = daily(measure, &found, from_day);
            if series.is_empty() {
                None
            } else {
                Some(rank(series.into_iter(), current))
            }
        }
        _ => None,
    };
    Cell {
        measure: measure.clone(),
        current,
        current_at,
        changes,
        pctl,
    }
}

pub fn reduce(params: &VolBoardParams, obs: &Observations, clock: &Clock) -> Reduced {
    let mut out = Reduced::default();
    let all_inputs: Vec<Input> = params
        .measures
        .iter()
        .chain(params.sparkline.iter().map(|(m, _)| m))
        .flat_map(|m| m.inputs())
        .collect();
    let reads_surface = all_inputs.iter().any(|i| i.role == Role::Surface);
    for member in &params.members {
        let found: Vec<(&Input, &Observed)> = all_inputs
            .iter()
            .filter_map(|i| {
                obs.by_input
                    .get(&(member.clone(), i.clone()))
                    .map(|o| (i, o))
            })
            .collect();
        // The row's time is the surface's, and a member is missing when
        // the surface answered nothing current for it: its row stays, with
        // whatever cells other datasets fill. A board that reads no
        // surface input counts any input's current observation instead, so
        // a realized-only board does not list every member.
        let as_of = found
            .iter()
            .filter(|(i, _)| i.role == Role::Surface)
            .filter_map(|(_, o)| o.current.map(|(t, _)| t))
            .max();
        let any_current = found.iter().any(|(_, o)| o.current.is_some());
        let present = as_of.is_some() || (!reads_surface && any_current);
        if !present {
            out.missing.push(member.clone());
        }
        let cells = params
            .measures
            .iter()
            .map(|m| cell(params, obs, member, m))
            .collect();
        let spark = match (&params.sparkline, obs.spark_from_day) {
            (Some((measure, _)), Some(from_day)) => gather(obs, member, &measure.inputs())
                .map(|found| daily(measure, &found, from_day))
                .filter(|s| !s.is_empty()),
            _ => None,
        };
        out.rows.push(VolRow {
            underlying: member.clone(),
            as_of,
            cells,
            spark,
        });
    }
    // One note per dataset whose lookbacks found nothing for some tuple.
    // With a window the cache holds the dataset's true start (the window
    // Between covers every lookback, by validation): "history starts".
    // Without one the cache holds point rows only, whose earliest says
    // nothing about where history starts: name the failing instant. Both
    // dates are the clock's local date: a lookback instant is the end of a
    // business day in the clock's zone, often already the next UTC day.
    let mut starts: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    let mut failed_at: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    for ((_, input), o) in &obs.by_input {
        let Some(dataset) = params.dataset(input.role) else {
            continue;
        };
        for (i, at) in o.at.iter().enumerate() {
            if at.is_none()
                && let Some(instant) = obs.instants.get(i)
            {
                let e = failed_at.entry(dataset.to_string()).or_insert(*instant);
                *e = (*e).min(*instant);
                if let Some(first) = o.first {
                    let e = starts.entry(dataset.to_string()).or_insert(first);
                    *e = (*e).min(first);
                }
            }
        }
    }
    out.notes = failed_at
        .into_iter()
        .map(
            |(ds, instant)| match (obs.window_from_day, starts.get(&ds)) {
                (Some(_), Some(first)) => {
                    format!("{ds}: history starts {}", clock.today(*first))
                }
                _ => format!("{ds}: no observation at or before {}", clock.today(instant)),
            },
        )
        .collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::QueryKey;
    use crate::volboard::{Lookback, Measure, Role, VolBoardParams};
    use std::time::{Duration, Instant};

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn point(tenor: &str, delta: &str, column: &'static str) -> Input {
        Input {
            role: Role::Surface,
            suffix: vec![tenor.into(), delta.into()],
            column,
        }
    }

    fn params(measures: Vec<Measure>) -> VolBoardParams {
        VolBoardParams {
            key: QueryKey(1),
            tag: 1,
            submitted: Instant::now(),
            surface: "vol_surface".into(),
            realized: Some("realized_vol".into()),
            varswap: None,
            members: vec!["SPX".into(), "ZZZ".into()],
            measures,
            lookbacks: vec![Lookback::D1, Lookback::M1],
            window: Some(Duration::from_secs(365 * 86_400)),
            sparkline: Some((Measure::atfm("3M"), Duration::from_secs(3 * 86_400))),
        }
    }

    fn instants() -> Vec<DateTime<Utc>> {
        vec![
            ts("2026-01-09T23:59:59.999999Z"),
            ts("2025-12-11T23:59:59.999999Z"),
        ]
    }

    /// SPX ATFM 3M: current 0.20 at day 10, D1 from 0.19 (day 9), M1 none
    /// (history starts day 8); daily 0.18, 0.19, 0.20 on days 8, 9, 10.
    fn observed_atfm() -> Observed {
        Observed {
            current: Some((ts("2026-01-10T21:00:00Z"), 0.20)),
            at: vec![Some((ts("2026-01-09T21:00:00Z"), 0.19)), None],
            daily: [(20_461, 0.18), (20_462, 0.19), (20_463, 0.20)]
                .into_iter()
                .collect(),
            first: Some(ts("2026-01-08T21:00:00Z")),
        }
    }

    #[test]
    fn rank_counts_strictly_below_and_carries_the_count() {
        assert_eq!(rank([1.0, 2.0, 3.0, 3.0, 4.0].into_iter(), 3.0), (0.4, 5));
        assert_eq!(rank([1.0, 2.0].into_iter(), 5.0), (1.0, 2));
        assert_eq!(rank(std::iter::empty(), 1.0), (0.0, 0));
    }

    #[test]
    fn a_point_cell_carries_current_changes_percentile_and_the_row_its_as_of_and_spark() {
        let p = params(vec![Measure::atfm("3M")]);
        let mut obs = Observations {
            window_from_day: Some(20_000),
            spark_from_day: Some(20_462),
            instants: instants(),
            ..Default::default()
        };
        obs.by_input
            .insert(("SPX".into(), point("3M", "0.5", "vol")), observed_atfm());
        let r = reduce(&p, &obs, &Clock::utc());
        assert_eq!(
            r.rows.len(),
            2,
            "one row per member, the missing one included"
        );
        let spx = &r.rows[0];
        assert_eq!(spx.underlying, "SPX");
        assert_eq!(spx.as_of, Some(ts("2026-01-10T21:00:00Z")));
        let cell = &spx.cells[0];
        assert_eq!(cell.measure, Measure::atfm("3M"));
        assert_eq!(cell.current, Some(0.20));
        let d1 = cell.changes[0].as_ref().unwrap();
        assert!((d1.value - 0.01).abs() < 1e-12);
        assert_eq!(d1.from, 0.19);
        assert_eq!(d1.at, ts("2026-01-09T21:00:00Z"));
        assert_eq!(
            cell.changes[1], None,
            "no observation at or before the M1 instant"
        );
        // 0.18 and 0.19 are below 0.20; the current day counts in the denominator.
        let (pctl, n) = cell.pctl.unwrap();
        assert!((pctl - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(n, 3);
        assert_eq!(
            spx.spark,
            Some(vec![0.19, 0.20]),
            "the sparkline starts at its own day"
        );
        assert_eq!(r.missing, vec!["ZZZ"]);
        let zzz = &r.rows[1];
        assert_eq!(zzz.as_of, None);
        assert_eq!(zzz.cells[0].current, None);
        assert_eq!(zzz.spark, None);
        assert_eq!(r.notes, vec!["vol_surface: history starts 2026-01-08"]);
    }

    #[test]
    fn a_derived_cell_combines_inputs_pointwise_and_ranks_over_the_days_both_hold() {
        // IV−RV 3M/20D: realized starts a day later than the surface.
        let p = params(vec![Measure::IvRv {
            tenor: "3M".into(),
            window: "20D".into(),
        }]);
        let mut obs = Observations {
            window_from_day: Some(20_000),
            spark_from_day: None,
            instants: instants(),
            ..Default::default()
        };
        obs.by_input
            .insert(("SPX".into(), point("3M", "0.5", "vol")), observed_atfm());
        let realized = Input {
            role: Role::Realized,
            suffix: vec!["20D".into()],
            column: "vol",
        };
        obs.by_input.insert(
            ("SPX".into(), realized),
            Observed {
                current: Some((ts("2026-01-10T20:00:00Z"), 0.15)),
                at: vec![Some((ts("2026-01-09T20:00:00Z"), 0.16)), None],
                daily: [(20_462, 0.16), (20_463, 0.15)].into_iter().collect(),
                first: Some(ts("2026-01-09T20:00:00Z")),
            },
        );
        let r = reduce(&p, &obs, &Clock::utc());
        let cell = &r.rows[0].cells[0];
        assert!((cell.current.unwrap() - 0.05).abs() < 1e-12);
        let d1 = cell.changes[0].as_ref().unwrap();
        assert!((d1.value - (0.05 - 0.03)).abs() < 1e-12);
        assert!((d1.from - 0.03).abs() < 1e-12);
        assert_eq!(
            d1.at,
            ts("2026-01-09T21:00:00Z"),
            "the later of the inputs' observation times"
        );
        // Days 9 and 10 only: 0.03 and 0.05; one below the current 0.05.
        assert_eq!(cell.pctl, Some((0.5, 2)));
        assert_eq!(
            r.rows[0].as_of,
            Some(ts("2026-01-10T21:00:00Z")),
            "as_of is the surface's time"
        );
        assert_eq!(
            r.notes,
            vec![
                "realized_vol: history starts 2026-01-09",
                "vol_surface: history starts 2026-01-08"
            ]
        );
    }

    #[test]
    fn a_missing_input_blanks_the_cell_but_not_the_row() {
        let p = params(vec![
            Measure::atfm("3M"),
            Measure::Skew { tenor: "3M".into() },
        ]);
        let mut obs = Observations {
            window_from_day: None,
            spark_from_day: None,
            instants: instants(),
            ..Default::default()
        };
        obs.by_input
            .insert(("SPX".into(), point("3M", "0.5", "vol")), observed_atfm());
        obs.by_input
            .insert(("SPX".into(), point("3M", "0.25", "vol")), observed_atfm());
        // No 0.75 point (the 25Δ put leg): skew cannot be formed.
        let r = reduce(&p, &obs, &Clock::utc());
        assert_eq!(r.rows[0].cells[0].current, Some(0.20));
        assert_eq!(r.rows[0].cells[1].current, None);
        assert_eq!(r.rows[0].cells[1].changes, vec![None, None]);
        assert_eq!(r.rows[0].cells[1].pctl, None, "no window asked");
        assert_eq!(r.missing, vec!["ZZZ"], "SPX has surface rows");
        // Without a window the cache holds point rows only, so the note
        // names the failing instant, not a history start.
        assert_eq!(
            r.notes,
            vec!["vol_surface: no observation at or before 2025-12-11"]
        );
    }

    #[test]
    fn the_window_day_restricts_the_rank_and_a_day_before_it_is_ignored() {
        let p = params(vec![Measure::atfm("3M")]);
        let mut obs = Observations {
            window_from_day: Some(20_462),
            spark_from_day: None,
            instants: instants(),
            ..Default::default()
        };
        obs.by_input
            .insert(("SPX".into(), point("3M", "0.5", "vol")), observed_atfm());
        let cell = &reduce(&p, &obs, &Clock::utc()).rows[0].cells[0];
        assert_eq!(cell.pctl, Some((0.5, 2)));
    }

    fn realized_20d() -> Input {
        Input {
            role: Role::Realized,
            suffix: vec!["20D".into()],
            column: "vol",
        }
    }

    fn observed_realized() -> Observed {
        Observed {
            current: Some((ts("2026-01-10T20:00:00Z"), 0.15)),
            at: vec![Some((ts("2026-01-09T20:00:00Z"), 0.16)), None],
            daily: [(20_462, 0.16), (20_463, 0.15)].into_iter().collect(),
            first: Some(ts("2026-01-09T20:00:00Z")),
        }
    }

    #[test]
    fn a_member_the_surface_answered_nothing_for_is_missing_though_realized_answered() {
        let p = params(vec![
            Measure::atfm("3M"),
            Measure::Realized {
                window: "20D".into(),
            },
        ]);
        let mut obs = Observations {
            window_from_day: None,
            spark_from_day: None,
            instants: instants(),
            ..Default::default()
        };
        obs.by_input
            .insert(("SPX".into(), realized_20d()), observed_realized());
        let r = reduce(&p, &obs, &Clock::utc());
        assert_eq!(
            r.missing,
            vec!["SPX", "ZZZ"],
            "SPX has no surface observation"
        );
        let spx = &r.rows[0];
        assert_eq!(spx.underlying, "SPX", "the missing member keeps its row");
        assert_eq!(spx.as_of, None, "as_of is the surface's time");
        assert_eq!(spx.cells[0].current, None);
        assert_eq!(
            spx.cells[1].current,
            Some(0.15),
            "the realized cell still fills"
        );
    }

    #[test]
    fn a_board_reading_no_surface_input_counts_any_input_as_present() {
        let mut p = params(vec![Measure::Realized {
            window: "20D".into(),
        }]);
        p.sparkline = None;
        let mut obs = Observations {
            window_from_day: None,
            spark_from_day: None,
            instants: instants(),
            ..Default::default()
        };
        obs.by_input
            .insert(("SPX".into(), realized_20d()), observed_realized());
        let r = reduce(&p, &obs, &Clock::utc());
        assert_eq!(
            r.missing,
            vec!["ZZZ"],
            "SPX has a realized observation and the board reads no surface"
        );
        assert_eq!(r.rows[0].cells[0].current, Some(0.15));
        assert_eq!(r.rows[0].as_of, None);
    }

    /// A cell is only as fresh as its stalest input: a point cell carries
    /// its observation's time, an IV-RV cell whose realized current is older
    /// than the surface's carries the realized time, and a blank cell none.
    #[test]
    fn a_cell_carries_its_oldest_inputs_observation_time() {
        let p = params(vec![
            Measure::atfm("3M"),
            Measure::IvRv {
                tenor: "3M".into(),
                window: "20D".into(),
            },
            Measure::Skew { tenor: "3M".into() },
        ]);
        let mut obs = Observations {
            window_from_day: None,
            spark_from_day: None,
            instants: instants(),
            ..Default::default()
        };
        obs.by_input
            .insert(("SPX".into(), point("3M", "0.5", "vol")), observed_atfm());
        obs.by_input
            .insert(("SPX".into(), realized_20d()), observed_realized());
        let r = reduce(&p, &obs, &Clock::utc());
        let cells = &r.rows[0].cells;
        assert_eq!(
            cells[0].current_at,
            Some(ts("2026-01-10T21:00:00Z")),
            "a point cell carries its observation time"
        );
        assert_eq!(
            cells[1].current_at,
            Some(ts("2026-01-10T20:00:00Z")),
            "IV-RV carries the older realized time"
        );
        assert_eq!(cells[2].current, None, "no skew legs");
        assert_eq!(cells[2].current_at, None, "a blank cell carries none");
        assert_eq!(
            r.rows[1].cells[0].current_at, None,
            "a missing member's cell carries none"
        );
    }

    /// The notes name dates on the clock's calendar: a lookback instant is
    /// the end of a business day in the clock's zone, which in New York is
    /// already the next UTC day, and the earliest observation's date is the
    /// desk's, not UTC's.
    #[test]
    fn notes_name_the_clocks_local_date() {
        let ny = Clock::in_zone_named("America/New_York");
        let p = params(vec![Measure::atfm("3M")]);
        let observed = Observed {
            current: Some((ts("2026-01-13T21:00:00Z"), 0.20)),
            at: vec![None, None],
            daily: BTreeMap::new(),
            // 22:00 on Thursday the 8th in New York.
            first: Some(ts("2026-01-09T03:00:00Z")),
        };
        // The end of Monday the 12th and of Friday Dec 12 in New York.
        let ny_instants = vec![
            ts("2026-01-13T04:59:59.999999Z"),
            ts("2025-12-13T04:59:59.999999Z"),
        ];
        let mut obs = Observations {
            window_from_day: None,
            spark_from_day: None,
            instants: ny_instants,
            ..Default::default()
        };
        obs.by_input
            .insert(("SPX".into(), point("3M", "0.5", "vol")), observed);
        assert_eq!(
            reduce(&p, &obs, &ny).notes,
            vec!["vol_surface: no observation at or before 2025-12-12"]
        );
        obs.window_from_day = Some(20_000);
        assert_eq!(
            reduce(&p, &obs, &ny).notes,
            vec!["vol_surface: history starts 2026-01-08"]
        );
    }
}
