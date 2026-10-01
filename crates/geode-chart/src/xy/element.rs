//! Xy chart painting with cached scales, labels and data paths.
//!
//! Layout uses a zero origin so cached paths can be translated to the
//! element's current bounds. Each pane paints its grid and axes, then every
//! visible slot the view shows something of: a line as a solid or dashed
//! stroke, a points slot as a diamond per point with a vertical bar over its
//! range. One x axis sits below the lowest pane.
//!
//! State lives under the element's stable, unique ID. Two caches avoid
//! repeating data-dependent preparation on unchanged paints:
//!
//! * `Buffers` caches side scales, y ticks and labels, and x ticks. Its key
//!   includes model version, view, bounds size and rem. Changing one of
//!   these inputs derives the chart chrome again.
//! * [`PathCaches`] caches decimation, dashing and tessellation, one path
//!   per slot. A key includes model version, slot number, pane, view and
//!   plot geometry; the rem reaches it through the plot geometry, which the
//!   axis columns and the x-axis strip make a function of it.
//!
//! Callers must bump [`XyModel::version`] whenever model contents change:
//! the keys do not independently include every model field.
//!
//! Warm paints still allocate. The component clones and translates cached
//! paths before painting, and its grid and axis interfaces collect vectors.
//! A line's path follows decimated output: up to two extrema per finite run
//! per pixel column, plus breaks, and a dashed line's dashes are bounded by
//! the plot rectangle. A points slot is not decimated: its path carries up
//! to five segments for every point in view.

use std::sync::Arc;

use gpui::{App, Bounds, ContentMask, ElementId, Path, Pixels, Window};
use gpui_component::plot::{IntoPlot, PathCaches, Plot, ShapeKey};

use super::model::{SlotKind, Style, XyModel, XySlot, YFormat};
use crate::core::axis::{Pane, Side};
use crate::core::layout::{Layout, PaneRects};
use crate::core::linear::{LinearX, x_ticks};
use crate::core::marks::{Clip, MARKER_R, Segment, dash_polyline, point_marks};
use crate::core::scale::{LinearScale, axis_domain, fmt_tick};
use crate::core::time::Tick;
use crate::core::view::View;
use crate::core::{DASH, GAP, Rect, TICK_GAP, design_px};
use crate::paint::{
    Ink, LINE_WIDTH, Scratch, SideAxis, axis_index, axis_of, bounds_of, decimated_points,
    note_chrome_rebuild, note_rebuild, paint_grid, paint_x_axis, paint_y_axis, pane_index,
    side_scale_of, stroke_points, stroke_segments, y_tick_hint,
};

/// Element-state key of the reused buffers, within this element's scope.
const BUFFERS: &str = "geode-xy-buffers";
/// Element-state key of the per-pane shape caches.
const SHAPES: &str = "geode-xy-shapes";

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

    fn y_label(format: YFormat) -> impl Fn(f64, f64) -> String {
        move |v, step| match format {
            YFormat::Plain => fmt_tick(v, step),
            YFormat::Percent => format!("{}%", fmt_tick(v * 100.0, step * 100.0)),
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
                        stroke_segments(&b.segments, LINE_WIDTH)
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
                for i in start..end {
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
        if plot.w <= 0.0 || plot.h <= 0.0 {
            return;
        }
        let bounds = ctx.bounds;
        let left = &ctx.sides[axis_index(axis_of(pane, Side::Left))];
        let right = &ctx.sides[axis_index(axis_of(pane, Side::Right))];

        // One grid, not two overlaid ones: the left side's y ticks when the
        // pane has a left scale, else the right's.
        let grid = if left.scale.is_some() { left } else { right };
        paint_grid(plot, ctx.x_ticks, grid, bounds, ctx.ink, window);

        if let (Some(r), Some(s)) = (rects.left_axis, left.scale) {
            paint_y_axis(r, &s, left, Side::Left, bounds, ctx.ink, window, cx);
        }
        if let (Some(r), Some(s)) = (rects.right_axis, right.scale) {
            paint_y_axis(r, &s, right, Side::Right, bounds, ctx.ink, window, cx);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::axis::Axis;
    use crate::core::linear::XFormat;
    use crate::paint::{chrome_rebuilds, rebuilds};
    use crate::xy::model::XAxis;
    use gpui::{Context, Entity, IntoElement, Render, div, prelude::*};

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
        assert!(
            narrow.side_domain(Pane::Lower, Side::Right).is_none(),
            "no slot on that side"
        );
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
    fn a_percent_axis_labels_a_ratio_as_a_percent() {
        assert_eq!(XyElement::y_label(YFormat::Percent)(0.2, 0.05), "20%");
        assert_eq!(XyElement::y_label(YFormat::Percent)(0.205, 0.005), "20.5%");
        assert_eq!(XyElement::y_label(YFormat::Plain)(0.2, 0.05), "0.20");
    }
}
