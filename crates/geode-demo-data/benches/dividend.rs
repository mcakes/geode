//! `DividendGenerator::next_document`'s cost at the shape the design
//! spec's Task 10 brief describes: an index underlying's 30-40-row
//! schedule (SPX) and a regular name's 8-12-row one (a made-up ticker),
//! each timed both on its very first call (schedule creation) and on a
//! republish (walk + occasional promotion/append) — the two calls this
//! generator's callers actually make, in the mould of `generate.rs`.

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

    // Every fifth republish appends a row (Task 10 brief), so a
    // republish benchmark reusing one long-lived generator across
    // criterion's thousands of iterations would time an ever-growing
    // schedule rather than one steady-state republish. `iter_batched`'s
    // setup — a fresh generator, seeded and given its first document —
    // is excluded from the timing, so every measured call is a
    // single republish against the same ~35-row starting shape.
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
