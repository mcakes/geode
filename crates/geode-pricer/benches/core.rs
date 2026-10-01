//! Parsing, edits and undo, result installation, storage conversion, scope
//! evaluation, the rollup tree, and grid preparation for 1,000 shorthand entries. Every
//! tenth entry is a two-leg package, so the sheet contains 1,200 rows. These benchmarks
//! measure local model work, excluding pricing execution, database I/O, and painting.
//! Budgets and reference measurements are in `docs/current/performance.md`.

use chrono::Utc;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use geode_core::clock::Clock;
use geode_core::dimensions::DerivedDimensions;
use geode_core::expansion::Expansion as GroupExpansion;
use geode_core::pricing::{Currency, Measure, PriceResult};
use geode_core::scope::{Scope, parse_expr};
use geode_pricer::core::rollup::{self, EffectiveChain, effective_chain};
use geode_pricer::core::{
    ColumnPlan, Edit, Expansion, LineId, OwnShifts, Place, RowSpec, Sheet, TemplateSet, Views,
    Visibility, apply_scope, from_rows, parse, to_rows,
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

/// `n` entries over four underlyings and three expiries, so a grouping
/// has nodes to make: every tenth a callspread, every twentieth (in its
/// place) a Z26/H27 calendar, which `expiry` splits across two nodes.
fn grouped_texts(n: usize) -> Vec<String> {
    const UNDS: [&str; 4] = ["SPX", "NDX", "SX5E", "RTY"];
    const EXPS: [&str; 3] = ["Z26", "H27", "M27"];
    (0..n)
        .map(|i| {
            let und = UNDS[i % 4];
            let exp = EXPS[(i / 4) % 3];
            let k = 4000 + (i % 400) * 5;
            match i % 20 {
                19 => format!("{und} Z26/H27 {k} CAL"),
                9 => format!("-{} {und} {exp} {k}/{} CS", 1 + i % 4, k + 100),
                _ => format!(
                    "{} {und} {exp} {k} {}",
                    if i % 3 == 0 { -1 } else { 1 + (i % 5) as i64 },
                    if i % 2 == 0 { "C" } else { "P" }
                ),
            }
        })
        .collect()
}

fn sheet(n: usize) -> Sheet {
    sheet_of(&texts(n))
}

fn sheet_of(texts: &[String]) -> Sheet {
    let mut s = Sheet::new("bench");
    let templates = TemplateSet::builtin();
    let rows: Vec<RowSpec> = texts
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

/// Every line of `s` answered at its current revision: npv 12.5, delta 0.5.
fn answers_for(s: &Sheet) -> Vec<(LineId, u64, Result<PriceResult, String>)> {
    (0..s.len())
        .filter(|r| s.is_line(*r))
        .map(|r| {
            let mut p = PriceResult::zero(Currency::USD);
            p.set(Measure::Npv, false, 12.5);
            p.set(Measure::Delta01, false, 0.5);
            (s.id(r), s.revision(r), Ok(p))
        })
        .collect()
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
                Ok({
                    let mut r = PriceResult::zero(Currency::USD);
                    r.set(Measure::Npv, false, 12.5);
                    r.set(Measure::Delta01, false, 0.5);
                    r
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
                Ok({
                    let mut r = PriceResult::zero(Currency::USD);
                    r.set(Measure::Npv, false, 12.5);
                    r.set(Measure::Delta01, false, 0.5);
                    r
                }),
            )
        })
        .collect();
    s.deliver_all(answers, Utc::now());
    let mut expansion = Expansion::default();
    expansion.open_all(&s);
    let views = Views::builtin();
    let plan = ColumnPlan::build(views.get("vanilla").expect("bundled"));
    let visibility = Visibility::all(&s);
    let flat = rollup::build(
        &s,
        &visibility,
        &EffectiveChain::default(),
        &DerivedDimensions::default(),
        Clock::utc(),
    );
    let no_groups = GroupExpansion::default();
    g.bench_function("grid_build_1000", |b| {
        b.iter(|| {
            black_box(GridModel::build(
                &s,
                &flat,
                &no_groups,
                &expansion,
                &plan,
                Clock::utc(),
            ))
        })
    });
    // The tile's flat rebuild as it runs: the grid is always built from a
    // rollup, so an ungrouped sheet pays for an empty-chain tree too.
    let no_levels = DerivedDimensions::default();
    g.bench_function("rebuild_1000_flat", |b| {
        b.iter(|| {
            let chain = effective_chain(&[], &no_levels);
            let tree = rollup::build(&s, &visibility, &chain, &no_levels, Clock::utc());
            black_box(GridModel::build(
                &s,
                &tree,
                &no_groups,
                &expansion,
                &plan,
                Clock::utc(),
            ))
        })
    });
    // What paint reads per frame today: 40 rows' prepared cells. After the
    // windowed slice this name measures a cold window fill of 40 rows.
    let built = GridModel::build(&s, &flat, &no_groups, &expansion, &plan, Clock::utc());
    g.bench_function("window_fill_40", |b| {
        b.iter(|| {
            let mut n = 0usize;
            for row in &built.rows[..40] {
                for cell in &row.cells {
                    n += black_box(cell.text.clone()).len();
                }
            }
            black_box(n)
        })
    });
    // A delivery that moves no line in or out of the tree, as the tile runs
    // it today: install, scope, chain, rollup, whole grid.
    {
        let mut s = sheet(1_000);
        let batch = answers_for(&s);
        s.deliver_all(batch.clone(), Utc::now());
        let mut expansion = Expansion::default();
        expansion.open_all(&s);
        let dims = DerivedDimensions::default();
        let empty = Scope::default();
        let now = Utc::now();
        g.bench_function("deliver_unchanged_structure_1000", |b| {
            b.iter_batched(
                || batch.clone(),
                |batch| {
                    s.mark_all_stale();
                    s.deliver_all(batch, now);
                    let visibility = apply_scope(&s, &empty, &dims, Clock::utc())
                        .expect("the empty scope applies");
                    let chain = effective_chain(&[], &dims);
                    let tree = rollup::build(&s, &visibility, &chain, &dims, Clock::utc());
                    black_box(GridModel::build(
                        &s,
                        &tree,
                        &no_groups,
                        &expansion,
                        &plan,
                        Clock::utc(),
                    ))
                },
                BatchSize::SmallInput,
            )
        });
    }

    // The scope the tile applies on every rebuild: a three-term expression
    // and a text filter over every line of the priced, opened sheet above.
    // `strike >= 5000` hides about half the lines (strikes run 4000–5995),
    // so the scoped build skips half the rows and folds the partly hidden
    // packages' shown legs.
    let scope = Scope {
        expression: Some(
            parse_expr("underlying_ref = 'SPX' and strike >= 5000 and npv != 0")
                .expect("bench expression parses"),
        ),
        text: Some("spx".into()),
        ..Scope::default()
    };
    let dims = DerivedDimensions::default();
    let scoped = apply_scope(&s, &scope, &dims, Clock::utc()).expect("the scope applies");
    assert!(
        scoped.hidden > 400 && scoped.hidden < 700,
        "about half hidden: {}",
        scoped.hidden
    );
    g.bench_function("apply_scope_1000", |b| {
        b.iter(|| black_box(apply_scope(&s, &scope, &dims, Clock::utc()).expect("applies")))
    });
    let scoped_flat = rollup::build(&s, &scoped, &EffectiveChain::default(), &dims, Clock::utc());
    g.bench_function("grid_build_1000_scoped", |b| {
        b.iter(|| {
            black_box(GridModel::build(
                &s,
                &scoped_flat,
                &no_groups,
                &expansion,
                &plan,
                Clock::utc(),
            ))
        })
    });

    // Regrouping: 1,000 priced entries over four underlyings and three
    // expiries under `[underlying_ref, expiry, position_ref]` — the tree,
    // then the grid with every group and package open, each group row
    // summing and reading unanimity over its legs. The tile does both on
    // every rebuild under a grouping (budget: 8 ms together).
    let mut s = sheet_of(&grouped_texts(1_000));
    let answers: Vec<(LineId, u64, Result<PriceResult, String>)> = (0..s.len())
        .filter(|r| s.is_line(*r))
        .map(|r| {
            let mut p = PriceResult::zero(Currency::USD);
            p.set(Measure::Npv, false, 12.5);
            p.set(Measure::Delta01, false, 0.5);
            (s.id(r), s.revision(r), Ok(p))
        })
        .collect();
    s.deliver_all(answers, Utc::now());
    let levels: Vec<String> = ["underlying_ref", "expiry", "position_ref"]
        .iter()
        .map(|l| l.to_string())
        .collect();
    let chain = effective_chain(&levels, &dims);
    assert_eq!(chain.kept.len(), 3, "every level groups");
    let visibility = Visibility::all(&s);
    g.bench_function("rollup_1000", |b| {
        b.iter(|| black_box(rollup::build(&s, &visibility, &chain, &dims, Clock::utc())))
    });
    let tree = rollup::build(&s, &visibility, &chain, &dims, Clock::utc());
    let mut groups = GroupExpansion::default();
    groups.open_all();
    let mut expansion = Expansion::default();
    expansion.open_all(&s);
    g.bench_function("grid_build_1000_grouped", |b| {
        b.iter(|| {
            black_box(GridModel::build(
                &s,
                &tree,
                &groups,
                &expansion,
                &plan,
                Clock::utc(),
            ))
        })
    });
    // Both steps together, as the tile's rebuild under the grouping runs
    // them (the 8 ms budget applies to this sum).
    g.bench_function("rebuild_1000_grouped", |b| {
        b.iter(|| {
            let tree = rollup::build(&s, &visibility, &chain, &dims, Clock::utc());
            black_box(GridModel::build(
                &s,
                &tree,
                &groups,
                &expansion,
                &plan,
                Clock::utc(),
            ))
        })
    });
    let built = GridModel::build(&s, &tree, &groups, &expansion, &plan, Clock::utc());
    g.bench_function("window_fill_40_grouped", |b| {
        b.iter(|| {
            let mut n = 0usize;
            for row in &built.rows[..40] {
                for cell in &row.cells {
                    n += black_box(cell.text.clone()).len();
                }
            }
            black_box(n)
        })
    });
    {
        let batch = answers_for(&s);
        let empty = Scope::default();
        let now = Utc::now();
        g.bench_function("deliver_unchanged_structure_1000_grouped", |b| {
            b.iter_batched(
                || batch.clone(),
                |batch| {
                    s.mark_all_stale();
                    s.deliver_all(batch, now);
                    let visibility = apply_scope(&s, &empty, &dims, Clock::utc())
                        .expect("the empty scope applies");
                    let chain = effective_chain(&levels, &dims);
                    let tree = rollup::build(&s, &visibility, &chain, &dims, Clock::utc());
                    black_box(GridModel::build(
                        &s,
                        &tree,
                        &groups,
                        &expansion,
                        &plan,
                        Clock::utc(),
                    ))
                },
                BatchSize::SmallInput,
            )
        });
    }

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
