//! Shared test-only fixtures (Task 3, spec §4.3): the dividend-schedule
//! panel and a document shaped for it.
//!
//! Lifted out of `matrix.rs`'s own test module so `tile.rs`'s integration
//! tests can drive the SAME flat, multi-typed-column panel through the
//! real tile — `open_flat`'s own door — rather than building a second
//! copy that could quietly drift from the pure-core one. `matrix.rs`'s
//! tests import from here too, so there is exactly one `SCHEDULE`.

#![cfg(test)]

use crate::core::spec::{Columns, PanelSpec, RowAxis, RowIdentity, ValueColumn};
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
pub(crate) const SCHEDULE: PanelSpec = PanelSpec {
    kind: "sched",
    title: "Dividends",
    dataset: "div_schedule",
    document: "div_schedule",
    rows: RowAxis {
        column: "dividend_id",
        identity: RowIdentity::Minted,
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
        provenance(BASE),
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
