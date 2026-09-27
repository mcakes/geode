//! Chart painting with cached scales, labels and data paths.
//!
//! Layout uses a zero origin so cached paths can be translated to the
//! element's current bounds. Each pane paints its grid and axes, then all
//! visible polylines, percentile rules and tags, and density bars. One x axis
//! sits below the lowest pane. The component supplies crosshair and tooltip
//! handling; the readout includes all visible slots.
//!
//! State lives under the element's stable, unique ID. Two caches avoid
//! repeating data-dependent preparation on unchanged paints:
//!
//! * `Buffers` caches side scales, y ticks and labels, and x ticks. Its key
//!   includes model version, view, clock offset, bucket count, bounds size and
//!   rem. Changing one of these inputs derives the chart chrome again.
//! * [`PathCaches`] caches decimation and tessellation. Polyline keys include
//!   model version, slot number, pane, view and plot geometry. Percentile keys
//!   include model version, slot number, percentile index and line geometry.
//!
//! Callers must bump [`ChartModel::version`] whenever model contents change:
//! the path keys do not independently include every model field.
//!
//! Warm paints still allocate. The component clones and translates cached
//! paths before painting, and its grid, axis and label interfaces collect
//! vectors. Tooltip readouts also format values. Path size follows decimated
//! output: up to two extrema per finite run per pixel column, plus breaks.
//! Numerous gaps can therefore exceed two points per column. Density bars
//! remain uncached, with [`MAX_DENSITY_QUADS`] limiting each chart paint.

use std::cell::Cell;
use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Utc};
use gpui::{
    AnyElement, App, Bounds, ContentMask, ElementId, Hsla, IntoElement, Path, PathBuilder, Pixels,
    SharedString, TextAlign, Window, fill, point, px, size,
};
use gpui_component::ActiveTheme;
use gpui_component::plot::label::Text;
use gpui_component::plot::tooltip::{CrossLine, Tooltip, TooltipState};
use gpui_component::plot::{
    AxisLabelSide, AxisText, Grid, IntoPlot, PathCaches, Plot, PlotAxis, PlotLabel, ShapeKey,
};

use crate::core::axis::{Axis, Pane, Side};
use crate::core::decimate::decimate;
use crate::core::layout::{Layout, PaneRects};
use crate::core::scale::{LinearScale, axis_domain, fmt_tick, fmt_value};
use crate::core::time::{Crosshair, Tick, TimeScale, ticks};
use crate::core::view::View;
use crate::core::{DASH, GAP, MAX_DENSITY_QUADS, Point, Rect, TICK_GAP, Y_TICK_GAP, design_px};
use crate::model::ChartModel;

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
    /// The same, for density bars actually painted — the one per-frame
    /// cost with no cache behind it and a bound instead
    /// ([`MAX_DENSITY_QUADS`]).
    static DENSITY_QUADS: Cell<usize> = const { Cell::new(0) };
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

/// How many density bars were painted on this thread since it started.
/// Each chart paint contributes at most [`MAX_DENSITY_QUADS`]. Multiple
/// charts or paint calls on this thread contribute to the same total.
pub fn density_quads() -> usize {
    DENSITY_QUADS.with(|c| c.get())
}

fn note_rebuild() {
    REBUILDS.with(|c| c.set(c.get() + 1));
}

fn note_chrome_rebuild() {
    CHROME_REBUILDS.with(|c| c.set(c.get() + 1));
}

fn note_density_quad() {
    DENSITY_QUADS.with(|c| c.set(c.get() + 1));
}

/// Element-state key of the reused buffers, within this element's scope.
const BUFFERS: &str = "geode-chart-buffers";
/// Element-state key of the per-pane polyline caches.
const LINES: &str = "geode-chart-lines";
/// Element-state key of the per-pane percentile caches.
const PERCENTILES: &str = "geode-chart-percentiles";
/// Most percentile lines one slot reserves cache slots for.
const MAX_PERCENTILES: usize = 8;
/// The polyline's stroke width, in device pixels (not on the rem scale:
/// a hairline is a hairline).
const LINE_WIDTH: f32 = 1.5;
/// Opacity of a density bar's fill.
const BAR_OPACITY: f32 = 0.45;
/// A percentile tag's right edge, inside the plot's right edge, how far
/// above its own line it sits, and how far BELOW it sits instead when
/// the lift would take it out of the pane (a tag near the top of the
/// lower pane would otherwise print into `PANE_GAP` and the upper pane;
/// the content mask would clip it, which reads as a half-drawn label).
/// Device pixels, not the rem scale: the component's plot text is a
/// fixed `label::TEXT_SIZE`, so an inset measured against it must not
/// scale either.
const TAG_INSET: f32 = 2.0;
const TAG_LIFT: f32 = 11.0;
const TAG_DROP: f32 = 2.0;

