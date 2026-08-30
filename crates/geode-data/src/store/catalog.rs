//! Freshness bookkeeping (spec §4.5). Records what was loaded from where
//! and when, in *source* time — never mtime, which any copy or restore
//! corrupts (spec §4.4).
//!
//! Freshness rolls up: a book is as fresh as its stalest contributing file,
//! and a dataset's headline as-of is the oldest book in the effective
//! scope. That is the same stalest-input rule joins use (spec §5.4).

use crate::health::Health;
use crate::store::StoreError;
use chrono::{DateTime, Utc};
use duckdb::Connection;
use std::path::{Path, PathBuf};

pub type FileId = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileGeneration {
    pub file_id: FileId,
    pub dataset: String,
    /// Filename with its date component removed: the partition's identity
    /// across business dates (spec §4.3).
    pub batch: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: DateTime<Utc>,
    /// From the sentinel. Orders generations and drives as-of.
    pub source_time: DateTime<Utc>,
    pub gen_id: i64,
    pub loaded_at: DateTime<Utc>,
    pub row_count: usize,
    pub books: Vec<String>,
    pub health: Health,
}

pub struct Catalog<'a> {
    conn: &'a Connection,
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS file_generations (
  file_id BIGINT PRIMARY KEY,
  dataset VARCHAR,
  batch VARCHAR,
  path VARCHAR,
  size BIGINT,
  mtime TIMESTAMP WITH TIME ZONE,
  source_time TIMESTAMP WITH TIME ZONE,
  gen_id BIGINT,
  loaded_at TIMESTAMP WITH TIME ZONE,
  row_count BIGINT,
  health VARCHAR,
  health_reason VARCHAR
);
CREATE SEQUENCE IF NOT EXISTS file_generations_id START 1;
CREATE TABLE IF NOT EXISTS file_books (
  file_id BIGINT,
  book VARCHAR
);
";

