//! Xy chart painting with cached scales, labels and data paths.
//!
//! Layout uses a zero origin so cached paths can be translated to the
//! element's current bounds. Each pane paints its grid and axes, then every
//! visible slot the view shows something of: a line as a solid or dashed
//! stroke, over a translucent fill to its axis's zero when the slot asks for
//! one, a points slot as a diamond per point with a vertical bar over its
//! range, or from its mid to the one end it has. One x axis sits below the
//! lowest pane.
//!
//! The crosshair sits on a quoted point that has a mark when the cursor is
//! within a few pixels of one and otherwise follows the cursor; its tooltip
//! reads every visible slot at that x, a line between its knots and a points
//! slot at the point the crosshair is on. Resolving it is, per slot, a binary
//! search and a walk over the points within the snap radius, and allocates
//! nothing; building the tooltip formats a row per slot.
//!
//! State lives under the element's stable, unique ID. Two caches avoid
//! repeating data-dependent preparation on unchanged paints:
//!
//! * `Buffers` caches side scales, y ticks and labels, and x ticks. Its key
//!   includes model version, slot count, view, bounds size and rem.
//!   Changing one of these inputs derives the chart chrome again.
//! * [`PathCaches`] caches decimation, dashing and tessellation, a stroke
//!   path per slot and a fill path per filled line. A key includes model
//!   version, slot number, pane, view, plot geometry and rem, which sizes
//!   the dashes and the markers; the fill's zero is the side scale's, which
//!   those inputs determine.
//!
//! Callers must bump [`XyModel::version`] whenever model contents change:
//! the keys do not independently include every model field.
//!
//! Warm paints still allocate. The component clones and translates cached
//! paths before painting, and its grid and axis interfaces collect vectors.
//! A line's path follows decimated output: up to two extrema per finite run
//! per pixel column, plus breaks, and a dashed line's dashes are bounded by
//! the plot rectangle. A fill outlines the same decimated points, two more
//! per finite run. A points slot carries up to five segments for every
//! point in view. A path of separate segments holds [`MAX_STROKE_SEGMENTS`]
//! at most: a points slot whose points in view, at five segments each, come
//! to more than that paints every k-th point and the last, and a dashed line
//! with more dashes than that is stroked solid.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Bounds, ContentMask, ElementId, Hsla, IntoElement, Path, Pixels, SharedString,
    Window, point, px,
};
use gpui_component::plot::tooltip::{CrossLine, Tooltip, TooltipState};
use gpui_component::plot::{IntoPlot, PathCaches, Plot, ShapeKey};

use super::model::{SlotKind, Style, XyModel, XySlot, YFormat};
use crate::core::axis::{Axis, Pane, Side};
use crate::core::layout::{Layout, PaneRects};
use crate::core::linear::{LinearX, XFormat, delta_label_with, step_decimals, x_ticks};
use crate::core::marks::{
    Clip, MARKER_R, SEGMENTS_PER_MARK, Segment, dash_polyline, fill_base, fill_outlines,
    mark_stride, point_marks, strided,
};
use crate::core::scale::{
    LinearScale, axis_domain, fmt_percent, fmt_tick, fmt_value, unsigned_zero,
};
use crate::core::view::View;
use crate::core::{DASH, GAP, Point, Rect, TICK_GAP, Tick, design_px};
use crate::paint::{
    Ink, LINE_WIDTH, MAX_STROKE_SEGMENTS, Scratch, SideAxis, TOOLTIP_GAP, bounds_of,
    decimated_points, fill_points, note_chrome_rebuild, note_rebuild, paint_pane_frame,
    paint_x_axis, side_scale_of, stroke_points, stroke_segments, y_tick_hint,
};

/// Element-state key of the reused buffers, within this element's scope.
const BUFFERS: &str = "geode-xy-buffers";
/// Element-state key of the per-pane shape caches.
const SHAPES: &str = "geode-xy-shapes";

/// The snap radius, in design pixels at the design rem: how near along x
/// the cursor must be to a quoted point for the crosshair to sit on it.
/// Farther than this from every quote, the crosshair follows the cursor.
const SNAP: f32 = 8.0;

// The crosshair's x rides in `TooltipState::index` as its bit pattern. A
// narrower `usize` would cut it and the tooltip would read another x.
const _: () = assert!(usize::BITS >= u64::BITS);

/// What a readout shows where there is no value.
const NONE: &str = "—";

/// A filled line's fill opacity, against the slot's own color (its alpha
/// included): the region reads as shading under the stroke, and the grid
/// and any other slot show through it.
pub const FILL_OPACITY: f32 = 0.3;

/// A y value as its axis reads it in a readout: finer than a tick label,
/// and never a signed zero.
fn fmt_y(v: f64, format: YFormat) -> String {
    match format {
        YFormat::Plain => fmt_value(v),
        YFormat::Percent => unsigned_zero(format!("{:.2}%", v * 100.0)),
    }
}

/// Whether a point's range paints a bar on its own: both ends finite and
/// apart, as `core::marks::point_marks` asks of them.
fn has_range(lo: f64, hi: f64) -> bool {
    lo.is_finite() && hi.is_finite() && lo != hi
}

/// Whether a point has a mark, as `core::marks::point_marks` draws it: a
/// diamond at a finite mid, with or without a bar, or a bar over a range
/// with no mid. One finite end with no mid has nothing to run to and
/// paints nothing.
fn paints(mid: f64, lo: f64, hi: f64) -> bool {
    mid.is_finite() || has_range(lo, hi)
}

/// One slot's readout at `u`. A line is read between its knots. A points
/// slot shows the nearest of its points in `window` when that lies within
/// `tol` of `u`: `mid  lo / hi`, with a dash in place of a mid or of one
/// end it does not have, and the mid alone when it has neither end or its
/// ends meet. A dash alone when the slot has nothing there, or a point
/// that paints no mark.
///
/// Of several points at the nearest x the one read is the first that
/// paints a mark, which is where the crosshair snaps: read by index
/// alone, a point with no quote would show a dash beside the mark of the
/// point it shares an x with. Two painted points at one x read the first.
///
/// The tooltip passes the slot's view window, so a point just past the
/// plot's edge, which is not painted, is not read from a cursor at the
/// edge. A line ignores the window.
pub(crate) fn readout(
    slot: &XySlot,
    window: (usize, usize),
    u: f64,
    tol: f64,
    format: YFormat,
) -> String {
    match &slot.kind {
        SlotKind::Line { .. } => slot
            .line_value_at(u)
            .map_or_else(|| NONE.to_string(), |v| fmt_y(v, format)),
        SlotKind::Points { xs, mid, lo, hi } => {
            let Some(nearest) = slot.nearest_in(window, u) else {
                return NONE.to_string();
            };
            let x = xs[nearest];
            // Asked as "is it near", so a tolerance that is not a number
            // accepts no point instead of every point.
            let near = (x - u).abs() <= tol;
            if !near {
                return NONE.to_string();
            }
            // The run of points at that x, within the window, from its
            // first: `nearest` is the run's first or its last.
            let (start, end) = (window.0, window.1.min(slot.len()));
            let first = start + xs[start..nearest].partition_point(|v| *v < x);
            let painted = (first..end)
                .take_while(|i| xs[*i] == x)
                .find(|i| paints(mid[*i], lo[*i], hi[*i]));
            let Some(i) = painted else {
                return NONE.to_string();
            };
            let (m, l, h) = (mid[i], lo[i], hi[i]);
            let read = |v: f64| {
                if v.is_finite() {
                    fmt_y(v, format)
                } else {
                    NONE.to_string()
                }
            };
            // A quote with one side missing says so: read as its mid
            // alone it would pass for a quote with no spread.
            let one_sided = l.is_finite() != h.is_finite();
            if has_range(l, h) || one_sided {
                format!("{}  {} / {}", read(m), read(l), read(h))
            } else {
                read(m)
            }
        }
    }
}

/// The tooltip's title: x at the crosshair. `tol` is one pixel column in x
/// units. Each format prints its own decimals, finer than a tick label, or
/// the decimals a step of `tol` needs when that is more, so two cursor
/// positions a pixel apart never read the same x. The column asks for six
/// decimals at most; a tolerance that is not a positive number asks for
/// none. Never a signed zero.
pub(crate) fn title(u: f64, format: XFormat, tol: f64) -> String {
    unsigned_zero(match format {
        XFormat::Price => format!("{u:.*}", step_decimals(tol).max(2)),
        XFormat::Percent => {
            let decimals = step_decimals(tol * 100.0).max(1);
            format!("{:.decimals$}%", u * 100.0)
        }
        XFormat::Fixed(n) => format!("{u:.*}", step_decimals(tol).max(n as usize + 2)),
        XFormat::Delta => delta_label_with(u, step_decimals(tol * 100.0).max(1)),
    })
}

/// Element state kept across frames: the reused buffers and the chrome of
/// the last chrome key.
#[derive(Default)]
pub(crate) struct Buffers {
    pub(crate) scratch: Scratch,
    /// A line's values in ascending pixel order, beside `scratch.xs`.
    vals: Vec<f64>,
    segments: Vec<Segment>,
    /// A filled line's outlines, parted by breaks.
    outlines: Vec<Point>,
    /// A points slot's pixel columns: x, mid, low and high.
    px: [Vec<f32>; 4],
    chrome_key: Option<u64>,
    x_ticks: Vec<Tick>,
    /// Indexed by [`Axis::index`]; `Axis::ALL` order.
    sides: [SideAxis; 4],
}

/// What one frame's pane painters share.
struct Paint<'a> {
    bounds: Bounds<Pixels>,
    x_ticks: &'a [Tick],
    sides: &'a [SideAxis; 4],
    ink: Ink,
}

#[derive(IntoPlot)]
pub struct XyElement {
    model: Arc<XyModel>,
    view: View,
    rem_px: f32,
    id: ElementId,
}

impl XyElement {
    /// Create a chart with a stable ID unique among chart elements in the
    /// window; its buffers and path caches live under it across frames.
    /// Reusing it for another chart can serve paths from the wrong model
    /// when their version and geometry keys coincide.
    pub fn new(model: Arc<XyModel>, view: View, rem_px: f32, id: impl Into<ElementId>) -> Self {
        Self {
            model,
            view,
            rem_px,
            id: id.into(),
        }
    }

    pub(crate) fn scale(&self) -> LinearX {
        self.model.x.scale()
    }

    /// The layout at a zero origin: paths are cached origin-free and
    /// translated by the cache; quads and labels add `bounds.origin`.
    pub(crate) fn layout(&self, bounds: Bounds<Pixels>) -> Layout {
        let r = Rect::new(
            0.0,
            0.0,
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
        );
        Layout::solve(r, self.model.layout_options(self.rem_px))
    }

    /// The padded y domain of a pane side over what the view shows of its
    /// visible slots; `None` when none has a finite value there.
    /// O(visible values): a chrome-miss path only.
    pub(crate) fn side_domain(&self, pane: Pane, side: Side) -> Option<(f64, f64)> {
        let view = self.view;
        axis_domain(
            self.model
                .slots
                .iter()
                .filter(|s| s.visible && s.axis.pane() == pane && s.axis.side() == side)
                .flat_map(|s| s.values_in(s.window(view))),
        )
    }

