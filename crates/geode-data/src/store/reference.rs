//! Reference tables: publish a whole snapshot, skip an unchanged one, and
//! read it back live or as of an instant.
//!
//! A reference dataset is one partition — the dataset name as its batch, a
//! NULL book — so each snapshot is one generation through the same catalog
//! and publication machinery as documents and files. Removed keys leave live
//! with the generation that dropped them and stay readable as of earlier.

use crate::query::as_of::{AsOf, resolve_generations};
use crate::store::catalog::{Catalog, FileGeneration};
use crate::store::compare::staged_equals_live;
use crate::store::ddl::{self, TablePair};
use crate::store::document::days_since_epoch;
use crate::store::publish::{Partition, PublishOutcome, PublishRequest, publish_in_transaction};
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::health::Health;
use geode_core::query::ReferenceTable;
use geode_core::reference::{ConformedRows, RefColumn};
use geode_core::schema::DatasetSpec;
use std::path::PathBuf;

/// Shared staging table, used only by the single ingest writer, which
/// serializes every publish: concurrent staging would replace another
/// snapshot's rows before publication.
const STAGING_TABLE: &str = "staging_reference";

pub struct ReferencePublishRequest<'a> {
    pub dataset: &'a DatasetSpec,
    /// Source configuration name, used in the synthetic provenance path.
    pub source: &'a str,
    /// Already conformed to the declaration: `document_columns()` order.
    pub rows: &'a ConformedRows,
    pub source_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReferencePublished {
    Published {
        gen_id: i64,
        rows: usize,
        outcome: PublishOutcome,
    },
    /// Live already holds exactly these rows: nothing was published and no
    /// identifier was spent. The shared staging table was still rewritten,
    /// since the comparison reads the staged rows.
    Unchanged,
}

/// The synthetic `file_generations.path` for a snapshot: there is no file,
/// but provenance still wants one string a person can read.
fn reference_path(source: &str, dataset: &str) -> PathBuf {
    PathBuf::from(format!("reference://{source}/{dataset}"))
}

fn exec(conn: &Connection, sql: &str) -> Result<(), StoreError> {
    conn.execute_batch(sql).map_err(|source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    })
}

