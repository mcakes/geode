//! Shared test fixtures for pure models and tile integration routes.
//! Both use the same typed schedule spec and snapshots so editing, painting,
//! and restoration tests cannot silently diverge in their document shape.

#![cfg(test)]

use crate::core::draft::{DocumentBase, Draft};
use crate::core::matrix::MatrixModel;
use crate::core::spec::{
    Columns, PanelSpec, RowAxis, RowIdentity, RowLabel, ValueColumn, builtin_panel,
};
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::schema::ColumnType;
use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};
use std::sync::{Arc, LazyLock};

/// The shipped panels as the builtin TOML reads, for tests that need the
/// real CVI or dividend spec.
pub(crate) static CVI: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| builtin_panel("cvi"));
pub(crate) static DIVIDEND: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| builtin_panel("dividend"));

pub(crate) const BASE: &str = "2026-09-12T14:00:00Z";

/// A draft base at `as_of` with no generation. Tests that only need an edit
/// stamped against *something* use this; a test about a same-time republish
/// names the generation itself.
pub(crate) fn at(as_of: &str) -> DocumentBase {
    DocumentBase {
        as_of: as_of.to_string(),
        generation: None,
    }
}

pub(crate) fn date(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn meta(name: &str, attribution: Attribution) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        // Document snapshots have depth 0 only, so each column has one
        // attribution entry.
        attribution_by_depth: vec![attribution],
        scope_semantics: ScopeSemantics::Direct,
        summable: false,
        mixed_flag: None,
    }
}

fn provenance(as_of: &str) -> Provenance {
    Provenance {
        datasets: vec![Freshness {
            dataset: "div_schedule".into(),
            as_of: Some(as_of.into()),
            generation: Some(7),
        }],
        as_of_request: None,
    }
}

/// `amount`'s own format: four places, no grouping — the same reasoning
/// the builtin CVI panel's `value.format` gives for a small number whose
/// fourth place is real, distinct from `ColumnFormat::MEASURE` so this
/// fixture actually exercises a column with its OWN format rather than the
/// panel's default.
const SCHEDULE_AMOUNT_FORMAT: ColumnFormat = ColumnFormat {
    precision: 4,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};

/// [`SCHEDULE`] with minted row identities hidden. This exercises table
/// columns, search, row copying, and insertion without a row-label column.
pub(crate) static HIDDEN_SCHEDULE: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
    Arc::new(PanelSpec {
        kind: "sched_hidden".into(),
        rows: RowAxis {
            column: "dividend_id".into(),
            identity: RowIdentity::Minted,
            label: RowLabel::Hidden,
        },
        ..(**SCHEDULE).clone()
    })
});

pub(crate) static SCHEDULE: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
    Arc::new(PanelSpec {
        kind: "sched".into(),
        title: "Dividends".into(),
        dataset: "div_schedule".into(),
        document: "div_schedule".into(),
        rows: RowAxis {
            column: "dividend_id".into(),
            identity: RowIdentity::Minted,
            label: RowLabel::Shown,
        },
        columns: Columns::Values(vec![
            ValueColumn {
                column: "ex_date".into(),
                label: "ex".into(),
                ty: ColumnType::Date,
                format: ColumnFormat::MEASURE,
                choices: None,
                required: true,
            },
            ValueColumn {
                column: "amount".into(),
                label: "amount".into(),
                ty: ColumnType::F64,
                format: SCHEDULE_AMOUNT_FORMAT,
                choices: None,
                required: true,
            },
            ValueColumn {
                column: "status".into(),
                label: "status".into(),
                ty: ColumnType::Utf8,
                format: ColumnFormat::MEASURE,
                choices: Some(
                    ["estimated", "declared", "paid", "cancelled"]
                        .map(String::from)
                        .into(),
                ),
                required: true,
            },
        ]),
        header: Vec::new(),
        slice_values: Vec::new(),
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        actions: Vec::new(),
    })
});

/// A [`SCHEDULE`] document: `(dividend_id, ex_date, amount, status)` per
/// row. `ex_date` is a real `Date32` column (`TestColumn::Date`) — a flat
/// panel's `ex_date` is a typed `Value::Date` CELL, not a row label
/// (unlike a pivot's axis columns, which read a document date as ISO
/// text through a dictionary), so `MatrixModel::build` must read it as
/// one.
pub(crate) fn schedule_snapshot(rows: &[(&str, &str, f64, &str)]) -> Snapshot {
    schedule_snapshot_at(rows, BASE)
}

