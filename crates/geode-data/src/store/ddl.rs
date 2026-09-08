//! Table DDL generated from the declared schema (spec §4.2). One live and
//! one archive table per grain present in the dataset.
//!
//! Live carries exactly the current rows for every file partition: no
//! generation column, no history predicate, size independent of retention.
//! That is what keeps the requery budget reachable by construction, so the
//! absence of `gen_id` from live is load-bearing, not an oversight.
//!
//! A grain's table carries its key columns plus the measures and attributes
//! declared *at that grain*, plus every dimension it carries (spec §3.3):
//! a key dimension it always had, and a *carried* dimension — one that
//! names the grain whose key determines it — as a payload column, stored
//! at that grain's table and every finer one's. A bare `Dimension` column
//! outside every grain key is rejected at parse (`schema::validate_dataset`)
//! rather than silently appearing in no table at all — so anything worth
//! displaying that is not itself a key (`business_date`, for one) must be
//! declared as an `attribute` at the grain that owns it, or as a dimension
//! carried by one.

use crate::store::StoreError;
use duckdb::Connection;
use geode_core::schema::{ColumnRole, DatasetSpec, Grain};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Live,
    Archive,
}

impl TableKind {
    pub fn suffix(self) -> &'static str {
        match self {
            TableKind::Live => "_live",
            TableKind::Archive => "_archive",
        }
    }
}

/// Table names carry the dataset, not just the grain.
///
/// Two datasets can declare columns at the same grain — `risk_snapshot`
/// and `instrument_ref` both have instrument-grain columns — and a
/// grain-only name would make `CREATE TABLE IF NOT EXISTS` silently give
/// the second dataset the first one's table, with the wrong columns.
pub fn table_name(dataset: &str, grain: Grain, kind: TableKind) -> String {
    format!("{dataset}_{}{}", grain.short(), kind.suffix())
}

/// The ENUM type name for a dimension column of a dataset.
pub fn enum_type_name(dataset: &str, column: &str) -> String {
    format!("{dataset}_{column}_enum")
}

/// Columns interned as ENUMs (spec §3.3, §3.6): the schema's `categorical`
/// flag, which defaults on for dimensions and off for keys, whose
/// vocabularies would make a useless dictionary.
pub fn categorical_columns(ds: &DatasetSpec) -> Vec<&str> {
    ds.categorical_columns()
}

/// Rebuild a dimension's ENUM type from the values currently live **and**
/// archived.
///
/// **Storage stays `VARCHAR`; the ENUM is derived and used only for a
/// query-time cast.** DuckDB 1.10505 has no `ALTER TYPE ... ADD VALUE`,
/// so an ENUM *column* could only be widened by dropping and recreating
/// the type — which means rewriting every table that uses it, the first
/// time a new book appears. Deriving the type instead keeps the
/// dictionary encoding §7.2 wants (the cast makes `query_arrow` return
/// `Dictionary(UInt8, Utf8)`) at the cost of one cheap rebuild per
/// ingest, and no table ever moves.
///
/// The dictionary covers every era because this runs after each publish,
/// which is also when the outgoing generation moves to the archive
/// (spec §4.3) — so no row in either table can hold a value the type
/// lacks at the moment this returns. Retention only ever removes rows,
/// never adds a value back, so a superset dictionary stays harmless to
/// the scope compiler's `IN` (spec §3.5): reading both tables here is
/// what lets the text filter's dictionary rewrite apply under an as-of
/// era too, not just live.
///
/// Returns the number of distinct values the type now carries.
pub fn refresh_enum(
    conn: &duckdb::Connection,
    dataset: &str,
    column: &str,
    live_table: &str,
    archive_table: &str,
) -> Result<usize, crate::store::StoreError> {
    let name = enum_type_name(dataset, column);
    // Nothing references the type — columns are VARCHAR — so dropping is
    // free and never touches stored data.
    let sql = format!(
        "drop type if exists {name};
         create type {name} as enum (
             select distinct \"{column}\"::varchar from {live_table}
             where \"{column}\" is not null
             union
             select distinct \"{column}\"::varchar from {archive_table}
             where \"{column}\" is not null
         );"
    );
    conn.execute_batch(&sql)
        .map_err(|source| crate::store::StoreError::Sql {
            statement: sql,
            source,
        })?;

    let count = format!("select count(*) from (select unnest(enum_range(NULL::{name})))");
    conn.query_row(&count, [], |r| r.get::<_, i64>(0))
        .map(|n| n as usize)
        .map_err(|source| crate::store::StoreError::Sql {
            statement: count,
            source,
        })
}

