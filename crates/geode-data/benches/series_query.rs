//! Series query round trips over one million stored rows: four identities,
//! each with 250,000 one-minute bars. Cases request daily output over a year
//! for one slot or four slots plus a ratio, and minute output over a month with
//! percentiles and bins, with a rolling expression, and with `ema`.
//!
//! Setup appends the data in day-sized chunks outside timing. Each timed call
//! submits through DataService and waits for its Series event, including
//! compilation, queueing, and DuckDB execution but excluding UI painting.
//!
//! The fixture declares a minimal series dataset without retention or history
//! windows, keeping every seeded row available throughout the benchmark.

use chrono::{DateTime, Duration, Utc};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{AsOf, QueryKey};
use geode_core::schema::SchemaSpec;
use geode_core::series::expr::{Ast, Op};
use geode_core::series::{BucketRule, Frequency, SeriesParams, SeriesSpec, SlotKind};
use geode_data::adapter::SeriesRows;
use geode_data::pricing::PricerConfig;
use geode_data::service::{DataEvent, DataService, DataServiceConfig};
use geode_data::store::series::{SeriesAppendRequest, append_series};
use geode_data::store::{Catalog, Store};
use std::hint::black_box;
use std::sync::mpsc::Receiver;

const IDENTITIES: [&str; 4] = ["A", "B", "C", "D"];
const SESSIONS: i64 = 250;
const BARS: i64 = 1_000;

fn schema() -> SchemaSpec {
    let text = "[series]\nfamily = \"series\"\n";
    let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
    SchemaSpec::from_doc(&doc).0
}

/// A million rows in a temporary store, then a service over it. The
/// temp directory is returned so it outlives the service.
fn service() -> (
    tempfile::TempDir,
    DataService,
    Receiver<DataEvent>,
    DateTime<Utc>,
) {
    let dir = tempfile::tempdir().unwrap();
    let schema = schema();
    let ds = schema.dataset("series").unwrap().clone();
    let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
    store.apply_schema(&ds).unwrap();
    Catalog::new(store.writer()).ensure_tables().unwrap();

    // 2025-01-06 is a Monday; weekends are skipped, so 250 sessions land
    // inside one calendar year.
    let start: DateTime<Utc> = "2025-01-06T14:30:00Z".parse().unwrap();
    let mut day = start;
    for d in 0..SESSIONS {
        for (i, id) in IDENTITIES.iter().enumerate() {
            let rows = SeriesRows {
                ts: (0..BARS).map(|m| day + Duration::minutes(m)).collect(),
                value: (0..BARS)
                    .map(|m| 100.0 + i as f64 + ((d * BARS + m) % 97) as f64 * 0.01)
                    .collect(),
            };
            append_series(
                &store,
                &SeriesAppendRequest {
                    dataset: &ds,
                    source: "bench",
                    identity: id,
                    rows: &rows,
                    span: (day, day + Duration::days(1)),
                    received_at: day + Duration::days(1),
                },
            )
            .unwrap();
        }
        // Friday (%u == 5) steps over the weekend.
        day += Duration::days(if day.format("%u").to_string() == "5" {
            3
        } else {
            1
        });
    }
    drop(store);

    let (service, rx) = DataService::open_channel(DataServiceConfig {
        db_path: dir.path().join("geode.duckdb"),
        schema,
        views: Vec::new(),
        dimensions: DerivedDimensions::default(),
        query_workers: 4,
        sources: Vec::new(),
        adapters: Default::default(),
        documents: Default::default(),
        egress: Vec::new(),
        clock: geode_core::clock::Clock::utc(),
        pricer: PricerConfig::default(),
        vol: geode_data::VolConfig::default(),
        positions: None,
    })
    .unwrap();
    (dir, service, rx, start)
}

fn source(slot: u8, id: &str) -> SeriesSpec {
    SeriesSpec {
        slot,
        kind: SlotKind::Source {
            source: "bench".into(),
            identity: id.into(),
            rule: BucketRule::Last,
        },
    }
}

/// Ask, and wait for this key's answer: what a tile experiences.
fn round_trip(svc: &DataService, rx: &Receiver<DataEvent>, params: &SeriesParams) -> usize {
    svc.series(params).unwrap();
    loop {
        match rx
            .recv_timeout(std::time::Duration::from_secs(120))
            .expect("no result")
        {
            DataEvent::Series(o) => {
                return o.result.expect("series query failed").buckets.len();
            }
            _ => continue,
        }
    }
}

fn bench(c: &mut Criterion) {
    let (_dir, svc, rx, start) = service();
    let year = (start, start + Duration::days(365));
    let base = |series: Vec<SeriesSpec>,
                frequency: Frequency,
                range: (DateTime<Utc>, DateTime<Utc>),
                stats: bool| SeriesParams {
        key: QueryKey(1),
        tag: 0,
        submitted: std::time::Instant::now(),
        dataset: "series".into(),
        range,
        window: range,
        as_of: AsOf::Live,
        frequency,
        series,
        percentiles: if stats {
            vec![0.05, 0.5, 0.95]
        } else {
            Vec::new()
        },
        bins: if stats { Some(40) } else { None },
    };

    let one = base(vec![source(1, "A")], Frequency::D1, year, false);
    c.bench_function("series_query/1_slot_1d_1y", |b| {
        b.iter(|| black_box(round_trip(&svc, &rx, &one)))
    });

    let ratio = Ast::Bin(Op::Div, Box::new(Ast::Ref(1)), Box::new(Ast::Ref(2)));
    let four = base(
        vec![
            source(1, "A"),
            source(2, "B"),
            source(3, "C"),
            source(4, "D"),
            SeriesSpec {
                slot: 5,
                kind: SlotKind::Expr(ratio),
            },
        ],
        Frequency::D1,
        year,
        false,
    );
    c.bench_function("series_query/4_slots_plus_ratio_1d_1y", |b| {
        b.iter(|| black_box(round_trip(&svc, &rx, &four)))
    });

    let month = (start, start + Duration::days(31));
    let resolve = |text: &str| {
        geode_core::series::expr::parse(text)
            .unwrap()
            .resolve(&mut |r| r.identity.strip_prefix('s')?.parse().ok())
            .unwrap()
    };
    let rolling = base(
        vec![
            source(1, "A"),
            SeriesSpec {
                slot: 2,
                kind: SlotKind::Expr(resolve("sma(diff(s1), 20) / s1[0]")),
            },
        ],
        Frequency::M1,
        month,
        false,
    );
    c.bench_function("series_query/1_slot_plus_sma_diff_1m_1mo", |b| {
        b.iter(|| black_box(round_trip(&svc, &rx, &rolling)))
    });
    let ema = base(
        vec![
            source(1, "A"),
            SeriesSpec {
                slot: 2,
                kind: SlotKind::Expr(resolve("ema(s1, 20)")),
            },
        ],
        Frequency::M1,
        month,
        false,
    );
    c.bench_function("series_query/1_slot_plus_ema_1m_1mo", |b| {
        b.iter(|| black_box(round_trip(&svc, &rx, &ema)))
    });

    let stats = base(
        vec![source(1, "A"), source(2, "B")],
        Frequency::M1,
        month,
        true,
    );
    c.bench_function("series_query/2_slots_1m_1mo_with_stats", |b| {
        b.iter(|| black_box(round_trip(&svc, &rx, &stats)))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