/// [`schedule_snapshot`] stamped as a generation of the caller's choosing
/// — a second delivery of the same rows at a later `as_of` is what moves
/// a flat panel's draft to `Behind`.
pub(crate) fn schedule_snapshot_at(rows: &[(&str, &str, f64, &str)], as_of: &str) -> Snapshot {
    let n = rows.len();
    Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into()); n]),
            ),
            (
                meta("dividend_id", Attribution::Additive),
                TestColumn::Dict(rows.iter().map(|r| Some(r.0.to_string())).collect()),
            ),
            (
                meta("ex_date", Attribution::DeterminedNonAdditive),
                TestColumn::Date(
                    rows.iter()
                        .map(|r| {
                            Some(
                                chrono::NaiveDate::parse_from_str(r.1, "%Y-%m-%d")
                                    .expect("a valid fixture date"),
                            )
                        })
                        .collect(),
                ),
            ),
            (
                meta("amount", Attribution::DeterminedNonAdditive),
                TestColumn::F64(rows.iter().map(|r| Some(r.2)).collect()),
            ),
            (
                meta("status", Attribution::DeterminedNonAdditive),
                TestColumn::Dict(rows.iter().map(|r| Some(r.3.to_string())).collect()),
            ),
        ],
        0,
        provenance(as_of),
    )
}

/// One [`schedule_snapshot`] row, plus an extra value column the spec
/// does not declare — what a schema drifting under a spec looks like.
pub(crate) fn schedule_snapshot_with_extra_value(extra: &str) -> Snapshot {
    Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into())]),
            ),
            (
                meta("dividend_id", Attribution::Additive),
                TestColumn::Dict(vec![Some("D1".into())]),
            ),
            (
                meta("ex_date", Attribution::DeterminedNonAdditive),
                TestColumn::Date(vec![Some(date(2026, 12, 18))]),
            ),
            (
                meta("amount", Attribution::DeterminedNonAdditive),
                TestColumn::F64(vec![Some(1.25)]),
            ),
            (
                meta("status", Attribution::DeterminedNonAdditive),
                TestColumn::Dict(vec![Some("declared".into())]),
            ),
            (
                meta(extra, Attribution::DeterminedNonAdditive),
                TestColumn::F64(vec![Some(9.0)]),
            ),
        ],
        0,
        provenance(BASE),
    )
}

/// A one-row [`SCHEDULE`] document whose `amount` — the row's only
/// `Number`-kind column — is NULL. A row bump here has three cells and
/// zero values either way (a NULL cell has nothing to add to, `ex`/
/// `status` are not `Number`-kind), which is what makes this fixture
/// worth having: it is the one case that tells apart a `CellKind`-based
/// skip from a NULL-based one, since with both present a bump's own
/// refusal must still name the two columns skipped for their KIND, not
/// fall back to the generic "no values to bump" a mutated kind guard
/// would produce.
pub(crate) fn schedule_snapshot_with_null_amount() -> Snapshot {
    Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into())]),
            ),
            (
                meta("dividend_id", Attribution::Additive),
                TestColumn::Dict(vec![Some("D1".into())]),
            ),
            (
                meta("ex_date", Attribution::DeterminedNonAdditive),
                TestColumn::Date(vec![Some(date(2026, 12, 18))]),
            ),
            (
                meta("amount", Attribution::DeterminedNonAdditive),
                TestColumn::F64(vec![None]),
            ),
            (
                meta("status", Attribution::DeterminedNonAdditive),
                TestColumn::Dict(vec![Some("declared".into())]),
            ),
        ],
        0,
        provenance(BASE),
    )
}

/// A one-row [`SCHEDULE`] document whose `status` — its one `Choice`
/// column — is NULL: a hole the desk left, with no current option for a
/// step to start from.
pub(crate) fn schedule_snapshot_with_null_status() -> Snapshot {
    Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into())]),
            ),
            (
                meta("dividend_id", Attribution::Additive),
                TestColumn::Dict(vec![Some("D1".into())]),
            ),
            (
                meta("ex_date", Attribution::DeterminedNonAdditive),
                TestColumn::Date(vec![Some(date(2026, 12, 18))]),
            ),
            (
                meta("amount", Attribution::DeterminedNonAdditive),
                TestColumn::F64(vec![Some(1.25)]),
            ),
            (
                meta("status", Attribution::DeterminedNonAdditive),
                TestColumn::Dict(vec![None]),
            ),
        ],
        0,
        provenance(BASE),
    )
}

