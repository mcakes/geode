//! Number formatting (Phase 3 spec §6.2). Scale first, then precision,
//! then grouping and the negative style; the sign is taken from the
//! rounded value so `-0.001` at two places is a zero, not a red zero.

use geode_core::view::{ColumnFormat, Negative, Scale};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Negative,
    Zero,
    Positive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatted {
    pub text: String,
    pub sign: Sign,
}

pub fn format_number(value: f64, format: &ColumnFormat) -> Formatted {
    if value.is_nan() {
        return Formatted {
            text: "NaN".into(),
            sign: Sign::Zero,
        };
    }
    if value.is_infinite() {
        return Formatted {
            text: if value > 0.0 {
                "∞".into()
            } else {
                "-∞".into()
            },
            sign: if value > 0.0 {
                Sign::Positive
            } else {
                Sign::Negative
            },
        };
    }
    let scaled = match format.scale {
        Scale::None => value,
        s => value / s.divisor(),
    };
    let precision = format.precision as usize;
    let factor = 10f64.powi(precision as i32);
    let rounded = (scaled * factor).round() / factor;
    let sign = if rounded == 0.0 {
        Sign::Zero
    } else if rounded < 0.0 {
        Sign::Negative
    } else {
        Sign::Positive
    };
    let magnitude = format!("{:.*}", precision, rounded.abs());
    let magnitude = if format.thousands {
        group_thousands(&magnitude)
    } else {
        magnitude
    };
    let text = match (sign, format.negative) {
        (Sign::Negative, Negative::Minus) => format!("-{magnitude}"),
        (Sign::Negative, Negative::Parens) => format!("({magnitude})"),
        _ => magnitude,
    };
    Formatted { text, sign }
}

/// `1234567.89` → `1,234,567.89`; the fraction is left alone.
fn group_thousands(s: &str) -> String {
    let (int, frac) = match s.find('.') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let digits: Vec<char> = int.chars().collect();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    out.push_str(frac);
    out
}

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
