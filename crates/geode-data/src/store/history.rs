//! Keyed bitemporal history storage: one payload table and one coverage
//! table per history dataset. A row is one observation of a key tuple at `ts`;
//! a correction appends a version under a later `received_at`, and the live
//! value of `(key, ts)` is the version with the greatest `received_at`.
//! Retention deletes superseded versions or observations beyond the history
//! window. History datasets have no live/archive pairs or generation IDs.
//!
//! Coverage records which asked key (an exact tuple, or a prefix stored with
//! trailing NULLs) was fetched over which span, whether or not any row came
//! back, so an empty answer is not asked for again.
//!
//! Both timestamps are UTC stored as naive `TIMESTAMP`, bound and read as
//! epoch microseconds (`make_timestamp` / `epoch_us`) so session time zones
//! cannot shift them. The ingest writer serializes calls to [`append_history`]
//! because they share the fixed `staging_history` table.

use crate::adapter::HistoryRows;
use crate::store::series::{cutoff, micros};
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::query::normalize_key;
use geode_core::schema::{ColumnSpec, ColumnType, DatasetSpec};

pub const STAGING_TABLE: &str = "staging_history";

/// A half-open `[from, to)` coverage span. Named apart from
/// `geode_core::query::Span`, the request vocabulary, so the two never meet
/// under one name.
pub type CoverSpan = crate::store::series::Span;

pub fn history_table(dataset: &str) -> String {
    format!("{dataset}_history")
}

pub fn history_coverage_table(dataset: &str) -> String {
    format!("{dataset}_history_coverage")
}

fn quote(name: &str) -> String {
    format!("\"{name}\"")
}

/// Create missing payload and coverage tables. Payload columns follow
/// `DatasetSpec::history_columns`: the key columns typed as declared, `ts`,
/// `received_at`, then the value columns as DOUBLE, keyed by
/// `(key…, ts, received_at)`. Coverage repeats the key columns (nullable, so
/// a prefix leaves the deeper ones NULL) with its span and `received_at`, and
/// has no key. Existing tables are not migrated.
// Any change to this DDL bumps `STORE_FORMAT` (`store::stamp`), so a
// collector from another build refuses this store instead of writing
// positionally into a different layout.
pub fn create_history_tables_sql(ds: &DatasetSpec) -> Vec<String> {
    let keys = ds.history_key();
    let values = ds.history_values();
    let mut cols: Vec<String> = keys
        .iter()
        .map(|c| format!("  {} {}", quote(&c.name), c.ty.sql()))
        .collect();
    cols.push("  \"ts\" TIMESTAMP".into());
    cols.push("  \"received_at\" TIMESTAMP".into());
    cols.extend(
        values
            .iter()
            .map(|c| format!("  {} DOUBLE", quote(&c.name))),
    );
    let pk: Vec<String> = keys
        .iter()
        .map(|c| quote(&c.name))
        .chain(["\"ts\"".to_string(), "\"received_at\"".to_string()])
        .collect();
    let mut cov: Vec<String> = keys
        .iter()
        .map(|c| format!("  {} {}", quote(&c.name), c.ty.sql()))
        .collect();
    cov.extend([
        "  \"from_ts\" TIMESTAMP".to_string(),
        "  \"to_ts\" TIMESTAMP".to_string(),
        "  \"received_at\" TIMESTAMP".to_string(),
    ]);
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{},\n  PRIMARY KEY ({})\n);",
            history_table(&ds.name),
            cols.join(",\n"),
            pk.join(", ")
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
            history_coverage_table(&ds.name),
            cov.join(",\n")
        ),
    ]
}

/// One asked key (prefix or exact, as the request named it) and the span
/// its answer covers. A `Prefix` tuple shorter than the key is stored with
/// trailing NULLs; lookups treat it as covering every deeper tuple.
#[derive(Debug, Clone, PartialEq)]
pub struct Covered {
    pub asked: Vec<String>,
    pub span: CoverSpan,
}

