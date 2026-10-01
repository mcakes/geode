//! Pure model costs for a CVI pivot (20 terms × 30 nodes) and a flat
//! schedule (10,000 rows × five value columns).
//!
//! Measure full builds, individual cell patches, builds with 100 inserted
//! rows, and rebase with 1,000 cell edits plus optional row inserts. Cell
//! commits patch prepared data; deliveries and structural edits rebuild it.
//! These operations spend UI-thread time before painting; current budgets
//! and measurement conditions are documented in `docs/current/performance.md`.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::document::Value;
use geode_core::schema::ColumnType;
use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
use geode_core::view::ColumnFormat;
use geode_marketdata::core::draft::{DocumentBase, Draft};
use geode_marketdata::core::matrix::MatrixModel;
use geode_marketdata::core::spec::{
    Columns, HeaderAttr, PanelSpec, RowAxis, RowIdentity, RowLabel, ValueColumn, builtin_panel,
};
use std::hint::black_box;
use std::sync::{Arc, LazyLock};

const BASE: &str = "2026-09-12T14:00:00Z";

/// The base every edit in these benches is stamped against — one
/// generation, so no measurement includes a `Behind` transition.
fn base() -> DocumentBase {
    DocumentBase {
        as_of: BASE.to_string(),
        generation: Some(7),
    }
}

fn meta(name: &str, attribution: Attribution) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![attribution],
        scope_semantics: ScopeSemantics::Direct,
        summable: false,
        mixed_flag: None,
    }
}

