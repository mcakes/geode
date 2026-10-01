//! Xy chart painting with cached scales, labels and data paths.
//!
//! Layout uses a zero origin so cached paths can be translated to the
//! element's current bounds. Each pane paints its grid and axes, then every
//! visible slot the view shows something of: a line as a solid or dashed
//! stroke, a points slot as a diamond per point with a vertical bar over its
//! range. One x axis sits below the lowest pane.
//!
//! The crosshair sits on a quoted point when the cursor is within a few
//! pixels of one and otherwise follows the cursor; its tooltip reads every
//! visible slot at that x, a line between its knots and a points slot at
//! the point the crosshair is on. Resolving it is a binary search per slot
//! and allocates nothing; building the tooltip formats a row per slot.
//!
//! State lives under the element's stable, unique ID. Two caches avoid
//! repeating data-dependent preparation on unchanged paints:
//!
//! * `Buffers` caches side scales, y ticks and labels, and x ticks. Its key
//!   includes model version, view, bounds size and rem. Changing one of
//!   these inputs derives the chart chrome again.
//! * [`PathCaches`] caches decimation, dashing and tessellation, one path
//!   per slot. A key includes model version, slot number, pane, view, plot
//!   geometry and rem, which sizes the dashes and the markers.
//!
//! Callers must bump [`XyModel::version`] whenever model contents change:
//! the keys do not independently include every model field.
//!
//! Warm paints still allocate. The component clones and translates cached
//! paths before painting, and its grid and axis interfaces collect vectors.
//! A line's path follows decimated output: up to two extrema per finite run
//! per pixel column, plus breaks, and a dashed line's dashes are bounded by
//! the plot rectangle. A points slot carries up to five segments for every
//! point in view. One path holds [`MAX_STROKE_SEGMENTS`] at most: a points
//! slot with more marks than that paints every k-th point and the last, and
//! a dashed line with more dashes than that is stroked solid.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Bounds, ContentMask, ElementId, IntoElement, Path, Pixels, SharedString,
    Window, point, px,
};
use gpui_component::plot::tooltip::{CrossLine, Tooltip, TooltipState};
use gpui_component::plot::{IntoPlot, PathCaches, Plot, ShapeKey};

use super::model::{SlotKind, Style, XyModel, XySlot, YFormat};
use crate::core::axis::{Pane, Side};
use crate::core::layout::{Layout, PaneRects};
use crate::core::linear::{LinearX, XFormat, delta_label_with, x_ticks};
use crate::core::marks::{
    Clip, MARKER_R, SEGMENTS_PER_MARK, Segment, dash_polyline, mark_stride, point_marks, strided,
};
use crate::core::scale::{LinearScale, axis_domain, fmt_percent, fmt_tick, fmt_value};
use crate::core::time::Tick;
use crate::core::view::View;
use crate::core::{DASH, GAP, Rect, TICK_GAP, design_px};
use crate::paint::{
    Ink, LINE_WIDTH, MAX_STROKE_SEGMENTS, Scratch, SideAxis, axis_index, axis_of, bounds_of,
    decimated_points, note_chrome_rebuild, note_rebuild, paint_pane_frame, paint_x_axis,
    pane_index, side_scale_of, stroke_points, stroke_segments, y_tick_hint,
};

/// Element-state key of the reused buffers, within this element's scope.
const BUFFERS: &str = "geode-xy-buffers";
/// Element-state key of the per-pane shape caches.
const SHAPES: &str = "geode-xy-shapes";

/// The snap radius, in design pixels at the design rem: how near along x
/// the cursor must be to a quoted point for the crosshair to sit on it.
/// Farther than this from every quote, the crosshair follows the cursor.
const SNAP: f32 = 8.0;

/// The distance from the cursor to the tooltip box, in design pixels.
const TOOLTIP_GAP: f32 = 8.0;

// The crosshair's x rides in `TooltipState::index` as its bit pattern. A
// narrower `usize` would cut it and the tooltip would read another x.
const _: () = assert!(usize::BITS >= u64::BITS);

/// A y value as its axis reads it in a readout: finer than a tick label.
fn fmt_y(v: f64, format: YFormat) -> String {
    match format {
        YFormat::Plain => fmt_value(v),
        YFormat::Percent => format!("{:.2}%", v * 100.0),
    }
}

