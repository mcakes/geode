//! Shared numeric formatting for feature modules. Scale first, then round,
//! then apply thousands grouping and the negative style. Sign follows the
//! rounded value, so `-0.001` at two places displays as unsigned zero.
//! NaN is labelled `NaN` with zero sign; infinities use `∞` and `-∞`.

use crate::view::{ColumnFormat, Negative, Scale};

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
