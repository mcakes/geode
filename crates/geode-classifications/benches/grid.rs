//! The Classifications tile's rebuild after a label edit, over 5,000 source
//! values: what one keystroke costs on the UI thread before paint. The edit
//! records its undo entry and produces the next object (`History::apply`),
//! renders it for the config door (`classification::to_toml`), joins it
//! with the observed values (`classification::rows`), and re-orders and
//! re-filters the grid with a sort and a filter active
//! (`GridModel::relabelled`). Table preparation and painting are excluded.
//! The budget and reference measurements are in
//! `docs/current/performance.md`.

use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use geode_classifications::core::grid::GridModel;
use geode_classifications::core::history::History;
use geode_classifications::core::session::SortCol;
use geode_core::classification;
use geode_core::dimensions::DerivedDimension;

const VALUES: usize = 5_000;

/// `VALUES` observed source values, the first half labelled across twelve
/// labels, the rest unclassified.
fn shape() -> (DerivedDimension, Vec<(String, u64)>) {
    let observed: Vec<(String, u64)> = (0..VALUES)
        .map(|i| (format!("SRC{i:05}"), (i as u64 * 7919) % 1000))
        .collect();
    let values: BTreeMap<String, String> = observed
        .iter()
        .take(VALUES / 2)
        .enumerate()
        .map(|(i, (s, _))| (s.clone(), format!("Label{:02}", i % 12)))
        .collect();
    let dim = DerivedDimension {
        name: "region".into(),
        from: "underlying_ref".into(),
        values,
    };
    (dim, observed)
}

fn rebuild_after_edit(c: &mut Criterion) {
    let (dim, observed) = shape();
    let mut grid = GridModel::new();
    grid.set_sort(Some((SortCol::Rows, true)));
    grid.set_filter("src1");
    grid.set_rows(classification::rows(&dim, &observed));
    let target = vec!["SRC04999".to_string()];
    c.bench_function("classifications_rebuild_after_edit_5k", |b| {
        b.iter(|| {
            // A fresh history each time: every iteration is one first edit.
            let mut history = History::default();
            let next = history
                .apply(&dim, &target, Some("Label05"))
                .expect("the edit changes a row");
            let value = classification::to_toml(&next);
            let rows = classification::rows(&next, &observed);
            grid.relabelled(rows);
            black_box((value, grid.visible().len()));
        })
    });
}

/// The rows-only rebuild a values answer or a reload costs, without the
/// edit: the part that scales with the grid.
fn rows_and_grid(c: &mut Criterion) {
    let (dim, observed) = shape();
    let mut grid = GridModel::new();
    grid.set_sort(Some((SortCol::Rows, true)));
    grid.set_filter("src1");
    c.bench_function("classifications_rows_and_grid_5k", |b| {
        b.iter(|| {
            grid.set_rows(classification::rows(&dim, &observed));
            black_box(grid.visible().len());
        })
    });
}

criterion_group!(benches, rebuild_after_edit, rows_and_grid);
criterion_main!(benches);
