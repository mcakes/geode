//! Archive retention by partition. Each `(batch, book)` retains its own
//! generations, so a busy book cannot evict a quiet book's history.
//!
//! Sweeps accept [`TablePair`] values: one per grain for measure datasets,
//! or the single document pair. The storage API supports both families; the
//! application schedules sweeps only for local document datasets, on the
//! ingest writer after each local publish (`ingest::LOCAL_KEEP_GENERATIONS`).

use crate::store::StoreError;
use crate::store::ddl::{TableKind, TablePair, table_pairs};
use chrono::{DateTime, Duration, Utc};
use duckdb::Connection;
use geode_core::schema::DatasetSpec;

#[derive(Debug, Clone, Default)]
pub struct RetentionPolicy {
    /// Keep this many archived generations per partition, excluding live.
    pub keep_generations: Option<usize>,
    /// Keep archived generations whose source time is within this window.
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
    /// Oldest source time remaining in the archive tables swept by this call.
    /// This excludes live tables and is not a completeness guarantee across
    /// partitions or unswept grains.
    pub oldest_remaining: Option<DateTime<Utc>>,
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// Delete summary entries only when no live or archive table in `pairs`
/// retains the generation. `reconcile_generations` supplies every pair owned by
/// the dataset, including pairs this sweep did not evict from. Otherwise a
/// generation surviving at one grain could become unreachable through as-of.
///
/// Compare all four identity fields with `is not distinct from`. Ordinary
/// equality cannot match NULL books or legacy NULL metadata and would delete
/// summary rows while their payload still exists. These table scans belong to
/// maintenance; requery resolution reads the resulting summary.
fn generations_reconcile_sql(dataset: &str, pairs: &[TablePair]) -> String {
    let escaped = dataset.replace('\'', "''");
    let checks: Vec<String> = pairs
        .iter()
        .flat_map(|p| [p.of(TableKind::Archive), p.of(TableKind::Live)])
        .map(|t| {
            format!(
                "not exists (
                     select 1 from {t} t
                     where t.batch is not distinct from g.batch
                       and t.book is not distinct from g.book
                       and t.gen_id is not distinct from g.gen_id
                       and t.source_time is not distinct from g.source_time
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

/// Reconcile against all table pairs declared by the dataset, independently
/// of the caller's eviction subset. A generation retained at any grain must
/// remain resolvable. Datasets with no table pairs require no reconciliation.
fn reconcile_generations(conn: &Connection, ds: &DatasetSpec) -> Result<(), StoreError> {
    let pairs = table_pairs(ds);
    if pairs.is_empty() {
        return Ok(());
    }
    let sql = generations_reconcile_sql(&ds.name, &pairs);
    conn.execute_batch(&sql).map_err(sql_err(&sql))
}

/// Evict archive generations from the supplied pairs, then reconcile the
/// summary against every pair the dataset owns. Measure and document datasets
/// use the same table-pair API.
///
/// Eviction, summary reconciliation, and report reads share one transaction.
/// Errors during the sweep roll back all earlier pair deletions. The connection
/// must not already have a transaction, and every declared live/archive pair
/// must exist even when only a subset is swept.
///
/// A caller using the ingest writer must serialize the whole sweep with
/// publication. Large sweeps delay writes for their duration; this function
/// does not schedule, split, or yield the work.
pub fn sweep(
    conn: &Connection,
    ds: &DatasetSpec,
    pairs: &[TablePair],
    policy: &RetentionPolicy,
    now: DateTime<Utc>,
) -> Result<SweepReport, StoreError> {
    conn.execute_batch("begin;").map_err(sql_err("begin"))?;
    match sweep_in_transaction(conn, ds, pairs, policy, now) {
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
/// per pair, then `reconcile_generations` once every pair has been swept.
fn sweep_in_transaction(
    conn: &Connection,
    ds: &DatasetSpec,
    pairs: &[TablePair],
    policy: &RetentionPolicy,
    now: DateTime<Utc>,
) -> Result<SweepReport, StoreError> {
    let mut report = SweepReport::default();

    for pair in pairs {
        let archive = pair.of(TableKind::Archive);

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

            // Use NULL-safe `NOT EXISTS`: a NULL book in a `NOT IN` subquery would
            // make comparisons unknown and prevent eviction. Rank each partition
            // by source time and then generation ID descending, matching as-of
            // resolution so a corrected republish wins a source-time tie.
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

    reconcile_generations(conn, ds)?;

    Ok(report)
}

/// Force a checkpoint. The caller must schedule it away from publication
/// because checkpointing can stall the writer; `sweep` does not call it.
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
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::schema::{Grain, SchemaSpec};

    /// `risk_snapshot` table pairs for the measure grains a test sweeps.
    fn pairs(grains: &[Grain]) -> Vec<TablePair> {
        grains
            .iter()
            .map(|g| TablePair::for_grain("risk_snapshot", *g))
            .collect()
    }

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// `risk_snapshot` with a single declared grain (Position) -- so
    /// `ds.grains()` names exactly the one grain `fixture()` creates
    /// tables for. Most tests here sweep Position alone and must not have
    /// the reconciliation reach for an Underlying table that does not
    /// exist.
    fn position_only_dataset() -> DatasetSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk_snapshot")
            .unwrap()
            .clone()
    }

    /// `risk_snapshot` declaring both Position and Underlying, for the
    /// tests that create both grains' tables.
    fn position_and_underlying_dataset() -> DatasetSpec {
        crate::store::ddl::tests_support::sample_dataset()
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
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
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
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
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
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
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
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
            &policy,
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.oldest_remaining.unwrap().hour(), 8);
    }

    #[test]
    fn the_oldest_remaining_bound_spans_every_grain_swept() {
        // `oldest_remaining` is the minimum source time across the swept archive
        // tables. Different per-grain minima distinguish this fold from a maximum;
        // it does not promise complete history for every partition.
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
            &position_and_underlying_dataset(),
            &pairs(&[Grain::Position, Grain::Underlying]),
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
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
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
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
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
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
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
        // For same-time corrections, keep-one retains the greatest generation
        // ID, matching the generation selected by as-of resolution.
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
                &position_only_dataset(),
                &pairs(&[Grain::Position]),
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
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
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
            &position_and_underlying_dataset(),
            &pairs(&[Grain::Position, Grain::Underlying]),
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
    fn reconciliation_covers_every_grain_the_dataset_has_not_just_the_swept_subset() {
        // Sweep only Position while the dataset also declares Underlying.
        // Reconciliation must preserve a generation that survives solely in
        // the unswept Underlying pair; checking only Position would lose its
        // as-of summary entry.
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

        // Only Position is swept -- `ds` (declaring both grains) is what
        // must protect Underlying's own copy during reconciliation.
        sweep(
            store.writer(),
            &position_and_underlying_dataset(),
            &pairs(&[Grain::Position]),
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
            "BK000's first generation survives via the unswept underlying grain"
        );
    }

    #[test]
    fn a_sweep_that_reconciles_nothing_still_leaves_the_summary_matching() {
        // An empty policy evicts no rows. Reconciliation must preserve every
        // summary entry whose generation remains in a live or archive table.
        let (_d, store) = fixture();
        fill(&store, 5);
        let tables = POSITION_TABLES.map(String::from);
        rebuild_generations(store.writer(), "risk_snapshot", &tables).unwrap();

        sweep(
            store.writer(),
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
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

    #[test]
    fn a_failed_sweep_rolls_back_both_the_archive_and_the_summary() {
        // Position eviction succeeds before the missing Underlying archive
        // causes a query error. Verify that rollback restores Position too,
        // so a failed sweep cannot commit only its earlier pairs.
        let (_d, store) = fixture();
        fill(&store, 3);
        let tables = POSITION_TABLES.map(String::from);
        rebuild_generations(store.writer(), "risk_snapshot", &tables).unwrap();
        let before_remaining = remaining(&store);
        let before_summary = generation_count(&store);

        let err = sweep(
            store.writer(),
            &position_and_underlying_dataset(),
            &pairs(&[Grain::Position, Grain::Underlying]),
            &RetentionPolicy {
                keep_generations: Some(1),
                keep_age: None,
            },
            ts("2026-08-31T00:00:00Z"),
        );
        assert!(
            err.is_err(),
            "the missing underlying table must fail the sweep"
        );

        assert_eq!(
            remaining(&store),
            before_remaining,
            "position's own eviction must be rolled back too"
        );
        assert_eq!(
            generation_count(&store),
            before_summary,
            "and so must the summary"
        );
    }

    #[test]
    fn sweep_then_checkpoint_both_succeed() {
        // A checkpoint immediately after a successful sweep must find no
        // transaction left open on the connection.
        let (_d, store) = fixture();
        fill(&store, 3);
        sweep(
            store.writer(),
            &position_only_dataset(),
            &pairs(&[Grain::Position]),
            &RetentionPolicy {
                keep_generations: Some(1),
                keep_age: None,
            },
            ts("2026-08-31T00:00:00Z"),
        )
        .unwrap();
        checkpoint(store.writer()).unwrap();
    }
}
