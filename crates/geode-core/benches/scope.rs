//! Measure in-process scope evaluation over one row. The pricer evaluates
//! its scope once per sheet line whenever the frame scope or a line
//! changes, so this cost multiplies by the sheet's line count.
//!
//! The fixture is a three-term expression with a text filter, over a row
//! the scope keeps, so every part is evaluated rather than short-circuited.

use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::dimensions::DerivedDimensions;
use geode_core::document::Value;
use geode_core::schema::{ColumnRole, ColumnSpec, ColumnType, DatasetSpec, Grain};
use geode_core::scope::{RowValues, Scope, parse_expr};
use std::hint::black_box;

struct Row;

impl RowValues for Row {
    fn value(&self, column: &str) -> Option<Value> {
        match column {
            "und" => Some(Value::Utf8("SPX".into())),
            "cur" => Some(Value::Utf8("USD".into())),
            "strike" => Some(Value::F64(4_500.0)),
            _ => None,
        }
    }
}

fn dataset() -> DatasetSpec {
    let col = |name: &str, ty, textual| ColumnSpec {
        name: name.into(),
        source_name: None,
        ty,
        required: false,
        textual,
        categorical: false,
        role: ColumnRole::Dimension {
            grain: Some(Grain::Underlying),
        },
    };
    DatasetSpec {
        name: "pricer".into(),
        columns: vec![
            col("und", ColumnType::Utf8, true),
            col("cur", ColumnType::Utf8, true),
            col("strike", ColumnType::F64, false),
        ],
        ..DatasetSpec::default()
    }
}

fn bench(c: &mut Criterion) {
    let ds = dataset();
    let dims = DerivedDimensions::default();
    let scope = Scope {
        text: Some("sp".into()),
        expression: Some(
            parse_expr("und = 'SPX' and strike > 100 and cur in ('USD', 'EUR')").unwrap(),
        ),
        ..Scope::default()
    };
    assert_eq!(scope.matches(&Row, &ds, &dims), Ok(true));
    c.bench_function("scope/expr_evaluate_row", |b| {
        b.iter(|| black_box(&scope).matches(black_box(&Row), &ds, &dims))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
