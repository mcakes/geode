//! The dividend-schedule kind's parse and write costs at two schedule
//! sizes: 35 rows (a single index underlying's schedule, the shape
//! `DividendGenerator` produces for SPX/NDX/RUT) and 2,000 rows (well
//! past anything one underlying's feed sends, so the walk's per-element
//! cost is visible rather than swamped by fixed overhead). Both are one
//! document, which is the unit the receiver thread pays per message
//! (design spec §6.2) — see `cvi.rs`'s identical bench for why.

use chrono::{Days, NaiveDate};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::document::{Column, DocumentKind, DocumentRows, Value};
use geode_documents::DividendKind;
use std::hint::black_box;

/// `rows` dividends for one underlying, in the same vocabulary the unit
/// tests' `expected()` uses.
fn schedule(rows: usize) -> DocumentRows {
    let base = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let statuses = ["estimated", "declared", "paid", "cancelled"];
    let mut ids = Vec::with_capacity(rows);
    let mut ex_dates = Vec::with_capacity(rows);
    let mut announced_dates = Vec::with_capacity(rows);
    let mut pay_dates = Vec::with_capacity(rows);
    let mut amounts = Vec::with_capacity(rows);
    let mut status_col = Vec::with_capacity(rows);
    for i in 0..rows {
        ids.push(format!("D{i}"));
        let ex = base + Days::new(i as u64 * 7);
        ex_dates.push(ex);
        announced_dates.push(ex - Days::new(30));
        pay_dates.push(ex + Days::new(14));
        amounts.push(1.0 + (i % 10) as f64 * 0.05);
        status_col.push(statuses[i % statuses.len()].to_string());
    }
    DocumentRows {
        key: vec!["SPX.Z".into()],
        attributes: vec![
            ("currency".into(), Value::Utf8("USD".into())),
            ("schedule_date".into(), Value::Date(base)),
        ],
        axes: vec![("dividend_id".into(), Column::Utf8(ids))],
        values: vec![
            ("ex_date".into(), Column::Date(ex_dates)),
            ("announced_date".into(), Column::Date(announced_dates)),
            ("pay_date".into(), Column::Date(pay_dates)),
            ("amount".into(), Column::F64(amounts)),
            ("status".into(), Column::Utf8(status_col)),
        ],
    }
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("dividend");
    for rows in [35usize, 2_000] {
        let doc = schedule(rows);
        let bytes = DividendKind
            .write(&doc)
            .expect("the schedule is well formed");
        g.bench_function(format!("write/{rows}"), |b| {
            b.iter(|| black_box(DividendKind.write(black_box(&doc)).unwrap()))
        });
        g.bench_function(format!("parse/{rows}"), |b| {
            b.iter(|| black_box(DividendKind.parse(black_box(&bytes)).unwrap()))
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
