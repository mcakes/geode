//! Board request costs over a cache of 25 members × 8 tenors × 5 deltas
//! (surface), 3 windows (realized) and 8 tenors (varswap), one close per
//! weekday over the last 399 calendar days (about 285 closes), with
//! coverage recorded per member prefix so lookbacks and the window read
//! from the cache. Setup appends outside timing.
//!
//! Cases: the board read alone (SQL projection plus reduce) for the
//! Changes and Rich/cheap views; the board round trip through the service
//! (a `Latest` prefix fetch per dataset the view reads, the lookback `At`
//! from coverage, the window or sparkline exact `Between` with its
//! coverage check and recent-tail fetch, then the read); the
//! history request shapes the ledger owes (a prefix `Latest`, a prefix
//! `At` that always fetches, an exact `At` over 250 tuples served from
//! coverage, an exact 30-day `Between` from coverage, and that `Between`'s
//! plan alone, the service thread's share); an append of a
//! 400-day single-member prefix answer into the full table (dedupe scan
//! and coverage coalescing); and `validate_against` over a prefix and an
//! exact ask.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{END_OF_DAY, HistoryParams, KeySet, QueryKey, Span};
use geode_core::schema::SchemaSpec;
use geode_core::volboard::{Lookback, Measure, VolBoardParams};
use geode_data::adapter::{
    Adapter, AdapterError, AdapterRegistry, Egress, HistoryQuery, HistoryRequest, HistoryRows,
    Subscription,
};
use geode_data::history_dispatch::plan_calls;
use geode_data::pricing::PricerConfig;
use geode_data::query::volboard::{BoardPlan, read_board};
use geode_data::service::{DataEvent, DataService, DataServiceConfig};
use geode_data::source::SourceSpec;
use geode_data::store::history::{Covered, HistoryAppendRequest, append_history};
use geode_data::store::{Catalog, Store};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

const MEMBERS: usize = 25;
const TENORS: [&str; 8] = ["1M", "2M", "3M", "6M", "1Y", "18M", "2Y", "3Y"];
const DELTAS: [&str; 5] = ["0.1", "0.25", "0.5", "0.75", "0.9"];
const WINDOWS: [&str; 3] = ["5D", "20D", "60D"];
const DAYS_BACK: i64 = 400;
const CLOSE: NaiveTime = match NaiveTime::from_hms_opt(21, 0, 0) {
    Some(t) => t,
    None => unreachable!(),
};

