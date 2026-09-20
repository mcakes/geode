//! Spec §8.4: 500,000 points into 1,600 columns plus the path rebuild —
//! the two halves of what one cache MISS costs on the UI thread.
//!
//! A hit costs neither: both the decimation and the tessellation sit
//! behind gpui-component's `PathCache`, so a frame that changed nothing
//! pays for neither (`geode_chart::rebuilds`). This is the price of the
//! frame after a pan, a zoom or a delivery, at the widest shape the
//! spec names: the series cap's 500,000 points across a 1,600-pixel-wide
//! plot, with a NaN hole every 5,000 points so the polyline breaks.
//!
//! Medians are recorded in `docs/perf.md` under "Timeseries chart".

use criterion::{Criterion, criterion_group, criterion_main};
use geode_chart::core::Point;
use geode_chart::core::decimate::decimate;
use gpui::{PathBuilder, point, px};
use std::hint::black_box;

/// The element's own line build (`element.rs`'s `slot_path`), lifted so
/// the bench tessellates exactly what a frame does: one stroke path,
/// a fresh subpath after every break.
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
