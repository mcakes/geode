//! Ingest benchmarks (spec §9.3). Establishes the cold-start and
//! throughput baselines, and answers the parse-parallelism question spec
//! §5.6 declines to assume.
//!
//! Uses the generated source directory, never checked-in fixtures (§7.4).

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::schema::{DatasetSpec, SchemaSpec};
use geode_data::ingest::{LoadRequest, build_plan, load_file};
use geode_data::source::{CandidateState, Priority, Readiness, SourceSpec, discover};
use geode_data::store::{Catalog, Store};
use geode_demo_data::{EmitOptions, GeneratorConfig, emit_directory, generate};
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// The dataset declaration the benchmarks load against — the same shape the
/// load-pipeline tests use.
fn schema() -> DatasetSpec {
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
[risk_snapshot.columns.gamma01]
type = "f64"
role = "measure"
grain = "underlying"
source_name = "Gamma01"
[risk_snapshot.columns.vega01]
type = "f64"
role = "measure"
grain = "underlying"
source_name = "Vega01"
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
[risk_snapshot.columns.daily_pnl]
type = "f64"
role = "measure"
grain = "instrument"
source_name = "DailyPNL"
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
    SchemaSpec::from_doc(&doc)
        .0
        .dataset("risk_snapshot")
        .unwrap()
        .clone()
}

fn source_spec(root: &Path) -> SourceSpec {
    SourceSpec {
        name: "risk".into(),
        dataset: "risk_snapshot".into(),
        paths: vec![format!("{}/*.csv", root.display())],
        readiness: Readiness::Sentinel,
        priority: Priority::LatestRisk,
        poll_interval: Duration::from_secs(30),
        pending_timeout: Duration::from_secs(3600),
        batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
    }
}

/// Emit a source directory of `rows` rows once, reused across samples.
fn source_dir(rows: usize) -> (tempfile::TempDir, geode_demo_data::EmittedDirectory) {
    let dir = tempfile::tempdir().unwrap();
    let batch = generate(&GeneratorConfig {
        rows,
        seed: 42,
        business_dates: 1,
    });
    let emitted = emit_directory(&batch, &EmitOptions::new(dir.path())).unwrap();
    (dir, emitted)
}

fn open_ready(db_dir: &Path, ds: &DatasetSpec) -> Store {
    let store = Store::open(db_dir.join("geode.duckdb")).unwrap();
    store.apply_schema(ds).unwrap();
    Catalog::new(store.writer()).ensure_tables().unwrap();
    store
}

/// Discover, plan and load every ready file in `src`.
fn run_full_ingest(db_dir: &Path, src: &Path, ds: &DatasetSpec) -> usize {
    let store = open_ready(db_dir, ds);
    let spec = source_spec(src);
    let found = {
        let cat = Catalog::new(store.writer());
        discover(&spec, &cat, SystemTime::now()).unwrap()
    };
    let plan = build_plan(&[(spec, found)]);
    let mut rows = 0;
    for item in &plan.items {
        let CandidateState::Ready(sentinel) = &item.candidate.state else {
            continue;
        };
        rows += load_file(
            &store,
            &LoadRequest {
                dataset: ds,
                dataset_name: "risk_snapshot",
                csv_path: &item.candidate.csv_path,
                sentinel,
                batch: &item.batch,
            },
        )
        .unwrap()
        .rows;
    }
    rows
}

/// Stage every ready file sequentially through one connection — no publish,
/// so the comparison isolates parsing.
fn stage_all_sequential(db_dir: &Path, src: &Path, ds: &DatasetSpec) -> usize {
    let store = open_ready(db_dir, ds);
    let spec = source_spec(src);
    let found = {
        let cat = Catalog::new(store.writer());
        discover(&spec, &cat, SystemTime::now()).unwrap()
    };
    let mut n = 0;
    for (i, c) in found.iter().enumerate() {
        if !matches!(c.state, CandidateState::Ready(_)) {
            continue;
        }
        stage_one(store.writer(), &c.csv_path, i);
        n += 1;
    }
    n
}

/// The same staging spread across `workers` threads, each with its own
/// connection on the same database.
fn stage_all_parallel(db_dir: &Path, src: &Path, ds: &DatasetSpec, workers: usize) -> usize {
    let store = open_ready(db_dir, ds);
    let spec = source_spec(src);
    let found = {
        let cat = Catalog::new(store.writer());
        discover(&spec, &cat, SystemTime::now()).unwrap()
    };
    let ready: Vec<_> = found
        .iter()
        .filter(|c| matches!(c.state, CandidateState::Ready(_)))
        .map(|c| c.csv_path.clone())
        .collect();

    let chunks: Vec<Vec<_>> = ready
        .chunks(ready.len().div_ceil(workers.max(1)))
        .map(|c| c.to_vec())
        .collect();

    std::thread::scope(|scope| {
        for (w, chunk) in chunks.iter().enumerate() {
            let conn = store.reader().unwrap();
            scope.spawn(move || {
                for (i, path) in chunk.iter().enumerate() {
                    stage_one(&conn, path, w * 1000 + i);
                }
            });
        }
    });
    ready.len()
}

