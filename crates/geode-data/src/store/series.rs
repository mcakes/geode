//! The series family's storage (timeseries spec §4.4–§4.7): one
//! bitemporal, append-only table per dataset and a coverage table beside
//! it. There is no live/archive pair and no generation: "live" is the
//! latest `received_at` per `(source, series_id, ts)`, and as-of is a
//! filter on `received_at`. `append_series` is the ONE door rows enter
//! by, whatever produced them — a fetch today, a tail later.
//!
//! Both timestamps are UTC stored as naive `TIMESTAMP`, bound and read as
//! epoch microseconds (`make_timestamp` / `epoch_us`), so no session time
//! zone can shift them. The staging table is the fixed global
//! `staging_series` — one writer, the ingest thread — for the reason
//! `docs/ingest-cold-start-handoff.md` records.

use crate::adapter::SeriesRows;
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use duckdb::Connection;
use geode_core::schema::{DatasetSpec, SERIES_COLUMNS};

pub const STAGING_TABLE: &str = "staging_series";

pub type Span = (DateTime<Utc>, DateTime<Utc>);

pub fn series_table(dataset: &str) -> String {
    format!("{dataset}_series")
}

pub fn coverage_table(dataset: &str) -> String {
    format!("{dataset}_series_coverage")
}

pub fn micros(t: DateTime<Utc>) -> i64 {
    t.timestamp_micros()
}

pub fn from_micros(us: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_micros(us).expect("a stored timestamp is in range")
}

/// The two `CREATE TABLE IF NOT EXISTS` statements a series dataset
/// owns. The column order is `SERIES_COLUMNS`, the one list every reader
/// and writer of this family shares.
pub fn create_series_tables_sql(ds: &DatasetSpec) -> Vec<String> {
    debug_assert_eq!(ds.series_columns(), &SERIES_COLUMNS);
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n  \"source\" VARCHAR,\n  \"series_id\" VARCHAR,\n  \
             \"ts\" TIMESTAMP,\n  \"received_at\" TIMESTAMP,\n  \"value\" DOUBLE,\n  \
             PRIMARY KEY (\"source\", \"series_id\", \"ts\", \"received_at\")\n);",
            series_table(&ds.name)
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n  \"source\" VARCHAR,\n  \"series_id\" VARCHAR,\n  \
             \"from_ts\" TIMESTAMP,\n  \"to_ts\" TIMESTAMP,\n  \"received_at\" TIMESTAMP\n);",
            coverage_table(&ds.name)
        ),
    ]
}