const SCHEMA: &str = r#"
[vol_surface]
family = "history"
key = ["underlying_ref", "tenor", "delta"]
history = "400d"
retention = "7d"
[vol_surface.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[vol_surface.columns.tenor]
type = "utf8"
role = "dimension"
order = "tenor"
[vol_surface.columns.delta]
type = "f64"
role = "dimension"
[vol_surface.columns.vol]
type = "f64"
role = "value"
[vol_surface.columns.forward]
type = "f64"
role = "value"

[realized_vol]
family = "history"
key = ["underlying_ref", "window"]
history = "400d"
retention = "7d"
[realized_vol.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[realized_vol.columns.window]
type = "utf8"
role = "dimension"
order = "tenor"
[realized_vol.columns.vol]
type = "f64"
role = "value"

[varswap]
family = "history"
key = ["underlying_ref", "tenor"]
history = "400d"
retention = "7d"
[varswap.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[varswap.columns.tenor]
type = "utf8"
role = "dimension"
order = "tenor"
[varswap.columns.strike]
type = "f64"
role = "value"
"#;

fn schema() -> SchemaSpec {
    let doc = merge_docs(
        "datasets",
        &[LayerDoc::builtin("datasets", SCHEMA).unwrap()],
    );
    let (schema, diags) = SchemaSpec::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    schema
}

fn member(i: usize) -> String {
    format!("M{i:02}")
}

fn members() -> Vec<String> {
    (0..MEMBERS).map(member).collect()
}

fn suffixes(table: &str) -> Vec<Vec<String>> {
    match table {
        "surface" => TENORS
            .iter()
            .flat_map(|t| {
                DELTAS
                    .iter()
                    .map(move |d| vec![t.to_string(), d.to_string()])
            })
            .collect(),
        "realized" => WINDOWS.iter().map(|w| vec![w.to_string()]).collect(),
        "varswap" => TENORS.iter().map(|t| vec![t.to_string()]).collect(),
        _ => unreachable!(),
    }
}

fn dataset(table: &str) -> &'static str {
    match table {
        "surface" => "vol_surface",
        "realized" => "realized_vol",
        "varswap" => "varswap",
        _ => unreachable!(),
    }
}

/// Deterministic values: a function of member, suffix and day ordinal,
/// varied enough that dedupe compares real values.
fn value(table: &str, tuple: &[String], day: i64) -> Vec<f64> {
    let m: i64 = tuple[0][1..].parse().unwrap();
    let wave = ((m * 7 + day) % 40) as f64 * 0.0005;
    match table {
        "surface" => {
            let t = TENORS.iter().position(|x| *x == tuple[1]).unwrap() as f64;
            let d: f64 = tuple[2].parse().unwrap();
            vec![
                0.15 + 0.01 * t + 0.2 * (d - 0.5) * (d - 0.5) + wave,
                100.0 + m as f64 + day as f64 * 0.01,
            ]
        }
        "realized" => vec![0.12 + wave],
        "varswap" => vec![0.17 + wave],
        _ => unreachable!(),
    }
}

/// Weekdays from `now − (DAYS_BACK − 1)` days up to yesterday, oldest
/// first. Starting a day inside the datasets' 400-day history keeps the
/// oldest 21:00 close inside the sweep cutoff whatever the time of day the
/// bench runs, so no case appends rows the sweep then removes.
fn closes(now: DateTime<Utc>) -> Vec<NaiveDate> {
    let mut day = (now - Duration::days(DAYS_BACK - 1)).date_naive();
    let last = (now - Duration::days(1)).date_naive();
    let mut out = Vec::new();
    while day <= last {
        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            out.push(day);
        }
        day = day.succ_opt().unwrap();
    }
    out
}

fn rows_for(table: &str, tuples: &[Vec<String>], days: &[NaiveDate]) -> HistoryRows {
    let mut rows = HistoryRows::default();
    // An ask the generator has no tuple for answers no rows.
    let Some(first) = tuples.first() else {
        return rows;
    };
    rows.keys.resize(first.len(), Vec::new());
    rows.values.resize(value(table, first, 0).len(), Vec::new());
    for tuple in tuples {
        for day in days {
            rows.ts.push(Utc.from_utc_datetime(&day.and_time(CLOSE)));
            for (c, v) in tuple.iter().enumerate() {
                rows.keys[c].push(v.clone());
            }
            for (c, v) in value(table, tuple, day.num_days_from_ce() as i64)
                .into_iter()
                .enumerate()
            {
                rows.values[c].push(v);
            }
        }
    }
    rows
}

fn full_tuples(table: &str, members: &[String]) -> Vec<Vec<String>> {
    members
        .iter()
        .flat_map(|m| {
            suffixes(table)
                .into_iter()
                .map(move |s| std::iter::once(m.clone()).chain(s).collect())
        })
        .collect()
}

/// Seed the three datasets in 20-day chunks, each chunk covered per
/// member prefix so coalescing leaves one span per member.
fn seed(store: &Store, schema: &SchemaSpec, now: DateTime<Utc>) {
    let days = closes(now);
    for table in ["surface", "realized", "varswap"] {
        let ds = schema.dataset(dataset(table)).unwrap();
        store.apply_schema(ds).unwrap();
        let tuples = full_tuples(table, &members());
        for chunk in days.chunks(20) {
            let rows = rows_for(table, &tuples, chunk);
            let from = Utc.from_utc_datetime(&chunk[0].and_time(NaiveTime::MIN));
            let to = Utc.from_utc_datetime(
                &chunk[chunk.len() - 1]
                    .succ_opt()
                    .unwrap()
                    .and_time(NaiveTime::MIN),
            );
            let covered = members()
                .into_iter()
                .map(|m| Covered {
                    asked: vec![m],
                    span: (from, to),
                })
                .collect();
            append_history(
                store,
                &HistoryAppendRequest {
                    dataset: ds,
                    source: "bench",
                    rows: &rows,
                    covered,
                    received_at: now,
                },
            )
            .unwrap();
        }
    }
}

/// Answers every ask from the same generator, so a fetched `Latest`
/// appends rows equal to the cache (dedupe) and an `At` or `Between`
/// answers what the seed holds.
struct BenchHanweck {
    now: DateTime<Utc>,
}

struct BenchHistory {
    now: DateTime<Utc>,
}

impl Adapter for BenchHanweck {
    fn name(&self) -> &'static str {
        "bench_hanweck"
    }
    fn subscription(&self) -> Option<Box<dyn Subscription>> {
        None
    }
    fn egress(&self) -> Option<Box<dyn Egress>> {
        None
    }
    fn history(&self) -> Option<Box<dyn HistoryQuery>> {
        Some(Box::new(BenchHistory { now: self.now }))
    }
}

