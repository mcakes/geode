//! The Watchlist tile's rebuild after a member edit, over 5,000 names: what
//! one keystroke costs on the UI thread before paint. The edit's pending
//! definition is re-derived over the snapshot's members
//! (`rows::rows` re-runs `resolve_members` over the rule names), each row
//! takes its reference name, and the grid re-orders and re-filters with a
//! sort and a filter active (`GridModel::after_verb`). Table preparation
//! and painting are excluded. The budget and reference measurements are in
//! `docs/current/performance.md`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::query::ReferenceTable;
use geode_core::reference::ReferenceData;
use geode_core::watchlist::members::{Member, Origin};
use geode_core::watchlist::state::{Status, WatchlistState};
use geode_core::watchlist::{Rule, Watchlist};
use geode_watchlist::core::grid::GridModel;
use geode_watchlist::core::rows;
use geode_watchlist::core::session::SortCol;

const NAMES: usize = 5_000;

/// `NAMES` members from three rules (every name from rule 1, every third
/// from rule 2, every fifth from rule 3), a hundred manual includes among
/// them, fifty exclusions, and a reference table naming every one.
fn shape() -> (WatchlistState, ReferenceData) {
    let name = |i: usize| format!("U{i:05}");
    let include: Vec<String> = (0..NAMES).step_by(50).map(name).collect();
    let exclude: Vec<String> = (0..NAMES).step_by(100).map(|i| name(i + 7)).collect();
    let members = (0..NAMES)
        .map(|i| {
            let n = name(i);
            let mut rules = vec![0];
            if i % 3 == 0 {
                rules.push(1);
            }
            if i % 5 == 0 {
                rules.push(2);
            }
            let manual = include.contains(&n);
            let origin = if exclude.contains(&n) {
                Origin::Excluded { rules, manual }
            } else if manual {
                Origin::Both(rules)
            } else {
                Origin::Rules(rules)
            };
            Member { name: n, origin }
        })
        .collect();
    let state = WatchlistState {
        definition: Watchlist {
            include,
            exclude,
            rules: vec![Rule::default(), Rule::default(), Rule::default()],
        },
        layer: None,
        shadowed: None,
        rule_errors: vec![],
        members,
        resolved_at: None,
        status: Status::Current,
    };
    let table = ReferenceTable {
        columns: vec!["underlying_ref".into(), "name".into()],
        rows: (0..NAMES)
            .map(|i| vec![Some(name(i)), Some(format!("Underlying {i}"))])
            .collect(),
        gen_id: 1,
        source_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
    };
    let reference = ReferenceData::default()
        .with_table(rows::REFERENCE_DATASET, &table, 1)
        .expect("a fresh table");
    (state, reference)
}

/// One manual add while the origin sort and a `/` filter are active: the
/// pending definition re-derived over the members, the reference names
/// joined, and the grid rebuilt with the cursor keeping its index.
fn rebuild_after_edit(c: &mut Criterion) {
    let (state, reference) = shape();
    let mut grid = GridModel::new();
    grid.set_sort(Some((SortCol::Origin, true)));
    grid.set_filter("u1");
    grid.set_rows(rows::rows(&state, None, &reference));
    let mut pending = state.definition.clone();
    pending.include.push("NEW".into());
    c.bench_function("watchlist_rebuild_after_edit_5k", |b| {
        b.iter(|| {
            let rows = rows::rows(&state, Some(&pending), &reference);
            grid.after_verb(rows);
            black_box(grid.visible().len());
        })
    });
}

/// The rows-only rebuild a new snapshot or reference table costs, without
/// a pending edit: the part that scales with the grid.
fn rows_and_grid(c: &mut Criterion) {
    let (state, reference) = shape();
    let mut grid = GridModel::new();
    grid.set_sort(Some((SortCol::Origin, true)));
    grid.set_filter("u1");
    c.bench_function("watchlist_rows_and_grid_5k", |b| {
        b.iter(|| {
            grid.set_rows(rows::rows(&state, None, &reference));
            black_box(grid.visible().len());
        })
    });
}

criterion_group!(benches, rebuild_after_edit, rows_and_grid);
criterion_main!(benches);
