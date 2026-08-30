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
    pub book: String,
}

#[derive(Debug, Clone)]
pub struct PublishRequest {
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
fn partition_predicate(partitions: &[Partition]) -> String {
    let terms: Vec<String> = partitions
        .iter()
        .map(|p| {
            format!(
                "(batch = '{}' and book = '{}')",
                p.batch.replace('\'', "''"),
                p.book.replace('\'', "''")
            )
        })
        .collect();
    if terms.is_empty() {
        "false".to_string()
    } else {
        terms.join(" or ")
    }
}

pub fn publish_file(conn: &Connection, req: &PublishRequest) -> Result<PublishOutcome, StoreError> {
    let live = table_name(req.grain, TableKind::Live);
    let archive = table_name(req.grain, TableKind::Archive);
    let predicate = partition_predicate(&req.partitions);

    // The backfill guard: a file older than what is already live becomes
    // history directly. Without this a backfill would overwrite this
    // morning's risk with last Tuesday's.
    let superseded = req
        .live_source_time
        .is_some_and(|live_t| req.source_time <= live_t);

    let staged_rows: i64 = {
        let sql = format!("select count(*) from {}", req.staging_table);
        conn.query_row(&sql, [], |r| r.get(0))
            .map_err(sql_err(&sql))?
    };

    if superseded {
        let sql = format!(
            "insert into {archive} select *, {}, '{}'::timestamptz from {}",
            req.gen_id,
            req.source_time.to_rfc3339(),
            req.staging_table
        );
        conn.execute_batch(&sql).map_err(sql_err(&sql))?;
        return Ok(PublishOutcome::ArchivedOnly {
            rows: staged_rows as usize,
            reason: format!(
                "source time {} is not newer than the live generation",
                req.source_time.to_rfc3339()
            ),
        });
    }

    // One transaction: archive the outgoing rows, drop them from live,
    // insert the new ones. Any failure rolls the whole thing back, so a
    // failed load leaves live untouched (spec §5.7).
    let sql = format!(
        "begin;
         insert into {archive}
             select l.*, {gen}, '{time}'::timestamptz from {live} l where {predicate};
         delete from {live} where {predicate};
         insert into {live} select * from {staging};
         commit;",
        gen = req.gen_id,
        time = req.source_time.to_rfc3339(),
        staging = req.staging_table,
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
    use chrono::{DateTime, Utc};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Minimal live/archive pair so the test exercises the transaction, not
    /// DDL generation.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(
                "create table measures_position_live(
                     book varchar, position_ref varchar, daily_trading_pnl double,
                     batch varchar, source_file_id bigint);
                 create table measures_position_archive(
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
            grain: Grain::Position,
            staging_table: "staging_position".into(),
            partitions: vec![Partition {
                batch: batch.into(),
                book: book.into(),
            }],
            gen_id: generation,
            source_time: t,
            live_source_time: None,
        }
    }

    fn live_rows(store: &Store) -> Vec<(String, f64)> {
        let conn = store.writer();
        let mut stmt = conn
            .prepare("select book, daily_trading_pnl from measures_position_live order by book")
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
        assert_eq!(count(&store, "measures_position_archive"), 0);
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
        let archived: f64 = store
            .writer()
            .query_row(
                "select daily_trading_pnl from measures_position_archive",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(archived, 10.0, "the superseded rows moved to archive");
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
            count(&store, "measures_position_archive"),
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
            book: "BK002".into(),
        });
        publish_file(store.writer(), &req).unwrap();
        assert_eq!(live_rows(&store).len(), 2);
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
}
