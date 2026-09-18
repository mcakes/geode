//! The panel's pure-core costs at the two shapes market-data spec §11
//! names: the CVI sketch (20 terms × 30 nodes, pivoted) and a broad-index
//! dividend schedule (10,000 rows × 5 value columns, flat) — the shape
//! that forced roadmap ruling 6's revision to a `uniform_list`. Plus
//! `Draft::rebase` over 1,000 edits, the cost a trader pays on `:rebase`
//! after a republish under a large draft.
//!
//! What these numbers are for: the model is built once per delivery or
//! per edit, never per frame, so this is the budget spent on the UI thread
//! between a snapshot arriving and the frame that shows it — it has to fit
//! inside spec §7.1's 50 ms requery alongside the paint. Medians are
//! recorded in docs/perf.md under "Market-data documents".

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::schema::ColumnType;
use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
use geode_core::view::ColumnFormat;
use geode_marketdata::core::draft::Draft;
use geode_marketdata::core::matrix::MatrixModel;
use geode_marketdata::core::spec::{CVI, Columns, HeaderAttr, PanelSpec};
use std::hint::black_box;

const BASE: &str = "2026-09-12T14:00:00Z";

fn meta(name: &str, attribution: Attribution) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![attribution],
        scope_semantics: ScopeSemantics::Direct,
    }
}

fn provenance() -> Provenance {
    Provenance {
        datasets: vec![Freshness {
            dataset: "cvi_params".into(),
            as_of: Some(BASE.into()),
            generation: 7,
        }],
        as_of_request: None,
    }
}

/// A CVI document of `terms` × `nodes`, in the axis order
/// `compile_document` selects.
fn cvi(terms: usize, nodes: usize) -> Snapshot {
    let rows = terms * nodes;
    let mut term_col = Vec::with_capacity(rows);
    let mut node_col = Vec::with_capacity(rows);
    let mut param = Vec::with_capacity(rows);
    // The per-slice values (2026-09-17), repeated on every node row of
    // their term — the long form the kind stores them in.
    let mut forward = Vec::with_capacity(rows);
    let mut atm = Vec::with_capacity(rows);
    let mut skew = Vec::with_capacity(rows);
    for t in 0..terms {
        // Distinct terms past twelve months too: the pivot keys on the
        // label, so a repeated term would be a duplicate-pair refusal
        // rather than a bigger grid.
        let term = format!("20{:02}-{:02}-16", 26 + t / 12, t % 12 + 1);
        for n in 0..nodes {
            term_col.push(Some(term.clone()));
            node_col.push(Some(n as f64 / 4.0 - 20.0));
            param.push(Some((t * nodes + n) as f64 / 8.0));
            forward.push(Some(4500.0 + 10.0 * t as f64));
            atm.push(Some(0.18 + 0.001 * t as f64));
            skew.push(Some(-1.0 - 0.01 * t as f64));
        }
    }
    Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into()); rows]),
            ),
            (
                meta("term", Attribution::Additive),
                TestColumn::Dict(term_col),
            ),
            (
                meta("node", Attribution::Additive),
                TestColumn::F64(node_col),
            ),
            (
                meta("param", Attribution::DeterminedNonAdditive),
                TestColumn::F64(param),
            ),
            (
                meta("forward", Attribution::DeterminedNonAdditive),
                TestColumn::F64(forward),
            ),
            (
                meta("atm", Attribution::DeterminedNonAdditive),
                TestColumn::F64(atm),
            ),
            (
                meta("skew", Attribution::DeterminedNonAdditive),
                TestColumn::F64(skew),
            ),
            (
                meta("anchor_date", Attribution::Additive),
                TestColumn::Dict(vec![Some("2026-09-12".into()); rows]),
            ),
            (
                meta("spot_ref", Attribution::Additive),
                TestColumn::F64(vec![Some(5000.0); rows]),
            ),
        ],
        0,
        provenance(),
    )
}

const SCHEDULE: PanelSpec = PanelSpec {
    kind: "sched",
    title: "Dividends",
    dataset: "div_schedule",
    document: "div_schedule",
    rows: "ex_date",
    columns: Columns::Values,
    header: &[HeaderAttr {
        column: "currency",
        label: "currency",
        ty: ColumnType::Utf8,
    }],
    slice_values: &[],
    value_type: ColumnType::F64,
    format: ColumnFormat::MEASURE,
    actions: &[],
};

/// A dividend schedule: `rows` dated rows, five value columns each.
///
/// The dates are strictly increasing on a 28-day-month calendar, so every
/// row label is distinct: a row label identifies an edit, so
/// `MatrixModel::build` refuses a repeat outright — and a fixture whose
/// dates cycled would measure a refusal rather than a build (and, before
/// that refusal existed, a `rebase` that quietly collapsed most of its
/// 1,000 edits onto the last row sharing each label).
fn schedule(rows: usize) -> Snapshot {
    let dates: Vec<Option<String>> = (0..rows)
        .map(|i| {
            Some(format!(
                "{:04}-{:02}-{:02}",
                2026 + i / 336,
                i / 28 % 12 + 1,
                i % 28 + 1
            ))
        })
        .collect();
    let mut columns = vec![
        (
            meta("underlying_ref", Attribution::Additive),
            TestColumn::Dict(vec![Some("SPX.Z".into()); rows]),
        ),
        (
            meta("ex_date", Attribution::Additive),
            TestColumn::Dict(dates),
        ),
    ];
    for v in 0..5 {
        columns.push((
            meta(&format!("v{v}"), Attribution::DeterminedNonAdditive),
            TestColumn::F64((0..rows).map(|r| Some((r + v) as f64 / 16.0)).collect()),
        ));
    }
    columns.push((
        meta("currency", Attribution::Additive),
        TestColumn::Dict(vec![Some("USD".into()); rows]),
    ));
    Snapshot::for_tests_with_provenance(columns, 0, provenance())
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("marketdata_core");

    let sketch = cvi(20, 30);
    let clean = Draft::default();
    g.bench_function("model_build_pivot_20x30", |b| {
        b.iter(|| black_box(MatrixModel::build(&sketch, &CVI, &clean).expect("a full grid")))
    });

    let flat = schedule(10_000);
    g.bench_function("model_build_values_10000x5", |b| {
        b.iter(|| black_box(MatrixModel::build(&flat, &SCHEDULE, &clean).expect("a flat document")))
    });

    // 1,000 edits over the flat shape, then rebased onto the same labels.
    // The cells are 1,000 distinct rows of the schedule, whose labels are
    // distinct by construction, so every edit resolves and nothing is
    // dropped — the full-cost path. `rebase` is run on one draft
    // repeatedly rather than on a fresh clone per iteration: it is
    // idempotent (the same labels resolve to the same cells every time),
    // so this measures the rebase and not a clone of a 1,000-entry map.
    let model = MatrixModel::build(&flat, &SCHEDULE, &clean).expect("a flat document");
    let mut draft = Draft::default();
    for i in 0..1_000 {
        let cell = (i % model.rows.len(), i % model.columns.len());
        let labels = model.label_of(cell);
        draft.set(
            cell,
            (labels.0.to_string(), labels.1.to_string()),
            i as f64,
            BASE,
        );
    }
    g.bench_function("draft_rebase_1000_edits", |b| {
        b.iter(|| black_box(draft.rebase(&model)))
    });

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
