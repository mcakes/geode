//! Table DDL derived from the declared schema. Measure datasets own a live/archive
//! pair per declared grain; document datasets own one pair for the whole dataset.
//! Publication, retention, and history use these [`TablePair`] values. Series
//! datasets use the separate storage layout in [`super::series`].
//!
//! Live tables hold one current generation per partition, keeping live query size
//! independent of retained history. Both live and archive carry `gen_id` and
//! `source_time`; moving outgoing rows preserves their historical identity.
//!
//! A grain table contains its key, measures and attributes declared at that grain,
//! and carried dimensions declared at that or a coarser grain. Schema validation
//! rejects a bare dimension outside every grain key; such a dimension must name
//! the grain that carries it. Attributes also declare their owning grain.

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

/// A dataset's live/archive table names. Measure datasets have one pair per
/// grain; document datasets have one pair without a grain. Carrying the names
/// lets publication, retention, and history work with either family.
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

/// Every live/archive pair a dataset owns: one per declared measure grain or
/// one per document dataset. Series datasets return no pairs; [`super::series`]
/// names their payload and coverage tables. History and retention share this
/// mapping so they cover the same tables as schema creation.
pub fn table_pairs(ds: &DatasetSpec) -> Vec<TablePair> {
    if ds.is_series() {
        // Series history uses received-at timestamps, not generation-based table pairs.
        return Vec::new();
    }
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

/// Columns selected for query-time ENUM encoding by the schema's `categorical`
/// flag. Dimensions default to categorical; keys default to noncategorical.
pub fn categorical_columns(ds: &DatasetSpec) -> Vec<&str> {
    ds.categorical_columns()
}

/// Rebuild a categorical column's derived ENUM from non-NULL values in both
/// live and archive tables. Returns the number of distinct values.
///
/// Stored columns retain their declared types. Query-time casts use the ENUM to
/// produce Arrow dictionaries without rewriting payload tables when new values
/// arrive. Publication refreshes the type after archiving outgoing rows, so
/// current and as-of queries can resolve every retained value. Retention only
/// removes rows, so a dictionary containing evicted values remains safe for
/// text-filter membership checks.
pub fn refresh_enum(
    conn: &duckdb::Connection,
    dataset: &str,
    column: &str,
    live_table: &str,
    archive_table: &str,
) -> Result<usize, crate::store::StoreError> {
    let name = enum_type_name(dataset, column);
    // Payload columns retain their declared types, so dropping the derived
    // ENUM does not change stored data.
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

/// Derived ENUM type names present for a dataset. View compilation uses them
/// for categorical projections; scope compilation applies text matching to the
/// dictionary before filtering payload rows.
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

    // Carried dimensions are payload columns at their declaring grain and every
    // finer grain.
    for c in ds.carried_dimensions_at(grain) {
        cols.push(format!("  \"{}\" {}", c.name, c.ty.sql()));
    }

    // `book` comes from the grain key. `batch` completes the replacement key;
    // `source_file_id` records provenance. Dated filenames are not partition IDs.
    cols.push("  \"batch\" VARCHAR".to_string());
    cols.push("  \"source_file_id\" BIGINT".to_string());
    // Live and archive share the same column order and generation stamps.
    // Publication moves outgoing rows with `select *`, preserving the identity
    // that as-of queries use. Live queries need no history predicate because
    // replacement leaves one current generation per partition.
    let _ = kind;
    cols.push("  \"gen_id\" BIGINT".to_string());
    cols.push("  \"source_time\" TIMESTAMP WITH TIME ZONE".to_string());

    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
        table_name(&ds.name, grain, kind),
        cols.join(",\n")
    )
}

