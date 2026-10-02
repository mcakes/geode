//! Service requery latency against the 50 ms budget at one million rows.
//!
//! Fixtures exercise grouped views across two measure grains, book scopes,
//! text filters, bounded expansion, and historical reads. Timing includes
//! submission through DataService and waiting for the result, but excludes UI
//! painting. See `docs/current/performance.md` for measured reference values.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::QueryKey;
use geode_core::schema::SchemaSpec;
use geode_core::scope::{DimensionSelection, Scope};
use geode_core::view::ViewSpec;
use geode_data::QueryParams;
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
textual = true
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
source_name = "LHU"
textual = true
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
source_name = "PositionRef"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
source_name = "Counterparty"
textual = true
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
source_name = "InstrumentRef"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
source_name = "Underlying1Ref"
textual = true
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

/// `schema()` plus the demo's instrument-carried `strike` and `expiry`,
/// which the `tree_carried` view shows ungrouped under the unanimity rule.
fn schema_with_carried() -> SchemaSpec {
    let mut s = schema();
    let text = r#"
[risk_snapshot.columns.strike]
type = "f64"
role = "dimension"
grain = "instrument"
source_name = "Strike"
[risk_snapshot.columns.expiry]
type = "utf8"
role = "dimension"
grain = "instrument"
textual = true
source_name = "Expiry"
"#;
    let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
    let extra = SchemaSpec::from_doc(&doc).0;
    let columns = extra.datasets.into_iter().next().unwrap().columns;
    s.datasets
        .first_mut()
        .expect("risk_snapshot declared")
        .columns
        .extend(columns);
    s
}