/// One slot's readout at `u`. A line is read between its knots. A points
/// slot shows its nearest point when that lies within `tol` of `u`, as
/// `mid  lo / hi`, or the value alone when it has no range. A dash when
/// the slot has nothing there.
pub(crate) fn readout(slot: &XySlot, u: f64, tol: f64, format: YFormat) -> String {
    const NONE: &str = "—";
    match &slot.kind {
        SlotKind::Line { .. } => slot
            .line_value_at(u)
            .map_or_else(|| NONE.to_string(), |v| fmt_y(v, format)),
        SlotKind::Points { xs, mid, lo, hi } => {
            let Some(i) = slot.nearest(u) else {
                return NONE.to_string();
            };
            // Asked as "is it near", so a tolerance that is not a number
            // accepts no point instead of every point.
            let near = (xs[i] - u).abs() <= tol;
            if !near || !mid[i].is_finite() {
                return NONE.to_string();
            }
            if lo[i].is_finite() && hi[i].is_finite() && lo[i] != hi[i] {
                format!(
                    "{}  {} / {}",
                    fmt_y(mid[i], format),
                    fmt_y(lo[i], format),
                    fmt_y(hi[i], format)
                )
            } else {
                fmt_y(mid[i], format)
            }
        }
    }
}

/// The tooltip's title: x at the crosshair, finer than a tick label.
pub(crate) fn title(u: f64, format: XFormat) -> String {
    match format {
        XFormat::Price => format!("{u:.2}"),
        XFormat::Percent => format!("{:.1}%", u * 100.0),
        XFormat::Fixed(n) => format!("{u:.*}", n as usize + 2),
        XFormat::Delta => delta_label_with(u, 1),
    }
}