pub struct SeriesAppendRequest<'a> {
    pub dataset: &'a DatasetSpec,
    pub source: &'a str,
    pub identity: &'a str,
    pub rows: &'a SeriesRows,
    /// The half-open span this fetch covered, recorded whether or not any
    /// row was new — an empty gap is not asked for again.
    pub span: Span,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SeriesAppended {
    pub appended: usize,
    /// Rows the per-pair retention sweep (§4.7) deleted in the same
    /// transaction.
    pub swept: usize,
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

pub fn append_series(
    store: &Store,
    req: &SeriesAppendRequest,
) -> Result<SeriesAppended, StoreError> {
    // Before anything is written: malformed rows leave the store exactly
    // as it was, coverage included.
    req.rows
        .validate()
        .map_err(|e| StoreError::Series(e.message))?;
    let conn = store.writer();
    let table = series_table(&req.dataset.name);
    let coverage = coverage_table(&req.dataset.name);

    // 1. Stage as BIGINT micros: position lines the insert up, and a
    // micros column cannot be bent by a session time zone.
    let create =
        format!("create or replace table {STAGING_TABLE} (\"ts_us\" BIGINT, \"value\" DOUBLE)");
    conn.execute_batch(&create).map_err(sql_err(&create))?;
    {
        let mut app = conn
            .appender(STAGING_TABLE)
            .map_err(sql_err("appender on staging_series"))?;
        for i in 0..req.rows.len() {
            app.append_row(duckdb::params![micros(req.rows.ts[i]), req.rows.value[i]])
                .map_err(sql_err("append row into staging_series"))?;
        }
        app.flush().map_err(sql_err("flush staging_series"))?;
    }

    conn.execute_batch("begin;").map_err(sql_err("begin"))?;
    match append_in_transaction(conn, req, &table, &coverage) {
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

fn append_in_transaction(
    conn: &Connection,
    req: &SeriesAppendRequest,
    table: &str,
    coverage: &str,
) -> Result<SeriesAppended, StoreError> {
    let received = micros(req.received_at);
    // 2. Drop a staged row equal to the LIVE value for its ts, so an
    // overlapping refetch grows nothing. Live is the greatest
    // received_at per ts, expressed inline.
    let dedupe = format!(
        "delete from {STAGING_TABLE} s where exists (
             select 1 from (
                 select ts, arg_max(value, received_at) as v from {table}
                 where source = ? and series_id = ? group by ts
             ) live
             where live.ts = make_timestamp(s.ts_us) and live.v = s.value
         )"
    );
    conn.execute(&dedupe, duckdb::params![req.source, req.identity])
        .map_err(sql_err(&dedupe))?;
    // 3. Insert what remains under one received_at.
    let insert = format!(
        "insert into {table} (source, series_id, ts, received_at, value)
         select ?, ?, make_timestamp(ts_us), make_timestamp(?), value from {STAGING_TABLE}"
    );
    let appended = conn
        .execute(&insert, duckdb::params![req.source, req.identity, received])
        .map_err(sql_err(&insert))?;
    // 4. Coverage, always.
    let cover = format!(
        "insert into {coverage} (source, series_id, from_ts, to_ts, received_at)
         values (?, ?, make_timestamp(?), make_timestamp(?), make_timestamp(?))"
    );
    conn.execute(
        &cover,
        duckdb::params![
            req.source,
            req.identity,
            micros(req.span.0),
            micros(req.span.1),
            received
        ],
    )
    .map_err(sql_err(&cover))?;
    // 5. Retention for this pair (Task 6 fills this in; 0 until then).
    let swept = 0;
    Ok(SeriesAppended { appended, swept })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::tests_support::{series_dataset, series_rows, ts};

    fn fixture() -> (tempfile::TempDir, Store, DatasetSpec) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = series_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store, ds)
    }

    fn append(
        store: &Store,
        ds: &DatasetSpec,
        rows: &SeriesRows,
        span: (&str, &str),
        at: &str,
    ) -> SeriesAppended {
        append_series(
            store,
            &SeriesAppendRequest {
                dataset: ds,
                source: "demo_kdb",
                identity: "SPX.close",
                rows,
                span: (ts(span.0), ts(span.1)),
                received_at: ts(at),
            },
        )
        .unwrap()
    }

    /// `(epoch micros of ts, epoch micros of received_at, value)` for the
    /// pair, oldest first, every version — the raw table, not the live view.
    fn all_rows(store: &Store) -> Vec<(i64, i64, f64)> {
        let mut stmt = store
            .writer()
            .prepare(
                "select epoch_us(ts), epoch_us(received_at), value from series_series \
                 where source = 'demo_kdb' and series_id = 'SPX.close' order by ts, received_at",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn a_first_append_lands_every_row_stamped_with_received_at() {
        let (_d, store, ds) = fixture();
        let rows = series_rows("2026-01-05T14:30:00Z", 3, 100.0);
        let out = append(
            &store,
            &ds,
            &rows,
            ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"),
            "2026-01-06T09:00:00Z",
        );
        assert_eq!(
            out,
            SeriesAppended {
                appended: 3,
                swept: 0
            }
        );
        let got = all_rows(&store);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[0],
            (
                micros(ts("2026-01-05T14:30:00Z")),
                micros(ts("2026-01-06T09:00:00Z")),
                100.0
            )
        );
        assert_eq!(got[2].0, micros(ts("2026-01-05T14:32:00Z")));
        let cov: (i64, i64, i64) = store
            .writer()
            .query_row(
                "select epoch_us(from_ts), epoch_us(to_ts), count(*) over () from series_series_coverage",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            cov,
            (
                micros(ts("2026-01-05T00:00:00Z")),
                micros(ts("2026-01-06T00:00:00Z")),
                1
            )
        );
    }

    #[test]
    fn an_overlapping_refetch_with_the_same_values_appends_nothing_but_records_coverage() {
        let (_d, store, ds) = fixture();
        let rows = series_rows("2026-01-05T14:30:00Z", 3, 100.0);
        append(
            &store,
            &ds,
            &rows,
            ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"),
            "2026-01-06T09:00:00Z",
        );
        let again = append(
            &store,
            &ds,
            &rows,
            ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"),
            "2026-01-07T09:00:00Z",
        );
        assert_eq!(again.appended, 0);
        assert_eq!(all_rows(&store).len(), 3, "the table did not grow");
        let n: i64 = store
            .writer()
            .query_row("select count(*) from series_series_coverage", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 2, "coverage is recorded even when nothing was new");
    }

    #[test]
    fn a_corrected_value_is_one_more_row_and_the_older_one_survives() {
        let (_d, store, ds) = fixture();
        let rows = series_rows("2026-01-05T14:30:00Z", 1, 100.0);
        append(
            &store,
            &ds,
            &rows,
            ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"),
            "2026-01-06T09:00:00Z",
        );
        let corrected = series_rows("2026-01-05T14:30:00Z", 1, 101.0);
        let out = append(
            &store,
            &ds,
            &corrected,
            ("2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"),
            "2026-01-07T09:00:00Z",
        );
        assert_eq!(out.appended, 1);
        let got = all_rows(&store);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].2, got[1].2), (100.0, 101.0));
        assert!(
            got[1].1 > got[0].1,
            "the correction has the later received_at"
        );
    }

    #[test]
    fn an_empty_fetch_records_coverage_and_appends_nothing() {
        let (_d, store, ds) = fixture();
        let out = append(
            &store,
            &ds,
            &SeriesRows::default(),
            ("2026-01-03T00:00:00Z", "2026-01-04T00:00:00Z"),
            "2026-01-06T09:00:00Z",
        );
        assert_eq!(
            out,
            SeriesAppended {
                appended: 0,
                swept: 0
            }
        );
        let n: i64 = store
            .writer()
            .query_row("select count(*) from series_series_coverage", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn invalid_rows_are_refused_before_anything_is_written() {
        let (_d, store, ds) = fixture();
        let bad = SeriesRows {
            ts: vec![ts("2026-01-05T14:30:00Z")],
            value: vec![1.0, 2.0],
        };
        let err = append_series(
            &store,
            &SeriesAppendRequest {
                dataset: &ds,
                source: "demo_kdb",
                identity: "SPX.close",
                rows: &bad,
                span: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Series(_)), "{err}");
        assert!(all_rows(&store).is_empty());
        let n: i64 = store
            .writer()
            .query_row("select count(*) from series_series_coverage", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn micros_round_trip() {
        let t = ts("2026-01-05T14:30:00Z");
        assert_eq!(from_micros(micros(t)), t);
    }
}
