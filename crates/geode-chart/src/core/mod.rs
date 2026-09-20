//! Window-free geometry (spec §8.1). Everything is `Copy` or borrows;
//! nothing allocates except into a caller-owned buffer.

pub mod axis;
pub mod decimate;
pub mod layout;
pub mod palette;
pub mod scale;
pub mod time;
pub mod view;

/// The rem every design-pixel constant below is authored at — the value
/// of `geode_shell::shell::scale::DESIGN_REM` (`FontSize::Medium`),
/// duplicated because this crate must not depend on the shell.
pub const DESIGN_REM: f32 = 12.0;

/// Spec §8 constants, in design pixels at [`DESIGN_REM`].
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

/// Most density bars one FRAME paints, across both panes and every
/// visible slot.
///
/// A bar is one `paint_quad` with no cache behind it, and the rendering
/// spike (`docs/superpowers/spikes/2026-08-29-gpui-chart-rendering-spike.md`)
/// measured per-cell `paint_quad` blowing up past about 5,000 quads —
/// 10,000 cost 42 ms, six times a 60 Hz frame. Nothing in the model
/// bounds the product: `geode_core::series::MAX_BINS` is 200 and a tile
/// may hold many slots, so nine slots with density on would be ~1,800
/// quads and a dozen more would cross the cliff. The element counts the
/// bars it paints and stops at this bound, per pane in slot order, so
/// the render thread's density cost has a ceiling whatever a module
/// asks for.
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
    /// A polyline break: the decimator emits one where a `NaN` value
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
