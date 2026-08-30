//! The cold-start priority ladder (spec §5.4). Ingest is a priority queue,
//! not a sweep: the desk has strong priors about what it wants to see
//! first, and a sentinel-only scan is cheap enough to plan the whole run
//! before opening a single CSV.

use crate::source::{Candidate, CandidateState, Priority, SourceSpec};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub source: String,
    pub dataset: String,
    pub batch: String,
    pub candidate: Candidate,
    pub priority: Priority,
    pub source_time: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct WorkPlan {
    /// Highest priority first; newest first within a priority.
    pub items: Vec<WorkItem>,
}

pub fn build_plan(discovered: &[(SourceSpec, Vec<Candidate>)]) -> WorkPlan {
    let mut items: Vec<WorkItem> = Vec::new();

    for (spec, candidates) in discovered {
        // Only the newest file per batch is "current"; the rest are history,
        // however recent the source. This is what puts today's risk on
        // screen before yesterday's finishes loading.
        let mut newest_per_batch: HashMap<&str, DateTime<Utc>> = HashMap::new();
        for c in candidates {
            if let CandidateState::Ready(s) = &c.state {
                let e = newest_per_batch.entry(c.batch.as_str()).or_insert(s.as_of);
                if s.as_of > *e {
                    *e = s.as_of;
                }
            }
        }

        for c in candidates {
            let CandidateState::Ready(sentinel) = &c.state else {
                continue;
            };
            let is_current = newest_per_batch
                .get(c.batch.as_str())
                .is_some_and(|newest| *newest == sentinel.as_of);
            items.push(WorkItem {
                source: spec.name.clone(),
                dataset: spec.dataset.clone(),
                batch: c.batch.clone(),
                candidate: c.clone(),
                priority: if is_current {
                    spec.priority
                } else {
                    Priority::Backfill
                },
                source_time: sentinel.as_of,
            });
        }
    }

    items.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(b.source_time.cmp(&a.source_time))
    });
    WorkPlan { items }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Candidate, CandidateState, Priority, Readiness, SourceSpec};
    use chrono::{DateTime, Datelike, Utc};
    use std::time::{Duration, SystemTime};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn spec(name: &str, priority: Priority) -> SourceSpec {
        SourceSpec {
            name: name.into(),
            dataset: format!("{name}_dataset"),
            paths: vec![],
            readiness: Readiness::Sentinel,
            priority,
            poll_interval: Duration::from_secs(30),
            pending_timeout: Duration::from_secs(60),
            batch_pattern: None,
        }
    }

    fn candidate(batch: &str, day: u8, state_ready: bool) -> Candidate {
        let as_of = ts(&format!("2026-08-{day:02}T07:00:00Z"));
        Candidate {
            csv_path: format!("/src/risk_2026-08-{day:02}_{batch}.csv").into(),
            sentinel_path: format!("/src/risk_2026-08-{day:02}_{batch}.csv.done").into(),
            batch: batch.into(),
            size: 1,
            mtime: SystemTime::now(),
            state: if state_ready {
                CandidateState::Ready(crate::source::Sentinel {
                    as_of,
                    columns: vec!["Book".into()],
                    books: vec![batch.into()],
                    row_count: None,
                    dataset: None,
                    business_date: None,
                })
            } else {
                CandidateState::Pending
            },
        }
    }

    #[test]
    fn only_ready_candidates_enter_the_plan() {
        let plan = build_plan(&[(
            spec("risk", Priority::LatestRisk),
            vec![candidate("BK000", 30, true), candidate("BK001", 30, false)],
        )]);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].batch, "BK000");
    }

    #[test]
    fn within_a_batch_only_the_newest_file_keeps_its_source_priority() {
        let plan = build_plan(&[(
            spec("risk", Priority::LatestRisk),
            vec![
                candidate("BK000", 28, true),
                candidate("BK000", 30, true),
                candidate("BK000", 29, true),
            ],
        )]);
        let newest = plan
            .items
            .iter()
            .find(|i| i.source_time.day() == 30)
            .unwrap();
        assert_eq!(newest.priority, Priority::LatestRisk);
        for older in plan.items.iter().filter(|i| i.source_time.day() != 30) {
            assert_eq!(
                older.priority,
                Priority::Backfill,
                "history must not outrank current risk"
            );
        }
    }

    #[test]
    fn ordering_is_priority_then_newest_first() {
        let plan = build_plan(&[
            (
                spec("vol", Priority::LatestOther),
                vec![candidate("VOL", 30, true)],
            ),
            (
                spec("risk", Priority::LatestRisk),
                vec![candidate("BK000", 30, true), candidate("BK000", 29, true)],
            ),
        ]);
        let order: Vec<(Priority, u32)> = plan
            .items
            .iter()
            .map(|i| (i.priority, i.source_time.day()))
            .collect();
        assert_eq!(
            order,
            vec![
                (Priority::LatestRisk, 30),
                (Priority::LatestOther, 30),
                (Priority::Backfill, 29),
            ],
            "current risk, then everything else current, then history"
        );
    }

    #[test]
    fn batches_are_independent() {
        let plan = build_plan(&[(
            spec("risk", Priority::LatestRisk),
            vec![candidate("BK000", 30, true), candidate("BK001", 29, true)],
        )]);
        // BK001's newest is the 29th; it is still that batch's current file.
        for item in &plan.items {
            assert_eq!(item.priority, Priority::LatestRisk, "{item:?}");
        }
    }

    #[test]
    fn an_empty_input_yields_an_empty_plan() {
        assert!(build_plan(&[]).items.is_empty());
    }
}
