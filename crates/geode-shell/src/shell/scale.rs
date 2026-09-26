//! Chrome geometry scaled with the window's rem size.
//!
//! Design lengths are expressed in pixels at [`DESIGN_REM`], the
//! [`crate::fontsize::FontSize::Medium`] size. [`design`] converts them to
//! `Rems`, keeping container dimensions proportional to text when the font
//! size changes. [`design_px`] resolves the same length to pixels for window
//! geometry calculations.
//!
//! Use actual pixels for tiling rectangles, hairlines, divider hit areas,
//! and other geometry independent of text size. Configured table column
//! widths also remain pixel values.

use gpui::{Pixels, Rems, rems};

/// The rem every chrome length was authored against — `FontSize::Medium`
/// (pinned by a test below, so the two cannot drift apart).
pub const DESIGN_REM: f32 = 12.0;

/// A chrome length spelled in the pixels it measures at the design rem,
/// on the rem scale.
pub fn design(px_at_design_rem: f32) -> Rems {
    rems(px_at_design_rem / DESIGN_REM)
}

/// [`design`] resolved to pixels for the current window rem — for
/// layout arithmetic that has to stay in `f32` pixels.
pub fn design_px(px_at_design_rem: f32, rem_size: Pixels) -> f32 {
    px_at_design_rem * f32::from(rem_size) / DESIGN_REM
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fontsize::FontSize;
    use gpui::px;

    #[test]
    fn the_design_rem_is_the_medium_font_size() {
        assert_eq!(DESIGN_REM, FontSize::Medium.rem_px());
    }

    /// At the design rem a length is exactly the pixels it was authored
    /// as; at the other two it scales by the same ratio the text does.
    #[test]
    fn design_lengths_are_identity_at_medium_and_proportional_elsewhere() {
        assert_eq!(design_px(44.0, px(FontSize::Medium.rem_px())), 44.0);
        assert_eq!(design_px(640.0, px(FontSize::Medium.rem_px())), 640.0);
        let large = design_px(44.0, px(FontSize::Large.rem_px()));
        let small = design_px(44.0, px(FontSize::Small.rem_px()));
        assert!((large - 44.0 * 14.0 / 12.0).abs() < 1e-4, "{large}");
        assert!((small - 44.0 * 10.0 / 12.0).abs() < 1e-4, "{small}");
    }

    /// The `Rems` form resolves to the same pixels as the `f32` form, so a
    /// container sized with one and a strip positioned with the other
    /// land on the same edge.
    #[test]
    fn the_rems_form_and_the_px_form_agree() {
        for rem in [10.0, 12.0, 14.0] {
            let r = design(28.0).to_pixels(px(rem));
            assert!(
                (f32::from(r) - design_px(28.0, px(rem))).abs() < 1e-4,
                "rem {rem}: {r:?}"
            );
        }
    }
}
