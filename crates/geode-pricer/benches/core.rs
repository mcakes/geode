//! Parsing, edits and undo, result installation, storage conversion, and grid
//! preparation for 1,000 shorthand entries. Every tenth entry is a two-leg
//! package, so the sheet contains 1,200 rows. These benchmarks measure local
//! model work, excluding pricing execution, database I/O, and painting.
//! Budgets and reference measurements are in `docs/current/performance.md`.

use chrono::Utc;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use geode_core::clock::Clock;
use geode_core::pricing::PriceResult;
use geode_pricer::core::{
    ColumnPlan, Edit, Expansion, LineId, OwnShifts, Place, RowSpec, Sheet, TemplateSet, Views,
    from_rows, parse, to_rows,
};
use geode_pricer::grid::GridModel;
use std::hint::black_box;

/// `n` shorthand entries with varied quantity, option kind, and strike.
/// Every tenth entry is a callspread, exercising package folding as well.
fn texts(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            if i % 10 == 9 {
                format!(
                    "-{} SPX Z26 {}/{} CS",
                    1 + i % 4,
                    4000 + (i % 400) * 5,
                    4100 + (i % 400) * 5
                )
            } else {
                format!(
                    "{} SPX Z26 {} {}",
                    if i % 3 == 0 { -1 } else { 1 + (i % 5) as i64 },
                    4000 + (i % 400) * 5,
                    if i % 2 == 0 { "C" } else { "P" }
                )
            }
        })
        .collect()
}

fn sheet(n: usize) -> Sheet {
    let mut s = Sheet::new("bench");
    let templates = TemplateSet::builtin();
    let rows: Vec<RowSpec> = texts(n)
        .iter()
        .map(|t| parse(t, &templates).expect("bench text parses"))
        .collect();
    s.apply(Edit::Insert {
        place: Place::Root { at: 0 },
        rows,
    })
    .expect("insert");
    s
}

fn bench(c: &mut Criterion) {
    let templates = TemplateSet::builtin();
    let mut g = c.benchmark_group("pricer_core");

    let lines = texts(1_000);
    g.bench_function("parse_1000_lines", |b| {
        b.iter(|| {
            for t in &lines {
                black_box(parse(t, &templates).expect("parses"));
            }
        })
    });

    // Each iteration sets the sheet shift and undoes it. Both operations
    // revise inheriting lines and mark them stale without submitting requests.
    let mut s = sheet(1_000);
    g.bench_function("apply_undo_sheet_shift_1000", |b| {
        b.iter(|| {
            let undo = s
                .apply(Edit::SetSheetShift(OwnShifts {
                    spot_pct: Some(2.0),
                    vol_pts: None,
                }))
                .expect("apply");
            black_box(s.undo(&undo).expect("undo"));
        })
    });

    let mut s = sheet(1_000);
    let instrument = s.instrument(500).expect("a line").clone();
    let other = parse("SPX Z26 9999 P", &templates).expect("parses");
    let other = match other {
        RowSpec::Line(l) => l.instrument,
        RowSpec::Package { .. } => unreachable!(),
    };
    g.bench_function("apply_undo_set_instrument_1000", |b| {
        b.iter(|| {
            let undo = s
                .apply(Edit::SetInstrument {
                    row: 500,
                    instrument: other.clone(),
                })
                .expect("apply");
            black_box(s.undo(&undo).expect("undo"));
            debug_assert_eq!(s.instrument(500), Some(&instrument));
        })
    });

    // One full reprice: every line's answer landing in one batch, with
    // the single fold `deliver_all` promises at the end of it.
    let mut s = sheet(1_000);
    let batch: Vec<(LineId, u64, Result<PriceResult, String>)> = (0..s.len())
        .filter(|r| s.is_line(*r))
        .map(|r| {
            (
                s.id(r),
                s.revision(r),
                Ok(PriceResult {
                    price: 12.5,
                    delta: 0.5,
                    gamma: 0.01,
                    vega: 1.0,
                    theta: -0.5,
                    rho: 0.1,
                }),
            )
        })
        .collect();
    let now = Utc::now();
    g.bench_function("deliver_all_1000", |b| {
        b.iter_batched(
            || batch.clone(),
            |batch| black_box(s.deliver_all(batch, now)),
            BatchSize::SmallInput,
        )
    });

    let s = sheet(1_000);
    g.bench_function("to_rows_from_rows_1000", |b| {
        b.iter(|| {
            let rows = to_rows(&s).expect("rows");
            black_box(from_rows("bench", &rows).expect("loads"))
        })
    });

    // A full grid rebuild with every package open and every line answered.
    // The tile performs this preparation after edits, deliveries, and
    // expansion changes.
    let mut s = sheet(1_000);
    let answers: Vec<(LineId, u64, Result<PriceResult, String>)> = (0..s.len())
        .filter(|r| s.is_line(*r))
        .map(|r| {
            (
                s.id(r),
                s.revision(r),
                Ok(PriceResult {
                    price: 12.5,
                    delta: 0.5,
                    gamma: 0.01,
                    vega: 1.0,
                    theta: -0.5,
                    rho: 0.1,
                }),
            )
        })
        .collect();
    s.deliver_all(answers, Utc::now());
    let mut expansion = Expansion::default();
    expansion.open_all(&s);
    let views = Views::builtin();
    let plan = ColumnPlan::build(views.get("vanilla").expect("bundled"));
    g.bench_function("grid_build_1000", |b| {
        b.iter(|| black_box(GridModel::build(&s, &expansion, &plan, Clock::utc())))
    });

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
