//! The prepared model over a synthetic 25-member board under the builtin
//! vol tab: every cell's text, glyph core, id and width, as the tile
//! rebuilds it when a board lands or the view, lens or paint changes;
//! flat, and under five `region` groups of five members (30 rows). Runs
//! on the UI thread; painting is excluded. Reference measurements are in
//! `docs/current/performance.md`.

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Duration;

use chrono::{TimeZone as _, Utc};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::volboard::{Cell, Change, Lookback, VolBoard, VolRow};
use geode_volwatch::core::grouping::group_rows;
use geode_volwatch::core::lens::LensState;
use geode_volwatch::core::model::{Inputs, prepare, rows};
use geode_volwatch::core::views::{ask, builtin, columns, order_tenors};

fn bench_prepare(c: &mut Criterion) {
    let tenors = order_tenors(None);
    let views = builtin();
    let vol = views.get("vol").unwrap();
    let lens = LensState::default();
    let asked = ask(vol, &lens, &tenors, false);
    let cols = columns(vol, &lens, true);
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
                            stats: None,
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
    let inputs = |rows| Inputs {
        tile: 1,
        rows,
        columns: &cols,
        asked: &asked,
        board_lookback: Lookback::D1,
        board_window: Some(lens.window.duration()),
        lens,
        cells: vol.cells,
        paint: true,
        stale_after: Duration::from_secs(3 * 86_400),
        now,
        mono_advance_px: 7.0,
    };
    // The name predates the configured tabs; the vol tab lays out 19
    // columns (Underlying, eight level-and-companion pairs, a spark, vega).
    let flat = inputs(&rows);
    c.bench_function("prepare_25x24", |b| b.iter(|| prepare(black_box(&flat))));

    // The same members under five `region` groups of five: 30 rows, the
    // group rows' means built once by `group_rows`, outside the timing, as
    // the tile builds them once per regroup.
    let grouped = group_rows(rows.clone(), &["region".into()], &regions(&members), &asked);
    assert_eq!(grouped.len(), 30);
    let grouped = inputs(&grouped);
    c.bench_function("prepare_25x24_grouped", |b| {
        b.iter(|| prepare(black_box(&grouped)))
    });
}

/// A `region` classification over `underlying_ref` mapping `members`
/// into five labels of five, as a dimensions document would.
fn regions(members: &[String]) -> DerivedDimensions {
    let mut toml = String::from("[region]\nfrom = \"underlying_ref\"\n[region.values]\n");
    for (i, chunk) in members.chunks(5).enumerate() {
        let names: Vec<String> = chunk.iter().map(|n| format!("\"{n}\"")).collect();
        toml.push_str(&format!("R{i} = [{}]\n", names.join(", ")));
    }
    let doc = merge_docs(
        "dimensions",
        &[LayerDoc::builtin("dimensions", &toml).expect("the bench's dimensions parse")],
    );
    let (dims, diags) = DerivedDimensions::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    dims
}

criterion_group!(benches, bench_prepare);
criterion_main!(benches);