/// A strike ladder with integer row identities and one `F64` value column.
/// It exercises the text row-label editor and its nudge/commit paths;
/// the shipped specs use minted identities or typed dates.
pub(crate) static LADDER: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
    Arc::new(PanelSpec {
        kind: "ladder".into(),
        title: "Strike ladder".into(),
        dataset: "strike_ladder".into(),
        document: "strike_ladder".into(),
        rows: RowAxis {
            column: "strike".into(),
            identity: RowIdentity::Typed(ColumnType::I64),
            label: RowLabel::Shown,
        },
        columns: Columns::Values(vec![ValueColumn {
            column: "vol".into(),
            label: "vol".into(),
            ty: ColumnType::F64,
            format: SCHEDULE_AMOUNT_FORMAT,
            choices: None,
            required: true,
        }]),
        header: Vec::new(),
        slice_values: Vec::new(),
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        actions: Vec::new(),
    })
});

/// A [`LADDER`] document: `(strike, vol)` per row, the strike a real
/// `Int64` column so the row label is read through the integer path
/// (`label_at`'s `f64_at` fallback spells `100`, never `100.0`).
pub(crate) fn ladder_snapshot(rows: &[(i64, f64)]) -> Snapshot {
    let n = rows.len();
    Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into()); n]),
            ),
            (
                meta("strike", Attribution::Additive),
                TestColumn::I64(rows.iter().map(|r| r.0).collect()),
            ),
            (
                meta("vol", Attribution::DeterminedNonAdditive),
                TestColumn::F64(rows.iter().map(|r| Some(r.1)).collect()),
            ),
        ],
        0,
        provenance(BASE),
    )
}

/// A [`DIVIDEND`] document: `(dividend_id, ex_date,
/// announced_date, pay_date, amount, status)` per row — the shipped
/// panel's own five value columns, for the one production-route test that
/// must prove `MarketDataTile::bump`'s `column_type` reads the REAL spec's
/// `Columns::Values` branch correctly, not `SCHEDULE`'s three-column
/// stand-in.
pub(crate) fn dividend_snapshot(rows: &[(&str, &str, &str, &str, f64, &str)]) -> Snapshot {
    dividend_snapshot_at(rows, BASE)
}

/// [`dividend_snapshot`] stamped as a generation of the caller's choosing
/// — a second delivery at a later `as_of` is what moves a
/// dividend panel's draft to `Behind`, the tile-level rebase-guard test's
/// own door onto `:rebase`.
pub(crate) fn dividend_snapshot_at(
    rows: &[(&str, &str, &str, &str, f64, &str)],
    as_of: &str,
) -> Snapshot {
    let n = rows.len();
    let dt =
        |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("a valid fixture date");
    Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into()); n]),
            ),
            (
                meta("dividend_id", Attribution::Additive),
                TestColumn::Dict(rows.iter().map(|r| Some(r.0.to_string())).collect()),
            ),
            (
                meta("ex_date", Attribution::DeterminedNonAdditive),
                TestColumn::Date(rows.iter().map(|r| Some(dt(r.1))).collect()),
            ),
            (
                meta("announced_date", Attribution::DeterminedNonAdditive),
                TestColumn::Date(rows.iter().map(|r| Some(dt(r.2))).collect()),
            ),
            (
                meta("pay_date", Attribution::DeterminedNonAdditive),
                TestColumn::Date(rows.iter().map(|r| Some(dt(r.3))).collect()),
            ),
            (
                meta("amount", Attribution::DeterminedNonAdditive),
                TestColumn::F64(rows.iter().map(|r| Some(r.4)).collect()),
            ),
            (
                meta("status", Attribution::DeterminedNonAdditive),
                TestColumn::Dict(rows.iter().map(|r| Some(r.5.to_string())).collect()),
            ),
        ],
        0,
        provenance(as_of),
    )
}

/// A [`DIVIDEND`] model whose rows carry exactly the given `dividend_id`
/// labels — for the rebase-guard tests, which key a cell edit by a
/// same-date ordinal (`2026-09-18#2`) and need only the row IDENTITY, not
/// any particular dates or amount. Built with `MatrixModel::build`, not by
/// hand, so this exercises the same `RowState::Document` labelling a real
/// delivery would; every row shares one ex date so a caller who wants a
/// same-day GROUP need only vary the labels' `#n` suffixes.
pub(crate) fn flat_model(labels: &[&str]) -> MatrixModel {
    let rows: Vec<(&str, &str, &str, &str, f64, &str)> = labels
        .iter()
        .map(|&label| {
            (
                label,
                "2026-09-18",
                "2026-08-01",
                "2026-10-01",
                1.0,
                "declared",
            )
        })
        .collect();
    let snapshot = dividend_snapshot(&rows);
    MatrixModel::build(&snapshot, &DIVIDEND, &Draft::default()).expect("a valid dividend fixture")
}