/// One pane side's resolved y axis: the scale over that side's VISIBLE
/// values and the ticks it paints, labels already formatted.
///
/// Every field is a function of the chrome key's inputs alone, so the
/// whole thing is derived on a chrome miss and only then — the scale in
/// particular is a scan of every visible value of every slot on the
/// side, which at the 500,000-point cap is the one piece of O(n) work
/// that could otherwise land on the render thread every frame.
#[derive(Default, Clone)]
struct SideAxis {
    scale: Option<LinearScale>,
    ticks: Vec<f64>,
    labels: Vec<SharedString>,
}

impl SideAxis {
    fn clear(&mut self) {
        self.scale = None;
        self.ticks.clear();
        self.labels.clear();
    }
}

/// The two `Vec`s the decimation path reuses: the plot-relative x of
/// every visible bucket, and the decimated points it produces. Kept
/// together so one `mem::take` moves both.
#[derive(Default)]
struct Scratch {
    xs: Vec<f32>,
    pts: Vec<Point>,
}

/// Element state kept across frames under the element id: the reused
/// decimation buffers, and the chrome of the last chrome key.
#[derive(Default)]
struct Buffers {
    scratch: Scratch,
    chrome_key: Option<u64>,
    x_ticks: Vec<Tick>,
    /// Indexed by [`axis_index`]; `Axis::ALL` order.
    sides: [SideAxis; 4],
}

/// The theme colours one frame paints its chrome in, read once.
#[derive(Clone, Copy)]
struct Ink {
    line: Hsla,
    text: Hsla,
    strip: Hsla,
}

/// Everything one frame's painters share, so a pane's painter takes one
/// reference rather than eight parameters.
struct Paint<'a> {
    bounds: Bounds<Pixels>,
    scale: &'a TimeScale<'a>,
    visible: (usize, usize),
    x_ticks: &'a [Tick],
    sides: &'a [SideAxis; 4],
    ink: Ink,
}

#[derive(IntoPlot)]
pub struct ChartElement {
    model: Arc<ChartModel>,
    view: View,
    rem_px: f32,
    id: ElementId,
}

impl ChartElement {
    /// Create a chart with a stable ID unique among chart elements in the
    /// window. Buffers and both path caches live under this ID across frames.
    /// Reusing it for another chart can serve paths from the wrong model when
    /// their version and geometry keys coincide. Tile callers use their tile ID.
    pub fn new(model: Arc<ChartModel>, view: View, rem_px: f32, id: impl Into<ElementId>) -> Self {
        Self {
            model,
            view,
            rem_px,
            id: id.into(),
        }
    }

    /// The layout at a zero origin: paths are cached origin-free and
    /// translated by the cache; quads and labels add `bounds.origin`.
    fn layout(&self, bounds: Bounds<Pixels>) -> Layout {
        let r = Rect::new(
            0.0,
            0.0,
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
        );
        Layout::solve(r, self.model.layout_options(self.rem_px))
    }

    /// The y scale for a pane's side over the VISIBLE values of the
    /// slots on it; `None` when no visible slot with a finite value
    /// uses it. O(visible values): a chrome-miss path only.
    fn side_scale(
        &self,
        pane: Pane,
        side: Side,
        plot: Rect,
        visible: (usize, usize),
    ) -> Option<LinearScale> {
        let values = self
            .model
            .slots
            .iter()
            .filter(|s| s.visible && s.axis.pane() == pane && s.axis.side() == side)
            .flat_map(|s| {
                let end = visible.1.min(s.values.len());
                let start = visible.0.min(end);
                s.values[start..end].iter().copied()
            });
        axis_domain(values).map(|d| LinearScale::new(d, plot.y, plot.bottom()))
    }

    /// About how many y ticks a pane of `h` pixels is worth.
    fn y_tick_hint(&self, h: f32) -> usize {
        (h / design_px(Y_TICK_GAP, self.rem_px)).max(2.0) as usize
    }

