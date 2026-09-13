//! Publishing a parsed document (market-data spec §4.1): stage the rows
//! through DuckDB's appender, then run the same transaction a CSV file's
//! grain tables go through (`publish_file`) against the dataset's one
//! document pair. The message *is* the file: its key is the batch, its
//! book is empty, and every downstream mechanism — generations, the
//! backfill guard, as-of, retention, the freshness catalog — is reused
//! rather than reimplemented.
//!
//! Nothing here is a row (PHILOSOPHY §6): `DocumentRows` is
//! struct-of-arrays, and the appender loop reads each staged column at
//! index `i` through a plan built once before the loop.

use crate::store::catalog::{Catalog, FileGeneration};
use crate::store::ddl::{self, TablePair};
use crate::store::publish::{Partition, PublishOutcome, PublishRequest, publish_file};
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use geode_core::document::{Column, DocumentRows, Value, join_key};
use geode_core::health::Health;
use geode_core::schema::{ColumnSpec, DatasetSpec};
use std::path::PathBuf;

/// One global staging table, like `staging_raw`: the ingest runner is the
/// single writer, so two documents never stage concurrently (see
/// `docs/ingest-cold-start-handoff.md` for why that invariant matters — a
/// fixed global name is exactly what makes concurrent staging unsafe).
pub const STAGING_TABLE: &str = "staging_document";

pub struct DocumentPublishRequest<'a> {
    pub dataset: &'a DatasetSpec,
    /// The `[sources.<name>]` name, for the synthetic path and the
    /// `Published` event. Not the dataset: a source whose name differs
    /// from its dataset is ordinary (Phase 4b's MAJ-1).
    pub source: &'a str,
    pub rows: &'a DocumentRows,
    pub source_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    /// The message's size, for provenance. A document has no `stat`, so
    /// this and `received_at` stand in for a file's length and mtime.
    pub bytes: u64,
}

#[derive(Debug)]
pub struct DocumentPublished {
    pub batch: String,
    pub gen_id: i64,
    pub rows: usize,
    pub outcome: PublishOutcome,
}

/// The synthetic `file_generations.path` for a document: there is no file,
/// but provenance still wants one string a person can read, and one a
/// diagnostics row can show where a CSV load shows its filename.
pub fn document_path(source: &str, dataset: &str, batch: &str) -> PathBuf {
    PathBuf::from(format!("document://{source}/{dataset}/{batch}"))
}

/// Where one staged column's cells come from. Built once per publish, in
/// `DatasetSpec::document_columns` order, so the appender's row is in the
/// table's own column order by construction rather than by three loops
/// remembering to agree with it.
enum Cell<'a> {
    /// A key part: one value for the whole document, repeated down every
    /// row, because the key identifies the document and every row is in it.
    Key(&'a str),
    /// A document-level attribute: likewise constant down the document. It
    /// repeats rather than living in a header table of its own — a
    /// document is published, replaced and read whole, so there is no
    /// second grain to join and nothing a header table would save.
    Const(&'a Value),
    /// An axis or a value column: read at the row's own index.
    Col(&'a Column),
}

/// The source a staged cell reads, found by name across the four groups
/// `document_columns()` draws from. `DocumentRows::validate` has already
/// established that all four are present, complete and correctly typed, so
/// `None` is a programming error rather than bad input — reported as one
/// (`StoreError::Document`) instead of panicked, because this runs on the
/// ingest thread where a panic costs the whole load and says less.
fn cell_source<'a>(
    ds: &DatasetSpec,
    rows: &'a DocumentRows,
    spec: &ColumnSpec,
) -> Option<Cell<'a>> {
    if let Some(part) = ds
        .key
        .iter()
        .position(|k| k == &spec.name)
        .and_then(|p| rows.key.get(p))
    {
        return Some(Cell::Key(part));
    }
    if let Some((_, col)) = rows.axes.iter().find(|(n, _)| n == &spec.name) {
        return Some(Cell::Col(col));
    }
    if let Some((_, col)) = rows.values.iter().find(|(n, _)| n == &spec.name) {
        return Some(Cell::Col(col));
    }
    rows.attributes
        .iter()
        .find(|(n, _)| n == &spec.name)
        .map(|(_, v)| Cell::Const(v))
}

