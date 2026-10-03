//! The Log section's prepared-table build over a full 4,096-record tail:
//! the level and target gates, time formatting, the fuzzy text filter with
//! its per-column marks, and the prepared rows. This is the work one
//! keystroke in the filter, or one batch of new records, costs on the UI
//! thread; GPUI and paint are excluded. Performance guidance and reference
//! measurements are in `docs/current/performance.md`.

use std::hint::black_box;
use std::time::{Duration, SystemTime};

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::clock::Clock;
use geode_core::log::{Level, Record};
use geode_diagnostics::log::{LOG_CAP, LogFilter};
use geode_diagnostics::{model, prepared};

const TARGETS: [&str; 4] = [
    "geode::ingest",
    "geode::query",
    "geode::shell",
    "geode::data",
];

fn tail() -> Vec<Record> {
    (0..LOG_CAP)
        .map(|i| Record {
            at: SystemTime::UNIX_EPOCH + Duration::from_millis(1_790_000_000_000 + i as u64 * 37),
            level: match i % 5 {
                0 => Level::ERROR,
                1 => Level::WARN,
                2 => Level::DEBUG,
                _ => Level::INFO,
            },
            target: TARGETS[i % TARGETS.len()],
            message: format!(
                "record {i}: partition 2026-09-27 · EU_TECH loaded {} rows in {} ms",
                i * 13,
                i % 97
            ),
            seq: i as u64,
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let records = tail();
    let clock = Clock::utc();
    let mut group = c.benchmark_group("log_table_4096");
    // Empty: no narrowing. `ingest`: one word, a quarter of rows kept.
    // `eutch ld`: two words in the message of every row (the worst case
    // for marks). `zzq`: one word no row holds (the reject path).
    for query in ["", "ingest", "eutch ld", "zzq"] {
        let mut filter = LogFilter::all();
        filter.text = query.to_string();
        group.bench_function(format!("query {query:?}"), |b| {
            b.iter(|| {
                let rows = model::log_rows(records.iter(), &filter, clock);
                black_box(prepared::log_table(&rows, 0))
            })
        });
    }
    group.finish();
}

/// The narrowing alone, over pre-formatted column text: what the fuzzy
/// filter adds to the build above, separated from formatting and rows.
fn narrow_only(c: &mut Criterion) {
    let columns: Vec<[String; 4]> = tail()
        .iter()
        .map(|r| {
            [
                "09:00:00.000".to_string(),
                r.level.as_str().to_string(),
                r.target.to_string(),
                r.message.clone(),
            ]
        })
        .collect();
    let mut group = c.benchmark_group("narrow_4096");
    for query in ["ingest", "eutch ld"] {
        group.bench_function(format!("query {query:?}"), |b| {
            b.iter(|| {
                let mut narrow = geode_shell::listfilter::Narrow::new(query);
                columns
                    .iter()
                    .filter_map(|c| narrow.row(&[&c[0], &c[1], &c[2], &c[3]]))
                    .count()
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench, narrow_only);
criterion_main!(benches);