    /// The crosshair for a cursor at pixel `cursor_x` of `plot`: the x it
    /// reads and the pixel x its line sits at. `None` when the cursor has
    /// no finite x.
    ///
    /// It sits on a quoted point when one is within [`SNAP`] of the cursor
    /// and otherwise glides with the cursor. The candidates are the points
    /// in view of the visible points slots of both panes whose values give
    /// them a mark, since the line spans both panes and the tooltip reads
    /// every slot; the nearest in pixels wins. A point a thinned slot
    /// leaves unpainted is a candidate all the same. A line is read
    /// between its knots, so its knots are never snapped to.
    ///
    /// A view with no span paints every point at one x, so the line sits
    /// there whatever the cursor's x.
    ///
    /// No allocation, and per slot one binary search and a walk over the
    /// points within the radius: this runs on every pointer move.
    pub(crate) fn crosshair_x(&self, cursor_x: f32, plot: Rect) -> Option<(f64, f32)> {
        let scale = self.scale();
        let view = self.view;
        let under = scale.value_at(cursor_x, view, plot);
        if !under.is_finite() {
            return None;
        }
        let span = view.span();
        if span.is_nan() || span <= 0.0 {
            return Some((view.lo, scale.x_of(view.lo, view, plot)));
        }
        let radius = design_px(SNAP, self.rem_px);
        let snapped = self
            .model
            .slots
            .iter()
            .filter(|s| s.visible)
            .filter_map(|s| self.snap_candidate(s, under, cursor_x, plot, radius))
            .min_by(|a, b| (a.1 - cursor_x).abs().total_cmp(&(b.1 - cursor_x).abs()));
        Some(snapped.unwrap_or((under, cursor_x)))
    }

    /// The point of `slot` the crosshair may sit on, as its x and its
    /// pixel x: the nearest to pixel `cursor_x` among the slot's points in
    /// view whose values give them a mark ([`paints`]), when that is within
    /// `radius` pixels. `None` for a line. `under` is the x under the
    /// cursor.
    ///
    /// The search runs outward from `under`, both ways, through the slot's
    /// view window. A point past the plot's edge is outside the window and
    /// one with neither a mid nor a range has no mark, so neither is a
    /// candidate, and neither hides a marked neighbour that is in reach.
    /// Each walk ends at the first point with a mark or the first beyond
    /// the radius, so the work is bounded by the points within the radius.
    fn snap_candidate(
        &self,
        slot: &XySlot,
        under: f64,
        cursor_x: f32,
        plot: Rect,
        radius: f32,
    ) -> Option<(f64, f32)> {
        let SlotKind::Points { xs, mid, lo, hi } = &slot.kind else {
            return None;
        };
        let (start, end) = slot.window(self.view);
        let split = start + xs[start..end].partition_point(|x| *x < under);
        let scale = self.scale();
        let in_reach = |i: usize| {
            let x = scale.x_of(xs[i], self.view, plot);
            ((x - cursor_x).abs() <= radius).then_some((i, x))
        };
        let painted = |(i, _): &(usize, f32)| paints(mid[*i], lo[*i], hi[*i]);
        let below = (start..split).rev().map_while(in_reach).find(painted);
        let above = (split..end).map_while(in_reach).find(painted);
        [below, above]
            .into_iter()
            .flatten()
            .min_by(|a, b| (a.1 - cursor_x).abs().total_cmp(&(b.1 - cursor_x).abs()))
            .map(|(i, x)| (xs[i], x))
    }

    /// The tooltip's title and one (color, label, readout) row per visible
    /// slot, for a crosshair at `u` over `plot`.
    pub(crate) fn tooltip_rows(
        &self,
        u: f64,
        plot: Rect,
    ) -> (String, Vec<(Hsla, SharedString, String)>) {
        let view = self.view;
        // One pixel column, in x units: how near `u` a point must be to be
        // read, and how fine the title must be. A snapped crosshair is on
        // its point's own x. A gliding one seldom has a point this near;
        // when it has, that point reads.
        let tol = view.span() / plot.w as f64;
        let rows = self
            .model
            .slots
            .iter()
            .filter(|s| s.visible)
            .map(|slot| {
                let format = self.model.y_format_of(slot.axis);
                let text = readout(slot, slot.window(view), u, tol, format);
                (slot.color, slot.label.clone(), text)
            })
            .collect();
        (title(u, self.model.x.format, tol), rows)
    }

    fn y_label(format: YFormat) -> impl Fn(f64, f64) -> String {
        move |v, step| match format {
            YFormat::Plain => fmt_tick(v, step),
            YFormat::Percent => fmt_percent(v, step),
        }
    }

    /// Derive the whole chrome: the x ticks and, per pane side, the scale,
    /// its ticks and their labels. Called on a chrome-key miss only.
    fn derive_chrome(
        &self,
        layout: &Layout,
        x_ticks_out: &mut Vec<Tick>,
        sides: &mut [SideAxis; 4],
    ) {
        note_chrome_rebuild();
        x_ticks(
            self.scale(),
            self.view,
            layout.x_axis,
            design_px(TICK_GAP, self.rem_px),
            self.model.x.format,
            x_ticks_out,
        );
        for (pane, rects) in [
            (Pane::Upper, Some(layout.upper)),
            (Pane::Lower, layout.lower),
        ] {
            for side in [Side::Left, Side::Right] {
                let axis_id = Axis::of(pane, side);
                let axis = &mut sides[axis_id.index()];
                axis.clear();
                let Some(rects) = rects else { continue };
                let plot = rects.plot;
                if plot.w <= 0.0 || plot.h <= 0.0 {
                    continue;
                }
                let Some(domain) = self.side_domain(pane, side) else {
                    continue;
                };
                axis.fill(
                    LinearScale::new(domain, plot.y, plot.bottom()),
                    y_tick_hint(plot.h, self.rem_px),
                    Self::y_label(self.model.y_format_of(axis_id)),
                );
            }
        }
    }

    /// Fill `scratch.xs` and `vals` with a line's visible window in
    /// ascending pixel order (a reversed axis is walked backwards), then
    /// leave its decimated layout polyline in `scratch.pts`.
    pub(crate) fn line_points(&self, slot: &XySlot, plot: Rect, y: &LinearScale, b: &mut Buffers) {
        let SlotKind::Line { xs, ys, .. } = &slot.kind else {
            b.scratch.pts.clear();
            return;
        };
        let (start, end) = slot.window(self.view);
        let scale = self.scale();
        b.scratch.xs.clear();
        b.vals.clear();
        let mut push = |i: usize| {
            b.scratch
                .xs
                .push(scale.x_of(xs[i], self.view, plot) - plot.x);
            b.vals.push(ys[i]);
        };
        if scale.reversed {
            (start..end).rev().for_each(&mut push);
        } else {
            (start..end).for_each(&mut push);
        }
        decimated_points(plot, y, &b.vals, &mut b.scratch);
    }

    /// One slot's path at a zero origin: a stroked or dashed polyline, or
    /// the bars and diamonds of a points slot. `None` when the view shows
    /// nothing of it.
    fn shape(
        &self,
        slot: &XySlot,
        plot: Rect,
        y: &LinearScale,
        b: &mut Buffers,
    ) -> Option<Path<Pixels>> {
        match &slot.kind {
            SlotKind::Line { .. } => {
                self.line_points(slot, plot, y, b);
                match slot.style {
                    Style::Solid => stroke_points(&b.scratch.pts, LINE_WIDTH),
                    Style::Dashed => {
                        // `scratch.pts` is in layout coordinates, the plot
                        // rectangle's own: the clip is the plot as it is.
                        let clip = Clip {
                            x0: plot.x,
                            y0: plot.y,
                            x1: plot.right(),
                            y1: plot.bottom(),
                        };
                        dash_polyline(
                            &b.scratch.pts,
                            design_px(DASH, self.rem_px),
                            design_px(GAP, self.rem_px),
                            clip,
                            &mut b.segments,
                        );
                        if b.segments.len() > MAX_STROKE_SEGMENTS {
                            // More dashes than one path holds: the line
                            // solid, never no line.
                            stroke_points(&b.scratch.pts, LINE_WIDTH)
                        } else {
                            stroke_segments(&b.segments, LINE_WIDTH)
                        }
                    }
                }
            }
            SlotKind::Points { xs, mid, lo, hi } => {
                let (start, end) = slot.window(self.view);
                let scale = self.scale();
                let [px_x, px_mid, px_lo, px_hi] = &mut b.px;
                px_x.clear();
                px_mid.clear();
                px_lo.clear();
                px_hi.clear();
                // More marks than one path holds are thinned evenly, the
                // last point kept, rather than the slot going unpainted.
                let stride = mark_stride(end - start, SEGMENTS_PER_MARK, MAX_STROKE_SEGMENTS);
                for i in strided(start, end, stride) {
                    px_x.push(scale.x_of(xs[i], self.view, plot));
                    px_mid.push(y.y(mid[i]));
                    px_lo.push(y.y(lo[i]));
                    px_hi.push(y.y(hi[i]));
                }
                point_marks(
                    px_x,
                    px_mid,
                    px_lo,
                    px_hi,
                    design_px(MARKER_R, self.rem_px),
                    &mut b.segments,
                );
                stroke_segments(&b.segments, LINE_WIDTH)
            }
        }
    }

    /// A filled line's fill at a zero origin: its decimated polyline closed
    /// down (or up) to the y of its axis's zero, held to the plot, one
    /// outline per finite run. `None` for a slot that is not a filled line
    /// or when the view shows nothing of it. Decimates the line afresh, as
    /// its stroke does: each is built on its own cache miss.
    fn fill_shape(
        &self,
        slot: &XySlot,
        plot: Rect,
        y: &LinearScale,
        b: &mut Buffers,
    ) -> Option<Path<Pixels>> {
        if !matches!(slot.kind, SlotKind::Line { fill: true, .. }) {
            return None;
        }
        self.line_points(slot, plot, y, b);
        let base = fill_base(y.y(0.0), plot.y, plot.bottom());
        fill_outlines(&b.scratch.pts, base, &mut b.outlines);
        fill_points(&b.outlines)
    }

