//! The demo Hanweck adapter's call shapes, for spec §11's question of
//! whether a member prefix should split into several calls: one prefix
//! `Latest` over the ten demo underlyings against ten single-member calls,
//! a 400-day prefix `Between` (the largest answer a board can ask), and an
//! exact `At` over the ten surface points per underlying the Changes view
//! reads.

use chrono::{Duration, Utc};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_compose::demo_hanweck::{DemoHanweck, UNDERLYINGS};
use geode_core::query::{KeySet, Span};
use geode_data::adapter::{Adapter, HistoryRequest};
use std::hint::black_box;

fn members() -> Vec<Vec<String>> {
    UNDERLYINGS.iter().map(|u| vec![u.to_string()]).collect()
}

fn bench(c: &mut Criterion) {
    let adapter = DemoHanweck::new(42);
    let mut history = adapter
        .history()
        .expect("the demo adapter has a history side");
    let now = Utc::now();
    let ask = |keys: KeySet, span: Span| HistoryRequest {
        table: "surface".into(),
        keys,
        span,
    };

    let prefix = ask(KeySet::Prefix(members()), Span::Latest);
    c.bench_function("demo_hanweck/prefix_latest_10", |b| {
        b.iter(|| black_box(history.query(&prefix).unwrap().len()))
    });
    let singles: Vec<HistoryRequest> = members()
        .into_iter()
        .map(|m| ask(KeySet::Prefix(vec![m]), Span::Latest))
        .collect();
    c.bench_function("demo_hanweck/single_latest_x10", |b| {
        b.iter(|| {
            black_box(
                singles
                    .iter()
                    .map(|r| history.query(r).unwrap().len())
                    .sum::<usize>(),
            )
        })
    });
    let year = ask(
        KeySet::Prefix(members()),
        Span::Between {
            from: now - Duration::days(400),
            to: now,
            step: None,
        },
    );
    c.bench_function("demo_hanweck/prefix_between_400d_10", |b| {
        b.iter(|| black_box(history.query(&year).unwrap().len()))
    });
    let points: Vec<Vec<String>> = UNDERLYINGS
        .iter()
        .flat_map(|u| {
            let mut v: Vec<Vec<String>> = ["1M", "2M", "3M", "6M", "1Y", "18M", "2Y", "3Y"]
                .iter()
                .map(|t| vec![u.to_string(), t.to_string(), "0.5".to_string()])
                .collect();
            v.push(vec![u.to_string(), "3M".to_string(), "0.25".to_string()]);
            v.push(vec![u.to_string(), "3M".to_string(), "0.75".to_string()]);
            v
        })
        .collect();
    let at = ask(KeySet::Exact(points), Span::At(now - Duration::days(1)));
    c.bench_function("demo_hanweck/exact_at_100", |b| {
        b.iter(|| black_box(history.query(&at).unwrap().len()))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
