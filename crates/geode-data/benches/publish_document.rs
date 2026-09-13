//! `publish_document`'s store-side cost (spec §5.4 step 3) at the same two
//! CVI grid shapes `geode-documents`'s own `cvi` bench uses — 20 terms ×
//! 30 nodes (600 rows, a single underlying's surface, the shape a panel
//! shows) and 200 × 300 (60,000 rows, well past anything the desk sends).
//! Read the two benches side by side: `cvi` measures parse + write (the
//! receiver thread's cost, before a document ever reaches the runner),
//! this one measures stage + publish (the runner's cost, after).
//!
//! The CVI `DatasetSpec` is read from the real `examples/demo-config/
//! datasets.toml` (`include_str!`, the whole file — `SchemaSpec::from_doc`
//! tolerates the sibling `risk_snapshot` declaration in it fine) rather
//! than a hand-typed copy, so this bench can never quietly drift from the
//! schema `--demo` actually runs on. `crate::store::ddl::tests_support::
//! cvi_dataset` is the crate's own such fixture, but a bench cannot see a
//! `#[cfg(test)]` item, hence this file's own copy of the *building*, not
//! the schema text.
//!
//! Each criterion iteration gets its own temp store (`iter_batched`,
//! `PerIteration`) — the same per-iteration-store shape `benches/
//! ingest.rs` uses — so a hundred-odd publishes never share one growing
//! archive that would skew later samples. The untimed setup half of each
//! iteration publishes the document once already; the TIMED half
//! publishes it again over the same key with a later `source_time`. That
//! is deliberate: the demo bus's steady state is exactly this — every
//! key republishes on its own cadence — so what this bench reports is
//! the cost of a live document overwriting a live document, not the
//! one-off, less interesting cost of the first insert into an empty
//! table.

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
