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
use geode_core::schema::{ColumnRole, DatasetSpec, Grain};
use std::path::{Path, PathBuf};

pub type FileId = i64;

/// Freshness per partition of a dataset. `None` is the bookless
/// partition — rows whose book is NULL, which have freshness of their own
/// like any other partition.
pub type BookFreshness = Vec<(Option<String>, DateTime<Utc>)>;

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
    /// The partitions this file wrote, as `(dataset, batch, book)` names
    /// them. `None` is the bookless partition — rows whose book is NULL,
    /// which 2a reports rather than drops, and which publishes under
    /// `book is null`. Modelled the same way `Partition.book` is, because
    /// a `Vec<String>` cannot represent it and everything that joined on
    /// it silently lost those rows.
    pub books: Vec<Option<String>>,
    pub health: Health,
}

/// An attribute column whose value for one entity disagrees across files.
/// The within-file equivalent is `ingest::split::Conflict`; this one is
/// counted per *entity* rather than per grain group, because the whole
/// point is that one entity appears under several keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeConflict {
    pub grain: Grain,
    pub column: String,
    /// How many distinct entities at this grain disagree with themselves.
    pub entities: usize,
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
CREATE TABLE IF NOT EXISTS attribute_conflicts (
  observed_at TIMESTAMP WITH TIME ZONE,
  dataset VARCHAR,
  grain VARCHAR,
  column_name VARCHAR,
  entities BIGINT
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
        self.sql(DDL)?;
        self.ensure_gen_id_sequence()
    }

    /// Create the generation sequence starting above whatever the catalog
    /// already holds.
    ///
    /// It cannot live in [`DDL`] with a literal `START 1`: on a database
    /// written before the sequence existed, that would hand out ids that
    /// are already stamped onto live rows — the same collision this
    /// sequence exists to prevent, but hitting every existing partition
    /// rather than one crashed load. `IF NOT EXISTS` makes this a no-op
    /// once created, so the start value is only ever read from a catalog
    /// the sequence has not yet been responsible for.
    fn ensure_gen_id_sequence(&self) -> Result<(), StoreError> {
        let start = self.latest_gen_id()? + 1;
        self.sql(&format!(
            "CREATE SEQUENCE IF NOT EXISTS file_generations_gen_id START {start};"
        ))
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

    /// Take the next generation id, consuming it.
    ///
    /// A sequence rather than `max(gen_id) + 1`, because that was peeked
    /// *before* the catalog row was written: a load that published its
    /// rows and then failed to record left the id free, and the next load
    /// stamped a different generation of the same partition with it. The
    /// generation predicate then could not tell the two apart. A sequence
    /// hands out an id once whether or not anything is ever recorded
    /// against it, so a crashed load costs an id and nothing else.
    pub fn reserve_gen_id(&self) -> Result<i64, StoreError> {
        let sql = "select nextval('file_generations_gen_id')";
        self.conn
            .query_row(sql, [], |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// The newest generation recorded, or 0 when nothing has loaded.
    ///
    /// Read-only: freshness reporting asks this on every query, and must
    /// not burn an id to answer.
    pub fn latest_gen_id(&self) -> Result<i64, StoreError> {
        let sql = "select coalesce(max(gen_id), 0) from file_generations";
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

        // A `None` book writes a NULL row rather than no row: the bookless
        // partition exists and its freshness has to be recoverable, which
        // an absent row cannot express.
        for book in &rec.books {
            let sql = "insert into file_books values (?, ?)";
            self.conn
                .execute(sql, duckdb::params![file_id, book.as_deref()])
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

    fn books_of(&self, file_id: FileId) -> Result<Vec<Option<String>>, StoreError> {
        let sql = "select book from file_books where file_id = ? order by book";
        let err = |source| StoreError::Sql {
            statement: sql.to_string(),
            source,
        };
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        let rows = stmt
            .query_map(duckdb::params![file_id], |r| r.get::<_, Option<String>>(0))
            .map_err(err)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// The newest source time published for a partition (spec §4.3). This is
    /// what the backfill guard compares against: an older file shares the
    /// batch, so it must not overwrite what is already live.
    /// `book` is `None` for the bookless partition, matching
    /// `Partition.book`. `fb.book = ?` cannot match a NULL, so asking with
    /// `&str` could never see it — and a load of purely unattributed rows
    /// therefore computed "nothing is live yet" and published with the
    /// guard disabled.
    pub fn live_source_time(
        &self,
        batch: &str,
        book: Option<&str>,
    ) -> Result<Option<DateTime<Utc>>, StoreError> {
        let (sql, params): (&str, Vec<duckdb::types::Value>) = match book {
            Some(b) => (
                "select max(fg.source_time) from file_generations fg
                 join file_books fb on fb.file_id = fg.file_id
                 where fg.batch = ? and fb.book = ?",
                vec![batch.to_string().into(), b.to_string().into()],
            ),
            None => (
                "select max(fg.source_time) from file_generations fg
                 join file_books fb on fb.file_id = fg.file_id
                 where fg.batch = ? and fb.book is null",
                vec![batch.to_string().into()],
            ),
        };
        self.conn
            .query_row(sql, duckdb::params_from_iter(params), |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// Per-book freshness: a book is as fresh as its *stalest* file.
    ///
    /// `None` is the bookless partition, which has freshness of its own.
    /// It used to have none: `file_books` held no row for it, so the join
    /// dropped it — invisibly on a file that also carried real books,
    /// where its staleness was reported as theirs, and entirely on a file
    /// of only unattributed rows.
    pub fn book_freshness(&self, dataset: &str) -> Result<BookFreshness, StoreError> {
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
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, DateTime<Utc>>(1)?,
                ))
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
            // Scoping to named books excludes the bookless partition: it is
            // not any of them. Unscoped includes it, because it is part of
            // the dataset.
            .filter(|(b, _)| books.is_empty() || b.as_ref().is_some_and(|b| books.contains(b)))
            .map(|(_, t)| t)
            .min())
    }

    /// Attribute columns whose value for one entity disagrees across the
    /// whole live table (spec §3.5).
    ///
    /// The same shape as the within-file detector in `ingest::split`, run
    /// over everything rather than one file's staging table — and grouped
    /// by the grain's *identity* columns rather than its key, because an
    /// instrument reused in two books has two keys and one identity. That
    /// grouping is what makes this cross-file rather than a re-run of the
    /// check ingest already did.
    ///
    /// Diagnostic, not corrective: newest source time still wins. The
    /// value is the signal — a column that conflicts on every load is
    /// evidence it does not live at this grain at all (spec §3.4).
    pub fn attribute_conflicts(
        &self,
        dataset: &str,
        ds: &DatasetSpec,
        grain: Grain,
    ) -> Result<Vec<AttributeConflict>, StoreError> {
        let attributes: Vec<&str> = ds
            .columns
            .iter()
            .filter(|c| matches!(c.role, ColumnRole::Attribute { grain: g } if g == grain))
            .map(|c| c.name.as_str())
            .collect();
        if attributes.is_empty() {
            return Ok(Vec::new());
        }
        let identity: Vec<String> = grain
            .identity_columns()
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect();
        let inner: Vec<String> = attributes
            .iter()
            .map(|c| format!("min(\"{c}\") as \"{c}_lo\", max(\"{c}\") as \"{c}_hi\""))
            .collect();
        let outer: Vec<String> = attributes
            .iter()
            .map(|c| {
                format!("count(*) filter (where \"{c}_lo\" is distinct from \"{c}_hi\") as \"{c}\"")
            })
            .collect();
        let sql = format!(
            "select {outer} from (select {id}, {inner} from {table} group by {id})",
            outer = outer.join(", "),
            id = identity.join(", "),
            inner = inner.join(", "),
            table =
                crate::store::ddl::table_name(dataset, grain, crate::store::ddl::TableKind::Live),
        );
        let err = |source| StoreError::Sql {
            statement: sql.clone(),
            source,
        };
        let mut stmt = self.conn.prepare(&sql).map_err(err)?;
        let mut rows = stmt.query([]).map_err(err)?;
        let mut out = Vec::new();
        if let Some(row) = rows.next().map_err(err)? {
            for (i, column) in attributes.iter().enumerate() {
                let entities: i64 = row.get(i).unwrap_or(0);
                if entities > 0 {
                    out.push(AttributeConflict {
                        grain,
                        column: (*column).to_string(),
                        entities: entities as usize,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Append a reading. Diagnostics want the trend, not the latest number.
    pub fn record_attribute_conflicts(
        &self,
        dataset: &str,
        conflicts: &[AttributeConflict],
        observed_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let sql = "insert into attribute_conflicts \
                   (observed_at, dataset, grain, column_name, entities) values (?, ?, ?, ?, ?)";
        for c in conflicts {
            self.conn
                .execute(
                    sql,
                    duckdb::params![
                        observed_at,
                        dataset,
                        c.grain.short(),
                        c.column,
                        c.entities as i64
                    ],
                )
                .map_err(|source| StoreError::Sql {
                    statement: sql.into(),
                    source,
                })?;
        }
        Ok(())
    }

    /// Every recorded reading for a dataset, oldest first.
    pub fn attribute_conflict_history(
        &self,
        dataset: &str,
    ) -> Result<Vec<(DateTime<Utc>, AttributeConflict)>, StoreError> {
        let sql = "select observed_at, grain, column_name, entities from attribute_conflicts \
                   where dataset = ? order by observed_at, column_name";
        let err = |source| StoreError::Sql {
            statement: sql.to_string(),
            source,
        };
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        let rows = stmt
            .query_map(duckdb::params![dataset], |r| {
                Ok((
                    r.get::<_, DateTime<Utc>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(err)?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter_map(|(at, grain, column, entities)| {
                Some((
                    at,
                    AttributeConflict {
                        grain: Grain::parse(&grain)?,
                        column,
                        entities: entities as usize,
                    },
                ))
            })
            .collect())
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
            books: books.iter().map(|b| Some(b.to_string())).collect(),
            health: Health::Ok,
        }
    }

    #[test]
    fn gen_ids_are_monotonic() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        assert_eq!(cat.reserve_gen_id().unwrap(), 1);
        let mut r = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
        r.gen_id = 1;
        cat.record(&r).unwrap();
        assert_eq!(cat.reserve_gen_id().unwrap(), 2);
    }

    #[test]
    fn a_reserved_gen_id_is_never_handed_out_twice() {
        // The id used to be `max(gen_id) + 1` over the catalog, peeked
        // before the row was written. A load that published its rows and
        // then failed to record left the id free, so the next load stamped
        // a *different* generation of the same partition with it — and the
        // generation predicate could no longer tell them apart.
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());

        let published = cat.reserve_gen_id().unwrap();
        // The catalog row is never written: this is the crash window.
        let next = cat.reserve_gen_id().unwrap();
        assert_ne!(
            published, next,
            "an id handed out once must not be handed out again, recorded or not"
        );
    }

    #[test]
    fn the_newest_recorded_generation_is_reported_without_consuming_an_id() {
        // Freshness reporting reads the newest generation; it must not
        // allocate, or merely asking how fresh a dataset is would burn an
        // id on every query.
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        assert_eq!(cat.latest_gen_id().unwrap(), 0, "nothing recorded yet");

        let mut r = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
        r.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&r).unwrap();

        assert_eq!(cat.latest_gen_id().unwrap(), r.gen_id);
        assert_eq!(
            cat.latest_gen_id().unwrap(),
            r.gen_id,
            "reading twice reports the same generation"
        );
    }

    #[test]
    fn a_database_that_predates_the_sequence_continues_above_its_generations() {
        // `CREATE SEQUENCE IF NOT EXISTS ... START 1` on a database that
        // already holds generations would hand out ids that are already in
        // use, which is the collision this change exists to remove — with
        // every existing partition as the victim rather than a crashed
        // load.
        let (_d, store) = store();
        {
            // A catalog as it looked before the sequence: rows carrying
            // gen_ids, and no sequence to match.
            let cat = Catalog::new(store.writer());
            for (i, book) in ["BK000", "BK001", "BK002"].iter().enumerate() {
                let mut r = record(book, &[book], ts("2026-08-30T07:00:00Z"));
                r.gen_id = i as i64 + 1;
                cat.record(&r).unwrap();
            }
            store
                .writer()
                .execute_batch("drop sequence if exists file_generations_gen_id;")
                .unwrap();
        }

        let cat = Catalog::new(store.writer());
        cat.ensure_tables().unwrap();
        assert!(
            cat.reserve_gen_id().unwrap() > 3,
            "the sequence must start above the generations already recorded"
        );
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
        assert_eq!(found.books, vec![Some("BK000".to_string())]);
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
        let t = cat
            .live_source_time("BK000", Some("BK000"))
            .unwrap()
            .unwrap();
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
        let bk000 = fresh
            .iter()
            .find(|(b, _)| b.as_deref() == Some("BK000"))
            .unwrap();
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
    fn the_bookless_partition_has_freshness_of_its_own() {
        // Rows whose book is NULL are an ordinary part of the feed (2a
        // reports them rather than dropping them) and publish as their own
        // partition, `book is null`. But `file_books` had no row for them,
        // and `book_freshness` inner-joins it — so their staleness was
        // either invisible or, on a file that also carries real books,
        // silently reported as those books' staleness instead.
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());

        // One file with a real book, one file of only unattributed rows,
        // and the unattributed one is much staler.
        let mut a = record("BK000", &["BK000"], ts("2026-08-30T14:00:00Z"));
        a.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&a).unwrap();

        let mut b = record("UNATTRIBUTED", &[], ts("2026-08-30T07:00:00Z"));
        b.books = vec![None];
        b.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&b).unwrap();

        let fresh = cat.book_freshness("risk_snapshot").unwrap();
        let bookless = fresh
            .iter()
            .find(|(b, _)| b.is_none())
            .unwrap_or_else(|| panic!("the bookless partition must appear: {fresh:?}"));
        assert_eq!(
            bookless.1.hour(),
            7,
            "and carry its own source time, not another book's"
        );
        assert!(
            fresh.iter().any(|(b, _)| b.as_deref() == Some("BK000")),
            "without displacing the real books: {fresh:?}"
        );
    }

    #[test]
    fn the_backfill_guard_sees_the_bookless_partition() {
        // `live_source_time` took `&str`, so nothing could ask what was
        // live for `book is null`, and the load folded the guard over its
        // named books only. Reachable whenever a batch's books change
        // between generations: v1 writes [A] plus unattributed, v2 writes
        // [B] plus unattributed, the guard asks only about B, B was never
        // live, so it publishes — overwriting the bookless partition v1
        // left live at a newer source time.
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        let mut r = record("MIXED", &["BK000"], ts("2026-08-30T14:00:00Z"));
        r.books = vec![Some("BK000".to_string()), None];
        r.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&r).unwrap();

        assert_eq!(
            cat.live_source_time("MIXED", None)
                .unwrap()
                .map(|t| t.hour()),
            Some(14),
            "the bookless partition of this batch is live and must say so"
        );
        assert_eq!(
            cat.live_source_time("MIXED", Some("BK000"))
                .unwrap()
                .map(|t| t.hour()),
            Some(14),
            "and the named book still resolves"
        );
        assert_eq!(
            cat.live_source_time("MIXED", Some("BK999")).unwrap(),
            None,
            "a book this batch does not carry is not live"
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