fn provenance() -> Provenance {
    Provenance {
        datasets: vec![Freshness {
            dataset: "cvi_params".into(),
            as_of: Some(BASE.into()),
            generation: Some(7),
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
    // Per-slice values repeat on every node row of their term, matching
    // the document's long form.
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

/// One of the schedule's five identical `f64` value columns.
fn value_column(name: &str) -> ValueColumn {
    ValueColumn {
        column: name.into(),
        label: name.into(),
        ty: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        choices: None,
        required: true,
    }
}

static SCHEDULE: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
    Arc::new(PanelSpec {
        kind: "sched".into(),
        title: "Dividends".into(),
        dataset: "div_schedule".into(),
        document: "div_schedule".into(),
        rows: RowAxis {
            column: "ex_date".into(),
            identity: RowIdentity::Typed(ColumnType::Date),
            label: RowLabel::Shown,
        },
        columns: Columns::Values(["v0", "v1", "v2", "v3", "v4"].map(value_column).to_vec()),
        header: vec![HeaderAttr {
            column: "currency".into(),
            label: "currency".into(),
            ty: ColumnType::Utf8,
        }],
        slice_values: Vec::new(),
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        actions: Vec::new(),
    })
});

/// A schedule with five value columns and distinct dated row labels.
/// The synthetic 28-day-month calendar keeps labels unique so these benches
/// measure full builds and rebases, not duplicate-label refusals.
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
    let cvi_panel = builtin_panel("cvi");
    let mut g = c.benchmark_group("marketdata_core");

    let sketch = cvi(20, 30);
    let clean = Draft::default();
    g.bench_function("model_build_pivot_20x30", |b| {
        b.iter(|| black_box(MatrixModel::build(&sketch, &cvi_panel, &clean).expect("a full grid")))
    });

    let flat = schedule(10_000);
    g.bench_function("model_build_values_10000x5", |b| {
        b.iter(|| black_box(MatrixModel::build(&flat, &SCHEDULE, &clean).expect("a flat document")))
    });

    // What paint reads per frame today: 40 rows × five prepared texts
    // cloned out of the built model. After the windowed slice this name
    // measures a cold `WindowCache` fill of the same 40 × 5 cells.
    let built = MatrixModel::build(&flat, &SCHEDULE, &clean).expect("a flat document");
    g.bench_function("window_fill_40x5", |b| {
        b.iter(|| {
            let mut n = 0usize;
            for row in &built.rows[..40] {
                for cell in &row.cells {
                    n += black_box(cell.text.clone()).len();
                }
            }
            black_box(n)
        })
    });
    // A delivery as far as a paintable window: today the whole build.
    g.bench_function("delivery_to_window_values_10000x5", |b| {
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
            Value::F64(i as f64),
            &base(),
        );
    }
    g.bench_function("draft_rebase_1000_edits", |b| {
        b.iter(|| black_box(draft.rebase(&model)))
    });

    // One committed cell, end to end: the draft write and the re-prepare.
    let edit_cell = (5_000, 2);
    let edit_labels = model.label_of(edit_cell);
    let mut edit_draft = Draft::default();
    let mut edited = MatrixModel::build(&flat, &SCHEDULE, &edit_draft).expect("a flat document");
    let mut v = 0.0;
    g.bench_function("one_cell_edit_values_10000x5", |b| {
        b.iter(|| {
            v += 1.0;
            edit_draft.set(
                edit_cell,
                (edit_labels.0.to_string(), edit_labels.1.to_string()),
                Value::F64(v),
                &base(),
            );
            black_box(edited.patch_cell(edit_cell.0, edit_cell.1, &flat, &SCHEDULE, &edit_draft))
        })
    });
    // The 500 ms session tick's group capture: today a clean build of
    // the base, then the capture over it.
    let mut tick_draft = draft.clone();
    g.bench_function("session_tick_values_10000x5", |b| {
        b.iter(|| {
            let base_model =
                MatrixModel::build(&flat, &SCHEDULE, &Draft::default()).expect("a flat document");
            tick_draft.capture_groups(&base_model);
            black_box(&tick_draft);
        })
    });

    // Measure re-preparing one edited cell on each model shape. Reuse
    // the same cell and draft edit to isolate steady-state patch cost.
    let pivot_cell = (1, 5);
    let pivot_labels = MatrixModel::build(&sketch, &cvi_panel, &Draft::default())
        .unwrap()
        .label_of(pivot_cell);
    let mut pivot_draft = Draft::default();
    pivot_draft.set(
        pivot_cell,
        (pivot_labels.0.to_string(), pivot_labels.1.to_string()),
        Value::F64(42.0),
        &base(),
    );
    let mut pivot_patched =
        MatrixModel::build(&sketch, &cvi_panel, &pivot_draft).expect("a full grid, edit painted");
    g.bench_function("patch_cell_pivot_20x30", |b| {
        b.iter(|| {
            black_box(pivot_patched.patch_cell(
                pivot_cell.0,
                pivot_cell.1,
                &sketch,
                &cvi_panel,
                &pivot_draft,
            ))
        })
    });

    let flat_cell = (5_000, 2);
    let flat_labels = model.label_of(flat_cell);
    let mut flat_draft = Draft::default();
    flat_draft.set(
        flat_cell,
        (flat_labels.0.to_string(), flat_labels.1.to_string()),
        Value::F64(7.0),
        &base(),
    );
    let mut flat_patched =
        MatrixModel::build(&flat, &SCHEDULE, &flat_draft).expect("a flat document, edit painted");
    g.bench_function("patch_cell_values_10000x5", |b| {
        b.iter(|| {
            black_box(flat_patched.patch_cell(
                flat_cell.0,
                flat_cell.1,
                &flat,
                &SCHEDULE,
                &flat_draft,
            ))
        })
    });

    // Build with 100 inserted rows anchored every 100 document rows.
    // Structural row edits rebuild the grid, so distribute anchors to measure
    // splicing across the full schedule.
    let mut rows_draft = Draft::default();
    for i in 0..100 {
        let anchor = model.rows[i * 100].label.to_string();
        rows_draft.insert_row(format!("new-{}", i + 1), Some(anchor), &base());
    }
    g.bench_function("model_build_values_10000x5_100_rows_spliced", |b| {
        b.iter(|| {
            black_box(
                MatrixModel::build(&flat, &SCHEDULE, &rows_draft)
                    .expect("a flat document with rows spliced in"),
            )
        })
    });

    // Rebase 1,000 cell edits plus 100 inserted rows onto matching labels.
    let mut mixed_draft = Draft::default();
    for i in 0..1_000 {
        let cell = (i % model.rows.len(), i % model.columns.len());
        let labels = model.label_of(cell);
        mixed_draft.set(
            cell,
            (labels.0.to_string(), labels.1.to_string()),
            Value::F64(i as f64),
            &base(),
        );
    }
    for i in 0..100 {
        let anchor = model.rows[i * 100].label.to_string();
        mixed_draft.insert_row(format!("new-{}", i + 1), Some(anchor), &base());
    }
    g.bench_function("draft_rebase_1000_edits_100_rows", |b| {
        b.iter(|| black_box(mixed_draft.rebase(&model)))
    });

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
