//! Retention (spec §4.6). Bounded by disk rather than RAM now that storage
//! is persistent, so defaults are generous — but unbounded history would
//! still grow the database file without limit.
//!
//! Retention is per partition: "keep 50 generations" means each (batch, book)
//! keeps its own 50, so a busy book cannot evict a quiet one's history.

use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
use chrono::{DateTime, Duration, Utc};
use duckdb::Connection;
use geode_core::schema::Grain;

#[derive(Debug, Clone, Default)]
pub struct RetentionPolicy {
    /// Keep this many generations per partition.
    pub keep_generations: Option<usize>,
    /// Keep generations whose source time is within this window.
    pub keep_age: Option<Duration>,
}

impl RetentionPolicy {
    pub fn is_empty(&self) -> bool {
        self.keep_generations.is_none() && self.keep_age.is_none()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub evicted_rows: usize,
    /// How far back time travel can go (spec §4.6).
    pub oldest_remaining: Option<DateTime<Utc>>,
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

pub fn sweep(
    conn: &Connection,
    grains: &[Grain],
    policy: &RetentionPolicy,
    now: DateTime<Utc>,
) -> Result<SweepReport, StoreError> {
    let mut report = SweepReport::default();

    for grain in grains {
        let archive = table_name(*grain, TableKind::Archive);

        if !policy.is_empty() {
            let before: i64 = {
                let sql = format!("select count(*) from {archive}");
                conn.query_row(&sql, [], |r| r.get(0))
                    .map_err(sql_err(&sql))?
            };

            // A generation survives only if it satisfies every configured
            // rule; the stricter one therefore wins.
            let mut keep: Vec<String> = Vec::new();
            if let Some(n) = policy.keep_generations {
                keep.push(format!("rn <= {n}"));
            }
            if let Some(age) = policy.keep_age {
                keep.push(format!(
                    "source_time >= '{}'::timestamptz",
                    (now - age).to_rfc3339()
                ));
            }

            let sql = format!(
                "delete from {archive} where (batch, book, gen_id) not in (
                     select batch, book, gen_id from (
                         select batch, book, gen_id, source_time,
                                row_number() over (
                                    partition by batch, book order by source_time desc
                                ) as rn
                         from (select distinct batch, book, gen_id, source_time from {archive})
                     ) where {keep}
                 )",
                keep = keep.join(" and "),
            );
            conn.execute_batch(&sql).map_err(sql_err(&sql))?;

            let after: i64 = {
                let sql = format!("select count(*) from {archive}");
                conn.query_row(&sql, [], |r| r.get(0))
                    .map_err(sql_err(&sql))?
            };
            report.evicted_rows += (before - after).max(0) as usize;
        }

        let sql = format!("select min(source_time) from {archive}");
        let oldest: Option<DateTime<Utc>> = conn
            .query_row(&sql, [], |r| r.get(0))
            .map_err(sql_err(&sql))?;
        report.oldest_remaining = match (report.oldest_remaining, oldest) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }

    Ok(report)
}

/// Force a checkpoint. Owned by the sweeper because a checkpoint can stall
/// the writer and must not land mid-refresh (spec §4.6).
pub fn checkpoint(conn: &Connection) -> Result<(), StoreError> {
    let sql = "checkpoint";
    conn.execute_batch(sql).map_err(sql_err(sql))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use chrono::{DateTime, Timelike, Utc};
    use geode_core::schema::Grain;

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(
                "create table measures_position_archive(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);",
            )
            .unwrap();
        (dir, store)
    }

    /// `gens` generations for each of two partitions, one hour apart.
    fn fill(store: &Store, gens: i64) {
        for batch in ["BK000", "BK001"] {
            for g in 1..=gens {
                store
                    .writer()
                    .execute(
                        "insert into measures_position_archive values (?, ?, ?, ?)",
                        duckdb::params![
                            batch,
                            batch,
                            g,
                            ts("2026-08-30T00:00:00Z") + Duration::hours(g)
                        ],
                    )
                    .unwrap();
            }
        }
    }

    fn remaining(store: &Store) -> i64 {
        store
            .writer()
            .query_row("select count(*) from measures_position_archive", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn keep_by_count_is_per_partition() {
        let (_d, store) = fixture();
        fill(&store, 10);
        let policy = RetentionPolicy {
            keep_generations: Some(3),
            keep_age: None,
        };
        let report = sweep(
            store.writer(),
            &[Grain::Position],
            &policy,
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();

        assert_eq!(
            remaining(&store),
            6,
            "3 generations for each of 2 partitions"
        );
        assert_eq!(report.evicted_rows, 14);
    }

    #[test]
    fn keep_by_age_evicts_on_source_time() {
        let (_d, store) = fixture();
        fill(&store, 10);
        let policy = RetentionPolicy {
            keep_generations: None,
            keep_age: Some(Duration::hours(5)),
        };
        sweep(
            store.writer(),
            &[Grain::Position],
            &policy,
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        // Keeps source_time >= 05:00, i.e. generations 5..=10.
        assert_eq!(remaining(&store), 12);
    }

    #[test]
    fn both_policies_apply_together() {
        let (_d, store) = fixture();
        fill(&store, 10);
        let policy = RetentionPolicy {
            keep_generations: Some(8),
            keep_age: Some(Duration::hours(3)),
        };
        sweep(
            store.writer(),
            &[Grain::Position],
            &policy,
            ts("2026-08-30T10:00:00Z"),
        )
        .unwrap();
        // Age keeps 7..=10 (4 per partition); count would keep 8. The
        // stricter rule wins.
        assert_eq!(remaining(&store), 8);
    }

    #[test]
    fn oldest_remaining_is_published_for_the_time_travel_ui() {
        let (_d, store) = fixture();
        fill(&store, 10);
        let policy = RetentionPolicy {
            keep_generations: Some(3),
            keep_age: None,
        };
        let report = sweep(
            store.writer(),
            &[Grain::Position],
            &policy,
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.oldest_remaining.unwrap().hour(), 8);
    }

    #[test]
    fn an_empty_policy_evicts_nothing() {
        let (_d, store) = fixture();
        fill(&store, 5);
        let report = sweep(
            store.writer(),
            &[Grain::Position],
            &RetentionPolicy::default(),
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.evicted_rows, 0);
        assert_eq!(remaining(&store), 10);
    }

    #[test]
    fn sweeping_an_empty_archive_is_not_an_error() {
        let (_d, store) = fixture();
        let report = sweep(
            store.writer(),
            &[Grain::Position],
            &RetentionPolicy {
                keep_generations: Some(3),
                keep_age: None,
            },
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.evicted_rows, 0);
        assert!(report.oldest_remaining.is_none());
    }

    #[test]
    fn checkpoint_succeeds_on_a_live_database() {
        let (_d, store) = fixture();
        fill(&store, 2);
        checkpoint(store.writer()).unwrap();
    }
}