/// Derived ENUM type names currently present for a dataset. Used both by
/// the view compiler (to intern dimension columns for a live query) and
/// the scope compiler (to route a text filter's `ILIKE` over the
/// dictionary rather than every row, spec §3.5).
pub fn existing_enum_types(
    conn: &duckdb::Connection,
    dataset: &str,
) -> Result<Vec<String>, crate::store::StoreError> {
    let sql = "select type_name from duckdb_types() where type_name like ?";
    let err = |source| crate::store::StoreError::Sql {
        statement: sql.to_string(),
        source,
    };
    let mut stmt = conn.prepare(sql).map_err(err)?;
    let rows = stmt
        .query_map(duckdb::params![format!("{dataset}_%_enum")], |r| {
            r.get::<_, String>(0)
        })
        .map_err(err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(err)
}

pub fn create_table_sql(ds: &DatasetSpec, grain: Grain, kind: TableKind) -> String {
    let mut cols: Vec<String> = Vec::new();

    for key in grain.key_columns() {
        let ty = ds.column(key).map(|c| c.ty.sql()).unwrap_or("VARCHAR");
        cols.push(format!("  \"{key}\" {ty}"));
    }

    for c in ds.columns.iter() {
        let keep = match c.role {
            ColumnRole::Measure { grain: g, .. } | ColumnRole::Attribute { grain: g } => g == grain,
            ColumnRole::Key | ColumnRole::Dimension { .. } => false,
        };
        if keep {
            cols.push(format!("  \"{}\" {}", c.name, c.ty.sql()));
        }
    }

    // Carried dimensions (spec §3.3): payload columns of the declaring
    // grain's table and every finer grain's.
    for c in ds.carried_dimensions_at(grain) {
        cols.push(format!("  \"{}\" {}", c.name, c.ty.sql()));
    }

    // Partition key completion: `book` is already in the grain key, `batch`
    // is what replacement matches on, `source_file_id` is provenance only
    // (spec §4.3 — filenames carry dates, so file id is not partition id).
    cols.push("  \"batch\" VARCHAR".to_string());
    cols.push("  \"source_file_id\" BIGINT".to_string());
    // Live and archive carry identical columns, including the generation
    // that produced the row. Live still holds exactly one generation per
    // partition, so no query ever filters on `gen_id` — the §4.2 property
    // that keeps live's size independent of retention is about the absence
    // of a *history predicate*, not the absence of the column.
    //
    // Carrying it is what lets archived rows keep their own identity: the
    // publish transaction moves rows out of live with their stamps intact,
    // so as-of to a time when an older generation was live still finds it.
    let _ = kind;
    cols.push("  \"gen_id\" BIGINT".to_string());
    cols.push("  \"source_time\" TIMESTAMP WITH TIME ZONE".to_string());

    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
        table_name(&ds.name, grain, kind),
        cols.join(",\n")
    )
}

