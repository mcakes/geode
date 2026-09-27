//! The sheet core's costs at spec §8.2's shape — a sheet of 1,000 lines —
//! against §7's 8 ms pure-UI budget. `parse` is per line typed; `apply`
//! and undo is per keystroke; `to_rows`/`from_rows` is per autosave and
//! per restore; `grid_build_1000` is the prepared `GridModel` rebuild —
//! the per-keystroke cost the table pays on every edit, delivery and
//! expansion change. Medians go to docs/perf.md under "Line pricer core"
//! (`grid_build_1000`: "Line pricer tile").

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

/// `n` distinct vanilla lines: alternating buy/sell, calls/puts, strikes
/// stepping through 400 levels — enough variety that no two requests are
/// equal, and every tenth line a callspread so packages are folded too.
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

    // The sheet-wide shift toggles between set and cleared on alternate
    // iterations, so every iteration is one apply that re-requests every
    // inheriting line plus the undo that re-requests them again.
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

    // Spec §8.2 / §12: the grid model is rebuilt on every edit, delivery
    // and expansion change, so a whole build at 1,000 lines — every
    // package open, every line answered — is the per-keystroke cost the
    // 8 ms budget constrains.
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
