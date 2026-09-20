//! Table DDL generated from the declared schema (spec §4.2). The measure
//! family gets one live and one archive table per grain present in the
//! dataset; the document family gets exactly one such pair for the whole
//! dataset (market-data spec §4.1). Either way a dataset owns a set of
//! [`TablePair`]s, which is what publish, retention and history all speak.
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

/// A dataset's live/archive pair. The measure family has one per grain,
/// the document family exactly one (market-data spec §4.1); everything
/// that publishes, sweeps or resolves history takes a pair rather than a
/// `Grain` so the two families go through one door.
///
/// The names are built once and carried, not rebuilt at each use: a pair
/// is never derivable from a grain alone anyway (a document dataset has
/// no grain), so passing the pair is the only shape that serves both
/// families without a family test at every call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePair {
    pub live: String,
    pub archive: String,
}

impl TablePair {
    pub fn for_grain(dataset: &str, grain: Grain) -> TablePair {
        TablePair {
            live: table_name(dataset, grain, TableKind::Live),
            archive: table_name(dataset, grain, TableKind::Archive),
        }
    }

    /// `document` is not a `Grain::short()` value, so this can never
    /// collide with a grain table of the same dataset — a dataset that
    /// somehow declared both families would get separate tables rather
    /// than one silently shared by both.
    pub fn for_document(dataset: &str) -> TablePair {
        TablePair {
            live: format!("{dataset}_document{}", TableKind::Live.suffix()),
            archive: format!("{dataset}_document{}", TableKind::Archive.suffix()),
        }
    }

    pub fn of(&self, kind: TableKind) -> &str {
        match kind {
            TableKind::Live => &self.live,
            TableKind::Archive => &self.archive,
        }
    }
}

/// Every live/archive pair a dataset owns: one per grain for the measure
/// family, exactly one for the document family. The single place the two
/// families' table sets are named, so `apply_schema`, `history_of` and
/// the sweep's reconciliation cannot drift apart on which tables exist.
pub fn table_pairs(ds: &DatasetSpec) -> Vec<TablePair> {
    if ds.is_document() {
        vec![TablePair::for_document(&ds.name)]
    } else {
        ds.grains()
            .into_iter()
            .map(|g| TablePair::for_grain(&ds.name, g))
            .collect()
    }
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
            ColumnRole::Measure { grain: g, .. } => g == grain,
            ColumnRole::Attribute { grain: Some(g) } => g == grain,
            // A document-level attribute (`grain: None`) carries no
            // grain to match; `Axis`/`Value` are the document family's
            // own row shape and never reach a grain table at all.
            ColumnRole::Attribute { grain: None }
            | ColumnRole::Key
            | ColumnRole::Dimension { .. }
            | ColumnRole::Axis
            | ColumnRole::Value => false,
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

/// The document family's one table (market-data spec §4.1):
/// `DatasetSpec::document_columns` in that order — key, axes, values,
/// then grainless attributes — followed by the storage columns every
/// grain table carries, `book` included.
///
/// The storage columns mean exactly what they mean for a grain table:
/// `batch` is what a republish replaces on, `source_file_id` is
/// provenance only, and `gen_id`/`source_time` ride on live as well as
/// archive so archived rows keep the identity they had while live (see
/// `create_table_sql` for the full argument — live still holds one
/// generation per partition, so no query filters on `gen_id`). `book` is
/// the one a grain table gets from its grain key and this one declares
/// itself: a document's book is *empty*, which is a NULL in a column that
/// exists rather than a missing column, and every partition-keyed
/// statement in `store` joins on it.
///
/// An attribute repeats down every row of its document rather than
/// living in a table of its own: a document is published, replaced and
/// read whole, so there is no second grain to join and nothing a
/// separate header table would save.
pub fn create_document_table_sql(ds: &DatasetSpec, kind: TableKind) -> String {
    let mut cols: Vec<String> = ds
        .document_columns()
        .iter()
        .map(|c| format!("  \"{}\" {}", c.name, c.ty.sql()))
        .collect();
    // Partition key completion, same four columns and the same meanings as
    // a grain table's (`create_table_sql`) -- plus `book`, which the
    // document family gets *here* because no grain key supplies it. A
    // document's book is empty (market-data spec §4.1), and empty means a
    // NULL value in a column that exists, not an absent column: every
    // partition-keyed statement in this module joins on `book`
    // (retention's eviction, `generations_reconcile_sql`,
    // `rebuild_generations`' union, `publish_file`'s own
    // `partition_predicate` with its `book is null` term), so a table
    // without the column could not be published into, swept, reconciled
    // or summarised at all.
    cols.push("  \"batch\" VARCHAR".to_string());
    cols.push("  \"book\" VARCHAR".to_string());
    cols.push("  \"source_file_id\" BIGINT".to_string());
    cols.push("  \"gen_id\" BIGINT".to_string());
    cols.push("  \"source_time\" TIMESTAMP WITH TIME ZONE".to_string());
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
        TablePair::for_document(&ds.name).of(kind),
        cols.join(",\n")
    )
}