impl HistoryQuery for BenchHistory {
    fn query(&mut self, req: &HistoryRequest) -> Result<HistoryRows, AdapterError> {
        let table = req.table.as_str();
        let all = suffixes(table);
        let tuples: Vec<Vec<String>> = req
            .keys
            .tuples()
            .iter()
            .flat_map(|t| match &req.keys {
                KeySet::Exact(_) => vec![t.clone()],
                KeySet::Prefix(_) => all
                    .iter()
                    .filter(|s| s.starts_with(&t[1..]))
                    .map(|s| {
                        std::iter::once(t[0].clone())
                            .chain(s.iter().cloned())
                            .collect()
                    })
                    .collect(),
            })
            .collect();
        let days = closes(self.now);
        let chosen: Vec<NaiveDate> = match req.span {
            Span::Latest => days.last().copied().into_iter().collect(),
            Span::At(t) => days
                .iter()
                .rev()
                .find(|d| Utc.from_utc_datetime(&d.and_time(CLOSE)) <= t)
                .copied()
                .into_iter()
                .collect(),
            Span::Between { from, to, .. } => days
                .into_iter()
                .filter(|d| {
                    let at = Utc.from_utc_datetime(&d.and_time(CLOSE));
                    from <= at && at < to
                })
                .collect(),
        };
        Ok(rows_for(table, &tuples, &chosen))
    }

    fn keys(&mut self, table: &str, column: &str) -> Option<Vec<String>> {
        match (table, column) {
            (_, "tenor") => Some(TENORS.iter().map(|s| s.to_string()).collect()),
            (_, "delta") => Some(DELTAS.iter().map(|s| s.to_string()).collect()),
            (_, "window") => Some(WINDOWS.iter().map(|s| s.to_string()).collect()),
            _ => None,
        }
    }
}

fn service(dir: &tempfile::TempDir, now: DateTime<Utc>) -> (DataService, Receiver<DataEvent>) {
    let mut adapters = AdapterRegistry::default();
    adapters.register(Arc::new(BenchHanweck { now }));
    let source = |name: &str, table: &str| SourceSpec {
        adapter: "bench_hanweck".to_string(),
        table: Some(table.to_string()),
        ..SourceSpec::directory(name, dataset(table), Vec::new())
    };
    DataService::open_channel(DataServiceConfig {
        db_path: dir.path().join("geode.duckdb"),
        schema: schema(),
        views: Vec::new(),
        dimensions: DerivedDimensions::default(),
        query_workers: 4,
        sources: vec![
            source("hanweck_surface", "surface"),
            source("hanweck_realized", "realized"),
            source("hanweck_varswap", "varswap"),
        ],
        adapters,
        documents: Default::default(),
        egress: Vec::new(),
        clock: geode_core::clock::Clock::utc(),
        pricer: PricerConfig::default(),
        vol: geode_data::VolConfig::default(),
        positions: None,
    })
    .unwrap()
}

fn changes_view() -> Vec<Measure> {
    let mut m = vec![Measure::Forward { tenor: "1M".into() }];
    m.extend(TENORS.iter().map(|t| Measure::atfm(t)));
    m.push(Measure::Skew { tenor: "3M".into() });
    m.push(Measure::Slope {
        long: "1Y".into(),
        short: "1M".into(),
    });
    m
}

