//! What a pointer position over a painted chart means (mouse pass,
//! 2026-09-24): a place in a pane's plot, the divider between the two
//! panes, or nothing the chart owns. Pure geometry over a solved
//! [`Layout`], so a module can answer a wheel, a press or a drag without
//! knowing how the element laid the panes out — and so the answer is
//! testable without a window.
//!
//! The divider is the [`PANE_GAP`] between the panes, widened by
//! [`DIVIDER_MARGIN`] on each side: a 6 px target is under the guide's
//! floor for a drag handle, and the extra margin is taken from the
//! plots' edges, where a press was never going to start a pan.

use super::axis::Pane;
use super::layout::Layout;
use super::{Rect, design_px};

/// How far the divider's grab band extends into each neighbouring plot,
/// in design pixels.
pub const DIVIDER_MARGIN: f32 = 3.0;

/// What sits under a pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// Inside a pane's plot rect. `fraction` is the horizontal position
    /// across the plot, `0` at its left edge and `1` at its right — the
    /// `about` a zoom keeps still.
    Plot { pane: Pane, fraction: f32 },
    /// On the band between the two panes.
    Divider,
    /// An axis, the density strip, or outside the chart.
    Outside,
}

/// Where the divider's grab band is, in the layout's own space, or
/// `None` while there is one pane. It spans the full width of `bounds`
/// so a press on the axis column beside the gap still takes it.
pub fn divider_band(layout: &Layout, bounds: Rect, rem_px: f32) -> Option<Rect> {
    let lower = layout.lower.as_ref()?;
    let margin = design_px(DIVIDER_MARGIN, rem_px);
    let top = layout.upper.plot.bottom() - margin;
    let bottom = lower.plot.y + margin;
    Some(Rect::new(bounds.x, top, bounds.w, (bottom - top).max(0.0)))
}

/// Classify `(x, y)` in the layout's space. The divider band is asked
/// first: its margins overlap the plots' edges on purpose, and a press
/// there means the divider.
pub fn hit_test(layout: &Layout, bounds: Rect, rem_px: f32, x: f32, y: f32) -> Hit {
    if divider_band(layout, bounds, rem_px).is_some_and(|b| b.contains(x, y)) {
        return Hit::Divider;
    }
    let panes = [
        (Pane::Upper, Some(layout.upper)),
        (Pane::Lower, layout.lower),
    ];
    for (pane, rects) in panes {
        let Some(rects) = rects else {
            continue;
        };
        let plot = rects.plot;
        if plot.contains(x, y) && plot.w > 0.0 {
            return Hit::Plot {
                pane,
                fraction: ((x - plot.x) / plot.w).clamp(0.0, 1.0),
            };
        }
    }
    Hit::Outside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::layout::LayoutOptions;
    use crate::core::{DESIGN_REM, PANE_GAP};

    fn two_panes() -> (Layout, Rect) {
        let bounds = Rect::new(0.0, 0.0, 400.0, 300.0);
        let layout = Layout::solve(
            bounds,
            LayoutOptions {
                upper_left: true,
                upper_right: false,
                lower_left: true,
                lower_right: false,
                density: false,
                split: 0.5,
                rem_px: DESIGN_REM,
            },
        );
        (layout, bounds)
    }

    fn one_pane() -> (Layout, Rect) {
        let bounds = Rect::new(0.0, 0.0, 400.0, 300.0);
        let layout = Layout::solve(
            bounds,
            LayoutOptions {
                upper_left: true,
                upper_right: false,
                lower_left: false,
                lower_right: false,
                density: false,
                split: 0.5,
                rem_px: DESIGN_REM,
            },
        );
        (layout, bounds)
    }

    #[test]
    fn a_point_in_a_plot_names_its_pane_and_its_fraction_across() {
        let (layout, bounds) = two_panes();
        let upper = layout.upper.plot;
        let lower = layout.lower.unwrap().plot;
        let hit = hit_test(
            &layout,
            bounds,
            DESIGN_REM,
            upper.x + upper.w / 4.0,
            upper.y + 2.0,
        );
        assert_eq!(
            hit,
            Hit::Plot {
                pane: Pane::Upper,
                fraction: 0.25
            }
        );
        let hit = hit_test(
            &layout,
            bounds,
            DESIGN_REM,
            lower.right() - 1.0,
            lower.y + lower.h / 2.0,
        );
        match hit {
            Hit::Plot { pane, fraction } => {
                assert_eq!(pane, Pane::Lower);
                assert!(fraction > 0.99 && fraction <= 1.0, "{fraction}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_axis_column_and_the_outside_are_not_a_plot() {
        let (layout, bounds) = two_panes();
        let axis = layout.upper.left_axis.unwrap();
        assert_eq!(
            hit_test(
                &layout,
                bounds,
                DESIGN_REM,
                axis.x + 1.0,
                axis.y + axis.h / 2.0
            ),
            Hit::Outside
        );
        assert_eq!(
            hit_test(&layout, bounds, DESIGN_REM, -5.0, 10.0),
            Hit::Outside
        );
        assert_eq!(
            hit_test(&layout, bounds, DESIGN_REM, 10.0, layout.x_axis.y + 2.0),
            Hit::Outside,
            "the x axis strip is not a plot"
        );
    }

    #[test]
    fn the_divider_band_is_the_gap_plus_a_margin_each_side_and_only_with_two_panes() {
        let (layout, bounds) = two_panes();
        let band = divider_band(&layout, bounds, DESIGN_REM).expect("two panes");
        let gap = design_px(PANE_GAP, DESIGN_REM);
        let margin = design_px(DIVIDER_MARGIN, DESIGN_REM);
        assert!((band.h - (gap + 2.0 * margin)).abs() < 1e-3, "{}", band.h);
        assert!((band.y - (layout.upper.plot.bottom() - margin)).abs() < 1e-3);
        assert_eq!(band.x, bounds.x);
        assert_eq!(band.w, bounds.w);
        // The margin wins over the plot it overlaps.
        let x = layout.upper.plot.x + 10.0;
        assert_eq!(
            hit_test(
                &layout,
                bounds,
                DESIGN_REM,
                x,
                layout.upper.plot.bottom() - 1.0
            ),
            Hit::Divider
        );
        // And a point on the axis column beside the gap is the divider too.
        assert_eq!(
            hit_test(&layout, bounds, DESIGN_REM, 2.0, band.y + band.h / 2.0),
            Hit::Divider
        );
        let (one, bounds) = one_pane();
        assert_eq!(divider_band(&one, bounds, DESIGN_REM), None);
        assert_ne!(
            hit_test(&one, bounds, DESIGN_REM, x, one.upper.plot.bottom() - 1.0),
            Hit::Divider
        );
    }

    #[test]
    fn the_fraction_is_clamped_into_the_unit_interval() {
        let (layout, bounds) = one_pane();
        let plot = layout.upper.plot;
        // Exactly on the left edge is inside (`contains` is half-open).
        assert_eq!(
            hit_test(&layout, bounds, DESIGN_REM, plot.x, plot.y + 1.0),
            Hit::Plot {
                pane: Pane::Upper,
                fraction: 0.0
            }
        );
    }
}
