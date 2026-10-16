//! Stateless cell glyphs for dense grids: a pure core decides every number
//! (sign, alpha, fraction, warning) and a painter draws it from the host's
//! preformatted text and a host-built paint struct. Painters never read the
//! theme, never format a value and always paint the number: color, length
//! and position are second channels, never the only one.

pub mod delta_chip;
pub mod heat;
pub mod percent_slider;
pub mod zero_bar;

/// Minimum widths of the painters, in design px at [`DESIGN_REM`], for a
/// host that sizes grid columns. The painters read these same constants, so
/// an exported width cannot drift from what is painted.
pub mod widths {
    /// Narrowest change chip (`delta_chip`).
    pub const CHIP_MIN: f32 = 40.0;
    /// Chip text inset, per side.
    pub const CHIP_INSET: f32 = 4.0;
    pub const SLIDER_TRACK: f32 = 52.0;
    pub const SLIDER_GAP: f32 = 6.0;
    pub const BAR_TRACK: f32 = 60.0;
    pub const BAR_GAP: f32 = 6.0;
    /// Heat-tint text inset, per side.
    pub const HEAT_INSET: f32 = 6.0;
    /// The missing-value text every painter's host centres.
    pub const DASH: &str = "\u{2014}";

    /// The narrowest cell that holds a chip of `text_px` of label.
    pub fn chip(text_px: f32) -> f32 {
        (text_px + 2.0 * CHIP_INSET).max(CHIP_MIN)
    }
    /// The narrowest cell for a slider with `label_px` of label beside it.
    pub const fn slider(label_px: f32) -> f32 {
        SLIDER_TRACK + SLIDER_GAP + label_px
    }
    /// The narrowest cell for a zero bar with `label_px` of label beside it.
    pub const fn bar(label_px: f32) -> f32 {
        BAR_TRACK + BAR_GAP + label_px
    }
    /// The narrowest cell that holds a heat-tinted `text_px` of text.
    pub const fn heat(text_px: f32) -> f32 {
        text_px + 2.0 * HEAT_INSET
    }
}

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
    fn the_exported_widths_are_the_painters_own() {
        assert_eq!(widths::CHIP_MIN, delta_chip::MIN_WIDTH);
        assert_eq!(widths::CHIP_INSET, delta_chip::INSET);
        assert_eq!(
            widths::SLIDER_TRACK + widths::SLIDER_GAP,
            percent_slider::TRACK_WIDTH + percent_slider::GAP
        );
        assert_eq!(
            widths::BAR_TRACK + widths::BAR_GAP,
            zero_bar::TRACK_WIDTH + zero_bar::GAP
        );
        assert_eq!(widths::HEAT_INSET, heat::INSET);
        assert_eq!(
            widths::chip(10.0),
            40.0,
            "a short label still gets the chip's minimum"
        );
        assert_eq!(widths::chip(60.0), 68.0);
        assert_eq!(widths::slider(20.0), 78.0);
        assert_eq!(widths::bar(20.0), 86.0);
        assert_eq!(widths::heat(30.0), 42.0);
    }

    #[test]
    fn design_is_a_twelfth_of_the_design_px() {
        assert_eq!(design(12.0), gpui::rems(1.0));
        assert_eq!(design(6.0), gpui::rems(0.5));
    }
}