    /// Derive the whole chrome for this layout: the x ticks and, per
    /// pane side, the scale, its ticks and their labels. Called on a
    /// chrome-key MISS only.
    fn derive_chrome(
        &self,
        layout: &Layout,
        scale: &TimeScale,
        visible: (usize, usize),
        x_ticks: &mut Vec<Tick>,
        sides: &mut [SideAxis; 4],
    ) {
        note_chrome_rebuild();
        ticks(
            scale,
            self.view,
            layout.x_axis,
            design_px(TICK_GAP, self.rem_px),
            self.model.offset_secs,
            x_ticks,
        );
        for (pane, rects) in [
            (Pane::Upper, Some(layout.upper)),
            (Pane::Lower, layout.lower),
        ] {
            for side in [Side::Left, Side::Right] {
                let axis = &mut sides[axis_index(axis_of(pane, side))];
                axis.clear();
                let Some(rects) = rects else { continue };
                let plot = rects.plot;
                if plot.w <= 0.0 || plot.h <= 0.0 {
                    continue;
                }
                let Some(s) = self.side_scale(pane, side, plot, visible) else {
                    continue;
                };
                let hint = self.y_tick_hint(plot.h);
                let step = s.step_for(hint);
                s.ticks(hint, &mut axis.ticks);
                for i in 0..axis.ticks.len() {
                    let v = axis.ticks[i];
                    axis.labels.push(SharedString::from(fmt_tick(v, step)));
                }
                axis.scale = Some(s);
            }
        }
    }

