//! Requery benchmarks against spec §7.1's central contract: **<50ms
//! end-to-end at 1M rows** for a regroup, refilter or scope change.
//!
//! Measures the shape the app actually runs — grouped three levels deep,
//! across two measure grains, scoped — not a bare `select`. Getting that
//! wrong is exactly the mistake phase 2a's first benchmark run made, and
//! it reported healthy numbers for a fixture a thousandth of the size.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;
use geode_core::scope::{DimensionSelection, Scope};
use geode_core::view::ViewSpec;
use geode_data::ingest::{LoadRequest, load_file};
use geode_data::query::AsOf;
use geode_data::service::{DataService, DataServiceConfig};
use geode_data::source::parse_sentinel;
use geode_data::store::{Catalog, Store};
use geode_demo_data::{EmitOptions, GeneratorConfig, emit_directory, generate};
use std::hint::black_box;
use std::time::Duration;

fn schema() -> SchemaSpec {
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
    SchemaSpec::from_doc(&doc).0
}

/// Views the benchmarks query. `tree` is the shape a blotter runs.
fn views() -> Vec<ViewSpec> {
    let text = r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu", "underlying_ref", "position_ref"]
[[tree.columns]]
name = "delta01"
kind = "measure"
[[tree.columns]]
name = "gamma01"
kind = "measure"
[[tree.columns]]
name = "vega01"
kind = "measure"
[[tree.columns]]
name = "npv"
kind = "measure"
[[tree.columns]]
name = "daily_trading_pnl"
kind = "measure"

[regrouped]
dataset = "risk_snapshot"
grouping = ["book", "model_code_placeholder"]
[[regrouped.columns]]
name = "delta01"
kind = "measure"

[shallow]
dataset = "risk_snapshot"
grouping = ["book"]
[[shallow.columns]]
name = "delta01"
kind = "measure"
[[shallow.columns]]
name = "npv"
kind = "measure"
[[shallow.columns]]
name = "daily_trading_pnl"
kind = "measure"
"#;
    let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
    ViewSpec::from_doc(&doc)
        .0
        .into_iter()
        // The regrouped view names a placeholder column deliberately left
        // undeclared; drop it rather than compile something invalid.
        .filter(|v| v.name != "regrouped")
        .collect()
}

/// Ingest `rows` rows into a fresh database and return a service over it.
fn service(rows: usize) -> (tempfile::TempDir, tempfile::TempDir, DataService, usize) {
    let db = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let schema = schema();
    let ds = schema.dataset("risk_snapshot").unwrap().clone();

    let store = Store::open(db.path().join("geode.duckdb")).unwrap();
    store.apply_schema(&ds).unwrap();
    Catalog::new(store.writer()).ensure_tables().unwrap();

    let batch = generate(&GeneratorConfig {
        rows,
        seed: 42,
        business_dates: 1,
    });
    let emitted = emit_directory(&batch, &EmitOptions::new(src.path())).unwrap();

    let mut loaded = 0;
    for file in emitted.files.iter().filter(|f| f.sentinel_path.is_some()) {
        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let sentinel = parse_sentinel(&text).unwrap();
        let stem = file.csv_path.file_stem().unwrap().to_string_lossy();
        let batch_id = stem.split('_').skip(2).collect::<Vec<_>>().join("_");
        if let Ok(out) = load_file(
            &store,
            &LoadRequest {
                dataset: &ds,
                dataset_name: "risk_snapshot",
                csv_path: &file.csv_path,
                sentinel: &sentinel,
                batch: &batch_id,
            },
        ) {
            loaded += out.rows;
        }
    }
    drop(store);

    let service = DataService::open(DataServiceConfig {
        db_path: db.path().join("geode.duckdb"),
        schema,
        views: views(),
        dimensions: DerivedDimensions::default(),
        query_workers: 4,
    })
    .unwrap();
    (db, src, service, loaded)
}