/// Every table a dataset's history lives in: the archive **and** live for
/// each grain. Generations are resolved (and rebuilt) across all of them
/// — a partition can be missing from one grain while present at another
/// (a cash-only book has no underlying rows), and the generation a
/// partition holds now is in live and nowhere else (see
/// `query::as_of::resolve_generations`). Shared by `service.rs` (the
/// freshness fold and the open-time migration) and `query::compile`
/// (`era_for` and the join path) so the table list is named in one place.
pub fn history_of(dataset: &str, ds: &DatasetSpec) -> Vec<String> {
    ds.grains()
        .into_iter()
        .flat_map(|g| {
            [
                table_name(dataset, g, TableKind::Archive),
                table_name(dataset, g, TableKind::Live),
            ]
        })
        .collect()
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// A `(batch, book, gen_id, source_time)` relation covering every
/// generation any of `tables` records, deduplicated **across** tables as
/// well as within one: a file publishes every grain, so one generation
/// ordinarily appears in more than one grain's table under the identical
/// tuple, and a raw `union all` of per-table distincts would multiply it
/// by how many grains carry it. The outer `distinct` is what keeps this
/// query's answer at "one row per generation" -- the same invariant
/// `publish_file`'s `where not exists` guard maintains incrementally.
///
/// An empty `tables` yields an empty, correctly-typed relation rather
/// than invalid SQL from an empty `union all`.
fn generations_union_sql(tables: &[String]) -> String {
    if tables.is_empty() {
        return "select null::varchar as batch, null::varchar as book, \
                 null::bigint as gen_id, \
                 null::timestamp with time zone as source_time \
                 where false"
            .to_string();
    }
    let union = tables
        .iter()
        .map(|t| format!("select batch, book, gen_id, source_time from {t}"))
        .collect::<Vec<_>>()
        .join(" union all ");
    format!("select distinct batch, book, gen_id, source_time from ({union})")
}

/// Rebuild the `generations` summary for `dataset` from its data tables
/// (spec §6.5 as amended): the migration path for a database written
/// before the table existed (`DataService::open`), and the tests' oracle
/// -- the summary is defined to equal this, always.
///
/// Inside one transaction: delete every row currently recorded for
/// `dataset`, then reinsert one row per generation found across `tables`
/// (ordinarily `history_of(dataset, ds)` -- every grain's archive and
/// live table, so a partition missing from one grain's history is not
/// silently dropped from the rebuilt summary either). Returns how many
/// rows the summary now holds for the dataset.
pub fn rebuild_generations(
    conn: &Connection,
    dataset: &str,
    tables: &[String],
) -> Result<usize, StoreError> {
    let escaped = dataset.replace('\'', "''");
    let sql = format!(
        "begin;
         delete from generations where dataset = '{escaped}';
         insert into generations
         select '{escaped}', batch, book, gen_id, source_time from ({union});
         commit;",
        union = generations_union_sql(tables),
    );
    if let Err(source) = conn.execute_batch(&sql) {
        let _ = conn.execute_batch("rollback;");
        return Err(StoreError::Sql {
            statement: sql,
            source,
        });
    }
    let count_sql = "select count(*) from generations where dataset = ?";
    conn.query_row(count_sql, duckdb::params![dataset], |r| r.get::<_, i64>(0))
        .map(|n| n as usize)
        .map_err(sql_err(count_sql))
}

/// Test-only oracle: the `generations` summary for `dataset` must equal a
/// fresh rebuild from `tables`, as a multiset (sorted, duplicate rows
/// included) -- so a duplicate the maintenance code accidentally left
/// behind fails this even though the *set* of generations still looks
/// right.
#[cfg(test)]
pub(crate) fn assert_generations_match_tables(conn: &Connection, dataset: &str, tables: &[String]) {
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct Row {
        batch: String,
        book: Option<String>,
        gen_id: i64,
        source_time: chrono::DateTime<chrono::Utc>,
    }
    fn fetch(conn: &Connection, sql: &str) -> Vec<Row> {
        let mut stmt = conn.prepare(sql).unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(Row {
                    batch: r.get(0)?,
                    book: r.get(1)?,
                    gen_id: r.get(2)?,
                    source_time: r.get(3)?,
                })
            })
            .unwrap();
        let mut v: Vec<Row> = rows.map(|r| r.unwrap()).collect();
        v.sort();
        v
    }
    let expected = fetch(
        conn,
        &format!(
            "select batch, book, gen_id, source_time from ({})",
            generations_union_sql(tables)
        ),
    );
    let escaped = dataset.replace('\'', "''");
    let actual = fetch(
        conn,
        &format!(
            "select batch, book, gen_id, source_time from generations where dataset = '{escaped}'"
        ),
    );
    assert_eq!(
        actual, expected,
        "the generations summary for '{dataset}' must equal a rebuild from \
         its tables (as a multiset -- duplicates included): summary {actual:?}, rebuild {expected:?}"
    );
}

