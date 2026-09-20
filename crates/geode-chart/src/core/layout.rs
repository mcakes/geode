//! One or two panes with their axis rects, a density strip and one
//! shared x axis (spec §8.1, ruling 12).

use super::{AXIS_WIDTH, DENSITY_STRIP, PANE_GAP, Rect, X_AXIS_HEIGHT, design_px};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutOptions {
    pub upper_left: bool,
    pub upper_right: bool,
    pub lower_left: bool,
    pub lower_right: bool,
    pub density: bool,
    /// The upper pane's share of the height while a lower pane exists.
    pub split: f32,
    pub rem_px: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PaneRects {
    pub plot: Rect,
    pub left_axis: Option<Rect>,
    pub right_axis: Option<Rect>,
    pub density: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub upper: PaneRects,
    pub lower: Option<PaneRects>,
    pub x_axis: Rect,
}

pub const SPLIT_DEFAULT: f32 = 0.7;
pub const SPLIT_MIN: f32 = 0.2;
pub const SPLIT_MAX: f32 = 0.8;

impl Layout {
    pub fn solve(bounds: Rect, o: LayoutOptions) -> Layout {
        let axis_w = design_px(AXIS_WIDTH, o.rem_px);
        let strip_w = if o.density {
            design_px(DENSITY_STRIP, o.rem_px)
        } else {
            0.0
        };
        let x_axis_h = design_px(X_AXIS_HEIGHT, o.rem_px);
        let gap = design_px(PANE_GAP, o.rem_px);

        // Columns are reserved for EITHER pane's use so both share one x.
        let any_left = o.upper_left || o.lower_left;
        let any_right = o.upper_right || o.lower_right;
        let left_w = if any_left { axis_w } else { 0.0 };
        let right_w = if any_right { axis_w } else { 0.0 };
        let plot_x = bounds.x + left_w;
        let plot_w = (bounds.w - left_w - right_w - strip_w).max(0.0);
        let strip_x = plot_x + plot_w;
        let right_x = strip_x + strip_w;

        let has_lower = o.lower_left || o.lower_right;
        let avail = (bounds.h - x_axis_h - if has_lower { gap } else { 0.0 }).max(0.0);
        let split = if o.split.is_finite() {
            o.split.clamp(SPLIT_MIN, SPLIT_MAX)
        } else {
            SPLIT_DEFAULT
        };
        let upper_h = if has_lower { avail * split } else { avail };
        let lower_h = if has_lower { avail - upper_h } else { 0.0 };

        let pane = |y: f32, h: f32, left: bool, right: bool| PaneRects {
            plot: Rect::new(plot_x, y, plot_w, h),
            left_axis: left.then(|| Rect::new(bounds.x, y, axis_w, h)),
            right_axis: right.then(|| Rect::new(right_x, y, axis_w, h)),
            density: o.density.then(|| Rect::new(strip_x, y, strip_w, h)),
        };
        let upper = pane(bounds.y, upper_h, o.upper_left, o.upper_right);
        let lower = has_lower.then(|| {
            pane(
                bounds.y + upper_h + gap,
                lower_h,
                o.lower_left,
                o.lower_right,
            )
        });
        let lowest_bottom = lower.map_or(upper.plot.bottom(), |l| l.plot.bottom());
        Layout {
            upper,
            lower,
            x_axis: Rect::new(plot_x, lowest_bottom, plot_w, x_axis_h),
        }
    }

    pub fn lowest_bottom(&self) -> f32 {
        self.x_axis.y
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const B: Rect = Rect::new(0.0, 0.0, 1000.0, 500.0);
    fn o() -> LayoutOptions {
        LayoutOptions {
            upper_left: true,
            upper_right: false,
            lower_left: false,
            lower_right: false,
            density: false,
            split: 0.7,
            rem_px: 12.0,
        }
    }

    #[test]
    fn an_axis_rect_exists_only_for_a_side_in_use() {
        let l = Layout::solve(B, o());
        assert!(l.upper.left_axis.is_some());
        assert!(l.upper.right_axis.is_none());
        assert_eq!(l.upper.plot.x, AXIS_WIDTH);
        assert_eq!(l.upper.plot.right(), 1000.0, "no right axis reserved");
        let l = Layout::solve(
            B,
            LayoutOptions {
                upper_left: false,
                upper_right: true,
                ..o()
            },
        );
        assert!(l.upper.left_axis.is_none());
        assert_eq!(l.upper.plot.x, 0.0);
        assert_eq!(l.upper.right_axis.unwrap().x, 1000.0 - AXIS_WIDTH);
        let l = Layout::solve(
            B,
            LayoutOptions {
                upper_left: false,
                ..o()
            },
        );
        assert_eq!(
            l.upper.plot,
            Rect::new(0.0, 0.0, 1000.0, 500.0 - X_AXIS_HEIGHT),
            "nothing reserved with no side in use"
        );
    }

    #[test]
    fn the_density_strip_exists_only_when_asked_and_sits_before_the_right_axis() {
        let l = Layout::solve(
            B,
            LayoutOptions {
                density: true,
                upper_right: true,
                ..o()
            },
        );
        let d = l.upper.density.unwrap();
        assert_eq!(d.w, DENSITY_STRIP);
        assert_eq!(d.right(), l.upper.right_axis.unwrap().x);
        assert_eq!(l.upper.plot.right(), d.x);
        assert!(Layout::solve(B, o()).upper.density.is_none());
    }

    #[test]
    fn the_lower_pane_exists_only_while_a_visible_slot_uses_a_bottom_axis() {
        assert!(Layout::solve(B, o()).lower.is_none());
        let l = Layout::solve(
            B,
            LayoutOptions {
                lower_right: true,
                ..o()
            },
        );
        let lower = l.lower.unwrap();
        assert!(lower.right_axis.is_some() && lower.left_axis.is_none());
        assert_eq!(
            l.x_axis.y,
            lower.plot.bottom(),
            "the x axis sits under the LOWEST pane"
        );
        assert_eq!(l.x_axis.h, X_AXIS_HEIGHT);
        let avail = 500.0 - X_AXIS_HEIGHT - PANE_GAP;
        assert!((l.upper.plot.h - avail * 0.7).abs() < 1e-3);
        assert!((lower.plot.h - avail * 0.3).abs() < 1e-3);
        assert_eq!(lower.plot.y, l.upper.plot.bottom() + PANE_GAP);
    }

    #[test]
    fn split_is_clamped() {
        let l = Layout::solve(
            B,
            LayoutOptions {
                lower_left: true,
                split: 0.05,
                ..o()
            },
        );
        let avail = 500.0 - X_AXIS_HEIGHT - PANE_GAP;
        assert!((l.upper.plot.h - avail * SPLIT_MIN).abs() < 1e-3);
        let l = Layout::solve(
            B,
            LayoutOptions {
                lower_left: true,
                split: 0.99,
                ..o()
            },
        );
        assert!((l.upper.plot.h - avail * SPLIT_MAX).abs() < 1e-3);
        let l = Layout::solve(
            B,
            LayoutOptions {
                lower_left: true,
                split: f32::NAN,
                ..o()
            },
        );
        assert!(
            (l.upper.plot.h - avail * SPLIT_DEFAULT).abs() < 1e-3,
            "NaN takes the default"
        );
    }

    #[test]
    fn both_panes_share_one_x_mapping() {
        // the lower pane uses the right side only; the upper the left only —
        // both plots still start and end at the same x
        let l = Layout::solve(
            B,
            LayoutOptions {
                lower_right: true,
                density: true,
                ..o()
            },
        );
        let lower = l.lower.unwrap();
        assert_eq!(l.upper.plot.x, lower.plot.x);
        assert_eq!(l.upper.plot.w, lower.plot.w);
        assert_eq!(l.upper.plot.x, AXIS_WIDTH);
        assert_eq!(l.upper.plot.right(), 1000.0 - AXIS_WIDTH - DENSITY_STRIP);
        assert!(
            l.upper.right_axis.is_none(),
            "the column is reserved, the rect is not painted"
        );
        assert!(lower.left_axis.is_none());
        assert_eq!(l.x_axis.x, l.upper.plot.x);
        assert_eq!(l.x_axis.w, l.upper.plot.w);
    }

    #[test]
    fn lengths_follow_the_rem() {
        let l = Layout::solve(
            B,
            LayoutOptions {
                density: true,
                rem_px: 14.0,
                ..o()
            },
        );
        assert!((l.upper.left_axis.unwrap().w - AXIS_WIDTH * 14.0 / 12.0).abs() < 1e-3);
        assert!((l.upper.density.unwrap().w - DENSITY_STRIP * 14.0 / 12.0).abs() < 1e-3);
        assert!((l.x_axis.h - X_AXIS_HEIGHT * 14.0 / 12.0).abs() < 1e-3);
    }

    #[test]
    fn a_tiny_bounds_never_yields_a_negative_rect() {
        let l = Layout::solve(
            Rect::new(0.0, 0.0, 30.0, 10.0),
            LayoutOptions {
                lower_left: true,
                upper_right: true,
                density: true,
                ..o()
            },
        );
        assert!(l.upper.plot.w >= 0.0 && l.upper.plot.h >= 0.0);
        let lower = l.lower.unwrap();
        assert!(lower.plot.w >= 0.0 && lower.plot.h >= 0.0);
    }
}
