//! Build database catalog snapshots from the generation summary, file-load
//! metadata, and DuckDB introspection. Payload tables are not scanned.

use crate::query::as_of::{AsOf, resolve_generations};
use crate::store::StoreError;
use chrono::{DateTime, Utc};
use duckdb::Connection;
use geode_core::query::{CatalogSnapshot, DatasetCatalog, GenerationInfo, PartitionCatalog};
use geode_core::schema::{DatasetSpec, SchemaSpec};
use std::collections::HashMap;

/// Read dataset history and database usage in one transaction.
///
/// Runs on the data service thread. Reads must remain limited to metadata:
/// `generations`, `file_generations`, and DuckDB introspection functions.
/// Scanning payload rows here would stall the service request loop.
pub fn build_catalog(
    conn: &Connection,
    schema: &SchemaSpec,
    as_of: &AsOf,
) -> Result<CatalogSnapshot, StoreError> {
    build_catalog_with(conn, schema, as_of, || {})
}

fn build_catalog_with(
    conn: &Connection,
    schema: &SchemaSpec,
    as_of: &AsOf,
    mut after_dataset: impl FnMut(),
) -> Result<CatalogSnapshot, StoreError> {
    let tx = crate::store::begin_transaction(conn)?;
    let conn = &tx;
    let sizes = table_sizes(conn)?;
    let mut datasets = Vec::with_capacity(schema.datasets.len());
    for ds in &schema.datasets {
        datasets.push(dataset_catalog(conn, ds, as_of, &sizes)?);
        after_dataset();
    }
    let (database_bytes, used_blocks, block_size) = database_size(conn)?;
    let memory_bytes = memory_bytes(conn)?;
    let threads = threads(conn)?;
    crate::store::commit_transaction(tx)?;
    Ok(CatalogSnapshot {
        as_of: as_of.clone(),
        datasets,
        database_bytes,
        used_blocks,
        block_size,
        memory_bytes,
        threads,
        // The service fills source identities from fetch-worker catalog results;
        // the database does not contain the source's available-identity catalog.
        identities: Vec::new(),
    })
}

fn dataset_catalog(
    conn: &Connection,
    ds: &DatasetSpec,
    as_of: &AsOf,
    sizes: &HashMap<String, u64>,
) -> Result<DatasetCatalog, StoreError> {
    let mut partitions = partitions_for(conn, &ds.name)?;
    let loaded = file_generations_for(conn, &ds.name)?;
    for p in &mut partitions {
        for g in &mut p.generations {
            if let Some((loaded_at, rows)) = loaded.get(&g.gen_id) {
                g.loaded_at = Some(*loaded_at);
                g.file_rows = Some(*rows);
            }
        }
        // The rows above are already ordered oldest-first within a
        // partition (`partitions_for`'s query orders by source_time,
        // gen_id) — the last one is the newest by the same tie-break
        // `resolve_generations` uses, i.e. the live one.
        if let Some(newest) = p.generations.last_mut() {
            newest.live = true;
        }
    }

    if let AsOf::At(t) = as_of {
        let resolved = resolve_generations(conn, &ds.name, *t)?;
        let mut by_partition: HashMap<(String, Option<String>), i64> = HashMap::new();
        for r in resolved {
            by_partition.insert((r.batch, r.book), r.gen_id);
        }
        for p in &mut partitions {
            p.resolved_gen = by_partition
                .get(&(p.batch.clone(), p.book.clone()))
                .copied();
        }
    }

    // Count the tables declared by `table_pairs`, including grainless document
    // datasets. Series use a separate storage layout and are handled below.
    let mut live_rows = 0u64;
    let mut archive_rows = 0u64;
    if ds.is_series() {
        // Series store their history in one table with no archive partner.
        // Report that table's estimate as live rows.
        live_rows = sizes
            .get(&crate::store::series::series_table(&ds.name))
            .copied()
            .unwrap_or(0);
    } else {
        for pair in crate::store::ddl::table_pairs(ds) {
            live_rows += sizes.get(&pair.live).copied().unwrap_or(0);
            archive_rows += sizes.get(&pair.archive).copied().unwrap_or(0);
        }
    }

    // Catalog-sized, like everything else here: the coverage table holds
    // one row per fetch, never one per bar.
    let series = if ds.is_series() {
        crate::store::series::series_catalog(conn, &ds.name)?
    } else {
        Vec::new()
    };

    Ok(DatasetCatalog {
        name: ds.name.clone(),
        partitions,
        live_rows,
        archive_rows,
        series,
    })
}

