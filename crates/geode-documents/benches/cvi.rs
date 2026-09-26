//! Parse and write benchmarks for 20 × 30 (600-row) and 200 × 300 (60,000-row)
//! CVI grids. Each iteration processes one document. The larger fixture exposes
//! per-element scaling relative to fixed document overhead.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::document::{Column, DocumentKind, DocumentRows, Value};
use geode_documents::CviKind;
use std::hint::black_box;

/// `terms × nodes`, term-major, in the same vocabulary the unit tests'
/// `expected()` uses — so the bench measures the real shape rather than
/// a second model of it.
fn grid(terms: usize, nodes: usize) -> DocumentRows {
    let base = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let node_values: Vec<f64> = (0..nodes).map(|i| -30.0 + i as f64 * 0.37).collect();
    let mut term_col = Vec::with_capacity(terms * nodes);
    let mut node_col = Vec::with_capacity(terms * nodes);
    let mut params = Vec::with_capacity(terms * nodes);
    let mut forward = Vec::with_capacity(terms * nodes);
    let mut atm = Vec::with_capacity(terms * nodes);
    let mut skew = Vec::with_capacity(terms * nodes);
    for t in 0..terms {
        let term = base + chrono::Days::new(t as u64 * 7);
        for (i, n) in node_values.iter().enumerate() {
            term_col.push(term);
            node_col.push(*n);
            params.push((t * nodes + i) as f64 * 0.001 - 1.0);
            // The per-slice values: constant across a term's nodes.
            forward.push(7650.0 * (1.0 + t as f64 * 0.001));
            atm.push(0.18 + t as f64 * 0.002);
            skew.push(-1.2 + t as f64 * 0.01);
        }
    }
    DocumentRows {
        key: vec!["SPX.Z".into()],
        attributes: vec![
            ("anchor_date".into(), Value::Date(base)),
            ("spot_ref".into(), Value::F64(7650.0)),
        ],
        axes: vec![
            ("term".into(), Column::Date(term_col)),
            ("node".into(), Column::F64(node_col)),
        ],
        values: vec![
            ("param".into(), Column::F64(params)),
            ("forward".into(), Column::F64(forward)),
            ("atm".into(), Column::F64(atm)),
            ("skew".into(), Column::F64(skew)),
        ],
    }
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("cvi");
    for (terms, nodes) in [(20usize, 30usize), (200, 300)] {
        let rows = grid(terms, nodes);
        let bytes = CviKind.write(&rows).expect("the grid is full");
        g.bench_function(format!("write/{terms}x{nodes}"), |b| {
            b.iter(|| black_box(CviKind.write(black_box(&rows)).unwrap()))
        });
        g.bench_function(format!("parse/{terms}x{nodes}"), |b| {
            b.iter(|| black_box(CviKind.parse(black_box(&bytes)).unwrap()))
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
