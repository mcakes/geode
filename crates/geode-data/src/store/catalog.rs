//! Publication catalog and freshness in source time. Filesystem modification
//! time cannot establish freshness because copies and restores can change it.
//!
//! A book is as fresh as its stalest contributing file; a dataset headline uses
//! the oldest book in scope. Joined views likewise report their stalest input.

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
    /// Filename with its date component removed, identifying the batch across
    /// business dates. Dataset and book complete the partition key.
    pub batch: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: DateTime<Utc>,
    /// From the sentinel. Orders generations and drives as-of.
    pub source_time: DateTime<Utc>,
    pub gen_id: i64,
    pub loaded_at: DateTime<Utc>,
    pub row_count: usize,
    /// Books written by this file within its dataset and batch. `None` names
    /// the bookless partition, retained by ingest and matched with `IS NULL`
    /// during publication and freshness lookup.
    pub books: Vec<Option<String>>,
    /// True when backfill routing sent the generation directly to archive.
    /// It remains part of provenance and history, but cannot establish live
    /// freshness because it never replaced live data.
    pub archived_only: bool,
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
  health_reason VARCHAR,
  archived_only BOOLEAN
);
-- Catalogs written before `archived_only` existed need the column added;
-- CREATE TABLE IF NOT EXISTS leaves an existing table untouched.
ALTER TABLE file_generations ADD COLUMN IF NOT EXISTS archived_only BOOLEAN;
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
-- Generation identities per dataset and partition, maintained with payload
-- publication and retention so as-of queries need not scan archive rows.
-- A primary key would exclude NULL books; maintenance code enforces uniqueness.
CREATE TABLE IF NOT EXISTS generations (
  dataset VARCHAR,
  batch VARCHAR,
  book VARCHAR,
  gen_id BIGINT,
  source_time TIMESTAMP WITH TIME ZONE
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

    /// Create the generation sequence above recorded history if it does not
    /// already exist. A fresh store starts at 1; a populated catalog starts at
    /// `latest + 2` to avoid an unrecorded legacy allocation at `latest + 1`.
    fn ensure_gen_id_sequence(&self) -> Result<(), StoreError> {
        // A legacy load could write rows with `max_recorded + 1` before
        // recording its catalog entry. Skip that possible orphaned ID when
        // initializing the sequence. IDs need only be unique and increasing,
        // so the gap is harmless. An existing sequence is left unchanged.
        let latest = self.latest_gen_id()?;
        let start = if latest == 0 { 1 } else { latest + 2 };
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

    /// Reserve and consume the next generation ID. A failed or rolled-back
    /// load must not make its allocation available to another generation.
    /// Sequence gaps are harmless; ID reuse makes history ambiguous.
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
    /// Read-only, and read once: `ensure_gen_id_sequence` asks
    /// it at `ensure_tables` to place the ID sequence above recorded
    /// history, which must not consume an id to do. It aggregates the whole
    /// catalog and names no dataset or partition, so it is not an answer to
    /// what a read was as of or which generation served it — those are
    /// [`Catalog::live_generation`] and [`Catalog::dataset_generation`].
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
                          ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
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
                    rec.archived_only,
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
                          loaded_at, row_count, health, health_reason, archived_only
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
        // NULL for rows written before the column existed, which is a
        // real state: such a generation was recorded under the old rule,
        // where every recorded generation counted toward freshness. Read
        // as `Option` so the migration case is expressible, but not
        // `unwrap_or`-ed over the *error* — a missing column is a bug in
        // this query, not a value.
        let archived_only: bool = row.get::<_, Option<bool>>(12).unwrap().unwrap_or(false);
        let found = FileGeneration {
            archived_only,
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

    /// Newest source time of a generation that went live for this partition.
    /// The backfill guard compares against it so older arrivals cannot replace
    /// current data. `None` names the bookless partition and requires `IS NULL`
    /// rather than ordinary equality.
    pub fn live_source_time(
        &self,
        dataset: &str,
        batch: &str,
        book: Option<&str>,
    ) -> Result<Option<DateTime<Utc>>, StoreError> {
        // Scope by dataset as well as batch and book. File stems can match
        // across datasets; another dataset's time must not reject this load.
        let (sql, params): (&str, Vec<duckdb::types::Value>) = match book {
            Some(b) => (
                "select max(fg.source_time) from file_generations fg
                 join file_books fb on fb.file_id = fg.file_id
                 where fg.dataset = ? and fg.batch = ? and fb.book = ?
                   and coalesce(fg.archived_only, false) = false",
                vec![
                    dataset.to_string().into(),
                    batch.to_string().into(),
                    b.to_string().into(),
                ],
            ),
            None => (
                "select max(fg.source_time) from file_generations fg
                 join file_books fb on fb.file_id = fg.file_id
                 where fg.dataset = ? and fg.batch = ? and fb.book is null
                   and coalesce(fg.archived_only, false) = false",
                vec![dataset.to_string().into(), batch.to_string().into()],
            ),
        };
        self.conn
            .query_row(sql, duckdb::params_from_iter(params), |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// One partition's newest live generation — [`Self::live_source_time`]'s
    /// identity, over the same rows under the same filters.
    ///
    /// A corrected republish keeps its source time and takes a new
    /// generation ID, so this is the only thing that distinguishes the two
    /// for a reader holding unsent work over the older one. `None` before
    /// the partition's first load.
    pub fn live_generation(
        &self,
        dataset: &str,
        batch: &str,
        book: Option<&str>,
    ) -> Result<Option<i64>, StoreError> {
        // The same dataset/batch/book scoping `live_source_time` explains:
        // file stems can match across datasets, and the bookless partition
        // is its own.
        let (sql, params): (&str, Vec<duckdb::types::Value>) = match book {
            Some(b) => (
                "select max(fg.gen_id) from file_generations fg
                 join file_books fb on fb.file_id = fg.file_id
                 where fg.dataset = ? and fg.batch = ? and fb.book = ?
                   and coalesce(fg.archived_only, false) = false",
                vec![
                    dataset.to_string().into(),
                    batch.to_string().into(),
                    b.to_string().into(),
                ],
            ),
            None => (
                "select max(fg.gen_id) from file_generations fg
                 join file_books fb on fb.file_id = fg.file_id
                 where fg.dataset = ? and fg.batch = ? and fb.book is null
                   and coalesce(fg.archived_only, false) = false",
                vec![dataset.to_string().into(), batch.to_string().into()],
            ),
        };
        self.conn
            .query_row(sql, duckdb::params_from_iter(params), |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// A dataset's newest live generation, across every partition.
    ///
    /// The maximum, where [`Self::dataset_as_of`] takes the minimum, because
    /// the two answer different questions. `as_of` reports how stale an answer
    /// is and so must name its stalest input; the generation reports whether
    /// this is the same data as last time, and a minimum would not move when
    /// a single partition republished — exactly the change a reader of this
    /// field exists to see. `None` before the dataset's first load.
    pub fn dataset_generation(&self, dataset: &str) -> Result<Option<i64>, StoreError> {
        let sql = "select max(gen_id) from file_generations
                   where dataset = ? and coalesce(archived_only, false) = false";
        self.conn
            .query_row(sql, [dataset], |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })
    }

    /// For each book, take the newest live-published source time per batch,
    /// then the oldest of those contributing batches. Bookless rows contribute
    /// their own `None` entry. Archive-only arrivals do not affect freshness.
    pub fn book_freshness(&self, dataset: &str) -> Result<BookFreshness, StoreError> {
        let sql = "select book, min(t) from (
                       select fg.batch as batch, fb.book as book, max(fg.source_time) as t
                       from file_generations fg
                       join file_books fb on fb.file_id = fg.file_id
                       where fg.dataset = ?
                         and coalesce(fg.archived_only, false) = false
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

    /// Read persisted unhealthy conditions for the generations served by live
    /// queries, one worst condition per batch. `DataService::open` uses this to
    /// seed load health: an unchanged file after restart will not republish,
    /// but any degradation in its retained live data must remain visible.
    ///
    /// Resolve the newest non-archive-only generation per `(batch, book)` by
    /// source time, breaking ties by greatest generation ID. Do not bound by
    /// the host clock: live queries also include future-stamped source data.
    /// A same-time corrected republish must supersede its predecessor's health.
    ///
    /// Exclude archive-only records in the join before ranking so a backfill
    /// cannot hide the live generation. Collapse books to their batch's worst
    /// severity, ordered `failed`, `degraded`, `pending_too_long`, `pending`,
    /// matching the data service's load-health lane.
    ///
    /// Only these known unhealthy labels are admitted. Unknown labels decode
    /// as `Ok` and must not seed a false health report. A summary generation
    /// without a file-catalog record has no recorded health and is omitted.
    /// Row decoding errors propagate instead of silently hiding a condition.
    pub fn live_health(&self, dataset: &str) -> Result<Vec<(String, Health)>, StoreError> {
        let sql = "select batch, health, health_reason from (
                       select batch, health, health_reason,
                              row_number() over (
                                  partition by batch
                                  order by case health
                                               when 'failed' then 4
                                               when 'degraded' then 3
                                               when 'pending_too_long' then 2
                                               when 'pending' then 1
                                               else 0
                                           end desc,
                                           source_time desc, gen_id desc
                              ) as batch_rn
                       from (
                           select g.batch as batch, g.gen_id as gen_id,
                                  g.source_time as source_time,
                                  fg.health as health,
                                  fg.health_reason as health_reason,
                                  row_number() over (
                                      partition by g.batch, g.book
                                      order by g.source_time desc, g.gen_id desc
                                  ) as rn
                           from generations g
                           join file_generations fg
                             on fg.gen_id = g.gen_id
                            and fg.dataset = g.dataset
                            and coalesce(fg.archived_only, false) = false
                           where g.dataset = ?
                       ) where rn = 1
                         and health in ('failed', 'degraded', 'pending_too_long', 'pending')
                   ) where batch_rn = 1 order by batch";
        let err = |source| StoreError::Sql {
            statement: sql.to_string(),
            source,
        };
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        let rows = stmt
            .query_map(duckdb::params![dataset], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(err)?;
        // Propagated, never swallowed, for `resolve_generations`' own
        // reason: a row dropped here reports a degraded source as clean,
        // which is the exact failure this function exists to prevent.
        let mut out = Vec::new();
        for row in rows {
            let (batch, label, reason) = row.map_err(err)?;
            out.push((batch, Health::from_parts(&label, reason.as_deref())));
        }
        Ok(out)
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

    /// Find attribute columns that disagree for one entity across the live
    /// table. Group by grain identity rather than its full key: an instrument
    /// held in two books has two keys but one identity. This catches cross-file
    /// conflicts that a staging-table check cannot see.
    ///
    /// The result is diagnostic; it does not rewrite data. Repeated conflicts
    /// can indicate that the attribute is declared at the wrong grain.
    pub fn attribute_conflicts(
        &self,
        dataset: &str,
        ds: &DatasetSpec,
        grain: Grain,
    ) -> Result<Vec<AttributeConflict>, StoreError> {
        let attributes: Vec<&str> = ds
            .columns
            .iter()
            .filter(|c| matches!(c.role, ColumnRole::Attribute { grain: Some(g) } if g == grain))
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
    use geode_core::schema::{ColumnSpec, ColumnType};

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
            archived_only: false,
            health: Health::Ok,
        }
    }

    /// A document-level attribute (`grain: None`) must never be folded
    /// into `attributes` at any measure grain. This is the one call this
    /// dataset shape cannot make honestly today (a document dataset never
    /// reaches `attribute_conflicts`), so a schema-shaped test cannot see
    /// a regression here — only a query issued against a table this
    /// dataset never created can. `store()` never runs `create_table_sql`,
    /// so if the filter ever matched `spot_ref` here, `.unwrap()` would
    /// panic on the missing `risk_snapshot_position_live` table instead of
    /// this test quietly passing.
    #[test]
    fn attribute_conflicts_ignores_a_document_level_attribute() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        let ds = DatasetSpec {
            name: "risk_snapshot".into(),
            columns: vec![ColumnSpec {
                name: "spot_ref".into(),
                source_name: None,
                ty: ColumnType::F64,
                required: false,
                textual: false,
                categorical: false,
                role: ColumnRole::Attribute { grain: None },
            }],
            local: false,
            ..Default::default()
        };
        let conflicts = cat
            .attribute_conflicts("risk_snapshot", &ds, Grain::Position)
            .unwrap();
        assert!(conflicts.is_empty(), "{conflicts:?}");
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
        // Consume an ID without recording a catalog row, as a failed load can
        // do. The next reservation must still return a different ID.
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
    fn the_migration_skips_the_id_a_crashed_pre_sequence_load_could_hold() {
        // The old allocator peeked `max(gen_id) + 1` before writing the
        // catalog row, so a load that published and then failed to record
        // left rows stamped with an id the catalog never learned about.
        // Starting the sequence at `latest + 1` would hand that exact id
        // out again — the migration reproducing, once, the collision it
        // exists to remove.
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        for i in 1..=3 {
            let mut r = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
            r.gen_id = i;
            cat.record(&r).unwrap();
        }
        // Generation 4 is the one a crashed load would have stamped onto
        // rows without ever recording.
        store
            .writer()
            .execute_batch("drop sequence if exists file_generations_gen_id;")
            .unwrap();

        let cat = Catalog::new(store.writer());
        cat.ensure_tables().unwrap();
        assert!(
            cat.reserve_gen_id().unwrap() > 4,
            "the first id handed out must clear the orphan a crashed \
             pre-sequence load could be holding"
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
            .live_source_time("risk_snapshot", "BK000", Some("BK000"))
            .unwrap()
            .unwrap();
        assert_eq!(
            t.day(),
            30,
            "the partition's live time is the newest across its files"
        );
    }

    #[test]
    fn live_generation_is_the_partitions_newest_and_moves_on_a_same_time_republish() {
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());
        // Two generations of ONE partition at the SAME source time: the
        // corrected republish a draft must be able to tell apart.
        for generation in [1i64, 2] {
            let mut r = FileGeneration {
                path: format!("/src/risk_v{generation}_BK000.csv").into(),
                ..record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"))
            };
            r.gen_id = generation;
            cat.record(&r).unwrap();
        }
        // Another partition, newer, must not answer for this one.
        let mut other = record("BK001", &["BK001"], ts("2026-08-30T14:00:00Z"));
        other.gen_id = 9;
        cat.record(&other).unwrap();

        assert_eq!(
            cat.live_generation("risk_snapshot", "BK000", Some("BK000"))
                .unwrap(),
            Some(2),
            "the partition's own newest generation, not the database's"
        );
        assert_eq!(
            cat.dataset_generation("risk_snapshot").unwrap(),
            Some(9),
            "the dataset's newest generation across its partitions"
        );
        assert_eq!(
            cat.live_generation("risk_snapshot", "NOSUCH", Some("BK000"))
                .unwrap(),
            None,
            "a partition that has never loaded has no generation"
        );
        assert_eq!(
            cat.dataset_generation("nosuch_dataset").unwrap(),
            None,
            "a dataset that has never loaded has no generation"
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
        // Two generations of one batch, at *different* times and covering
        // different partitions. Asking with the same time for both would
        // let a query that matched the wrong partition return the right
        // answer by accident — which is exactly what an earlier version of
        // this fixture did, and the mutation harness is what said so.
        let mut named = record("MIXED", &["BK000"], ts("2026-08-30T14:00:00Z"));
        named.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&named).unwrap();

        let mut r = record("MIXED", &[], ts("2026-08-30T09:00:00Z"));
        r.books = vec![None];
        r.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&r).unwrap();

        assert_eq!(
            cat.live_source_time("risk_snapshot", "MIXED", None)
                .unwrap()
                .map(|t| t.hour()),
            Some(9),
            "the bookless partition has a live time of its own — not the \
             named book's, which is four hours newer"
        );
        assert_eq!(
            cat.live_source_time("risk_snapshot", "MIXED", Some("BK000"))
                .unwrap()
                .map(|t| t.hour()),
            Some(14),
            "and the named book still resolves"
        );
        assert_eq!(
            cat.live_source_time("risk_snapshot", "MIXED", Some("BK999"))
                .unwrap(),
            None,
            "a book this batch does not carry is not live"
        );
    }

    #[test]
    fn the_bookless_partition_is_in_the_unscoped_as_of_but_not_a_named_scope() {
        // The headline as-of is what consumers actually read
        // (`service.rs` calls `dataset_as_of(dataset, &[])` on every live
        // query), and the roll-up of the bookless partition into it is the
        // whole point of making that partition visible. It was asserted
        // nowhere: the freshness test stops at `book_freshness`, and the
        // scoping test records no bookless data at all — so both
        // directions of this rule could be inverted with the suite green.
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());

        let mut named = record("BK001", &["BK001"], ts("2026-08-30T14:00:00Z"));
        named.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&named).unwrap();

        // The bookless partition is the stalest thing in the dataset.
        let mut bookless = record("UNATTRIBUTED", &[], ts("2026-08-30T06:00:00Z"));
        bookless.books = vec![None];
        bookless.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&bookless).unwrap();

        let unscoped = cat.dataset_as_of("risk_snapshot", &[]).unwrap().unwrap();
        assert_eq!(
            unscoped.hour(),
            6,
            "unscoped, the dataset is as stale as its bookless rows"
        );

        let scoped = cat
            .dataset_as_of("risk_snapshot", &["BK001".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(
            scoped.hour(),
            14,
            "scoping to a named book must not inherit the bookless \
             partition's staleness — `book in (…)` does not match NULL, so \
             the as-of and the rows it describes have to agree"
        );
    }

    #[test]
    fn a_generation_that_never_went_live_does_not_move_freshness() {
        // The backfill guard files an older file as history without it
        // ever being current, but `record` runs regardless — so its
        // `file_books` rows counted toward `book_freshness`, which has no
        // live/archive distinction. A file that never contributed a single
        // live row could therefore drag the dataset's headline as-of
        // backwards, and `service.rs` reads that unscoped on every live
        // query.
        //
        // Pre-existing, but the bookless partition made it far more
        // reachable: `None` is a partition almost every real file has, so
        // an archived-only generation nearly always introduces a
        // (batch, book) pair that nothing live covers.
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());

        let mut live = record("BK000", &["BK000"], ts("2026-08-30T14:00:00Z"));
        live.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&live).unwrap();

        // An older file for the same batch, carrying a partition nothing
        // live covers. It became history and was never current.
        let mut history = record("BK000", &[], ts("2026-08-30T06:00:00Z"));
        history.books = vec![None];
        history.gen_id = cat.reserve_gen_id().unwrap();
        history.archived_only = true;
        cat.record(&history).unwrap();

        assert_eq!(
            cat.dataset_as_of("risk_snapshot", &[])
                .unwrap()
                .unwrap()
                .hour(),
            14,
            "a generation that never went live must not make the dataset \
             look stale"
        );
        assert!(
            !cat.book_freshness("risk_snapshot")
                .unwrap()
                .iter()
                .any(|(b, _)| b.is_none()),
            "and must not report a partition it never made live"
        );
    }

    #[test]
    fn a_catalog_written_before_archived_only_gains_the_column() {
        // `CREATE TABLE IF NOT EXISTS` leaves an existing table alone, so
        // a database written by an older build would keep the old shape
        // and every insert would fail on column count. The ALTER is what
        // makes `ensure_tables` a migration rather than a first-run.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(
                "create table file_generations (
                     file_id BIGINT PRIMARY KEY, dataset VARCHAR, batch VARCHAR,
                     path VARCHAR, size BIGINT,
                     mtime TIMESTAMP WITH TIME ZONE,
                     source_time TIMESTAMP WITH TIME ZONE, gen_id BIGINT,
                     loaded_at TIMESTAMP WITH TIME ZONE, row_count BIGINT,
                     health VARCHAR, health_reason VARCHAR);",
            )
            .unwrap();

        let cat = Catalog::new(store.writer());
        cat.ensure_tables().unwrap();

        // The column is there, and a record round-trips through it.
        let mut r = record("BK000", &["BK000"], ts("2026-08-30T07:00:00Z"));
        r.gen_id = cat.reserve_gen_id().unwrap();
        r.archived_only = true;
        let id = cat.record(&r).unwrap();
        let found = cat.lookup_by_path(&r.path).unwrap().unwrap();
        assert_eq!(found.file_id, id);
        assert!(found.archived_only, "the flag survives the round trip");
    }

    #[test]
    fn the_backfill_guard_does_not_read_another_datasets_source_times() {
        // `batch` is the filename with its date component removed, so two
        // datasets whose source files share a naming stem produce the same
        // batch. Without the dataset filter the guard for one read the
        // other's source times, and a legitimately new file was filed as
        // history — no error, no degradation, just data that never went
        // live.
        //
        // The first mutation entry for this was a false positive: it bound
        // `dataset` to `fg.batch`, which broke batch matching rather than
        // dataset scoping, so it was "caught" for the wrong reason and
        // this case had no test at all.
        let (_d, store) = store();
        let cat = Catalog::new(store.writer());

        let mut risk = record("SHARED", &["BK000"], ts("2026-08-30T14:00:00Z"));
        risk.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&risk).unwrap();

        let mut vol = record("SHARED", &["BK000"], ts("2026-08-30T06:00:00Z"));
        vol.dataset = "implied_vol_summary".into();
        vol.path = std::path::PathBuf::from("/src/vol_2026-08-30_BK000.csv");
        vol.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&vol).unwrap();

        assert_eq!(
            cat.live_source_time("implied_vol_summary", "SHARED", Some("BK000"))
                .unwrap()
                .map(|t| t.hour()),
            Some(6),
            "the vol dataset's own generation, not the risk one eight \
             hours newer that happens to share a batch name"
        );
        assert_eq!(
            cat.live_source_time("risk_snapshot", "SHARED", Some("BK000"))
                .unwrap()
                .map(|t| t.hour()),
            Some(14),
            "and each dataset still sees its own"
        );

        // The bookless arm is a separate SQL statement and needs its own
        // coverage: scoping one and not the other would be invisible here
        // otherwise.
        let mut risk_bookless = record("SHARED2", &[], ts("2026-08-30T15:00:00Z"));
        risk_bookless.books = vec![None];
        risk_bookless.path = std::path::PathBuf::from("/src/risk2.csv");
        risk_bookless.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&risk_bookless).unwrap();

        let mut vol_bookless = record("SHARED2", &[], ts("2026-08-30T05:00:00Z"));
        vol_bookless.dataset = "implied_vol_summary".into();
        vol_bookless.books = vec![None];
        vol_bookless.path = std::path::PathBuf::from("/src/vol2.csv");
        vol_bookless.gen_id = cat.reserve_gen_id().unwrap();
        cat.record(&vol_bookless).unwrap();

        assert_eq!(
            cat.live_source_time("implied_vol_summary", "SHARED2", None)
                .unwrap()
                .map(|t| t.hour()),
            Some(5),
            "the bookless partition is scoped by dataset too"
        );
        assert_eq!(
            cat.live_source_time("risk_snapshot", "SHARED2", None)
                .unwrap()
                .map(|t| t.hour()),
            Some(15)
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

    /// Record one generation into BOTH the catalog and the `generations`
    /// summary, the way a real publish does — `live_health` reads the
    /// join of the two, so a fixture that writes only one of them cannot
    /// reach it.
    fn published(
        store: &crate::store::Store,
        batch: &str,
        book: Option<&str>,
        gen_id: i64,
        source_time: DateTime<Utc>,
        health: Health,
        archived_only: bool,
    ) {
        let mut r = record(batch, &[], source_time);
        r.books = vec![book.map(|b| b.to_string())];
        r.gen_id = gen_id;
        r.file_id = gen_id;
        r.health = health;
        r.archived_only = archived_only;
        Catalog::new(store.writer()).record(&r).unwrap();
        store
            .writer()
            .execute(
                "insert into generations values ('risk_snapshot', ?, ?, ?, ?)",
                duckdb::params![batch, book, gen_id, source_time],
            )
            .unwrap();
    }

    #[test]
    fn live_health_reports_only_the_batches_whose_live_generation_is_unhealthy() {
        // Recover the persisted health of still-live data after restart, when
        // no new load is needed to produce another health event.
        let (_d, store) = store();
        // BK000: degraded and still live — the whole point.
        published(
            &store,
            "BK000",
            Some("BK000"),
            1,
            ts("2026-08-30T07:00:00Z"),
            Health::Degraded {
                reason: "currency varies within instrument key".into(),
            },
            false,
        );
        // BK001: clean. Never reported — seeding it would say a source
        // is degraded because some other batch once was.
        published(
            &store,
            "BK001",
            Some("BK001"),
            2,
            ts("2026-08-30T07:00:00Z"),
            Health::Ok,
            false,
        );
        // BK002: degraded, then CORRECTED by a later clean republish. The
        // live generation is the clean one, so nothing is outstanding —
        // a fixture that looked at history rather than at what is live
        // would wrongly report this one forever.
        published(
            &store,
            "BK002",
            Some("BK002"),
            3,
            ts("2026-08-30T07:00:00Z"),
            Health::Degraded {
                reason: "stale".into(),
            },
            false,
        );
        published(
            &store,
            "BK002",
            Some("BK002"),
            4,
            ts("2026-08-30T08:00:00Z"),
            Health::Ok,
            false,
        );

        let live = Catalog::new(store.writer())
            .live_health("risk_snapshot")
            .unwrap();
        assert_eq!(
            live,
            vec![(
                "BK000".to_string(),
                Health::Degraded {
                    reason: "currency varies within instrument key".into(),
                }
            )],
            "only the batch whose LIVE generation is unhealthy"
        );
    }

    #[test]
    fn live_health_reports_a_generation_whose_source_time_is_in_the_future() {
        // A future-stamped generation can be live because publication applies
        // no host-time upper bound. Read its health too; a clock cutoff would
        // incorrectly report the preceding clean generation.
        let (_d, store) = store();
        published(
            &store,
            "BK000",
            Some("BK000"),
            1,
            ts("2026-08-30T07:00:00Z"),
            Health::Ok,
            false,
        );
        published(
            &store,
            "BK000",
            Some("BK000"),
            2,
            Utc::now() + chrono::Duration::days(3650),
            Health::Degraded {
                reason: "stamped ahead of the clock".into(),
            },
            false,
        );
        assert_eq!(
            Catalog::new(store.writer())
                .live_health("risk_snapshot")
                .unwrap(),
            vec![(
                "BK000".to_string(),
                Health::Degraded {
                    reason: "stamped ahead of the clock".into()
                }
            )],
            "what is live is what the live path serves, not what a clock bound admits"
        );
    }

    #[test]
    fn live_health_breaks_a_tied_source_time_on_the_newer_generation() {
        // A same-time corrected republish replaces live data. Its greater
        // generation ID must also replace the predecessor's health on restart.
        let (_d, store) = store();
        let tied = ts("2026-08-30T07:00:00Z");
        published(
            &store,
            "BK000",
            Some("BK000"),
            1,
            tied,
            Health::Degraded {
                reason: "the operator has already fixed this".into(),
            },
            false,
        );
        published(&store, "BK000", Some("BK000"), 2, tied, Health::Ok, false);
        assert!(
            Catalog::new(store.writer())
                .live_health("risk_snapshot")
                .unwrap()
                .is_empty(),
            "the corrected republish is live; its superseded generation is not"
        );
    }

    #[test]
    fn live_health_ignores_a_health_label_it_does_not_recognise() {
        // Unknown stored health labels must not seed `Ok` before a producer
        // has reported. Admit only the unhealthy labels that round-trip
        // through `Health::from_parts`.
        let (_d, store) = store();
        published(
            &store,
            "BK000",
            Some("BK000"),
            1,
            ts("2026-08-30T07:00:00Z"),
            Health::Ok,
            false,
        );
        store
            .writer()
            .execute(
                "update file_generations set health = 'quarantined' where gen_id = 1",
                [],
            )
            .unwrap();
        assert!(
            Catalog::new(store.writer())
                .live_health("risk_snapshot")
                .unwrap()
                .is_empty(),
            "a label this build cannot round-trip is not a health report"
        );
    }

    #[test]
    fn live_health_never_reads_an_archived_only_generation_as_live() {
        // A generation filed directly to archive must not supply live health.
        // Give it the newest source time so only the `archived_only` guard,
        // rather than ordering alone, can exclude it.
        let (_d, store) = store();
        published(
            &store,
            "BK000",
            Some("BK000"),
            1,
            ts("2026-08-30T08:00:00Z"),
            Health::Ok,
            false,
        );
        published(
            &store,
            "BK000",
            Some("BK000"),
            2,
            ts("2026-08-30T09:00:00Z"),
            Health::Degraded {
                reason: "never went live".into(),
            },
            true,
        );
        assert!(
            Catalog::new(store.writer())
                .live_health("risk_snapshot")
                .unwrap()
                .is_empty(),
            "an archived-only generation is never the live one"
        );
    }

    #[test]
    fn live_health_takes_the_worst_across_the_books_of_one_batch() {
        // One batch, two books, two generations: `resolve_generations`
        // resolves per (batch, book), so a batch can hold a clean book
        // and a failed one at once. It is reported ONCE, at its worst —
        // the tracker keys its load lane by batch, and the seed must not
        // hand it two rows whose order decides which survives.
        let (_d, store) = store();
        published(
            &store,
            "BK000",
            Some("BK000_A"),
            1,
            ts("2026-08-30T07:00:00Z"),
            Health::Degraded {
                reason: "one column".into(),
            },
            false,
        );
        published(
            &store,
            "BK000",
            Some("BK000_B"),
            2,
            ts("2026-08-30T07:00:00Z"),
            Health::Failed {
                reason: "torn read".into(),
            },
            false,
        );
        assert_eq!(
            Catalog::new(store.writer())
                .live_health("risk_snapshot")
                .unwrap(),
            vec![(
                "BK000".to_string(),
                Health::Failed {
                    reason: "torn read".into()
                }
            )]
        );
    }

    #[test]
    fn live_health_does_not_read_another_datasets_generations() {
        let (_d, store) = store();
        published(
            &store,
            "BK000",
            Some("BK000"),
            1,
            ts("2026-08-30T07:00:00Z"),
            Health::Degraded {
                reason: "wrong dataset".into(),
            },
            false,
        );
        assert!(
            Catalog::new(store.writer())
                .live_health("other_dataset")
                .unwrap()
                .is_empty()
        );
    }
}
