//! Measures seeded risk generation at 100,000 and 1,000,000 rows.
//! Each iteration allocates a fresh batch; ingestion and queries are excluded.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_demo_data::{GeneratorConfig, generate};
use std::hint::black_box;

fn bench_generate(c: &mut Criterion) {
    let mut group = c.benchmark_group("generate");
    group.sample_size(10);
    group.bench_function("100k_rows", |b| {
        b.iter(|| {
            black_box(generate(&GeneratorConfig {
                rows: 100_000,
                seed: 42,
                business_dates: 1,
            }))
        })
    });
    group.bench_function("1m_rows", |b| {
        b.iter(|| {
            black_box(generate(&GeneratorConfig {
                rows: 1_000_000,
                seed: 42,
                business_dates: 1,
            }))
        })
    });
    group.finish();
}

criterion_group!(benches, bench_generate);
criterion_main!(benches);
