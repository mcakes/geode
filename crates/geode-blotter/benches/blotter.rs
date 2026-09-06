//! The blotter's pure-core costs at the three result shapes docs/perf.md
//! records for the tree index and requery benches (133 / 135,733 /
//! 720,881 rows): flatten fully expanded, flatten fully collapsed,
//! filling a 40-row cache window, and building a 100-column plan.
//! Medians are recorded in docs/perf.md under "Phase 3c: blotter core".

use criterion::{Criterion, criterion_group, criterion_main};
use geode_blotter::core::cache::FormatCache;
use geode_blotter::core::cache::cell;
use geode_blotter::core::expansion::Expansion;
use geode_blotter::core::flatten::flatten;
use geode_blotter::core::plan::ColumnPlan;
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
use geode_core::view::{ViewColumn, ViewSpec};
use std::collections::BTreeMap;
use std::hint::black_box;

fn dim(name: &str) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![Attribution::Additive; 4],
        scope_semantics: ScopeSemantics::Direct,
    }
}

/// Same shape builder as geode-core's tree bench
/// (`crates/geode-core/benches/tree.rs::shape`), plus `measures` numeric
/// columns `m0..mN` (each `(row as f64) * 1.5`) and a view over the
/// tree dimensions grouped `lhu`/`underlying_ref`/`position_ref` with
/// every measure column selected.
fn shape(l1: usize, l2: usize, l3: usize, measures: usize) -> (Snapshot, ViewSpec) {
    let mut lhu: Vec<Option<String>> = vec![None];
    let mut und: Vec<Option<String>> = vec![None];
    let mut pos: Vec<Option<String>> = vec![None];
    let mut depth: Vec<i32> = vec![0];
    for a in 0..l1 {
        lhu.push(Some(format!("L{a}")));
        und.push(None);
        pos.push(None);
        depth.push(1);
    }
    for b in 0..l2 {
        for a in 0..l1 {
            lhu.push(Some(format!("L{a}")));
            und.push(Some(format!("U{b}")));
            pos.push(None);
            depth.push(2);
        }
    }
    for c in 0..l3 {
        for b in 0..l2 {
            for a in 0..l1 {
                lhu.push(Some(format!("L{a}")));
                und.push(Some(format!("U{b}")));
                pos.push(Some(format!("P{a}_{b}_{c}")));
                depth.push(3);
            }
        }
    }
    let rows = lhu.len();
    let mut columns = vec![
        (dim("lhu"), TestColumn::Dict(lhu)),
        (dim("underlying_ref"), TestColumn::Dict(und)),
        (dim("position_ref"), TestColumn::Dict(pos)),
        (dim("row_depth"), TestColumn::I32(depth)),
    ];
    let mut view_columns = Vec::with_capacity(measures);
    for m in 0..measures {
        let name = format!("m{m}");
        let values: Vec<Option<f64>> = (0..rows).map(|r| Some(r as f64 * 1.5)).collect();
        columns.push((dim(&name), TestColumn::F64(values)));
        view_columns.push(ViewColumn::Measure { name });
    }
    let snapshot = Snapshot::for_tests(columns, 3);
    let view = ViewSpec {
        name: "bench".into(),
        dataset: "d".into(),
        joins: Vec::new(),
        columns: view_columns,
        grouping: vec!["lhu".into(), "underlying_ref".into(), "position_ref".into()],
        sort: Vec::new(),
        presentation: BTreeMap::new(),
    };
    (snapshot, view)
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("blotter_core");
    g.sample_size(10);
    for (name, l1, l2, l3) in [
        ("133_rows", 12, 10, 0),
        ("137k_rows", 12, 10, 1_130),
        ("729k_rows", 80, 10, 900),
    ] {
        let (snap, view) = shape(l1, l2, l3, 6);
        let plan = ColumnPlan::build(&view, snap.grouping(), &snap);
        let mut open = Expansion::default();
        open.open_all();
        let mut out = Vec::with_capacity(snap.rows());
        g.bench_function(format!("flatten_all_{name}"), |b| {
            b.iter(|| {
                flatten(&snap, &plan, &open, None, &mut out);
                black_box(out.len())
            })
        });
        g.bench_function(format!("flatten_collapsed_{name}"), |b| {
            b.iter(|| {
                flatten(&snap, &plan, &Expansion::default(), None, &mut out);
                black_box(out.len())
            })
        });
        flatten(&snap, &plan, &open, None, &mut out);
        let shown = out.clone();
        g.bench_function(
            format!("cache_fill_40x{}_{name}", plan.columns.len()),
            |b| {
                b.iter(|| {
                    let mut cache = FormatCache::default();
                    cache.set_window(0..40, plan.columns.len(), |r, c| {
                        cell(&snap, &plan, shown[r] as usize, c)
                    });
                    black_box(cache.window().len())
                })
            },
        );
    }
    let (snap, view) = shape(12, 10, 0, 100);
    g.bench_function("plan_build_100_columns", |b| {
        b.iter(|| black_box(ColumnPlan::build(&view, snap.grouping(), &snap)))
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
