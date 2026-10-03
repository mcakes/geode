//! Slice-model preparation: reading a batch's answers into the xy model
//! the chart paints. The batch is answered once, outside the timed loop;
//! only `model` is measured, the work a repaint does on the UI thread
//! when an outcome lands. Target: under 1 ms.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use chrono::{Months, NaiveDate};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_chart::core::palette::HuePalette;
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::link::DraftMark;
use geode_core::query::QueryKey;
use geode_core::vol::VolSliceOutcome;
use geode_data::vol::{VolConfig, evaluate};
use geode_volslice::core::build::{batch, model};
use geode_volslice::core::docs::ChainExpiry;
use geode_volslice::core::model::{Kind, Loaded, Pair, State, strip};

const EXPIRIES: u32 = 12;
const STRIKES: usize = 60;

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 2).unwrap()
}

/// Monthly expiries on the 16th, the first a fortnight out.
fn terms() -> Vec<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    (0..EXPIRIES)
        .map(|m| first.checked_add_months(Months::new(m)).unwrap())
        .collect()
}

/// A CVI document with a five-node ladder per term; `bump` lifts every atm.
fn cvi(terms: &[NaiveDate], bump: f64) -> Arc<DocumentRows> {
    let nodes = [-20.0, -10.0, 0.0, 10.0, 20.0];
    let (mut t, mut n, mut param, mut fwd, mut atm, mut skew) =
        (vec![], vec![], vec![], vec![], vec![], vec![]);
    for term in terms {
        for node in nodes {
            t.push(*term);
            n.push(node);
            param.push(0.0);
            fwd.push(100.0);
            atm.push(0.2 + bump);
            skew.push(-0.1);
        }
    }
    Arc::new(DocumentRows {
        key: vec!["SPX.Z".into()],
        attributes: vec![
            ("anchor_date".into(), Value::Date(today())),
            ("spot_ref".into(), Value::F64(100.0)),
        ],
        axes: vec![
            ("term".into(), Column::Date(t)),
            ("node".into(), Column::F64(n)),
        ],
        values: vec![
            ("param".into(), Column::F64(param)),
            ("forward".into(), Column::F64(fwd)),
            ("atm".into(), Column::F64(atm)),
            ("skew".into(), Column::F64(skew)),
        ],
    })
}

fn chain(expiry: NaiveDate) -> ChainExpiry {
    let strikes: Vec<f64> = (0..STRIKES).map(|i| 70.0 + i as f64).collect();
    let mid: Vec<f64> = strikes
        .iter()
        .map(|k| 0.2 - 0.1 * (k / 100.0).ln())
        .collect();
    ChainExpiry {
        expiry,
        as_of: today(),
        forward: 100.0,
        bid: mid.iter().map(|v| v - 0.005).collect(),
        ask: mid.iter().map(|v| v + 0.005).collect(),
        mid,
        strikes,
    }
}

fn model_build(c: &mut Criterion) {
    let terms = terms();
    let loaded = Loaded {
        cvi: Some(cvi(&terms, 0.0)),
        draft: Some((cvi(&terms, 0.01), DraftMark::Editing)),
        chain: terms.iter().map(|t| chain(*t)).collect(),
    };
    let rows = strip(&loaded, today());
    let mut state = State {
        active: Some(terms.iter().copied().collect()),
        density: true,
        diffs: vec![
            Pair::new(Kind::Draft, Kind::Chain).unwrap(),
            Pair::new(Kind::Cvi, Kind::Draft).unwrap(),
        ],
        ..State::default()
    };
    state.reconcile(&rows);
    let plan = batch(&state, &loaded, &rows);
    let params = plan.params(QueryKey(1), 1, Instant::now());
    let config = VolConfig::with(Arc::new(geode_pricing::DemoVolModel));
    let outcome = VolSliceOutcome {
        key: params.key,
        tag: 1,
        submitted: params.submitted,
        results: evaluate(&config, &params),
    };
    assert!(
        outcome.results.iter().all(Result::is_ok),
        "the bench measures a whole answer"
    );
    let shade = |l| gpui::hsla(0.0, 0.0, l, 1.0);
    let palette = HuePalette::from_theme(
        [shade(0.1), shade(0.2), shade(0.3), shade(0.4), shade(0.5)],
        shade(1.0),
        shade(0.0),
    );

    let mut group = c.benchmark_group("volslice_model_build");
    group.bench_function("model_twelve_expiries_three_kinds", |b| {
        b.iter(|| {
            black_box(model(
                black_box(&plan),
                black_box(&outcome),
                &loaded,
                &palette,
                state.split,
                1,
            ))
        })
    });
    group.finish();
}

criterion_group!(benches, model_build);
criterion_main!(benches);
