//! Per-file load pipeline:
//!
//! sentinel -> validate columns -> read_csv staging -> column map ->
//! split by grain -> publish -> record generation
//!
//! The sentinel determines the projection before the CSV is opened. Declared
//! columns use canonical names; absent columns become typed NULLs. Staging
//! precedes the transaction that publishes all grains and catalog metadata.

use crate::health::Health;
use crate::ingest::split::{Conflict, SplitRequest, split_by_grain};
use crate::source::Sentinel;
use crate::store::catalog::{Catalog, FileGeneration, FileId};
use crate::store::ddl::TablePair;
use crate::store::publish::{Partition, PublishOutcome, PublishRequest, publish_in_transaction};
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use geode_core::schema::DatasetSpec;
use std::path::Path;

pub struct LoadRequest<'a> {
    pub dataset: &'a DatasetSpec,
    pub dataset_name: &'a str,
    pub csv_path: &'a Path,
    pub sentinel: &'a Sentinel,
    /// The file's partition batch: its name with the date component removed.
    pub batch: &'a str,
}

#[derive(Debug)]
pub struct LoadOutcome {
    pub file_id: FileId,
    pub gen_id: i64,
    pub rows: usize,
    pub health: Health,
    /// Disagreement *within* this file.
    pub conflicts: Vec<Conflict>,
    /// Disagreement between this file and everything already loaded — a
    /// different signal, so it is kept apart rather than merged in.
    pub cross_file: Vec<crate::store::AttributeConflict>,
    pub missing_optional: Vec<String>,
    pub missing_required: Vec<String>,
    /// Source columns the schema does not declare. Not stored — the declared
    /// schema governs the tables — but recorded so they surface in
    /// diagnostics rather than vanishing silently.
    pub extra_columns: Vec<String>,
    pub published: Vec<PublishOutcome>,
    /// The partitions this load wrote, `None` being the bookless one.
    /// Taken from the staged rows rather than the sentinel, because the
    /// sentinel's book list is advisory: a row whose book it omits is
    /// still published, and a bookless partition is in no sentinel at all.
    pub partitions: Vec<Option<String>>,
}

