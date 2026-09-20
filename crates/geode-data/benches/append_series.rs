//! `append_series`'s store-side cost (timeseries spec §4.4) at two
//! shapes: one day of minute bars (390 rows, the widening a chart asks
//! for when the day rolls) and two years of them (196,560 rows, a first
//! load).
//!
//! The series `DatasetSpec` is read from the real `examples/demo-config/
//! datasets.toml` (`include_str!`, the whole file — `SchemaSpec::from_doc`
//! tolerates its sibling declarations fine), the same way
//! `benches/publish_document.rs` reads the CVI one, so this bench cannot
//! quietly drift from the schema `--demo` actually runs on. That dataset
//! declares `retention = "7d"`, so the timed half also pays for the
//! per-pair retention sweep `append_series` runs inside its own
//! transaction (§4.7 as built) — which is the honest figure, because
//! there is no other place a sweep happens.
//!
//! Each criterion iteration gets its own temp store (`iter_batched`,
//! `PerIteration`), the shape `benches/ingest.rs` and
//! `benches/publish_document.rs` both use. The untimed setup half
//! appends the same span once already; the TIMED half appends a second
//! set of values over the same timestamps. That is deliberate: what a
//! fetch source does all day is offer a span that partly overlaps what
//! is stored, so the cost worth reporting is dedupe-against-live plus
//! the insert, not the one-off cost of the first insert into an empty
//! table.

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
