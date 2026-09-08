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

/// The `generations` reconciliation delete: a row survives only if at
/// least one of `grains`' archive **or** live tables still holds it. Must
/// cover every grain the dataset has -- `sweep`'s own callers pass
/// `ds.grains()` -- because a generation this sweep evicted from one
/// grain's archive can still be present at another (a cash-only book has
/// no underlying rows, so its position-grain history outlives anything
/// recorded at the underlying grain, and the reverse is just as real: a
/// grain published less often can hold a generation long after a busier
/// grain has aged it out). Reconciling against a subset would delete a
/// summary row for a generation that a grain outside the subset still
/// has, and time travel to it would find data with no summary entry to
/// resolve through.
///
/// This scans every named table, same as the eviction above already
/// does -- background work on the sweeper's own cadence, never paid by a
/// requery (`query::as_of::resolve_generations` reads only the summary).
fn generations_reconcile_sql(dataset: &str, grains: &[Grain]) -> String {
    let escaped = dataset.replace('\'', "''");
    let checks: Vec<String> = grains
        .iter()
        .flat_map(|g| {
            [
                table_name(dataset, *g, TableKind::Archive),
                table_name(dataset, *g, TableKind::Live),
            ]
        })
        .map(|t| {
            format!(
                "not exists (
                     select 1 from {t} t
                     where t.batch is not distinct from g.batch
                       and t.book is not distinct from g.book
                       and t.gen_id = g.gen_id
                       and t.source_time = g.source_time
                 )"
            )
        })
        .collect();
    format!(
        "delete from generations g
         where g.dataset = '{escaped}' and {}",
        checks.join(" and "),
    )
}

pub fn sweep(
    conn: &Connection,
    dataset: &str,
    grains: &[Grain],
    policy: &RetentionPolicy,
    now: DateTime<Utc>,
) -> Result<SweepReport, StoreError> {
    conn.execute_batch("begin;").map_err(sql_err("begin"))?;
    match sweep_in_transaction(conn, dataset, grains, policy, now) {
        Ok(report) => {
            conn.execute_batch("commit;").map_err(sql_err("commit"))?;
            Ok(report)
        }
        Err(e) => {
            let _ = conn.execute_batch("rollback;");
            Err(e)
        }
    }
}