pub struct HistoryAppendRequest<'a> {
    pub dataset: &'a DatasetSpec,
    /// The source that answered. The tables are per dataset and do not
    /// record it.
    pub source: &'a str,
    pub rows: &'a HistoryRows,
    /// Every asked key this fetch answered, recorded whether or not any row
    /// was new: an empty answer is not asked for again.
    pub covered: Vec<Covered>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct HistoryAppended {
    pub appended: usize,
    /// Payload rows removed by retention across the dataset in the same
    /// transaction; coverage deletions are excluded from this count.
    pub swept: usize,
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// The rows' key columns in canonical spelling, column-major. Refuses a
/// shape that does not match the dataset or a key its type cannot read, so a
/// bad answer leaves the store exactly as it was, coverage included. The
/// checks and the mapping are `HistoryRows`' own, so the store and the
/// adapter boundary agree on what a valid answer is.
fn normalized_keys(
    keys: &[&ColumnSpec],
    n_values: usize,
    rows: &HistoryRows,
) -> Result<Vec<Vec<String>>, StoreError> {
    rows.check_shape(keys.len(), n_values)
        .map_err(StoreError::History)?;
    let types: Vec<ColumnType> = keys.iter().map(|c| c.ty).collect();
    rows.normalized_key_columns(&types)
        .map_err(StoreError::History)
}

/// Each covered key padded to the key's arity with NULLs, values in
/// canonical spelling, so `0.50` asked is `0.5` covered.
fn normalized_covered(
    keys: &[&ColumnSpec],
    covered: &[Covered],
) -> Result<Vec<Vec<Option<String>>>, StoreError> {
    covered
        .iter()
        .map(|c| {
            if c.asked.len() > keys.len() {
                return Err(StoreError::History(format!(
                    "covered key ({}) is longer than the {}-column key",
                    c.asked.join(" / "),
                    keys.len()
                )));
            }
            keys.iter()
                .enumerate()
                .map(|(i, spec)| match c.asked.get(i) {
                    Some(v) => normalize_key(v, spec.ty).map(Some).map_err(|e| {
                        StoreError::History(format!("covered key '{}': {e}", spec.name))
                    }),
                    None => Ok(None),
                })
                .collect()
        })
        .collect()
}

pub fn append_history(
    store: &Store,
    req: &HistoryAppendRequest,
) -> Result<HistoryAppended, StoreError> {
    let keys = req.dataset.history_key();
    let values = req.dataset.history_values();
    // Before anything is written: a malformed answer leaves the store
    // exactly as it was, coverage included.
    let key_cols = normalized_keys(&keys, values.len(), req.rows)?;
    let covers = normalized_covered(&keys, &req.covered)?;
    let conn = store.writer();

    // Stage keys as canonical text and ts as BIGINT micros: the insert
    // casts each key to its declared type, and a micros column cannot be
    // bent by a session time zone.
    let mut staged = vec!["  \"ts_us\" BIGINT".to_string()];
    staged.extend(keys.iter().map(|c| format!("  {} VARCHAR", quote(&c.name))));
    staged.extend(
        values
            .iter()
            .map(|c| format!("  {} DOUBLE", quote(&c.name))),
    );
    let create = format!(
        "create or replace table {STAGING_TABLE} (\n{}\n)",
        staged.join(",\n")
    );
    conn.execute_batch(&create).map_err(sql_err(&create))?;
    {
        let mut app = conn
            .appender(STAGING_TABLE)
            .map_err(sql_err("appender on staging_history"))?;
        for i in 0..req.rows.len() {
            let mut row = Vec::with_capacity(1 + keys.len() + values.len());
            row.push(Value::BigInt(micros(req.rows.ts[i])));
            row.extend(key_cols.iter().map(|c| Value::Text(c[i].clone())));
            row.extend(req.rows.values.iter().map(|c| Value::Double(c[i])));
            app.append_row(duckdb::appender_params_from_iter(row))
                .map_err(sql_err("append row into staging_history"))?;
        }
        app.flush().map_err(sql_err("flush staging_history"))?;
    }

    conn.execute_batch("begin;").map_err(sql_err("begin"))?;
    match append_in_transaction(conn, req, &keys, &values, &covers) {
        Ok(out) => {
            conn.execute_batch("commit;").map_err(sql_err("commit"))?;
            Ok(out)
        }
        Err(e) => {
            let _ = conn.execute_batch("rollback;");
            Err(e)
        }
    }
}

fn joined(cols: &[&ColumnSpec], f: impl Fn(&ColumnSpec) -> String, sep: &str) -> String {
    cols.iter().map(|c| f(c)).collect::<Vec<_>>().join(sep)
}

fn append_in_transaction(
    conn: &Connection,
    req: &HistoryAppendRequest,
    keys: &[&ColumnSpec],
    values: &[&ColumnSpec],
    covers: &[Vec<Option<String>>],
) -> Result<HistoryAppended, StoreError> {
    let table = history_table(&req.dataset.name);
    let coverage = history_coverage_table(&req.dataset.name);
    let received = micros(req.received_at);
    let key_list = joined(keys, |c| quote(&c.name), ", ");
    let value_list = joined(values, |c| quote(&c.name), ", ");

    // Drop a staged row equal in every value column to the live version of
    // its (key, ts), so an overlapping refetch grows nothing. Live is the
    // greatest received_at per (key, ts), expressed inline; the match is on
    // the whole key, or an equal value under another key would be dropped.
    let live_values = joined(
        values,
        |c| format!("arg_max({0}, received_at) as {0}", quote(&c.name)),
        ", ",
    );
    let key_match = joined(
        keys,
        |c| format!("live.{0} = CAST(s.{0} AS {1})", quote(&c.name), c.ty.sql()),
        " and ",
    );
    let value_match = joined(
        values,
        |c| format!("live.{0} = s.{0}", quote(&c.name)),
        " and ",
    );
    let dedupe = format!(
        "delete from {STAGING_TABLE} s where exists (
             select 1 from (
                 select {key_list}, ts, {live_values} from {table} group by {key_list}, ts
             ) live
             where {key_match} and live.ts = make_timestamp(s.ts_us) and {value_match}
         )"
    );
    conn.execute(&dedupe, []).map_err(sql_err(&dedupe))?;

    // Insert what remains under one received_at, each key cast from its
    // canonical text to the declared type.
    let casts = joined(
        keys,
        |c| format!("CAST({} AS {})", quote(&c.name), c.ty.sql()),
        ", ",
    );
    let insert = format!(
        "insert into {table} ({key_list}, ts, received_at, {value_list})
         select {casts}, make_timestamp(ts_us), make_timestamp(?), {value_list} \
         from {STAGING_TABLE}"
    );
    let appended = conn
        .execute(&insert, duckdb::params![received])
        .map_err(sql_err(&insert))?;

    // Record coverage even when deduplication leaves no new rows.
    let cover_casts = joined(keys, |c| format!("CAST(? AS {})", c.ty.sql()), ", ");
    let cover = format!(
        "insert into {coverage} ({key_list}, from_ts, to_ts, received_at)
         values ({cover_casts}, make_timestamp(?), make_timestamp(?), make_timestamp(?))"
    );
    for (asked, c) in covers.iter().zip(&req.covered) {
        let mut params: Vec<Value> = asked
            .iter()
            .map(|v| v.clone().map_or(Value::Null, Value::Text))
            .collect();
        params.extend([
            Value::BigInt(micros(c.span.0)),
            Value::BigInt(micros(c.span.1)),
            Value::BigInt(received),
        ]);
        conn.execute(&cover, duckdb::params_from_iter(params))
            .map_err(sql_err(&cover))?;
    }

    // Bound the dataset's history in the same transaction as rows and
    // coverage.
    let swept = sweep_history(conn, req.dataset, req.received_at)?;
    Ok(HistoryAppended { appended, swept })
}