/// Publish a whole snapshot as one generation, or report it unchanged.
/// Only the ingest runner calls this: it owns the writer.
pub fn publish_reference(
    store: &Store,
    req: &ReferencePublishRequest,
) -> Result<ReferencePublished, StoreError> {
    let conn = store.writer();
    let ds = req.dataset;
    let batch = ds.name.clone();
    let columns = ds.document_columns();
    // A column count that disagrees with the declaration would stage
    // shifted cells under the wrong names; refuse before anything is written.
    if req.rows.columns.len() != columns.len()
        || req.rows.columns.iter().any(|c| c.len() != req.rows.rows)
    {
        return Err(StoreError::Document(format!(
            "reference rows for '{}' do not match its {} declared columns",
            ds.name,
            columns.len()
        )));
    }

    // Staging matches the payload table by position — payload columns, then
    // `batch`, `book`, `source_file_id` — because publication moves rows
    // with `select *`. Publish itself supplies `gen_id` and `source_time`.
    // `create or replace` so a previous attempt's leftovers never publish.
    let create = format!(
        "create or replace table {STAGING_TABLE} ({}, \"batch\" VARCHAR, \
         \"book\" VARCHAR, \"source_file_id\" BIGINT)",
        columns
            .iter()
            .map(|c| format!("\"{}\" {}", c.name, c.ty.sql()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    exec(conn, &create)?;
    append_rows(conn, req.rows, &batch)?;

    // Unchanged when a live generation exists and holds exactly these rows
    // (see `staged_equals_live`). A live generation, not live rows: an
    // empty snapshot is a real generation, and repeating it must not
    // publish another.
    let catalog = Catalog::new(conn);
    let live_source_time = catalog.live_source_time(&ds.name, &batch, None)?;
    let tables = TablePair::for_reference(&ds.name);
    if live_source_time.is_some()
        && staged_equals_live(conn, STAGING_TABLE, &tables.live, &columns, &batch)?
    {
        return Ok(ReferencePublished::Unchanged);
    }

    // Reserved only once the snapshot is known to publish, so an unchanged
    // poll spends no ids. Stamped onto staging before the transaction so a
    // failed publish cannot leave rows that name another generation's file.
    let file_id = catalog.reserve_file_id()?;
    let gen_id = catalog.reserve_gen_id()?;
    exec(
        conn,
        &format!("update {STAGING_TABLE} set source_file_id = {file_id}"),
    )?;

    let tx = crate::store::begin_transaction(conn)?;
    let conn = &tx;
    let catalog = Catalog::new(conn);
    let outcome = publish_in_transaction(
        conn,
        &PublishRequest {
            dataset: ds.name.clone(),
            tables: tables.clone(),
            staging_table: STAGING_TABLE.to_string(),
            partitions: vec![Partition {
                batch: batch.clone(),
                book: None,
            }],
            gen_id,
            source_time: req.source_time,
            live_source_time,
        },
    )?;
    // Dictionaries cover live and archive rows, so as-of reads and text
    // filters resolve values from any retained snapshot.
    for col in ddl::categorical_columns(ds) {
        ddl::refresh_enum(conn, &ds.name, col, &tables.live, &tables.archive)?;
    }
    catalog.record(&FileGeneration {
        file_id,
        dataset: ds.name.clone(),
        batch,
        path: reference_path(req.source, &ds.name),
        size: 0,
        mtime: req.received_at,
        source_time: req.source_time,
        gen_id,
        loaded_at: Utc::now(),
        row_count: req.rows.rows,
        // The one bookless partition: a `file_books` row with a NULL book,
        // or its freshness is unrecoverable (see `Catalog::record`).
        books: vec![None],
        archived_only: matches!(outcome, PublishOutcome::ArchivedOnly { .. }),
        health: Health::Ok,
    })?;
    crate::store::commit_transaction(tx)?;

    Ok(ReferencePublished::Published {
        gen_id,
        rows: req.rows.rows,
        outcome,
    })
}

/// Append every row to staging with a NULL `source_file_id`, filled once the
/// snapshot is known to publish.
fn append_rows(conn: &Connection, rows: &ConformedRows, batch: &str) -> Result<(), StoreError> {
    let mut app = conn
        .appender(STAGING_TABLE)
        .map_err(|source| StoreError::Sql {
            statement: format!("appender on {STAGING_TABLE}"),
            source,
        })?;
    // One buffer for the whole snapshot, cleared per row.
    let mut cells: Vec<Value> = Vec::with_capacity(rows.columns.len() + 3);
    for i in 0..rows.rows {
        cells.clear();
        cells.extend(rows.columns.iter().map(|c| cell(c, i)));
        cells.push(Value::Text(batch.to_string()));
        // The bookless partition: the column stays because partition-keyed
        // storage statements join on it.
        cells.push(Value::Null);
        cells.push(Value::Null);
        app.append_row(duckdb::appender_params_from_iter(cells.iter()))
            .map_err(|source| StoreError::Sql {
                statement: format!("append row {i} into {STAGING_TABLE}"),
                source,
            })?;
    }
    // Explicitly, and before the appender is dropped: the implicit flush on
    // drop discards its error, so a row DuckDB refused would otherwise be
    // published as a silently shorter snapshot.
    app.flush().map_err(|source| StoreError::Sql {
        statement: format!("flush {STAGING_TABLE}"),
        source,
    })
}

fn cell(col: &RefColumn, i: usize) -> Value {
    match col {
        RefColumn::Utf8(v) => v[i].clone().map_or(Value::Null, Value::Text),
        RefColumn::F64(v) => v[i].map_or(Value::Null, Value::Double),
        RefColumn::I64(v) => v[i].map_or(Value::Null, Value::BigInt),
        RefColumn::Date(v) => v[i].map_or(Value::Null, |d| Value::Date32(days_since_epoch(d))),
        RefColumn::Bool(v) => v[i].map_or(Value::Null, Value::Boolean),
    }
}

/// The reference table live or at `as_of`, cells cast to text in SQL so the
/// shell needs no column types; `None` when no generation exists then.
///
/// The generation comes from the `generations` summary, newest by
/// `(source_time, gen_id)` — the tie-break `resolve_generations` uses, which
/// under `Live` is the live generation. Reading it from the summary rather
/// than from a live row keeps an empty snapshot a table with no rows instead
/// of "nothing published".
///
/// Both statements run in one read transaction, so they see one database
/// snapshot: a publish committing between them would move the resolved
/// generation's rows to the archive and answer it with none.
pub fn read_reference(
    conn: &Connection,
    ds: &DatasetSpec,
    as_of: &AsOf,
) -> Result<Option<ReferenceTable>, StoreError> {
    let sql_err = |statement: &str| {
        let statement = statement.to_string();
        move |source| StoreError::Sql { statement, source }
    };
    let tx = conn
        .unchecked_transaction()
        .map_err(sql_err("begin transaction"))?;
    let table = read_reference_in(&tx, ds, as_of)?;
    tx.commit().map_err(sql_err("commit"))?;
    Ok(table)
}

fn read_reference_in(
    conn: &Connection,
    ds: &DatasetSpec,
    as_of: &AsOf,
) -> Result<Option<ReferenceTable>, StoreError> {
    let tables = TablePair::for_reference(&ds.name);
    let (from, gen_id, source_time) = match as_of {
        AsOf::Live => {
            let sql = "select gen_id, source_time from generations \
                       where dataset = ? and batch = ? and book is null \
                       order by source_time desc, gen_id desc limit 1";
            let mut stmt = conn.prepare(sql).map_err(|source| StoreError::Sql {
                statement: sql.into(),
                source,
            })?;
            let newest = stmt
                .query_map(duckdb::params![ds.name, ds.name], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, DateTime<Utc>>(1)?))
                })
                .and_then(|mut rows| rows.next().transpose())
                .map_err(|source| StoreError::Sql {
                    statement: sql.into(),
                    source,
                })?;
            let Some((gen_id, source_time)) = newest else {
                return Ok(None);
            };
            (tables.live.clone(), gen_id, source_time)
        }
        AsOf::At(t) => {
            let Some(r) = resolve_generations(conn, &ds.name, *t)?
                .into_iter()
                .find(|r| r.batch == ds.name && r.book.is_none())
            else {
                return Ok(None);
            };
            // The resolved generation may still be live: publication archives
            // only the outgoing generation.
            (
                format!(
                    "(select * from {} union all select * from {})",
                    tables.live, tables.archive
                ),
                r.gen_id,
                r.source_time,
            )
        }
    };

    let columns = ds.document_columns();
    let select = columns
        .iter()
        .map(|c| format!("cast(\"{}\" as varchar)", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let order = ds
        .key
        .iter()
        .map(|k| format!("\"{k}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "select {select} from {from} where batch = ? and book is null and gen_id = ? \
         order by {order}"
    );
    let err = |source| StoreError::Sql {
        statement: sql.clone(),
        source,
    };
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let width = columns.len();
    let rows = stmt
        .query_map(duckdb::params![ds.name, gen_id], |r| {
            (0..width)
                .map(|i| r.get::<_, Option<String>>(i))
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(Some(ReferenceTable {
        columns: columns.iter().map(|c| c.name.clone()).collect(),
        rows,
        gen_id,
        source_time,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::catalog::build_catalog;
    use crate::store::catalog::Catalog;
    use geode_core::reference::{ConformedRows, RefColumn, test_support::reference_dataset};
    use geode_core::schema::SchemaSpec;

    fn ds() -> DatasetSpec {
        reference_dataset()
    }

    fn store_with(ds: &DatasetSpec) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    fn conformed(rows: &[(&str, Option<&str>, Option<f64>)]) -> ConformedRows {
        ConformedRows {
            columns: vec![
                RefColumn::Utf8(rows.iter().map(|r| Some(r.0.to_string())).collect()),
                RefColumn::Utf8(rows.iter().map(|r| r.1.map(str::to_string)).collect()),
                RefColumn::F64(rows.iter().map(|r| r.2).collect()),
            ],
            rows: rows.len(),
            extra: vec![],
            missing: vec![],
        }
    }

    fn publish(
        store: &Store,
        ds: &DatasetSpec,
        rows: &ConformedRows,
        at: DateTime<Utc>,
    ) -> ReferencePublished {
        publish_reference(
            store,
            &ReferencePublishRequest {
                dataset: ds,
                source: "refdb",
                rows,
                source_time: at,
                received_at: at,
            },
        )
        .unwrap()
    }

    fn read(store: &Store, ds: &DatasetSpec, as_of: &AsOf) -> Option<ReferenceTable> {
        read_reference(&store.reader().unwrap(), ds, as_of).unwrap()
    }

    fn t(s: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + s, 0).unwrap()
    }

    #[test]
    fn a_first_snapshot_publishes_and_reads_back_live() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        let r = publish(
            &store,
            &ds,
            &conformed(&[
                ("SPX", Some("USD"), Some(100.0)),
                ("SX5E", Some("EUR"), Some(10.0)),
            ]),
            t(0),
        );
        assert!(matches!(r, ReferencePublished::Published { rows: 2, .. }));
        let table = read(&store, &ds, &AsOf::Live).unwrap();
        assert_eq!(
            table.columns,
            vec!["underlying_ref", "currency", "multiplier"]
        );
        assert_eq!(
            table.rows[0],
            vec![Some("SPX".into()), Some("USD".into()), Some("100.0".into())]
        );
        assert_eq!(table.source_time, t(0));
    }

    /// A live read resolves the generation and reads its rows from one
    /// database snapshot: a publish landing between the two would archive
    /// the resolved generation's rows and answer it with none. Concurrent
    /// publishes and reads, so each iteration is a chance at the gap.
    #[test]
    fn a_live_read_never_answers_a_generation_without_its_rows() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        let a = conformed(&[("SPX", Some("USD"), Some(100.0))]);
        let b = conformed(&[("SPX", Some("EUR"), Some(100.0))]);
        publish(&store, &ds, &a, t(0));
        let reader = store.reader().unwrap();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = std::thread::spawn({
            let ds = ds.clone();
            let done = done.clone();
            move || {
                for i in 1..60 {
                    let rows = if i % 2 == 0 { &a } else { &b };
                    publish(&store, &ds, rows, t(i));
                }
                done.store(true, std::sync::atomic::Ordering::Release);
            }
        });
        let mut reads = 0;
        while !done.load(std::sync::atomic::Ordering::Acquire) {
            let table = read_reference(&reader, &ds, &AsOf::Live).unwrap().unwrap();
            assert_eq!(
                table.rows.len(),
                1,
                "gen {} answered with {} rows after {reads} reads",
                table.gen_id,
                table.rows.len()
            );
            reads += 1;
        }
        writer.join().unwrap();
    }

    #[test]
    fn republishing_equal_rows_is_unchanged() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        let rows = conformed(&[("SPX", Some("USD"), Some(100.0))]);
        publish(&store, &ds, &rows, t(0));
        assert_eq!(
            publish(&store, &ds, &rows, t(60)),
            ReferencePublished::Unchanged
        );
        let table = read(&store, &ds, &AsOf::Live).unwrap();
        assert_eq!(table.source_time, t(0), "no new generation");
    }

    #[test]
    fn null_cells_round_trip_and_compare_equal() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        let rows = conformed(&[("SPX", None, None)]);
        publish(&store, &ds, &rows, t(0));
        assert_eq!(
            publish(&store, &ds, &rows, t(60)),
            ReferencePublished::Unchanged
        );
        let table = read(&store, &ds, &AsOf::Live).unwrap();
        assert_eq!(table.rows[0], vec![Some("SPX".into()), None, None]);
    }

    #[test]
    fn a_changed_cell_publishes_a_new_generation() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        publish(
            &store,
            &ds,
            &conformed(&[("SPX", Some("USD"), Some(100.0))]),
            t(0),
        );
        let r = publish(
            &store,
            &ds,
            &conformed(&[("SPX", Some("USD"), Some(50.0))]),
            t(60),
        );
        assert!(matches!(r, ReferencePublished::Published { .. }));
    }

    #[test]
    fn a_removed_key_leaves_live_and_stays_in_history() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        publish(
            &store,
            &ds,
            &conformed(&[("SPX", Some("USD"), None), ("SX5E", Some("EUR"), None)]),
            t(0),
        );
        publish(
            &store,
            &ds,
            &conformed(&[("SPX", Some("USD"), None)]),
            t(60),
        );
        let live = read(&store, &ds, &AsOf::Live).unwrap();
        assert_eq!(live.rows.len(), 1);
        let before = read(&store, &ds, &AsOf::At(t(30))).unwrap();
        assert_eq!(before.rows.len(), 2);
        assert_eq!(before.source_time, t(0));
    }

    #[test]
    fn nothing_published_reads_as_no_generation() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        assert_eq!(read(&store, &ds, &AsOf::Live), None);
        publish(&store, &ds, &conformed(&[("SPX", None, None)]), t(60));
        assert_eq!(
            read(&store, &ds, &AsOf::At(t(0))),
            None,
            "before the first generation"
        );
    }

    #[test]
    fn an_empty_snapshot_is_a_generation_and_repeats_unchanged() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        publish(&store, &ds, &conformed(&[("SPX", None, None)]), t(0));
        let r = publish(&store, &ds, &conformed(&[]), t(60));
        assert!(matches!(r, ReferencePublished::Published { rows: 0, .. }));
        let live = read(&store, &ds, &AsOf::Live).unwrap();
        assert!(live.rows.is_empty());
        assert_eq!(live.source_time, t(60), "an empty table, not no table");
        assert_eq!(
            publish(&store, &ds, &conformed(&[]), t(120)),
            ReferencePublished::Unchanged
        );
    }

    #[test]
    fn rows_that_do_not_match_the_declaration_are_refused_before_writing() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        let mut rows = conformed(&[("SPX", None, None)]);
        rows.columns.pop();
        let err = publish_reference(
            &store,
            &ReferencePublishRequest {
                dataset: &ds,
                source: "refdb",
                rows: &rows,
                source_time: t(0),
                received_at: t(0),
            },
        );
        assert!(matches!(err, Err(StoreError::Document(_))));
        assert_eq!(read(&store, &ds, &AsOf::Live), None);
    }

    #[test]
    fn a_reference_dataset_owns_one_table_pair_without_drift() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        assert_eq!(
            crate::store::ddl::table_pairs(&ds),
            vec![TablePair::for_reference("u")]
        );
        assert_eq!(TablePair::for_reference("u").live, "u_reference_live");
        assert_eq!(TablePair::for_reference("u").archive, "u_reference_archive");
        assert!(store.check_drift(&ds).unwrap().is_none());
    }

    #[test]
    fn the_catalog_lists_reference_generations() {
        let ds = ds();
        let (_dir, store) = store_with(&ds);
        publish(&store, &ds, &conformed(&[("SPX", Some("USD"), None)]), t(0));
        publish(
            &store,
            &ds,
            &conformed(&[("SPX", Some("EUR"), None)]),
            t(60),
        );
        let schema = SchemaSpec {
            datasets: vec![ds.clone()],
        };
        let snap = build_catalog(store.writer(), &schema, &AsOf::Live).unwrap();
        let u = snap.datasets.iter().find(|d| d.name == "u").unwrap();
        assert_eq!(u.partitions.len(), 1);
        let p = &u.partitions[0];
        assert_eq!((p.batch.as_str(), p.book.as_deref()), ("u", None));
        let gens: Vec<_> = p
            .generations
            .iter()
            .map(|g| (g.source_time, g.live, g.file_rows))
            .collect();
        assert_eq!(gens, vec![(t(0), false, Some(1)), (t(60), true, Some(1))]);
    }
}