/// The body of `sweep`, run inside the transaction `sweep` owns: eviction
/// per grain, then the `generations` reconciliation once every grain has
/// been swept.
fn sweep_in_transaction(
    conn: &Connection,
    dataset: &str,
    grains: &[Grain],
    policy: &RetentionPolicy,
    now: DateTime<Utc>,
) -> Result<SweepReport, StoreError> {
    let mut report = SweepReport::default();

    for grain in grains {
        let archive = table_name(dataset, *grain, TableKind::Archive);

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

            // NOT EXISTS, not NOT IN: a single NULL in the subquery makes
            // `NOT IN` evaluate to UNKNOWN for every row, so one archived
            // row with a NULL book would silently disable retention
            // forever. `is not distinct from` keeps NULL keys matchable.
            //
            // Ties on `source_time` break on `gen_id`, newest first — the
            // same order `as_of.rs` resolves by, so the generation time
            // travel would pick is never the one retention evicts. A
            // corrected republish makes such ties ordinary (§4.4).
            let sql = format!(
                "delete from {archive} a where not exists (
                     select 1 from (
                         select batch, book, gen_id, source_time,
                                row_number() over (
                                    partition by batch, book
                                    order by source_time desc, gen_id desc
                                ) as rn
                         from (select distinct batch, book, gen_id, source_time from {archive})
                     ) k
                     where k.batch is not distinct from a.batch
                       and k.book is not distinct from a.book
                       and k.gen_id is not distinct from a.gen_id
                       and {keep}
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

    if !grains.is_empty() {
        let sql = generations_reconcile_sql(dataset, grains);
        conn.execute_batch(&sql).map_err(sql_err(&sql))?;
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
    use crate::store::catalog::Catalog;
    use crate::store::ddl::{assert_generations_match_tables, rebuild_generations};
    use chrono::{DateTime, Datelike, Timelike, Utc};
    use geode_core::schema::Grain;

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// The archive plus an (empty) live table, so a reconciliation query
    /// naming both tables of the grain compiles even when nothing is live.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_position_archive(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 create table risk_snapshot_position_live(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);",
            )
            .unwrap();
        (dir, store)
    }

    const POSITION_TABLES: [&str; 2] = [
        "risk_snapshot_position_archive",
        "risk_snapshot_position_live",
    ];

    fn generation_count(store: &Store) -> i64 {
        store
            .writer()
            .query_row(
                "select count(*) from generations where dataset = 'risk_snapshot'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// `gens` generations for each of two partitions, one hour apart.
    fn fill(store: &Store, gens: i64) {
        for batch in ["BK000", "BK001"] {
            for g in 1..=gens {
                store
                    .writer()
                    .execute(
                        "insert into risk_snapshot_position_archive values (?, ?, ?, ?)",
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
            .query_row(
                "select count(*) from risk_snapshot_position_archive",
                [],
                |r| r.get(0),
            )
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
            "risk_snapshot",
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
            "risk_snapshot",
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
            "risk_snapshot",
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
            "risk_snapshot",
            &[Grain::Position],
            &policy,
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.oldest_remaining.unwrap().hour(), 8);
    }

    #[test]
    fn the_oldest_remaining_bound_spans_every_grain_swept() {
        // `oldest_remaining` answers "how far back can time travel go",
        // and it folds across grains. Every other test here sweeps a
        // single grain, where folding a lone value with `min` and with
        // `max` are the same thing — so the fold itself was never
        // exercised, and reporting the *newest* grain's oldest row would
        // have understated the history actually held.
        let (_d, store) = fixture();
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_underlying_archive(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 create table risk_snapshot_underlying_live(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);",
            )
            .unwrap();
        fill(&store, 3);

        // The underlying archive reaches further back than the position
        // one, so the two grains disagree and the fold has to choose.
        store
            .writer()
            .execute(
                "insert into risk_snapshot_underlying_archive values (?, ?, ?, ?)",
                duckdb::params!["BK000", "BK000", 1i64, ts("2026-08-29T02:00:00Z")],
            )
            .unwrap();

        let report = sweep(
            store.writer(),
            "risk_snapshot",
            &[Grain::Position, Grain::Underlying],
            &RetentionPolicy::default(),
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();

        let oldest = report.oldest_remaining.expect("history is held");
        assert_eq!(
            (oldest.day(), oldest.hour()),
            (29, 2),
            "the bound is the oldest row across every grain, not the \
             oldest of whichever grain was swept last"
        );
    }

    #[test]
    fn an_empty_policy_evicts_nothing() {
        let (_d, store) = fixture();
        fill(&store, 5);
        let report = sweep(
            store.writer(),
            "risk_snapshot",
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
            "risk_snapshot",
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
    fn a_null_book_does_not_disable_the_whole_sweep() {
        // With `NOT IN`, one NULL key makes the predicate UNKNOWN for every
        // row and retention silently stops working forever.
        let (_d, store) = fixture();
        fill(&store, 10);
        store
            .writer()
            .execute(
                "insert into risk_snapshot_position_archive values (NULL, NULL, 99, ?)",
                duckdb::params![ts("2026-08-30T01:00:00Z")],
            )
            .unwrap();

        let report = sweep(
            store.writer(),
            "risk_snapshot",
            &[Grain::Position],
            &RetentionPolicy {
                keep_generations: Some(3),
                keep_age: None,
            },
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();

        assert!(
            report.evicted_rows > 0,
            "a NULL key must not poison the predicate"
        );
        assert_eq!(
            remaining(&store),
            7,
            "3 generations per real partition, plus the NULL partition's own"
        );
    }

    #[test]
    fn a_tie_on_source_time_keeps_the_generation_as_of_would_pick() {
        // Two generations at one instant (a corrected republish, §4.4):
        // keep-one must keep the newer gen_id, the one `as_of.rs`
        // resolves to, or time travel points at an evicted generation.
        let (_d, store) = fixture();
        for _ in 0..10 {
            store
                .writer()
                .execute_batch(
                    "delete from risk_snapshot_position_archive;
                     insert into risk_snapshot_position_archive values
                       ('BK000', 'BK000', 1, '2026-08-30T07:00:00Z'),
                       ('BK000', 'BK000', 2, '2026-08-30T07:00:00Z');",
                )
                .unwrap();
            sweep(
                store.writer(),
                "risk_snapshot",
                &[Grain::Position],
                &RetentionPolicy {
                    keep_generations: Some(1),
                    keep_age: None,
                },
                ts("2026-08-31T00:00:00Z"),
            )
            .unwrap();
            let kept: i64 = store
                .writer()
                .query_row(
                    "select gen_id from risk_snapshot_position_archive",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(kept, 2, "the correction survives, every time");
        }
    }

    #[test]
    fn checkpoint_succeeds_on_a_live_database() {
        let (_d, store) = fixture();
        fill(&store, 2);
        checkpoint(store.writer()).unwrap();
    }

    #[test]
    fn sweeping_leaves_the_summary_matching_the_tables() {
        let (_d, store) = fixture();
        fill(&store, 3);
        let tables = POSITION_TABLES.map(String::from);
        rebuild_generations(store.writer(), "risk_snapshot", &tables).unwrap();

        sweep(
            store.writer(),
            "risk_snapshot",
            &[Grain::Position],
            &RetentionPolicy {
                keep_generations: Some(1),
                keep_age: None,
            },
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();

        assert_generations_match_tables(store.writer(), "risk_snapshot", &tables);
        assert_eq!(
            generation_count(&store),
            2,
            "one kept generation per partition, two partitions"
        );
    }

    #[test]
    fn a_generation_present_at_only_one_grain_survives_the_reconciliation() {
        // Position holds three generations of BK000 and BK001; retention
        // evicts all but the newest of each. Underlying holds only
        // BK000's first generation -- never swept away, because
        // `keep_generations = 1` already keeps a lone generation. That
        // first generation must survive in the summary through the
        // underlying grain alone, even though position's own copy of it
        // is gone.
        let (_d, store) = fixture();
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_underlying_archive(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 create table risk_snapshot_underlying_live(
                     book varchar, batch varchar, gen_id bigint,
                     source_time timestamp with time zone);",
            )
            .unwrap();
        fill(&store, 3);
        store
            .writer()
            .execute(
                "insert into risk_snapshot_underlying_archive values (?, ?, ?, ?)",
                duckdb::params!["BK000", "BK000", 1i64, ts("2026-08-30T01:00:00Z")],
            )
            .unwrap();

        let tables = [
            "risk_snapshot_position_archive",
            "risk_snapshot_position_live",
            "risk_snapshot_underlying_archive",
            "risk_snapshot_underlying_live",
        ]
        .map(String::from);
        rebuild_generations(store.writer(), "risk_snapshot", &tables).unwrap();

        sweep(
            store.writer(),
            "risk_snapshot",
            &[Grain::Position, Grain::Underlying],
            &RetentionPolicy {
                keep_generations: Some(1),
                keep_age: None,
            },
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();

        assert_generations_match_tables(store.writer(), "risk_snapshot", &tables);
        let gen1_bk000: i64 = store
            .writer()
            .query_row(
                "select count(*) from generations
                 where dataset = 'risk_snapshot' and batch = 'BK000' and gen_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            gen1_bk000, 1,
            "BK000's first generation survives via the underlying grain alone"
        );
        assert_eq!(
            generation_count(&store),
            3,
            "BK000 gen1 (underlying), BK000 gen3 and BK001 gen3 (position)"
        );
    }

    #[test]
    fn a_sweep_that_reconciles_nothing_still_leaves_the_summary_matching() {
        // An empty policy evicts no rows, so reconciliation should find
        // every summarised generation still present -- a regression check
        // that the reconciliation added alongside eviction does not
        // itself drop rows it should not.
        let (_d, store) = fixture();
        fill(&store, 5);
        let tables = POSITION_TABLES.map(String::from);
        rebuild_generations(store.writer(), "risk_snapshot", &tables).unwrap();

        sweep(
            store.writer(),
            "risk_snapshot",
            &[Grain::Position],
            &RetentionPolicy::default(),
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();

        assert_generations_match_tables(store.writer(), "risk_snapshot", &tables);
        assert_eq!(
            generation_count(&store),
            10,
            "nothing evicted, nothing reconciled away"
        );
    }
}
