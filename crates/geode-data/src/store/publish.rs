//! Transactional publication and backfill routing for live/archive table pairs.
//! Live holds one generation per partition regardless of publication order,
//! keeping live query size independent of retained history. File and document
//! loaders can publish multiple pairs in their shared transaction.

use crate::store::StoreError;
use crate::store::ddl::TablePair;
use chrono::{DateTime, Utc};
use duckdb::Connection;

/// The replacement key within a dataset: batch and optional book. File names
/// include dates, so file identity alone cannot identify the partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub batch: String,
    /// Rows without a book form a real partition. NULL-safe replacement must
    /// remove their outgoing generation too, or each republish duplicates it.
    pub book: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PublishRequest {
    /// The dataset the published rows belong to. It names the summary
    /// row (`generations`), not a table: table names live in `tables`.
    pub dataset: String,
    /// Dataset-owned live/archive tables: a grain pair for measures or the
    /// single document pair. Resolve names with the dataset as well as the
    /// grain, because different datasets can share a grain.
    pub tables: TablePair,
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

/// Select partitions using escaped catalog/sentinel literals. User-supplied
/// scope values use bound parameters elsewhere. Match bookless partitions
/// with `book is null`; ordinary equality would leave their outgoing rows
/// in live and duplicate them on republish.
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

/// Insert one summary row per distinct partition and generation identity.
/// The `where not exists` guard makes repeated calls for different grains of
/// the same file idempotent. Literals are escaped as in `partition_predicate`.
///
/// `select distinct` also removes repeated partitions within a request: every
/// row in the VALUES list checks the summary as of statement start, so the
/// existence guard alone cannot deduplicate that list.
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
         select distinct v.* from (values {rows}) v(dataset, batch, book, gen_id, source_time)
         where not exists (
             select 1 from generations g
             where g.dataset = v.dataset and g.batch = v.batch
               and g.book is not distinct from v.book
               and g.gen_id = v.gen_id and g.source_time = v.source_time
         );"
    )
}

pub fn publish_file(conn: &Connection, req: &PublishRequest) -> Result<PublishOutcome, StoreError> {
    let tx = super::begin_transaction(conn)?;
    let outcome = publish_in_transaction(&tx, req)?;
    super::commit_transaction(tx)?;
    Ok(outcome)
}