/// A flat panel with one `F64` and one `I64` value column — cheap to add as a `const`, and the one shape neither
/// [`SCHEDULE`] (no `I64` column) nor [`LADDER`] (its `I64` is the row
/// AXIS, never a value cell `:bump` can reach) offers: a row whose cells
/// `:bump` must land at TWO different declared types, the fixture the
/// tile-level atomicity test for `bumped`'s refusal needs.
pub(crate) static MIXED: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
    Arc::new(PanelSpec {
        kind: "mixed".into(),
        title: "Mixed".into(),
        dataset: "mixed".into(),
        document: "mixed".into(),
        rows: RowAxis {
            column: "mixed_id".into(),
            identity: RowIdentity::Minted,
            label: RowLabel::Shown,
        },
        columns: Columns::Values(vec![
            ValueColumn {
                column: "amt".into(),
                label: "amt".into(),
                ty: ColumnType::F64,
                format: SCHEDULE_AMOUNT_FORMAT,
                choices: None,
                required: false,
            },
            ValueColumn {
                column: "n".into(),
                label: "n".into(),
                ty: ColumnType::I64,
                format: ColumnFormat::MEASURE,
                choices: None,
                required: false,
            },
        ]),
        header: Vec::new(),
        slice_values: Vec::new(),
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        actions: Vec::new(),
    })
});

/// A [`MIXED`] document: `(mixed_id, amt, n)` per row.
pub(crate) fn mixed_snapshot(rows: &[(&str, f64, i64)]) -> Snapshot {
    let n = rows.len();
    Snapshot::for_tests_with_provenance(
        vec![
            (
                meta("underlying_ref", Attribution::Additive),
                TestColumn::Dict(vec![Some("SPX.Z".into()); n]),
            ),
            (
                meta("mixed_id", Attribution::Additive),
                TestColumn::Dict(rows.iter().map(|r| Some(r.0.to_string())).collect()),
            ),
            (
                meta("amt", Attribution::DeterminedNonAdditive),
                TestColumn::F64(rows.iter().map(|r| Some(r.1)).collect()),
            ),
            (
                meta("n", Attribution::DeterminedNonAdditive),
                TestColumn::I64(rows.iter().map(|r| r.2).collect()),
            ),
        ],
        0,
        provenance(BASE),
    )
}

/// A snapshot in the shape `compile_document` delivers `doc` in —
/// `document_columns()` order (key, axes, values, document-level
/// attributes), each value `DeterminedNonAdditive` and every label column
/// `Additive`, every attribute repeated on every row — so an upload test
/// can compare what `assemble` rebuilds with the very `DocumentRows` the
/// snapshot came from (the round trip compares like with like). The key
/// column is `underlying_ref`, the one both shipped datasets declare.
pub(crate) fn snapshot_of(spec: &PanelSpec, doc: &geode_core::document::DocumentRows) -> Snapshot {
    snapshot_of_at(spec, doc, BASE)
}

/// [`snapshot_of`] stamped as a generation of the caller's choosing — the
/// echo of an upload arrives as a NEW generation carrying the sent rows.
pub(crate) fn snapshot_of_at(
    spec: &PanelSpec,
    doc: &geode_core::document::DocumentRows,
    as_of: &str,
) -> Snapshot {
    use geode_core::document::{Column, Value};
    let n = doc.rows();
    let column = |col: &Column| match col {
        Column::F64(v) => TestColumn::F64(v.iter().map(|x| Some(*x)).collect()),
        Column::I64(v) => TestColumn::I64(v.clone()),
        Column::Utf8(v) => TestColumn::Dict(v.iter().map(|s| Some(s.clone())).collect()),
        Column::Date(v) => TestColumn::Date(v.iter().map(|d| Some(*d)).collect()),
    };
    let repeated = |value: &Value| match value {
        Value::F64(x) => TestColumn::F64(vec![Some(*x); n]),
        Value::I64(x) => TestColumn::I64(vec![*x; n]),
        Value::Utf8(s) => TestColumn::Dict(vec![Some(s.clone()); n]),
        Value::Date(d) => TestColumn::Date(vec![Some(*d); n]),
    };
    let mut columns = vec![(
        meta("underlying_ref", Attribution::Additive),
        TestColumn::Dict(vec![Some(doc.key.join("/")); n]),
    )];
    columns.extend(
        doc.axes
            .iter()
            .map(|(name, col)| (meta(name, Attribution::Additive), column(col))),
    );
    columns.extend(
        doc.values
            .iter()
            .map(|(name, col)| (meta(name, Attribution::DeterminedNonAdditive), column(col))),
    );
    columns.extend(
        doc.attributes
            .iter()
            .map(|(name, value)| (meta(name, Attribution::Additive), repeated(value))),
    );
    Snapshot::for_tests_with_provenance(
        columns,
        0,
        Provenance {
            datasets: vec![Freshness {
                dataset: spec.dataset.clone(),
                as_of: Some(as_of.into()),
                generation: Some(7),
            }],
            as_of_request: None,
        },
    )
}