pub fn publish_document(
    store: &Store,
    req: &DocumentPublishRequest,
) -> Result<DocumentPublished, StoreError> {
    // Before anything is written, including before an id is spent: an
    // invalid document must leave the store exactly as it was, so a
    // malformed message cannot half-publish.
    req.rows
        .validate(req.dataset)
        .map_err(StoreError::Document)?;
    let conn = store.writer();
    let ds = req.dataset;
    let batch = join_key(&req.rows.key);
    let catalog = Catalog::new(conn);
    // Reserved rather than peeked, for the reasons `ingest::load` records:
    // the file id is stamped onto the staged rows before the catalog entry
    // exists, and a generation id must be spent whether or not anything is
    // ever recorded against it.
    let file_id = catalog.reserve_file_id()?;
    let gen_id = catalog.reserve_gen_id()?;

    // 1. Stage. `create or replace` so a previous attempt's leftovers can
    // never be published as this document's rows. The column list is
    // `document_columns()` order followed by the same storage columns
    // `ddl::create_document_table_sql` appends — `batch`, `book`,
    // `source_file_id` — because `publish_file` moves the staged rows with
    // `insert into live select *, gen, time from staging`: position, not
    // name, is what lines the two tables up. `gen_id` and `source_time`
    // are the two that publish itself supplies, so staging omits them.
    let columns = ds.document_columns();
    let create = format!(
        "create or replace table {STAGING_TABLE} ({}, \"batch\" VARCHAR, \
         \"book\" VARCHAR, \"source_file_id\" BIGINT)",
        columns
            .iter()
            .map(|c| format!("\"{}\" {}", c.name, c.ty.sql()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    conn.execute_batch(&create)
        .map_err(|source| StoreError::Sql {
            statement: create.clone(),
            source,
        })?;

    let rows = req.rows.rows();
    let plan: Vec<Cell> = columns
        .iter()
        .map(|spec| {
            cell_source(ds, req.rows, spec).ok_or_else(|| {
                StoreError::Document(format!(
                    "column '{}' of dataset '{}' has no staged values",
                    spec.name, ds.name
                ))
            })
        })
        .collect::<Result<_, _>>()?;
    {
        let mut app = conn
            .appender(STAGING_TABLE)
            .map_err(|source| StoreError::Sql {
                statement: format!("appender on {STAGING_TABLE}"),
                source,
            })?;
        // One buffer for the whole document, cleared per row: the appender
        // wants an owned `duckdb::types::Value` per cell, so a text cell
        // costs a `String` clone, but the Vec itself is allocated once.
        let mut cells: Vec<duckdb::types::Value> = Vec::with_capacity(plan.len() + 3);
        for i in 0..rows {
            cells.clear();
            for source in &plan {
                cells.push(match source {
                    Cell::Key(part) => duckdb::types::Value::Text((*part).to_string()),
                    Cell::Const(v) => value(v),
                    Cell::Col(col) => cell(col, i),
                });
            }
            cells.push(duckdb::types::Value::Text(batch.clone()));
            // A document's book is empty (market-data spec §4.1), and
            // empty is a NULL in a column that exists: every
            // partition-keyed statement in `store` joins on `book`.
            cells.push(duckdb::types::Value::Null);
            cells.push(duckdb::types::Value::BigInt(file_id));
            app.append_row(duckdb::appender_params_from_iter(cells.iter()))
                .map_err(|source| StoreError::Sql {
                    statement: format!("append row {i} into {STAGING_TABLE}"),
                    source,
                })?;
        }
        // Explicitly, and before the appender is dropped: the implicit
        // flush on drop discards its error, so a row DuckDB refused would
        // otherwise be published as a silently shorter document.
        app.flush().map_err(|source| StoreError::Sql {
            statement: format!("flush {STAGING_TABLE}"),
            source,
        })?;
    }

    // 2. Publish through the shared transaction. One partition: the key as
    // the batch, no book. The backfill guard reads the live source time
    // the same way `load_file` does — for the one partition this writes,
    // so a document older than what is live becomes history instead of
    // overwriting it.
    let live_source_time = catalog.live_source_time(&ds.name, &batch, None)?;
    let tables = TablePair::for_document(&ds.name);
    let outcome = publish_file(
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

    // 3. Dictionary refresh, so the query path can cast the key dimension
    // to its ENUM and the text filter's rewrite can reach it (spec §3.5,
    // §3.6; same reasoning as `load_file` step 5). No grain search here,
    // unlike a CSV load: the document family's one pair carries every
    // column the dataset declares, so there is only one table a
    // categorical column could be read from.
    for col in ddl::categorical_columns(ds) {
        ddl::refresh_enum(conn, &ds.name, col, &tables.live, &tables.archive)?;
    }

    // 4. Provenance, recorded after the rows are committed so the catalog
    // can never claim a generation the publish rolled back. A document
    // that went straight to the archive is recorded all the same — the
    // load happened — but flagged, because a generation that was never
    // live cannot be what freshness measures staleness from (§4.5).
    catalog.record(&FileGeneration {
        file_id,
        dataset: ds.name.clone(),
        batch: batch.clone(),
        path: document_path(req.source, &ds.name, &batch),
        size: req.bytes,
        mtime: req.received_at,
        source_time: req.source_time,
        gen_id,
        loaded_at: Utc::now(),
        row_count: rows,
        // The one partition this wrote, bookless — a `file_books` row with
        // a NULL book, not an absent row, or its freshness is
        // unrecoverable (see `Catalog::record`).
        books: vec![None],
        archived_only: matches!(outcome, PublishOutcome::ArchivedOnly { .. }),
        health: Health::Ok,
    })?;

    Ok(DocumentPublished {
        batch,
        gen_id,
        rows,
        outcome,
    })
}

fn cell(col: &Column, i: usize) -> duckdb::types::Value {
    match col {
        Column::F64(v) => duckdb::types::Value::Double(v[i]),
        Column::I64(v) => duckdb::types::Value::BigInt(v[i]),
        Column::Utf8(v) => duckdb::types::Value::Text(v[i].clone()),
        Column::Date(v) => duckdb::types::Value::Date32(days_since_epoch(v[i])),
    }
}

fn value(v: &Value) -> duckdb::types::Value {
    match v {
        Value::F64(x) => duckdb::types::Value::Double(*x),
        Value::I64(x) => duckdb::types::Value::BigInt(*x),
        Value::Utf8(s) => duckdb::types::Value::Text(s.clone()),
        Value::Date(d) => duckdb::types::Value::Date32(days_since_epoch(*d)),
    }
}

/// DuckDB's `DATE` is days since the epoch (`duckdb_date { days }`), which
/// `duckdb::types::Value::Date32` binds directly through the appender.
/// Staging a date as text and casting in the publish insert is the
/// alternative, and it is worse: `publish_file` moves staged rows with
/// `select *`, so a cast would force it to project column by column for
/// one family only.
fn days_since_epoch(d: chrono::NaiveDate) -> i32 {
    (d - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 is a date")).num_days()
        as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::as_of::resolve_generations;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::{
        assert_generations_match_tables, table_pairs,
        tests_support::{cvi_dataset, cvi_doc, ts},
    };
    use crate::store::retention::{RetentionPolicy, sweep};
    use geode_core::schema::DatasetSpec;

    fn fixture() -> (tempfile::TempDir, Store, DatasetSpec) {
        let (dir, store, ds) = fixture_for(cvi_dataset());
        (dir, store, ds)
    }

    fn fixture_for(ds: DatasetSpec) -> (tempfile::TempDir, Store, DatasetSpec) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store, ds)
    }

    fn publish(
        store: &Store,
        ds: &DatasetSpec,
        rows: &DocumentRows,
        at: &str,
    ) -> DocumentPublished {
        publish_document(
            store,
            &DocumentPublishRequest {
                dataset: ds,
                source: "cvi",
                rows,
                source_time: ts(at),
                received_at: ts(at),
                bytes: 1234,
            },
        )
        .unwrap()
    }

    fn live_params(store: &Store, key: &str) -> Vec<f64> {
        let mut stmt = store
            .writer()
            .prepare(
                "select param from cvi_params_document_live \
                 where underlying_ref = ? order by term, node",
            )
            .unwrap();
        stmt.query_map(duckdb::params![key], |r| r.get::<_, f64>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn a_first_publish_lands_live_with_the_key_as_batch_and_no_book() {
        let (_d, store, ds) = fixture();
        let out = publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.]),
            "2026-09-12T14:00:00Z",
        );
        assert_eq!(out.batch, "SPX.Z");
        assert_eq!(out.rows, 6);
        assert!(matches!(out.outcome, PublishOutcome::Published { rows: 6 }));
        assert_eq!(live_params(&store, "SPX.Z"), vec![1., 2., 3., 4., 5., 6.]);
        let (batch, book, generation): (String, Option<String>, i64) = store
            .writer()
            .query_row(
                "select batch, book, gen_id from generations where dataset = 'cvi_params'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (batch.as_str(), book, generation),
            ("SPX.Z", None, out.gen_id)
        );
        // The attributes ride on every row.
        let spot: f64 = store
            .writer()
            .query_row(
                "select min(spot_ref) from cvi_params_document_live",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(spot, 7650.0);
        assert_generations_match_tables(
            store.writer(),
            "cvi_params",
            &crate::store::ddl::history_of("cvi_params", &ds),
        );
    }

    #[test]
    fn a_republish_replaces_live_and_archives_the_previous_generation() {
        let (_d, store, ds) = fixture();
        let first = publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [1.; 6]),
            "2026-09-12T14:00:00Z",
        );
        let second = publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [2.; 6]),
            "2026-09-12T14:05:00Z",
        );
        assert_eq!(live_params(&store, "SPX.Z"), vec![2.; 6]);
        let archived: i64 = store
            .writer()
            .query_row(
                "select count(*) from cvi_params_document_archive where gen_id = ?",
                duckdb::params![first.gen_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(archived, 6);
        assert_ne!(first.gen_id, second.gen_id);
    }

    #[test]
    fn two_keys_are_two_partitions_that_do_not_disturb_each_other() {
        let (_d, store, ds) = fixture();
        publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [1.; 6]),
            "2026-09-12T14:00:00Z",
        );
        publish(
            &store,
            &ds,
            &cvi_doc("NDX.Z", [9.; 6]),
            "2026-09-12T14:01:00Z",
        );
        publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [2.; 6]),
            "2026-09-12T14:02:00Z",
        );
        assert_eq!(live_params(&store, "SPX.Z"), vec![2.; 6]);
        assert_eq!(live_params(&store, "NDX.Z"), vec![9.; 6]);
    }

    #[test]
    fn an_older_document_is_archived_only_and_live_is_untouched() {
        let (_d, store, ds) = fixture();
        publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [5.; 6]),
            "2026-09-12T14:05:00Z",
        );
        let out = publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [1.; 6]),
            "2026-09-12T14:00:00Z",
        );
        assert!(matches!(out.outcome, PublishOutcome::ArchivedOnly { .. }));
        assert_eq!(live_params(&store, "SPX.Z"), vec![5.; 6]);
        // Provenance says the generation happened and never went live, so
        // freshness does not measure staleness from it (§4.5).
        let flag: bool = store
            .writer()
            .query_row(
                "select archived_only from file_generations where gen_id = ?",
                duckdb::params![out.gen_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(flag);
    }

    #[test]
    fn as_of_resolves_the_generation_live_at_that_instant() {
        let (_d, store, ds) = fixture();
        let first = publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [1.; 6]),
            "2026-09-12T14:00:00Z",
        );
        publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [2.; 6]),
            "2026-09-12T14:05:00Z",
        );
        let resolved =
            resolve_generations(store.writer(), "cvi_params", ts("2026-09-12T14:02:00Z")).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].gen_id, first.gen_id);
        assert_eq!(resolved[0].book, None);
    }

    #[test]
    fn retention_by_count_sweeps_document_batches() {
        let (_d, store, ds) = fixture();
        for (i, at) in ["14:00", "14:01", "14:02"].iter().enumerate() {
            publish(
                &store,
                &ds,
                &cvi_doc("SPX.Z", [i as f64; 6]),
                &format!("2026-09-12T{at}:00Z"),
            );
        }
        // `keep_generations` counts *archived* generations (retention never
        // reads live), so keep-one leaves the archive holding the second
        // generation and evicts the first. The third is live and outside
        // the sweep entirely — which is why the summary still has two rows.
        let policy = RetentionPolicy {
            keep_generations: Some(1),
            keep_age: None,
        };
        sweep(
            store.writer(),
            &ds,
            &table_pairs(&ds),
            &policy,
            ts("2026-09-12T15:00:00Z"),
        )
        .unwrap();
        let archived: i64 = store
            .writer()
            .query_row(
                "select count(*) from cvi_params_document_archive",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(archived, 6, "one generation's rows remain archived");
        let gens: i64 = store
            .writer()
            .query_row(
                "select count(*) from generations where dataset = 'cvi_params'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gens, 2, "the oldest generation is evicted");
        assert_eq!(
            live_params(&store, "SPX.Z"),
            vec![2.; 6],
            "live is never swept"
        );
        assert_generations_match_tables(
            store.writer(),
            "cvi_params",
            &crate::store::ddl::history_of("cvi_params", &ds),
        );
    }

    #[test]
    fn the_file_generations_row_records_the_document_with_a_synthetic_path() {
        let (_d, store, ds) = fixture();
        // `received_at` deliberately later than `source_time`: a document's
        // receive time is what stands in for a file's mtime, and recording
        // the source time there instead would pass every other test here.
        let rows_in = cvi_doc("SPX.Z", [1.; 6]);
        let out = publish_document(
            &store,
            &DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &rows_in,
                source_time: ts("2026-09-12T14:00:00Z"),
                received_at: ts("2026-09-12T14:01:00Z"),
                bytes: 1234,
            },
        )
        .unwrap();
        let (path, size, rows, health, mtime, source_time): (
            String,
            i64,
            i64,
            String,
            DateTime<Utc>,
            DateTime<Utc>,
        ) = store
            .writer()
            .query_row(
                "select path, size, row_count, health, mtime, source_time \
                 from file_generations where gen_id = ?",
                duckdb::params![out.gen_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(path, "document://cvi/cvi_params/SPX.Z");
        assert_eq!((size, rows, health.as_str()), (1234, 6, "ok"));
        assert_eq!(mtime, ts("2026-09-12T14:01:00Z"), "mtime is received_at");
        assert_eq!(source_time, ts("2026-09-12T14:00:00Z"));
    }

    #[test]
    fn the_key_dimension_enum_is_refreshed_after_publish() {
        let (_d, store, ds) = fixture();
        publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [1.; 6]),
            "2026-09-12T14:00:00Z",
        );
        publish(
            &store,
            &ds,
            &cvi_doc("NDX.Z", [1.; 6]),
            "2026-09-12T14:00:00Z",
        );
        let n: i64 = store
            .writer()
            .query_row(
                "select count(*) from \
                 (select unnest(enum_range(NULL::cvi_params_underlying_ref_enum)))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn an_invalid_document_is_refused_before_anything_is_written() {
        let (_d, store, ds) = fixture();
        let mut bad = cvi_doc("SPX.Z", [1.; 6]);
        bad.values[0].1 = Column::F64(vec![1.; 5]);
        let err = publish_document(
            &store,
            &DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &bad,
                source_time: ts("2026-09-12T14:00:00Z"),
                received_at: ts("2026-09-12T14:00:00Z"),
                bytes: 0,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("value 'param' has 5 rows"),
            "{err}"
        );
        let n: i64 = store
            .writer()
            .query_row("select count(*) from file_generations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    /// What the row floor in `DocumentRows::validate` protects on this side:
    /// an empty document would archive and delete the batch's live rows,
    /// insert none, and record a generation no table holds — so the panel
    /// would read as "no document has arrived for this key" and the summary
    /// would name a generation `assert_generations_match_tables` rejects.
    #[test]
    fn an_empty_document_is_refused_and_the_live_generation_survives() {
        let (_d, store, ds) = fixture();
        publish(
            &store,
            &ds,
            &cvi_doc("SPX.Z", [7.; 6]),
            "2026-09-12T14:00:00Z",
        );
        let mut empty = cvi_doc("SPX.Z", [1.; 6]);
        empty.axes[0].1 = Column::Date(Vec::new());
        empty.axes[1].1 = Column::F64(Vec::new());
        empty.values[0].1 = Column::F64(Vec::new());
        let err = publish_document(
            &store,
            &DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &empty,
                source_time: ts("2026-09-12T14:05:00Z"),
                received_at: ts("2026-09-12T14:05:00Z"),
                bytes: 0,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("document has no rows"), "{err}");
        assert_eq!(
            live_params(&store, "SPX.Z"),
            vec![7.; 6],
            "the live generation is untouched"
        );
        let files: i64 = store
            .writer()
            .query_row("select count(*) from file_generations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "only the real publish is recorded");
        let gens: i64 = store
            .writer()
            .query_row(
                "select count(*) from generations where dataset = 'cvi_params'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gens, 1, "no generation is summarised for the refusal");
        assert_generations_match_tables(
            store.writer(),
            "cvi_params",
            &crate::store::ddl::history_of("cvi_params", &ds),
        );
    }

    #[test]
    fn a_two_column_key_joins_with_the_separator() {
        // Correlation-shaped: key = [underlying_ref, underlying2_ref].
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::schema::SchemaSpec;
        let cvi_base = r#"
[cvi_params]
family = "document"
key = ["underlying_ref", "underlying2_ref"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
[cvi_params.columns.underlying2_ref]
type = "utf8"
role = "dimension"
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", cvi_base).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("cvi_params").unwrap().clone();
        let (_d, store, _) = fixture_for(ds.clone());
        let mut rows = cvi_doc("SPX.Z", [1.; 6]);
        rows.key.push("NDX.Z".into());
        let out = publish(&store, &ds, &rows, "2026-09-12T14:00:00Z");
        assert_eq!(
            out.batch,
            geode_core::document::join_key(&["SPX.Z".to_string(), "NDX.Z".to_string()])
        );
        assert_eq!(
            geode_core::document::split_key(&out.batch),
            vec!["SPX.Z".to_string(), "NDX.Z".to_string()]
        );
    }
}
