//! What the database holds (Phase 4b spec §4.5): `Request::Catalog`'s
//! implementation, built from the `generations` summary table,
//! `file_generations`, and DuckDB's own introspection functions.

use crate::query::as_of::{AsOf, resolve_generations};
use crate::store::StoreError;
use chrono::{DateTime, Utc};
use duckdb::Connection;
use geode_core::query::{CatalogSnapshot, DatasetCatalog, GenerationInfo, PartitionCatalog};
use geode_core::schema::{DatasetSpec, SchemaSpec};
use std::collections::HashMap;

/// What the database holds, as of the moment it is read.
///
/// **Runs on the data service thread** (the plan's ruling, not a pool
/// worker): every query below is catalog-sized — the `generations`
/// summary table, `file_generations`, and DuckDB's own introspection
/// functions (`duckdb_tables()`, `pragma_database_size()`,
/// `duckdb_memory()`) — none of which scans a data table's rows. A
/// data-table scan here would stall the request loop and is forbidden.
pub fn build_catalog(
    conn: &Connection,
    schema: &SchemaSpec,
    as_of: &AsOf,
) -> Result<CatalogSnapshot, StoreError> {
    let sizes = table_sizes(conn)?;
    let mut datasets = Vec::with_capacity(schema.datasets.len());
    for ds in &schema.datasets {
        datasets.push(dataset_catalog(conn, ds, as_of, &sizes)?);
    }
    let (database_bytes, used_blocks, block_size) = database_size(conn)?;
    let memory_bytes = memory_bytes(conn)?;
    let threads = threads(conn)?;
    Ok(CatalogSnapshot {
        as_of: as_of.clone(),
        datasets,
        database_bytes,
        used_blocks,
        block_size,
        memory_bytes,
        threads,
        // Filled by `DataService::catalog` from the fetch workers'
        // answers: nothing about a source's catalogue is in the
        // database, so this read cannot know it (timeseries spec §5.5).
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

    // Per TABLE, not per grain. A document dataset has no grain at all
    // (`grains()` is empty for it), so summing over grains reported 0
    // live and 0 archive rows however many rows the dataset held — the
    // diagnostics data section painted `0` beside a real list of live
    // partitions, which reads as "the partitions are empty" rather than
    // "this counter cannot see them". `table_pairs` is the one place
    // either family's table set is named (`store::ddl`), the same list
    // `apply_schema` created and `history_of` resolves over, so this
    // count cannot drift from what exists.
    let mut live_rows = 0u64;
    let mut archive_rows = 0u64;
    if ds.is_series() {
        // A series dataset has no live/archive pair at all (timeseries
        // spec §4.4), so `table_pairs` is empty for it and the loop
        // below would report 0 rows however much history it holds. Its
        // one table is the live side; there is no archive.
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

/// `gen_id -> (loaded_at, row_count)`, bounded to the generations
/// `generations` still names for `dataset` — never every row
/// `file_generations` has ever recorded.
///
/// `file_generations` is never pruned (`generations` is, by
/// `retention::sweep`'s reconciliation): one row per file ever loaded,
/// forever. An unqualified `select … from file_generations where
/// dataset = ?` is still catalog-*shaped* SQL, but its cost grows with
/// the database's whole history rather than with what the snapshot can
/// display, which is what "catalog-sized" is supposed to rule out — on
/// a multi-year desk this can be orders of magnitude more rows than the
/// handful of generations any partition actually keeps. The `exists`
/// join below bounds the read to exactly the generations already
/// surviving in `partitions_for`'s result, and it survives retention
/// pruning for free since it reads the swept table as the boundary.
///
/// A legacy database written before the `gen_id` sequence existed can
/// hold two generations of one partition sharing an id
/// (`query::as_of::resolve_generations`'s doc comment); on such a
/// database `out.insert` is last-write-wins over an unordered read, so
/// this map is not authoritative about *which* of the two a shared
/// `gen_id` reports `loaded_at`/`file_rows` for. Not worth a query
/// change — a caller needing that precision should join by `file_id`.
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

/// `(database_bytes, used_blocks, block_size)` from
/// `pragma_database_size()`.
///
/// `pragma_database_size()` returns one row **per attached database**
/// (its first column, `database_name`, is why) — `query_row` on the raw
/// three-column select would silently take whichever row DuckDB happens
/// to return first, with no `where` and no ordering. Geode attaches
/// exactly one database today, so that was latent, not wrong yet; a
/// future second `ATTACH` (a read replica, an extension) would have
/// retargeted the number without changing a single call site. Summing
/// is stable regardless of how many rows come back, and costs nothing
/// extra for the one-row case this runs against today.
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
    /// live only (2 rows). `generations` and `file_generations` are
    /// populated directly (not through `publish_file`) so the test
    /// controls `gen_id`, `source_time`, `loaded_at` and `row_count`
    /// exactly.
    ///
    /// `file_generations`' own `file_id`s (11, 12, 13) are deliberately
    /// **not** the same as the `gen_id`s (1, 2, 3) they name: review
    /// round 1 MAJ-1 found that with `file_id == gen_id`, a join keyed
    /// on the wrong column still happened to line up, and no assertion
    /// checked the actual `loaded_at`/`file_rows` *values* to notice.
    /// One more `file_generations` row (file_id 199, gen_id 99) has no
    /// matching `generations` row at all — an orphan `file_generations`
    /// never reconciled away, the shape MAJ-2's bound exists for.
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
        // By value, not just presence (review round 1 MAJ-1): both
        // generations, so a join that takes "any row" rather than the
        // one keyed by this exact `gen_id` cannot pass by accident.
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
        // `estimated_size` is an estimate; on a freshly checkpointed
        // table it equals `count(*)` (verified by execution 2026-09-08).
        // This only verifies the easy direction: the fixture never
        // deletes, so it says nothing about `estimated_size` diverging
        // from `count(*)` after a delete that has not been vacuumed —
        // exactly what a republish does (`store::publish`'s outgoing
        // generation is deleted from live before the incoming one is
        // inserted). That gap is consistent with the field being
        // documented as an estimate, not a defect this test should
        // close.
        let f = fixture_with_two_generations();
        // Redundant with `fixture_with_two_generations`'s own
        // up-front checkpoint (kept for this test's own documentation
        // value: this is the assertion that specifically depends on a
        // checkpointed table, so it states the precondition itself
        // rather than relying on the fixture silently having done it).
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
        // MIN-1: `archive_rows` had no assertion anywhere and no
        // harness entry defended it — the fixture's own oracle is the 2
        // rows inserted into `risk_snapshot_position_archive`.
        assert_eq!(snap.datasets[0].archive_rows, 2);
    }

    /// Review round 1 MAJ-2: `file_generations_for` must read only the
    /// generations `generations` still names, not every row
    /// `file_generations` has ever accumulated. The fixture's orphan
    /// row (`gen_id` 99, `file_id` 199) has no matching `generations`
    /// row; a correct read excludes it, so the map holds exactly the
    /// three surviving generations, not four.
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
    /// a missing one (review round 1 MAJ-3). Two batches so a test can
    /// isolate either half of the `(batch, book)` partition key:
    ///
    /// - `EOD/BK000` (2 generations), `EOD/BK001`, and `EOD/NULL` all
    ///   share batch `"EOD"` — comparing `batch` alone collapses these
    ///   three into one partition.
    /// - `EOD/NULL` and `FOLLOWUP/NULL` share book `None` and sort
    ///   adjacently (`order by batch, book, …`, NULL last within a
    ///   batch, `"EOD" < "FOLLOWUP"`) — comparing `book` alone collapses
    ///   these two into one partition instead.
    ///
    /// No live/archive data rows: `partitions_for` and `resolve_
    /// generations` read only `generations`, so this fixture populates
    /// nothing else.
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

    /// Step 6: `partitions_for` already lists one partition per document
    /// batch with `book: None` (it reads the `generations` summary the
    /// same way for both families) — this pins that against two real
    /// documents published through `publish_document`, splitting each
    /// partition's `batch` back to the key that produced it
    /// (`geode_core::document::split_key`, `join_key`'s exact inverse).
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
        // Important 1 (final fix wave): the row counts are per table, not
        // per grain, and a document dataset has no grain — summed over
        // `grains()` this dataset reported 0 live rows beside two live
        // partitions. Two keys x six rows, both live, nothing archived.
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

    /// The series family's catalog rows (timeseries spec §4.6): one per
    /// `(identity, source)` pair, the hull of its fetched spans, and a
    /// `live_rows` that comes from the series table — a series dataset
    /// has no live/archive pair, so the `table_pairs` loop alone would
    /// report 0 rows beside a real list of pairs.
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
