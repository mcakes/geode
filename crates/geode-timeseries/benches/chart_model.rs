//! What one delivery costs the UI thread in `geode-timeseries`: building
//! the `ChartModel` from a `SeriesResult` at the series query's point cap
//! — 500,000 buckets, four slots (`chart::build` clones every value
//! vector once; the element then owns the model by `Arc`).

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::series::{SeriesResult, SlotProvenance, SlotResult};
use geode_timeseries::core::{Color, Model, chart};
use std::hint::black_box;

fn result(n: usize, slots: u8) -> SeriesResult {
    let prov = || SlotProvenance {
        loaded: None,
        latest_received_at: None,
        health: None,
    };
    SeriesResult {
        buckets: (0..n as i64).map(|i| i * 60_000_000).collect(),
        slots: (1..=slots)
            .map(|s| SlotResult {
                slot: s,
                values: (0..n).map(|i| (i as f64).sin()).collect(),
                percentiles: vec![(0.05, -0.9), (0.5, 0.0), (0.95, 0.9)],
                bins: (0..40).map(|b| (b as f64, b as f64 + 1.0, 10)).collect(),
                provenance: prov(),
            })
            .collect(),
    }
}

fn bench(c: &mut Criterion) {
    let mut model = Model::new();
    for id in ["A", "B", "C", "D"] {
        model.add_source(id, "demo_kdb", "series").unwrap();
    }
    let r = result(500_000, 4);
    let colour = |_: &Color| gpui::black();
    c.bench_function("chart_model/500k_x_4", |b| {
        b.iter(|| black_box(chart::build(&r, &model, 1, 0, &colour, Some("demo_kdb"))))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