fn rich_cheap_view() -> Vec<Measure> {
    vec![
        Measure::atfm("3M"),
        Measure::Realized {
            window: "5D".into(),
        },
        Measure::Realized {
            window: "20D".into(),
        },
        Measure::IvRv {
            tenor: "3M".into(),
            window: "20D".into(),
        },
        Measure::Varswap { tenor: "3M".into() },
        Measure::VsSpread { tenor: "3M".into() },
        Measure::Skew { tenor: "3M".into() },
        Measure::Slope {
            long: "1Y".into(),
            short: "1M".into(),
        },
    ]
}

fn params(
    tag: u64,
    measures: Vec<Measure>,
    lookbacks: Vec<Lookback>,
    window_days: Option<u64>,
    spark: bool,
) -> VolBoardParams {
    VolBoardParams {
        key: QueryKey(1),
        tag,
        submitted: std::time::Instant::now(),
        surface: "vol_surface".into(),
        realized: Some("realized_vol".into()),
        varswap: Some("varswap".into()),
        members: members(),
        measures,
        lookbacks,
        window: window_days.map(|d| std::time::Duration::from_secs(d * 86_400)),
        sparkline: spark.then(|| {
            (
                Measure::atfm("3M"),
                std::time::Duration::from_secs(30 * 86_400),
            )
        }),
    }
}

fn plan(params: VolBoardParams, now: DateTime<Utc>) -> BoardPlan {
    let clock = geode_core::clock::Clock::utc();
    let instants = params
        .lookbacks
        .iter()
        .map(|lb| geode_core::volboard::plan::lookback_instant(&clock, now, *lb).unwrap())
        .collect();
    BoardPlan {
        params,
        now,
        instants,
        clock,
        dataset_errors: Vec::new(),
    }
}

fn board_trip(svc: &DataService, rx: &Receiver<DataEvent>, p: &VolBoardParams) -> usize {
    svc.vol_board(p.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left).expect("no board within 120 s") {
            DataEvent::VolBoard(o) if o.tag == p.tag => {
                return o.result.expect("board failed").rows.len();
            }
            _ => continue,
        }
    }
}

fn history_trip(svc: &DataService, rx: &Receiver<DataEvent>, p: &HistoryParams) -> usize {
    svc.history(p.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left).expect("no history within 120 s") {
            DataEvent::History(o) if o.tag == p.tag => {
                return o.result.expect("history failed").rows.len();
            }
            _ => continue,
        }
    }
}