/// Mark plain-string key columns as textual as well. Unlike categorical
/// ENUMs, these columns require row scans for text filtering, exposing the
/// cost that dictionary lookup cannot remove.
fn schema_with_textual_keys() -> SchemaSpec {
    let mut s = schema();
    let ds = s.datasets.first_mut().expect("risk_snapshot declared");
    for c in ds.columns.iter_mut() {
        if matches!(
            c.name.as_str(),
            "business_date" | "position_ref" | "instrument_ref"
        ) {
            c.textual = true;
        }
    }
    s
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

# `tree` plus two ungrouped dimensions, read under the unanimity rule.
# Only served by `schema_with_carried`; under `schema()` it is refused.
[tree_carried]
dataset = "risk_snapshot"
grouping = ["lhu", "underlying_ref", "position_ref"]
[[tree_carried.columns]]
name = "delta01"
kind = "measure"
[[tree_carried.columns]]
name = "gamma01"
kind = "measure"
[[tree_carried.columns]]
name = "vega01"
kind = "measure"
[[tree_carried.columns]]
name = "npv"
kind = "measure"
[[tree_carried.columns]]
name = "daily_trading_pnl"
kind = "measure"
[[tree_carried.columns]]
name = "strike"
kind = "dimension"
[[tree_carried.columns]]
name = "expiry"
kind = "dimension"

[regrouped]
dataset = "risk_snapshot"
grouping = ["book", "model_code_placeholder"]
[[regrouped.columns]]
name = "delta01"
kind = "measure"

# Underlying-grain measures only, grouped by neither context key, so no
# context column is already grouped or shown: the `query_context` shape.
[underlying_only]
dataset = "risk_snapshot"
grouping = ["book", "lhu"]
[[underlying_only.columns]]
name = "delta01"
kind = "measure"
[[underlying_only.columns]]
name = "gamma01"
kind = "measure"
[[underlying_only.columns]]
name = "vega01"
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
fn service(
    rows: usize,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    DataService,
    std::sync::mpsc::Receiver<geode_data::DataEvent>,
    usize,
) {
    service_with(rows, schema())
}

/// [`service`] under a caller-chosen schema.
fn service_with(
    rows: usize,
    schema: SchemaSpec,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    DataService,
    std::sync::mpsc::Receiver<geode_data::DataEvent>,
    usize,
) {
    let db = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
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

    let (service, rx) = DataService::open_channel(DataServiceConfig {
        db_path: db.path().join("geode.duckdb"),
        schema,
        views: views(),
        dimensions: DerivedDimensions::default(),
        query_workers: 4,
        sources: Vec::new(),
        adapters: Default::default(),
        documents: Default::default(),
        egress: Vec::new(),
        pricer: Default::default(),
        vol: Default::default(),
        positions: None,
    })
    .unwrap();
    (db, src, service, rx, loaded)
}

/// A second service over the same database path, under a different
/// schema. No ingest — `apply_schema` (which `DataService::open` runs for
/// every declared dataset) is idempotent over tables that already exist,
/// so this just lets the bench measure the same data under a schema that
/// declares more (or fewer) textual columns.
fn reopen(
    db: &tempfile::TempDir,
    _src: &tempfile::TempDir,
    schema: SchemaSpec,
) -> (
    DataService,
    std::sync::mpsc::Receiver<geode_data::DataEvent>,
) {
    DataService::open_channel(DataServiceConfig {
        db_path: db.path().join("geode.duckdb"),
        schema,
        views: views(),
        dimensions: DerivedDimensions::default(),
        query_workers: 4,
        sources: Vec::new(),
        adapters: Default::default(),
        documents: Default::default(),
        egress: Vec::new(),
        pricer: Default::default(),
        vol: Default::default(),
        positions: None,
    })
    .unwrap()
}

/// Build live and archived generations by ingesting the same source twice,
/// with the second set of sentinel times one hour later. Return the service
/// and an instant after all first-generation times but before the second set,
/// so historical benchmarks read populated archive tables.
fn service_with_history(
    rows: usize,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    DataService,
    std::sync::mpsc::Receiver<geode_data::DataEvent>,
    usize,
    chrono::DateTime<chrono::Utc>,
) {
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
    let ready: Vec<_> = emitted
        .files
        .iter()
        .filter(|f| f.sentinel_path.is_some())
        .collect();

    let mut loaded = 0;
    let mut latest_first_gen = chrono::DateTime::<chrono::Utc>::MIN_UTC;
    for file in &ready {
        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let sentinel = parse_sentinel(&text).unwrap();
        latest_first_gen = latest_first_gen.max(sentinel.as_of);
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
    let between = latest_first_gen + chrono::Duration::minutes(30);

    // A corrected republish of the same content: same rows, an hour
    // later, so live is replaced and the first generation files to the
    // archive (the pattern `load.rs`'s tests use for a second publish).
    for file in &ready {
        let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
        let mut sentinel = parse_sentinel(&text).unwrap();
        sentinel.as_of += chrono::Duration::hours(1);
        let stem = file.csv_path.file_stem().unwrap().to_string_lossy();
        let batch_id = stem.split('_').skip(2).collect::<Vec<_>>().join("_");
        load_file(
            &store,
            &LoadRequest {
                dataset: &ds,
                dataset_name: "risk_snapshot",
                csv_path: &file.csv_path,
                sentinel: &sentinel,
                batch: &batch_id,
            },
        )
        .unwrap();
    }
    drop(store);

    let (service, rx) = DataService::open_channel(DataServiceConfig {
        db_path: db.path().join("geode.duckdb"),
        schema,
        views: views(),
        dimensions: DerivedDimensions::default(),
        query_workers: 4,
        sources: Vec::new(),
        adapters: Default::default(),
        documents: Default::default(),
        egress: Vec::new(),
        pricer: Default::default(),
        vol: Default::default(),
        positions: None,
    })
    .unwrap();
    (db, src, service, rx, loaded, between)
}

/// Measure request submission through snapshot receipt, excluding UI painting.
fn requery(
    svc: &DataService,
    rx: &std::sync::mpsc::Receiver<geode_data::DataEvent>,
    view: &str,
    scope: &Scope,
    max_depth: usize,
) -> usize {
    requery_at(svc, rx, view, scope, max_depth, AsOf::Live)
}

/// `requery`, under a given era.
fn requery_at(
    svc: &DataService,
    rx: &std::sync::mpsc::Receiver<geode_data::DataEvent>,
    view: &str,
    scope: &Scope,
    max_depth: usize,
    as_of: AsOf,
) -> usize {
    svc.query(&QueryParams {
        key: QueryKey(1),
        tag: 0,
        submitted: std::time::Instant::now(),
        view: view.to_string(),
        grouping: None,
        scope: scope.clone(),
        as_of,
        max_depth,
    })
    .unwrap();
    loop {
        match rx
            .recv_timeout(Duration::from_secs(120))
            .expect("no result")
        {
            geode_data::DataEvent::Query(o) => return o.snapshot.expect("query failed").rows(),
            _ => continue,
        }
    }
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
        let (_db, _src, svc, rx, loaded) = service(rows);
        assert!(loaded > 0, "fixture ingested nothing");

        // Result sizes, printed once per fixture: a latency number is
        // uninterpretable without knowing how many rows crossed the
        // boundary to produce it.
        eprintln!(
            "\n[{rows} rows ingested {loaded}] result rows — \
             tree/scoped/d1 {} · tree/scoped/d2 {} · \
             tree/scoped {} · shallow/scoped {} · tree/wide {} · tree/unscoped {}",
            requery(&svc, &rx, "tree", &book_scope(), 1),
            requery(&svc, &rx, "tree", &book_scope(), 2),
            requery(&svc, &rx, "tree", &book_scope(), usize::MAX),
            requery(&svc, &rx, "shallow", &book_scope(), usize::MAX),
            requery(
                &svc,
                &rx,
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
            requery(&svc, &rx, "tree", &Scope::default(), usize::MAX),
        );

        // Three grouping levels across two measure grains, with a book scope.
        group.bench_function(format!("{rows}_rows_grouped_scoped"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, "tree", &book_scope(), usize::MAX)))
        });

        // Bound expansion to one open level plus its children, matching the
        // initial collapsed-tree query shape.
        group.bench_function(format!("{rows}_rows_grouped_scoped_depth_2"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, "tree", &book_scope(), 2)))
        });

        // A regroup is a different grouping over the same data — what
        // Ctrl+1..9 does.
        group.bench_function(format!("{rows}_rows_regroup"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, "shallow", &book_scope(), usize::MAX)))
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
            b.iter(|| black_box(requery(&svc, &rx, "tree", &wide, usize::MAX)))
        });

        // Unscoped, so the scope predicate is not doing the work.
        group.bench_function(format!("{rows}_rows_unscoped"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, "tree", &Scope::default(), usize::MAX)))
        });

        // Compare depth-bounded unscoped output with full leaf materialization.
        // The source scan is unchanged; fewer result rows cross the query boundary.
        group.bench_function(format!("{rows}_rows_unscoped_depth_2"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, "tree", &Scope::default(), 2)))
        });

        // Text filters with broad, narrow, and zero matches, each tested
        // unscoped, depth-bounded, and with a book scope.
        for (label, needle) in [("broad", "bk00"), ("narrow", "bk007"), ("none", "zzz")] {
            let text_only = Scope {
                text: Some(needle.to_string()),
                ..Scope::default()
            };
            let text_and_books = Scope {
                text: Some(needle.to_string()),
                ..book_scope()
            };
            group.bench_function(format!("{rows}_rows_text_{label}_unscoped"), |b| {
                b.iter(|| black_box(requery(&svc, &rx, "tree", &text_only, usize::MAX)))
            });
            group.bench_function(format!("{rows}_rows_text_{label}_depth_2"), |b| {
                b.iter(|| black_box(requery(&svc, &rx, "tree", &text_only, 2)))
            });
            group.bench_function(format!("{rows}_rows_text_{label}_with_books"), |b| {
                b.iter(|| black_box(requery(&svc, &rx, "tree", &text_and_books, usize::MAX)))
            });
        }
        svc.shutdown();

        // The same needle with the plain-string key columns textual too:
        // the residual row scan the dictionary rewrite cannot remove.
        let (svc, rx) = reopen(&_db, &_src, schema_with_textual_keys());
        {
            let (label, needle) = ("none", "zzz");
            let text_only = Scope {
                text: Some(needle.to_string()),
                ..Scope::default()
            };
            group.bench_function(
                format!("{rows}_rows_text_{label}_keys_textual_depth_2"),
                |b| b.iter(|| black_box(requery(&svc, &rx, "tree", &text_only, 2))),
            );
        }
        svc.shutdown();

        // Historical zero-match query at depth two. The fixture has two
        // published generations per partition, so the live/archive union
        // contains real historical data.
        let (_db3, _src3, svc, rx, loaded3, between) = service_with_history(rows);
        assert!(loaded3 > 0, "history fixture ingested nothing");
        {
            let (label, needle) = ("none", "zzz");
            let text_only = Scope {
                text: Some(needle.to_string()),
                ..Scope::default()
            };
            group.bench_function(format!("{rows}_rows_text_{label}_depth_2_asof"), |b| {
                b.iter(|| {
                    black_box(requery_at(
                        &svc,
                        &rx,
                        "tree",
                        &text_only,
                        2,
                        AsOf::At(between),
                    ))
                })
            });
        }
        // The plain scoped depth-2 shape — no text filter at all — under
        // the same as-of instant. This isolates the generation
        // predicate's own cost from the text filter's dictionary rewrite,
        // which the `_text_*_asof` cases above already cover.
        group.bench_function(format!("{rows}_rows_scoped_depth_2_asof"), |b| {
            b.iter(|| {
                black_box(requery_at(
                    &svc,
                    &rx,
                    "tree",
                    &book_scope(),
                    2,
                    AsOf::At(between),
                ))
            })
        });
        svc.shutdown();
    }
    group.finish();
}

