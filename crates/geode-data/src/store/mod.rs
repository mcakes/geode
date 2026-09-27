//! The store: one persistent DuckDB database that is the system of record.
//! History survives relaunch and CSV ingest is paid once.
//!
//! One dedicated writer connection serves ingest; readers are independent
//! connections on the same database. No in-memory mirror: a
//! dual store doubles the coherency surface for a win nothing has measured.

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
    /// A document was refused before it reached SQL at all: the message
    /// disagreed with the dataset it claims to be (`DocumentRows::validate`,
    /// market-data spec §6.2). A variant of its own rather than a `Sql`
    /// with a fabricated statement, because there is no statement — and a
    /// caller that wants to report "the feed sent something malformed"
    /// separately from "the database refused a statement" can match on it.
    Document(String),
    /// Series rows were refused before they reached SQL at all
    /// (`SeriesRows::validate`, timeseries spec §4.6) — a variant of its
    /// own for the same reason `Document` is: there is no statement to
    /// report.
    Series(String),
    /// A scope was refused before it reached SQL at all — a variant of its
    /// own for the same reason `Document` and `Series` are: there is no
    /// statement to report.
    Scope(String),
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
            StoreError::Scope(reason) => write!(f, "scope: {reason}"),
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

    /// The single writer connection. DuckDB is single-writer/multi-reader,
    /// so every publish transaction serializes through this (spec §5.6).
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

    /// Create the tables a dataset owns, one family at a time: a live and
    /// archive pair per grain for the measure family (every grain it
    /// declares a measure or an attribute at), one such pair for the whole
    /// dataset for the document family, and for the series family no pair
    /// at all — its one append-only table plus the coverage table beside
    /// it (timeseries spec §4.4). Idempotent.
    ///
    /// `CREATE TABLE IF NOT EXISTS` never migrates an existing table, so a
    /// dataset whose column set grew since the database was written keeps
    /// the old table and fails at publish with a column-count mismatch —
    /// see `CLAUDE.md` on deleting the demo database after a schema change.
    pub fn apply_schema(&self, ds: &DatasetSpec) -> Result<(), StoreError> {
        if ds.is_series() {
            // No live/archive pair: one table plus its coverage table
            // (timeseries spec §4.4), created once for both "kinds".
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
