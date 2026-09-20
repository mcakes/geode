//! The sheet core's costs at spec §8.2's shape — a sheet of 1,000 lines —
//! against §7's 8 ms pure-UI budget. `parse` is per line typed; `apply`
//! and undo is per keystroke; `to_rows`/`from_rows` is per autosave and
//! per restore. Medians go to docs/perf.md under "Line pricer core".

use criterion::{Criterion, criterion_group, criterion_main};
use geode_pricer::core::{Edit, OwnShifts, Place, RowSpec, Sheet, from_rows, parse, to_rows};
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
    let rows: Vec<RowSpec> = texts(n)
        .iter()
        .map(|t| parse(t).expect("bench text parses"))
        .collect();
    s.apply(Edit::Insert {
        place: Place::Root { at: 0 },
        rows,
    })
    .expect("insert");
    s
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("pricer_core");

    let lines = texts(1_000);
    g.bench_function("parse_1000_lines", |b| {
        b.iter(|| {
            for t in &lines {
                black_box(parse(t).expect("parses"));
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
    let other = parse("SPX Z26 9999 P").expect("parses");
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

    let s = sheet(1_000);
    g.bench_function("to_rows_from_rows_1000", |b| {
        b.iter(|| {
            let rows = to_rows(&s).expect("rows");
            black_box(from_rows("bench", &rows).expect("loads"))
        })
    });

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
