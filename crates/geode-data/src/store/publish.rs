//! The per-file publish transaction (spec §4.3) and the backfill guard
//! (spec §4.4).
//!
//! Live holds exactly one generation per partition after any sequence of
//! publishes in any order. That invariant is what makes live's size
//! independent of retention, which is what keeps the §7.1 requery budget
//! reachable by construction rather than by tuning.

use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
use chrono::{DateTime, Utc};
use duckdb::Connection;
use geode_core::schema::Grain;

/// The unit of replacement: a batch within a book (spec §4.3). Not the file
/// — filenames carry dates, so file identity is not partition identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub batch: String,
    /// `None` is the partition of rows with no book. Ingest keeps such
    /// rows and reports them (spec §4.4), so they are live data and must
    /// be replaced on republish like any other partition — otherwise every
    /// republish appends another copy and live no longer holds one
    /// generation per partition.
    pub book: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PublishRequest {
    /// Table names carry the dataset (spec §4.2), so the request must
    /// name it: two datasets can share a grain.
    pub dataset: String,
    pub grain: Grain,
    pub staging_table: String,
    pub partitions: Vec<Partition>,
    pub gen_id: i64,
    pub source_time: DateTime<Utc>,
    /// The newest source time already live for these partitions, from
    /// `Catalog::live_source_time`. `None` means nothing is live yet.
    pub live_source_time: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    Published {
        rows: usize,
    },
    /// The file was older than what is live, so it became history without
    /// ever being current.
    ArchivedOnly {
        rows: usize,
        reason: String,
    },
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// A `(batch, book) IN (…)` predicate with the values inlined as quoted
/// literals. Safe because both come from the catalog and the sentinel, not
/// from user input; scope predicates, which do take user input, bind
/// instead (spec §6.2).
///
/// `book is null` for the bookless partition: `book = '…'` matches no
/// NULL, so that partition would never be deleted and would accumulate a
/// copy per republish.
fn partition_predicate(partitions: &[Partition]) -> String {
    let terms: Vec<String> = partitions
        .iter()
        .map(|p| {
            let book = match &p.book {
                Some(b) => format!("book = '{}'", b.replace('\'', "''")),
                None => "book is null".to_string(),
            };
            format!("(batch = '{}' and {book})", p.batch.replace('\'', "''"))
        })
        .collect();
    if terms.is_empty() {
        "false".to_string()
    } else {
        terms.join(" or ")
    }
}

/// The `generations` summary insert for one publish: one `values` row per
/// `req.partitions` entry, guarded by `where not exists` so it is
/// idempotent across the grains that call `publish_file` for the same
/// file -- every grain names the identical (dataset, batch, book, gen_id,
/// source_time) tuple, and only the first grain's call actually inserts
/// it. Literals are quoted the same way `partition_predicate` quotes
/// them: these values come from the catalog and the sentinel, not user
/// input.
fn generation_summary_insert(req: &PublishRequest) -> String {
    let dataset = req.dataset.replace('\'', "''");
    let time = req.source_time.to_rfc3339();
    let rows = req
        .partitions
        .iter()
        .map(|p| {
            let book = match &p.book {
                Some(b) => format!("'{}'", b.replace('\'', "''")),
                None => "NULL::varchar".to_string(),
            };
            format!(
                "('{dataset}', '{}', {book}, {}::bigint, '{time}'::timestamptz)",
                p.batch.replace('\'', "''"),
                req.gen_id,
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "insert into generations
         select v.* from (values {rows}) v(dataset, batch, book, gen_id, source_time)
         where not exists (
             select 1 from generations g
             where g.dataset = v.dataset and g.batch = v.batch
               and g.book is not distinct from v.book
               and g.gen_id = v.gen_id and g.source_time = v.source_time
         );"
    )
}

pub fn publish_file(conn: &Connection, req: &PublishRequest) -> Result<PublishOutcome, StoreError> {
    let live = table_name(&req.dataset, req.grain, TableKind::Live);
    let archive = table_name(&req.dataset, req.grain, TableKind::Archive);
    let predicate = partition_predicate(&req.partitions);

    // Publishing with no partitions would delete nothing and insert
    // everything, so live would accumulate a duplicate copy on every
    // republish. That is a caller bug, not a degradation.
    if req.partitions.is_empty() {
        return Err(StoreError::Sql {
            statement: format!("publish into {live}"),
            source: duckdb::Error::InvalidParameterName(
                "publish requires at least one partition; an empty set would \
                 append to live without replacing anything"
                    .into(),
            ),
        });
    }

    // The backfill guard: a file *older* than what is already live becomes
    // history directly. Without this a backfill would overwrite this
    // morning's risk with last Tuesday's.
    //
    // Strictly older, deliberately. Discovery only queues a file whose
    // (size, source_time) differs from what was loaded, so a file arriving
    // with a source time equal to the live one is a *corrected* republish
    // of the same generation — it must replace, not be filed as history.
    let superseded = req
        .live_source_time
        .is_some_and(|live_t| req.source_time < live_t);

    let staged_rows: i64 = {
        let sql = format!("select count(*) from {}", req.staging_table);
        conn.query_row(&sql, [], |r| r.get(0))
            .map_err(sql_err(&sql))?
    };

    if superseded {
        // In its own transaction, same as the normal branch below: the
        // generation must not be recorded in the summary unless the
        // archive insert it describes actually committed.
        let sql = format!(
            "begin;
             insert into {archive} select *, {}, '{}'::timestamptz from {};
             {summary}
             commit;",
            req.gen_id,
            req.source_time.to_rfc3339(),
            req.staging_table,
            summary = generation_summary_insert(req),
        );
        if let Err(source) = conn.execute_batch(&sql) {
            let _ = conn.execute_batch("rollback;");
            return Err(StoreError::Sql {
                statement: sql,
                source,
            });
        }
        return Ok(PublishOutcome::ArchivedOnly {
            rows: staged_rows as usize,
            reason: format!(
                "source time {} is not newer than the live generation",
                req.source_time.to_rfc3339()
            ),
        });
    }

    // One transaction: archive the outgoing rows, drop them from live,
    // insert the new ones, and record the generation in the summary. Any
    // failure rolls the whole thing back, so a failed load leaves live
    // untouched (spec §5.7) *and* the summary unrecorded -- it must never
    // claim a generation that did not actually land.
    //
    // The outgoing rows move with `select *` — keeping the `gen_id` and
    // `source_time` they carried while live. Stamping them with the
    // incoming generation instead would make as-of to any moment when the
    // older generation was live return nothing (spec §4.4).
    let sql = format!(
        "begin;
         insert into {archive} select * from {live} where {predicate};
         delete from {live} where {predicate};
         insert into {live}
             select *, {gen}, '{time}'::timestamptz from {staging};
         {summary}
         commit;",
        gen = req.gen_id,
        time = req.source_time.to_rfc3339(),
        staging = req.staging_table,
        summary = generation_summary_insert(req),
    );
    if let Err(source) = conn.execute_batch(&sql) {
        let _ = conn.execute_batch("rollback;");
        return Err(StoreError::Sql {
            statement: sql,
            source,
        });
    }

    Ok(PublishOutcome::Published {
        rows: staged_rows as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::assert_generations_match_tables;
    use chrono::{DateTime, Utc};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// This dataset's whole history, for the generations-summary oracle.
    const POSITION_TABLES: [&str; 2] = [
        "risk_snapshot_position_live",
        "risk_snapshot_position_archive",
    ];

    fn assert_summary_matches(store: &Store) {
        assert_generations_match_tables(
            store.writer(),
            "risk_snapshot",
            &POSITION_TABLES.map(String::from),
        );
    }

    /// Minimal live/archive pair so the test exercises the transaction, not
    /// DDL generation.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_position_live(
                     book varchar, position_ref varchar, daily_trading_pnl double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 create table risk_snapshot_position_archive(
                     book varchar, position_ref varchar, daily_trading_pnl double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 create table staging_position(
                     book varchar, position_ref varchar, daily_trading_pnl double,
                     batch varchar, source_file_id bigint);",
            )
            .unwrap();
        (dir, store)
    }

    fn stage(store: &Store, book: &str, pnl: f64, batch: &str, file_id: i64) {
        store
            .writer()
            .execute(
                "insert into staging_position values (?, 'POS1', ?, ?, ?)",
                duckdb::params![book, pnl, batch, file_id],
            )
            .unwrap();
    }

    fn request(batch: &str, book: &str, generation: i64, t: DateTime<Utc>) -> PublishRequest {
        PublishRequest {
            dataset: "risk_snapshot".into(),
            grain: Grain::Position,
            staging_table: "staging_position".into(),
            partitions: vec![Partition {
                batch: batch.into(),
                book: Some(book.into()),
            }],
            gen_id: generation,
            source_time: t,
            live_source_time: None,
        }
    }

    fn live_rows(store: &Store) -> Vec<(String, f64)> {
        let conn = store.writer();
        let mut stmt = conn
            .prepare(
                "select book, daily_trading_pnl from risk_snapshot_position_live order by book",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn count(store: &Store, table: &str) -> i64 {
        store
            .writer()
            .query_row(&format!("select count(*) from {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn first_publish_inserts_into_live_and_leaves_archive_empty() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        let out = publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        assert!(matches!(out, PublishOutcome::Published { rows: 1 }));
        assert_eq!(live_rows(&store), vec![("BK000".to_string(), 10.0)]);
        assert_eq!(count(&store, "risk_snapshot_position_archive"), 0);
    }

    #[test]
    fn republishing_a_partition_replaces_it_and_archives_the_old_rows() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-29T07:00:00Z")),
        )
        .unwrap();
        store
            .writer()
            .execute_batch("delete from staging_position")
            .unwrap();
        stage(&store, "BK000", 99.0, "BK000", 2);

        let mut req = request("BK000", "BK000", 2, ts("2026-08-30T07:00:00Z"));
        req.live_source_time = Some(ts("2026-08-29T07:00:00Z"));
        publish_file(store.writer(), &req).unwrap();

        assert_eq!(
            live_rows(&store),
            vec![("BK000".to_string(), 99.0)],
            "live holds one generation"
        );
        let (archived, gen_id, stamp): (f64, i64, DateTime<Utc>) = store
            .writer()
            .query_row(
                "select daily_trading_pnl, gen_id, source_time
                 from risk_snapshot_position_archive",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(archived, 10.0, "the superseded rows moved to archive");
        // The archived rows must keep the generation they had while live.
        // Stamping them with the incoming generation would make as-of to a
        // time when they *were* live return nothing (spec §4.4).
        assert_eq!(gen_id, 1, "archived rows keep their own gen_id");
        assert_eq!(
            stamp,
            ts("2026-08-29T07:00:00Z"),
            "archived rows keep their own source_time, not the successor's"
        );
    }

    #[test]
    fn a_corrected_republish_at_the_same_source_time_replaces_live() {
        // Discovery only queues a file whose (size, source_time) differs
        // from what was loaded, so a file arriving with the *same* source
        // time is a correction of that generation and must replace it.
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        store
            .writer()
            .execute_batch("delete from staging_position")
            .unwrap();
        stage(&store, "BK000", 42.0, "BK000", 2);

        let mut req = request("BK000", "BK000", 2, ts("2026-08-30T07:00:00Z"));
        req.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        let out = publish_file(store.writer(), &req).unwrap();

        assert!(matches!(out, PublishOutcome::Published { .. }), "{out:?}");
        assert_eq!(
            live_rows(&store),
            vec![("BK000".to_string(), 42.0)],
            "the correction must reach live, not be filed as history"
        );
    }

    #[test]
    fn publishing_with_no_partitions_is_rejected() {
        // An empty partition set deletes nothing and inserts everything, so
        // live would accumulate a duplicate copy on every republish.
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        let mut req = request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z"));
        req.partitions.clear();
        assert!(publish_file(store.writer(), &req).is_err());
        assert!(live_rows(&store).is_empty(), "nothing may reach live");
    }

    #[test]
    fn a_backfilled_older_file_never_reaches_live() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 99.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        store
            .writer()
            .execute_batch("delete from staging_position")
            .unwrap();

        // Last Tuesday's file, loaded after this morning's.
        stage(&store, "BK000", 10.0, "BK000", 2);
        let mut req = request("BK000", "BK000", 2, ts("2026-08-25T07:00:00Z"));
        req.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        let out = publish_file(store.writer(), &req).unwrap();

        assert!(
            matches!(out, PublishOutcome::ArchivedOnly { .. }),
            "{out:?}"
        );
        assert_eq!(
            live_rows(&store),
            vec![("BK000".to_string(), 99.0)],
            "this morning's risk must survive a backfill"
        );
        assert_eq!(
            count(&store, "risk_snapshot_position_archive"),
            1,
            "the old generation is history, not live"
        );
    }

    #[test]
    fn publishing_one_partition_leaves_its_siblings_alone() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000_part1", 1);
        publish_file(
            store.writer(),
            &request("BK000_part1", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        store
            .writer()
            .execute_batch("delete from staging_position")
            .unwrap();

        // Same book, different batch: the split file's other half.
        stage(&store, "BK000", 20.0, "BK000_part2", 2);
        publish_file(
            store.writer(),
            &request("BK000_part2", "BK000", 2, ts("2026-08-30T08:00:00Z")),
        )
        .unwrap();

        let rows = live_rows(&store);
        assert_eq!(
            rows.len(),
            2,
            "a split book keeps both halves live: {rows:?}"
        );
    }

    #[test]
    fn a_multi_book_file_replaces_every_partition_it_covers() {
        let (_d, store) = fixture();
        stage(&store, "BK001", 1.0, "BK001_BK002", 1);
        stage(&store, "BK002", 2.0, "BK001_BK002", 1);
        let mut req = request("BK001_BK002", "BK001", 1, ts("2026-08-30T07:00:00Z"));
        req.partitions.push(Partition {
            batch: "BK001_BK002".into(),
            book: Some("BK002".into()),
        });
        publish_file(store.writer(), &req).unwrap();
        assert_eq!(live_rows(&store).len(), 2);
    }

    #[test]
    fn the_bookless_partition_is_replaced_like_any_other() {
        // Rows with no book are kept and reported, not dropped, so they
        // are live data. `book = '…'` never matched them, so every
        // republish appended another copy: live no longer held one
        // generation per partition, and the total drifted upward.
        let (_d, store) = fixture();
        let publish = |generation: i64, t: &str, live_t: Option<&str>| {
            store
                .writer()
                .execute_batch("delete from staging_position")
                .unwrap();
            stage(&store, "BK000", 10.0, "BK000", generation);
            store
                .writer()
                .execute(
                    "insert into staging_position values (NULL, 'POS9', ?, 'BK000', ?)",
                    duckdb::params![3.0, generation],
                )
                .unwrap();
            let mut req = request("BK000", "BK000", generation, ts(t));
            req.partitions.push(Partition {
                batch: "BK000".into(),
                book: None,
            });
            req.live_source_time = live_t.map(ts);
            publish_file(store.writer(), &req).unwrap();
        };
        publish(1, "2026-08-30T07:00:00Z", None);
        publish(2, "2026-08-30T08:00:00Z", Some("2026-08-30T07:00:00Z"));
        assert_eq!(
            count(&store, "risk_snapshot_position_live"),
            2,
            "one generation per partition, the bookless one included"
        );
        assert_eq!(
            count(&store, "risk_snapshot_position_archive"),
            2,
            "and the outgoing bookless rows went to history"
        );
    }

    #[test]
    fn a_failed_publish_leaves_live_untouched() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();

        let mut bad = request("BK000", "BK000", 2, ts("2026-08-31T07:00:00Z"));
        bad.staging_table = "no_such_table".into();
        bad.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        assert!(publish_file(store.writer(), &bad).is_err());

        assert_eq!(
            live_rows(&store),
            vec![("BK000".to_string(), 10.0)],
            "a failed load never clobbers the last good generation (spec §5.7)"
        );
    }

    fn generation_row_count(store: &Store) -> i64 {
        store
            .writer()
            .query_row(
                "select count(*) from generations where dataset = 'risk_snapshot'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_normal_publish_records_the_generation_in_the_summary() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        assert_summary_matches(&store);
        assert_eq!(generation_row_count(&store), 1);
    }

    #[test]
    fn a_republish_leaves_the_summary_matching_the_archived_and_live_generations() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-29T07:00:00Z")),
        )
        .unwrap();
        store
            .writer()
            .execute_batch("delete from staging_position")
            .unwrap();
        stage(&store, "BK000", 99.0, "BK000", 2);
        let mut req = request("BK000", "BK000", 2, ts("2026-08-30T07:00:00Z"));
        req.live_source_time = Some(ts("2026-08-29T07:00:00Z"));
        publish_file(store.writer(), &req).unwrap();

        assert_summary_matches(&store);
        assert_eq!(generation_row_count(&store), 2, "both generations recorded");
    }

    #[test]
    fn an_archived_only_publish_records_the_generation_too() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 99.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        store
            .writer()
            .execute_batch("delete from staging_position")
            .unwrap();
        stage(&store, "BK000", 10.0, "BK000", 2);
        let mut req = request("BK000", "BK000", 2, ts("2026-08-25T07:00:00Z"));
        req.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        let out = publish_file(store.writer(), &req).unwrap();
        assert!(matches!(out, PublishOutcome::ArchivedOnly { .. }));

        assert_summary_matches(&store);
        let recorded: i64 = store
            .writer()
            .query_row(
                "select count(*) from generations
                 where dataset = 'risk_snapshot' and batch = 'BK000'
                   and gen_id = 2 and source_time = '2026-08-25T07:00:00Z'::timestamptz",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded, 1, "the archived-only generation must be recorded");
    }

    #[test]
    fn a_failed_publish_leaves_no_summary_row() {
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();

        let mut bad = request("BK000", "BK000", 2, ts("2026-08-31T07:00:00Z"));
        bad.staging_table = "no_such_table".into();
        bad.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        assert!(publish_file(store.writer(), &bad).is_err());

        assert_summary_matches(&store);
        assert_eq!(
            generation_row_count(&store),
            1,
            "the failed publish's rollback must cover the summary insert too"
        );
    }

    /// A second grain's live/archive pair, publishing the same file's other
    /// grain under the identical generation.
    fn add_underlying_tables(store: &Store) {
        store
            .writer()
            .execute_batch(
                "create table risk_snapshot_underlying_live(
                     book varchar, position_ref varchar, delta01 double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 create table risk_snapshot_underlying_archive(
                     book varchar, position_ref varchar, delta01 double,
                     batch varchar, source_file_id bigint,
                     gen_id bigint, source_time timestamp with time zone);
                 create table staging_underlying(
                     book varchar, position_ref varchar, delta01 double,
                     batch varchar, source_file_id bigint);",
            )
            .unwrap();
    }

    fn underlying_request(
        batch: &str,
        book: Option<&str>,
        generation: i64,
        t: DateTime<Utc>,
    ) -> PublishRequest {
        PublishRequest {
            dataset: "risk_snapshot".into(),
            grain: Grain::Underlying,
            staging_table: "staging_underlying".into(),
            partitions: vec![Partition {
                batch: batch.into(),
                book: book.map(String::from),
            }],
            gen_id: generation,
            source_time: t,
            live_source_time: None,
        }
    }

    #[test]
    fn publishing_the_same_files_second_grain_does_not_duplicate_the_summary_row() {
        let (_d, store) = fixture();
        add_underlying_tables(&store);
        stage(&store, "BK000", 10.0, "BK000", 1);
        store
            .writer()
            .execute(
                "insert into staging_underlying values ('BK000', 'POS1', 5.0, 'BK000', 1)",
                [],
            )
            .unwrap();

        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        publish_file(
            store.writer(),
            &underlying_request("BK000", Some("BK000"), 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();

        let tables = [
            "risk_snapshot_position_live",
            "risk_snapshot_position_archive",
            "risk_snapshot_underlying_live",
            "risk_snapshot_underlying_archive",
        ]
        .map(String::from);
        assert_generations_match_tables(store.writer(), "risk_snapshot", &tables);
        assert_eq!(
            generation_row_count(&store),
            1,
            "one file publishing two grains under the same generation is one row"
        );
    }

    #[test]
    fn a_null_book_partition_round_trips_with_no_duplicate_on_the_second_grain() {
        let (_d, store) = fixture();
        add_underlying_tables(&store);
        store
            .writer()
            .execute(
                "insert into staging_position values (NULL, 'POS9', 3.0, 'BK000', 1)",
                [],
            )
            .unwrap();
        store
            .writer()
            .execute(
                "insert into staging_underlying values (NULL, 'POS9', 5.0, 'BK000', 1)",
                [],
            )
            .unwrap();

        publish_file(
            store.writer(),
            &underlying_request("BK000", None, 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        let mut pos_req = request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z"));
        pos_req.partitions = vec![Partition {
            batch: "BK000".into(),
            book: None,
        }];
        publish_file(store.writer(), &pos_req).unwrap();

        let tables = [
            "risk_snapshot_position_live",
            "risk_snapshot_position_archive",
            "risk_snapshot_underlying_live",
            "risk_snapshot_underlying_archive",
        ]
        .map(String::from);
        assert_generations_match_tables(store.writer(), "risk_snapshot", &tables);
        assert_eq!(
            generation_row_count(&store),
            1,
            "no duplicate on the second grain"
        );
        let book: Option<String> = store
            .writer()
            .query_row(
                "select book from generations where dataset = 'risk_snapshot'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(book, None, "the bookless partition must be present");
    }
}