/// Time `resolve_generations` against the generation summary catalog. Setup
/// publishes a 20,000-row fixture 50 times, advancing source time one hour per
/// pass. Resolution runs at an instant after every load; the small row count
/// keeps this focused on generation count rather than payload volume.
fn bench_resolve(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolve");
    group.sample_size(20);
    let rows = 20_000usize;
    let generations = 50usize;

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
    let ready: Vec<_> = emitted
        .files
        .iter()
        .filter(|f| f.sentinel_path.is_some())
        .collect();

    let mut at = chrono::DateTime::<chrono::Utc>::MIN_UTC;
    for pass in 0..generations {
        for file in &ready {
            let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
            let mut sentinel = parse_sentinel(&text).unwrap();
            sentinel.as_of += chrono::Duration::hours(pass as i64);
            let stem = file.csv_path.file_stem().unwrap().to_string_lossy();
            let batch_id = stem.split('_').skip(2).collect::<Vec<_>>().join("_");
            load_file(
                &store,
                &LoadRequest {
                    dataset: &ds,
                    dataset_name: "risk_snapshot",
                    csv_path: &file.csv_path,
                    sentinel: &sentinel,
                    batch: &batch_id,
                },
            )
            .unwrap();
            at = at.max(sentinel.as_of);
        }
    }

    group.bench_function("resolve_generations_50_generations", |b| {
        b.iter(|| {
            black_box(
                geode_data::query::resolve_generations(store.writer(), "risk_snapshot", at)
                    .unwrap(),
            )
        })
    });
    group.finish();
}