/// Publish one grain inside its owner's complete file/document transaction.
/// Requiring a transaction prevents accidentally committing a partial file.
pub(crate) fn publish_in_transaction(
    conn: &duckdb::Transaction<'_>,
    req: &PublishRequest,
) -> Result<PublishOutcome, StoreError> {
    let live = &req.tables.live;
    let archive = &req.tables.archive;
    // Payload tables and generation summary must name the same dataset.
    // A mismatch makes stored rows unreachable through the summary. Both
    // table families prefix names with the dataset, checked here in debug.
    debug_assert!(
        live.starts_with(&format!("{}_", req.dataset))
            && archive.starts_with(&format!("{}_", req.dataset)),
        "publish request names dataset '{}' but tables {live}/{archive}",
        req.dataset,
    );
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

    // A strictly older source time routes the incoming generation directly to
    // archive. Equal source times replace live too, allowing corrected republishes;
    // their distinct generation IDs preserve both versions in history.
    let superseded = req
        .live_source_time
        .is_some_and(|live_t| req.source_time < live_t);

    let staged_rows: i64 = {
        let sql = format!("select count(*) from {}", req.staging_table);
        conn.query_row(&sql, [], |r| r.get(0))
            .map_err(sql_err(&sql))?
    };

    if superseded {
        // In the caller's transaction, same as the normal branch below: the
        // generation must not be recorded in the summary unless the
        // archive insert it describes actually committed.
        let sql = format!(
            "insert into {archive} select *, {}, '{}'::timestamptz from {};
             {summary}",
            req.gen_id,
            req.source_time.to_rfc3339(),
            req.staging_table,
            summary = generation_summary_insert(req),
        );
        if let Err(source) = conn.execute_batch(&sql) {
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

    // Archive outgoing rows, replace live, and record the summary in the
    // caller's transaction. Any failure must roll back all three so the
    // summary cannot claim a generation whose payload did not land.
    //
    // Moving outgoing rows with `select *` preserves their generation ID
    // and source time, keeping their historical identity intact.
    let sql = format!(
        "insert into {archive} select * from {live} where {predicate};
         delete from {live} where {predicate};
         insert into {live}
             select *, {gen}, '{time}'::timestamptz from {staging};
         {summary}",
        gen = req.gen_id,
        time = req.source_time.to_rfc3339(),
        staging = req.staging_table,
        summary = generation_summary_insert(req),
    );
    if let Err(source) = conn.execute_batch(&sql) {
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
    use geode_core::schema::Grain;

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

    /// A staging table with an extra column. Counting its rows succeeds, but
    /// inserting into live or archive fails inside the publication transaction.
    /// This exercises rollback after earlier statements have run.
    fn stage_with_a_mismatched_column_count(
        store: &Store,
        book: &str,
        pnl: f64,
        batch: &str,
        file_id: i64,
    ) -> &'static str {
        store
            .writer()
            .execute_batch(
                "create table if not exists staging_position_bad(
                     book varchar, position_ref varchar, daily_trading_pnl double,
                     batch varchar, source_file_id bigint, extra_col double);",
            )
            .unwrap();
        store
            .writer()
            .execute(
                "insert into staging_position_bad values (?, 'POS1', ?, ?, ?, 0.0)",
                duckdb::params![book, pnl, batch, file_id],
            )
            .unwrap();
        "staging_position_bad"
    }

    fn request(batch: &str, book: &str, generation: i64, t: DateTime<Utc>) -> PublishRequest {
        PublishRequest {
            dataset: "risk_snapshot".into(),
            tables: TablePair::for_grain("risk_snapshot", Grain::Position),
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
        // Archived rows retain their original generation ID and source time
        // so queries for the period when they were live can still find them.
        assert_eq!(gen_id, 1, "archived rows keep their own gen_id");
        assert_eq!(
            stamp,
            ts("2026-08-29T07:00:00Z"),
            "archived rows keep their own source_time, not the successor's"
        );
    }

    #[test]
    fn a_corrected_republish_at_the_same_source_time_replaces_live() {
        // A correction with the same source time must replace live under its new
        // generation ID, preserving the outgoing version in archive.
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
        // Rows with no book form their own partition. Replacement must match NULL
        // books explicitly to avoid accumulating duplicate live rows.
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
        // A staging column-count mismatch fails during the live insert, after
        // outgoing rows have moved. This exercises rollback of partial work;
        // a missing staging table would fail earlier at the row-count query.
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();

        let mut bad = request("BK000", "BK000", 2, ts("2026-08-31T07:00:00Z"));
        bad.staging_table =
            stage_with_a_mismatched_column_count(&store, "BK000", 42.0, "BK000", 2).into();
        bad.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        assert!(publish_file(store.writer(), &bad).is_err());

        assert_eq!(
            live_rows(&store),
            vec![("BK000".to_string(), 10.0)],
            "a failed load never clobbers the last good generation"
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
        // Fail at the live insert with a staging column-count mismatch.
        // The transaction has already moved outgoing rows, so this verifies
        // rollback keeps payload and summary consistent.
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();

        let mut bad = request("BK000", "BK000", 2, ts("2026-08-31T07:00:00Z"));
        bad.staging_table =
            stage_with_a_mismatched_column_count(&store, "BK000", 42.0, "BK000", 2).into();
        bad.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        assert!(publish_file(store.writer(), &bad).is_err());

        assert_summary_matches(&store);
        assert_eq!(
            generation_row_count(&store),
            1,
            "the failed publish's rollback must cover the summary insert too"
        );
    }

    #[test]
    fn a_failed_archived_only_publish_leaves_no_summary_row() {
        // A column-count mismatch in the archive insert must leave neither
        // partial archive rows nor a summary entry for the failed backfill.
        let (_d, store) = fixture();
        stage(&store, "BK000", 10.0, "BK000", 1);
        publish_file(
            store.writer(),
            &request("BK000", "BK000", 1, ts("2026-08-30T07:00:00Z")),
        )
        .unwrap();
        let before_archive = count(&store, "risk_snapshot_position_archive");

        // Older than live, so this takes the `superseded` (archived-only)
        // branch rather than the normal one.
        let mut bad = request("BK000", "BK000", 2, ts("2026-08-25T07:00:00Z"));
        bad.staging_table =
            stage_with_a_mismatched_column_count(&store, "BK000", 42.0, "BK000", 2).into();
        bad.live_source_time = Some(ts("2026-08-30T07:00:00Z"));
        assert!(publish_file(store.writer(), &bad).is_err());

        assert_eq!(
            count(&store, "risk_snapshot_position_archive"),
            before_archive,
            "the failed archive insert must not partially land"
        );
        assert_summary_matches(&store);
        assert_eq!(
            generation_row_count(&store),
            1,
            "the failed generation 2 must not appear in the summary"
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
            tables: TablePair::for_grain("risk_snapshot", Grain::Underlying),
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
