//! Payload-table drift, decided once at open. `apply_schema` creates a missing
//! table but never alters an existing one, and publication moves rows by
//! position, so a table whose columns differ from the configured dataset in
//! name, type, number or order would fail with a raw DuckDB error or, worse,
//! load values into the wrong columns while health stays `Ok`.
//!
//! Each managed table is compared with a probe built from the same DDL in the
//! connection's temp catalog, so DuckDB spells both sides: `TEXT`/`STRING` and
//! `VARCHAR`, `FLOAT8` and `DOUBLE`, `INT8` and `BIGINT`, `TIMESTAMPTZ` and
//! `TIMESTAMP WITH TIME ZONE` compare equal. Names compare without case, as
//! DuckDB resolves them. `duckdb_columns()` lists no system columns. A table
//! `apply_schema` just created always matches, so it is never drift.

use crate::store::StoreError;
use duckdb::Connection;
use geode_core::schema::DatasetSpec;

/// The scratch table a probe builds, in the connection's own temp catalog.
const PROBE: &str = "geode_drift_probe";
/// Differences named in a reason before the rest are counted.
const SHOWN: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub dataset: String,
    /// One line per difference, prefixed by its table, in table then column order.
    pub differences: Vec<String>,
}

impl Drift {
    /// `schema drift in '<dataset>': <diff>; delete the table or fix the dataset`.
    pub fn reason(&self) -> String {
        let mut shown: Vec<String> = self.differences.iter().take(SHOWN).cloned().collect();
        let more = self.differences.len().saturating_sub(SHOWN);
        if more > 0 {
            shown.push(format!("and {more} more"));
        }
        format!(
            "schema drift in '{}': {}; delete the table or fix the dataset",
            self.dataset,
            shown.join(", ")
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Column {
    name: String,
    ty: String,
}

/// Compare every table `apply_schema` manages for `ds` with its declaration.
/// Run after `apply_schema`, so every table exists.
pub fn check_drift(conn: &Connection, ds: &DatasetSpec) -> Result<Option<Drift>, StoreError> {
    let database: String = conn
        .query_row("select current_database()", [], |r| r.get(0))
        .map_err(|source| StoreError::Sql {
            statement: "select current_database()".into(),
            source,
        })?;
    let mut differences = Vec::new();
    for (table, sql) in super::managed_tables(ds) {
        let expected = probe_columns(conn, &table, &sql)?;
        let actual = read_columns(conn, &database, &table)?;
        differences.extend(compare(&table, &actual, &expected));
    }
    if differences.is_empty() {
        return Ok(None);
    }
    Ok(Some(Drift {
        dataset: ds.name.clone(),
        differences,
    }))
}

/// The columns `sql` would create, read back from a temp-table probe.
fn probe_columns(conn: &Connection, table: &str, sql: &str) -> Result<Vec<Column>, StoreError> {
    let create = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let Some(body) = sql.strip_prefix(&create) else {
        return Err(StoreError::Drift(format!(
            "internal: the DDL for '{table}' does not start with its CREATE"
        )));
    };
    let probe = format!("CREATE OR REPLACE TEMP TABLE {PROBE} ({body}");
    conn.execute_batch(&probe)
        .map_err(|source| StoreError::Sql {
            statement: probe.clone(),
            source,
        })?;
    let columns = read_columns(conn, "temp", PROBE);
    let drop = format!("DROP TABLE IF EXISTS temp.{PROBE}");
    conn.execute_batch(&drop)
        .map_err(|source| StoreError::Sql {
            statement: drop.clone(),
            source,
        })?;
    columns
}

fn read_columns(conn: &Connection, database: &str, table: &str) -> Result<Vec<Column>, StoreError> {
    let sql = "select column_name, data_type from duckdb_columns() \
               where database_name = ? and schema_name = 'main' \
               and lower(table_name) = lower(?) order by column_index";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    let mut stmt = conn.prepare(sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![database, table], |r| {
            Ok(Column {
                name: r.get(0)?,
                ty: r.get(1)?,
            })
        })
        .map_err(err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

/// Position by position: a missing, extra, renamed, retyped or moved column.
fn compare(table: &str, actual: &[Column], expected: &[Column]) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..actual.len().max(expected.len()) {
        let n = i + 1;
        match (actual.get(i), expected.get(i)) {
            (Some(a), Some(e)) if a.name.eq_ignore_ascii_case(&e.name) && a.ty == e.ty => {}
            (Some(a), Some(e)) => out.push(format!(
                "'{table}' column {n} is '{} {}', expected '{} {}'",
                a.name, a.ty, e.name, e.ty
            )),
            (None, Some(e)) => out.push(format!(
                "'{table}' column {n} is missing, expected '{} {}'",
                e.name, e.ty
            )),
            (Some(a), None) => out.push(format!(
                "'{table}' column {n} '{} {}' is not in the dataset",
                a.name, a.ty
            )),
            (None, None) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::store::Store;
    use crate::store::ddl::tests_support::local_dataset;

    /// A real DuckDB file holding `sql`, reopened so the check reads what was
    /// persisted, with the dataset's schema applied as `open` does.
    fn reopened(sql: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geode.duckdb");
        if !sql.is_empty() {
            Store::open(&path)
                .unwrap()
                .writer()
                .execute_batch(sql)
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        store.apply_schema(&local_dataset()).unwrap();
        (dir, store)
    }

    #[test]
    fn a_fresh_table_is_not_drift() {
        let (_d, store) = reopened("");
        assert_eq!(store.check_drift(&local_dataset()).unwrap(), None);
    }

    #[test]
    fn a_matching_table_spelled_with_aliases_and_other_case_is_not_drift() {
        let (_d, store) = reopened(
            "create table SHEETS_DOCUMENT_LIVE (
                 SHEET TEXT, line INT8, qty BIGINT, batch STRING, book VARCHAR,
                 source_file_id INT8, gen_id BIGINT, source_time TIMESTAMPTZ
             );",
        );
        assert_eq!(store.check_drift(&local_dataset()).unwrap(), None);
    }

    #[test]
    fn reordered_same_type_columns_are_drift() {
        let (_d, store) = reopened(
            "create table sheets_document_live (
                 sheet VARCHAR, qty BIGINT, line BIGINT, batch VARCHAR, book VARCHAR,
                 source_file_id BIGINT, gen_id BIGINT, source_time TIMESTAMPTZ
             );",
        );
        let drift = store.check_drift(&local_dataset()).unwrap().expect("drift");
        assert_eq!(drift.dataset, "sheets");
        assert_eq!(
            drift.differences,
            vec![
                "'sheets_document_live' column 2 is 'qty BIGINT', expected 'line BIGINT'"
                    .to_string(),
                "'sheets_document_live' column 3 is 'line BIGINT', expected 'qty BIGINT'"
                    .to_string(),
            ]
        );
        assert_eq!(
            drift.reason(),
            "schema drift in 'sheets': 'sheets_document_live' column 2 is 'qty BIGINT', \
             expected 'line BIGINT', 'sheets_document_live' column 3 is 'line BIGINT', \
             expected 'qty BIGINT'; delete the table or fix the dataset"
        );
    }

    #[test]
    fn an_added_column_is_drift() {
        let (_d, store) = reopened("");
        store
            .writer()
            .execute_batch("alter table sheets_document_archive add column surprise VARCHAR;")
            .unwrap();
        let drift = store.check_drift(&local_dataset()).unwrap().expect("drift");
        assert_eq!(
            drift.differences,
            vec![
                "'sheets_document_archive' column 9 'surprise VARCHAR' is not in the dataset"
                    .to_string()
            ]
        );
    }

    #[test]
    fn a_removed_column_is_drift() {
        let (_d, store) = reopened(
            "create table sheets_document_live (
                 sheet VARCHAR, line BIGINT, batch VARCHAR, book VARCHAR,
                 source_file_id BIGINT, gen_id BIGINT, source_time TIMESTAMPTZ
             );",
        );
        let drift = store.check_drift(&local_dataset()).unwrap().expect("drift");
        assert_eq!(
            drift.differences.first().map(String::as_str),
            Some("'sheets_document_live' column 3 is 'batch VARCHAR', expected 'qty BIGINT'")
        );
        assert_eq!(
            drift.differences.last().map(String::as_str),
            Some(
                "'sheets_document_live' column 8 is missing, expected 'source_time TIMESTAMP WITH TIME ZONE'"
            )
        );
        assert!(
            drift.reason().contains(" and 3 more; "),
            "{}",
            drift.reason()
        );
    }

    #[test]
    fn the_probe_leaves_no_table_behind() {
        let (_d, store) = reopened("");
        store.check_drift(&local_dataset()).unwrap();
        let n: i64 = store
            .writer()
            .query_row(
                "select count(*) from duckdb_tables() where table_name = 'geode_drift_probe'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }
}
