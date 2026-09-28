//! Rules a selection-wide edit applies per cell: whether a typed value
//! lands in a cell of a given kind, how far one arrow step moves a
//! numeric cell, and the one notice line that counts what was written
//! and what was skipped and why.

use crate::core::draft::{parse_attr, parse_cell};
use crate::core::matrix::CellKind;
use geode_core::document::Value;
use geode_core::schema::ColumnType;
use std::collections::BTreeMap;

/// Why a selected cell took no part in a bulk edit. The order is the
/// notice's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Skip {
    Deleted,
    /// An inserted row the model still paints but the draft no longer
    /// holds, so the write had nowhere to land.
    Moved,
    Empty,
    NotNumeric,
    WrongType,
    Required,
    NotAnOption,
}

impl Skip {
    fn phrase(self) -> &'static str {
        match self {
            Skip::Deleted => "deleted",
            Skip::Moved => "moved",
            Skip::Empty => "empty",
            Skip::NotNumeric => "not numeric",
            Skip::WrongType => "wrong type",
            Skip::Required => "required",
            Skip::NotAnOption => "not an option",
        }
    }
}

/// Skipped cells counted per reason, kept in [`Skip`]'s order.
#[derive(Debug, Clone, Default)]
pub struct Skips(BTreeMap<Skip, usize>);

impl Skips {
    pub fn add(&mut self, skip: Skip) {
        *self.0.entry(skip).or_default() += 1;
    }

    pub fn total(&self) -> usize {
        self.0.values().sum()
    }

    /// `""` when nothing was skipped, else `", skipped 3 (2 deleted, 1 wrong type)"`.
    pub fn describe(&self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|(skip, n)| format!("{n} {}", skip.phrase()))
            .collect();
        format!(", skipped {} ({})", self.total(), parts.join(", "))
    }
}

/// `N cell` or `N cells`: every bulk notice counts through this one door.
pub(crate) fn cells(n: usize) -> String {
    format!("{n} cell{}", if n == 1 { "" } else { "s" })
}

pub fn set_notice(n: usize, skips: &Skips) -> String {
    format!("set {}{}", cells(n), skips.describe())
}

/// `total_steps` is the signed count since the editor opened, so the line
/// says where the block stands, not only the last press.
pub fn step_notice(n: usize, total_steps: i64, skips: &Skips) -> String {
    format!("stepped {} {total_steps:+}{}", cells(n), skips.describe())
}

/// Whether `text` lands in a cell of `kind`, and as what. `ty` is the
/// column's declared type for a `Number` cell (`None` for other kinds).
/// A choice must name one of its options exactly (after trimming): a bulk
/// write has no popup to rank a near miss against.
pub fn accept(
    kind: &CellKind,
    ty: Option<ColumnType>,
    required: bool,
    text: &str,
) -> Result<Value, Skip> {
    match kind {
        CellKind::Number(_) => {
            let ty = ty.ok_or(Skip::WrongType)?;
            parse_cell(text, ty).map_err(|_| Skip::WrongType)
        }
        CellKind::Text => {
            let trimmed = text.trim();
            if trimmed.is_empty() && required {
                Err(Skip::Required)
            } else {
                Ok(Value::Utf8(trimmed.to_string()))
            }
        }
        CellKind::Choice(options) => {
            let trimmed = text.trim();
            options
                .iter()
                .find(|option| **option == trimmed)
                .map(|option| Value::Utf8((*option).to_string()))
                .ok_or(Skip::NotAnOption)
        }
        CellKind::Date => parse_attr(text, ColumnType::Date).map_err(|_| Skip::WrongType),
    }
}

/// One arrow step on a numeric column: one unit of its painted places, or
/// a whole one on an integer column. The value it is added to is exact
/// (`draft::bumped`), never snapped to the painted grid.
pub fn step_delta(ty: ColumnType, precision: u8, steps: i64) -> f64 {
    match ty {
        ColumnType::I64 => steps as f64,
        _ => steps as f64 / 10f64.powi(i32::from(precision)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::view::ColumnFormat;

    #[test]
    fn a_number_cell_parses_by_its_declared_type_and_refuses_otherwise() {
        let n = CellKind::Number(ColumnFormat::MEASURE);
        assert_eq!(
            accept(&n, Some(ColumnType::F64), false, " 0.25 "),
            Ok(Value::F64(0.25))
        );
        assert_eq!(
            accept(&n, Some(ColumnType::I64), false, "3"),
            Ok(Value::I64(3))
        );
        assert_eq!(
            accept(&n, Some(ColumnType::I64), false, "0.5"),
            Err(Skip::WrongType)
        );
        assert_eq!(
            accept(&n, Some(ColumnType::F64), false, "abc"),
            Err(Skip::WrongType)
        );
    }

    #[test]
    fn text_choice_and_date_cells_each_keep_their_own_rule() {
        assert_eq!(
            accept(&CellKind::Text, None, false, " x "),
            Ok(Value::Utf8("x".into()))
        );
        assert_eq!(
            accept(&CellKind::Text, None, true, "  "),
            Err(Skip::Required)
        );
        let c = CellKind::Choice(&["declared", "estimated"]);
        assert_eq!(
            accept(&c, None, false, "declared"),
            Ok(Value::Utf8("declared".into()))
        );
        assert_eq!(accept(&c, None, false, "maybe"), Err(Skip::NotAnOption));
        assert!(matches!(
            accept(&CellKind::Date, None, false, "2027-03-19"),
            Ok(Value::Date(_))
        ));
        assert_eq!(
            accept(&CellKind::Date, None, false, "0.25"),
            Err(Skip::WrongType)
        );
    }

    #[test]
    fn a_step_is_one_unit_of_the_columns_places_or_one_on_an_integer() {
        assert_eq!(step_delta(ColumnType::F64, 4, 1), 0.0001);
        assert_eq!(step_delta(ColumnType::F64, 2, -10), -0.1);
        assert_eq!(step_delta(ColumnType::I64, 0, 10), 10.0);
    }

    #[test]
    fn notices_count_cells_and_name_each_skip_reason_in_a_fixed_order() {
        let mut s = Skips::default();
        assert_eq!(set_notice(1, &s), "set 1 cell");
        s.add(Skip::WrongType);
        s.add(Skip::Deleted);
        s.add(Skip::Deleted);
        assert_eq!(
            set_notice(12, &s),
            "set 12 cells, skipped 3 (2 deleted, 1 wrong type)"
        );
        assert_eq!(step_notice(9, 12, &Skips::default()), "stepped 9 cells +12");
        assert_eq!(
            step_notice(2, -1, &s),
            "stepped 2 cells -1, skipped 3 (2 deleted, 1 wrong type)"
        );
    }

    #[test]
    fn a_cell_the_draft_no_longer_holds_is_named_moved_after_deleted() {
        let mut s = Skips::default();
        s.add(Skip::Empty);
        s.add(Skip::Moved);
        s.add(Skip::Deleted);
        assert_eq!(
            set_notice(1, &s),
            "set 1 cell, skipped 3 (1 deleted, 1 moved, 1 empty)"
        );
    }
}