/// Ungrouped dimensions under the unanimity rule: the `tree` view against
/// the same view plus `strike` and `expiry`, over one million ingested rows
/// with both columns stored. The difference is what the two columns cost:
/// their aggregates ride the underlying-grain measure scan, and the result
/// gains a numeric strike, a text expiry, and their two flags.
fn bench_carried(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_carried");
    group.sample_size(20);
    let rows = 1_000_000usize;
    let (_db, _src, svc, rx, loaded) = service_with(rows, schema_with_carried());
    assert!(loaded > 0, "fixture ingested nothing");
    eprintln!(
        "\n[{rows} rows ingested {loaded}] result rows — tree_carried/scoped {} · \
         tree_carried/scoped/d2 {} · tree_carried/unscoped {}",
        requery(&svc, &rx, "tree_carried", &book_scope(), usize::MAX),
        requery(&svc, &rx, "tree_carried", &book_scope(), 2),
        requery(&svc, &rx, "tree_carried", &Scope::default(), usize::MAX),
    );
    for view in ["tree", "tree_carried"] {
        group.bench_function(format!("{rows}_rows_{view}_scoped"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, view, &book_scope(), usize::MAX)))
        });
        group.bench_function(format!("{rows}_rows_{view}_scoped_depth_2"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, view, &book_scope(), 2)))
        });
        group.bench_function(format!("{rows}_rows_{view}_unscoped_depth_2"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, view, &Scope::default(), 2)))
        });
        group.bench_function(format!("{rows}_rows_{view}_unscoped"), |b| {
            b.iter(|| black_box(requery(&svc, &rx, view, &Scope::default(), usize::MAX)))
        });
    }
    group.finish();
}

/// The roster's context columns (`ModuleRoster::context_columns`, which
/// production hands to every query): a view of underlying-grain measures
/// only, grouped by neither key, at one million rows, with the startup list
/// set and with none. Each column is read at the coarsest grain that
/// carries it: `underlying_ref` rides the underlying-grain measure scan,
/// while no shown measure reads the position or instrument grain, so
/// `position_ref` and `instrument_ref` each cost a `dim_` CTE (one more
/// table scan and join) of their own: the worst case for these three.
fn bench_context(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_context");
    group.sample_size(20);
    let rows = 1_000_000usize;
    let (_db, _src, svc, rx, loaded) = service(rows);
    assert!(loaded > 0, "fixture ingested nothing");
    let startup: Vec<String> = ["underlying_ref", "position_ref", "instrument_ref"]
        .iter()
        .map(|c| c.to_string())
        .collect();
    for (label, columns) in [("no_context", Vec::new()), ("with_context", startup)] {
        svc.set_context_columns(columns);
        eprintln!(
            "\n[{rows} rows ingested {loaded}] {label} result rows — underlying_only/unscoped/d2 {}",
            requery(&svc, &rx, "underlying_only", &Scope::default(), 2),
        );
        group.bench_function(
            format!("{rows}_rows_underlying_only_depth_2_{label}"),
            |b| b.iter(|| black_box(requery(&svc, &rx, "underlying_only", &Scope::default(), 2))),
        );
    }
    svc.set_context_columns(Vec::new());
    group.finish();
}

criterion_group!(
    benches,
    bench_requery,
    bench_resolve,
    bench_carried,
    bench_context
);
criterion_main!(benches);