/// Element state kept across frames: the reused buffers and the chrome of
/// the last chrome key.
#[derive(Default)]
pub(crate) struct Buffers {
    pub(crate) scratch: Scratch,
    /// A line's values in ascending pixel order, beside `scratch.xs`.
    vals: Vec<f64>,
    segments: Vec<Segment>,
    /// A points slot's pixel columns: x, mid, low and high.
    px: [Vec<f32>; 4],
    chrome_key: Option<u64>,
    x_ticks: Vec<Tick>,
    /// Indexed by [`axis_index`]; `Axis::ALL` order.
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
        LinearX {
            reversed: self.model.x.reversed,
        }
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
    /// and otherwise glides with the cursor. The candidates are the
    /// visible points slots of both panes, since the line spans both and
    /// the tooltip reads every slot; a line is read between its knots, so
    /// its knots are never snapped to. A point outside the view is not a
    /// candidate either: it is not painted, and its x lies off the plot,
    /// where the line would cross an axis column.
    ///
    /// One binary search per slot and no allocation: this runs on every
    /// pointer move.
    pub(crate) fn crosshair_x(&self, cursor_x: f32, plot: Rect) -> Option<(f64, f32)> {
        let scale = self.scale();
        let view = self.view;
        let under = scale.value_at(cursor_x, view, plot);
        if !under.is_finite() {
            return None;
        }
        let radius = design_px(SNAP, self.rem_px);
        let snapped = self
            .model
            .slots
            .iter()
            .filter(|s| s.visible && matches!(s.kind, SlotKind::Points { .. }))
            .filter_map(|s| s.nearest(under).map(|i| s.xs()[i]))
            .filter(|x| x.is_finite() && (view.lo..=view.hi).contains(x))
            .map(|x| (x, scale.x_of(x, view, plot)))
            .min_by(|a, b| (a.1 - cursor_x).abs().total_cmp(&(b.1 - cursor_x).abs()))
            .filter(|(_, x)| (x - cursor_x).abs() <= radius);
        Some(snapped.unwrap_or((under, cursor_x)))
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
                let axis_id = axis_of(pane, side);
                let axis = &mut sides[axis_index(axis_id)];
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
        let SlotKind::Line { xs, ys } = &slot.kind else {
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

    /// Paint one pane in layers: grid, axes, then every visible slot's
    /// path in slot order.
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
        let left = &ctx.sides[axis_index(axis_of(pane, Side::Left))];
        let right = &ctx.sides[axis_index(axis_of(pane, Side::Right))];
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
            let caches = PathCaches::for_paint((SHAPES, pane_index(pane)), window, cx);
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
                    let path = caches.slot(k).get(key, bounds.origin, || {
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
        // only, never per frame.
        let chrome_key = ShapeKey::new((self.model.version, self.view.key()))
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
        let plot = [Some(layout.upper), layout.lower]
            .into_iter()
            .flatten()
            .map(|p| p.plot)
            .find(|p| p.contains(x, y))?;
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
        let u = f64::from_bits(state.index as u64);
        // One pixel column, in x units: how near a point must be to be
        // read. A snapped crosshair is on the point's own x; a gliding one
        // has no point within the snap radius, so quotes read a dash.
        let tol = self.view.span() / plot.w as f64;
        let top = plot.y;
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(design_px(TOOLTIP_GAP, self.rem_px)))
            .cross_line(CrossLine::new(state.cross_line).span(top, layout.lowest_bottom() - top))
            .title(title(u, self.model.x.format));
        for slot in self.model.slots.iter().filter(|s| s.visible) {
            tooltip = tooltip.row(
                slot.color,
                slot.label.clone(),
                SharedString::from(readout(slot, u, tol, self.model.y_format_of(slot.axis))),
            );
        }
        Some(tooltip.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::axis::Axis;
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
                    },
                ),
                slot(
                    2,
                    Axis::Left,
                    Style::Dashed,
                    SlotKind::Line {
                        xs: xs.clone(),
                        ys: curve(0.01),
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
                kind: SlotKind::Line { xs, ys },
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

    #[test]
    fn a_readout_reads_a_line_between_knots_and_a_point_only_near_one() {
        let m = fixture(false);
        // The solid curve at x = 1.0005, between knots 1.000 and 1.002.
        let line = readout(&m.slots[0], 1.0005, 0.001, YFormat::Percent);
        assert_eq!(line, "20.00%");
        // The chain has a point at 1.00 with mid 0.2 and a range of ±0.005.
        assert_eq!(
            readout(&m.slots[2], 1.0, 0.001, YFormat::Percent),
            "20.00%  19.50% / 20.50%"
        );
        assert_eq!(
            readout(&m.slots[2], 1.004, 0.001, YFormat::Percent),
            "—",
            "no point within a column"
        );
        // A point with no range reads as its value alone.
        assert_eq!(readout(&m.slots[4], 1.0, 0.001, YFormat::Plain), "0.001000");
        // Outside a line's own range there is nothing to read.
        assert_eq!(readout(&m.slots[0], 5.0, 0.001, YFormat::Percent), "—");
        // Midway between the quotes at 0.96 and 0.97 the chain has nothing
        // to read, and the curve still reads between its knots: about
        // 0.2 + 0.035², which is 0.201225.
        assert_eq!(readout(&m.slots[2], 0.965, 0.001, YFormat::Percent), "—");
        assert_eq!(
            readout(&m.slots[0], 0.965, 0.001, YFormat::Percent),
            "20.12%"
        );
        // A tolerance that is not a number accepts no point.
        assert_eq!(readout(&m.slots[2], 1.0, f64::NAN, YFormat::Percent), "—");
    }

    #[test]
    fn a_title_names_x_in_the_axis_format() {
        assert_eq!(title(0.953, XFormat::Percent), "95.3%");
        assert_eq!(title(7650.0, XFormat::Price), "7650.00");
        assert_eq!(title(-0.0512, XFormat::Fixed(2)), "-0.0512");
        assert_eq!(title(0.75, XFormat::Delta), "25.0p");
        assert_eq!(title(0.2537, XFormat::Delta), "25.4c");
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
        let tol = view.span() / upper.w as f64;
        assert_eq!(readout(&m.slots[2], gliding, tol, YFormat::Percent), "—");
        assert_eq!(
            readout(&m.slots[0], gliding, tol, YFormat::Percent),
            "20.12%"
        );

        let axis = layout
            .upper
            .left_axis
            .expect("a left slot reserves a column");
        assert!(
            vcx.update(|_, cx| element.tooltip_state(at(axis, 0.5, 0.5), bounds, cx))
                .is_none(),
            "the y-axis column is not the plot"
        );

        // The lower pane holds only the difference points, 0.01 apart.
        let lower = layout
            .lower
            .expect("a bottom-left slot opens a lower pane")
            .plot;
        let cursor = at(lower, 0.5, 0.5);
        let state = vcx
            .update(|_, cx| element.tooltip_state(cursor, bounds, cx))
            .expect("a cursor inside the lower plot resolves an x too");
        let u = f64::from_bits(state.index as u64);
        assert!(
            quotes(&m.slots[4]).contains(&u),
            "snapped to a difference point, not a curve knot: {u}"
        );
        assert_eq!(state.cross_line.x, px(scale.x_of(u, view, lower)));

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
}