#[derive(Debug)]
pub enum LoadError {
    Store(StoreError),
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Store(e) => write!(f, "{e}"),
            LoadError::Io { path, source } => write!(f, "reading {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<StoreError> for LoadError {
    fn from(e: StoreError) -> Self {
        LoadError::Store(e)
    }
}

const RAW_TABLE: &str = "staging_raw";

pub fn load_file(store: &Store, req: &LoadRequest) -> Result<LoadOutcome, LoadError> {
    let conn = store.writer();
    let declared: Vec<&str> = req.sentinel.columns.iter().map(String::as_str).collect();

    // Decide the projection from the sentinel, before opening the CSV.
    let mut projection: Vec<String> = Vec::new();
    let mut missing_optional = Vec::new();
    let mut missing_required = Vec::new();
    for col in &req.dataset.columns {
        if declared.contains(&col.source_name()) {
            projection.push(format!(
                "\"{}\"::{} as \"{}\"",
                col.source_name(),
                col.ty.sql(),
                col.name
            ));
        } else {
            projection.push(format!("NULL::{} as \"{}\"", col.ty.sql(), col.name));
            if col.required {
                missing_required.push(col.name.clone());
            } else {
                missing_optional.push(col.name.clone());
            }
        }
    }
    let known: Vec<&str> = req
        .dataset
        .columns
        .iter()
        .map(|c| c.source_name())
        .collect();
    let extra_columns: Vec<String> = req
        .sentinel
        .columns
        .iter()
        .filter(|c| !known.contains(&c.as_str()))
        .cloned()
        .collect();

    // read_csv into raw staging. DuckDB's reader is multi-threaded and
    // keeps this loop small.
    let sql = format!(
        "create or replace table {RAW_TABLE} as
         select {projection} from read_csv('{path}', header = true)",
        projection = projection.join(", "),
        path = req.csv_path.to_string_lossy().replace('\'', "''"),
    );
    conn.execute_batch(&sql).map_err(|source| StoreError::Sql {
        statement: sql,
        source,
    })?;

    let rows: i64 = {
        let sql = format!("select count(*) from {RAW_TABLE}");
        conn.query_row(&sql, [], |r| r.get(0))
            .map_err(|source| StoreError::Sql {
                statement: sql,
                source,
            })?
    };

    // Split by grain, deduplicating coarse measures and canonicalizing
    // pairs; collect any disagreements.
    let catalog = Catalog::new(conn);
    // Reserved, not peeked: this id is stamped onto the published rows
    // before the catalog row exists, and if the record below never happens
    // the id must still be spent rather than handed to the next load.
    let gen_id = catalog.reserve_gen_id()?;
    // Reserve the file id up front so the source_file_id stamped onto every
    // data row is the same id the catalog entry gets below. Assigning them
    // independently would break every provenance join.
    let file_id = catalog.reserve_file_id()?;

    let split = split_by_grain(
        conn,
        &SplitRequest {
            dataset: req.dataset,
            raw_table: RAW_TABLE,
            batch: req.batch,
            file_id,
        },
    )?;

    // Staging is complete. Publish every grain, its dictionaries and catalog
    // metadata atomically; an error or unwind rolls the whole generation back.
    let tx = crate::store::begin_transaction(conn)?;
    let conn = &tx;
    let catalog = Catalog::new(conn);

    // Publish each grain, guarded against backfill.
    //
    // The books come from the staged rows, not the sentinel: the sentinel's
    // list is advisory, and a row whose book is absent from it would enter
    // a partition no future publish replaces. Any mismatch is degradation,
    // not a silent drop.
    let (staged_books, unattributed_rows) = distinct_books(conn)?;
    let undeclared: Vec<String> = staged_books
        .iter()
        .filter(|b| !req.sentinel.books.is_empty() && !req.sentinel.books.contains(b))
        .cloned()
        .collect();
    let books = staged_books;
    if books.is_empty() {
        return Err(LoadError::Store(StoreError::Sql {
            statement: format!("publish {}", req.csv_path.display()),
            source: duckdb::Error::InvalidParameterName(
                "no book values in the staged rows; every row would land in a \
                 partition that replacement can never match"
                    .into(),
            ),
        }));
    }
    // Rows with no book are kept and reported rather than dropped, so they
    // are a partition of their own — one the publish has to replace, or
    // every republish of this batch appends another copy of them to live.
    let partitions: Vec<Partition> = books
        .iter()
        .map(|book| Some(book.clone()))
        .chain((unattributed_rows > 0).then_some(None))
        .map(|book| Partition {
            batch: req.batch.to_string(),
            book,
        })
        .collect();

    // Check every written partition, including NULL book, before choosing live
    // or archive. A lookup error must propagate: treating it as no live data
    // would allow an older file to overwrite current data. A changed named-book
    // set does not remove the existing bookless partition from this guard.
    let mut live_source_time = None;
    for partition in &partitions {
        if let Some(t) =
            catalog.live_source_time(req.dataset_name, req.batch, partition.book.as_deref())?
        {
            live_source_time = Some(live_source_time.map_or(t, |cur: DateTime<Utc>| cur.max(t)));
        }
    }

    let mut published = Vec::new();
    for (grain, staging_table) in &split.staged {
        published.push(publish_in_transaction(
            conn,
            &PublishRequest {
                dataset: req.dataset_name.to_string(),
                tables: TablePair::for_grain(req.dataset_name, *grain),
                staging_table: staging_table.clone(),
                partitions: partitions.clone(),
                gen_id,
                source_time: req.sentinel.as_of,
                live_source_time,
            },
        )?);
    }

    // Refresh dimension ENUM types from live and archived values for dictionary
    // encoding and historical text filters. Storage stays VARCHAR; see
    // `refresh_enum` for the type-rebuild contract.
    for col in crate::store::ddl::categorical_columns(req.dataset) {
        // Refresh from the coarsest staged table carrying the column,
        // which is the smallest scan that sees every value. The third
        // disjunct is what lets an opted-in *attribute* like `expiry`
        // refresh from its own grain's table.
        let Some(grain) = geode_core::schema::Grain::ALL.iter().find(|g| {
            (g.key_columns().contains(&col)
                || req.dataset.carries(**g, col)
                || req.dataset.column(col).and_then(|c| c.grain()) == Some(**g))
                && split.staged.iter().any(|(s, _)| s == *g)
        }) else {
            continue;
        };
        crate::store::ddl::refresh_enum(
            conn,
            req.dataset_name,
            col,
            &crate::store::ddl::table_name(
                req.dataset_name,
                *grain,
                crate::store::ddl::TableKind::Live,
            ),
            &crate::store::ddl::table_name(
                req.dataset_name,
                *grain,
                crate::store::ddl::TableKind::Archive,
            ),
        )?;
    }

    // Record the generation.
    let mut degradations: Vec<String> = Vec::new();
    if !missing_required.is_empty() {
        degradations.push(format!(
            "required columns missing: {}",
            missing_required.join(", ")
        ));
    }
    if unattributed_rows > 0 {
        degradations.push(format!("{unattributed_rows} rows have no book"));
    }
    if !undeclared.is_empty() {
        degradations.push(format!(
            "books present in the data but absent from the sentinel: {}",
            undeclared.join(", ")
        ));
    }
    // A carried dimension that varied inside its key: the
    // file disagrees with the schema's dependency claim. The row was
    // written with one of the values; say so rather than hide it.
    for c in &split.conflicts {
        if req
            .dataset
            .column(&c.column)
            .and_then(|col| col.carried_grain())
            .is_some()
        {
            degradations.push(format!(
                "'{}' varies within its {:?} key in {} group(s)",
                c.column, c.grain, c.groups
            ));
        }
    }
    let health = if degradations.is_empty() {
        Health::Ok
    } else {
        Health::Degraded {
            reason: degradations.join("; "),
        }
    };
    let meta = std::fs::metadata(req.csv_path).map_err(|source| LoadError::Io {
        path: req.csv_path.to_path_buf(),
        source,
    })?;
    // A generation every grain filed straight to the archive was never
    // live, so it must not count toward freshness. Recorded all the
    // same: the load happened, and provenance should say so.
    let archived_only = !published.is_empty()
        && published
            .iter()
            .all(|p| matches!(p, PublishOutcome::ArchivedOnly { .. }));
    let file_id = catalog.record(&FileGeneration {
        file_id,
        dataset: req.dataset_name.to_string(),
        batch: req.batch.to_string(),
        path: req.csv_path.to_path_buf(),
        size: meta.len(),
        mtime: meta
            .modified()
            .ok()
            .map(DateTime::<Utc>::from)
            .unwrap_or(req.sentinel.as_of),
        source_time: req.sentinel.as_of,
        gen_id,
        loaded_at: Utc::now(),
        row_count: rows as usize,
        // Every partition written, not just the named books: the bookless
        // one needs a `file_books` row or its freshness is unrecoverable.
        books: partitions.iter().map(|p| p.book.clone()).collect(),
        archived_only,
        health: health.clone(),
    })?;

    // Cross-file disagreement, recorded per load so diagnostics can
    // show a trend. Read after publishing, because it is the
    // whole live table that has to agree, not this file.
    let mut cross_file = Vec::new();
    for grain in req.dataset.grains() {
        cross_file.extend(catalog.attribute_conflicts(req.dataset_name, req.dataset, grain)?);
    }
    catalog.record_attribute_conflicts(req.dataset_name, &cross_file, Utc::now())?;

    crate::store::commit_transaction(tx)?;

    Ok(LoadOutcome {
        file_id,
        gen_id,
        rows: rows as usize,
        health,
        conflicts: split.conflicts,
        cross_file,
        missing_optional,
        missing_required,
        extra_columns,
        published,
        partitions: partitions.iter().map(|p| p.book.clone()).collect(),
    })
}

/// Books present in staged rows and the number of NULL-book rows. NULL books
/// are retained as a replaceable partition and reported as degradation.
fn distinct_books(conn: &duckdb::Connection) -> Result<(Vec<String>, usize), StoreError> {
    let sql = format!("select book, count(*) from {RAW_TABLE} group by book order by book");
    let err = |source| StoreError::Sql {
        statement: sql.clone(),
        source,
    };
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let mut rows = stmt.query([]).map_err(err)?;
    let mut books = Vec::new();
    let mut unattributed = 0usize;
    while let Some(row) = rows.next().map_err(err)? {
        match row.get::<_, Option<String>>(0).map_err(err)? {
            Some(b) => books.push(b),
            None => unattributed += row.get::<_, i64>(1).map_err(err)? as usize,
        }
    }
    Ok((books, unattributed))
}

#[cfg(test)]
pub(crate) mod tests_support {
    use geode_demo_data::{EmitOptions, GeneratorConfig, emit_directory, generate};

