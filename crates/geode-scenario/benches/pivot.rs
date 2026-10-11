//! One delivery's pure-UI cost for a 40 × 40 matrix with its Σ companion:
//! the pivot build, then the delegate preparing a 40-row window of cells.
//! The snapshots, domains and (for the window bench) the pivot are built
//! once, outside the timed loop. Target: together under the 8 ms pure-UI
//! budget. Most of the window's cost is choosing readable text over each
//! heat fill (`geode_core::colour::readable_text_on`).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::colour::Rgb;
use geode_core::scenario::{AxisInfo, Columns, MeasureColumn, Paint, ShapeSpec};
use geode_core::schema::ColumnType;
use geode_core::schema::axis::{AxisKind, AxisOrder};
use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
use geode_core::view::ColumnFormat;
use geode_scenario::core::pivot::{AxisDomains, Currency, Mode, Pivot};
use geode_scenario::core::prepare::{self, Inputs, Layout, Palette, PreparedCell};
use geode_tile::grid::WindowCache;

const N: usize = 40;

fn meta(name: &str, depth: usize, summable: bool) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![Attribution::Additive; depth + 1],
        scope_semantics: ScopeSemantics::Direct,
        summable,
        mixed_flag: None,
    }
}

fn shape() -> ShapeSpec {
    ShapeSpec {
        kind: "vega_kt".into(),
        title: "Vega K/T".into(),
        dataset: "vega_buckets".into(),
        rows: "tenor".into(),
        columns: Columns::Axes(vec!["moneyness".into()]),
        value: Some(MeasureColumn {
            column: "vega".into(),
            label: "vega".into(),
            format: ColumnFormat::MEASURE,
            unit: None,
            usd_twin: Some("vega_usd".into()),
        }),
        diff: None,
        paint: Paint::Heat,
        axes: vec![
            AxisInfo {
                name: "tenor".into(),
                kind: AxisKind::Bucket,
                order: AxisOrder::Tenor,
                ty: ColumnType::Utf8,
            },
            AxisInfo {
                name: "moneyness".into(),
                kind: AxisKind::Bucket,
                order: AxisOrder::Lexical,
                ty: ColumnType::F64,
            },
        ],
    }
}

fn tenors() -> Vec<String> {
    (1..=N).map(|m| format!("{m}M")).collect()
}

fn buckets() -> Vec<String> {
    (0..N)
        .map(|i| format!("{:.1}", 60.0 + i as f64 * 2.0))
        .collect()
}

fn provenance() -> Provenance {
    Provenance {
        datasets: vec![Freshness {
            dataset: "vega_buckets".into(),
            as_of: None,
            generation: Some(7),
        }],
        as_of_request: None,
    }
}

/// Leaves for every (tenor, bucket), a subtotal per tenor, one total.
fn main_snapshot() -> Snapshot {
    let tenors = tenors();
    let buckets = buckets();
    let (mut t, mut b, mut d, mut v, mut u) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let leak = |s: &String| -> &'static str { Box::leak(s.clone().into_boxed_str()) };
    for (i, tenor) in tenors.iter().enumerate() {
        for (j, bucket) in buckets.iter().enumerate() {
            t.push(Some(leak(tenor)));
            b.push(Some(leak(bucket)));
            d.push(2);
            let x = ((i * 7 + j * 3) % 23) as f64 - 11.0;
            v.push(Some(x));
            u.push(Some(x * 1.1));
        }
        t.push(Some(leak(tenor)));
        b.push(None);
        d.push(1);
        v.push(Some(i as f64));
        u.push(Some(i as f64 * 1.1));
    }
    t.push(None);
    b.push(None);
    d.push(0);
    v.push(Some(1.0));
    u.push(Some(1.1));
    Snapshot::for_tests_with_provenance(
        vec![
            (meta("tenor", 2, false), TestColumn::Str(t)),
            (meta("moneyness", 2, false), TestColumn::Str(b)),
            (meta("row_depth", 2, false), TestColumn::I32(d)),
            (meta("vega", 2, true), TestColumn::F64(v)),
            (meta("vega_usd", 2, true), TestColumn::F64(u)),
        ],
        2,
        provenance(),
    )
}

fn totals_snapshot() -> Snapshot {
    let buckets = buckets();
    let leak = |s: &String| -> &'static str { Box::leak(s.clone().into_boxed_str()) };
    let mut b: Vec<Option<&'static str>> = buckets.iter().map(|x| Some(leak(x))).collect();
    let mut d = vec![1; N];
    let mut v: Vec<Option<f64>> = (0..N).map(|j| Some(j as f64)).collect();
    b.push(None);
    d.push(0);
    v.push(Some(1.0));
    let u = v.iter().map(|x| x.map(|x| x * 1.1)).collect();
    Snapshot::for_tests_with_provenance(
        vec![
            (meta("moneyness", 1, false), TestColumn::Str(b)),
            (meta("row_depth", 1, false), TestColumn::I32(d)),
            (meta("vega", 1, true), TestColumn::F64(v)),
            (meta("vega_usd", 1, true), TestColumn::F64(u)),
        ],
        1,
        provenance(),
    )
}

fn domains() -> AxisDomains {
    let mut domains = AxisDomains::default();
    domains.insert("tenor", tenors());
    domains.insert("moneyness", buckets());
    domains
}

const USD: Mode = Mode {
    currency: Currency::Usd,
    diff: false,
};

fn build(c: &mut Criterion) {
    let shape = shape();
    let main = main_snapshot();
    let totals = totals_snapshot();
    let domains = domains();
    let mode = USD;
    c.bench_function("pivot_build_40x40", |bch| {
        bch.iter(|| {
            let p = Pivot::build(
                black_box(&main),
                Some(black_box(&totals)),
                &shape,
                mode,
                &domains,
            )
            .unwrap();
            black_box(p.heat_domains)
        })
    });
}

/// Preparing a 40-row window of the 40 × 40 matrix with its Σ edges:
/// text, sign, heat fill and readable color per cell, over a built pivot
/// (the delegate's `fill_window`, from an empty cache as after a new
/// picture). Together with `pivot_build_40x40` this is one delivery's
/// pure-UI cost; the paint itself (`to_hsla`, elements) is not measured.
fn prepare_window(c: &mut Criterion) {
    let shape = shape();
    let main = main_snapshot();
    let totals = totals_snapshot();
    let pivot = Pivot::build(&main, Some(&totals), &shape, USD, &domains()).unwrap();
    let g = |v: f32| Rgb { r: v, g: v, b: v };
    let palette = Palette {
        bullish: Rgb {
            r: 0.1,
            g: 0.8,
            b: 0.3,
        },
        bearish: Rgb {
            r: 0.9,
            g: 0.2,
            b: 0.2,
        },
        background: g(0.1),
        foreground: g(0.95),
        muted: g(0.6),
    };
    let inputs = Inputs {
        pivot: &pivot,
        palette: &palette,
        scale: None,
        heat: true,
    };
    let layout = Layout::of(&pivot);
    assert_eq!((layout.rows(), layout.cols()), (N + 1, N + 2));
    c.bench_function("prepare_window_40x40", |bch| {
        bch.iter(|| {
            let mut cache: WindowCache<PreparedCell> = WindowCache::default();
            cache.set_window(0..N, layout.cols(), |r, col| {
                prepare::cell(black_box(&inputs), r, col)
            });
            black_box(cache.get(N - 1, N + 1).map(|c| c.text.len()))
        })
    });
}

criterion_group!(benches, build, prepare_window);
criterion_main!(benches);