/// The CVI terms and nodes [`fixture_cvi_rows`] lays out, term-major.
pub(crate) const CVI_TERMS: [&str; 2] = ["2026-10-16", "2026-11-20"];
pub(crate) const CVI_NODES: [f64; 3] = [-20.0, -1.0, 3.5];

/// A [`CVI`] document in the kind's own long form: two terms
/// × three nodes, term-major, `param` running 0.1 … 0.6, each term's
/// `forward`/`atm`/`skew` repeated on every node row of its slice, and the
/// dataset's own value (`param, forward, atm, skew`) and attribute
/// (`anchor_date, spot_ref`) orders.
pub(crate) fn fixture_cvi_rows() -> geode_core::document::DocumentRows {
    use geode_core::document::{Column, DocumentRows, Value};
    let slices = [(4512.3, 0.182, -1.1), (4530.75, 0.19, -0.95)];
    let (mut terms, mut nodes, mut params) = (Vec::new(), Vec::new(), Vec::new());
    let (mut fwd, mut atm, mut skew) = (Vec::new(), Vec::new(), Vec::new());
    for (t, term) in CVI_TERMS.iter().enumerate() {
        for (n, node) in CVI_NODES.iter().enumerate() {
            terms.push(chrono::NaiveDate::parse_from_str(term, "%Y-%m-%d").unwrap());
            nodes.push(*node);
            params.push((t * CVI_NODES.len() + n + 1) as f64 / 10.0);
            fwd.push(slices[t].0);
            atm.push(slices[t].1);
            skew.push(slices[t].2);
        }
    }
    DocumentRows {
        key: vec!["SPX.Z".into()],
        attributes: vec![
            ("anchor_date".into(), Value::Date(date(2026, 9, 12))),
            ("spot_ref".into(), Value::F64(5000.0)),
        ],
        axes: vec![
            ("term".into(), Column::Date(terms)),
            ("node".into(), Column::F64(nodes)),
        ],
        values: vec![
            ("param".into(), Column::F64(params)),
            ("forward".into(), Column::F64(fwd)),
            ("atm".into(), Column::F64(atm)),
            ("skew".into(), Column::F64(skew)),
        ],
    }
}

/// A [`DIVIDEND`] document of three rows labelled `A`, `B`, `C`, in the
/// dataset's own value (`ex_date, announced_date, pay_date, amount,
/// status`) and attribute (`currency, schedule_date`) orders.
pub(crate) fn fixture_dividend_rows() -> geode_core::document::DocumentRows {
    use geode_core::document::{Column, DocumentRows, Value};
    DocumentRows {
        key: vec!["SPX.Z".into()],
        attributes: vec![
            ("currency".into(), Value::Utf8("USD".into())),
            ("schedule_date".into(), Value::Date(date(2026, 9, 1))),
        ],
        axes: vec![(
            "dividend_id".into(),
            Column::Utf8(vec!["A".into(), "B".into(), "C".into()]),
        )],
        values: vec![
            (
                "ex_date".into(),
                Column::Date(vec![
                    date(2026, 9, 18),
                    date(2026, 12, 18),
                    date(2027, 3, 19),
                ]),
            ),
            (
                "announced_date".into(),
                Column::Date(vec![date(2026, 8, 1), date(2026, 11, 1), date(2027, 2, 1)]),
            ),
            (
                "pay_date".into(),
                Column::Date(vec![date(2026, 10, 1), date(2027, 1, 4), date(2027, 4, 1)]),
            ),
            ("amount".into(), Column::F64(vec![1.25, 1.3, 1.35])),
            (
                "status".into(),
                Column::Utf8(vec!["paid".into(), "declared".into(), "estimated".into()]),
            ),
        ],
    }
}
