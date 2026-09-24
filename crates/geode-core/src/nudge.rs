//! Arrow-key nudging of the text in an open numeric editor: one unit of a
//! precision per step. Pure — text in, text out — so a module decides
//! which type and precision its open field carries and writes the answer
//! back into its input. Nothing here commits. Shared by the market-data
//! panel and the line pricer, which may not depend on each other.

use crate::schema::ColumnType;

/// Step the number spelled by `text` by `steps` units.
///
/// - `F64`: one unit is `10^-precision`, where `precision` is the column's
///   own painted precision when the caller knows it (a cell's format) and
///   otherwise the digits the text itself carries after the point (an
///   attribute paints with `{}`, so `5000` steps by `1` and `4512.30` by
///   `0.01`). The result is spelled with that same precision. The
///   arithmetic is done on the scaled integer, never on the float, so
///   `0.1` + one step at one place is `0.2`, not `0.20000000000000001`.
/// - `I64`: whole units, spelled plainly.
/// - anything else (a `Date` included — the date field owns those), or
///   text that does not parse as its type: `Err`
///   naming the text, in `parse_cell`'s own wording — the inline notice
///   appears beside a field the trader can no longer see the whole of.
pub fn nudge_text(
    text: &str,
    ty: ColumnType,
    precision: Option<usize>,
    steps: i64,
) -> Result<String, String> {
    let trimmed = text.trim();
    match ty {
        ColumnType::F64 => {
            let value: f64 = trimmed
                .parse()
                .ok()
                .filter(|v: &f64| v.is_finite())
                .ok_or_else(|| format!("'{text}' is not a number"))?;
            let precision = precision.unwrap_or_else(|| painted_precision(trimmed));
            // Scaled to an integer at the painted precision and stepped
            // there: the float only ever carries the FINAL value once,
            // and `{:.p$}` rounds that back to the same places.
            let unit = 10f64.powi(precision as i32);
            let scaled = (value * unit).round() as i128 + steps as i128;
            Ok(format!("{:.*}", precision, scaled as f64 / unit))
        }
        ColumnType::I64 => trimmed
            .parse::<i64>()
            .map(|v| v.saturating_add(steps).to_string())
            .map_err(|_| format!("'{text}' is not a whole number")),
        other => Err(format!("a {other:?} value cannot be nudged")),
    }
}

/// How many places `text` spells after its point — the precision an
/// attribute painted with `{}` implies. No point, or an exponent form,
/// reads as zero places.
fn painted_precision(text: &str) -> usize {
    text.split_once('.')
        .map(|(_, frac)| frac.chars().take_while(char::is_ascii_digit).count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_steps_one_unit_of_its_precision() {
        assert_eq!(
            nudge_text("0.2710", ColumnType::F64, Some(4), 1).unwrap(),
            "0.2711"
        );
        assert_eq!(
            nudge_text("0.2710", ColumnType::F64, Some(4), -1).unwrap(),
            "0.2709"
        );
        assert_eq!(
            nudge_text("0.2710", ColumnType::F64, Some(4), 10).unwrap(),
            "0.2720"
        );
    }

    #[test]
    fn an_attribute_steps_at_the_precision_its_text_paints() {
        assert_eq!(
            nudge_text("5000", ColumnType::F64, None, 1).unwrap(),
            "5001"
        );
        assert_eq!(
            nudge_text("4512.30", ColumnType::F64, Some(2), 10).unwrap(),
            "4512.40"
        );
        assert_eq!(
            nudge_text("4512.30", ColumnType::F64, None, 10).unwrap(),
            "4512.40"
        );
        assert_eq!(
            nudge_text("5000", ColumnType::I64, None, -3).unwrap(),
            "4997"
        );
    }

    /// A date is the segmented field's (2026-09-19), never this function's.
    #[test]
    fn a_date_is_not_nudged_as_text() {
        assert!(nudge_text("2026-09-14", ColumnType::Date, None, 1).is_err());
    }

    #[test]
    fn float_rounding_never_leaks_into_the_text() {
        assert_eq!(
            nudge_text("0.1", ColumnType::F64, Some(1), 1).unwrap(),
            "0.2"
        );
        assert_eq!(nudge_text("0.1", ColumnType::F64, None, 1).unwrap(), "0.2");
        assert_eq!(
            nudge_text("-0.0001", ColumnType::F64, Some(4), 1).unwrap(),
            "0.0000"
        );
        assert_eq!(
            nudge_text("1.005", ColumnType::F64, Some(3), -5).unwrap(),
            "1.000"
        );
    }

    #[test]
    fn unparseable_text_is_refused_naming_the_text() {
        let err = nudge_text("abc", ColumnType::F64, Some(4), 1).unwrap_err();
        assert!(err.contains("'abc'"), "{err}");
        let err = nudge_text("1.5", ColumnType::I64, None, 1).unwrap_err();
        assert!(err.contains("'1.5'"), "{err}");
        assert!(nudge_text("x", ColumnType::Utf8, None, 1).is_err());
    }
}