/// `read_csv` one file into its own uniquely named staging table.
fn stage_one(conn: &duckdb::Connection, csv: &Path, n: usize) {
    let sql = format!(
        "create or replace table bench_stage_{n} as
         select * from read_csv('{}', header = true)",
        csv.to_string_lossy().replace('\'', "''")
    );
    conn.execute_batch(&sql).unwrap();
}

fn bench_cold_start(c: &mut Criterion) {
    let ds = schema();
    let mut group = c.benchmark_group("ingest_cold_start");
    group.sample_size(10);
    for rows in [100_000usize, 1_000_000] {
        let (src, _emitted) = source_dir(rows);
        group.bench_function(format!("{rows}_rows"), |b| {
            b.iter_batched(
                || tempfile::tempdir().unwrap(),
                |db_dir| black_box(run_full_ingest(db_dir.path(), src.path(), &ds)),
                criterion::BatchSize::PerIteration,
            )
        });
    }
    group.finish();
}

fn bench_warm_start(c: &mut Criterion) {
    // Reopening a populated database must be milliseconds: live tables are
    // queryable the moment it opens, which is what keeps the <1s startup
    // budget reachable without reading a CSV (spec §5.4, §7.1).
    let ds = schema();
    let (src, _emitted) = source_dir(100_000);
    let db_dir = tempfile::tempdir().unwrap();
    run_full_ingest(db_dir.path(), src.path(), &ds);

    let mut group = c.benchmark_group("ingest_warm_start");
    group.sample_size(50);
    group.bench_function("reopen_populated_db", |b| {
        b.iter(|| {
            let store = Store::open(db_dir.path().join("geode.duckdb")).unwrap();
            let n: i64 = store
                .writer()
                .query_row("select count(*) from measures_position_live", [], |r| {
                    r.get(0)
                })
                .unwrap();
            black_box(n)
        })
    });
    group.finish();
}

fn bench_single_file_load(c: &mut Criterion) {
    let ds = schema();
    let (src, emitted) = source_dir(200_000);
    let file = emitted
        .files
        .iter()
        .find(|f| f.sentinel_path.is_some())
        .unwrap();
    let sentinel = geode_data::source::parse_sentinel(
        &std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    let batch = {
        let stem = file.csv_path.file_stem().unwrap().to_string_lossy();
        stem.split('_').skip(2).collect::<Vec<_>>().join("_")
    };

    let mut group = c.benchmark_group("ingest_single_file");
    group.sample_size(10);
    group.throughput(criterion::Throughput::Elements(file.rows as u64));
    group.bench_function("load_one_file", |b| {
        b.iter_batched(
            || tempfile::tempdir().unwrap(),
            |db_dir| {
                let store = open_ready(db_dir.path(), &ds);
                black_box(
                    load_file(
                        &store,
                        &LoadRequest {
                            dataset: &ds,
                            dataset_name: "risk_snapshot",
                            csv_path: &file.csv_path,
                            sentinel: &sentinel,
                            batch: &batch,
                        },
                    )
                    .unwrap()
                    .rows,
                )
            },
            criterion::BatchSize::PerIteration,
        )
    });
    let _ = src;
    group.finish();
}

/// Spec §5.6's open question: does staging on separate connections beat
/// staging sequentially through the writer?
///
/// **File size is the variable that decides it**, so this measures both
/// regimes. DuckDB's CSV reader is itself multi-threaded, so it should
/// saturate the cores on a large file on its own and inter-file
/// parallelism should stop paying; on small files it cannot, and spreading
/// files across connections should win. The desk's real files run to
/// hundreds of thousands of rows, so the large case is the one that
/// governs — the small case is here to show where the crossover is.
fn bench_parse_parallelism(c: &mut Criterion) {
    let ds = schema();
    let mut group = c.benchmark_group("ingest_parallelism");
    group.sample_size(10);

    for total_rows in [400_000usize, 2_000_000] {
        let (src, emitted) = source_dir(total_rows);
        let files = emitted.files.len().max(1);
        let per_file = total_rows / files;
        let label = format!("{files}_files_of_{per_file}_rows");

        group.bench_function(format!("sequential/{label}"), |b| {
            b.iter_batched(
                || tempfile::tempdir().unwrap(),
                |db_dir| black_box(stage_all_sequential(db_dir.path(), src.path(), &ds)),
                criterion::BatchSize::PerIteration,
            )
        });
        group.bench_function(format!("parallel_4/{label}"), |b| {
            b.iter_batched(
                || tempfile::tempdir().unwrap(),
                |db_dir| black_box(stage_all_parallel(db_dir.path(), src.path(), &ds, 4)),
                criterion::BatchSize::PerIteration,
            )
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_cold_start,
    bench_warm_start,
    bench_single_file_load,
    bench_parse_parallelism
);
criterion_main!(benches);
