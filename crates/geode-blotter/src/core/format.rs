//! Shared number formatting from `geode_core::format`. Feature crates use
//! this common implementation without depending on sibling features. The
//! blotter tests pin its precision, scaling, sign, and non-finite display rules.

pub use geode_core::format::{Formatted, Sign, format_number};

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::view::{Colour, ColumnFormat, Negative, Scale};

    fn f(precision: u8, thousands: bool, negative: Negative, scale: Scale) -> ColumnFormat {
        ColumnFormat {
            precision,
            thousands,
            negative,
            colour: Colour::Sign,
            scale,
        }
    }

    #[test]
    fn precision_thousands_and_sign() {
        let m = ColumnFormat::MEASURE;
        assert_eq!(format_number(1234567.891, &m).text, "1,234,567.89");
        assert_eq!(format_number(-0.5, &m).text, "-0.50");
        assert_eq!(format_number(0.0, &m).sign, Sign::Zero);
        assert_eq!(
            format_number(-0.001, &m).text,
            "0.00",
            "rounds to zero, no negative zero"
        );
        assert_eq!(format_number(-0.001, &m).sign, Sign::Zero);
        assert_eq!(
            format_number(42.0, &f(0, false, Negative::Minus, Scale::None)).text,
            "42"
        );
        assert_eq!(
            format_number(-42.5, &f(0, false, Negative::Minus, Scale::None)).text,
            "-43"
        );
    }

    #[test]
    fn parentheses_and_scale() {
        assert_eq!(
            format_number(-1234.5, &f(1, true, Negative::Parens, Scale::None)).text,
            "(1,234.5)"
        );
        assert_eq!(
            format_number(1234567.89, &f(0, true, Negative::Minus, Scale::Thousands)).text,
            "1,235"
        );
        assert_eq!(
            format_number(1234567.89, &f(2, true, Negative::Minus, Scale::Millions)).text,
            "1.23"
        );
        assert_eq!(
            format_number(-999.0, &f(0, true, Negative::Parens, Scale::Thousands)).text,
            "(1)"
        );
        assert_eq!(
            format_number(-400.0, &f(0, true, Negative::Parens, Scale::Thousands)).text,
            "0",
            "rounds to zero after scaling"
        );
    }

    #[test]
    fn non_finite_values_are_spelled_not_crashed() {
        let m = ColumnFormat::MEASURE;
        assert_eq!(format_number(f64::NAN, &m).text, "NaN");
        assert_eq!(format_number(f64::INFINITY, &m).text, "∞");
        assert_eq!(format_number(f64::NEG_INFINITY, &m).text, "-∞");
    }
}
