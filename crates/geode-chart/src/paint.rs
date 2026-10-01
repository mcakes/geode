//! The gpui-dependent chart kit: what every element shares once it has a
//! window. Rebuild counters, one pane side's resolved y axis, the theme
//! colors a frame paints its chrome in, grid and axis painters, and stroke
//! builders. `core` holds the window-free geometry these are built from.

use std::cell::Cell;

use gpui::{
    App, Bounds, Hsla, Path, PathBuilder, Pixels, SharedString, TextAlign, Window, point, px, size,
};
use gpui_component::ActiveTheme;
use gpui_component::plot::{AxisLabelSide, AxisText, Grid, PlotAxis};

use crate::core::axis::{Axis, Pane, Side};
use crate::core::decimate::decimate;
use crate::core::marks::Segment;
use crate::core::scale::LinearScale;
use crate::core::time::Tick;
use crate::core::{Point, Rect, Y_TICK_GAP, design_px};

thread_local! {
    /// Per THREAD, not per process: the counter is read as a delta
    /// across a few frames, and two window tests running in parallel on
    /// their own threads would otherwise each see the other's paints.
    /// Windows on the same UI thread contribute to the same counter.
    static REBUILDS: Cell<usize> = const { Cell::new(0) };
    /// The same, for the chrome derivation (the four side scales, their
    /// ticks and labels, the x ticks) — the O(n) work that is invisible
    /// to [`REBUILDS`] because it never touches a path.
    static CHROME_REBUILDS: Cell<usize> = const { Cell::new(0) };
}

/// Polyline and percentile path rebuilds on this thread since it started.
pub fn rebuilds() -> usize {
    REBUILDS.with(|c| c.get())
}

/// How many times the chrome — the side scales, the y ticks and their
/// labels, the x ticks — was derived on this thread since it started.
/// A frame that changed nothing must not move this either.
pub fn chrome_rebuilds() -> usize {
    CHROME_REBUILDS.with(|c| c.get())
}

pub(crate) fn note_rebuild() {
    REBUILDS.with(|c| c.set(c.get() + 1));
}

pub(crate) fn note_chrome_rebuild() {
    CHROME_REBUILDS.with(|c| c.set(c.get() + 1));
}

/// The polyline's stroke width, in device pixels (not on the rem scale:
/// a hairline is a hairline).
pub(crate) const LINE_WIDTH: f32 = 1.5;

/// One pane side's resolved y axis: the scale over that side's VISIBLE
/// values and the ticks it paints, labels already formatted.
///
/// Every field is a function of the chrome key's inputs alone, so the
/// whole thing is derived on a chrome miss and only then — the scale in
/// particular is a scan of every visible value of every slot on the
/// side, which at the 500,000-point cap is the one piece of O(n) work
/// that could otherwise land on the render thread every frame.
#[derive(Default, Clone)]
pub(crate) struct SideAxis {
    pub(crate) scale: Option<LinearScale>,
    pub(crate) ticks: Vec<f64>,
    pub(crate) labels: Vec<SharedString>,
}

impl SideAxis {
    pub(crate) fn clear(&mut self) {
        self.scale = None;
        self.ticks.clear();
        self.labels.clear();
    }

    /// Resolve this side for `scale`: about `hint` nice ticks and one label
    /// per tick from `label(value, step)`.
    pub(crate) fn fill(
        &mut self,
        scale: LinearScale,
        hint: usize,
        label: impl Fn(f64, f64) -> String,
    ) {
        let step = scale.step_for(hint);
        scale.ticks(hint, &mut self.ticks);
        self.labels.clear();
        for v in &self.ticks {
            self.labels.push(SharedString::from(label(*v, step)));
        }
        self.scale = Some(scale);
    }
}

/// The two `Vec`s the decimation path reuses: the plot-relative x of
/// every visible bucket, and the decimated points it produces. Kept
/// together so one `mem::take` moves both.
#[derive(Default)]
pub(crate) struct Scratch {
    pub(crate) xs: Vec<f32>,
    pub(crate) pts: Vec<Point>,
}

/// The theme colors one frame paints its chrome in, read once.
#[derive(Clone, Copy)]
pub(crate) struct Ink {
    pub(crate) line: Hsla,
    pub(crate) text: Hsla,
    pub(crate) strip: Hsla,
}

