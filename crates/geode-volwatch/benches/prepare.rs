//! The prepared model over a synthetic 25-member Changes board: every
//! cell's text, glyph core, id and width, as the tile rebuilds it when a
//! board lands or the view, lookback or paint changes. Runs on the UI
//! thread; painting is excluded. Reference measurements are in
//! `docs/current/performance.md`.

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Duration;

use chrono::{TimeZone as _, Utc};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::volboard::{Cell, Change, Lookback, VolBoard, VolRow};
use geode_volwatch::core::model::{Inputs, prepare, rows};
use geode_volwatch::core::views::{View, ask, columns, order_tenors};

fn bench_prepare(c: &mut Criterion) {
    let tenors = order_tenors(None);
    let asked = ask(View::Changes, &tenors, Lookback::D1, false);
    let cols = columns(View::Changes, &tenors);
    let now = Utc.with_ymd_and_hms(2026, 10, 12, 16, 0, 0).unwrap();
    let members: Vec<String> = (0..25).map(|i| format!("IDX{i:02}")).collect();
    let board = VolBoard {
        rows: members
            .iter()
            .enumerate()
            .map(|(r, name)| VolRow {
                underlying: name.clone(),
                as_of: Some(now - chrono::Duration::hours(1)),
                cells: asked
                    .measures
                    .iter()
                    .enumerate()
                    .map(|(m, measure)| {
                        let v = 10.0 + (r * 7 + m * 3) as f64 % 17.0;
                        let d = ((r + m) as f64 % 9.0 - 4.0) * 0.37;
                        Cell {
                            measure: measure.clone(),
                            current: Some(v),
                            current_at: Some(now),
                            changes: vec![Some(Change {
                                value: d,
                                from: v - d,
                                at: now - chrono::Duration::days(1),
                            })],
                            pctl: Some(((r * 13 % 100) as f64 / 100.0, 251)),
                        }
                    })
                    .collect(),
                sparks: asked
                    .sparklines
                    .iter()
                    .map(|_| Some((0..30).map(|i| 15.0 + f64::from(i % 7) * 0.2).collect()))
                    .collect(),
            })
            .collect(),
        ..VolBoard::default()
    };
    let vega: HashMap<String, f64> = members
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), (i as f64 - 12.0) * 25_000.0))
        .collect();
    let rows = rows(&members, Some(&board), &|_| None, Some(&vega));
    let inputs = Inputs {
        tile: 1,
        rows: &rows,
        columns: &cols,
        asked: &asked,
        board_lookback: Lookback::D1,
        shown: Lookback::D1,
        paint: true,
        stale_after: Duration::from_secs(3 * 86_400),
        now,
        mono_advance_px: 7.0,
    };
    c.bench_function("prepare_25x24", |b| b.iter(|| prepare(black_box(&inputs))));
}

criterion_group!(benches, bench_prepare);
criterion_main!(benches);