    /// Paint one pane in layers: grid, axes, then every visible slot's
    /// path in slot order, a filled line's fill under its own stroke.
    fn paint_pane(
        &self,
        pane: Pane,
        rects: &PaneRects,
        ctx: &Paint<'_>,
        buffers: &mut Buffers,
        window: &mut Window,
        cx: &mut App,
    ) {
        let plot = rects.plot;
        let bounds = ctx.bounds;
        let left = &ctx.sides[Axis::of(pane, Side::Left).index()];
        let right = &ctx.sides[Axis::of(pane, Side::Right).index()];
        if !paint_pane_frame(rects, ctx.x_ticks, left, right, bounds, ctx.ink, window, cx) {
            return;
        }

        let model = &*self.model;
        let view = self.view;

        // Data is clipped to the pane's own plot rect. A path is cached at
        // a zero origin and translated, and the element's own mask is the
        // whole element, so without this a line's knot beyond the view
        // paints over the axis column and a marker at the pane's edge runs
        // into the neighbouring pane. `with_content_mask` intersects with
        // the mask already in force, so this only ever narrows.
        let mask = ContentMask {
            bounds: bounds_of(plot, bounds),
        };
        window.with_content_mask(Some(mask), |window| {
            let caches = PathCaches::for_paint((SHAPES, pane.index()), window, cx);
            caches.update(cx, |caches, _| {
                for (k, slot) in model.slots.iter().enumerate() {
                    if !slot.visible || slot.axis.pane() != pane {
                        continue;
                    }
                    let Some(y) = side_scale_of(slot.axis.side(), left, right) else {
                        continue;
                    };
                    // A slot the view shows nothing of has no path to
                    // build, on this frame or any other at this view.
                    let (start, end) = slot.window(view);
                    if start >= end {
                        continue;
                    }
                    let key = ShapeKey::new((model.version, slot.number, pane as u8, view.key()))
                        .f32(plot.x)
                        .f32(plot.y)
                        .f32(plot.w)
                        .f32(plot.h)
                        .f32(self.rem_px)
                        .finish();
                    let (stroke, fill) = caches.slot_pair(k);
                    if matches!(slot.kind, SlotKind::Line { fill: true, .. }) {
                        let path = fill.get(key, bounds.origin, || {
                            note_rebuild();
                            self.fill_shape(slot, plot, &y, buffers)
                        });
                        if let Some(path) = path {
                            window.paint_path(path, slot.color.opacity(FILL_OPACITY));
                        }
                    }
                    let path = stroke.get(key, bounds.origin, || {
                        note_rebuild();
                        self.shape(slot, plot, &y, buffers)
                    });
                    if let Some(path) = path {
                        window.paint_path(path, slot.color);
                    }
                }
            });
        });
    }
}

impl Plot for XyElement {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let layout = self.layout(bounds);
        let ink = Ink::read(cx);

        // The chrome key: everything the chrome derivation reads. A hit
        // keeps the last frame's ticks, labels and side scales, so the scan
        // of every visible value and the label formatting run on a change
        // only, never per frame. The slot count parts the empty model from
        // a first model that shares its version: served the empty model's
        // chrome, that model has no scales and paints nothing.
        let slots = self.model.slots.len();
        let chrome_key = ShapeKey::new((self.model.version, slots, self.view.key()))
            .f32(bounds.size.width.as_f32())
            .f32(bounds.size.height.as_f32())
            .f32(self.rem_px)
            .finish();

        // Move the buffers out for painting and return them after the
        // path-cache updates. `mem::take` keeps the vector allocations
        // without copying their contents or holding this state's update
        // open while the painters update their own.
        let state = window.use_keyed_state(BUFFERS, cx, |_, _| Buffers::default());
        let mut buffers = state.update(cx, |b, _| std::mem::take(b));
        let warm = buffers.chrome_key == Some(chrome_key);
        buffers.chrome_key = Some(chrome_key);
        let mut x_ticks = std::mem::take(&mut buffers.x_ticks);
        let mut sides = std::mem::take(&mut buffers.sides);
        if !warm {
            self.derive_chrome(&layout, &mut x_ticks, &mut sides);
        }

        let ctx = Paint {
            bounds,
            x_ticks: &x_ticks,
            sides: &sides,
            ink,
        };
        self.paint_pane(Pane::Upper, &layout.upper, &ctx, &mut buffers, window, cx);
        if let Some(lower) = layout.lower.as_ref() {
            self.paint_pane(Pane::Lower, lower, &ctx, &mut buffers, window, cx);
        }

        // The one shared x axis, under the lowest pane.
        paint_x_axis(layout.x_axis, &x_ticks, bounds, ink, window, cx);

        buffers.x_ticks = x_ticks;
        buffers.sides = sides;
        state.update(cx, |b, _| *b = buffers);
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn tooltip_state(
        &self,
        position: gpui::Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        let layout = self.layout(bounds);
        let (x, y) = (position.x.as_f32(), position.y.as_f32());
        let (_, plot) = layout.plot_at(x, y)?;
        let (u, line_x) = self.crosshair_x(x, plot)?;
        // The index is the chosen x's bit pattern, so `tooltip` reads at
        // exactly the x chosen here, a snapped point's x as stored, rather
        // than at one recovered from the line's pixel.
        Some(TooltipState::new(
            u.to_bits() as usize,
            point(px(line_x), position.y),
            vec![],
        ))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: gpui::Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        let layout = self.layout(bounds);
        let plot = layout.upper.plot;
        if plot.w <= 0.0 {
            return None;
        }
        let (title, rows) = self.tooltip_rows(f64::from_bits(state.index as u64), plot);
        let top = plot.y;
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(design_px(TOOLTIP_GAP, self.rem_px)))
            .cross_line(CrossLine::new(state.cross_line).span(top, layout.lowest_bottom() - top))
            .title(title);
        for (color, label, text) in rows {
            tooltip = tooltip.row(color, label, text);
        }
        Some(tooltip.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Point;
    use crate::paint::{chrome_rebuilds, rebuilds};
    use crate::xy::model::XAxis;
    use gpui::{Context, Entity, IntoElement, Render, div, prelude::*, size};

    pub(crate) struct Host {
        pub(crate) model: Arc<XyModel>,
        pub(crate) view: View,
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(XyElement::new(
                self.model.clone(),
                self.view,
                window.rem_size().as_f32(),
                "xy",
            ))
        }
    }

    /// Two curves (one dashed), a quoted chain and a density on the upper
    /// pane, and a difference of points on the lower: five slots.
    pub(crate) fn fixture(reversed: bool) -> Arc<XyModel> {
        let xs: Vec<f64> = (0..200).map(|i| 0.8 + i as f64 * 0.002).collect();
        let curve = |shift: f64| -> Vec<f64> {
            xs.iter()
                .map(|x| 0.2 + (x - 1.0) * (x - 1.0) + shift)
                .collect()
        };
        let chain_x: Vec<f64> = (0..40).map(|i| 0.81 + i as f64 * 0.01).collect();
        let chain_mid: Vec<f64> = chain_x
            .iter()
            .map(|x| 0.2 + (x - 1.0) * (x - 1.0))
            .collect();
        let slot = |number, axis, style, kind| XySlot {
            number,
            label: format!("s{number}").into(),
            color: gpui::red(),
            axis,
            visible: true,
            style,
            kind,
        };
        XyModel::new(
            1,
            XAxis {
                format: XFormat::Percent,
                reversed,
            },
            [
                YFormat::Percent,
                YFormat::Plain,
                YFormat::Plain,
                YFormat::Plain,
            ],
            0.7,
            vec![
                slot(
                    1,
                    Axis::Left,
                    Style::Solid,
                    SlotKind::Line {
                        xs: xs.clone(),
                        ys: curve(0.0),
                        fill: false,
                    },
                ),
                slot(
                    2,
                    Axis::Left,
                    Style::Dashed,
                    SlotKind::Line {
                        xs: xs.clone(),
                        ys: curve(0.01),
                        fill: false,
                    },
                ),
                slot(
                    3,
                    Axis::Left,
                    Style::Solid,
                    SlotKind::Points {
                        xs: chain_x.clone(),
                        mid: chain_mid.clone(),
                        lo: chain_mid.iter().map(|v| v - 0.005).collect(),
                        hi: chain_mid.iter().map(|v| v + 0.005).collect(),
                    },
                ),
                slot(
                    4,
                    Axis::Right,
                    Style::Solid,
                    SlotKind::Line {
                        xs: xs.clone(),
                        ys: curve(1.0),
                        fill: false,
                    },
                ),
                slot(
                    5,
                    Axis::BottomLeft,
                    Style::Solid,
                    SlotKind::Points {
                        xs: chain_x.clone(),
                        mid: vec![0.001; 40],
                        lo: vec![0.001; 40],
                        hi: vec![0.001; 40],
                    },
                ),
            ],
        )
    }

