//! `TreeIndex::build` at the three result shapes `docs/perf.md` records
//! for the blotter's tree view: 133 rows (bounded to depth 2), 136,868
//! (scoped to three books, all depths) and 729,466 (unscoped). The build
//! runs on the query worker, so this is what §7.1's handoff pays.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
use geode_core::tree::TreeIndex;
use std::hint::black_box;

fn dim(name: &str) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![Attribution::Additive; 4],
        scope_semantics: ScopeSemantics::Direct,
    }
}

/// A three-level tree with `l1` first-level nodes, `l2` under each, and
/// `l3` under each of those, rows emitted depth-first by level (the
/// compiler's order) with siblings interleaved as a declared sort would.
fn shape(l1: usize, l2: usize, l3: usize) -> Snapshot {
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
    Snapshot::for_tests(
        vec![
            (dim("lhu"), TestColumn::Dict(lhu)),
            (dim("underlying_ref"), TestColumn::Dict(und)),
            (dim("position_ref"), TestColumn::Dict(pos)),
            (dim("row_depth"), TestColumn::I32(depth)),
        ],
        3,
    )
}

fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_index_build");
    group.sample_size(10);
    for (name, l1, l2, l3) in [
        ("133_rows", 12, 10, 0),
        ("137k_rows", 12, 10, 1_130),
        ("729k_rows", 80, 10, 900),
    ] {
        let snap = shape(l1, l2, l3);
        eprintln!("[{name}] {} rows", snap.rows());
        group.bench_function(name, |b| b.iter(|| black_box(TreeIndex::build(&snap))));
    }
    group.finish();
}

criterion_group!(benches, bench_build);
criterion_main!(benches);
