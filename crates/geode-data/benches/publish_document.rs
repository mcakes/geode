//! Document staging and publication for 600-row and 60,000-row CVI grids.
//! The `geode-documents` CVI benchmark separately measures parsing and writing.
//!
//! The dataset schema comes from `examples/demo-config/datasets.toml`. Each
//! iteration gets a fresh temporary store and an untimed initial publication.
//! The timed publication replaces the same key at a later source time, including
//! archiving the outgoing document without accumulating history across samples.

use chrono::{DateTime, Utc};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::schema::{DatasetSpec, SchemaSpec};
use geode_data::store::document::{DocumentPublishRequest, publish_document};
use geode_data::store::{Catalog, Store};

fn cvi_dataset() -> DatasetSpec {
    let text = include_str!("../../../examples/demo-config/datasets.toml");
    let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
    SchemaSpec::from_doc(&doc)
        .0
        .dataset("cvi_params")
        .unwrap()
        .clone()
}

/// The same grid shape `geode-documents`'s `cvi` bench builds — see that
/// file's own doc for why term-major, and for why these two sizes.
/// `offset` shifts every `param` value so the setup publish and the timed
/// publish carry genuinely different rows, the way two real publishes of
/// the same key always would.
fn grid(terms: usize, nodes: usize, offset: f64) -> DocumentRows {
    let base = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let node_values: Vec<f64> = (0..nodes).map(|i| -30.0 + i as f64 * 0.37).collect();
    let mut term_col = Vec::with_capacity(terms * nodes);
    let mut node_col = Vec::with_capacity(terms * nodes);
    let mut params = Vec::with_capacity(terms * nodes);
    for t in 0..terms {
        let term = base + chrono::Days::new(t as u64 * 7);
        for (i, n) in node_values.iter().enumerate() {
            term_col.push(term);
            node_col.push(*n);
            params.push((t * nodes + i) as f64 * 0.001 - 1.0 + offset);
        }
    }
    DocumentRows {
        key: vec!["SPX.Z".into()],
        attributes: vec![
            ("anchor_date".into(), Value::Date(base)),
            ("spot_ref".into(), Value::F64(7650.0)),
        ],
        axes: vec![
            ("term".into(), Column::Date(term_col)),
            ("node".into(), Column::F64(node_col)),
        ],
        values: vec![("param".into(), Column::F64(params))],
    }
}

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn bench(c: &mut Criterion) {
    let ds = cvi_dataset();
    let mut g = c.benchmark_group("publish_document");
    for (terms, nodes) in [(20usize, 30usize), (200, 300)] {
        let first = grid(terms, nodes, 0.0);
        let second = grid(terms, nodes, 0.5);
        g.bench_function(format!("{terms}x{nodes}"), |b| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
                    store.apply_schema(&ds).unwrap();
                    Catalog::new(store.writer()).ensure_tables().unwrap();
                    publish_document(
                        &store,
                        &DocumentPublishRequest {
                            dataset: &ds,
                            source: "cvi",
                            rows: &first,
                            source_time: ts("2026-01-01T00:00:00Z"),
                            received_at: ts("2026-01-01T00:00:00Z"),
                            bytes: 1234,
                        },
                    )
                    .unwrap();
                    (dir, store)
                },
                |(dir, store)| {
                    // Returned, not dropped here: `Bencher::iter_batched`
                    // times only up to this closure's return value being
                    // produced (`criterion`'s own `measurement.end` runs
                    // before its `drop(black_box(output))`), so `dir` and
                    // `store` must move OUT rather than be dropped inline
                    // — an inline drop would fold the temp file's close
                    // and the directory's deletion into "publish cost".
                    let published = publish_document(
                        &store,
                        &DocumentPublishRequest {
                            dataset: &ds,
                            source: "cvi",
                            rows: &second,
                            source_time: ts("2026-01-01T00:00:05Z"),
                            received_at: ts("2026-01-01T00:00:05Z"),
                            bytes: 1234,
                        },
                    )
                    .unwrap();
                    (dir, store, published.rows)
                },
                criterion::BatchSize::PerIteration,
            )
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
