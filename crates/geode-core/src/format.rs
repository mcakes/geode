//! Number formatting (Phase 3 spec §6.2). Scale first, then precision,
//! then grouping and the negative style; the sign is taken from the
//! rounded value so `-0.001` at two places is a zero, not a red zero.
//!
//! It lives in core rather than in the blotter because two modules now
//! paint numbers from a `ColumnFormat` — the blotter's cells and the
//! market-data panel's matrix (market-data spec §8.2) — and modules
//! never depend on each other. `geode_blotter::core::format` re-exports
//! these three names, so the blotter's own call sites and its tests read
//! unchanged.

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
