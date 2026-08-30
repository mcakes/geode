//! The per-file load pipeline (spec §5.5):
//!
//!   sentinel -> validate columns -> read_csv into raw staging ->
//!   column map -> split by grain -> publish -> record the generation
//!
//! Schema tolerance lives in the projection. The sentinel declares the
//! CSV's columns, so the projection is decided *before* the file is opened:
//! a declared column is projected under its canonical name, an absent one
//! becomes NULL of the declared type. Drift costs milliseconds instead of a
//! multi-hundred-megabyte parse (spec §5.2).

use crate::health::Health;
use crate::ingest::split::{Conflict, SplitRequest, split_by_grain};
use crate::source::Sentinel;
use crate::store::catalog::{Catalog, FileGeneration, FileId};
use crate::store::publish::{Partition, PublishOutcome, PublishRequest, publish_file};
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
    pub conflicts: Vec<Conflict>,
    pub missing_optional: Vec<String>,
    pub missing_required: Vec<String>,
    /// Source columns the schema does not declare. Not stored — the declared
    /// schema governs the tables — but recorded so they surface in
    /// diagnostics rather than vanishing silently.
    pub extra_columns: Vec<String>,
    pub published: Vec<PublishOutcome>,
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

    // 1. Decide the projection from the sentinel, before opening the CSV.
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

    // 2. read_csv into raw staging. DuckDB's reader is multi-threaded and
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

    // 3. Split by grain, deduplicating coarse measures and canonicalizing
    // pairs; collect any disagreements.
    let catalog = Catalog::new(conn);
    let gen_id = catalog.next_gen_id()?;
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

    // 4. Publish each grain, guarded against backfill.
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
    let partitions: Vec<Partition> = books
        .iter()
        .map(|book| Partition {
            batch: req.batch.to_string(),
            book: book.clone(),
        })
        .collect();

    // A failed lookup must not collapse into "nothing is live yet" — that
    // would silently disable the backfill guard and let an old file
    // overwrite current risk, the exact outcome the guard exists to prevent.
    let mut live_source_time = None;
    for book in &books {
        if let Some(t) = catalog.live_source_time(req.batch, book)? {
            live_source_time = Some(live_source_time.map_or(t, |cur: DateTime<Utc>| cur.max(t)));
        }
    }

    let mut published = Vec::new();
    for (grain, staging_table) in &split.staged {
        published.push(publish_file(
            conn,
            &PublishRequest {
                dataset: req.dataset_name.to_string(),
                grain: *grain,
                staging_table: staging_table.clone(),
                partitions: partitions.clone(),
                gen_id,
                source_time: req.sentinel.as_of,
                live_source_time,
            },
        )?);
    }

    // 5. Record the generation.
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
        books,
        health: health.clone(),
    })?;

    Ok(LoadOutcome {
        file_id,
        gen_id,
        rows: rows as usize,
        health,
        conflicts: split.conflicts,
        missing_optional,
        missing_required,
        extra_columns,
        published,
    })
}

/// Books present in the staged rows, plus a count of rows whose `book` is
/// NULL. Those are reported rather than dropped: a NULL book produces a
/// partition no publish can ever match, so its rows would sit in live
/// forever, invisible to replacement.
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
    /// date component stripped (spec §4.3).
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
        // The fixture plants a wrong model_code on alternate rows of one
        // instrument, so its instrument-grain group disagrees with itself.
        // §3.5 wants that surfaced, not averaged away.
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
        assert_eq!(before, after, "spec §5.7: a failed load never clobbers");
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
