//! Shared test-only fixtures (Task 3, spec §4.3): the dividend-schedule
//! panel and a document shaped for it.
//!
//! Lifted out of `matrix.rs`'s own test module so `tile.rs`'s integration
//! tests can drive the SAME flat, multi-typed-column panel through the
//! real tile — `open_flat`'s own door — rather than building a second
//! copy that could quietly drift from the pure-core one. `matrix.rs`'s
//! tests import from here too, so there is exactly one `SCHEDULE`.

#![cfg(test)]

use crate::core::draft::Draft;
use crate::core::matrix::MatrixModel;
use crate::core::spec::{
    Columns, DIVIDEND, PanelSpec, RowAxis, RowIdentity, RowLabel, ValueColumn,
};
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::schema::ColumnType;
use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};

pub(crate) const BASE: &str = "2026-09-12T14:00:00Z";

pub(crate) fn date(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn meta(name: &str, attribution: Attribution) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        // A document snapshot is depth 0 only (spec §7), which is why
        // `compile_document` emits exactly one attribution per column.
        attribution_by_depth: vec![attribution],
        scope_semantics: ScopeSemantics::Direct,
    }
}

fn provenance(as_of: &str) -> Provenance {
    Provenance {
        datasets: vec![Freshness {
            dataset: "div_schedule".into(),
            as_of: Some(as_of.into()),
            generation: 7,
        }],
        as_of_request: None,
    }
}

/// `amount`'s own format: four places, no grouping — the same reasoning
/// `spec::CVI_FORMAT` gives for a small number whose fourth place is
/// real, distinct from `ColumnFormat::MEASURE` so this fixture actually
/// exercises a column with its OWN format rather than the panel's
/// default.
const SCHEDULE_AMOUNT_FORMAT: ColumnFormat = ColumnFormat {
    precision: 4,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};

/// A dividend schedule (spec §4.3): one row per dividend, minted row
/// identity (the trader does not name a row), three typed value columns
/// — a date, a number at its own precision, and a status chosen from a
/// fixed vocabulary — the shape [`crate::core::matrix::CellKind`] exists
/// to paint and edit correctly.
/// [`SCHEDULE`] with its row label withheld (`RowLabel::Hidden`) — the
/// shipped `DIVIDEND`'s own shape, for the tests that pin what a hidden
/// label changes: the table's columns, `/`, `yy`, and where `o` lands.
pub(crate) const HIDDEN_SCHEDULE: PanelSpec = PanelSpec {
    kind: "sched_hidden",
    rows: RowAxis {
        column: "dividend_id",
        identity: RowIdentity::Minted,
        label: RowLabel::Hidden,
    },
    ..SCHEDULE
};

pub(crate) const SCHEDULE: PanelSpec = PanelSpec {
    kind: "sched",
    title: "Dividends",
    dataset: "div_schedule",
    document: "div_schedule",
    rows: RowAxis {
        column: "dividend_id",
        identity: RowIdentity::Minted,
        label: RowLabel::Shown,
    },
    columns: Columns::Values(&[
        ValueColumn {
            column: "ex_date",
            label: "ex",
            ty: ColumnType::Date,
            format: ColumnFormat::MEASURE,
            choices: None,
            required: true,
        },
        ValueColumn {
            column: "amount",
            label: "amount",
            ty: ColumnType::F64,
            format: SCHEDULE_AMOUNT_FORMAT,
            choices: None,
            required: true,
        },
        ValueColumn {
            column: "status",
            label: "status",
            ty: ColumnType::Utf8,
            format: ColumnFormat::MEASURE,
            choices: Some(&["estimated", "declared", "paid", "cancelled"]),
            required: true,
        },
    ]),
    header: &[],
    slice_values: &[],
    value_type: ColumnType::F64,
    format: ColumnFormat::MEASURE,
    actions: &[],
};

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

/// A strike ladder (Task 8's review): one row per strike, the row
/// identity TYPED as an integer — the shape that opens the TEXT row-label
/// editor on `o` (the shipped specs are `Minted` and `Typed(Date)`, so
/// without this the `(Text, RowLabel)` commit arm and `nudge`'s
/// `RowLabel` arm had no fixture). One `F64` value column at the
/// schedule's own four-place format.
pub(crate) const LADDER: PanelSpec = PanelSpec {
    kind: "ladder",
    title: "Strike ladder",
    dataset: "strike_ladder",
    document: "strike_ladder",
    rows: RowAxis {
        column: "strike",
        identity: RowIdentity::Typed(ColumnType::I64),
        label: RowLabel::Shown,
    },
    columns: Columns::Values(&[ValueColumn {
        column: "vol",
        label: "vol",
        ty: ColumnType::F64,
        format: SCHEDULE_AMOUNT_FORMAT,
        choices: None,
        required: true,
    }]),
    header: &[],
    slice_values: &[],
    value_type: ColumnType::F64,
    format: ColumnFormat::MEASURE,
    actions: &[],
};

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

/// A [`crate::core::DIVIDEND`] document: `(dividend_id, ex_date,
/// announced_date, pay_date, amount, status)` per row — the shipped
/// panel's own five value columns, for the one production-route test that
/// must prove `MarketDataTile::bump`'s `ty_of` reads the REAL spec's
/// `Columns::Values` branch correctly, not `SCHEDULE`'s three-column
/// stand-in (task 3 review, fix round 1).
pub(crate) fn dividend_snapshot(rows: &[(&str, &str, &str, &str, f64, &str)]) -> Snapshot {
    dividend_snapshot_at(rows, BASE)
}

/// [`dividend_snapshot`] stamped as a generation of the caller's choosing
/// (Task 4) — a second delivery at a later `as_of` is what moves a
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
/// labels — Task 4's rebase-guard tests, which key a cell edit by a
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

/// A flat panel with one `F64` and one `I64` value column (task 3 review,
/// fix round 1) — cheap to add as a `const`, and the one shape neither
/// [`SCHEDULE`] (no `I64` column) nor [`LADDER`] (its `I64` is the row
/// AXIS, never a value cell `:bump` can reach) offers: a row whose cells
/// `:bump` must land at TWO different declared types, the fixture the
/// tile-level atomicity test for `bumped`'s refusal needs.
pub(crate) const MIXED: PanelSpec = PanelSpec {
    kind: "mixed",
    title: "Mixed",
    dataset: "mixed",
    document: "mixed",
    rows: RowAxis {
        column: "mixed_id",
        identity: RowIdentity::Minted,
        label: RowLabel::Shown,
    },
    columns: Columns::Values(&[
        ValueColumn {
            column: "amt",
            label: "amt",
            ty: ColumnType::F64,
            format: SCHEDULE_AMOUNT_FORMAT,
            choices: None,
            required: false,
        },
        ValueColumn {
            column: "n",
            label: "n",
            ty: ColumnType::I64,
            format: ColumnFormat::MEASURE,
            choices: None,
            required: false,
        },
    ]),
    header: &[],
    slice_values: &[],
    value_type: ColumnType::F64,
    format: ColumnFormat::MEASURE,
    actions: &[],
};

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
                dataset: spec.dataset.into(),
                as_of: Some(as_of.into()),
                generation: 7,
            }],
            as_of_request: None,
        },
    )
}

/// The CVI terms and nodes [`fixture_cvi_rows`] lays out, term-major.
pub(crate) const CVI_TERMS: [&str; 2] = ["2026-10-16", "2026-11-20"];
pub(crate) const CVI_NODES: [f64; 3] = [-20.0, -1.0, 3.5];

/// A [`crate::core::CVI`] document in the kind's own long form: two terms
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