fn bench(c: &mut Criterion) {
    let now = Utc::now();
    let dir = tempfile::tempdir().unwrap();
    let schema = schema();
    let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
    Catalog::new(store.writer()).ensure_tables().unwrap();
    seed(&store, &schema, now);

    // The read alone: SQL projection plus reduce, on one connection.
    let changes = plan(
        params(0, changes_view(), vec![Lookback::D1], None, true),
        now,
    );
    c.bench_function("board_read/changes_view_25", |b| {
        b.iter(|| {
            black_box(
                read_board(store.writer(), &schema, &changes)
                    .unwrap()
                    .rows
                    .len(),
            )
        })
    });
    let rich = plan(params(0, rich_cheap_view(), vec![], Some(365), false), now);
    c.bench_function("board_read/rich_cheap_view_25", |b| {
        b.iter(|| {
            black_box(
                read_board(store.writer(), &schema, &rich)
                    .unwrap()
                    .rows
                    .len(),
            )
        })
    });

    // An append of one member's whole surface history (a prefix answer of
    // about 11,000 rows) into the full table: every row equal to live, so
    // this is the dedupe scan plus the coverage coalescing.
    let ds = schema.dataset("vol_surface").unwrap();
    let days = closes(now);
    let one = rows_for("surface", &full_tuples("surface", &[member(0)]), &days);
    let span = (
        Utc.from_utc_datetime(&days[0].and_time(NaiveTime::MIN)),
        Utc.from_utc_datetime(
            &days[days.len() - 1]
                .succ_opt()
                .unwrap()
                .and_time(NaiveTime::MIN),
        ),
    );
    c.bench_function("append/between_400d_prefix_answer_deduped", |b| {
        b.iter(|| {
            black_box(
                append_history(
                    &store,
                    &HistoryAppendRequest {
                        dataset: ds,
                        source: "bench",
                        rows: &one,
                        covered: vec![Covered {
                            asked: vec![member(0)],
                            span,
                        }],
                        received_at: now,
                    },
                )
                .unwrap()
                .appended,
            )
        })
    });

    // validate_against as the worker runs it, over a prefix and an exact ask.
    let prefix_rows = rows_for("surface", &full_tuples("surface", &members()), &days);
    let prefix_ask = KeySet::Prefix(members().into_iter().map(|m| vec![m]).collect());
    c.bench_function("validate/286k_rows_vs_25_prefix", |b| {
        b.iter(|| black_box(prefix_rows.validate_against(3, 2, &prefix_ask)).unwrap())
    });
    let exact_tuples: Vec<Vec<String>> = {
        let p = params(0, changes_view(), vec![], None, false);
        let s = geode_core::volboard::plan::suffixes(&p);
        members()
            .into_iter()
            .flat_map(|m| {
                s[&geode_core::volboard::Role::Surface]
                    .iter()
                    .map(move |x| {
                        std::iter::once(m.clone())
                            .chain(x.iter().cloned())
                            .collect()
                    })
            })
            .collect()
    };
    let exact_rows = rows_for("surface", &exact_tuples, &days);
    let exact_ask = KeySet::Exact(exact_tuples.clone());
    c.bench_function("validate/71k_rows_vs_250_exact", |b| {
        b.iter(|| black_box(exact_rows.validate_against(3, 2, &exact_ask)).unwrap())
    });

    // The service-thread share of a window Between: planning alone, no
    // fetch, no append, no read, over the same covered store and the same
    // 250 exact tuples as `history/exact_between_30d_250_from_coverage`.
    let surface = schema.dataset("vol_surface").unwrap();
    c.bench_function("history/plan_between_250", |b| {
        b.iter(|| {
            let span = Span::Between {
                from: now - Duration::days(30),
                to: now,
                step: None,
            };
            black_box(
                plan_calls(store.writer(), surface, "surface", &exact_ask, span, now)
                    .unwrap()
                    .len(),
            )
        })
    });
    drop(store);

    // Through the service: the round trips a tile pays.
    let (svc, rx) = service(&dir, now);
    let mut tag = 0u64;
    let mut next = || {
        tag += 1;
        tag
    };
    c.bench_function("vol_board/changes_view_25", |b| {
        b.iter(|| {
            black_box(board_trip(
                &svc,
                &rx,
                &params(next(), changes_view(), vec![Lookback::D1], None, true),
            ))
        })
    });
    c.bench_function("vol_board/rich_cheap_view_25", |b| {
        b.iter(|| {
            black_box(board_trip(
                &svc,
                &rx,
                &params(next(), rich_cheap_view(), vec![], Some(365), false),
            ))
        })
    });
    let yesterday_end = geode_core::clock::Clock::utc()
        .resolve_local(
            geode_core::clock::business_days_back((now - Duration::days(1)).date_naive(), 0),
            END_OF_DAY,
        )
        .unwrap();
    let history = |tag: u64, keys: KeySet, span: Span| HistoryParams {
        key: QueryKey(2),
        tag,
        submitted: std::time::Instant::now(),
        dataset: "vol_surface".into(),
        keys,
        span,
    };
    c.bench_function("history/prefix_latest_25", |b| {
        b.iter(|| {
            black_box(history_trip(
                &svc,
                &rx,
                &history(next(), prefix_ask.clone(), Span::Latest),
            ))
        })
    });
    c.bench_function("history/prefix_at_25_always_fetches", |b| {
        b.iter(|| {
            black_box(history_trip(
                &svc,
                &rx,
                &history(next(), prefix_ask.clone(), Span::At(yesterday_end)),
            ))
        })
    });
    c.bench_function("history/exact_at_250_from_coverage", |b| {
        b.iter(|| {
            black_box(history_trip(
                &svc,
                &rx,
                &history(next(), exact_ask.clone(), Span::At(yesterday_end)),
            ))
        })
    });
    let month = Span::Between {
        from: now - Duration::days(30),
        to: now,
        step: None,
    };
    c.bench_function("history/exact_between_30d_250_from_coverage", |b| {
        b.iter(|| {
            black_box(history_trip(
                &svc,
                &rx,
                &history(next(), exact_ask.clone(), month),
            ))
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench
}
criterion_main!(benches);