/// Submit and block until the snapshot arrives — the end-to-end path the
/// §7.1 budget is written against, minus the paint.
fn requery(svc: &DataService, view: &str, scope: &Scope, max_depth: usize) -> usize {
    svc.query(view, scope, AsOf::Live, max_depth).unwrap();
    let r = svc
        .query_results()
        .recv_timeout(Duration::from_secs(120))
        .expect("no result");
    r.snapshot.expect("query failed").rows()
}

fn book_scope() -> Scope {
    Scope {
        dimensions: vec![DimensionSelection {
            column: "book".into(),
            values: vec!["BK000".into(), "BK001".into(), "BK002".into()],
        }],
        ..Scope::default()
    }
}

fn bench_requery(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_requery");
    group.sample_size(20);

    for rows in [100_000usize, 1_000_000] {
        let (_db, _src, svc, loaded) = service(rows);
        assert!(loaded > 0, "fixture ingested nothing");

        // Result sizes, printed once per fixture: a latency number is
        // uninterpretable without knowing how many rows crossed the
        // boundary to produce it.
        eprintln!(
            "\n[{rows} rows ingested {loaded}] result rows — \
             tree/scoped/d1 {} · tree/scoped/d2 {} · \
             tree/scoped {} · shallow/scoped {} · tree/wide {} · tree/unscoped {}",
            requery(&svc, "tree", &book_scope(), 1),
            requery(&svc, "tree", &book_scope(), 2),
            requery(&svc, "tree", &book_scope(), usize::MAX),
            requery(&svc, "shallow", &book_scope(), usize::MAX),
            requery(
                &svc,
                "tree",
                &Scope {
                    dimensions: vec![DimensionSelection {
                        column: "book".into(),
                        values: (0..20).map(|i| format!("BK{i:03}")).collect(),
                    }],
                    ..Scope::default()
                },
                usize::MAX,
            ),
            requery(&svc, "tree", &Scope::default(), usize::MAX),
        );

        // The §7.1 contract: three levels, two measure grains, scoped.
        group.bench_function(format!("{rows}_rows_grouped_scoped"), |b| {
            b.iter(|| black_box(requery(&svc, "tree", &book_scope(), usize::MAX)))
        });

        // The same view bounded to what a collapsed tree actually shows:
        // one level open, so one more is materialized. This is the shape
        // the blotter opens with, and the one the §7.1 budget has to hold
        // for on every keystroke.
        group.bench_function(format!("{rows}_rows_grouped_scoped_depth_2"), |b| {
            b.iter(|| black_box(requery(&svc, "tree", &book_scope(), 2)))
        });

        // A regroup is a different grouping over the same data — what
        // Ctrl+1..9 does.
        group.bench_function(format!("{rows}_rows_regroup"), |b| {
            b.iter(|| black_box(requery(&svc, "shallow", &book_scope(), usize::MAX)))
        });

        // A rescope is the same grouping with a different selection.
        let wide = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: (0..20).map(|i| format!("BK{i:03}")).collect(),
            }],
            ..Scope::default()
        };
        group.bench_function(format!("{rows}_rows_rescope"), |b| {
            b.iter(|| black_box(requery(&svc, "tree", &wide, usize::MAX)))
        });

        // Unscoped, so the scope predicate is not doing the work.
        group.bench_function(format!("{rows}_rows_unscoped"), |b| {
            b.iter(|| black_box(requery(&svc, "tree", &Scope::default(), usize::MAX)))
        });

        // The unscoped tree is the one shape that misses the §7.1 budget
        // unbounded — it materializes every leaf. Bounded to what a
        // collapsed tree shows, the scan is unchanged but the result is
        // not, which is the whole claim depth bounding makes.
        group.bench_function(format!("{rows}_rows_unscoped_depth_2"), |b| {
            b.iter(|| black_box(requery(&svc, "tree", &Scope::default(), 2)))
        });

        svc.shutdown();
    }
    group.finish();
}

criterion_group!(benches, bench_requery);
criterion_main!(benches);
