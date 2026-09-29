//! The blotter's prepared header: its dataset time runs, the frame's
//! historical as-of warning and the datasets its health question reads,
//! rebuilt when a snapshot lands or the display clock changes — never in
//! render, which only decides each run's staleness.

use geode_core::clock::Clock;
use geode_core::snapshot::Provenance;
use gpui::{App, SharedString};

use crate::tile::short_time;

/// One dataset's freshness run: its label (`risk 14:00`, or `risk —`) and
/// the raw source time render compares against `stale_after`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DatasetTime {
    pub label: SharedString,
    pub as_of: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeaderModel {
    /// Oldest source time first, as the header has always ordered them.
    pub times: Vec<DatasetTime>,
    /// `AS OF …` for a historical read, shown only while following the frame.
    pub frame_as_of: Option<SharedString>,
    /// The health question: the provenance datasets, sorted, once each.
    pub datasets: Vec<String>,
}

impl HeaderModel {
    pub(crate) fn prepare(p: &Provenance, clock: Clock) -> HeaderModel {
        let mut sorted: Vec<_> = p.datasets.iter().collect();
        sorted.sort_by(|a, b| a.as_of.cmp(&b.as_of));
        let times = sorted
            .into_iter()
            .map(|f| DatasetTime {
                label: match &f.as_of {
                    Some(t) => format!("{} {}", f.dataset, short_time(t, clock)).into(),
                    None => format!("{} —", f.dataset).into(),
                },
                as_of: f.as_of.clone(),
            })
            .collect();
        let frame_as_of = p.as_of_request.as_ref().map(|req| {
            chrono::DateTime::parse_from_rfc3339(req)
                .map(|t| format!("AS OF {}", clock.local(t.to_utc()).format("%Y-%m-%d %H:%M")))
                .unwrap_or_else(|_| format!("AS OF {}", req.get(..16).unwrap_or(req)))
                .into()
        });
        let mut datasets: Vec<String> = p.datasets.iter().map(|f| f.dataset.clone()).collect();
        datasets.sort_unstable();
        datasets.dedup();
        HeaderModel {
            times,
            frame_as_of,
            datasets,
        }
    }
}

/// The installed display clock, or the machine's for a tile hosted without
/// `AppClock`.
pub(crate) fn app_clock(cx: &App) -> Clock {
    cx.try_global::<geode_shell::clock::AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| Clock::machine().0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::snapshot::Freshness;

    #[test]
    fn prepare_orders_times_and_collects_the_datasets() {
        let p = Provenance {
            datasets: vec![
                Freshness {
                    dataset: "pnl".into(),
                    as_of: Some("2026-09-12T15:00:00Z".into()),
                    generation: None,
                },
                Freshness {
                    dataset: "risk".into(),
                    as_of: Some("2026-09-12T14:00:00Z".into()),
                    generation: None,
                },
                Freshness {
                    dataset: "risk".into(),
                    as_of: None,
                    generation: None,
                },
            ],
            as_of_request: Some("2026-09-11T09:30:00Z".into()),
        };
        let h = HeaderModel::prepare(&p, Clock::utc());
        let labels: Vec<&str> = h.times.iter().map(|t| t.label.as_ref()).collect();
        assert_eq!(labels, vec!["risk —", "risk 14:00", "pnl 15:00"]);
        assert_eq!(h.datasets, vec!["pnl".to_string(), "risk".to_string()]);
        assert_eq!(h.frame_as_of.as_deref(), Some("AS OF 2026-09-11 09:30"));
    }
}