/// Create a document table with `DatasetSpec::document_columns` in key, axes,
/// values, then attribute order, followed by storage metadata.
///
/// `batch` is the document's replacement key, `book` is NULL, and
/// `source_file_id` records provenance. Live and archive both carry `gen_id`
/// and `source_time` so outgoing rows retain their historical identity.
/// Document-level attributes repeat on each row because documents are published,
/// replaced, and read as a whole.
pub fn create_document_table_sql(ds: &DatasetSpec, kind: TableKind) -> String {
    let mut cols: Vec<String> = ds
        .document_columns()
        .iter()
        .map(|c| format!("  \"{}\" {}", c.name, c.ty.sql()))
        .collect();
    // Document tables declare `book` explicitly because there is no grain key.
    // Its NULL value identifies the bookless partition used by publication,
    // retention, and generation-summary queries.
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

/// Every live and archive table owned by the dataset. Generation resolution
/// and rebuilding must include every pair: a partition may occur at only one
/// grain, and its current generation may exist only in live.
///
/// `dataset` must equal `ds.name`. Series datasets return an empty list.
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

/// Replace the dataset's `generations` summary with the distinct generation
/// identities in `tables`. Used during store opening and as a test oracle for
/// incremental summary maintenance.
///
/// Deletion and reinsertion share one transaction. Pass every live/archive
/// table from `history_of` so generations present at only one grain survive.
/// Returns the number of summary rows for this dataset after rebuilding.
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

    /// A fixture with `currency` carried by the instrument grain and `expiry`
    /// explicitly marked as a categorical attribute.
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

    /// A CVI document dataset parsed from TOML. Using the schema reader verifies
    /// that its family, key, axes, and derived column roles match a configuration
    /// the application can load.
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

    /// A local document dataset for publication tests: one key, axis, and value.
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

    /// A `sheets` document: one row per `qty` value, `line` numbered
    /// from one. An empty `qty` produces a zero-row document, which
    /// `DocumentRows::validate`'s row floor refuses — the shape the
    /// failed-local-publish test wants.
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

    /// A series dataset parsed through the schema reader.
    pub(crate) fn series_dataset() -> DatasetSpec {
        let text = "[series]\nfamily = \"series\"\nretention = \"30d\"\nhistory = \"5y\"\n";
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("series")
            .unwrap()
            .clone()
    }

    /// `minutes` one-minute bars from `start`, values `first`, `first + 1`, …
    pub(crate) fn series_rows(
        start: &str,
        minutes: usize,
        first: f64,
    ) -> crate::adapter::SeriesRows {
        let start = ts(start);
        crate::adapter::SeriesRows {
            ts: (0..minutes)
                .map(|i| start + chrono::Duration::minutes(i as i64))
                .collect(),
            value: (0..minutes).map(|i| first + i as f64).collect(),
        }
    }

    pub(crate) fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    pub(crate) fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// Two terms by three nodes, in term-major order, with caller-supplied
    /// values for the six `param` cells.
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

    /// A test [`DocumentKind`] with the same six columns as `cvi_dataset`, using
    /// a compact byte format:
    ///
    /// ```text
    /// SPX.Z:1,2,3,4,5,6[:elem/path,other/path]
    /// ```
    ///
    /// The fields are the key, six `param` values accepted by `cvi_doc`, and an
    /// optional list of unrecognised element paths (`ParsedDocument::unknown_paths`).
    /// Receiver and service tests share this fixture because `geode-data` must not
    /// depend on the production parser in `geode-documents`.
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
        /// source-open time.
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
        // dates, so file identity is not partition identity.
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

    /// Measure-grain DDL excludes document axes, values, and grainless
    /// attributes. A hand-built mixed schema exercises this exclusion even
    /// though the schema reader does not produce that combination.
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

    #[test]
    fn a_series_dataset_has_no_live_archive_pair() {
        let ds = tests_support::series_dataset();
        assert!(table_pairs(&ds).is_empty());
        assert!(history_of("series", &ds).is_empty());
    }

    /// Document table names cannot collide with grain tables of the same
    /// dataset. Otherwise `CREATE TABLE IF NOT EXISTS` could reuse a table
    /// with the wrong family's columns.
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
            // and document publication writes NULL into it, because every
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