/// Every partition's generations, oldest first within a partition — the
/// caller marks the last one per partition live, the same
/// `(source_time, gen_id)` tie-break `resolve_generations` uses.
fn partitions_for(conn: &Connection, dataset: &str) -> Result<Vec<PartitionCatalog>, StoreError> {
    let sql = "select batch, book, gen_id, source_time from generations \
               where dataset = ? order by batch, book, source_time, gen_id";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    let mut stmt = conn.prepare(sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![dataset], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, DateTime<Utc>>(3)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;

    let mut partitions: Vec<PartitionCatalog> = Vec::new();
    for (batch, book, gen_id, source_time) in rows {
        let same_partition = partitions
            .last()
            .is_some_and(|p| p.batch == batch && p.book == book);
        if !same_partition {
            partitions.push(PartitionCatalog {
                batch: batch.clone(),
                book: book.clone(),
                generations: Vec::new(),
                resolved_gen: None,
            });
        }
        partitions
            .last_mut()
            .expect("just pushed if empty")
            .generations
            .push(GenerationInfo {
                gen_id,
                source_time,
                loaded_at: None,
                file_rows: None,
                live: false,
            });
    }
    Ok(partitions)
}

/// Map retained generation IDs to file load times and row counts.
///
/// File-load records survive retention. The `exists` join restricts returned
/// metadata to generations still present in the summary, avoiding materializing
/// the full load history for a catalog snapshot.
///
/// Legacy storage can contain reused generation IDs. This map retains whichever
/// matching row is read last, so load metadata is ambiguous for those IDs.
/// Consumers needing exact file identity must resolve the corresponding file
/// record instead of relying on this generation-keyed map.
fn file_generations_for(
    conn: &Connection,
    dataset: &str,
) -> Result<HashMap<i64, (DateTime<Utc>, u64)>, StoreError> {
    let sql = "select fg.gen_id, fg.loaded_at, fg.row_count \
               from file_generations fg \
               where fg.dataset = ? \
                 and exists (select 1 from generations g \
                             where g.dataset = fg.dataset and g.gen_id = fg.gen_id)";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    let mut stmt = conn.prepare(sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![dataset], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, DateTime<Utc>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(err)?;
    let mut out = HashMap::new();
    for row in rows {
        let (gen_id, loaded_at, row_count) = row.map_err(err)?;
        out.insert(gen_id, (loaded_at, row_count.max(0) as u64));
    }
    Ok(out)
}

/// `estimated_size` per table in the `main` schema, over every table
/// `duckdb_tables()` knows about — a row *estimate*, not an exact
/// count; `build_catalog`'s callers label the field "rows (est.)".
/// Scoped to `schema_name = 'main'` so a same-named table in another
/// schema (`temp`, from a session-local `CREATE TEMP TABLE`) cannot
/// collide with a dataset's table in the map — `table_name` builds bare
/// names with no schema qualifier, so without this filter the later
/// `HashMap::insert` would silently keep whichever same-named table's
/// row came back last.
fn table_sizes(conn: &Connection) -> Result<HashMap<String, u64>, StoreError> {
    let sql = "select table_name, estimated_size from duckdb_tables() where schema_name = 'main'";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    let mut stmt = conn.prepare(sql).map_err(err)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(err)?;
    let mut out = HashMap::new();
    for row in rows {
        let (name, size) = row.map_err(err)?;
        out.insert(name, size.max(0) as u64);
    }
    Ok(out)
}

/// Database bytes, used blocks and maximum block size across all attached
/// databases. `pragma_database_size()` returns one row per database, so sums
/// cover the whole connection instead of selecting an arbitrary database.
fn database_size(conn: &Connection) -> Result<(u64, u64, u64), StoreError> {
    let sql = "select coalesce(sum(block_size * total_blocks), 0), \
               coalesce(sum(used_blocks), 0), coalesce(max(block_size), 0) \
               from pragma_database_size()";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    conn.query_row(sql, [], |r| {
        let bytes: i64 = r.get(0)?;
        let used_blocks: i64 = r.get(1)?;
        let block_size: i64 = r.get(2)?;
        Ok((
            bytes.max(0) as u64,
            used_blocks.max(0) as u64,
            block_size.max(0) as u64,
        ))
    })
    .map_err(err)
}

fn memory_bytes(conn: &Connection) -> Result<u64, StoreError> {
    let sql = "select coalesce(sum(memory_usage_bytes), 0) from duckdb_memory()";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    conn.query_row(sql, [], |r| r.get::<_, i64>(0))
        .map(|n| n.max(0) as u64)
        .map_err(err)
}

fn threads(conn: &Connection) -> Result<u64, StoreError> {
    // The brief's verified type is VARCHAR, but this pinned DuckDB
    // returns `current_setting('threads')` as BIGINT directly — cast to
    // VARCHAR explicitly so the read is stable regardless, then parse.
    let sql = "select current_setting('threads')::varchar";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    let text: String = conn.query_row(sql, [], |r| r.get(0)).map_err(err)?;
    text.trim().parse::<u64>().map_err(|_| StoreError::Sql {
        statement: sql.to_string(),
        source: duckdb::Error::InvalidParameterName(format!(
            "threads setting '{text}' is not an integer"
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ddl::tests_support::{cvi_dataset, cvi_doc, ts};
    use geode_core::config::{LayerDoc, merge_docs};

    #[test]
    fn catalog_datasets_share_one_snapshot() {
        let f = fixture_with_two_generations();
        let mut schema = f.schema.clone();
        schema.datasets.push(schema.datasets[0].clone());
        let reader = f.store.reader().unwrap();
        let mut changed = false;
        let snapshot = build_catalog_with(&reader, &schema, &AsOf::Live, || {
            if !changed {
                f.store
                    .writer()
                    .execute_batch(
                        "update generations set source_time = '2030-01-01T00:00:00Z'::timestamptz",
                    )
                    .unwrap();
                changed = true;
            }
        })
        .unwrap();
        assert_eq!(
            snapshot.datasets[0].partitions,
            snapshot.datasets[1].partitions
        );
        let next = build_catalog(&reader, &schema, &AsOf::Live).unwrap();
        assert_ne!(snapshot.datasets[0].partitions, next.datasets[0].partitions);
    }

    /// `risk_snapshot` with a single declared grain (Position) — matches
    /// `store::retention::tests::position_only_dataset`'s shape, kept
    /// local since that one is private to its own module.
    fn position_only_dataset() -> DatasetSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk_snapshot")
            .unwrap()
            .clone()
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: crate::store::Store,
        schema: SchemaSpec,
        /// Strictly after BK000's gen 1 (2026-08-01) and strictly before
        /// its gen 2 (2026-08-10); strictly before BK001's only
        /// generation, gen 3 (2026-08-12).
        between: DateTime<Utc>,
        /// `select count(*) from risk_snapshot_position_live` — the
        /// oracle the "row counts agree" test checks `build_catalog`
        /// against, and what `live_rows` must equal in every test here.
        live_rows_expected: u64,
    }

    /// BK000: gen 1 archived (2 rows), gen 2 live (3 rows). BK001: gen 3
    /// live only (2 rows). Direct metadata inserts control generation IDs,
    /// source times, load times and row counts independently.
    ///
    /// File IDs (11, 12, 13) differ from generation IDs (1, 2, 3), exposing a
    /// join on the wrong key. File 199 names absent generation 99, so the
    /// catalog must exclude its orphaned load record.
    fn fixture_with_two_generations() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        let ds = position_only_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);

        store
            .writer()
            .execute_batch(
                "insert into risk_snapshot_position_archive values
                   ('BK000','L','P1','C',1.0,'BK000',1,1,TIMESTAMPTZ '2026-08-01T00:00:00Z'),
                   ('BK000','L','P2','C',1.0,'BK000',1,1,TIMESTAMPTZ '2026-08-01T00:00:00Z');
                 insert into risk_snapshot_position_live values
                   ('BK000','L','P1','C',1.0,'BK000',1,2,TIMESTAMPTZ '2026-08-10T00:00:00Z'),
                   ('BK000','L','P3','C',1.0,'BK000',1,2,TIMESTAMPTZ '2026-08-10T00:00:00Z'),
                   ('BK000','L','P4','C',1.0,'BK000',1,2,TIMESTAMPTZ '2026-08-10T00:00:00Z'),
                   ('BK001','L','P5','C',1.0,'BK001',1,3,TIMESTAMPTZ '2026-08-12T00:00:00Z'),
                   ('BK001','L','P6','C',1.0,'BK001',1,3,TIMESTAMPTZ '2026-08-12T00:00:00Z');
                 insert into generations values
                   ('risk_snapshot','BK000','BK000',1,TIMESTAMPTZ '2026-08-01T00:00:00Z'),
                   ('risk_snapshot','BK000','BK000',2,TIMESTAMPTZ '2026-08-10T00:00:00Z'),
                   ('risk_snapshot','BK001','BK001',3,TIMESTAMPTZ '2026-08-12T00:00:00Z');
                 insert into file_generations values
                   (11,'risk_snapshot','BK000','/p1',100,
                    TIMESTAMPTZ '2026-08-01T00:05:00Z',TIMESTAMPTZ '2026-08-01T00:00:00Z',
                    1,TIMESTAMPTZ '2026-08-01T00:05:00Z',2,'ok',NULL,false),
                   (12,'risk_snapshot','BK000','/p2',150,
                    TIMESTAMPTZ '2026-08-10T00:05:00Z',TIMESTAMPTZ '2026-08-10T00:00:00Z',
                    2,TIMESTAMPTZ '2026-08-10T00:05:00Z',3,'ok',NULL,false),
                   (13,'risk_snapshot','BK001','/p3',120,
                    TIMESTAMPTZ '2026-08-12T00:05:00Z',TIMESTAMPTZ '2026-08-12T00:00:00Z',
                    3,TIMESTAMPTZ '2026-08-12T00:05:00Z',2,'ok',NULL,false),
                   (199,'risk_snapshot','ORPHAN','/orphan',1,
                    TIMESTAMPTZ '2026-08-13T00:05:00Z',TIMESTAMPTZ '2026-08-13T00:00:00Z',
                    99,TIMESTAMPTZ '2026-08-13T00:05:00Z',7,'ok',NULL,false);",
            )
            .unwrap();
        // `pragma_database_size()`'s `total_blocks` reflects what has
        // reached disk, not what sits in the WAL — real usage sees it
        // grow because `retention::sweep` checkpoints on its own cadence
        // (`store::checkpoint`); this fixture checkpoints once up front
        // so `database_bytes` is deterministically non-zero for every
        // test here, not just the one that does it explicitly to make
        // its own point about `estimated_size`.
        store.writer().execute_batch("checkpoint;").unwrap();

        Fixture {
            _dir: dir,
            store,
            schema,
            between: ts("2026-08-05T00:00:00Z"),
            live_rows_expected: 5,
        }
    }

    #[test]
    fn the_catalog_lists_every_partitions_generations_with_the_live_one_marked() {
        let f = fixture_with_two_generations();
        let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::Live).unwrap();
        let ds = snap
            .datasets
            .iter()
            .find(|d| d.name == "risk_snapshot")
            .unwrap();
        let bk000 = ds
            .partitions
            .iter()
            .find(|p| p.book.as_deref() == Some("BK000"))
            .unwrap();
        assert_eq!(
            bk000
                .generations
                .iter()
                .map(|g| (g.gen_id, g.live))
                .collect::<Vec<_>>(),
            vec![(1, false), (2, true)]
        );
        assert_eq!(bk000.resolved_gen, None, "live: nothing resolved");
        // Check both generations' values so joining an arbitrary file-load row
        // instead of matching the generation cannot pass.
        assert_eq!(
            bk000.generations[0].loaded_at,
            Some(ts("2026-08-01T00:05:00Z")),
            "gen 1's own loaded_at"
        );
        assert_eq!(
            bk000.generations[0].file_rows,
            Some(2),
            "gen 1's own file, not gen 2's"
        );
        assert_eq!(
            bk000.generations[1].loaded_at,
            Some(ts("2026-08-10T00:05:00Z")),
            "gen 2's own loaded_at"
        );
        assert_eq!(
            bk000.generations[1].file_rows,
            Some(3),
            "gen 2's own file, not gen 1's"
        );
        assert_eq!(ds.live_rows, f.live_rows_expected);
        assert!(
            snap.database_bytes > 0
                && snap.block_size > 0
                && snap.memory_bytes > 0
                && snap.threads > 0
        );
    }

    #[test]
    fn under_an_as_of_the_resolved_generation_is_named_per_partition() {
        let f = fixture_with_two_generations();
        let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::At(f.between)).unwrap();
        let ds = &snap.datasets[0];
        let bk000 = ds
            .partitions
            .iter()
            .find(|p| p.book.as_deref() == Some("BK000"))
            .unwrap();
        assert_eq!(bk000.resolved_gen, Some(1));
    }

    #[test]
    fn the_row_counts_agree_with_duckdb_by_execution() {
        // The estimate equals the exact count for this checkpointed fixture,
        // which performs no deletes. Deletes and republishes may leave estimates
        // above the surviving row count; this test does not require exactness there.
        let f = fixture_with_two_generations();
        // Checkpoint explicitly: this comparison requires persisted table metadata.
        f.store.writer().execute_batch("checkpoint;").unwrap();
        let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::Live).unwrap();
        let live: i64 = f
            .store
            .writer()
            .query_row(
                "select count(*) from risk_snapshot_position_live",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(snap.datasets[0].live_rows, live as u64);
        // The fixture inserted two archived rows.
        assert_eq!(snap.datasets[0].archive_rows, 2);
    }

    /// File metadata must be limited to retained generations. The orphaned
    /// file record (generation 99, file 199) is excluded, leaving exactly the
    /// three generations present in the summary.
    #[test]
    fn file_generations_for_excludes_a_row_whose_generation_no_longer_exists() {
        let f = fixture_with_two_generations();
        let loaded = file_generations_for(f.store.writer(), "risk_snapshot").unwrap();
        assert_eq!(
            loaded.len(),
            3,
            "exactly the three generations `generations` still names, not the orphan too"
        );
        assert!(
            !loaded.contains_key(&99),
            "the orphaned file_generations row must not be read"
        );
    }

    /// The production shape (`store::publish`'s `generation_summary_insert`):
    /// one file publishes every book it touches under a single `gen_id`
    /// and `source_time`, and `book` can be NULL — a real partition, not
    /// a missing one. Two batches isolate either half of the `(batch, book)`
    /// partition key:
    ///
    /// - `EOD/BK000` (2 generations), `EOD/BK001`, and `EOD/NULL` all
    ///   share batch `"EOD"` — comparing `batch` alone collapses these
    ///   three into one partition.
    /// - `EOD/NULL` and `FOLLOWUP/NULL` share book `None` and sort
    ///   adjacently (`order by batch, book, …`, NULL last within a
    ///   batch, `"EOD" < "FOLLOWUP"`) — comparing `book` alone collapses
    ///   these two into one partition instead.
    ///
    /// Only summary rows are needed: both `partitions_for` and
    /// `resolve_generations` read `generations` without scanning payloads.
    fn fixture_one_batch_many_books() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        let ds = position_only_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);

        store
            .writer()
            .execute_batch(
                "insert into generations values
                   ('risk_snapshot','EOD','BK000',1,TIMESTAMPTZ '2026-08-01T00:00:00Z'),
                   ('risk_snapshot','EOD','BK000',2,TIMESTAMPTZ '2026-08-10T00:00:00Z'),
                   ('risk_snapshot','EOD','BK001',2,TIMESTAMPTZ '2026-08-10T00:00:00Z'),
                   ('risk_snapshot','EOD',NULL,2,TIMESTAMPTZ '2026-08-10T00:00:00Z'),
                   ('risk_snapshot','FOLLOWUP',NULL,6,TIMESTAMPTZ '2026-08-11T00:00:00Z');",
            )
            .unwrap();

        Fixture {
            _dir: dir,
            store,
            schema,
            between: ts("2026-08-05T00:00:00Z"),
            live_rows_expected: 0,
        }
    }

    #[test]
    fn partitions_group_by_batch_and_book_together_not_either_alone() {
        let f = fixture_one_batch_many_books();
        let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::Live).unwrap();
        let ds = &snap.datasets[0];
        let names: std::collections::BTreeSet<(String, Option<String>)> = ds
            .partitions
            .iter()
            .map(|p| (p.batch.clone(), p.book.clone()))
            .collect();
        assert_eq!(
            names,
            std::collections::BTreeSet::from([
                ("EOD".to_string(), Some("BK000".to_string())),
                ("EOD".to_string(), Some("BK001".to_string())),
                ("EOD".to_string(), None),
                ("FOLLOWUP".to_string(), None),
            ]),
            "four distinct (batch, book) partitions — neither half of the key alone"
        );
    }

    #[test]
    fn the_bookless_partition_is_kept_as_its_own_partition_with_its_generation_live() {
        let f = fixture_one_batch_many_books();
        let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::Live).unwrap();
        let ds = &snap.datasets[0];
        let bookless = ds
            .partitions
            .iter()
            .find(|p| p.batch == "EOD" && p.book.is_none())
            .expect("EOD's bookless partition must be present, not dropped");
        assert_eq!(bookless.generations.len(), 1);
        assert_eq!(bookless.generations[0].gen_id, 2);
        assert!(bookless.generations[0].live);
    }

    #[test]
    fn under_an_as_of_the_bookless_partition_resolves_too() {
        let f = fixture_one_batch_many_books();
        let at = ts("2026-08-15T00:00:00Z"); // after every generation
        let snap = build_catalog(f.store.writer(), &f.schema, &AsOf::At(at)).unwrap();
        let ds = &snap.datasets[0];
        let bookless = ds
            .partitions
            .iter()
            .find(|p| p.batch == "EOD" && p.book.is_none())
            .unwrap();
        assert_eq!(bookless.resolved_gen, Some(2));
    }

    /// Each published document appears as a partition with `book: None`.
    /// Decode each partition's batch with `split_key` to verify that the catalog
    /// reports the two original document keys.
    #[test]
    fn a_document_datasets_partitions_split_back_to_their_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        crate::store::document::publish_document(
            &store,
            &crate::store::document::DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("SPX.Z", [1.; 6]),
                source_time: ts("2026-09-12T14:00:00Z"),
                received_at: ts("2026-09-12T14:00:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        crate::store::document::publish_document(
            &store,
            &crate::store::document::DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("NDX.Z", [2.; 6]),
                source_time: ts("2026-09-12T14:01:00Z"),
                received_at: ts("2026-09-12T14:01:00Z"),
                bytes: 0,
            },
        )
        .unwrap();

        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        // `estimated_size` equals `count(*)` on a freshly checkpointed
        // table and is an estimate otherwise (see
        // `the_row_counts_agree_with_duckdb_by_execution`), and
        // `publish_document` does not checkpoint — so the row-count
        // assertion below states its own precondition.
        store.writer().execute_batch("checkpoint;").unwrap();
        let snap = build_catalog(store.writer(), &schema, &AsOf::Live).unwrap();
        let ds_catalog = &snap.datasets[0];
        // Document counts come from their tables despite having no grains:
        // two keys with six live rows each and no archived rows.
        assert_eq!(
            (ds_catalog.live_rows, ds_catalog.archive_rows),
            (12, 0),
            "two documents of six rows each, both still live"
        );
        let mut keys: Vec<Vec<String>> = ds_catalog
            .partitions
            .iter()
            .map(|p| {
                assert_eq!(p.book, None, "a document partition is always bookless");
                assert_eq!(p.generations.len(), 1);
                assert!(p.generations[0].live, "the only generation is live");
                geode_core::document::split_key(&p.batch)
            })
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![vec!["NDX.Z".to_string()], vec!["SPX.Z".to_string()],]
        );
    }

    /// Series catalog entries group coverage by `(identity, source)` and
    /// report the hull of fetched spans. Row estimates come from the series
    /// table even though the dataset has no live/archive table pair.
    #[test]
    fn a_series_datasets_catalog_lists_its_pairs_from_coverage() {
        use crate::store::ddl::tests_support::{series_dataset, series_rows};
        use crate::store::series::{SeriesAppendRequest, append_series};

        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        let ds = series_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        let append = |identity: &str, start: &str, from: &str, to: &str, received: &str| {
            append_series(
                &store,
                &SeriesAppendRequest {
                    dataset: &ds,
                    source: "kdb_hist",
                    identity,
                    rows: &series_rows(start, 3, 1.0),
                    span: (ts(from), ts(to)),
                    received_at: ts(received),
                },
            )
            .unwrap()
        };
        // Two fetches for SPX.close, one for VIX.
        append(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
            "2026-01-06T09:00:00Z",
        );
        append(
            "SPX.close",
            "2026-01-06T00:00:00Z",
            "2026-01-06T00:00:00Z",
            "2026-01-07T00:00:00Z",
            "2026-01-07T09:00:00Z",
        );
        append(
            "VIX",
            "2026-01-05T00:00:00Z",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
            "2026-01-06T09:00:00Z",
        );

        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        // Same precondition the document test states: `estimated_size`
        // equals `count(*)` only on a checkpointed table.
        store.writer().execute_batch("checkpoint;").unwrap();
        let snap = build_catalog(store.writer(), &schema, &AsOf::Live).unwrap();
        let ds_catalog = &snap.datasets[0];
        assert_eq!(
            (ds_catalog.live_rows, ds_catalog.archive_rows),
            (9, 0),
            "three appends of three bars; a series dataset has no archive"
        );
        let pairs: Vec<(&str, &str, u64)> = ds_catalog
            .series
            .iter()
            .map(|s| (s.identity.as_str(), s.source.as_str(), s.fetches))
            .collect();
        assert_eq!(
            pairs,
            vec![("SPX.close", "kdb_hist", 2), ("VIX", "kdb_hist", 1)],
            "one row per pair, ordered by identity"
        );
        let spx = &ds_catalog.series[0];
        assert_eq!(
            (spx.from, spx.to),
            (ts("2026-01-05T00:00:00Z"), ts("2026-01-07T00:00:00Z")),
            "the hull of both fetched spans"
        );
        assert_eq!(spx.latest_received_at, ts("2026-01-07T09:00:00Z"));
        // Every other family reports no series rows at all.
        let mut with_document = SchemaSpec::default();
        with_document.datasets.push(cvi_dataset());
        let other = build_catalog(store.writer(), &with_document, &AsOf::Live).unwrap();
        assert!(other.datasets[0].series.is_empty());
    }
}