#[cfg(test)]
pub(crate) mod tests_support {
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::schema::{DatasetSpec, SchemaSpec};

    pub(crate) fn sample_dataset() -> DatasetSpec {
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
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
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

    /// The Phase 4 §3.3 fixture: `currency` carried by the instrument
    /// grain, and `expiry` an attribute opted into `categorical`.
    pub(crate) fn carried_dataset() -> DatasetSpec {
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk.columns.expiry]
type = "utf8"
role = "attribute"
grain = "instrument"
categorical = true
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk")
            .unwrap()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::{carried_dataset, sample_dataset};
    use super::*;

    #[test]
    fn live_table_carries_the_grain_key_and_its_measures_only() {
        let sql = create_table_sql(&sample_dataset(), Grain::Position, TableKind::Live);
        assert!(sql.contains("risk_snapshot_position_live"), "{sql}");
        assert!(sql.contains("\"book\" VARCHAR"), "{sql}");
        assert!(sql.contains("\"daily_trading_pnl\" DOUBLE"), "{sql}");
        // A finer grain's key column must not appear at position grain.
        assert!(!sql.contains("underlying_ref"), "{sql}");
        // Nor a measure declared at another grain.
        assert!(!sql.contains("delta01"), "{sql}");
    }

    #[test]
    fn two_datasets_sharing_a_grain_get_separate_tables() {
        // risk_snapshot and instrument_ref both have instrument-grain
        // columns. A grain-only table name would make CREATE TABLE IF NOT
        // EXISTS silently give the second one the first's table, with the
        // wrong columns — a data bug with no symptom until a query.
        assert_eq!(
            table_name("risk_snapshot", Grain::Instrument, TableKind::Live),
            "risk_snapshot_instrument_live"
        );
        assert_eq!(
            table_name("instrument_ref", Grain::Instrument, TableKind::Live),
            "instrument_ref_instrument_live"
        );
        assert_ne!(
            table_name("risk_snapshot", Grain::Instrument, TableKind::Live),
            table_name("instrument_ref", Grain::Instrument, TableKind::Live)
        );
    }

    #[test]
    fn live_and_archive_have_identical_columns() {
        // The publish transaction moves rows between them with `select *`,
        // so any divergence would silently mis-map columns — and archived
        // rows must keep the generation stamps they had while live.
        let ds = sample_dataset();
        for grain in [Grain::Position, Grain::Underlying] {
            let live = create_table_sql(&ds, grain, TableKind::Live);
            let archive = create_table_sql(&ds, grain, TableKind::Archive);
            let cols = |sql: &str| {
                sql.lines()
                    .filter(|l| l.trim_start().starts_with('"'))
                    .map(|l| l.trim().trim_end_matches(',').to_string())
                    .collect::<Vec<_>>()
            };
            assert_eq!(cols(&live), cols(&archive), "grain {grain:?}");
            assert!(live.contains("\"gen_id\" BIGINT"), "{live}");
            assert!(
                live.contains("\"source_time\" TIMESTAMP WITH TIME ZONE"),
                "{live}"
            );
        }
    }

    #[test]
    fn live_carries_batch_for_replacement_and_file_id_for_provenance() {
        let sql = create_table_sql(&sample_dataset(), Grain::Underlying, TableKind::Live);
        // `batch` is what the publish transaction matches on: filenames carry
        // dates, so file identity is not partition identity (spec §4.3).
        assert!(sql.contains("\"batch\" VARCHAR"), "{sql}");
        assert!(sql.contains("\"source_file_id\" BIGINT"), "{sql}");
        // `book` is part of the grain key at every grain, completing the
        // partition key (dataset, batch, book).
        assert!(sql.contains("\"book\" VARCHAR"), "{sql}");
    }

    #[test]
    fn archive_adds_gen_id_and_source_time() {
        let sql = create_table_sql(&sample_dataset(), Grain::Underlying, TableKind::Archive);
        assert!(sql.contains("risk_snapshot_underlying_archive"), "{sql}");
        assert!(sql.contains("\"gen_id\" BIGINT"), "{sql}");
        assert!(
            sql.contains("\"source_time\" TIMESTAMP WITH TIME ZONE"),
            "{sql}"
        );
    }

    #[test]
    fn create_table_carries_a_carried_dimension_at_its_grain_and_finer() {
        let ds = carried_dataset();
        let sql = |g| create_table_sql(&ds, g, TableKind::Live);
        assert!(!sql(Grain::Position).contains("\"currency\""));
        assert!(sql(Grain::Instrument).contains("\"currency\" VARCHAR"));
        assert!(sql(Grain::Underlying).contains("\"currency\" VARCHAR"));
    }

    #[test]
    fn categorical_columns_follow_the_flag_not_the_role() {
        let ds = carried_dataset();
        assert_eq!(
            categorical_columns(&ds),
            vec![
                "book",
                "lhu",
                "counterparty",
                "underlying_ref",
                "currency",
                "expiry"
            ]
        );
    }

    #[test]
    fn history_of_names_the_archive_and_live_table_of_every_grain() {
        let ds = sample_dataset();
        let tables = history_of("risk_snapshot", &ds);
        assert!(tables.contains(&"risk_snapshot_position_archive".to_string()));
        assert!(tables.contains(&"risk_snapshot_position_live".to_string()));
        assert!(tables.contains(&"risk_snapshot_underlying_archive".to_string()));
        assert!(tables.contains(&"risk_snapshot_underlying_live".to_string()));
    }
}

#[cfg(test)]
mod rebuild_generations_tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;

