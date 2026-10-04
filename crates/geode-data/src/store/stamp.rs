//! The store-format stamp: which on-disk layout a store was written with.
//!
//! Two processes built from different commits can share one store file (the
//! app and the background collector). A process that writes with a layout
//! the file was not created with would publish positionally into the wrong
//! columns, so the layout carries a number, `STORE_FORMAT`, kept in
//! `geode_meta`. Any change to the catalog, series, document or reference DDL
//! bumps it. Staging tables are exempt: each is created or replaced before
//! every use, so a leftover from another build is never read.
//!
//! The app (`App`) and a direct open (`Direct`) stamp after their own DDL:
//! they own the layout. The
//! collector checks before any DDL and refuses a different format, so an old
//! collector never writes into a new store or the reverse.

use std::path::Path;

use duckdb::{AccessMode, Connection, OptionalExt};

use super::StoreError;

/// The on-disk layout version this build reads and writes.
pub const STORE_FORMAT: u32 = 1;

/// The build that last stamped the store, `<version>+<GEODE_GIT_HASH>` or
/// `<version>+dev` when the build sets no hash. For diagnosis only; never
/// compared.
pub const WRITER_BUILD: &str = match std::str::from_utf8(&BUILD_BYTES) {
    Ok(build) => build,
    Err(_) => panic!("WRITER_BUILD is not UTF-8"),
};

// `concat!` takes only literals, and `option_env!` with a fallback is not
// one, so the identity is joined byte by byte at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");
const HASH: &str = match option_env!("GEODE_GIT_HASH") {
    Some(hash) => hash,
    None => "dev",
};
const BUILD_LEN: usize = VERSION.len() + 1 + HASH.len();
const BUILD_BYTES: [u8; BUILD_LEN] = {
    let mut out = [0u8; BUILD_LEN];
    let (version, hash) = (VERSION.as_bytes(), HASH.as_bytes());
    let mut i = 0;
    while i < version.len() {
        out[i] = version[i];
        i += 1;
    }
    out[i] = b'+';
    let mut j = 0;
    while j < hash.len() {
        out[i + 1 + j] = hash[j];
        j += 1;
    }
    out
};

/// The table alone, for a caller that must check the stamp before running
/// `Catalog::ensure_tables` (the collector). `Catalog`'s DDL repeats it.
pub(crate) const META_DDL: &str =
    "CREATE TABLE IF NOT EXISTS geode_meta (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);";

const READ_FORMAT: &str = "select value from geode_meta where key = 'store_format'";

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// Upsert this build's format and build identity. `geode_meta` must exist.
pub fn write(conn: &Connection) -> Result<(), StoreError> {
    let statement =
        "INSERT OR REPLACE INTO geode_meta VALUES ('store_format', ?), ('writer_build', ?)";
    conn.execute(
        statement,
        duckdb::params![STORE_FORMAT.to_string(), WRITER_BUILD],
    )
    .map(|_| ())
    .map_err(sql_err(statement))
}

/// The stamped format, if any. An unparsable value reads as format 0, which
/// no build writes, so it is always a mismatch.
fn stamped(conn: &Connection) -> Result<Option<u32>, StoreError> {
    let value: Option<String> = conn
        .query_row(READ_FORMAT, [], |r| r.get(0))
        .optional()
        .map_err(sql_err(READ_FORMAT))?;
    Ok(value.map(|v| v.trim().parse().unwrap_or(0)))
}

/// Accept a store stamped with `STORE_FORMAT`, stamp an unstamped one (the
/// first writer to open a store stamps it), and refuse any other format
/// before the caller runs DDL against it. `geode_meta` must exist.
pub fn check(conn: &Connection) -> Result<(), StoreError> {
    match stamped(conn)? {
        None => write(conn),
        Some(found) if found != STORE_FORMAT => Err(StoreError::FormatMismatch {
            found,
            expected: STORE_FORMAT,
        }),
        Some(_) => Ok(()),
    }
}

/// The format stamped on the store at `db`, read through a read-only open
/// that never creates the file. `None` when there is no file, no
/// `geode_meta` table, or no stamp row.
pub fn read_format(db: &Path) -> Result<Option<u32>, StoreError> {
    if !db.exists() {
        return Ok(None);
    }
    let open_err = |source| StoreError::Open {
        path: db.to_path_buf(),
        source,
    };
    let config = duckdb::Config::default()
        .access_mode(AccessMode::ReadOnly)
        .map_err(open_err)?;
    let conn = Connection::open_with_flags(db, config).map_err(open_err)?;
    let probe = "select count(*) from duckdb_tables() where table_name = 'geode_meta'";
    let tables: i64 = conn
        .query_row(probe, [], |r| r.get(0))
        .map_err(sql_err(probe))?;
    if tables == 0 {
        return Ok(None);
    }
    stamped(&conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    fn format_of(store: &Store) -> Option<u32> {
        store
            .writer()
            .query_row(
                "select value from geode_meta where key = 'store_format'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .map(|v| v.parse().unwrap())
    }

    #[test]
    fn a_write_stamps_the_format_and_the_build() {
        let (_d, store) = store();
        write(store.writer()).unwrap();
        assert_eq!(format_of(&store), Some(STORE_FORMAT));
        let build: String = store
            .writer()
            .query_row(
                "select value from geode_meta where key = 'writer_build'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(build, WRITER_BUILD);
    }

    #[test]
    fn a_check_stamps_an_unstamped_store_and_accepts_an_equal_one() {
        let (_d, store) = store();
        check(store.writer()).unwrap();
        assert_eq!(format_of(&store), Some(STORE_FORMAT));
        check(store.writer()).unwrap();
    }

    #[test]
    fn a_check_refuses_a_different_format_naming_both() {
        let (_d, store) = store();
        store
            .writer()
            .execute_batch(&format!(
                "insert into geode_meta values ('store_format', '{}')",
                STORE_FORMAT + 1
            ))
            .unwrap();
        let err = check(store.writer()).unwrap_err();
        assert!(matches!(err, StoreError::FormatMismatch { found, expected }
            if found == STORE_FORMAT + 1 && expected == STORE_FORMAT));
        let text = err.to_string();
        assert!(
            text.contains(&(STORE_FORMAT + 1).to_string())
                && text.contains(&STORE_FORMAT.to_string()),
            "{text}"
        );
    }

    #[test]
    fn read_format_is_none_without_a_file_or_a_stamp_and_reads_a_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.duckdb");
        assert_eq!(read_format(&db).unwrap(), None);
        assert!(!db.exists(), "read_format creates no database");
        {
            let store = Store::open(&db).unwrap();
            Catalog::new(store.writer()).ensure_tables().unwrap();
        }
        assert_eq!(read_format(&db).unwrap(), None);
        {
            let store = Store::open(&db).unwrap();
            write(store.writer()).unwrap();
        }
        assert_eq!(read_format(&db).unwrap(), Some(STORE_FORMAT));
    }
}