    /// Paint one pane in layers: grid, axes, polylines, percentile rules,
    /// percentile labels, then density bars. Batching by kind uses two cache
    /// updates and one label batch per pane; all percentile rules cover all
    /// polylines regardless of slot order.
    ///
    /// `painted` carries the density-bar count across both panes, so
    /// [`MAX_DENSITY_QUADS`] limits the entire chart paint.
    #[allow(clippy::too_many_arguments)]
    fn paint_pane(
        &self,
        pane: Pane,
        rects: &PaneRects,
        ctx: &Paint<'_>,
        scratch: &mut Scratch,
        painted: &mut usize,
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

        // Grid: the x ticks of the shared axis, the y ticks of whichever
        // side the pane has (left wins when it has both — one grid, not
        // two overlaid ones). The two collects allocate the line vectors
        // required by `Grid`.
        let grid = if left.scale.is_some() { left } else { right };
        let gx: Vec<Pixels> = ctx.x_ticks.iter().map(|t| px(t.x - plot.x)).collect();
        let gy: Vec<Pixels> = grid
            .scale
            .map(|s| grid.ticks.iter().map(|v| px(s.y(*v) - plot.y)).collect())
            .unwrap_or_default();
        Grid::new()
            .x(gx)
            .y(gy)
            .stroke(ctx.ink.line)
            .dash_array(&[px(4.), px(2.)])
            .paint(&bounds_of(plot, bounds), window);

        // Axes. A left axis line sits at the RIGHT edge of its rect (the
        // plot's left edge) with its labels right-aligned inside it; a
        // right axis line at the left edge of its own rect, labels left.
        if let (Some(r), Some(s)) = (rects.left_axis, left.scale) {
            paint_y_axis(r, &s, left, Side::Left, bounds, ctx.ink, window, cx);
        }
        if let (Some(r), Some(s)) = (rects.right_axis, right.scale) {
            paint_y_axis(r, &s, right, Side::Right, bounds, ctx.ink, window, cx);
        }

        let model = &*self.model;
        let view = self.view;
        let pane_ix = pane_index(pane);
        let scale = ctx.scale;
        let visible = ctx.visible;

        // Everything data-shaped is clipped to the pane's OWN plot rect.
        // A path is cached at a zero origin and translated, and the
        // element's own mask is the whole element, so without this a
        // polyline paints over the left axis column (the first visible
        // bucket's centre can sit half a bucket left of `view.lo`, which
        // at the zoom floor of two buckets is a quarter of the plot's
        // width) and a dashed rule runs into the neighbouring pane.
        // `with_content_mask` intersects with the mask already in force,
        // so this only ever narrows.
        let mask = ContentMask {
            bounds: bounds_of(plot, bounds),
        };
        window.with_content_mask(Some(mask), |window| {
            // Polylines.
            let caches = PathCaches::for_paint((LINES, pane_ix), window, cx);
            caches.update(cx, |caches, _| {
                for (k, slot) in model.slots.iter().enumerate() {
                    if !slot.visible || slot.axis.pane() != pane {
                        continue;
                    }
                    let Some(y) = side_scale_of(slot.axis.side(), left, right) else {
                        continue;
                    };
                    let key = ShapeKey::new((model.version, slot.number, pane as u8, view.key()))
                        .f32(plot.x)
                        .f32(plot.y)
                        .f32(plot.w)
                        .f32(plot.h)
                        .finish();
                    let path = caches.slot(k).get(key, bounds.origin, || {
                        note_rebuild();
                        polyline(scale, view, plot, &y, &slot.values, visible, scratch)
                    });
                    if let Some(path) = path {
                        window.paint_path(path, slot.colour);
                    }
                }
            });

            // Percentile lines, then their tags in one label batch.
            //
            // Percentiles describe the query window; the y domain uses the
            // visible slice. Skip both line and tag when a percentile falls
            // outside its pane, before accessing the path cache. Clamping
            // it to an edge would display a false level at that edge.
            let dash = design_px(DASH, self.rem_px);
            let gap = design_px(GAP, self.rem_px);
            let caches = PathCaches::for_paint((PERCENTILES, pane_ix), window, cx);
            caches.update(cx, |caches, _| {
                for (k, slot) in model.slots.iter().enumerate() {
                    if !slot.visible || slot.axis.pane() != pane {
                        continue;
                    }
                    let Some(scale_y) = side_scale_of(slot.axis.side(), left, right) else {
                        continue;
                    };
                    for (j, (_, value)) in slot.percentiles.iter().enumerate().take(MAX_PERCENTILES)
                    {
                        let y = scale_y.y(*value);
                        if !inside(y, plot) {
                            continue;
                        }
                        let key = ShapeKey::new((model.version, slot.number, j))
                            .f32(y)
                            .f32(plot.x)
                            .f32(plot.w)
                            .finish();
                        let path =
                            caches
                                .slot(k * MAX_PERCENTILES + j)
                                .get(key, bounds.origin, || {
                                    note_rebuild();
                                    dashed_horizontal(plot.x, plot.right(), y, dash, gap)
                                });
                        if let Some(path) = path {
                            window.paint_path(path, slot.colour);
                        }
                    }
                }
            });
            let tags: Vec<Text> = model
                .slots
                .iter()
                .filter(|s| s.visible && s.axis.pane() == pane)
                .flat_map(|slot| {
                    let scale_y = side_scale_of(slot.axis.side(), left, right);
                    slot.percentiles
                        .iter()
                        .zip(slot.percentile_labels.iter())
                        .take(MAX_PERCENTILES)
                        .filter_map(move |((_, value), label)| {
                            let y = scale_y.as_ref()?.y(*value);
                            if !inside(y, plot) {
                                return None;
                            }
                            Some(
                                Text::new(
                                    label.clone(),
                                    point(px(plot.right() - TAG_INSET), px(tag_y(y, plot))),
                                    slot.colour,
                                )
                                .align(TextAlign::Right),
                            )
                        })
                })
                .collect();
            if !tags.is_empty() {
                PlotLabel::new(tags).paint(&bounds, window, cx);
            }
        });

        // Density bars: one strip shared by the pane's visible slots,
        // clipped to the strip (its own column, beside the plot rather
        // than inside it) and counted against `MAX_DENSITY_QUADS`, which
        // is this chart paint's budget across both panes. A bar is an
        // uncached `paint_quad` and nothing in the model bounds the
        // product of slots and bins, so past the bound this pane simply
        // stops drawing them, in slot order.
        if let Some(strip) = rects.density {
            window.paint_quad(fill(bounds_of(strip, bounds), ctx.ink.strip));
            let mask = ContentMask {
                bounds: bounds_of(strip, bounds),
            };
            window.with_content_mask(Some(mask), |window| {
                'bars: for slot in model.slots.iter() {
                    if !slot.visible || slot.axis.pane() != pane || slot.bins.is_empty() {
                        continue;
                    }
                    let Some(scale_y) = side_scale_of(slot.axis.side(), left, right) else {
                        continue;
                    };
                    let max = slot.bins.iter().map(|(_, _, n)| *n).max().unwrap_or(0);
                    if max == 0 {
                        continue;
                    }
                    for (lo, hi, n) in slot.bins.iter() {
                        let top = scale_y.y(*hi).clamp(strip.y, strip.bottom());
                        let bottom = scale_y.y(*lo).clamp(strip.y, strip.bottom());
                        let w = strip.w * *n as f32 / max as f32;
                        if w <= 0.0 {
                            continue;
                        }
                        if *painted >= MAX_DENSITY_QUADS {
                            break 'bars;
                        }
                        *painted += 1;
                        note_density_quad();
                        window.paint_quad(fill(
                            Bounds::new(
                                bounds.origin + point(px(strip.x), px(top)),
                                size(px(w), px((bottom - top).max(1.0))),
                            ),
                            slot.colour.opacity(BAR_OPACITY),
                        ));
                    }
                }
            });
        }
    }

    /// Every visible slot, in slot order — the tooltip's readout rows.
    fn visible_slots(&self) -> impl Iterator<Item = &crate::model::ChartSlot> {
        self.model.slots.iter().filter(|s| s.visible)
    }

    /// Format the bucket time as `%Y-%m-%d %H:%M` at the model's UTC offset.
    fn bucket_title(&self, index: usize) -> Option<String> {
        let us = *self.model.buckets.get(index)?;
        let offset = FixedOffset::east_opt(self.model.offset_secs)
            .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid offset"));
        let t: DateTime<FixedOffset> =
            DateTime::<Utc>::from_timestamp_micros(us)?.with_timezone(&offset);
        Some(t.format("%Y-%m-%d %H:%M").to_string())
    }
}

