//! The Log section's work over a full 4,096-record tail, in the three
//! parts the page pays separately:
//!
//! - `log_cache_sync`: formatting and lowering every record, which a
//!   rebuild pays only for records it has not seen;
//! - `narrowed_run`: one query's fuzzy narrowing with its marks, which the
//!   page runs off the UI thread when the query changes (and over only
//!   the new records when records arrive under an unchanged query);
//! - `log_table`: the table a rebuild builds on the UI thread from the
//!   cache and the held narrowing, the same for every query.
//!
//! Every query below keeps every record (the worst case for marks), except
//! `zzq`, which keeps none. GPUI and paint are excluded. Reference
//! measurements are in `docs/current/performance.md`.

use std::hint::black_box;
use std::time::{Duration, SystemTime};

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::clock::Clock;
use geode_core::log::{Level, Record};
use geode_diagnostics::log::{LOG_CAP, LogFilter};
use geode_diagnostics::log_cache::{LogCache, Narrowed, log_table};

const TARGETS: [&str; 4] = [
    "geode::ingest",
    "geode::query",
    "geode::shell",
    "geode::data",
];

const QUERIES: [&str; 6] = [
    "eutch ld",
    "partition loaded rows",
    "record partition loaded rows ms",
    "partition2026loaded",
    "ingest",
    "zzq",
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
            seq: i as u64 + 1,
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let records = tail();
    let clock = Clock::utc();
    c.bench_function("log_cache_sync/cold_4096", |b| {
        b.iter(|| {
            let mut cache = LogCache::default();
            cache.sync(&records, clock);
            black_box(cache.len())
        })
    });
    let mut cache = LogCache::default();
    cache.sync(&records, clock);
    let entries = cache.after(None);
    let mut group = c.benchmark_group("narrowed_run_4096");
    for query in QUERIES {
        group.bench_function(format!("query {query:?}"), |b| {
            b.iter(|| black_box(Narrowed::run(query, &entries)))
        });
    }
    group.finish();
    let mut group = c.benchmark_group("log_table_4096");
    group.bench_function("no query", |b| {
        b.iter(|| black_box(log_table(&cache, &LogFilter::all(), None, 0)))
    });
    for query in ["eutch ld", "record partition loaded rows ms"] {
        let narrowed = Narrowed::run(query, &entries);
        let mut filter = LogFilter::all();
        filter.text = query.to_string();
        group.bench_function(format!("held {query:?}"), |b| {
            b.iter(|| black_box(log_table(&cache, &filter, Some(&narrowed), 0)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