    /// The tree view over the fixture dataset: lhu >
    /// underlying > position, one measure at underlying grain and one at
    /// position grain.
    pub(crate) fn tree_view() -> geode_core::view::ViewSpec {
        use geode_core::config::{LayerDoc, merge_docs};
        let text = r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu", "underlying_ref", "position_ref"]
[[tree.columns]]
name = "delta01"
kind = "measure"
[[tree.columns]]
name = "daily_trading_pnl"
kind = "measure"
"#;
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        geode_core::view::ViewSpec::from_doc(&doc)
            .0
            .into_iter()
            .next()
            .unwrap()
    }

    /// The same fixture the load tests use: a populated store with the
    /// schema applied and catalog tables created, plus a generated source
    /// directory. Both TempDirs are returned so the caller keeps them alive.
    pub(crate) fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        crate::store::Store,
        geode_core::schema::DatasetSpec,
        geode_demo_data::EmittedDirectory,
    ) {
        let db_dir = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(db_dir.path().join("geode.duckdb")).unwrap();
        let ds = super::tests::schema();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        let batch = generate(&GeneratorConfig {
            rows: 3_000,
            seed: 42,
            business_dates: 1,
        });
        let emitted = emit_directory(&batch, &EmitOptions::new(src_dir.path())).unwrap();
        (db_dir, src_dir, store, ds, emitted)
    }

