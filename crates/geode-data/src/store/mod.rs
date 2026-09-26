//! Persistent DuckDB storage for payloads, history, and publication metadata.
//!
//! One dedicated writer connection serves ingest. Query workers use independent
//! connections on the same database; no separate in-memory copy is maintained.

pub mod catalog;
pub mod ddl;
pub mod document;
pub mod publish;
pub mod retention;
pub mod series;

pub use catalog::{AttributeConflict, Catalog, FileGeneration, FileId};
pub use publish::{Partition, PublishOutcome, PublishRequest, publish_file};
pub use retention::{RetentionPolicy, SweepReport, checkpoint, sweep};

use duckdb::Connection;
use geode_core::schema::DatasetSpec;
use std::path::{Path, PathBuf};

use ddl::TableKind;

#[derive(Debug)]
pub enum StoreError {
    Open {
        path: PathBuf,
        source: duckdb::Error,
    },
    Sql {
        statement: String,
        source: duckdb::Error,
    },
    /// A query worker thread could not be started. Reported rather than
    /// panicked, so the pool can shut down the workers it already spawned
    /// instead of leaving them detached.
    SpawnWorker { source: std::io::Error },
    /// Document validation or staging could not match the declared dataset.
    /// The message describes a document error rather than a SQL statement.
    Document(String),
    /// Series rows failed `SeriesRows::validate` before any SQL was issued.
    Series(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Open { path, source } => {
                write!(f, "opening database at {}: {source}", path.display())
            }
            StoreError::Sql { statement, source } => {
                write!(f, "executing `{statement}`: {source}")
            }
            StoreError::SpawnWorker { source } => {
                write!(f, "starting a query worker: {source}")
            }
            StoreError::Document(reason) => write!(f, "document: {reason}"),
            StoreError::Series(reason) => write!(f, "series: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// RAII rollback also covers panics caught at the worker boundary.
pub(crate) fn begin_transaction(conn: &Connection) -> Result<duckdb::Transaction<'_>, StoreError> {
    conn.unchecked_transaction()
        .map_err(|source| StoreError::Sql {
            statement: "begin transaction".into(),
            source,
        })
}

pub(crate) fn commit_transaction(tx: duckdb::Transaction<'_>) -> Result<(), StoreError> {
    tx.commit().map_err(|source| StoreError::Sql {
        statement: "commit transaction".into(),
        source,
    })
}

pub struct Store {
    writer: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let writer = Connection::open(&path).map_err(|source| StoreError::Open {
            path: path.clone(),
            source,
        })?;
        Ok(Store { writer, path })
    }

    /// The ingest writer connection. Callers serialize publication and maintenance
    /// through this connection so transactions and shared staging tables do not overlap.
    pub fn writer(&self) -> &Connection {
        &self.writer
    }

    /// A fresh read connection on the same database, for the query pool.
    pub fn reader(&self) -> Result<Connection, StoreError> {
        self.writer.try_clone().map_err(|source| StoreError::Open {
            path: self.path.clone(),
            source,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create missing tables for a dataset: one live/archive pair per declared
    /// measure grain, one pair per document dataset, or one series table and its
    /// coverage table. Repeated calls leave existing tables unchanged.
    ///
    /// This does not migrate payload schemas. An existing table with different
    /// columns can therefore fail publication with a column-count mismatch.
    pub fn apply_schema(&self, ds: &DatasetSpec) -> Result<(), StoreError> {
        if ds.is_series() {
            // Series and coverage tables are created once; neither has a live/archive pair.
            for sql in series::create_series_tables_sql(ds) {
                self.writer
                    .execute_batch(&sql)
                    .map_err(|source| StoreError::Sql {
                        statement: sql,
                        source,
                    })?;
            }
            return Ok(());
        }
        for kind in [TableKind::Live, TableKind::Archive] {
            let statements: Vec<String> = if ds.is_document() {
                vec![ddl::create_document_table_sql(ds, kind)]
            } else {
                ds.grains()
                    .into_iter()
                    .map(|g| ddl::create_table_sql(ds, g, kind))
                    .collect()
            };
            for sql in statements {
                self.writer
                    .execute_batch(&sql)
                    .map_err(|source| StoreError::Sql {
                        statement: sql,
                        source,
                    })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_a_persistent_database_that_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geode.duckdb");
        {
            let store = Store::open(&path).unwrap();
            store
                .writer()
                .execute_batch("create table probe(x integer); insert into probe values (7);")
                .unwrap();
        }
        assert!(path.exists(), "database file must be on disk");
        let store = Store::open(&path).unwrap();
        let x: i32 = store
            .writer()
            .query_row("select x from probe", [], |r| r.get(0))
            .unwrap();
        assert_eq!(x, 7, "data must survive reopen (spec §2.1)");
    }

    #[test]
    fn readers_are_independent_connections_on_the_same_database() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store
            .writer()
            .execute_batch("create table probe(x integer); insert into probe values (1),(2);")
            .unwrap();
        let reader = store.reader().unwrap();
        let n: i64 = reader
            .query_row("select count(*) from probe", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn apply_schema_creates_live_and_archive_per_declared_grain() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store
            .apply_schema(&super::ddl::tests_support::sample_dataset())
            .unwrap();

        let tables: Vec<String> = {
            let conn = store.writer();
            let mut stmt = conn
                .prepare("select table_name from information_schema.tables order by table_name")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        for expected in [
            "risk_snapshot_position_live",
            "risk_snapshot_position_archive",
            "risk_snapshot_underlying_live",
            "risk_snapshot_underlying_archive",
        ] {
            assert!(
                tables.contains(&expected.to_string()),
                "missing {expected}: {tables:?}"
            );
        }
    }

    #[test]
    fn apply_schema_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = super::ddl::tests_support::sample_dataset();
        store.apply_schema(&ds).unwrap();
        store.apply_schema(&ds).unwrap();
    }
}
