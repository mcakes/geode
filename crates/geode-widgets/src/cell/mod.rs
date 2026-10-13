//! Stateless cell glyphs for dense grids: a pure core decides every number
//! (sign, alpha, fraction, warning) and a painter draws it from the host's
//! preformatted text and a host-built paint struct. Painters never read the
//! theme, never format a value and always paint the number: color, length
//! and position are second channels, never the only one.

pub mod delta_chip;
pub mod heat;
pub mod percent_slider;
pub mod zero_bar;

use gpui::{Rems, rems};

/// The design rem the shell and the chart kit size against; duplicated
/// because this crate must not depend on the shell.
pub const DESIGN_REM: f32 = 12.0;

/// A length given in pixels at the 12 px design rem, as rems, so a glyph
/// scales with interface zoom like the text beside it.
pub fn design(px_at_design_rem: f32) -> Rems {
    rems(px_at_design_rem / DESIGN_REM)
}

/// The largest finite absolute value among `values`, 0.0 when there is none:
/// the scale a column's chips and bars are measured against. Non-finite
/// values are skipped so one bad cell cannot flatten or blow up the column.
pub fn column_scale(values: impl IntoIterator<Item = f64>) -> f64 {
    values
        .into_iter()
        .filter(|v| v.is_finite())
        .fold(0.0, |acc: f64, v| acc.max(v.abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_scale_is_the_largest_absolute_value() {
        assert_eq!(column_scale([0.5, -1.25, 1.0]), 1.25);
        assert_eq!(column_scale(std::iter::empty()), 0.0);
        assert_eq!(column_scale([0.0, -0.0]), 0.0);
    }

    #[test]
    fn column_scale_skips_non_finite() {
        assert_eq!(
            column_scale([f64::NAN, -2.0, f64::INFINITY, f64::NEG_INFINITY]),
            2.0
        );
        assert_eq!(column_scale([f64::NAN]), 0.0);
    }

    #[test]
    fn design_is_a_twelfth_of_the_design_px() {
        assert_eq!(design(12.0), gpui::rems(1.0));
        assert_eq!(design(6.0), gpui::rems(0.5));
    }
}