    fn store_with_generations_table() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    fn dataset_count(conn: &duckdb::Connection, dataset: &str) -> i64 {
        conn.query_row(
            "select count(*) from generations where dataset = ?",
            duckdb::params![dataset],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn rebuild_populates_the_summary_from_the_tables() {
        let (_d, store) = store_with_generations_table();
        store
            .writer()
            .execute_batch(
                "create table t_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into t_archive values ('b', 'BK0', 1, '2026-08-30T07:00:00Z');",
            )
            .unwrap();
        let n = rebuild_generations(store.writer(), "ds", &["t_archive".to_string()]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(dataset_count(store.writer(), "ds"), 1);
    }

    #[test]
    fn rebuild_replaces_a_prior_summary_rather_than_appending() {
        // A stray or stale row for this dataset must not survive a
        // rebuild -- the summary after rebuilding must equal exactly what
        // the tables hold, not the union of the old summary and the
        // tables.
        let (_d, store) = store_with_generations_table();
        store
            .writer()
            .execute_batch(
                "create table t_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into t_archive values ('b', 'BK0', 1, '2026-08-30T07:00:00Z');
                 insert into generations values ('ds', 'stale', 'BKX', 99, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
        let n = rebuild_generations(store.writer(), "ds", &["t_archive".to_string()]).unwrap();
        assert_eq!(n, 1, "the stale planted row must be gone");
        let kept: String = store
            .writer()
            .query_row(
                "select batch from generations where dataset = 'ds'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, "b");
    }

    #[test]
    fn rebuild_with_no_tables_clears_the_summary_and_returns_zero() {
        let (_d, store) = store_with_generations_table();
        store
            .writer()
            .execute_batch(
                "insert into generations values ('ds', 'stale', 'BKX', 99, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
        let n = rebuild_generations(store.writer(), "ds", &[]).unwrap();
        assert_eq!(n, 0);
        assert_eq!(dataset_count(store.writer(), "ds"), 0);
    }

    #[test]
    fn rebuild_deduplicates_a_generation_shared_by_every_grains_tables() {
        // One file publishes every grain under the same (batch, book,
        // gen_id, source_time). Rebuilding from a raw per-table union
        // must not multiply that one generation by how many grain tables
        // carry it, or the summary would disagree with what publish's
        // own dedup-guarded insert produces for the same data.
        let (_d, store) = store_with_generations_table();
        store
            .writer()
            .execute_batch(
                "create table pos_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 create table under_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into pos_archive values ('b', 'BK0', 1, '2026-08-30T07:00:00Z');
                 insert into under_archive values ('b', 'BK0', 1, '2026-08-30T07:00:00Z');",
            )
            .unwrap();
        let n = rebuild_generations(
            store.writer(),
            "ds",
            &["pos_archive".to_string(), "under_archive".to_string()],
        )
        .unwrap();
        assert_eq!(n, 1, "one generation, seen at two grains, is one row");
    }

    #[test]
    fn rebuild_keeps_a_null_book_partition() {
        let (_d, store) = store_with_generations_table();
        store
            .writer()
            .execute_batch(
                "create table t_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into t_archive values ('b', NULL, 1, '2026-08-30T07:00:00Z');",
            )
            .unwrap();
        rebuild_generations(store.writer(), "ds", &["t_archive".to_string()]).unwrap();
        let book: Option<String> = store
            .writer()
            .query_row(
                "select book from generations where dataset = 'ds'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(book, None, "the bookless partition must round-trip");
    }

    #[test]
    fn rebuild_scopes_to_the_named_dataset_only() {
        let (_d, store) = store_with_generations_table();
        store
            .writer()
            .execute_batch(
                "create table t_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into t_archive values ('b', 'BK0', 1, '2026-08-30T07:00:00Z');
                 insert into generations values ('other', 'x', 'BKX', 1, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
        rebuild_generations(store.writer(), "ds", &["t_archive".to_string()]).unwrap();
        assert_eq!(
            dataset_count(store.writer(), "other"),
            1,
            "another dataset's summary must be untouched"
        );
    }

    #[test]
    fn assert_generations_match_tables_passes_after_a_rebuild() {
        let (_d, store) = store_with_generations_table();
        store
            .writer()
            .execute_batch(
                "create table t_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into t_archive values ('b', 'BK0', 1, '2026-08-30T07:00:00Z');",
            )
            .unwrap();
        rebuild_generations(store.writer(), "ds", &["t_archive".to_string()]).unwrap();
        assert_generations_match_tables(store.writer(), "ds", &["t_archive".to_string()]);
    }

    #[test]
    #[should_panic(expected = "must equal a rebuild")]
    fn assert_generations_match_tables_catches_a_duplicate_row() {
        let (_d, store) = store_with_generations_table();
        store
            .writer()
            .execute_batch(
                "create table t_archive(
                     batch varchar, book varchar, gen_id bigint,
                     source_time timestamp with time zone);
                 insert into t_archive values ('b', 'BK0', 1, '2026-08-30T07:00:00Z');
                 -- Two identical rows in the summary where the tables hold one.
                 insert into generations values
                   ('ds', 'b', 'BK0', 1, '2026-08-30T07:00:00Z'),
                   ('ds', 'b', 'BK0', 1, '2026-08-30T07:00:00Z');",
            )
            .unwrap();
        assert_generations_match_tables(store.writer(), "ds", &["t_archive".to_string()]);
    }
}
