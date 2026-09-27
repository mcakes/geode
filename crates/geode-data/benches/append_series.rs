//! Series append cost for 390 minute bars and 196,560 minute bars.
//!
//! The dataset uses `examples/demo-config/datasets.toml`, including its retention
//! window. Each iteration creates a temporary store and appends the span once
//! outside timing. The timed append writes new values at the same timestamps,
//! measuring duplicate checks, insertion, and retention in one transaction.

use chrono::{DateTime, Duration, Utc};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::schema::{DatasetSpec, SchemaSpec};
use geode_data::adapter::SeriesRows;
use geode_data::store::series::{SeriesAppendRequest, Span, append_series};
use geode_data::store::{Catalog, Store};

fn series_dataset() -> DatasetSpec {
    let text = include_str!("../../../examples/demo-config/datasets.toml");
    let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
    SchemaSpec::from_doc(&doc)
        .0
        .dataset("series")
        .unwrap()
        .clone()
}

/// `n` one-minute bars from a fixed instant. `first` shifts every value,
/// so the setup append and the timed one carry genuinely different
/// numbers over the same timestamps — every staged row survives the
/// dedupe, which is the expensive direction.
fn rows(n: usize, first: f64) -> SeriesRows {
    let start: DateTime<Utc> = "2024-01-02T14:30:00Z".parse().unwrap();
    SeriesRows {
        ts: (0..n)
            .map(|i| start + Duration::minutes(i as i64))
            .collect(),
        value: (0..n).map(|i| first + (i % 97) as f64 * 0.01).collect(),
    }
}

fn request<'a>(
    ds: &'a DatasetSpec,
    rows: &'a SeriesRows,
    span: Span,
    received_at: DateTime<Utc>,
) -> SeriesAppendRequest<'a> {
    SeriesAppendRequest {
        dataset: ds,
        source: "bench",
        identity: "SPX.close",
        rows,
        span,
        received_at,
    }
}

fn bench(c: &mut Criterion) {
    let ds = series_dataset();
    let mut g = c.benchmark_group("append_series");
    // 390 = one US cash session of minute bars; 196,560 = 504 sessions of
    // them, two years, the first load a chart would ask for.
    for n in [390usize, 196_560] {
        g.bench_function(n.to_string(), |b| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
                    store.apply_schema(&ds).unwrap();
                    Catalog::new(store.writer()).ensure_tables().unwrap();
                    let first = rows(n, 100.0);
                    let span = (
                        first.ts[0],
                        *first.ts.last().unwrap() + Duration::minutes(1),
                    );
                    let now = Utc::now();
                    append_series(&store, &request(&ds, &first, span, now)).unwrap();
                    // A LATER `received_at` for the timed append: the
                    // series table's primary key is (source, series_id,
                    // ts, received_at), so a second version of a bar is
                    // a second instant by construction — which is what a
                    // real refetch is. Computed here rather than in the
                    // timed half so no clock read is measured.
                    (dir, store, rows(n, 100.5), span, now + Duration::seconds(1))
                },
                |(dir, store, second, span, now)| {
                    // Returned, never dropped here, for the reason
                    // `publish_document.rs` records: `iter_batched` ends
                    // its measurement before dropping this closure's
                    // return value, so dropping the store and the temp
                    // directory inline would fold closing a DuckDB file
                    // and deleting a directory into "append cost".
                    let out = append_series(&store, &request(&ds, &second, span, now)).unwrap();
                    (dir, store, out.appended)
                },
                BatchSize::PerIteration,
            )
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