impl<'a> Catalog<'a> {
    pub fn new(conn: &'a Connection) -> Catalog<'a> {
        Catalog { conn }
    }

    fn sql(&self, statement: &str) -> Result<(), StoreError> {
        self.conn
            .execute_batch(statement)
            .map_err(|source| StoreError::Sql {
                statement: statement.to_string(),
                source,
            })
    }

    pub fn ensure_tables(&self) -> Result<(), StoreError> {
        self.sql(DDL)
    }

    /// Reserve a file id *before* loading, so the `source_file_id` stamped
    /// onto every data row matches the catalog entry recorded afterwards.
    /// Without this the two are assigned independently and provenance joins
    /// silently return nothing.
    pub fn reserve_file_id(&self) -> Result<FileId, StoreError> {
        let sql = "select nextval('file_generations_id')";
        self.conn
            .query_row(sql, [], |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// Peek at the next generation id without consuming it.
    pub fn next_gen_id(&self) -> Result<i64, StoreError> {
        let sql = "select coalesce(max(gen_id), 0) + 1 from file_generations";
        self.conn
            .query_row(sql, [], |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// `rec.file_id` of 0 means "assign one"; any other value is used as
    /// given, which is how a load stamps its rows and its catalog entry with
    /// the same id (see [`Catalog::reserve_file_id`]).
    pub fn record(&self, rec: &FileGeneration) -> Result<FileId, StoreError> {
        let (health, reason) = rec.health.to_parts();
        let sql = "insert into file_generations
                   select case when ? = 0 then nextval('file_generations_id') else ? end,
                          ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
                   returning file_id";
        let file_id: i64 = self
            .conn
            .query_row(
                sql,
                duckdb::params![
                    rec.file_id,
                    rec.file_id,
                    rec.dataset,
                    rec.batch,
                    rec.path.to_string_lossy().to_string(),
                    rec.size as i64,
                    rec.mtime,
                    rec.source_time,
                    rec.gen_id,
                    rec.loaded_at,
                    rec.row_count as i64,
                    health,
                    reason,
                ],
                |r| r.get(0),
            )
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })?;

        for book in &rec.books {
            let sql = "insert into file_books values (?, ?)";
            self.conn
                .execute(sql, duckdb::params![file_id, book])
                .map_err(|source| StoreError::Sql {
                    statement: sql.into(),
                    source,
                })?;
        }
        Ok(file_id)
    }

    pub fn lookup_by_path(&self, path: &Path) -> Result<Option<FileGeneration>, StoreError> {
        let sql = "select file_id, dataset, batch, path, size, mtime, source_time, gen_id,
                          loaded_at, row_count, health, health_reason
                   from file_generations where path = ? order by gen_id desc limit 1";
        let err = |source| StoreError::Sql {
            statement: sql.to_string(),
            source,
        };
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        let mut rows = stmt
            .query(duckdb::params![path.to_string_lossy().to_string()])
            .map_err(err)?;
        let Some(row) = rows.next().map_err(err)? else {
            return Ok(None);
        };
        let file_id: i64 = row.get(0).unwrap();
        let health_label: String = row.get(10).unwrap();
        let health_reason: Option<String> = row.get(11).unwrap();
        let found = FileGeneration {
            file_id,
            dataset: row.get(1).unwrap(),
            batch: row.get(2).unwrap(),
            path: PathBuf::from(row.get::<_, String>(3).unwrap()),
            size: row.get::<_, i64>(4).unwrap() as u64,
            mtime: row.get(5).unwrap(),
            source_time: row.get(6).unwrap(),
            gen_id: row.get(7).unwrap(),
            loaded_at: row.get(8).unwrap(),
            row_count: row.get::<_, i64>(9).unwrap() as usize,
            books: Vec::new(),
            health: Health::from_parts(&health_label, health_reason.as_deref()),
        };
        drop(rows);
        drop(stmt);
        Ok(Some(FileGeneration {
            books: self.books_of(file_id)?,
            ..found
        }))
    }

    fn books_of(&self, file_id: FileId) -> Result<Vec<String>, StoreError> {
        let sql = "select book from file_books where file_id = ? order by book";
        let err = |source| StoreError::Sql {
            statement: sql.to_string(),
            source,
        };
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        let rows = stmt
            .query_map(duckdb::params![file_id], |r| r.get::<_, String>(0))
            .map_err(err)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// The newest source time published for a partition (spec §4.3). This is
    /// what the backfill guard compares against: an older file shares the
    /// batch, so it must not overwrite what is already live.
    pub fn live_source_time(
        &self,
        batch: &str,
        book: &str,
    ) -> Result<Option<DateTime<Utc>>, StoreError> {
        let sql = "select max(fg.source_time) from file_generations fg
                   join file_books fb on fb.file_id = fg.file_id
                   where fg.batch = ? and fb.book = ?";
        self.conn
            .query_row(sql, duckdb::params![batch, book], |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// Per-book freshness: a book is as fresh as its *stalest* file.
    pub fn book_freshness(
        &self,
        dataset: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>, StoreError> {
        let sql = "select book, min(t) from (
                       select fg.batch as batch, fb.book as book, max(fg.source_time) as t
                       from file_generations fg
                       join file_books fb on fb.file_id = fg.file_id
                       where fg.dataset = ?
                       group by fg.batch, fb.book
                   ) group by book order by book";
        let err = |source| StoreError::Sql {
            statement: sql.to_string(),
            source,
        };
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        let rows = stmt
            .query_map(duckdb::params![dataset], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, DateTime<Utc>>(1)?))
            })
            .map_err(err)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// A dataset's headline as-of: the oldest book in scope. An empty scope
    /// means every book.
    pub fn dataset_as_of(
        &self,
        dataset: &str,
        books: &[String],
    ) -> Result<Option<DateTime<Utc>>, StoreError> {
        let fresh = self.book_freshness(dataset)?;
        Ok(fresh
            .into_iter()
            .filter(|(b, _)| books.is_empty() || books.contains(b))
            .map(|(_, t)| t)
            .min())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Datelike, Timelike, Utc};

    /// Terse RFC 3339 literal for tests.
    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn store() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("geode.duckdb")).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    fn record(batch: &str, books: &[&str], source_time: DateTime<Utc>) -> FileGeneration {
        FileGeneration {
            file_id: 0,
            dataset: "risk_snapshot".into(),
            batch: batch.into(),
            path: format!("/src/{batch}.csv").into(),
            size: 1234,
            mtime: source_time,
            source_time,
            gen_id: 0,
            loaded_at: source_time,
            row_count: 10,
            books: books.iter().map(|b| b.to_string()).collect(),
            health: Health::Ok,
        }
    }

    #[test]
    fn gen_ids_are_monotonic() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        assert_eq!(cat.next_gen_id().unwrap(), 1);
        let mut r = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
        r.gen_id = 1;
        cat.record(&r).unwrap();
        assert_eq!(cat.next_gen_id().unwrap(), 2);
    }

    #[test]
    fn reserved_file_ids_are_used_as_given() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        let reserved = cat.reserve_file_id().unwrap();
        let mut r = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
        r.file_id = reserved;
        r.gen_id = 1;
        let recorded = cat.record(&r).unwrap();
        assert_eq!(
            recorded, reserved,
            "the id stamped onto data rows must be the id the catalog stores"
        );
    }

    #[test]
    fn lookup_by_path_returns_the_latest_generation_for_that_file() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        for (generation, hour) in [(1, 7), (2, 9)] {
            let mut r = record(
                "BK000",
                &["BK000"],
                ts(&format!("2026-08-30T{hour:02}:00:00Z")),
            );
            r.gen_id = generation;
            cat.record(&r).unwrap();
        }
        let found = cat
            .lookup_by_path(std::path::Path::new("/src/BK000.csv"))
            .unwrap()
            .unwrap();
        assert_eq!(found.gen_id, 2);
        assert_eq!(found.source_time.hour(), 9);
        assert_eq!(found.books, vec!["BK000".to_string()]);
    }