impl Ink {
    /// The chrome colors of the current theme.
    pub(crate) fn read(cx: &App) -> Ink {
        let theme = cx.theme();
        Ink {
            line: theme.border,
            text: theme.muted_foreground,
            strip: theme.background,
        }
    }
}

/// About how many y ticks a pane of `h` pixels is worth.
pub(crate) fn y_tick_hint(h: f32, rem_px: f32) -> usize {
    (h / design_px(Y_TICK_GAP, rem_px)).max(2.0) as usize
}

/// A pane's grid: a vertical rule per x tick and a horizontal one per y
/// tick of `grid` (the side the pane's grid follows). The two collects
/// allocate the line vectors `Grid` requires.
pub(crate) fn paint_grid(
    plot: Rect,
    x_ticks: &[Tick],
    grid: &SideAxis,
    bounds: Bounds<Pixels>,
    ink: Ink,
    window: &mut Window,
) {
    let gx: Vec<Pixels> = x_ticks.iter().map(|t| px(t.x - plot.x)).collect();
    let gy: Vec<Pixels> = grid
        .scale
        .map(|s| grid.ticks.iter().map(|v| px(s.y(*v) - plot.y)).collect())
        .unwrap_or_default();
    Grid::new()
        .x(gx)
        .y(gy)
        .stroke(ink.line)
        .dash_array(&[px(4.), px(2.)])
        .paint(&bounds_of(plot, bounds), window);
}

/// One pane side's axis line and its prepared tick labels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_y_axis(
    r: Rect,
    s: &LinearScale,
    axis: &SideAxis,
    side: Side,
    bounds: Bounds<Pixels>,
    ink: Ink,
    window: &mut Window,
    cx: &mut App,
) {
    let (line_x, label_side, align) = match side {
        Side::Left => (r.w, AxisLabelSide::Start, TextAlign::Right),
        Side::Right => (0.0, AxisLabelSide::End, TextAlign::Left),
    };
    PlotAxis::new()
        .x_axis(false)
        .y_axis(true)
        .y(px(line_x))
        .y_label_side(label_side)
        .y_label(axis.ticks.iter().zip(axis.labels.iter()).map(|(v, label)| {
            AxisText::new(label.clone(), px(s.y(*v) - r.y), ink.text).align(align)
        }))
        .stroke(ink.line)
        .paint(&bounds_of(r, bounds), window, cx);
}

/// The one shared x axis, under the lowest pane. A tick's `x` is in layout
/// space and the axis rect starts at the plot's left edge.
pub(crate) fn paint_x_axis(
    x_axis: Rect,
    ticks: &[Tick],
    bounds: Bounds<Pixels>,
    ink: Ink,
    window: &mut Window,
    cx: &mut App,
) {
    PlotAxis::new()
        .x(px(0.))
        .x_label(ticks.iter().map(|t| {
            AxisText::new(t.label.clone(), px(t.x - x_axis.x), ink.text).align(TextAlign::Center)
        }))
        .stroke(ink.line)
        .paint(&bounds_of(x_axis, bounds), window, cx);
}

pub(crate) fn pane_index(pane: Pane) -> usize {
    match pane {
        Pane::Upper => 0,
        Pane::Lower => 1,
    }
}

/// The one [`Axis`] a `(pane, side)` pair names.
pub(crate) fn axis_of(pane: Pane, side: Side) -> Axis {
    match (pane, side) {
        (Pane::Upper, Side::Left) => Axis::Left,
        (Pane::Upper, Side::Right) => Axis::Right,
        (Pane::Lower, Side::Left) => Axis::BottomLeft,
        (Pane::Lower, Side::Right) => Axis::BottomRight,
    }
}

/// An axis's slot in an element's four [`SideAxis`] — its position in
/// `Axis::ALL`.
pub(crate) fn axis_index(axis: Axis) -> usize {
    match axis {
        Axis::Left => 0,
        Axis::Right => 1,
        Axis::BottomLeft => 2,
        Axis::BottomRight => 3,
    }
}

pub(crate) fn side_scale_of(side: Side, left: &SideAxis, right: &SideAxis) -> Option<LinearScale> {
    match side {
        Side::Left => left.scale,
        Side::Right => right.scale,
    }
}

