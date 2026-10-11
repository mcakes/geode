//! Sparkline layout costs, the window-free half of a grid of sparklines:
//! a 30-day series into a 72 px cell, a one-year series into a 140 px band
//! cell, and a 25-row column with a shared range, as the vol watchlist
//! paints them.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_chart::core::Rect;
use geode_chart::core::spark::{SparkScratch, layout, shared_range};
use std::hint::black_box;

fn series(n: usize, seed: f64) -> Vec<f64> {
    (0..n)
        .map(|i| 0.18 + 0.02 * ((i as f64) * 0.21 + seed).sin())
        .collect()
}

fn bench(c: &mut Criterion) {
    let month = series(30, 0.0);
    let year = series(252, 0.0);
    let mut s = SparkScratch::default();
    c.bench_function("spark/layout_30_into_72", |b| {
        b.iter(|| {
            black_box(layout(
                &month,
                None,
                None,
                Rect::new(0.0, 0.0, 72.0, 16.0),
                3.0,
                &mut s,
            ))
        })
    });
    c.bench_function("spark/layout_252_into_140", |b| {
        b.iter(|| {
            black_box(layout(
                &year,
                None,
                None,
                Rect::new(0.0, 0.0, 140.0, 16.0),
                3.0,
                &mut s,
            ))
        })
    });
    let rows: Vec<Vec<f64>> = (0..25).map(|r| series(30, r as f64)).collect();
    c.bench_function("spark/column_25x30_shared_range", |b| {
        b.iter(|| {
            let range = shared_range(rows.iter().map(Vec::as_slice));
            for (i, r) in rows.iter().enumerate() {
                black_box(layout(
                    r,
                    range,
                    Some(2),
                    Rect::new(0.0, 21.0 * i as f32, 72.0, 16.0),
                    3.0,
                    &mut s,
                ));
            }
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
