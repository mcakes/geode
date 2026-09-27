//! Decimation and path rebuilding for 500,000 points across 1,600 columns.
//!
//! The fixture includes a NaN gap every 5,000 points. The first benchmark
//! isolates decimation; the second includes stroke tessellation. These are
//! parts of a path-cache miss after a pan, zoom or model change; scale and
//! tick derivation, coordinate conversion and painting are not timed here.
//!
//! Measurement context: `docs/current/performance.md`.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_chart::core::Point;
use geode_chart::core::decimate::decimate;
use gpui::{PathBuilder, point, px};
use std::hint::black_box;

/// Tessellate the decimator's output using the element's `polyline` stroke
/// convention: one path with a fresh subpath after every break.
fn build_path(pts: &[Point]) -> Option<gpui::Path<gpui::Pixels>> {
    let mut b = PathBuilder::stroke(px(1.5));
    let mut pen_up = true;
    for p in pts {
        if p.is_break() {
            pen_up = true;
            continue;
        }
        let q = point(px(p.x), px(p.y));
        if pen_up {
            b.move_to(q);
            pen_up = false;
        } else {
            b.line_to(q);
        }
    }
    b.build().ok()
}

fn bench(c: &mut Criterion) {
    let n = 500_000usize;
    let cols = 1_600usize;
    let xs: Vec<f32> = (0..n).map(|i| i as f32 * cols as f32 / n as f32).collect();
    let mut s = 0x9E3779B97F4A7C15u64;
    let mut v = 100.0f64;
    let ys: Vec<f64> = (0..n)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            v += ((s % 200) as f64 - 100.0) / 50.0;
            if i % 5_000 == 2_500 { f64::NAN } else { v }
        })
        .collect();
    let mut out = Vec::with_capacity(2 * cols + 200);
    c.bench_function("decimate/500k_into_1600", |b| {
        b.iter(|| {
            decimate(black_box(&xs), black_box(&ys), cols, &mut out);
            black_box(out.len())
        })
    });
    c.bench_function("decimate_and_path/500k_into_1600", |b| {
        b.iter(|| {
            decimate(black_box(&xs), black_box(&ys), cols, &mut out);
            black_box(build_path(&out).map(|p| p.vertices.len()))
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
