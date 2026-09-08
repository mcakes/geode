//! What the database holds (Phase 4b spec §4.5): `Request::Catalog`'s
//! implementation, built from the `generations` summary table,
//! `file_generations`, and DuckDB's own introspection functions.

use crate::query::as_of::{AsOf, resolve_generations};
use crate::store::StoreError;
use crate::store::ddl::{TableKind, table_name};
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
    let (database_bytes, used_blocks) = database_size(conn)?;
    let memory_bytes = memory_bytes(conn)?;
    let threads = threads(conn)?;
    Ok(CatalogSnapshot {
        datasets,
        database_bytes,
        used_blocks,
        memory_bytes,
        threads,
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

    let mut live_rows = 0u64;
    let mut archive_rows = 0u64;
    for grain in ds.grains() {
        live_rows += sizes
            .get(&table_name(&ds.name, grain, TableKind::Live))
            .copied()
            .unwrap_or(0);
        archive_rows += sizes
            .get(&table_name(&ds.name, grain, TableKind::Archive))
            .copied()
            .unwrap_or(0);
    }

    Ok(DatasetCatalog {
        name: ds.name.clone(),
        partitions,
        live_rows,
        archive_rows,
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

/// `gen_id -> (loaded_at, row_count)` for every generation
/// `file_generations` recorded for `dataset`.
fn file_generations_for(
    conn: &Connection,
    dataset: &str,
) -> Result<HashMap<i64, (DateTime<Utc>, u64)>, StoreError> {
    let sql = "select gen_id, loaded_at, row_count from file_generations where dataset = ?";
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

/// `estimated_size` per table, over every table `duckdb_tables()` knows
/// about — a row *estimate*, not an exact count; `build_catalog`'s
/// callers label the field "rows (est.)".
fn table_sizes(conn: &Connection) -> Result<HashMap<String, u64>, StoreError> {
    let sql = "select table_name, estimated_size from duckdb_tables()";
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

/// `block_size * total_blocks` and `used_blocks`, both in blocks/bytes,
/// from `pragma_database_size()`.
fn database_size(conn: &Connection) -> Result<(u64, u64), StoreError> {
    let sql = "select block_size, total_blocks, used_blocks from pragma_database_size()";
    let err = |source| StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    conn.query_row(sql, [], |r| {
        let block_size: i64 = r.get(0)?;
        let total_blocks: i64 = r.get(1)?;
        let used_blocks: i64 = r.get(2)?;
        Ok((
            (block_size * total_blocks).max(0) as u64,
            used_blocks.max(0) as u64,
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
    use geode_core::config::{LayerDoc, merge_docs};

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
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
    /// live only (2 rows). `generations` and `file_generations` are
    /// populated directly (not through `publish_file`) so the test
    /// controls `gen_id`, `source_time`, `loaded_at` and `row_count`
    /// exactly.
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
                   (1,'risk_snapshot','BK000','/p1',100,
                    TIMESTAMPTZ '2026-08-01T00:05:00Z',TIMESTAMPTZ '2026-08-01T00:00:00Z',
                    1,TIMESTAMPTZ '2026-08-01T00:05:00Z',2,'ok',NULL,false),
                   (2,'risk_snapshot','BK000','/p2',150,
                    TIMESTAMPTZ '2026-08-10T00:05:00Z',TIMESTAMPTZ '2026-08-10T00:00:00Z',
                    2,TIMESTAMPTZ '2026-08-10T00:05:00Z',3,'ok',NULL,false),
                   (3,'risk_snapshot','BK001','/p3',120,
                    TIMESTAMPTZ '2026-08-12T00:05:00Z',TIMESTAMPTZ '2026-08-12T00:00:00Z',
                    3,TIMESTAMPTZ '2026-08-12T00:05:00Z',2,'ok',NULL,false);",
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
        assert!(
            bk000.generations[1].loaded_at.is_some() && bk000.generations[1].file_rows.is_some()
        );
        assert_eq!(ds.live_rows, f.live_rows_expected);
        assert!(snap.database_bytes > 0 && snap.memory_bytes > 0 && snap.threads > 0);
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
        let f = fixture_with_two_generations();
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
    }
}