/// Whether a y coordinate is inside a pane's plot rect, ends included.
pub(crate) fn inside(y: f32, plot: Rect) -> bool {
    y.is_finite() && y >= plot.y && y <= plot.bottom()
}

/// A layout rect (zero origin) as window bounds.
pub(crate) fn bounds_of(r: Rect, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        bounds.origin + point(px(r.x), px(r.y)),
        size(px(r.w), px(r.h)),
    )
}

/// Decimate `values` at the plot-relative, ascending `scratch.xs` into
/// `scratch.pts`, then rewrite those points in layout coordinates
/// (`plot.x + x`, `y.y(value)`). Breaks stay breaks.
pub(crate) fn decimated_points(plot: Rect, y: &LinearScale, values: &[f64], scratch: &mut Scratch) {
    let Scratch { xs, pts } = scratch;
    decimate(xs, values, plot.w.max(1.0) as usize, pts);
    for p in pts.iter_mut() {
        if !p.is_break() {
            *p = Point::new(plot.x + p.x, y.y(p.y as f64));
        }
    }
}

/// One stroke through `pts`, a new subpath after every break.
pub(crate) fn stroke_points(pts: &[Point], width: f32) -> Option<Path<Pixels>> {
    if pts.is_empty() {
        return None;
    }
    let mut builder = PathBuilder::stroke(px(width));
    let mut move_next = true;
    for p in pts {
        if p.is_break() {
            move_next = true;
            continue;
        }
        let at = point(px(p.x), px(p.y));
        if move_next {
            builder.move_to(at);
            move_next = false;
        } else {
            builder.line_to(at);
        }
    }
    builder.build().ok()
}

/// One stroke path over independent segments.
pub(crate) fn stroke_segments(segments: &[Segment], width: f32) -> Option<Path<Pixels>> {
    if segments.is_empty() {
        return None;
    }
    let mut builder = PathBuilder::stroke(px(width));
    for (a, b) in segments {
        builder.move_to(point(px(a.x), px(a.y)));
        builder.line_to(point(px(b.x), px(b.y)));
    }
    builder.build().ok()
}

/// The decimated polyline of one series, stroked at [`LINE_WIDTH`].
pub(crate) fn stroke_polyline(
    plot: Rect,
    y: &LinearScale,
    values: &[f64],
    scratch: &mut Scratch,
) -> Option<Path<Pixels>> {
    decimated_points(plot, y, values, scratch);
    stroke_points(&scratch.pts, LINE_WIDTH)
}

/// How many dashes a horizontal run of `width` carries at `dash` on and
/// `gap` off. A dashless pattern is one solid segment; a run with no
/// width has none at all.
pub fn dash_count(width: f32, dash: f32, gap: f32) -> usize {
    if width <= 0.0 || width.is_nan() {
        return 0;
    }
    if dash <= 0.0 {
        return 1;
    }
    let period = dash + gap.max(0.0);
    if period <= 0.0 {
        return 1;
    }
    (width / period).ceil() as usize
}

/// A dashed horizontal line from `x0` to `x1` at `y`, one `move_to`/
/// `line_to` pair per dash.
pub(crate) fn dashed_horizontal(
    x0: f32,
    x1: f32,
    y: f32,
    dash: f32,
    gap: f32,
) -> Option<Path<Pixels>> {
    let width = x1 - x0;
    let count = dash_count(width, dash, gap);
    if count == 0 {
        return None;
    }
    let (on, period) = if dash <= 0.0 {
        (width, width)
    } else {
        (dash, dash + gap.max(0.0))
    };
    let mut builder = PathBuilder::stroke(px(1.));
    for k in 0..count {
        let start = x0 + k as f32 * period;
        let end = (start + on).min(x1);
        if end <= start {
            continue;
        }
        builder.move_to(point(px(start), px(y)));
        builder.line_to(point(px(end), px(y)));
    }
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_percentile_line_is_dashed_at_dash_and_gap() {
        assert_eq!(dash_count(100.0, 4.0, 3.0), 15, "ceil(100 / 7)");
        assert_eq!(dash_count(7.0, 4.0, 3.0), 1);
        assert_eq!(dash_count(0.0, 4.0, 3.0), 0);
        assert_eq!(
            dash_count(10.0, 0.0, 3.0),
            1,
            "no dash length: one solid segment"
        );
    }
}