/// Apply retention across a history dataset. `append_history` calls this
/// inside its transaction with the append's `received_at` as `now`; other
/// callers must supply any required transaction.
///
/// `retention` removes superseded versions whose `received_at` precedes the
/// cutoff; the live version of every `(key, ts)` survives regardless of age.
/// `history` removes payload rows with `ts` before its cutoff and coverage
/// spans ending at or before it. Returns the number of deleted payload rows.
///
/// An unrepresentable cutoff skips only that window's deletions
/// (`series::cutoff`): wrapping it could produce a future cutoff and delete
/// every row.
pub fn sweep_history(
    conn: &Connection,
    ds: &DatasetSpec,
    now: DateTime<Utc>,
) -> Result<usize, StoreError> {
    let Some(policy) = ds.series_retention else {
        return Ok(0);
    };
    let table = history_table(&ds.name);
    let mut swept = 0;
    if let Some(window) = policy.retention
        && let Some(cutoff) = cutoff(now, window)
    {
        let same_key = joined(
            &ds.history_key(),
            |c| format!("n.{0} = t.{0}", quote(&c.name)),
            " and ",
        );
        let sql = format!(
            "delete from {table} t where epoch_us(received_at) < ? \
             and exists (select 1 from {table} n where {same_key} \
                 and n.ts = t.ts and n.received_at > t.received_at)"
        );
        swept += conn
            .execute(&sql, duckdb::params![cutoff])
            .map_err(sql_err(&sql))?;
    }
    if let Some(window) = policy.history
        && let Some(cutoff) = cutoff(now, window)
    {
        let sql = format!("delete from {table} where epoch_us(ts) < ?");
        swept += conn
            .execute(&sql, duckdb::params![cutoff])
            .map_err(sql_err(&sql))?;
        let sql = format!(
            "delete from {} where epoch_us(to_ts) <= ?",
            history_coverage_table(&ds.name)
        );
        conn.execute(&sql, duckdb::params![cutoff])
            .map_err(sql_err(&sql))?;
    }
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::tests_support::{history_dataset, ts};
    use crate::store::series::micros;

    fn fixture() -> (tempfile::TempDir, Store, DatasetSpec) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = history_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store, ds)
    }

    /// `days` daily observations of `("SPX", "3M", "0.5")` at 21:00 UTC from
    /// `day0`, `vol = base + i`, `forward = 7650.0`.
    fn surface_rows(day0: &str, days: usize, base: f64) -> HistoryRows {
        let start = ts(&format!("{day0}T21:00:00Z"));
        HistoryRows {
            ts: (0..days)
                .map(|i| start + chrono::Duration::days(i as i64))
                .collect(),
            keys: vec![
                vec!["SPX".into(); days],
                vec!["3M".into(); days],
                vec!["0.5".into(); days],
            ],
            values: vec![
                (0..days).map(|i| base + i as f64).collect(),
                vec![7650.0; days],
            ],
        }
    }

    type Row = (i64, i64, String, String, f64, f64, f64);

    /// Every stored version, oldest observation first: the raw table, not
    /// the live view.
    fn all_rows(store: &Store) -> Vec<Row> {
        let mut stmt = store
            .writer()
            .prepare(
                "select epoch_us(ts), epoch_us(received_at), underlying_ref, tenor, delta, vol, \
                 forward from vol_surface_history order by ts, received_at",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn coverage_count(store: &Store) -> i64 {
        store
            .writer()
            .query_row(
                "select count(*) from vol_surface_history_coverage",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn spx(from: &str, to: &str) -> Vec<Covered> {
        vec![Covered {
            asked: vec!["SPX".into()],
            span: (ts(from), ts(to)),
        }]
    }

    #[test]
    fn the_ddl_lists_key_columns_then_ts_received_at_then_values() {
        let ds = history_dataset();
        let sql = create_history_tables_sql(&ds);
        assert_eq!(sql.len(), 2);
        assert!(sql[0].contains("vol_surface_history ("));
        assert!(sql[0].contains("\"underlying_ref\" VARCHAR"));
        assert!(sql[0].contains("\"delta\" DOUBLE"));
        assert!(sql[0].contains(
            "PRIMARY KEY (\"underlying_ref\", \"tenor\", \"delta\", \"ts\", \"received_at\")"
        ));
        let mut last = 0usize;
        for needle in [
            "\"underlying_ref\" VARCHAR",
            "\"tenor\" VARCHAR",
            "\"delta\" DOUBLE",
            "\"ts\" TIMESTAMP",
            "\"received_at\" TIMESTAMP",
            "\"vol\" DOUBLE",
            "\"forward\" DOUBLE",
        ] {
            let at = sql[0]
                .find(needle)
                .unwrap_or_else(|| panic!("{needle:?} missing from {}", sql[0]));
            assert!(at > last, "{needle:?} out of order in {}", sql[0]);
            last = at;
        }
        assert!(sql[1].contains("vol_surface_history_coverage ("));
        assert!(sql[1].contains("\"from_ts\" TIMESTAMP"));
        assert!(
            !sql[1].contains("PRIMARY KEY"),
            "coverage rows repeat freely"
        );
    }

    #[test]
    fn a_first_append_lands_rows_normalizes_f64_keys_and_records_coverage() {
        let (_d, store, ds) = fixture();
        let mut rows = surface_rows("2026-01-05", 3, 15.0);
        rows.keys[2] = vec!["0.50".into(); 3]; // the source's spelling
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: spx("2026-01-05T00:00:00Z", "2026-01-08T00:00:00Z"),
                received_at: ts("2026-01-08T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(
            out,
            HistoryAppended {
                appended: 3,
                swept: 0
            }
        );
        let got = all_rows(&store);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].4, 0.5, "the f64 key is stored as a number");
        assert_eq!(
            got[0],
            (
                micros(ts("2026-01-05T21:00:00Z")),
                micros(ts("2026-01-08T09:00:00Z")),
                "SPX".to_string(),
                "3M".to_string(),
                0.5,
                15.0,
                7650.0
            )
        );
        let cov: (Option<String>, Option<String>, Option<f64>, i64, i64) = store
            .writer()
            .query_row(
                "select underlying_ref, tenor, delta, epoch_us(from_ts), epoch_us(to_ts) \
                 from vol_surface_history_coverage",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(cov.0.as_deref(), Some("SPX"));
        assert_eq!(
            (cov.1, cov.2),
            (None, None),
            "a prefix is stored with trailing NULLs"
        );
        assert_eq!(
            (cov.3, cov.4),
            (
                micros(ts("2026-01-05T00:00:00Z")),
                micros(ts("2026-01-08T00:00:00Z"))
            )
        );
    }

    #[test]
    fn an_exact_covered_tuple_is_stored_typed_and_normalized() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 1, 15.0);
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![Covered {
                    asked: vec!["SPX".into(), "3M".into(), "0.50".into()],
                    span: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
                }],
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        let cov: (String, String, f64) = store
            .writer()
            .query_row(
                "select underlying_ref, tenor, delta from vol_surface_history_coverage",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(cov, ("SPX".to_string(), "3M".to_string(), 0.5));
    }

    #[test]
    fn a_covered_tuple_longer_than_the_key_is_refused_before_any_sql() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let err = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![Covered {
                    asked: vec!["SPX".into(), "3M".into(), "0.5".into(), "x".into()],
                    span: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
                }],
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::History(_)), "{err}");
        assert!(all_rows(&store).is_empty());
        assert_eq!(coverage_count(&store), 0);
    }

    #[test]
    fn an_overlapping_refetch_with_equal_values_appends_nothing_but_records_coverage() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 3, 15.0);
        let req = |at: &str| HistoryAppendRequest {
            dataset: &ds,
            source: "hanweck",
            rows: &rows,
            covered: spx("2026-01-05T00:00:00Z", "2026-01-08T00:00:00Z"),
            received_at: ts(at),
        };
        append_history(&store, &req("2026-01-08T09:00:00Z")).unwrap();
        let again = append_history(&store, &req("2026-01-09T09:00:00Z")).unwrap();
        assert_eq!(again.appended, 0);
        assert_eq!(all_rows(&store).len(), 3);
        assert_eq!(coverage_count(&store), 2);
    }

    #[test]
    fn a_changed_value_in_any_column_is_a_new_version_and_the_old_survives() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let mut corrected = rows.clone();
        corrected.values[1][0] = 7700.0; // forward changed, vol equal
        let covered = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: covered(),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &corrected,
                covered: covered(),
                received_at: ts("2026-01-07T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(out.appended, 1);
        assert_eq!(all_rows(&store).len(), 2);
    }

    #[test]
    fn a_third_append_of_the_original_values_re_appends_because_live_is_the_correction() {
        let (_d, store, ds) = fixture();
        let original = surface_rows("2026-01-05", 1, 15.0);
        let mut corrected = original.clone();
        corrected.values[0][0] = 15.5;
        for (r, at, appended) in [
            (&original, "2026-01-06T09:00:00Z", 1),
            (&corrected, "2026-01-07T09:00:00Z", 1),
            // Live is now the correction; the original again is a third
            // version, because the dedupe compares against live only.
            (&original, "2026-01-08T09:00:00Z", 1),
        ] {
            let out = append_history(
                &store,
                &HistoryAppendRequest {
                    dataset: &ds,
                    source: "hanweck",
                    rows: r,
                    covered: vec![],
                    received_at: ts(at),
                },
            )
            .unwrap();
            assert_eq!(out.appended, appended, "at {at}");
        }
        let vols: Vec<f64> = all_rows(&store).iter().map(|r| r.5).collect();
        assert_eq!(vols, [15.0, 15.5, 15.0]);
    }

    #[test]
    fn an_equal_value_under_another_key_still_lands() {
        let (_d, store, ds) = fixture();
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let covered = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: covered(),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        // Same ts and values, another delta: the dedupe must match on the
        // whole key, or this row would be dropped as already live.
        let mut other = rows.clone();
        other.keys[2] = vec!["0.25".into()];
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &other,
                covered: covered(),
                received_at: ts("2026-01-07T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(out.appended, 1);
        assert_eq!(all_rows(&store).len(), 2);
    }

    #[test]
    fn an_unparsable_f64_key_refuses_the_whole_append_before_any_sql() {
        let (_d, store, ds) = fixture();
        let mut rows = surface_rows("2026-01-05", 2, 15.0);
        rows.keys[2][1] = "atm".into();
        let err = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: vec![],
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::History(_)), "{err}");
        assert!(all_rows(&store).is_empty());
        assert_eq!(coverage_count(&store), 0, "coverage untouched");
    }

    #[test]
    fn a_short_value_column_is_refused_before_any_sql() {
        let (_d, store, ds) = fixture();
        let mut rows = surface_rows("2026-01-05", 2, 15.0);
        rows.values[1].pop();
        let err = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &rows,
                covered: spx("2026-01-05T00:00:00Z", "2026-01-07T00:00:00Z"),
                received_at: ts("2026-01-07T09:00:00Z"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::History(_)), "{err}");
        assert!(all_rows(&store).is_empty());
        assert_eq!(coverage_count(&store), 0, "coverage untouched");
    }

    #[test]
    fn the_history_window_sweeps_old_rows_and_coverage_and_retention_sweeps_superseded_versions() {
        let (_d, store, ds) = fixture(); // history 400d, retention 7d
        let old = surface_rows("2024-01-08", 1, 10.0);
        append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &old,
                covered: spx("2024-01-08T00:00:00Z", "2024-01-09T00:00:00Z"),
                received_at: ts("2024-01-09T09:00:00Z"),
            },
        )
        .unwrap();
        let recent = surface_rows("2026-01-05", 1, 15.0);
        let mut corrected = recent.clone();
        corrected.values[0][0] = 15.5;
        let cov = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        // The 2024 row is older than 400d from this `now`: the sweep is
        // dataset-wide, so this append's history window removes it.
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &recent,
                covered: cov(),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(out.appended, 1);
        assert_eq!(out.swept, 1, "the 2024 row by history");
        let n: i64 = store
            .writer()
            .query_row(
                "select count(*) from vol_surface_history_coverage where epoch_us(to_ts) <= ?",
                [micros(ts("2024-12-12T09:00:00Z"))],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "coverage ending before the horizon is gone");
        // Correct it 10 days later: the 2026-01-06 version is now superseded
        // and older than 7d.
        let out = append_history(
            &store,
            &HistoryAppendRequest {
                dataset: &ds,
                source: "hanweck",
                rows: &corrected,
                covered: cov(),
                received_at: ts("2026-01-16T09:00:00Z"),
            },
        )
        .unwrap();
        assert_eq!(out.appended, 1);
        assert_eq!(out.swept, 1, "the superseded 2026 version by retention");
        let got = all_rows(&store);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].5, 15.5);
        assert_eq!(coverage_count(&store), 2, "the 2026 coverage stays");
    }

    #[test]
    fn retention_keeps_a_superseded_version_inside_its_window() {
        let (_d, store, ds) = fixture(); // retention 7d
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let mut corrected = rows.clone();
        corrected.values[0][0] = 15.5;
        let cov = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        for (r, at) in [
            (&rows, "2026-01-06T09:00:00Z"),
            (&corrected, "2026-01-08T09:00:00Z"),
        ] {
            append_history(
                &store,
                &HistoryAppendRequest {
                    dataset: &ds,
                    source: "hanweck",
                    rows: r,
                    covered: cov(),
                    received_at: ts(at),
                },
            )
            .unwrap();
        }
        assert_eq!(all_rows(&store).len(), 2, "two days old, inside 7d");
    }

    #[test]
    fn the_live_version_survives_retention_whatever_its_age() {
        let (_d, store, ds) = fixture(); // retention 7d
        let rows = surface_rows("2026-01-05", 1, 15.0);
        let mut other = rows.clone();
        other.keys[2] = vec!["0.25".into()];
        let cov = || spx("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z");
        for (r, at) in [
            (&rows, "2026-01-06T09:00:00Z"),
            (&other, "2026-01-30T09:00:00Z"),
        ] {
            append_history(
                &store,
                &HistoryAppendRequest {
                    dataset: &ds,
                    source: "hanweck",
                    rows: r,
                    covered: cov(),
                    received_at: ts(at),
                },
            )
            .unwrap();
        }
        let got = all_rows(&store);
        assert_eq!(got.len(), 2, "a lone version is live, never superseded");
        assert_eq!(got[0].1, micros(ts("2026-01-06T09:00:00Z")));
    }

    #[test]
    fn a_history_dataset_has_no_live_archive_pair_and_two_managed_tables() {
        let ds = history_dataset();
        assert!(crate::store::ddl::table_pairs(&ds).is_empty());
        assert!(crate::store::ddl::history_of("vol_surface", &ds).is_empty());
        let names: Vec<String> = crate::store::managed_tables(&ds)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            names,
            ["vol_surface_history", "vol_surface_history_coverage"]
        );
    }

    #[test]
    fn a_fresh_history_dataset_does_not_drift() {
        let (_d, store, ds) = fixture();
        assert!(store.check_drift(&ds).unwrap().is_none());
    }
}
