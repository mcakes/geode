//! Window-free chart geometry, time and linear x labels, mark strokes,
//! decimation and palette values.
//!
//! Geometry operates over slices and values. Decimation reuses a caller-owned
//! buffer; time ticks allocate candidate vectors and formatted labels, and
//! `linear` does the same for a strike-like x axis.

pub mod axis;
pub mod decimate;
pub mod hit;
pub mod layout;
pub mod linear;
pub mod marks;
pub mod palette;
pub mod scale;
pub mod time;
pub mod view;

/// The rem every design-pixel constant below is authored at — the value
/// of `geode_shell::shell::scale::DESIGN_REM` (`FontSize::Medium`),
/// duplicated because this crate must not depend on the shell.
pub const DESIGN_REM: f32 = 12.0;

/// Density-strip width in design pixels at [`DESIGN_REM`].
pub const DENSITY_STRIP: f32 = 80.0;
pub const TICK_GAP: f32 = 64.0;
pub const DASH: f32 = 4.0;
pub const GAP: f32 = 3.0;
pub const AXIS_WIDTH: f32 = 44.0;
pub const PANE_GAP: f32 = 6.0;
/// The shared x-axis strip under the lowest pane — gpui-component's own
/// `AXIS_GAP`, so our axis text sits where the component's charts put it.
pub const X_AXIS_HEIGHT: f32 = 18.0;
/// Minimum vertical distance between two y ticks.
pub const Y_TICK_GAP: f32 = 40.0;

/// Maximum density bars painted by one chart in one paint call, shared
/// across both panes and all visible slots.
///
/// Bars use uncached `paint_quad` calls, so their cost grows with the product
/// of slots and bins. The painter stops at this limit, visiting the upper
/// pane before the lower pane and slots in model order within each pane.
/// Density-strip backgrounds are outside this count.
pub const MAX_DENSITY_QUADS: usize = 2_000;

/// A design length resolved for the window's rem.
pub fn design_px(px_at_design_rem: f32, rem_px: f32) -> f32 {
    px_at_design_rem * rem_px / DESIGN_REM
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    /// A polyline break: the decimator emits one where a non-finite value
    /// ends a segment; the path builder starts a new subpath after it.
    pub const BREAK: Point = Point {
        x: f32::NAN,
        y: f32::NAN,
    };
    pub fn is_break(&self) -> bool {
        self.x.is_nan()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn design_rem_mirrors_the_shells_medium_rem() {
        assert_eq!(DESIGN_REM, 12.0);
        assert_eq!(design_px(44.0, 12.0), 44.0, "identity at the design rem");
        assert_eq!(design_px(12.0, 14.0), 14.0, "scales with the rem");
    }
}
