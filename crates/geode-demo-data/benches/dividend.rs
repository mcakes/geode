//! Measures initial dividend schedule creation and the first republish
//! for an index key (SPX) and a regular key (ACME). Creation includes
//! generator setup; republish setup is excluded. The measured republish
//! walks amounts but does not reach the promotion or append cadence.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use geode_demo_data::documents::dividend::DividendGenerator;
use std::hint::black_box;

fn today() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap()
}

fn bench_next_document(c: &mut Criterion) {
    let mut group = c.benchmark_group("dividend_generator");

    group.bench_function("first_document/index", |b| {
        b.iter(|| {
            let mut g = DividendGenerator::new(42, vec!["SPX".to_string()], today());
            black_box(g.next_document("SPX"))
        })
    });
    group.bench_function("first_document/regular", |b| {
        b.iter(|| {
            let mut g = DividendGenerator::new(42, vec!["ACME".to_string()], today());
            black_box(g.next_document("ACME"))
        })
    });

    // Reset outside the timed section so each iteration measures the
    // first republish of the same starting schedule. Reusing one generator
    // would grow the schedule on every fifth republish.
    group.bench_function("republish/index", |b| {
        b.iter_batched(
            || {
                let mut g = DividendGenerator::new(42, vec!["SPX".to_string()], today());
                g.next_document("SPX");
                g
            },
            |mut g| black_box(g.next_document("SPX")),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("republish/regular", |b| {
        b.iter_batched(
            || {
                let mut g = DividendGenerator::new(42, vec!["ACME".to_string()], today());
                g.next_document("ACME");
                g
            },
            |mut g| black_box(g.next_document("ACME")),
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(benches, bench_next_document);
criterion_main!(benches);