    pub(crate) fn open(
        cx: &mut gpui::TestAppContext,
        model: Arc<XyModel>,
    ) -> (Entity<Host>, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let mut host = None;
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let h = cx.new(|_| Host {
                        // A moneyness range is a fraction of a unit wide:
                        // the time chart's two-unit floor would pin it.
                        view: View::with_min_span(model.full(), 0.01),
                        model: model.clone(),
                    });
                    host = Some(h.clone());
                    cx.new(|cx| gpui_component::Root::new(h, window, cx))
                })
            })
            .unwrap();
        let vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        (host.unwrap(), vcx)
    }

    pub(crate) fn draw(vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn an_unchanged_frame_rebuilds_nothing_and_a_moved_view_rebuilds(
        cx: &mut gpui::TestAppContext,
    ) {
        // Opening the window paints its first frame, so the counts that
        // frame leaves behind are measured from before `open`.
        let before = rebuilds();
        let before_chrome = chrome_rebuilds();
        let (host, mut vcx) = open(cx, fixture(false));
        let first = rebuilds() - before;
        assert_eq!(first, 5, "one path per slot on the first frame");
        assert_eq!(chrome_rebuilds() - before_chrome, 1);
        draw(&mut vcx);
        draw(&mut vcx);
        assert_eq!(
            rebuilds() - before,
            first,
            "an unchanged frame rebuilt nothing"
        );
        assert_eq!(
            chrome_rebuilds() - before_chrome,
            1,
            "and derived no chrome"
        );
        host.update(&mut vcx, |h, cx| {
            let full = h.model.full();
            h.view.zoom(2.0, 0.5, full);
            cx.notify();
        });
        draw(&mut vcx);
        assert_eq!(
            rebuilds() - before,
            2 * first,
            "a moved view rebuilds every path once"
        );
        assert_eq!(chrome_rebuilds() - before_chrome, 2);
    }

    /// The path and chrome rebuilds since `mark`.
    fn since(mark: (usize, usize)) -> (usize, usize) {
        (rebuilds() - mark.0, chrome_rebuilds() - mark.1)
    }

    #[gpui::test]
    fn a_new_model_version_or_a_resize_rebuilds_every_path_and_the_chrome_once(
        cx: &mut gpui::TestAppContext,
    ) {
        let (host, mut vcx) = open(cx, fixture(false));
        draw(&mut vcx);
        let mark = (rebuilds(), chrome_rebuilds());
        // What a data delivery is: the same slots and the same view under
        // the next version.
        let m = fixture(false);
        let next = XyModel::new(m.version + 1, m.x, m.y_format, m.split, m.slots.clone());
        host.update(&mut vcx, |h, cx| {
            h.model = next;
            cx.notify();
        });
        draw(&mut vcx);
        assert_eq!(since(mark), (5, 1), "a new version at an unchanged view");
        // The same model in a window of another width, then another height.
        let viewport = vcx.update(|window, _| window.viewport_size());
        let wider = size(viewport.width + px(120.), viewport.height);
        vcx.simulate_resize(wider);
        draw(&mut vcx);
        assert_eq!(since(mark), (10, 2), "an unchanged model, a wider window");
        vcx.simulate_resize(size(wider.width, wider.height + px(90.)));
        draw(&mut vcx);
        assert_eq!(since(mark), (15, 3), "an unchanged model, a taller window");
        draw(&mut vcx);
        draw(&mut vcx);
        assert_eq!(since(mark), (15, 3), "a further unchanged frame");
    }

    #[gpui::test]
    fn a_rem_change_alone_re_derives_the_chrome(cx: &mut gpui::TestAppContext) {
        let (host, mut vcx) = open(cx, fixture(false));
        draw(&mut vcx);
        let mark = (rebuilds(), chrome_rebuilds());
        // The window, the model and the view stay as they are: the tick
        // gaps and the axis columns are lengths on the rem scale.
        // The root view sets the window's rem from the theme on every
        // render, so the theme is where a rem change is made.
        let rem = vcx.update(|window, _| window.rem_size());
        vcx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = rem + px(4.));
        host.update(&mut vcx, |_, cx| cx.notify());
        draw(&mut vcx);
        assert_eq!(
            vcx.update(|window, _| window.rem_size()),
            rem + px(4.),
            "the rem moved"
        );
        assert_eq!(since(mark).1, 1, "the chrome follows the rem");
        draw(&mut vcx);
        assert_eq!(since(mark).1, 1, "and then holds");
    }

    #[gpui::test]
    fn a_first_model_at_version_zero_does_not_take_the_empty_models_chrome(
        cx: &mut gpui::TestAppContext,
    ) {
        // A restored view is in place before any data: the empty model
        // paints under it, and its chrome has no ticks and no scales.
        let (host, mut vcx) = open(cx, XyModel::empty());
        host.update(&mut vcx, |h, cx| {
            h.view = View::with_min_span((0.8, 1.2), 0.01);
            cx.notify();
        });
        draw(&mut vcx);
        let mark = (rebuilds(), chrome_rebuilds());
        // The first real model, from a builder that also counts from zero.
        let m = fixture(false);
        let first = XyModel::new(0, m.x, m.y_format, m.split, m.slots.clone());
        assert_eq!(first.version, XyModel::empty().version);
        host.update(&mut vcx, |h, cx| {
            h.model = first;
            cx.notify();
        });
        draw(&mut vcx);
        assert_eq!(since(mark), (5, 1), "the axes are derived for the slots");
    }

    #[gpui::test]
    fn an_empty_model_and_a_hidden_lower_pane_paint_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let (host, mut vcx) = open(cx, XyModel::empty());
        draw(&mut vcx);
        let mut slots = fixture(false).slots.clone();
        slots[4].visible = false;
        let m = XyModel::new(2, XAxis::default(), [YFormat::Plain; 4], 0.7, slots);
        host.update(&mut vcx, |h, cx| {
            h.view = View::with_min_span(m.full(), 0.01);
            h.model = m;
            cx.notify();
        });
        draw(&mut vcx);
        // One x value only: a zero-span view.
        let one = XyModel::new(
            3,
            XAxis::default(),
            [YFormat::Plain; 4],
            0.7,
            vec![XySlot {
                number: 1,
                label: "p".into(),
                color: gpui::red(),
                axis: Axis::Left,
                visible: true,
                style: Style::Solid,
                kind: SlotKind::Points {
                    xs: vec![1.0],
                    mid: vec![0.2],
                    lo: vec![0.19],
                    hi: vec![0.21],
                },
            }],
        );
        host.update(&mut vcx, |h, cx| {
            h.view = View::with_min_span(one.full(), 0.01);
            h.model = one;
            cx.notify();
        });
        draw(&mut vcx);
    }

    #[gpui::test]
    fn a_slot_outside_the_view_builds_no_path(cx: &mut gpui::TestAppContext) {
        let (host, mut vcx) = open(cx, fixture(false));
        let mark = rebuilds();
        host.update(&mut vcx, |h, cx| {
            // The chain runs 0.81..1.20; the curves run 0.8..1.198. A view
            // of 0.8..0.805 holds curve knots and no chain point.
            h.view = View {
                lo: 0.8,
                hi: 0.805,
                min_span: 0.001,
            };
            cx.notify();
        });
        draw(&mut vcx);
        assert_eq!(
            rebuilds() - mark,
            3,
            "three lines; neither points slot has a point in view"
        );
    }

    /// A quoted chain over 1.0..1.1 and, when asked for, a curve over
    /// 0.5..0.6 on the same axis, in the given stroke.
    fn chain_beside_a_curve(version: u64, curve: Option<Style>) -> Arc<XyModel> {
        let slot = |number, style, kind| XySlot {
            number,
            label: format!("s{number}").into(),
            color: gpui::red(),
            axis: Axis::Left,
            visible: true,
            style,
            kind,
        };
        let mut slots = vec![slot(
            1,
            Style::Solid,
            SlotKind::Points {
                xs: vec![1.0, 1.05, 1.1],
                mid: vec![0.2, 0.21, 0.22],
                lo: vec![0.195, 0.205, 0.215],
                hi: vec![0.205, 0.215, 0.225],
            },
        )];
        if let Some(style) = curve {
            slots.push(slot(
                2,
                style,
                SlotKind::Line {
                    xs: vec![0.5, 0.55, 0.6],
                    ys: vec![5.0, 6.0, 7.0],
                    fill: false,
                },
            ));
        }
        XyModel::new(version, XAxis::default(), [YFormat::Plain; 4], 0.7, slots)
    }

    /// A view that holds the chain of `chain_beside_a_curve` and lies
    /// wholly to the right of its curve.
    const CHAIN_ONLY: View = View {
        lo: 0.9,
        hi: 1.2,
        min_span: 0.001,
    };

    #[test]
    fn a_line_wholly_outside_the_view_neither_scales_its_side_nor_has_a_shape() {
        let alone = XyElement::new(chain_beside_a_curve(1, None), CHAIN_ONLY, 12.0, "e");
        let (lo, hi) = alone.side_domain(Pane::Upper, Side::Left).unwrap();
        // The chain's own 0.195..0.225, padded by a twentieth of its span.
        assert!((lo - 0.1935).abs() < 1e-12 && (hi - 0.2265).abs() < 1e-12);
        let plot = Rect::new(44.0, 0.0, 400.0, 200.0);
        let y = LinearScale::new((lo, hi), plot.y, plot.bottom());
        for style in [Style::Solid, Style::Dashed] {
            let m = chain_beside_a_curve(1, Some(style));
            let e = XyElement::new(m.clone(), CHAIN_ONLY, 12.0, "f");
            assert_eq!(
                e.side_domain(Pane::Upper, Side::Left),
                Some((lo, hi)),
                "{style:?}: a curve value that is not on screen is not on the axis"
            );
            let mut b = Buffers::default();
            assert!(
                e.shape(&m.slots[1], plot, &y, &mut b).is_none(),
                "{style:?}: the view shows nothing of the curve"
            );
            assert!(e.shape(&m.slots[0], plot, &y, &mut b).is_some());
        }
    }

    #[gpui::test]
    fn a_line_wholly_outside_the_view_counts_no_rebuild(cx: &mut gpui::TestAppContext) {
        let (host, mut vcx) = open(cx, chain_beside_a_curve(1, Some(Style::Solid)));
        let mark = rebuilds();
        host.update(&mut vcx, |h, cx| {
            h.view = CHAIN_ONLY;
            cx.notify();
        });
        draw(&mut vcx);
        assert_eq!(
            rebuilds() - mark,
            1,
            "the chain; the curve's one knot past the edge is no path"
        );
    }

    #[test]
    fn a_side_scales_over_what_the_view_shows_only() {
        let m = fixture(false);
        let whole = XyElement::new(m.clone(), View::with_min_span(m.full(), 0.01), 12.0, "a");
        let (lo, hi) = whole.side_domain(Pane::Upper, Side::Left).unwrap();
        let narrow = XyElement::new(
            m.clone(),
            View {
                lo: 0.99,
                hi: 1.01,
                min_span: 0.001,
            },
            12.0,
            "b",
        );
        let (nlo, nhi) = narrow.side_domain(Pane::Upper, Side::Left).unwrap();
        assert!(
            nhi - nlo < hi - lo,
            "the wings left the view, so the domain tightened"
        );
        // Lowest: the chain's low at 1.00, 0.2 - 0.005. Highest: the dashed
        // curve one knot past either edge, at 0.988 and 1.012, where it is
        // 0.2 + 0.012² + 0.01; the knots inside the view stop at 0.2101.
        // Padded by a twentieth of that span.
        let (raw_lo, raw_hi) = (0.195, 0.210144);
        let pad = 0.0007572;
        assert!((nlo - (raw_lo - pad)).abs() < 1e-9, "{nlo}");
        assert!((nhi - (raw_hi + pad)).abs() < 1e-9, "{nhi}");
        assert!(
            narrow.side_domain(Pane::Lower, Side::Right).is_none(),
            "no slot on that side"
        );
    }

    #[test]
    fn a_marker_sits_at_the_pixel_x_the_line_gives_the_same_value() {
        // A curve and one quote on its middle knot, a quarter of the way
        // along the view, on a plot that starts at neither x = 0 nor y = 0.
        let plot = Rect::new(44.0, 10.0, 400.0, 200.0);
        let y = LinearScale::new((0.0, 2.0), plot.y, plot.bottom());
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;
        for (reversed, want_x) in [(false, 144.0), (true, 344.0)] {
            let slot = |number, kind| XySlot {
                number,
                label: format!("s{number}").into(),
                color: gpui::red(),
                axis: Axis::Left,
                visible: true,
                style: Style::Solid,
                kind,
            };
            let m = XyModel::new(
                1,
                XAxis {
                    format: XFormat::Price,
                    reversed,
                },
                [YFormat::Plain; 4],
                0.7,
                vec![
                    slot(
                        1,
                        SlotKind::Line {
                            xs: vec![0.8, 0.9, 1.2],
                            ys: vec![0.5, 1.0, 1.5],
                            fill: false,
                        },
                    ),
                    slot(
                        2,
                        SlotKind::Points {
                            xs: vec![0.9],
                            mid: vec![1.0],
                            lo: vec![0.8],
                            hi: vec![1.2],
                        },
                    ),
                ],
            );
            let e = XyElement::new(m.clone(), View::with_min_span((0.8, 1.2), 0.01), 12.0, "i");
            let mut b = Buffers::default();
            e.line_points(&m.slots[0], plot, &y, &mut b);
            assert_eq!(b.scratch.pts.len(), 3, "reversed={reversed}");
            let knot = b.scratch.pts[1];
            assert!(near(knot.x, want_x), "reversed={reversed}: {knot:?}");
            assert!(e.shape(&m.slots[1], plot, &y, &mut b).is_some());
            // The bar, then the diamond's four edges from its left tip.
            assert_eq!(b.segments.len(), 5, "reversed={reversed}");
            let (bar_from, bar_to) = b.segments[0];
            let (left_tip, top_tip) = b.segments[1];
            let case = format!("reversed={reversed}: {:?}", b.segments);
            assert!(near(bar_from.x, knot.x) && near(bar_to.x, knot.x), "{case}");
            assert!(
                near(bar_from.y, y.y(0.8)) && near(bar_to.y, y.y(1.2)),
                "{case}"
            );
            assert!(near(top_tip.x, knot.x), "{case}");
            assert!(near(left_tip.x, knot.x - MARKER_R), "{case}");
            assert!(
                near(left_tip.y, knot.y),
                "the quote is on the curve: {case}"
            );
            assert!(near(top_tip.y, knot.y - MARKER_R), "{case}");
        }
    }

    /// One line over `xs`/`ys` on the upper left axis, filled or not.
    fn line_model(version: u64, xs: Vec<f64>, ys: Vec<f64>, fill: bool) -> Arc<XyModel> {
        XyModel::new(
            version,
            XAxis::default(),
            [YFormat::Plain; 4],
            0.7,
            vec![XySlot {
                number: 1,
                label: "density".into(),
                color: gpui::red(),
                axis: Axis::Left,
                visible: true,
                style: Style::Solid,
                kind: SlotKind::Line { xs, ys, fill },
            }],
        )
    }

    /// The outlines a filled line's fill was built from, split at breaks.
    fn outlines(b: &Buffers) -> Vec<Vec<Point>> {
        b.outlines
            .split(|p| p.is_break())
            .map(<[Point]>::to_vec)
            .collect()
    }

    #[test]
    fn a_filled_line_fills_each_finite_run_to_zero_and_a_gap_parts_the_fill() {
        let xs: Vec<f64> = (0..9).map(f64::from).collect();
        let ys = vec![0.0, 1.0, 2.0, 1.0, f64::NAN, 1.0, 2.0, 1.0, 0.0];
        let m = line_model(1, xs.clone(), ys.clone(), true);
        let plot = Rect::new(44.0, 10.0, 400.0, 200.0);
        let y = LinearScale::new((-1.0, 3.0), plot.y, plot.bottom());
        let e = XyElement::new(m.clone(), View::with_min_span(m.full(), 0.01), 12.0, "f");
        let mut b = Buffers::default();
        assert!(e.fill_shape(&m.slots[0], plot, &y, &mut b).is_some());
        let runs = outlines(&b);
        assert_eq!(runs.len(), 2, "the NaN parts the fill: {runs:?}");
        let zero = y.y(0.0);
        for run in &runs {
            // Four knots, framed by their two ends at zero.
            assert_eq!(run.len(), 6, "{run:?}");
            let (first, last) = (run[0], run[run.len() - 1]);
            assert!((first.y - zero).abs() < 1e-3 && (last.y - zero).abs() < 1e-3);
            assert!((first.x - run[1].x).abs() < 1e-3 && (last.x - run[4].x).abs() < 1e-3);
        }
        // The stroke of the same slot is the line alone, and an unfilled
        // line has no fill.
        assert!(e.shape(&m.slots[0], plot, &y, &mut b).is_some());
        let bare = line_model(1, xs, ys, false);
        let e = XyElement::new(bare.clone(), View::with_min_span(m.full(), 0.01), 12.0, "g");
        assert!(e.fill_shape(&bare.slots[0], plot, &y, &mut b).is_none());
    }

    #[test]
    fn a_fill_runs_to_the_plot_edge_when_zero_is_out_of_view_and_up_to_zero_from_below() {
        let plot = Rect::new(44.0, 10.0, 400.0, 200.0);
        let xs: Vec<f64> = (0..5).map(f64::from).collect();
        // Every value well above zero, on a scale that starts at one: zero
        // is below the plot, so the fill runs down to its bottom edge.
        let m = line_model(1, xs.clone(), vec![1.5, 2.0, 2.5, 2.0, 1.5], true);
        let e = XyElement::new(m.clone(), View::with_min_span(m.full(), 0.01), 12.0, "h");
        let y = LinearScale::new((1.0, 3.0), plot.y, plot.bottom());
        let mut b = Buffers::default();
        assert!(e.fill_shape(&m.slots[0], plot, &y, &mut b).is_some());
        let run = &outlines(&b)[0];
        assert_eq!(run[0].y, plot.bottom());
        assert_eq!(run[run.len() - 1].y, plot.bottom());
        // Every value below zero: zero is above the plot, its top edge.
        let m = line_model(1, xs.clone(), vec![-1.5, -2.0, -2.5, -2.0, -1.5], true);
        let y = LinearScale::new((-3.0, -1.0), plot.y, plot.bottom());
        let e = XyElement::new(m.clone(), View::with_min_span(m.full(), 0.01), 12.0, "i");
        assert!(e.fill_shape(&m.slots[0], plot, &y, &mut b).is_some());
        let run = &outlines(&b)[0];
        assert_eq!((run[0].y, run[run.len() - 1].y), (plot.y, plot.y));
        // A negative lobe with zero in view: the outline dips below the
        // zero line (larger pixel y) and returns to it, one outline.
        let m = line_model(1, xs, vec![1.0, 2.0, -1.0, 2.0, 1.0], true);
        let y = LinearScale::new((-2.0, 3.0), plot.y, plot.bottom());
        let e = XyElement::new(m.clone(), View::with_min_span(m.full(), 0.01), 12.0, "j");
        assert!(e.fill_shape(&m.slots[0], plot, &y, &mut b).is_some());
        let runs = outlines(&b);
        assert_eq!(runs.len(), 1);
        let zero = y.y(0.0);
        assert!(runs[0].iter().any(|p| p.y > zero + 1.0), "{runs:?}");
        assert!((runs[0][0].y - zero).abs() < 1e-3);
    }

    #[gpui::test]
    fn a_filled_line_caches_its_fill_beside_its_stroke(cx: &mut gpui::TestAppContext) {
        let xs: Vec<f64> = (0..200).map(|i| 0.8 + i as f64 * 0.002).collect();
        let ys: Vec<f64> = xs
            .iter()
            .map(|x| (-(x - 1.0) * (x - 1.0) * 50.0).exp())
            .collect();
        let before = rebuilds();
        let (host, mut vcx) = open(cx, line_model(1, xs, ys, true));
        assert_eq!(
            rebuilds() - before,
            2,
            "a stroke and a fill on the first frame"
        );
        draw(&mut vcx);
        draw(&mut vcx);
        assert_eq!(rebuilds() - before, 2, "an unchanged frame rebuilt neither");
        host.update(&mut vcx, |h, cx| {
            let full = h.model.full();
            h.view.zoom(2.0, 0.5, full);
            cx.notify();
        });
        draw(&mut vcx);
        assert_eq!(rebuilds() - before, 4, "a moved view rebuilds both once");
    }

    #[test]
    fn a_reversed_axis_feeds_the_decimator_in_ascending_pixel_order() {
        let plot = Rect::new(44.0, 0.0, 400.0, 200.0);
        let y = LinearScale::new((0.0, 2.0), plot.y, plot.bottom());
        let mut pixels = Vec::new();
        for reversed in [false, true] {
            let m = fixture(reversed);
            let e = XyElement::new(m.clone(), View::with_min_span(m.full(), 0.01), 12.0, "c");
            let mut b = Buffers::default();
            e.line_points(&m.slots[0], plot, &y, &mut b);
            assert!(
                b.scratch.xs.windows(2).all(|w| w[1] >= w[0]),
                "reversed={reversed}: the decimator needs ascending x"
            );
            pixels.push(b.scratch.pts.clone());
        }
        // The same curve mirrored: the forward line's first point and the
        // reversed line's last point are the same knot.
        let (fwd, rev) = (&pixels[0], &pixels[1]);
        assert!(!fwd.is_empty() && !rev.is_empty());
        let mirror = |x: f32| plot.x + plot.right() - x;
        assert!((fwd[0].x - mirror(rev[rev.len() - 1].x)).abs() < 0.5);
        assert!((fwd[0].y - rev[rev.len() - 1].y).abs() < 0.5);
    }

    #[test]
    fn a_dashed_line_zoomed_far_in_is_dashed_across_its_own_plot() {
        // A plot that does not start at x = 0, and a view that sits between
        // two knots: the line's window is the knot either side, one plot
        // width left of the plot and nineteen right of it. Only a clip in
        // the points' own coordinates keeps the dashes that cross the plot.
        let plot = Rect::new(44.0, 0.0, 400.0, 200.0);
        let y = LinearScale::new((0.0, 2.0), plot.y, plot.bottom());
        let view = View {
            lo: 1.0001,
            hi: 1.0002,
            min_span: 0.0,
        };
        let period = DASH + GAP;
        for reversed in [false, true] {
            let m = fixture(reversed);
            let dashed = &m.slots[1];
            assert_eq!(dashed.style, Style::Dashed);
            assert_eq!(dashed.window(view), (100, 102));
            let e = XyElement::new(m.clone(), view, 12.0, "d");
            let mut b = Buffers::default();
            assert!(
                e.shape(dashed, plot, &y, &mut b).is_some(),
                "reversed={reversed}: the dashed slot paints a path"
            );
            let s = &b.segments;
            assert!(!s.is_empty(), "reversed={reversed}");
            let left = s.iter().map(|(a, b)| a.x.min(b.x)).fold(f32::MAX, f32::min);
            let right = s.iter().map(|(a, b)| a.x.max(b.x)).fold(f32::MIN, f32::max);
            assert!(
                left >= plot.x - 0.01 && right <= plot.right() + 0.01,
                "reversed={reversed}: no dash outside the plot: {left}..{right}"
            );
            assert!(
                left - plot.x < period && plot.right() - right < period,
                "reversed={reversed}: the dashes reach both edges: {left}..{right}"
            );
            let want = plot.w / period;
            assert!(
                (s.len() as f32 - want).abs() <= 2.0,
                "reversed={reversed}: about one dash a period: {} for {want}",
                s.len()
            );
        }
    }

    #[test]
    fn a_points_slot_past_the_stroke_cap_is_thinned_not_dropped() {
        // 20,000 quoted points in view: 100,000 segments unthinned, six
        // times what one path can hold.
        let n = 20_000usize;
        let xs: Vec<f64> = (0..n).map(|i| 1.0 + i as f64 * 1e-4).collect();
        let mid: Vec<f64> = xs.iter().map(|x| 1.0 + 0.5 * (x * 3.0).sin()).collect();
        let m = XyModel::new(
            1,
            XAxis::default(),
            [YFormat::Plain; 4],
            0.7,
            vec![XySlot {
                number: 1,
                label: "chain".into(),
                color: gpui::red(),
                axis: Axis::Left,
                visible: true,
                style: Style::Solid,
                kind: SlotKind::Points {
                    lo: mid.iter().map(|v| v - 0.05).collect(),
                    hi: mid.iter().map(|v| v + 0.05).collect(),
                    xs,
                    mid,
                },
            }],
        );
        let plot = Rect::new(44.0, 0.0, 400.0, 200.0);
        let y = LinearScale::new((0.0, 2.0), plot.y, plot.bottom());
        let view = View::with_min_span(m.full(), 0.01);
        assert_eq!(m.slots[0].window(view), (0, n), "every point is in view");
        let e = XyElement::new(m.clone(), view, 12.0, "g");
        let mut b = Buffers::default();
        assert!(
            e.shape(&m.slots[0], plot, &y, &mut b).is_some(),
            "the slot paints"
        );
        let segments = b.segments.len();
        assert!(segments <= MAX_STROKE_SEGMENTS, "{segments}");
        assert!(
            segments > MAX_STROKE_SEGMENTS / 2,
            "thinned no further than the cap asks: {segments}"
        );
        // The first and the last point are both among those kept: their
        // diamonds' outer tips sit a marker radius past the plot's edges.
        let left = b.segments.iter().map(|(a, _)| a.x).fold(f32::MAX, f32::min);
        let right = b.segments.iter().map(|(a, _)| a.x).fold(f32::MIN, f32::max);
        assert!((left - (plot.x - MARKER_R)).abs() < 0.01, "{left}");
        assert!((right - (plot.right() + MARKER_R)).abs() < 0.01, "{right}");
    }

    #[test]
    fn a_dashed_line_with_too_many_dashes_is_stroked_solid_not_dropped() {
        // Two knots a pixel column swinging the plot's whole height: the
        // decimated line is some 800 spans of 200 px, about 23,000 dashes.
        let n = 800usize;
        let xs: Vec<f64> = (0..n).map(|i| 1.0 + i as f64 * 1e-3).collect();
        let ys: Vec<f64> = (0..n).map(|i| if i % 2 == 0 { 0.0 } else { 2.0 }).collect();
        let m = XyModel::new(
            1,
            XAxis::default(),
            [YFormat::Plain; 4],
            0.7,
            vec![XySlot {
                number: 1,
                label: "noise".into(),
                color: gpui::red(),
                axis: Axis::Left,
                visible: true,
                style: Style::Dashed,
                kind: SlotKind::Line {
                    xs,
                    ys,
                    fill: false,
                },
            }],
        );
        let plot = Rect::new(44.0, 0.0, 400.0, 200.0);
        let y = LinearScale::new((0.0, 2.0), plot.y, plot.bottom());
        let e = XyElement::new(m.clone(), View::with_min_span(m.full(), 0.01), 12.0, "h");
        let mut b = Buffers::default();
        let path = e.shape(&m.slots[0], plot, &y, &mut b);
        assert!(
            b.segments.len() > MAX_STROKE_SEGMENTS,
            "the fixture's dashes are past the cap: {}",
            b.segments.len()
        );
        assert!(path.is_some(), "a line past the cap is solid, never absent");
    }

    #[test]
    fn a_percent_axis_labels_a_ratio_as_a_percent() {
        assert_eq!(XyElement::y_label(YFormat::Percent)(0.2, 0.05), "20%");
        assert_eq!(XyElement::y_label(YFormat::Percent)(0.205, 0.005), "20.5%");
        assert_eq!(XyElement::y_label(YFormat::Plain)(0.2, 0.05), "0.20");
    }

    /// A slot's readout over all of its points, whatever the view.
    fn read_all(slot: &XySlot, u: f64, tol: f64, format: YFormat) -> String {
        readout(slot, (0, slot.len()), u, tol, format)
    }

    #[test]
    fn a_readout_reads_a_line_between_knots_and_a_point_only_near_one() {
        let m = fixture(false);
        // The solid curve at x = 1.0005, between knots 1.000 and 1.002.
        let line = read_all(&m.slots[0], 1.0005, 0.001, YFormat::Percent);
        assert_eq!(line, "20.00%");
        // The chain has a point at 1.00 with mid 0.2 and a range of ±0.005.
        assert_eq!(
            read_all(&m.slots[2], 1.0, 0.001, YFormat::Percent),
            "20.00%  19.50% / 20.50%"
        );
        assert_eq!(
            read_all(&m.slots[2], 1.004, 0.001, YFormat::Percent),
            "—",
            "no point within a column"
        );
        // A point with no range reads as its value alone.
        assert_eq!(
            read_all(&m.slots[4], 1.0, 0.001, YFormat::Plain),
            "0.001000"
        );
        // Outside a line's own range there is nothing to read.
        assert_eq!(read_all(&m.slots[0], 5.0, 0.001, YFormat::Percent), "—");
        // Midway between the quotes at 0.96 and 0.97 the chain has nothing
        // to read, and the curve still reads between its knots: about
        // 0.2 + 0.035², which is 0.201225.
        assert_eq!(read_all(&m.slots[2], 0.965, 0.001, YFormat::Percent), "—");
        assert_eq!(
            read_all(&m.slots[0], 0.965, 0.001, YFormat::Percent),
            "20.12%"
        );
        // A tolerance that is not a number accepts no point.
        assert_eq!(read_all(&m.slots[2], 1.0, f64::NAN, YFormat::Percent), "—");
    }

    #[test]
    fn a_title_names_x_in_the_axis_format() {
        // A pixel column no finer than the format's own decimals.
        assert_eq!(title(0.953, XFormat::Percent, 0.001), "95.3%");
        assert_eq!(title(7650.0, XFormat::Price, 0.5), "7650.00");
        assert_eq!(title(-0.0512, XFormat::Fixed(2), 0.001), "-0.0512");
        assert_eq!(title(0.75, XFormat::Delta, 0.001), "25.0p");
        assert_eq!(title(0.2537, XFormat::Delta, 0.001), "25.4c");
        // No usable tolerance asks for nothing more.
        assert_eq!(title(0.953, XFormat::Percent, 0.0), "95.3%");
        assert_eq!(title(0.953, XFormat::Percent, f64::NAN), "95.3%");
    }

    #[test]
    fn a_title_is_never_coarser_than_a_pixel_column() {
        assert_eq!(title(0.95312, XFormat::Percent, 0.00001), "95.312%");
        assert_eq!(title(7650.125, XFormat::Price, 0.001), "7650.125");
        assert_eq!(title(-0.051234, XFormat::Fixed(2), 0.00001), "-0.05123");
        assert_eq!(title(0.25371, XFormat::Delta, 0.00001), "25.371c");
        // The column asks for six decimals at most.
        assert_eq!(title(1.0, XFormat::Price, 1e-12), "1.000000");
        assert_eq!(title(0.5, XFormat::Percent, 1e-12), "50.000000%");
        // A fixed format's own count stands when it is the larger.
        assert_eq!(title(0.5, XFormat::Fixed(5), 0.5), "0.5000000");
    }

    #[test]
    fn no_readout_or_title_carries_a_signed_zero() {
        assert_eq!(title(-0.0001, XFormat::Percent, 0.001), "0.0%");
        assert_eq!(title(-0.00001, XFormat::Fixed(2), 0.001), "0.0000");
        assert_eq!(title(-0.001, XFormat::Price, 0.5), "0.00");
        assert_eq!(title(-0.0001, XFormat::Delta, 0.001), "0.0c");
        assert_eq!(fmt_y(-0.00001, YFormat::Percent), "0.00%");
        assert_eq!(fmt_y(-1e-9, YFormat::Plain), "0.000000");
        // A value that does not round to zero keeps its sign.
        assert_eq!(title(-0.001, XFormat::Percent, 0.001), "-0.1%");
        assert_eq!(fmt_y(-0.0001, YFormat::Percent), "-0.01%");
        assert_eq!(fmt_y(-0.5, YFormat::Plain), "-0.500000");
    }

    /// The rows' labels and readouts, without their colors.
    fn texts(rows: &[(Hsla, SharedString, String)]) -> Vec<(&str, &str)> {
        rows.iter()
            .map(|(_, label, value)| (label.as_ref(), value.as_str()))
            .collect()
    }

    #[test]
    fn the_tooltip_reads_every_visible_slot_in_its_own_axis_format() {
        let m = fixture(false);
        let view = View::with_min_span(m.full(), 0.01);
        let e = XyElement::new(m.clone(), view, 12.0, "s");
        // On a quote: the chain and the differences read their point. The
        // left axis is a percent; the right and the lower left are plain.
        // A pixel column is 0.044% of moneyness: two decimals in the title.
        let (title, rows) = e.tooltip_rows(quotes(&m.slots[2])[15], WIDE);
        assert_eq!(title, "96.00%");
        assert_eq!(
            texts(&rows),
            [
                ("s1", "20.16%"),
                ("s2", "21.16%"),
                ("s3", "20.16%  19.66% / 20.66%"),
                ("s4", "1.2016"),
                ("s5", "0.001000"),
            ]
        );
        // Midway between two quotes, eleven pixels from each: the points
        // read a dash and the lines between their knots.
        let scale = e.scale();
        let midway = scale.value_at(scale.x_of(0.965, view, WIDE), view, WIDE);
        let (title, rows) = e.tooltip_rows(midway, WIDE);
        assert_eq!(title, "96.50%");
        assert_eq!(
            texts(&rows),
            [
                ("s1", "20.12%"),
                ("s2", "21.12%"),
                ("s3", "—"),
                ("s4", "1.2012"),
                ("s5", "—"),
            ]
        );
        // A hidden slot has no row, and a row carries its slot's color.
        let mut slots = m.slots.clone();
        slots[1].visible = false;
        slots[3].color = gpui::blue();
        let hidden = XyModel::new(2, m.x, m.y_format, m.split, slots);
        let (_, rows) = XyElement::new(hidden, view, 12.0, "s").tooltip_rows(midway, WIDE);
        let labels: Vec<&str> = texts(&rows).iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["s1", "s3", "s4", "s5"]);
        let colors: Vec<Hsla> = rows.iter().map(|(color, _, _)| *color).collect();
        let (red, blue) = (gpui::red(), gpui::blue());
        assert_eq!(colors, [red, red, blue, red]);
    }

    #[test]
    fn a_point_just_outside_the_view_does_not_read() {
        let m = fixture(false);
        let quote = quotes(&m.slots[2])[15];
        // The quote sits a quarter of a pixel column left of the plot.
        let view = View {
            lo: quote + 1e-5,
            hi: 1.0,
            min_span: 0.001,
        };
        let tol = view.span() / WIDE.w as f64;
        let whole = "20.16%  19.66% / 20.66%";
        assert!(view.lo - quote < tol);
        assert_eq!(
            read_all(&m.slots[2], view.lo, tol, YFormat::Percent),
            whole,
            "by distance alone it is in reach of a cursor at the edge"
        );
        let (_, rows) = XyElement::new(m.clone(), view, 12.0, "t").tooltip_rows(view.lo, WIDE);
        assert_eq!(texts(&rows)[2], ("s3", "—"));
        assert_eq!(texts(&rows)[4], ("s5", "—"));
        // On the edge itself it is in the view, and reads.
        let view = View { lo: quote, ..view };
        let (_, rows) = XyElement::new(m.clone(), view, 12.0, "t").tooltip_rows(quote, WIDE);
        assert_eq!(texts(&rows)[2], ("s3", whole));
    }

    /// The stored xs of a points slot.
    fn quotes(slot: &XySlot) -> &[f64] {
        let SlotKind::Points { xs, .. } = &slot.kind else {
            panic!("slot {} is not a points slot", slot.number);
        };
        xs
    }

    #[gpui::test]
    fn the_crosshair_snaps_to_the_nearest_point_inside_a_plot_and_nowhere_else(
        cx: &mut gpui::TestAppContext,
    ) {
        let m = fixture(false);
        let (_host, mut vcx) = open(cx, m.clone());
        // A synthetic bounds, not the window's: `tooltip_state` is handed a
        // position already relative to the plot's origin, so the origin is
        // free and a fixed size makes the rects the test reasons about
        // exact.
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(600.)));
        let view = View::with_min_span(m.full(), 0.01);
        let element = XyElement::new(m.clone(), view, 12.0, "probe");
        let layout = element.layout(bounds);
        let scale = element.scale();
        let at = |r: Rect, fx: f32, fy: f32| point(px(r.x + r.w * fx), px(r.y + r.h * fy));
        let chain = quotes(&m.slots[2]);

        // 0.4 of the way along 0.8..1.2 is 0.96, where the chain quotes.
        let upper = layout.upper.plot;
        let cursor = at(upper, 0.4, 0.5);
        let state = vcx
            .update(|_, cx| element.tooltip_state(cursor, bounds, cx))
            .expect("a cursor inside the upper plot resolves an x");
        let on_quote = f64::from_bits(state.index as u64);
        assert!(
            chain.contains(&on_quote),
            "snapped to a quote exactly as stored: {on_quote}"
        );
        assert_eq!(
            state.cross_line.x,
            px(scale.x_of(on_quote, view, upper)),
            "the line sits on the point"
        );
        assert_eq!(state.cross_line.y, cursor.y);

        // A few pixels off the quote the line still sits on it, not on the
        // cursor.
        let cursor = point(cursor.x + px(5.0), cursor.y);
        let state = vcx
            .update(|_, cx| element.tooltip_state(cursor, bounds, cx))
            .expect("inside the plot");
        assert_eq!(f64::from_bits(state.index as u64), on_quote);
        assert_eq!(state.cross_line.x, px(scale.x_of(on_quote, view, upper)));
        assert_ne!(state.cross_line.x, cursor.x);

        // Midway between two quotes neither is inside the radius, so the
        // crosshair glides: it reads the cursor's own x and sits under it.
        let cursor = point(px(scale.x_of(0.965, view, upper)), cursor.y);
        let nearest_quote = chain
            .iter()
            .map(|x| (scale.x_of(*x, view, upper) - cursor.x.as_f32()).abs())
            .fold(f32::MAX, f32::min);
        assert!(
            nearest_quote > design_px(SNAP, 12.0),
            "the fixture keeps both neighbours outside the radius: {nearest_quote}"
        );
        let state = vcx
            .update(|_, cx| element.tooltip_state(cursor, bounds, cx))
            .expect("inside the plot");
        let gliding = f64::from_bits(state.index as u64);
        assert_eq!(gliding, scale.value_at(cursor.x.as_f32(), view, upper));
        assert!(!chain.contains(&gliding), "no quote was chosen: {gliding}");
        assert_eq!(state.cross_line.x, cursor.x, "the line is the cursor's");
        // There the chain reads a dash and the curve between its knots.
        let (_, rows) = element.tooltip_rows(gliding, upper);
        assert_eq!(texts(&rows)[2], ("s3", "—"));
        assert_eq!(texts(&rows)[0], ("s1", "20.12%"));

        let axis = layout
            .upper
            .left_axis
            .expect("a left slot reserves a column");
        assert!(
            vcx.update(|_, cx| element.tooltip_state(at(axis, 0.5, 0.5), bounds, cx))
                .is_none(),
            "the y-axis column is not the plot"
        );

        // Without the chain the only quoted points are the lower pane's
        // differences. The crosshair sits on one from a few pixels off it
        // in either pane: the line spans both.
        let slots: Vec<XySlot> = m.slots.iter().filter(|s| s.number != 3).cloned().collect();
        let m = XyModel::new(2, m.x, m.y_format, m.split, slots);
        let element = XyElement::new(m.clone(), view, 12.0, "probe");
        let layout = element.layout(bounds);
        let difference = quotes(&m.slots[3])[19];
        let lower = layout
            .lower
            .expect("a bottom-left slot opens a lower pane")
            .plot;
        let mut hovered = None;
        for (pane, plot) in [("lower", lower), ("upper", layout.upper.plot)] {
            let on_point = scale.x_of(difference, view, plot);
            let cursor = point(px(on_point + 4.0), px(plot.y + plot.h * 0.5));
            let state = vcx
                .update(|_, cx| element.tooltip_state(cursor, bounds, cx))
                .expect("a cursor inside either plot resolves an x");
            assert_eq!(
                f64::from_bits(state.index as u64),
                difference,
                "{pane}: snapped to the difference point as stored"
            );
            assert_eq!(state.cross_line.x, px(on_point), "{pane}");
            assert_ne!(state.cross_line.x, cursor.x, "{pane}");
            hovered = Some((state, cursor));
        }
        let (state, cursor) = hovered.expect("both panes were hovered");

        let built = vcx.update(|window, cx| {
            element
                .tooltip(&state, cursor, bounds, window, cx)
                .is_some()
        });
        assert!(built, "the tooltip builds over an x the cursor resolved");
    }

    /// A plot as wide as the fixture's on 1000 px at the design rem, where
    /// its quotes sit 22.8 px apart.
    const WIDE: Rect = Rect::new(44.0, 0.0, 912.0, 400.0);

    #[test]
    fn the_snap_radius_is_design_pixels_scaled_by_the_rem() {
        for reversed in [false, true] {
            let m = fixture(reversed);
            let view = View::with_min_span(m.full(), 0.01);
            let quote = quotes(&m.slots[2])[15];
            let at_rem = |rem: f32| XyElement::new(m.clone(), view, rem, "j");
            let scale = at_rem(12.0).scale();
            let quote_x = scale.x_of(quote, view, WIDE);
            let glide = |x: f32| Some((scale.value_at(x, view, WIDE), x));
            let case = format!("reversed={reversed}");
            // 6 px from the quote: inside 8 px, outside the 4 px of half
            // the design rem.
            let x = quote_x + 6.0;
            assert_eq!(
                at_rem(12.0).crosshair_x(x, WIDE),
                Some((quote, quote_x)),
                "{case}"
            );
            assert_eq!(at_rem(6.0).crosshair_x(x, WIDE), glide(x), "{case}");
            // 10 px from it: outside 8 px, inside the 16 px of twice the
            // design rem.
            let x = quote_x - 10.0;
            assert_eq!(at_rem(12.0).crosshair_x(x, WIDE), glide(x), "{case}");
            assert_eq!(
                at_rem(24.0).crosshair_x(x, WIDE),
                Some((quote, quote_x)),
                "{case}"
            );
        }
    }

    #[test]
    fn a_chart_of_lines_alone_never_snaps() {
        let slots: Vec<XySlot> = fixture(false)
            .slots
            .iter()
            .filter(|s| matches!(s.kind, SlotKind::Line { .. }))
            .cloned()
            .collect();
        assert_eq!(slots.len(), 3);
        let m = XyModel::new(2, XAxis::default(), [YFormat::Plain; 4], 0.7, slots);
        let view = View::with_min_span(m.full(), 0.01);
        let e = XyElement::new(m, view, 12.0, "k");
        // A knot every 4.6 px: one is inside the radius wherever the
        // cursor is, and none of them is snapped to.
        for fx in [0.1, 0.4, 0.83] {
            let x = WIDE.x + WIDE.w * fx;
            assert_eq!(
                e.crosshair_x(x, WIDE),
                Some((e.scale().value_at(x, view, WIDE), x)),
                "at {fx}"
            );
        }
    }

    #[test]
    fn a_hidden_points_slot_is_not_a_snap_candidate() {
        let view = View::with_min_span((0.8, 1.2), 0.01);
        let scale = LinearX::default();
        let shown = fixture(false).slots.clone();
        let quote = quotes(&shown[2])[15];
        let quote_x = scale.x_of(quote, view, WIDE);
        // Three pixels off the quote both points slots share at 0.96.
        let x = quote_x + 3.0;
        let chosen = |slots: Vec<XySlot>| {
            let m = XyModel::new(2, XAxis::default(), [YFormat::Plain; 4], 0.7, slots);
            XyElement::new(m, view, 12.0, "l").crosshair_x(x, WIDE)
        };
        assert_eq!(chosen(shown.clone()), Some((quote, quote_x)));
        // The lower pane's points are candidates wherever the cursor is:
        // the line spans both panes and the tooltip reads every slot.
        let mut chain_hidden = shown.clone();
        chain_hidden[2].visible = false;
        assert_eq!(chosen(chain_hidden.clone()), Some((quote, quote_x)));
        let mut both_hidden = chain_hidden;
        both_hidden[4].visible = false;
        assert_eq!(
            chosen(both_hidden),
            Some((scale.value_at(x, view, WIDE), x)),
            "nothing shown is quoted there"
        );
    }

    #[test]
    fn a_point_outside_the_view_is_not_a_snap_candidate() {
        let m = fixture(false);
        // The quote at 0.96 sits a little over two pixels left of the plot
        // and the next, at 0.97, far inside it.
        let view = View {
            lo: 0.9601,
            hi: 1.0,
            min_span: 0.001,
        };
        let e = XyElement::new(m.clone(), view, 12.0, "n");
        let scale = e.scale();
        let off = WIDE.x - scale.x_of(quotes(&m.slots[2])[15], view, WIDE);
        let x = WIDE.x + 1.0;
        assert!(
            off > 0.0 && off + 1.0 < design_px(SNAP, 12.0),
            "the unpainted quote is inside the radius: {off}"
        );
        assert_eq!(
            e.crosshair_x(x, WIDE),
            Some((scale.value_at(x, view, WIDE), x)),
            "the line stays in the plot, on the cursor"
        );
    }

    #[test]
    fn a_quote_past_the_edge_does_not_hide_the_next_one_inside_the_plot() {
        let m = fixture(false);
        // Seven pixels a quote: 0.96 sits two pixels left of the plot and
        // 0.97 five inside it. A cursor on the plot's first pixel is
        // nearer the one outside.
        let view = View {
            lo: 0.963,
            hi: 2.263,
            min_span: 0.001,
        };
        let e = XyElement::new(m.clone(), view, 12.0, "o");
        let scale = e.scale();
        let chain = quotes(&m.slots[2]);
        let outside = scale.x_of(chain[15], view, WIDE);
        let inside = scale.x_of(chain[16], view, WIDE);
        let x = WIDE.x + 1.0;
        assert!(
            outside < WIDE.x && x - outside < inside - x && inside - x < design_px(SNAP, 12.0),
            "the fixture's geometry: {outside} {x} {inside}"
        );
        assert_eq!(e.crosshair_x(x, WIDE), Some((chain[16], inside)));
    }

    /// A points slot on the left axis with the given columns.
    fn chain_of(number: u16, xs: &[f64], mid: &[f64], lo: &[f64], hi: &[f64]) -> XySlot {
        XySlot {
            number,
            label: format!("s{number}").into(),
            color: gpui::red(),
            axis: Axis::Left,
            visible: true,
            style: Style::Solid,
            kind: SlotKind::Points {
                xs: xs.to_vec(),
                mid: mid.to_vec(),
                lo: lo.to_vec(),
                hi: hi.to_vec(),
            },
        }
    }

    #[test]
    fn the_quote_nearest_the_cursor_wins_whichever_slot_holds_it() {
        // Two chains whose quotes at 0.960 and 0.962 sit ten pixels apart,
        // so both are inside the radius of a cursor between them.
        let view = View {
            lo: 0.9,
            hi: 1.0824,
            min_span: 0.001,
        };
        let chain = |number: u16, x: f64| {
            chain_of(number, &[0.91, x, 1.05], &[0.2; 3], &[0.19; 3], &[0.21; 3])
        };
        for reversed in [false, true] {
            for (first, second) in [(0.960, 0.962), (0.962, 0.960)] {
                let x = XAxis {
                    format: XFormat::Price,
                    reversed,
                };
                let slots = vec![chain(1, first), chain(2, second)];
                let m = XyModel::new(1, x, [YFormat::Plain; 4], 0.7, slots);
                let e = XyElement::new(m, view, 12.0, "p");
                let scale = e.scale();
                let low = scale.x_of(0.960, view, WIDE);
                let high = scale.x_of(0.962, view, WIDE);
                let case = format!("reversed={reversed}, the first slot quotes {first}");
                assert!(((low - high).abs() - 10.0).abs() < 0.1, "{case}");
                // Three pixels from one and seven from the other.
                assert_eq!(
                    e.crosshair_x(low + 0.3 * (high - low), WIDE),
                    Some((0.960, low)),
                    "{case}"
                );
                assert_eq!(
                    e.crosshair_x(low + 0.7 * (high - low), WIDE),
                    Some((0.962, high)),
                    "{case}"
                );
            }
        }
    }

    /// A chain with holes, 4 px to 0.001 in [`HOLES_VIEW`] on [`WIDE`]:
    /// 0.950 has no quote at all and 0.951 beside it is whole; 1.00 has a
    /// range and no mid; 1.05 has nothing; 1.10 has no mid and a range of
    /// no height.
    fn holes() -> Arc<XyModel> {
        let nan = f64::NAN;
        let slot = chain_of(
            1,
            &[0.90, 0.950, 0.951, 1.00, 1.05, 1.10],
            &[0.2, nan, 0.2, nan, nan, nan],
            &[0.19, nan, 0.19, 0.19, nan, 0.2],
            &[0.21, nan, 0.21, 0.21, nan, 0.2],
        );
        let y_format = [YFormat::Percent; 4];
        XyModel::new(1, XAxis::default(), y_format, 0.7, vec![slot])
    }

    const HOLES_VIEW: View = View {
        lo: 0.9,
        hi: 1.128,
        min_span: 0.001,
    };

    #[test]
    fn a_point_that_paints_nothing_is_not_a_snap_candidate() {
        let e = XyElement::new(holes(), HOLES_VIEW, 12.0, "q");
        let scale = e.scale();
        let at = |u: f64| scale.x_of(u, HOLES_VIEW, WIDE);
        let glide = |x: f32| Some((scale.value_at(x, HOLES_VIEW, WIDE), x));
        // Neither a mid nor a range, or a range of no height: no mark.
        for unpainted in [1.05, 1.10] {
            let x = at(unpainted) - 3.0;
            assert_eq!(e.crosshair_x(x, WIDE), glide(x), "{unpainted}");
        }
        // A range with no mid paints its bar.
        assert_eq!(e.crosshair_x(at(1.00) + 3.0, WIDE), Some((1.00, at(1.00))));
        // Two pixels from the hole at 0.950 and six from the whole quote
        // at 0.951: the hole does not shadow its neighbour.
        assert_eq!(
            e.crosshair_x(at(0.950) - 2.0, WIDE),
            Some((0.951, at(0.951)))
        );
    }

    #[test]
    fn a_point_with_a_range_and_no_mid_reads_its_range() {
        let m = holes();
        let read = |u: f64| read_all(&m.slots[0], u, 1e-6, YFormat::Percent);
        assert_eq!(read(1.00), "—  19.00% / 21.00%");
        assert_eq!(read(0.951), "20.00%  19.00% / 21.00%");
        assert_eq!(read(1.05), "—", "nothing to read");
        assert_eq!(read(1.10), "—", "a range of no height is no range");
    }

    /// Quotes with a side missing, 4 px to 0.001 in [`HOLES_VIEW`] on
    /// [`WIDE`]: 0.95 has a mid and a low, 1.00 a mid and a high, 1.05 a
    /// low alone, 1.08 a mid alone, and 1.10 a mid with a low at the mid.
    fn one_sided() -> Arc<XyModel> {
        let nan = f64::NAN;
        let slot = chain_of(
            1,
            &[0.95, 1.00, 1.05, 1.08, 1.10],
            &[0.2, 0.2, nan, 0.2, 0.2],
            &[0.19, nan, 0.19, nan, 0.2],
            &[nan, 0.21, nan, nan, nan],
        );
        let y_format = [YFormat::Percent; 4];
        XyModel::new(1, XAxis::default(), y_format, 0.7, vec![slot])
    }

    #[test]
    fn a_one_sided_quote_paints_a_half_bar_from_its_mid() {
        let m = one_sided();
        let e = XyElement::new(m.clone(), HOLES_VIEW, 12.0, "u");
        let y = LinearScale::new((0.18, 0.22), WIDE.y, WIDE.bottom());
        let mut b = Buffers::default();
        assert!(e.shape(&m.slots[0], WIDE, &y, &mut b).is_some());
        // A half bar and a diamond at 0.95 and at 1.00, nothing at 1.05,
        // and a diamond alone at 1.08 and at 1.10.
        assert_eq!(b.segments.len(), 5 + 5 + 4 + 4, "{:?}", b.segments);
        let at = |u: f64, v: f64| Point::new(e.scale().x_of(u, HOLES_VIEW, WIDE), y.y(v));
        assert_eq!(b.segments[0], (at(0.95, 0.2), at(0.95, 0.19)), "down");
        assert_eq!(b.segments[5], (at(1.00, 0.2), at(1.00, 0.21)), "up");
    }

    #[test]
    fn a_one_sided_quote_is_a_snap_candidate_and_a_lone_end_is_not() {
        let e = XyElement::new(one_sided(), HOLES_VIEW, 12.0, "v");
        let scale = e.scale();
        let at = |u: f64| scale.x_of(u, HOLES_VIEW, WIDE);
        for painted in [0.95, 1.00, 1.08, 1.10] {
            assert_eq!(
                e.crosshair_x(at(painted) + 3.0, WIDE),
                Some((painted, at(painted))),
                "{painted}"
            );
        }
        // A low with neither a mid nor a high paints nothing.
        let x = at(1.05) + 3.0;
        assert_eq!(
            e.crosshair_x(x, WIDE),
            Some((scale.value_at(x, HOLES_VIEW, WIDE), x))
        );
    }

    #[test]
    fn a_one_sided_quote_reads_a_dash_for_its_missing_side() {
        let m = one_sided();
        let read = |u: f64| read_all(&m.slots[0], u, 1e-6, YFormat::Percent);
        assert_eq!(read(0.95), "20.00%  19.00% / —");
        assert_eq!(read(1.00), "20.00%  — / 21.00%");
        assert_eq!(read(1.05), "—", "an end alone paints nothing and reads so");
        assert_eq!(read(1.08), "20.00%", "no range at all is the mid alone");
        assert_eq!(
            read(1.10),
            "20.00%  20.00% / —",
            "the missing side shows though the other has no spread"
        );
    }

    #[test]
    fn a_run_of_equal_xs_reads_the_point_the_crosshair_snapped_to() {
        let nan = f64::NAN;
        let view = View {
            lo: 0.85,
            hi: 1.15,
            min_span: 0.001,
        };
        // Two points at 1.0. In the first chain the earlier one has no
        // quote and the later a mid; in the second both have a mid; in the
        // third neither has anything.
        let run = |mids: [f64; 2]| {
            chain_of(
                1,
                &[0.9, 1.0, 1.0, 1.1],
                &[0.2, mids[0], mids[1], 0.3],
                &[nan; 4],
                &[nan; 4],
            )
        };
        for reversed in [false, true] {
            let x = XAxis {
                format: XFormat::Price,
                reversed,
            };
            let model = |mids| XyModel::new(1, x, [YFormat::Percent; 4], 0.7, vec![run(mids)]);
            let case = format!("reversed={reversed}");

            let m = model([nan, 0.25]);
            let e = XyElement::new(m.clone(), view, 12.0, "w");
            let on_point = e.scale().x_of(1.0, view, WIDE);
            for off in [-3.0, 3.0] {
                let (u, line_x) = e.crosshair_x(on_point + off, WIDE).expect("in the plot");
                assert_eq!((u, line_x), (1.0, on_point), "{case}, {off} px off");
                let (_, rows) = e.tooltip_rows(u, WIDE);
                assert_eq!(texts(&rows), [("s1", "25.00%")], "{case}, {off} px off");
            }
            // A crosshair gliding a hair to either side of the run reads
            // the same point.
            for u in [1.0 - 1e-7, 1.0 + 1e-7] {
                let read = read_all(&m.slots[0], u, 1e-6, YFormat::Percent);
                assert_eq!(read, "25.00%", "{case} at {u}");
            }

            // Two painted points at one x: the first reads.
            let m = model([0.25, 0.26]);
            for u in [1.0 - 1e-7, 1.0, 1.0 + 1e-7] {
                let read = read_all(&m.slots[0], u, 1e-6, YFormat::Percent);
                assert_eq!(read, "25.00%", "{case} at {u}");
            }

            // No point of the run paints: nothing to read.
            let m = model([nan, nan]);
            assert_eq!(read_all(&m.slots[0], 1.0, 1e-6, YFormat::Percent), "—");
        }
    }

    /// One quoted point, so a view with no span.
    fn one_point() -> (Arc<XyModel>, View) {
        let slot = chain_of(1, &[1.0], &[0.2], &[0.19], &[0.21]);
        let y_format = [YFormat::Percent; 4];
        let m = XyModel::new(1, XAxis::default(), y_format, 0.7, vec![slot]);
        let view = View::with_min_span(m.full(), 0.01);
        assert_eq!(view.span(), 0.0);
        (m, view)
    }

    #[test]
    fn a_view_with_no_span_puts_the_line_where_the_point_is_painted() {
        let (m, view) = one_point();
        let e = XyElement::new(m, view, 12.0, "r");
        let marker = e.scale().x_of(1.0, view, WIDE);
        for fx in [0.0, 0.3, 0.9] {
            assert_eq!(
                e.crosshair_x(WIDE.x + WIDE.w * fx, WIDE),
                Some((1.0, marker)),
                "at {fx}"
            );
        }
        // And there the one point reads.
        let (title, rows) = e.tooltip_rows(1.0, WIDE);
        assert_eq!(title, "1.00");
        assert_eq!(texts(&rows), [("s1", "20.00%  19.00% / 21.00%")]);
    }

    #[gpui::test]
    fn a_cursor_between_the_panes_or_a_view_with_no_x_has_no_crosshair(
        cx: &mut gpui::TestAppContext,
    ) {
        let m = fixture(false);
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(600.)));
        let view = View::with_min_span(m.full(), 0.01);
        let state_at = |cx: &mut gpui::TestAppContext, view: View, at: gpui::Point<Pixels>| {
            let element = XyElement::new(m.clone(), view, 12.0, "probe");
            cx.update(|cx| element.tooltip_state(at, bounds, cx))
        };
        let layout = XyElement::new(m.clone(), view, 12.0, "probe").layout(bounds);
        let upper = layout.upper.plot;
        let lower = layout.lower.expect("the fixture has a lower pane").plot;
        assert!(lower.y > upper.bottom(), "the panes are parted");
        let x = px(upper.x + upper.w * 0.37);
        let inside = point(x, px(upper.y + upper.h * 0.5));
        assert!(state_at(cx, view, inside).is_some(), "this x resolves");
        let between = point(x, px((upper.bottom() + lower.y) / 2.0));
        assert!(
            state_at(cx, view, between).is_none(),
            "the gap between the panes is neither plot"
        );
        for bound in ["lo", "hi"] {
            let mut broken = view;
            if bound == "lo" {
                broken.lo = f64::NAN;
            } else {
                broken.hi = f64::NAN;
            }
            assert!(
                state_at(cx, broken, inside).is_none(),
                "a view whose {bound} is not a number has no x under the cursor"
            );
        }
    }
}
