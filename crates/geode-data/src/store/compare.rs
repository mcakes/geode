//! Whether staged rows equal a live partition. Set difference both ways
//! with EXCEPT ALL: duplicates count, and NULLs compare equal in set
//! operations, so a NULL cell does not read as a change. Compared against
//! stored rows rather than a remembered hash, so it holds across restarts
//! and a failed publish never reads as unchanged.

use crate::store::StoreError;
use duckdb::Connection;
use geode_core::schema::ColumnSpec;

/// True when `staging` holds exactly the payload rows `live` holds for
/// `batch`, counting duplicates. Only the payload columns compare: the
/// storage columns (`batch`, `book`, `source_file_id`, `gen_id`,
/// `source_time`) differ between any two generations by construction.
///
/// An empty staging table equals an empty live partition, so a caller that
/// must tell "no live generation" from "a live generation with no rows"
/// gates on the catalog first.
pub(crate) fn staged_equals_live(
    conn: &Connection,
    staging: &str,
    live: &str,
    columns: &[&ColumnSpec],
    batch: &str,
) -> Result<bool, StoreError> {
    let payload = columns
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let diff = format!(
        "select (select count(*) from (select {payload} from {staging} \
                 except all select {payload} from {live} where batch = ?)) + \
                (select count(*) from (select {payload} from {live} where batch = ? \
                 except all select {payload} from {staging}))"
    );
    let differing: i64 = conn
        .query_row(&diff, duckdb::params![batch, batch], |r| r.get(0))
        .map_err(|source| StoreError::Sql {
            statement: diff.clone(),
            source,
        })?;
    Ok(differing == 0)
}