    #[test]
    fn live_source_time_is_per_partition_not_per_file() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        // Two business dates share a batch: same partition, different files.
        for (generation, day) in [(1, 29u8), (2, 30u8)] {
            let mut r = FileGeneration {
                path: format!("/src/risk_2026-08-{day}_BK000.csv").into(),
                ..record(
                    "BK000",
                    &["BK000"],
                    ts(&format!("2026-08-{day:02}T07:00:00Z")),
                )
            };
            r.gen_id = generation;
            cat.record(&r).unwrap();
        }
        let t = cat.live_source_time("BK000", "BK000").unwrap().unwrap();
        assert_eq!(
            t.day(),
            30,
            "the partition's live time is the newest across its files"
        );
    }

    #[test]
    fn book_freshness_is_the_oldest_contributing_file() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        // BK000 is split across two batches with different source times.
        let mut a = record("BK000_part1", &["BK000"], ts("2026-08-30T07:00:00Z"));
        a.gen_id = 1;
        cat.record(&a).unwrap();
        let mut b = record("BK000_part2", &["BK000"], ts("2026-08-30T14:00:00Z"));
        b.gen_id = 2;
        cat.record(&b).unwrap();

        let fresh = cat.book_freshness("risk_snapshot").unwrap();
        let bk000 = fresh.iter().find(|(b, _)| b == "BK000").unwrap();
        assert_eq!(bk000.1.hour(), 7, "a book is as fresh as its stalest file");
    }

    #[test]
    fn dataset_as_of_is_the_oldest_book_in_scope() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        let mut a = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
        a.gen_id = 1;
        cat.record(&a).unwrap();
        let mut b = record("BK001", &["BK001"], ts("2026-08-30T14:00:00Z"));
        b.gen_id = 2;
        cat.record(&b).unwrap();

        let all = cat.dataset_as_of("risk_snapshot", &[]).unwrap().unwrap();
        assert_eq!(all.hour(), 7, "unscoped as-of is the oldest book");
        let scoped = cat
            .dataset_as_of("risk_snapshot", &["BK001".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(
            scoped.hour(),
            14,
            "scoping to a fresh book must not inherit a stale one"
        );
    }

    #[test]
    fn a_multi_book_file_contributes_to_every_book_it_covers() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        let mut r = record(
            "BK001_BK002",
            &["BK001", "BK002"],
            ts("2026-08-30T09:00:00Z"),
        );
        r.gen_id = 1;
        cat.record(&r).unwrap();
        let fresh = cat.book_freshness("risk_snapshot").unwrap();
        assert_eq!(fresh.len(), 2);
    }
}