    /// `risk_2026-08-24_BK000_part1` -> `BK000_part1`: the filename with its
    /// date component stripped.
    pub(crate) fn batch_of(csv: &std::path::Path) -> String {
        let stem = csv.file_stem().unwrap().to_string_lossy().to_string();
        let parts: Vec<&str> = stem.split('_').collect();
        parts[2..].join("_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::Health;
    use crate::store::Store;
    use geode_core::schema::Grain;

    pub(crate) fn schema() -> geode_core::schema::DatasetSpec {
        use geode_core::config::{LayerDoc, merge_docs};
        let text = r#"
[risk_snapshot.columns.business_date]
type = "utf8"
role = "attribute"
grain = "position"
source_name = "BusinessDate"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
source_name = "Book"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
source_name = "LHU"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
source_name = "PositionRef"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
source_name = "Counterparty"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
source_name = "InstrumentRef"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
source_name = "Underlying1Ref"
[risk_snapshot.columns.underlying2_ref]
type = "utf8"
role = "dimension"
source_name = "Underlying2Ref"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
source_name = "Delta01"
[risk_snapshot.columns.skew01]
type = "f64"
role = "measure"
grain = "underlying"
source_name = "Skew01"
required = false
[risk_snapshot.columns.cross_gamma02]
type = "f64"
role = "measure"
grain = "underlying_pair"
source_name = "CrossGamma02"
[risk_snapshot.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
source_name = "NPV"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
source_name = "DailyTradingPNL"
[risk_snapshot.columns.model_code]
type = "utf8"
role = "attribute"
grain = "instrument"
source_name = "ModelCode"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        geode_core::schema::SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk_snapshot")
            .unwrap()
            .clone()
    }

    struct Fixture {
        _db: tempfile::TempDir,
        _src: tempfile::TempDir,
        store: Store,
        emitted: geode_demo_data::EmittedDirectory,
        ds: geode_core::schema::DatasetSpec,
    }

    fn fixture() -> Fixture {
        let (db, src, store, ds, emitted) = super::tests_support::fixture();
        Fixture {
            _db: db,
            _src: src,
            store,
            emitted,
            ds,
        }
    }

    fn ready_file(f: &Fixture) -> &geode_demo_data::EmittedFile {
        f.emitted
            .files
            .iter()
            .find(|x| x.sentinel_path.is_some())
            .unwrap()
    }

    fn load(f: &Fixture, file: &geode_demo_data::EmittedFile) -> LoadOutcome {
        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let sentinel = crate::source::parse_sentinel(&text).unwrap();
        let batch = super::tests_support::batch_of(&file.csv_path);
        load_file(
            &f.store,
            &LoadRequest {
                dataset: &f.ds,
                dataset_name: "risk_snapshot",
                csv_path: &file.csv_path,
                sentinel: &sentinel,
                batch: &batch,
            },
        )
        .unwrap()
    }

    fn count(f: &Fixture, table: &str) -> i64 {
        f.store
            .writer()
            .query_row(&format!("select count(*) from {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn loads_a_file_end_to_end_into_live() {
        let f = fixture();
        let file = ready_file(&f);
        let out = load(&f, file);
        assert_eq!(out.rows, file.rows);
        assert_eq!(out.health, Health::Ok);

        let live = count(&f, "risk_snapshot_position_live");
        assert!(
            live > 0 && live < file.rows as i64,
            "position grain must collapse rows: {live} from {}",
            file.rows
        );
    }

    #[test]
    fn a_missing_optional_column_is_silent_and_becomes_null() {
        let f = fixture();
        let file = f
            .emitted
            .files
            .iter()
            .find(|x| x.sentinel_path.is_some() && !x.columns.iter().any(|c| c == "Skew01"))
            .expect("a file missing the optional column");
        let out = load(&f, file);
        assert_eq!(
            out.health,
            Health::Ok,
            "optional absence is expected, not a warning"
        );
        assert_eq!(out.missing_optional, vec!["skew01".to_string()]);
        let nulls = count(&f, "risk_snapshot_underlying_live where skew01 is null");
        assert!(nulls > 0);
    }

    #[test]
    fn a_missing_required_column_degrades_but_still_loads() {
        let f = fixture();
        let file = ready_file(&f);
        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let mut sentinel = crate::source::parse_sentinel(&text).unwrap();
        sentinel.columns.retain(|c| c != "NPV");

        let out = load_file(
            &f.store,
            &LoadRequest {
                dataset: &f.ds,
                dataset_name: "risk_snapshot",
                csv_path: &file.csv_path,
                sentinel: &sentinel,
                batch: &super::tests_support::batch_of(&file.csv_path),
            },
        )
        .unwrap();
        assert!(
            matches!(out.health, Health::Degraded { .. }),
            "{:?}",
            out.health
        );
        assert_eq!(out.missing_required, vec!["npv".to_string()]);
        assert!(
            out.rows > 0,
            "a degraded load still delivers the columns it has"
        );
    }

    #[test]
    fn undeclared_source_columns_are_reported_not_silently_dropped() {
        let f = fixture();
        let file = ready_file(&f);
        let out = load(&f, file);
        // The generator emits _USD twins the test schema does not declare.
        assert!(
            out.extra_columns.iter().any(|c| c == "Delta01_USD"),
            "undeclared columns must be visible: {:?}",
            out.extra_columns
        );
    }

    #[test]
    fn the_planted_attribute_disagreement_is_detected() {
        // Conflicting model codes within one instrument must surface as degradation,
        // not disappear into the selected representative value.
        let f = fixture();
        let mut found = Vec::new();
        for file in f.emitted.files.iter().filter(|x| x.sentinel_path.is_some()) {
            found.extend(load(&f, file).conflicts);
        }
        assert!(
            !f.emitted.conflicting_instruments.is_empty(),
            "fixture precondition: a conflict must have been planted"
        );
        assert!(
            found
                .iter()
                .any(|c| c.column == "model_code"
                    && c.grain == geode_core::schema::Grain::Instrument),
            "the detector must report the planted model_code disagreement: {found:?}"
        );
    }

    /// A minimal schema with `currency` a dimension carried by the
    /// instrument grain — deliberately not the shared
    /// `schema()`/`fixture()` above, which the generator populates and
    /// has no way to plant a carried-dimension disagreement in.
    fn carried_schema() -> geode_core::schema::DatasetSpec {
        use geode_core::config::{LayerDoc, merge_docs};
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
source_name = "Book"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
source_name = "LHU"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
source_name = "PositionRef"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
source_name = "Counterparty"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
source_name = "InstrumentRef"
[risk_snapshot.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
source_name = "Currency"
[risk_snapshot.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
source_name = "NPV"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        geode_core::schema::SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk_snapshot")
            .unwrap()
            .clone()
    }

    #[test]
    fn a_carried_dimension_dependency_violation_degrades_health() {
        // Two rows share an instrument key but disagree on its carried currency.
        // Publishing one representative value must also report degraded health.
        let ds = carried_schema();
        let db_dir = tempfile::tempdir().unwrap();
        let store = Store::open(db_dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();

        let src_dir = tempfile::tempdir().unwrap();
        let csv_path = src_dir.path().join("risk_2026-08-24_BK0.csv");
        std::fs::write(
            &csv_path,
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK0,L0,P1,C,I1,USD,100\n\
             BK0,L0,P1,C,I1,EUR,100\n",
        )
        .unwrap();

        let sentinel = crate::source::Sentinel {
            as_of: Utc::now(),
            columns: [
                "Book",
                "LHU",
                "PositionRef",
                "Counterparty",
                "InstrumentRef",
                "Currency",
                "NPV",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            books: vec!["BK0".to_string()],
            row_count: None,
            dataset: None,
            business_date: None,
        };

        let out = load_file(
            &store,
            &LoadRequest {
                dataset: &ds,
                dataset_name: "risk_snapshot",
                csv_path: &csv_path,
                sentinel: &sentinel,
                batch: "BK0",
            },
        )
        .unwrap();

        assert!(
            matches!(&out.health, Health::Degraded { reason } if reason.contains("currency")),
            "{:?}",
            out.health
        );
    }

    /// Rewrite every value of one CSV column, leaving the rest intact.
    fn rewrite_column(csv: &str, header: &str, value: &str) -> String {
        let mut lines = csv.lines();
        let head = lines.next().expect("header");
        let at = head
            .split(',')
            .position(|h| h == header)
            .expect("column present");
        let mut out = String::from(head);
        for line in lines {
            let mut fields: Vec<&str> = line.split(',').collect();
            fields[at] = value;
            out.push('\n');
            out.push_str(&fields.join(","));
        }
        out
    }

    /// Blank one CSV column on every other data row, leaving the rest intact.
    fn blank_alternate_rows(csv: &str, header: &str) -> String {
        let mut lines = csv.lines();
        let head = lines.next().expect("header");
        let at = head
            .split(',')
            .position(|h| h == header)
            .expect("column present");
        let mut out = String::from(head);
        for (i, line) in lines.enumerate() {
            let mut fields: Vec<&str> = line.split(',').collect();
            if i % 2 == 0 {
                fields[at] = "";
            }
            out.push('\n');
            out.push_str(&fields.join(","));
        }
        out
    }

    /// Set `header` to `value` on every row that currently has one,
    /// leaving blanks blank — a file covering a different book, with the
    /// same bookless rows.
    fn rename_books(csv: &str, header: &str, value: &str) -> String {
        let mut lines = csv.lines();
        let head = lines.next().expect("header");
        let at = head
            .split(',')
            .position(|h| h == header)
            .expect("column present");
        let mut out = String::from(head);
        for line in lines {
            let mut fields: Vec<&str> = line.split(',').collect();
            if !fields[at].is_empty() {
                fields[at] = value;
            }
            out.push('\n');
            out.push_str(&fields.join(","));
        }
        out
    }

    #[test]
    fn an_older_file_cannot_overwrite_the_bookless_partition() {
        // Change the named books while retaining NULL-book rows in an older file.
        // The bookless partition must still be protected from backfill overwriting
        // its newer live generation.
        let f = fixture();
        let file = ready_file(&f);
        let text = std::fs::read_to_string(&file.csv_path).unwrap();
        let sentinel_text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let mut sentinel = crate::source::parse_sentinel(&sentinel_text).unwrap();

        // v1: real books plus bookless rows, at the sentinel's own time.
        let v1 = file.csv_path.with_file_name("risk_20260830_shift.csv");
        std::fs::write(&v1, blank_alternate_rows(&text, "Book")).unwrap();
        let load = |path: &std::path::Path, sentinel: &crate::source::Sentinel| {
            load_file(
                &f.store,
                &LoadRequest {
                    dataset: &f.ds,
                    dataset_name: "risk_snapshot",
                    csv_path: path,
                    sentinel,
                    batch: "shift",
                },
            )
            .unwrap()
        };
        load(&v1, &sentinel);

        // v2: same batch, same bookless rows, a different named book, and
        // an hour *older*. It must not become live.
        let v2 = file
            .csv_path
            .with_file_name("risk_20260830_shift_older.csv");
        let renamed = rename_books(&blank_alternate_rows(&text, "Book"), "Book", "BKZZZ");
        std::fs::write(&v2, renamed).unwrap();
        sentinel.as_of -= chrono::Duration::hours(1);
        sentinel.books = vec!["BKZZZ".to_string()];
        let older = load(&v2, &sentinel);

        assert!(
            !older.published.is_empty(),
            "the load must have published something to assert about"
        );
        assert!(
            older
                .published
                .iter()
                .all(|p| matches!(p, PublishOutcome::ArchivedOnly { .. })),
            "an older file must become history, not overwrite the bookless \
             partition that is live at a newer time: {:?}",
            older.published
        );

        // And it must not move freshness. The generation is recorded —
        // the load happened — but it never made a row live, so counting
        // it would report the dataset as stale as a file nobody can see.
        use chrono::Timelike;
        let cat = crate::store::Catalog::new(f.store.writer());
        let stale_hour = sentinel.as_of.hour();
        assert!(
            !cat.book_freshness("risk_snapshot")
                .unwrap()
                .iter()
                .any(|(_, t)| t.hour() == stale_hour),
            "a generation that never went live must not appear in freshness"
        );
    }

    #[test]
    fn republishing_a_file_with_bookless_rows_does_not_accumulate_them() {
        // Rows with no book are kept and reported, which makes them
        // live data in a partition of their own. That partition has to be
        // replaced on republish like any other, or every republish of the
        // batch appends another copy and the desk total drifts upward.
        let f = fixture();
        let file = ready_file(&f);
        let text = std::fs::read_to_string(&file.csv_path).unwrap();
        let twin = file.csv_path.with_file_name("risk_20260830_blank.csv");
        std::fs::write(&twin, blank_alternate_rows(&text, "Book")).unwrap();
        let sentinel_text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let mut sentinel = crate::source::parse_sentinel(&sentinel_text).unwrap();

        let bookless = |f: &Fixture| -> i64 {
            f.store
                .writer()
                .query_row(
                    "select count(*) from risk_snapshot_position_live where book is null",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let load_twin = |sentinel: &crate::source::Sentinel| {
            load_file(
                &f.store,
                &LoadRequest {
                    dataset: &f.ds,
                    dataset_name: "risk_snapshot",
                    csv_path: &twin,
                    sentinel,
                    batch: "blank",
                },
            )
            .unwrap()
        };

        let first = load_twin(&sentinel);
        assert!(
            matches!(&first.health, Health::Degraded { reason } if reason.contains("no book")),
            "{:?}",
            first.health
        );
        let after_first = bookless(&f);
        assert!(after_first > 0, "the bookless rows must be live");
        let live_after_first = count(&f, "risk_snapshot_position_live");

        // A corrected republish of the same batch, an hour later.
        sentinel.as_of += chrono::Duration::hours(1);
        load_twin(&sentinel);
        assert_eq!(
            bookless(&f),
            after_first,
            "the bookless partition is replaced, not appended to"
        );
        assert_eq!(
            count(&f, "risk_snapshot_position_live"),
            live_after_first,
            "live holds exactly one generation of the batch"
        );
    }

    #[test]
    fn the_same_instrument_disagreeing_across_files_is_recorded() {
        // Reuse instruments across batches with different attributes to exercise
        // cross-file disagreement separately from within-file conflicts.
        let f = fixture();
        let file = ready_file(&f);
        load(&f, file);

        let cat = crate::store::Catalog::new(f.store.writer());
        let one_file = cat
            .attribute_conflicts("risk_snapshot", &f.ds, Grain::Instrument)
            .unwrap();
        assert!(
            !one_file.iter().any(|c| c.column == "model_code"),
            "one file's rows dedup to one value per instrument: {one_file:?}"
        );

        // The same instruments again under a different batch, carrying a
        // different model code — a separate partition, so both survive in
        // live and the disagreement only exists *between* the files.
        let text = std::fs::read_to_string(&file.csv_path).unwrap();
        let twin = file.csv_path.with_file_name("risk_20260830_twin.csv");
        std::fs::write(&twin, rewrite_column(&text, "ModelCode", "ZZZZ")).unwrap();
        let sentinel_text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let sentinel = crate::source::parse_sentinel(&sentinel_text).unwrap();
        load_file(
            &f.store,
            &LoadRequest {
                dataset: &f.ds,
                dataset_name: "risk_snapshot",
                csv_path: &twin,
                sentinel: &sentinel,
                batch: "twin",
            },
        )
        .unwrap();

        let conflicts = cat
            .attribute_conflicts("risk_snapshot", &f.ds, Grain::Instrument)
            .unwrap();
        let model = conflicts
            .iter()
            .find(|c| c.column == "model_code")
            .unwrap_or_else(|| panic!("model_code disagreement missed: {conflicts:?}"));
        assert!(model.entities > 0);
    }

    #[test]
    fn conflict_counts_are_recorded_so_diagnostics_can_show_a_trend() {
        // A single reading says little; a column that conflicts on every
        // load is evidence its declared grain is wrong.
        let f = fixture();
        load(&f, ready_file(&f));
        let cat = crate::store::Catalog::new(f.store.writer());
        let observed = vec![crate::store::AttributeConflict {
            grain: Grain::Instrument,
            column: "model_code".into(),
            entities: 3,
        }];
        cat.record_attribute_conflicts("risk_snapshot", &observed, Utc::now())
            .unwrap();
        cat.record_attribute_conflicts("risk_snapshot", &observed, Utc::now())
            .unwrap();
        let history = cat.attribute_conflict_history("risk_snapshot").unwrap();
        assert_eq!(history.len(), 2, "each load appends: {history:?}");
        assert_eq!(history[0].1.entities, 3);
    }

    #[test]
    fn dimension_columns_come_back_dictionary_encoded() {
        // A dictionary-encoded result avoids per-cell string comparisons. A plain
        // VARCHAR would return StringArray instead.
        let f = fixture();
        load(&f, ready_file(&f));
        let conn = f.store.writer();
        let mut stmt = conn
            .prepare(
                "select book::risk_snapshot_book_enum
                 from risk_snapshot_position_live limit 10",
            )
            .unwrap();
        let batches: Vec<duckdb::arrow::record_batch::RecordBatch> =
            stmt.query_arrow([]).unwrap().collect();
        assert!(
            matches!(
                batches[0].schema().field(0).data_type(),
                duckdb::arrow::datatypes::DataType::Dictionary(_, _)
            ),
            "got {:?}",
            batches[0].schema().field(0).data_type()
        );
    }

    #[test]
    fn a_new_dimension_value_does_not_need_a_table_rewrite() {
        // DuckDB has no ALTER TYPE ADD VALUE, so an ENUM *column* could
        // only grow by rewriting every table. The type is derived instead:
        // refreshing it after a load is a metadata operation.
        let f = fixture();
        load(&f, ready_file(&f));
        let before = crate::store::ddl::refresh_enum(
            f.store.writer(),
            "risk_snapshot",
            "book",
            "risk_snapshot_position_live",
            "risk_snapshot_position_archive",
        )
        .unwrap();
        f.store
            .writer()
            .execute_batch(
                "create temp table one as
                     select * from risk_snapshot_position_live limit 1;
                 update one set book = 'BK999';
                 insert into risk_snapshot_position_live select * from one;",
            )
            .unwrap();
        let after = crate::store::ddl::refresh_enum(
            f.store.writer(),
            "risk_snapshot",
            "book",
            "risk_snapshot_position_live",
            "risk_snapshot_position_archive",
        )
        .unwrap();
        assert_eq!(after, before + 1, "the new book joined the type");
    }

    #[test]
    fn reloading_the_same_batch_replaces_rather_than_accumulates() {
        let f = fixture();
        let file = ready_file(&f);
        let first = load(&f, file);
        let before = count(&f, "risk_snapshot_underlying_live");
        let second = load(&f, file);
        let after = count(&f, "risk_snapshot_underlying_live");
        assert_eq!(before, after, "live must not accumulate across reloads");
        assert!(second.gen_id > first.gen_id);

        // Equal counts alone prove nothing — they also hold if the second
        // load never touched live. Assert the replacement path actually ran.
        assert!(
            second
                .published
                .iter()
                .all(|p| matches!(p, crate::store::PublishOutcome::Published { .. })),
            "the reload must replace live, not be filed as history: {:?}",
            second.published
        );
        let archived = count(&f, "risk_snapshot_underlying_archive");
        assert_eq!(
            archived, before,
            "the superseded generation must have moved to archive"
        );
    }

    #[test]
    fn a_value_dropped_from_a_republished_partition_still_shows_in_the_enum() {
        // Use a non-key categorical column: replacing the same partitions can remove
        // its old value from live while preserving it in archive. The dictionary
        // must retain that value for historical text filters. Changing `book` would
        // leave the old partition live and would not exercise this condition.
        let f = fixture();
        let file = ready_file(&f);
        load(&f, file);

        let old_lhu: String = f
            .store
            .writer()
            .query_row(
                "select lhu from risk_snapshot_position_live limit 1",
                [],
                |r| r.get(0),
            )
            .unwrap();

        let csv_text = std::fs::read_to_string(&file.csv_path).unwrap();
        let twin = file
            .csv_path
            .with_file_name("risk_20260830_lhu_dropped.csv");
        std::fs::write(&twin, rewrite_column(&csv_text, "LHU", "LHU_NEW")).unwrap();
        let sentinel_text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let mut sentinel = crate::source::parse_sentinel(&sentinel_text).unwrap();
        // A newer source time: a corrected republish of the same batch,
        // not a separate generation that leaves the old one live.
        sentinel.as_of += chrono::Duration::hours(1);
        let batch = super::tests_support::batch_of(&file.csv_path);
        load_file(
            &f.store,
            &LoadRequest {
                dataset: &f.ds,
                dataset_name: "risk_snapshot",
                csv_path: &twin,
                sentinel: &sentinel,
                batch: &batch,
            },
        )
        .unwrap();

        // Live no longer carries the old value...
        let still_live: i64 = f
            .store
            .writer()
            .query_row(
                "select count(*) from risk_snapshot_position_live where lhu = ?",
                duckdb::params![old_lhu],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            still_live, 0,
            "the republish must have replaced every row carrying the old value"
        );

        // ...but the dictionary still does, because `refresh_enum` reads
        // the archive the republish just filed the old rows into.
        let dictionary: Vec<String> = {
            let conn = f.store.writer();
            let mut stmt = conn
                .prepare("select v from unnest(enum_range(null::risk_snapshot_lhu_enum)) t(v)")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert!(
            dictionary.contains(&old_lhu),
            "the dropped value must still be in the dictionary: {dictionary:?}"
        );
    }

    #[test]
    fn a_missing_csv_fails_without_touching_live() {
        let f = fixture();
        let file = ready_file(&f);
        load(&f, file);
        let before = count(&f, "risk_snapshot_underlying_live");

        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let sentinel = crate::source::parse_sentinel(&text).unwrap();
        let missing = file.csv_path.with_file_name("nope.csv");
        let err = load_file(
            &f.store,
            &LoadRequest {
                dataset: &f.ds,
                dataset_name: "risk_snapshot",
                csv_path: &missing,
                sentinel: &sentinel,
                batch: "BK000",
            },
        );
        assert!(err.is_err());
        let after = count(&f, "risk_snapshot_underlying_live");
        assert_eq!(before, after, "a failed load never clobbers");
    }

    #[test]
    fn every_ready_file_loads_and_freshness_rolls_up() {
        let f = fixture();
        let ready: Vec<_> = f
            .emitted
            .files
            .iter()
            .filter(|x| x.sentinel_path.is_some())
            .collect();
        for file in &ready {
            load(&f, file);
        }
        let cat = crate::store::Catalog::new(f.store.writer());
        let fresh = cat.book_freshness("risk_snapshot").unwrap();
        assert!(!fresh.is_empty(), "books must have freshness recorded");
        let as_of = cat.dataset_as_of("risk_snapshot", &[]).unwrap();
        assert!(as_of.is_some());
    }
}