impl Plot for ChartElement {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let rem = self.rem_px;
        let layout = self.layout(bounds);
        let scale = self.model.time_scale();
        let view = self.view;
        let visible = scale.visible(view);
        let ink = {
            let theme = cx.theme();
            Ink {
                line: theme.border,
                text: theme.muted_foreground,
                strip: theme.background,
            }
        };

        // The chrome key: everything the chrome derivation reads. A hit
        // keeps the last frame's ticks, labels and side scales, so the
        // two O(n) walks — `core::time::ticks` and the four
        // `side_scale`s over every visible value — run on a change only,
        // never per frame.
        let chrome_key = ShapeKey::new((
            self.model.version,
            view.key(),
            self.model.offset_secs,
            self.model.buckets.len(),
        ))
        .f32(bounds.size.width.as_f32())
        .f32(bounds.size.height.as_f32())
        .f32(rem)
        .finish();

        // Move buffer ownership out for painting, then return it after
        // the path-cache updates. `mem::take` preserves vector allocations
        // without copying their contents or holding the buffer entity update
        // open while the painters update their own state.
        let buffers = window.use_keyed_state(BUFFERS, cx, |_, _| Buffers::default());
        let (mut scratch, mut x_ticks, mut sides, warm) = buffers.update(cx, |b, _| {
            let warm = b.chrome_key == Some(chrome_key);
            b.chrome_key = Some(chrome_key);
            (
                std::mem::take(&mut b.scratch),
                std::mem::take(&mut b.x_ticks),
                std::mem::take(&mut b.sides),
                warm,
            )
        });
        if !warm {
            self.derive_chrome(&layout, &scale, visible, &mut x_ticks, &mut sides);
        }

        let ctx = Paint {
            bounds,
            scale: &scale,
            visible,
            x_ticks: &x_ticks,
            sides: &sides,
            ink,
        };
        let mut painted = 0usize;
        self.paint_pane(
            Pane::Upper,
            &layout.upper,
            &ctx,
            &mut scratch,
            &mut painted,
            window,
            cx,
        );
        if let Some(lower) = layout.lower.as_ref() {
            self.paint_pane(
                Pane::Lower,
                lower,
                &ctx,
                &mut scratch,
                &mut painted,
                window,
                cx,
            );
        }

        // The one shared x axis, under the lowest pane. A tick's `x` is
        // in layout space and the axis rect starts at the plot's left
        // edge, so the label's own offset is `t.x - x_axis.x`.
        let x_axis = layout.x_axis;
        PlotAxis::new()
            .x(px(0.))
            .x_label(x_ticks.iter().map(|t| {
                AxisText::new(t.label.clone(), px(t.x - x_axis.x), ink.text)
                    .align(TextAlign::Center)
            }))
            .stroke(ink.line)
            .paint(&bounds_of(x_axis, bounds), window, cx);

        buffers.update(cx, |b, _| {
            b.scratch = scratch;
            b.x_ticks = x_ticks;
            b.sides = sides;
        });
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
        let scale = self.model.time_scale();
        let index = Crosshair::at(x, &scale, self.view, plot)?;
        Some(TooltipState::new(
            index,
            point(px(scale.x_of(index, self.view, plot)), position.y),
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
        let title = self.bucket_title(state.index)?;
        let top = layout.upper.plot.y;
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(design_px(8.0, self.rem_px)))
            .cross_line(CrossLine::new(state.cross_line).span(top, layout.lowest_bottom() - top))
            .title(title);
        for slot in self.visible_slots() {
            let value = slot.values.get(state.index).copied().unwrap_or(f64::NAN);
            tooltip = tooltip.row(
                slot.colour,
                slot.label.clone(),
                SharedString::from(fmt_value(value)),
            );
        }
        Some(tooltip.into_any_element())
    }
}