/// Every table a dataset's history lives in: the archive **and** live of
/// every pair it owns — each grain's pair for the measure family, the one
/// document pair for the document family. Generations are resolved (and
/// rebuilt) across all of them — a partition can be missing from one
/// grain while present at another (a cash-only book has no underlying
/// rows), and the generation a partition holds now is in live and nowhere
/// else (see `query::as_of::resolve_generations`). Shared by `service.rs`
/// (the freshness fold and the open-time migration) and `query::compile`
/// (`era_for` and the join path) so the table list is named in one place.
///
/// `dataset` is the caller's own name for the dataset and must be
/// `ds.name` — kept as a parameter because every caller already has it to
/// hand, and passing it makes the table names visibly the named
/// dataset's at the call site.
pub fn history_of(dataset: &str, ds: &DatasetSpec) -> Vec<String> {
    debug_assert_eq!(
        dataset, ds.name,
        "history_of must name the dataset it is given"
    );
    table_pairs(ds)
        .into_iter()
        .flat_map(|p| [p.archive, p.live])
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
/// (ordinarily `history_of(dataset, ds)` -- every pair's archive and live
/// table, so a partition missing from one pair's history is not silently
/// dropped from the rebuilt summary either). Returns how many rows the
/// summary now holds for the dataset.
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
    use chrono::{DateTime, NaiveDate, Utc};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::document::{
        Column, DocumentKind, DocumentRows, ParseError, ParsedDocument, Value, WriteError,
        check_kind_against,
    };
    use geode_core::schema::{ColumnType, DatasetSpec, SchemaSpec};

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

    /// The market-data spec's own document dataset (§2.1), parsed through
    /// the real reader rather than hand-built: a document dataset's table
    /// shape is decided by `family`/`key`/`axes` and the roles the reader
    /// derives from them, so a hand-built `DatasetSpec` could disagree
    /// with what a TOML layer can actually produce.
    pub(crate) fn cvi_dataset() -> DatasetSpec {
        let text = r#"
[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]

[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true

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
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("cvi_params")
            .unwrap()
            .clone()
    }

    /// A `local = true` document dataset for the publish tests
    /// (line-pricer spec §7.2): one key, one axis, one value.
    pub(crate) fn local_dataset() -> DatasetSpec {
        let text = r#"
[sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]
[sheets.columns.sheet]
type = "utf8"
role = "dimension"
[sheets.columns.line]
type = "i64"
role = "axis"
[sheets.columns.qty]
type = "i64"
role = "value"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema.dataset("sheets").unwrap().clone()
    }

    pub(crate) fn sheet_rows(sheet: &str, qty: &[i64]) -> DocumentRows {
        DocumentRows {
            key: vec![sheet.to_string()],
            attributes: Vec::new(),
            axes: vec![(
                "line".to_string(),
                Column::I64((1..=qty.len() as i64).collect()),
            )],
            values: vec![("qty".to_string(), Column::I64(qty.to_vec()))],
        }
    }

    pub(crate) fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    pub(crate) fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// Two terms x three nodes, term-major — the same shape Task 4's tests
    /// validate, with the six `param` values the caller chooses.
    pub(crate) fn cvi_doc(key: &str, params: [f64; 6]) -> DocumentRows {
        DocumentRows {
            key: vec![key.into()],
            attributes: vec![
                ("anchor_date".into(), Value::Date(d("2026-09-12"))),
                ("spot_ref".into(), Value::F64(7650.0)),
            ],
            axes: vec![
                (
                    "term".into(),
                    Column::Date(vec![
                        d("2026-09-18"),
                        d("2026-09-18"),
                        d("2026-09-18"),
                        d("2026-10-16"),
                        d("2026-10-16"),
                        d("2026-10-16"),
                    ]),
                ),
                (
                    "node".into(),
                    Column::F64(vec![-20.0, -1.0, 3.5, -20.0, -1.0, 3.5]),
                ),
            ],
            values: vec![("param".into(), Column::F64(params.to_vec()))],
        }
    }

    /// A [`DocumentKind`] shaped exactly like the real CVI XML kind —
    /// the same six columns `cvi_dataset` declares — over a byte format
    /// small enough to write in a test literal:
    ///
    /// ```text
    /// SPX.Z:1,2,3,4,5,6[:elem/path,other/path]
    /// ```
    ///
    /// the key, then the six `param` values `cvi_doc` takes, then an
    /// optional comma-separated list of element paths the "parser" did
    /// not recognise (`ParsedDocument::unknown_paths`).
    ///
    /// It lives here rather than beside the receiver's own tests for a
    /// layering reason (this plan's global constraints): the real CVI
    /// kind is in `geode-documents`, which `geode-data` must never
    /// depend on, so every test in this crate that needs a kind at all
    /// needs a fake — the receiver's (`ingest::subscribe`) and the
    /// service's (`service`) both, which is what makes this the one
    /// place to spell it.
    pub(crate) struct FakeKind {
        columns: Vec<(&'static str, ColumnType)>,
    }

    impl FakeKind {
        /// The six columns `cvi_dataset` declares, in
        /// `document_columns()` order — so `check_kind_against` accepts
        /// the pair.
        pub(crate) fn new() -> FakeKind {
            FakeKind {
                columns: vec![
                    ("underlying_ref", ColumnType::Utf8),
                    ("term", ColumnType::Date),
                    ("node", ColumnType::F64),
                    ("param", ColumnType::F64),
                    ("anchor_date", ColumnType::Date),
                    ("spot_ref", ColumnType::F64),
                ],
            }
        }

        /// The same kind plus one column no dataset declares — the
        /// kind/dataset mismatch `check_kind_against` refuses at
        /// source-open time (spec §6.4).
        pub(crate) fn with_extra_column() -> FakeKind {
            let mut kind = FakeKind::new();
            kind.columns.push(("surface_id", ColumnType::Utf8));
            kind
        }

        /// One message body in the format `parse` reads. The inverse of
        /// `write` for the no-unknown-paths case, and the one spelling
        /// of the format a test should use — a hand-written literal
        /// would fall out of step with `parse` the first time the format
        /// changes.
        pub(crate) fn message(key: &str, params: [f64; 6]) -> Vec<u8> {
            let params = params
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("{key}:{params}").into_bytes()
        }
    }

    impl DocumentKind for FakeKind {
        fn name(&self) -> &'static str {
            "fake_cvi"
        }

        fn columns(&self) -> &[(&'static str, ColumnType)] {
            &self.columns
        }

        fn parse(&self, bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
            let text = std::str::from_utf8(bytes).map_err(|e| ParseError {
                message: format!("not utf-8: {e}"),
            })?;
            let mut fields = text.splitn(3, ':');
            let key = fields.next().unwrap_or_default();
            if key.is_empty() {
                return Err(ParseError {
                    message: format!("{text:?} has no key before the ':'"),
                });
            }
            let Some(params) = fields.next() else {
                return Err(ParseError {
                    message: format!("{text:?} is not 'key:p1,...,p6'"),
                });
            };
            let params: Vec<f64> = params
                .split(',')
                .map(|p| {
                    p.trim().parse::<f64>().map_err(|e| ParseError {
                        message: format!("{p:?} is not a number: {e}"),
                    })
                })
                .collect::<Result<_, _>>()?;
            let params: [f64; 6] = params.try_into().map_err(|p: Vec<f64>| ParseError {
                message: format!("{} parameters, six expected", p.len()),
            })?;
            Ok(ParsedDocument {
                rows: cvi_doc(key, params),
                unknown_paths: fields
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(|s| s.split(',').map(str::to_string).collect())
                    .unwrap_or_default(),
            })
        }

        fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
            let Some(key) = rows.key.first() else {
                return Err(WriteError {
                    message: "the document has no key".into(),
                });
            };
            let Some((_, Column::F64(params))) = rows.values.iter().find(|(n, _)| n == "param")
            else {
                return Err(WriteError {
                    message: "the document has no 'param' f64 column".into(),
                });
            };
            let params: [f64; 6] = params
                .clone()
                .try_into()
                .map_err(|p: Vec<f64>| WriteError {
                    message: format!("{} parameters, six expected", p.len()),
                })?;
            Ok(FakeKind::message(key, params))
        }
    }

    /// The message a [`PanickingKind`] parser panics with. A constant so
    /// the test asserting the payload reached the health lane and the
    /// panic that produced it cannot drift apart.
    pub(crate) const PARSE_PANIC: &str = "the parser fell over";

    /// A [`DocumentKind`] whose `parse` PANICS rather than answering
    /// `Err` — the failure a real vendor parser has that a `Result` does
    /// not describe (an index out of range on a truncated body, an
    /// `unwrap` on an element a schema promised).
    ///
    /// It exists because the receiver thread's panic boundary is not
    /// testable any other way: a boundary is only observable through the
    /// panic it contains, so the fixture has to be the thing that panics.
    /// Everything but `parse` delegates to [`FakeKind`], so a source
    /// wired to this kind is identical to one wired to that one right up
    /// to the panic.
    pub(crate) struct PanickingKind {
        inner: FakeKind,
    }

    impl PanickingKind {
        pub(crate) fn new() -> PanickingKind {
            PanickingKind {
                inner: FakeKind::new(),
            }
        }
    }

    impl DocumentKind for PanickingKind {
        fn name(&self) -> &'static str {
            self.inner.name()
        }

        fn columns(&self) -> &[(&'static str, ColumnType)] {
            self.inner.columns()
        }

        /// Panics with a `String` payload (a formatted `panic!`), which is
        /// the shape `panic_payload_message` reads second — the `&str`
        /// arm is already covered by the runner's own panic tests.
        fn parse(&self, _bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
            panic!("{PARSE_PANIC}");
        }

        fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
            self.inner.write(rows)
        }
    }

    #[test]
    fn the_fake_kind_round_trips_its_own_byte_format() {
        let kind = FakeKind::new();
        let bytes = FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]);
        let parsed = kind.parse(&bytes).expect("its own format parses");
        assert_eq!(parsed.rows, cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.]));
        assert!(parsed.unknown_paths.is_empty());
        assert_eq!(kind.write(&parsed.rows).unwrap(), bytes);
        // Unknown paths ride in a third field, so the receiver's
        // log-once path has something to report.
        let with_unknown = kind
            .parse(b"SPX.Z:1,2,3,4,5,6:a/b,c/d")
            .expect("a third field parses");
        assert_eq!(with_unknown.unknown_paths, vec!["a/b", "c/d"]);
        // And garbage is an `Err`, not a panic and not an empty document.
        assert!(kind.parse(b"not-a-document").is_err());
        assert!(kind.parse(b"SPX.Z:1,2,3").is_err());
        assert!(kind.parse(b"SPX.Z:1,2,3,4,5,six").is_err());
        assert_eq!(check_kind_against(&kind, &cvi_dataset()), Ok(()));
        assert!(
            check_kind_against(&FakeKind::with_extra_column(), &cvi_dataset())
                .unwrap_err()
                .contains("surface_id")
        );
    }

    #[test]
    fn the_cvi_fixture_is_term_major() {
        let rows = cvi_doc("X", [1., 2., 3., 4., 5., 6.]);
        // Axes are [term, node]; term is the first axis.
        let term_axis = &rows.axes[0];
        assert_eq!(term_axis.0, "term");
        // Six rows, term-major: three rows at 2026-09-18, then three at 2026-10-16.
        match &term_axis.1 {
            Column::Date(dates) => {
                assert_eq!(
                    dates,
                    &[
                        d("2026-09-18"),
                        d("2026-09-18"),
                        d("2026-09-18"),
                        d("2026-10-16"),
                        d("2026-10-16"),
                        d("2026-10-16"),
                    ]
                );
            }
            _ => panic!("term axis should be a date column"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::{carried_dataset, cvi_dataset, sample_dataset};
    use super::*;
    use geode_core::schema::{ColumnSpec, ColumnType};

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

    /// The document family's own roles, and a document-level attribute,
    /// must never be selected into a measure-family grain table — even
    /// though nothing in the schema reader can produce this mix today
    /// (`parse_column` only emits `Axis`/`Value` on a document dataset).
    /// `keep`'s "not this branch" arm covering them is otherwise untested:
    /// flipping it from `false` to `true` compiles clean and every other
    /// test stays green, since none of them ever puts one of these roles
    /// on a measure dataset's columns.
    #[test]
    fn create_table_never_selects_a_document_shaped_column() {
        let mut ds = sample_dataset();
        ds.columns.push(ColumnSpec {
            name: "term".into(),
            source_name: None,
            ty: ColumnType::Date,
            required: false,
            textual: false,
            categorical: false,
            role: ColumnRole::Axis,
        });
        ds.columns.push(ColumnSpec {
            name: "param".into(),
            source_name: None,
            ty: ColumnType::F64,
            required: false,
            textual: false,
            categorical: false,
            role: ColumnRole::Value,
        });
        ds.columns.push(ColumnSpec {
            name: "spot_ref".into(),
            source_name: None,
            ty: ColumnType::F64,
            required: false,
            textual: false,
            categorical: false,
            role: ColumnRole::Attribute { grain: None },
        });
        let sql = create_table_sql(&ds, Grain::Position, TableKind::Live);
        assert!(!sql.contains("\"term\""), "{sql}");
        assert!(!sql.contains("\"param\""), "{sql}");
        assert!(!sql.contains("\"spot_ref\""), "{sql}");
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

    #[test]
    fn a_document_dataset_has_one_pair_named_document_and_no_grain_pairs() {
        let ds = cvi_dataset();
        let pairs = table_pairs(&ds);
        assert_eq!(
            pairs,
            vec![TablePair {
                live: "cvi_params_document_live".into(),
                archive: "cvi_params_document_archive".into(),
            }]
        );
        assert_eq!(TablePair::for_document("cvi_params"), pairs[0]);
        assert_eq!(
            history_of("cvi_params", &ds),
            vec![
                "cvi_params_document_archive".to_string(),
                "cvi_params_document_live".to_string(),
            ]
        );
    }

    /// `TablePair::for_document`'s safety claim, checked rather than
    /// asserted in prose: a document pair's name can never be some
    /// grain's table of the same dataset, so the two families can share a
    /// database (and a dataset name) with no chance of
    /// `CREATE TABLE IF NOT EXISTS` handing one family the other's table.
    #[test]
    fn document_is_not_a_grain_short_name() {
        for grain in Grain::ALL {
            assert_ne!(grain.short(), "document");
            assert_ne!(
                TablePair::for_document("ds"),
                TablePair::for_grain("ds", grain)
            );
        }
    }

    #[test]
    fn a_measure_dataset_has_one_pair_per_grain_in_grain_order() {
        let ds = sample_dataset();
        let pairs = table_pairs(&ds);
        assert_eq!(pairs.len(), ds.grains().len());
        for (pair, grain) in pairs.iter().zip(ds.grains()) {
            assert_eq!(*pair, TablePair::for_grain("risk_snapshot", grain));
        }
    }

    #[test]
    fn document_table_columns_are_document_columns_then_the_storage_columns() {
        let ds = cvi_dataset();
        let sql = create_document_table_sql(&ds, TableKind::Live);
        assert!(
            sql.starts_with("CREATE TABLE IF NOT EXISTS cvi_params_document_live ("),
            "{sql}"
        );
        let expected = [
            "\"underlying_ref\" VARCHAR",
            "\"term\" DATE",
            "\"node\" DOUBLE",
            "\"param\" DOUBLE",
            "\"anchor_date\" DATE",
            "\"spot_ref\" DOUBLE",
            "\"batch\" VARCHAR",
            // A document's book is empty, not absent: the column exists
            // and Task 7 writes NULL into it, because every
            // partition-keyed statement in `store` joins on `book`.
            "\"book\" VARCHAR",
            "\"source_file_id\" BIGINT",
            "\"gen_id\" BIGINT",
            "\"source_time\" TIMESTAMP WITH TIME ZONE",
        ];
        let mut last = 0;
        for col in expected {
            let at = sql[last..]
                .find(col)
                .unwrap_or_else(|| panic!("{col} missing or out of order in {sql}"));
            last += at + col.len();
        }
        assert_eq!(
            create_document_table_sql(&ds, TableKind::Archive).replace("_archive", "_live"),
            sql,
            "live and archive carry identical columns"
        );
    }

    #[test]
    fn apply_schema_creates_the_document_pair() {
        use crate::store::Store;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("g.duckdb")).unwrap();
        store.apply_schema(&cvi_dataset()).unwrap();
        for t in ["cvi_params_document_live", "cvi_params_document_archive"] {
            let n: i64 = store
                .writer()
                .query_row(&format!("select count(*) from {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0);
        }
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