/// One pane side's axis line and its prepared tick labels.
#[allow(clippy::too_many_arguments)]
fn paint_y_axis(
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

fn pane_index(pane: Pane) -> usize {
    match pane {
        Pane::Upper => 0,
        Pane::Lower => 1,
    }
}

/// The one [`Axis`] a `(pane, side)` pair names.
fn axis_of(pane: Pane, side: Side) -> Axis {
    match (pane, side) {
        (Pane::Upper, Side::Left) => Axis::Left,
        (Pane::Upper, Side::Right) => Axis::Right,
        (Pane::Lower, Side::Left) => Axis::BottomLeft,
        (Pane::Lower, Side::Right) => Axis::BottomRight,
    }
}

/// An axis's slot in [`Buffers::sides`] — its position in `Axis::ALL`.
fn axis_index(axis: Axis) -> usize {
    match axis {
        Axis::Left => 0,
        Axis::Right => 1,
        Axis::BottomLeft => 2,
        Axis::BottomRight => 3,
    }
}

fn side_scale_of(side: Side, left: &SideAxis, right: &SideAxis) -> Option<LinearScale> {
    match side {
        Side::Left => left.scale,
        Side::Right => right.scale,
    }
}

/// Whether a y coordinate is inside a pane's plot rect, ends included.
fn inside(y: f32, plot: Rect) -> bool {
    y.is_finite() && y >= plot.y && y <= plot.bottom()
}

/// Where a percentile tag's text TOP sits for a line at `y`: lifted
/// clear of its own line, or dropped below it when the lift would leave
/// the pane. A percentile at the very top of the lower pane would
/// otherwise print into `PANE_GAP` and the upper pane — the same bleed
/// the out-of-pane skip exists to prevent, one order smaller — and
/// under the pane's content mask it would simply be cut in half.
fn tag_y(y: f32, plot: Rect) -> f32 {
    if y - TAG_LIFT < plot.y {
        y + TAG_DROP
    } else {
        y - TAG_LIFT
    }
}

/// A layout rect (zero origin) as window bounds.
fn bounds_of(r: Rect, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        bounds.origin + point(px(r.x), px(r.y)),
        size(px(r.w), px(r.h)),
    )
}

/// The decimated polyline of one slot, built at a zero origin: `xs` is
/// refilled plot-relative (the decimator's columns are pixel columns
/// from the plot's left edge), decimated into `pts`, then walked into a
/// stroke with a new subpath after every `BREAK`.
#[allow(clippy::too_many_arguments)]
fn polyline(
    scale: &TimeScale,
    view: View,
    plot: Rect,
    y: &LinearScale,
    values: &[f64],
    visible: (usize, usize),
    scratch: &mut Scratch,
) -> Option<Path<Pixels>> {
    let Scratch { xs, pts } = scratch;
    let end = visible.1.min(values.len());
    let start = visible.0.min(end);
    xs.clear();
    for i in start..end {
        xs.push(scale.x_of(i, view, plot) - plot.x);
    }
    decimate(xs, &values[start..end], plot.w.max(1.0) as usize, pts);
    if pts.is_empty() {
        return None;
    }
    let mut builder = PathBuilder::stroke(px(LINE_WIDTH));
    let mut move_next = true;
    for p in pts.iter() {
        if p.is_break() {
            move_next = true;
            continue;
        }
        let at = point(px(plot.x + p.x), px(y.y(p.y as f64)));
        if move_next {
            builder.move_to(at);
            move_next = false;
        } else {
            builder.line_to(at);
        }
    }
    builder.build().ok()
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
fn dashed_horizontal(x0: f32, x1: f32, y: f32, dash: f32, gap: f32) -> Option<Path<Pixels>> {
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
    use crate::core::axis::AxisMode;
    use crate::model::ChartSlot;
    use gpui::{Context, Entity, Render, div, prelude::*};

    struct Host {
        model: Arc<ChartModel>,
        view: View,
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(ChartElement::new(
                self.model.clone(),
                self.view,
                window.rem_size().as_f32(),
                "chart",
            ))
        }
    }

    pub(crate) fn model(n: usize) -> Arc<ChartModel> {
        let day = 86_400_000_000i64;
        let buckets: Vec<i64> = (0..n as i64).map(|i| i * day).collect();
        let walk = |seed: u64| -> Vec<f64> {
            let mut s = seed | 1;
            let mut v = 100.0;
            (0..n)
                .map(|i| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    v += ((s % 200) as f64 - 100.0) / 50.0;
                    if i % 97 == 50 { f64::NAN } else { v }
                })
                .collect()
        };
        let slot = |number, axis, seed| {
            let values = walk(seed);
            // Three values the slot itself takes at the CENTRE of the
            // series. A percentile is only painted while it falls inside
            // its pane, and the pane's domain comes from the VISIBLE
            // slice — so a fixed level would drop in and out as the test
            // zooms and the frame's path count would not be a constant.
            // A centred zoom always keeps the centre visible, so these
            // three are always inside the domain they came from.
            let mid = values.len() / 2;
            let pick = |i: usize| values.get(i).copied().unwrap_or(100.0);
            let percentiles = vec![
                (0.05, pick(mid.saturating_sub(1))),
                (0.5, pick(mid)),
                (0.95, pick(mid + 1)),
            ];
            ChartSlot {
                number,
                label: format!("s{number}").into(),
                values,
                colour: gpui::red(),
                axis,
                visible: true,
                percentiles,
                percentile_labels: vec!["p5".into(), "p50".into(), "p95".into()],
                bins: (0..40)
                    .map(|b| {
                        (
                            90.0 + b as f64 * 0.5,
                            90.5 + b as f64 * 0.5,
                            (b % 7 + 1) as u32,
                        )
                    })
                    .collect(),
            }
        };
        Arc::new(ChartModel {
            version: 1,
            buckets,
            step_us: day,
            axis_mode: AxisMode::Session,
            offset_secs: 0,
            split: 0.7,
            density: true,
            slots: vec![
                slot(1, Axis::Left, 7),
                slot(2, Axis::Right, 11),
                slot(3, Axis::BottomLeft, 13),
            ],
        })
    }

    /// One upper-left slot climbing 100.00 → 101.96, so the pane's
    /// domain is a known `[99.9, 102.06]` and a percentile can be placed
    /// deliberately inside it or outside it.
    fn one_slot(version: u64, percentiles: Vec<(f64, f64)>) -> Arc<ChartModel> {
        let day = 86_400_000_000i64;
        let n = 50usize;
        let labels: Vec<SharedString> = percentiles
            .iter()
            .map(|(f, _)| ChartModel::percentile_label(*f))
            .collect();
        Arc::new(ChartModel {
            version,
            buckets: (0..n as i64).map(|i| i * day).collect(),
            step_us: day,
            axis_mode: AxisMode::Session,
            offset_secs: 0,
            split: 0.7,
            density: false,
            slots: vec![ChartSlot {
                number: 1,
                label: "s1".into(),
                values: (0..n).map(|i| 100.0 + i as f64 * 0.04).collect(),
                colour: gpui::red(),
                axis: Axis::Left,
                visible: true,
                percentiles,
                percentile_labels: labels,
                bins: Vec::new(),
            }],
        })
    }

    fn open(
        cx: &mut gpui::TestAppContext,
        model: Arc<ChartModel>,
    ) -> (Entity<Host>, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let mut host = None;
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let h = cx.new(|_| Host {
                        view: View::full(model.full()),
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

    fn draw(vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn an_unchanged_frame_rebuilds_nothing_and_a_moved_view_rebuilds(
        cx: &mut gpui::TestAppContext,
    ) {
        // Opening the window paints its first frame, so the counts that
        // frame leaves behind are measured from BEFORE `open`.
        let before = rebuilds();
        let before_chrome = chrome_rebuilds();
        let (host, mut vcx) = open(cx, model(500));
        let first = rebuilds() - before;
        assert_eq!(
            first,
            3 + 9,
            "three polylines and nine percentile lines on the first frame"
        );
        assert_eq!(
            chrome_rebuilds() - before_chrome,
            1,
            "the side scales, their ticks and the x ticks were derived once"
        );
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
            "an unchanged frame re-derived no chrome either — no scan of the values, no tick label"
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
        assert_eq!(
            chrome_rebuilds() - before_chrome,
            2,
            "and re-derives the chrome once"
        );
    }

    #[gpui::test]
    fn a_percentile_outside_its_panes_domain_is_neither_built_nor_painted(
        cx: &mut gpui::TestAppContext,
    ) {
        let before = rebuilds();
        let (host, mut vcx) = open(
            cx,
            one_slot(1, vec![(0.05, 100.5), (0.5, 101.0), (0.95, 101.5)]),
        );
        assert_eq!(
            rebuilds() - before,
            1 + 3,
            "the polyline and three percentile lines, all three inside the pane"
        );

        // The same values and the same view, so the pane's domain does
        // not move; only p5 (below it) and p95 (above it) leave the pane.
        let mark = rebuilds();
        host.update(&mut vcx, |h, cx| {
            h.model = one_slot(2, vec![(0.05, 50.0), (0.5, 101.0), (0.95, 500.0)]);
            cx.notify();
        });
        draw(&mut vcx);
        assert_eq!(
            rebuilds() - mark,
            1 + 1,
            "the polyline and the one percentile still inside the pane; \
             a line outside it is never built, so it is never painted across \
             the other pane or the x-axis strip"
        );
    }

    #[gpui::test]
    fn an_empty_model_and_a_hidden_lower_pane_paint_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let (host, mut vcx) = open(cx, ChartModel::empty());
        draw(&mut vcx);
        let mut m = (*model(10)).clone();
        m.slots[2].visible = false;
        m.version = 2;
        host.update(&mut vcx, |h, cx| {
            h.model = Arc::new(m);
            cx.notify();
        });
        draw(&mut vcx);
    }

    /// `slots` slots on one axis, each carrying `bins` density bins —
    /// the shape nothing in the model bounds.
    fn dense(slots: usize, bins: usize) -> Arc<ChartModel> {
        let day = 86_400_000_000i64;
        let n = 40usize;
        let one = |number: u8| ChartSlot {
            number,
            label: format!("s{number}").into(),
            values: (0..n).map(|i| 100.0 + i as f64 * 0.1).collect(),
            colour: gpui::red(),
            axis: Axis::Left,
            visible: true,
            percentiles: Vec::new(),
            percentile_labels: Vec::new(),
            // Every count is at least one, so no bar is skipped for
            // having no width and the painted total is the product.
            bins: (0..bins)
                .map(|b| {
                    (
                        100.0 + b as f64 * 0.01,
                        100.01 + b as f64 * 0.01,
                        (b % 7 + 1) as u32,
                    )
                })
                .collect(),
        };
        Arc::new(ChartModel {
            version: 1,
            buckets: (0..n as i64).map(|i| i * day).collect(),
            step_us: day,
            axis_mode: AxisMode::Session,
            offset_secs: 0,
            split: 0.7,
            density: true,
            slots: (0..slots).map(|i| one(i as u8 + 1)).collect(),
        })
    }

    #[gpui::test]
    fn a_frame_paints_at_most_the_density_bound(cx: &mut gpui::TestAppContext) {
        // 12 × 200 = 2,400 bars asked for, against a 2,000 bound.
        let (_host, mut vcx) = open(cx, dense(12, 200));
        // Measured across ONE deliberate frame: a bar has no cache, so
        // unlike `rebuilds()` the count is not idempotent across the
        // frames opening a window happens to paint.
        draw(&mut vcx);
        let before = density_quads();
        draw(&mut vcx);
        assert_eq!(
            density_quads() - before,
            MAX_DENSITY_QUADS,
            "the frame painted the bound and stopped, not all 2,400 uncached quads"
        );

        // Under the bound, every bar is painted.
        let (_host, mut vcx) = open(cx, dense(3, 100));
        draw(&mut vcx);
        let before = density_quads();
        draw(&mut vcx);
        assert_eq!(
            density_quads() - before,
            300,
            "three slots of a hundred bins is under the bound and paints in full"
        );
    }

    #[gpui::test]
    fn the_crosshair_answers_inside_a_plot_and_nowhere_else(cx: &mut gpui::TestAppContext) {
        let m = model(500);
        let (_host, mut vcx) = open(cx, m.clone());
        // A synthetic bounds, not the window's: `tooltip_state` is
        // handed a position already relative to the plot's origin
        // (gpui-component's `Plot` contract), so the origin is free and
        // a fixed size makes the rects the test reasons about exact.
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(600.)));
        let element = ChartElement::new(m.clone(), View::full(m.full()), 12.0, "probe");
        let view = element.view;
        let layout = element.layout(bounds);
        let scale = m.time_scale();
        let at = |r: Rect, fx: f32, fy: f32| point(px(r.x + r.w * fx), px(r.y + r.h * fy));

        let upper = layout.upper.plot;
        let cursor = at(upper, 0.4, 0.5);
        let state = vcx
            .update(|_, cx| element.tooltip_state(cursor, bounds, cx))
            .expect("a cursor inside the upper plot resolves a bucket");
        let index = Crosshair::at(cursor.x.as_f32(), &scale, view, upper)
            .expect("the visible window is not empty");
        assert_eq!(
            state.index, index,
            "the upper pane's rect resolves the index"
        );
        assert_eq!(
            state.cross_line.x,
            px(scale.x_of(index, view, upper)),
            "the cross line sits on the bucket's own centre, not the cursor"
        );
        assert_eq!(state.cross_line.y, cursor.y);

        let axis = layout
            .upper
            .left_axis
            .expect("a left slot reserves a column");
        assert!(
            vcx.update(|_, cx| element.tooltip_state(at(axis, 0.5, 0.5), bounds, cx))
                .is_none(),
            "the y-axis column is not the plot"
        );
        let strip = layout.upper.density.expect("density is on");
        assert!(
            vcx.update(|_, cx| element.tooltip_state(at(strip, 0.5, 0.5), bounds, cx))
                .is_none(),
            "the density strip is not the plot"
        );

        let lower = layout
            .lower
            .expect("a bottom-left slot opens a lower pane")
            .plot;
        let cursor = at(lower, 0.8, 0.5);
        let state = vcx
            .update(|_, cx| element.tooltip_state(cursor, bounds, cx))
            .expect("a cursor inside the lower plot resolves a bucket too");
        let index = Crosshair::at(cursor.x.as_f32(), &scale, view, lower)
            .expect("the visible window is not empty");
        assert_eq!(state.index, index);
        assert_eq!(state.cross_line.x, px(scale.x_of(index, view, lower)));

        // And the readout builds for a state the cursor resolved.
        let built = vcx.update(|window, cx| {
            element
                .tooltip(&state, cursor, bounds, window, cx)
                .is_some()
        });
        assert!(built, "the tooltip renders over a bucket the model has");
    }

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
