//! The chart surface's pointer gestures (mouse pass, 2026-09-24): a
//! wheel zooms about the pointer or pans, a drag on a plot pans, a drag
//! on the band between the panes moves the split. Every gesture ends
//! at the tail its key would — a pan or zoom at `view_moved`, a split
//! at `apply_changed` — so the mouse and the keyboard cannot disagree
//! about what a view move costs.
//!
//! **Where the chart is.** A listener gets a window position and
//! nothing else, so the surface paints a zero-cost `canvas` sibling
//! whose prepaint stores the surface's bounds in [`ChartBounds`]; a
//! gesture reads the LAST frame's bounds, solves `geode_chart`'s own
//! layout over them and asks [`geode_chart::hit_test`]. The one-frame
//! lag is harmless: bounds move on a resize, and a press mid-resize is
//! not a gesture anyone makes.
//!
//! **Why a drag never sticks.** gpui's `on_mouse_move` is hover-gated,
//! so while a drag is armed the tile paints an occluding catcher over
//! the surface that owns every move and release; a release outside it
//! reaches the capture-phase `on_mouse_up_out`, and a move that arrives
//! with no button held is treated as the release that was missed — the
//! shell's own divider-drag rule, applied tile-locally.

use std::cell::Cell;
use std::rc::Rc;

use geode_chart::core::layout::{SPLIT_MAX, SPLIT_MIN};
use geode_chart::core::{PANE_GAP, Rect, design_px};
use geode_chart::{Hit, Layout, divider_band, hit_test};
use gpui::{
    Bounds, Context, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, ScrollWheelEvent, Window,
};

use super::TimeseriesTile;
use crate::core::model::ZOOM_FACTOR;

/// The surface's last painted bounds, shared between the canvas that
/// records them and the listeners that read them.
pub(crate) type ChartBounds = Rc<Cell<Option<Bounds<Pixels>>>>;

/// How many wheel pixels make one `ZOOM_FACTOR` step. A notched mouse
/// wheel reports a line at a time (one line is the window's line
/// height, ~16 px), so a notch is about a third of a keyboard `=`; a
/// trackpad's precise deltas make the zoom continuous.
pub(crate) const WHEEL_ZOOM_PX: f32 = 48.0;

/// The split is quantised to this while dragged: `split` sits in the
/// chart key, so every distinct value rebuilds the chart model (a copy
/// of every visible value), and a drag that produced a new `f32` per
/// pixel would do that hundreds of times. A hundredth is finer than
/// the keyboard's `SPLIT_STEP` and invisible at any pane height.
pub(crate) const SPLIT_QUANTUM: f32 = 0.01;

/// An armed drag. `Pan` carries the pointer's last x so each move pans
/// by the distance since the previous one — the data follows the
/// pointer one-to-one — and never by the distance from the press,
/// which would double up with the moves already applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Drag {
    Pan { last_x: f32 },
    Split,
}

impl TimeseriesTile {
    /// The chart's bounds, its layout at a zero origin and that origin's
    /// rect, from the last frame — `None` before the first paint.
    fn geometry(&self, rem_px: f32) -> Option<(Bounds<Pixels>, Layout, Rect)> {
        let bounds = self.chart_bounds.get()?;
        let rect = Rect::new(
            0.0,
            0.0,
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
        );
        let layout = Layout::solve(rect, self.chart.layout_options(rem_px));
        Some((bounds, layout, rect))
    }

    /// The divider's grab band in the surface's own space, for the
    /// resize-cursor affordance the surface paints over it.
    pub(crate) fn divider_rect(&self, rem_px: f32) -> Option<Rect> {
        let (_, layout, rect) = self.geometry(rem_px)?;
        divider_band(&layout, rect, rem_px)
    }

    #[cfg(test)]
    pub(crate) fn drag(&self) -> Option<Drag> {
        self.drag
    }

    /// A wheel over the surface. The dominant axis wins — a trackpad
    /// reports both, and a swipe meant as a pan must not also zoom —
    /// and only a plot answers: a wheel over an axis or the x strip
    /// does nothing, because there is no "about" there.
    ///
    /// Sign: gpui's `delta.y` is positive when the wheel rolls away
    /// from the trader (the direction that scrolls a list toward its
    /// top), and that zooms IN, the mapping every map and chart tool
    /// uses; a positive `delta.x` moves the content right, which shows
    /// EARLIER buckets, so the view pans left.
    pub(crate) fn wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let rem_px = window.rem_size().as_f32();
        let Some((bounds, layout, rect)) = self.geometry(rem_px) else {
            return;
        };
        let x = (event.position.x - bounds.origin.x).as_f32();
        let y = (event.position.y - bounds.origin.y).as_f32();
        let Hit::Plot { fraction, .. } = hit_test(&layout, rect, rem_px, x, y) else {
            return;
        };
        let delta = event.delta.pixel_delta(window.line_height());
        let (dx, dy) = (delta.x.as_f32(), delta.y.as_f32());
        let changed = if dx.abs() > dy.abs() {
            let w = layout.upper.plot.w;
            if w <= 0.0 {
                return;
            }
            self.model.pan_by(-(dx / w) as f64)
        } else {
            if dy == 0.0 {
                return;
            }
            self.model.zoom_at(
                ZOOM_FACTOR.powf((dy / WHEEL_ZOOM_PX) as f64),
                fraction as f64,
            )
        };
        self.view_moved(changed, cx);
    }

    /// A left press on the surface: on a plot it arms a pan, on the
    /// divider band a split drag, anywhere else nothing. A second press
    /// of a double-click arms nothing — the shell owns the tile's
    /// double-click (fullscreen), and a pan armed under it would follow
    /// the pointer into the toggle.
    ///
    /// Deliberately no `stop_propagation`: the shell's tile-level
    /// mouse-down (click-to-focus, the command-line leave, the
    /// focus-restore re-arm) must still run — the header chip's own
    /// rule, for the same reason.
    pub(crate) fn chart_pressed(
        &mut self,
        event: &MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left || event.click_count > 1 {
            return;
        }
        let rem_px = window.rem_size().as_f32();
        let Some((bounds, layout, rect)) = self.geometry(rem_px) else {
            return;
        };
        let x = (event.position.x - bounds.origin.x).as_f32();
        let y = (event.position.y - bounds.origin.y).as_f32();
        let armed = match hit_test(&layout, rect, rem_px, x, y) {
            Hit::Plot { .. } => Some(Drag::Pan { last_x: x }),
            Hit::Divider => Some(Drag::Split),
            Hit::Outside => None,
        };
        if armed.is_some() {
            self.drag = armed;
            cx.notify();
        }
    }

    /// A move while a drag is armed (from the catcher). A buttonless
    /// move is the release that was missed; a move with another button
    /// is a chord and is ignored; a left-button move applies the drag.
    pub(crate) fn drag_moved(
        &mut self,
        event: &MouseMoveEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        match event.pressed_button {
            None => self.drag_finished(cx),
            Some(MouseButton::Left) => self.drag_apply(event.position, window, cx),
            Some(_) => {}
        }
    }

    pub(crate) fn drag_finished(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    fn drag_apply(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.drag else {
            return;
        };
        let rem_px = window.rem_size().as_f32();
        let Some((bounds, layout, _)) = self.geometry(rem_px) else {
            return;
        };
        let x = (position.x - bounds.origin.x).as_f32();
        let y = (position.y - bounds.origin.y).as_f32();
        match drag {
            Drag::Pan { last_x } => {
                self.drag = Some(Drag::Pan { last_x: x });
                let w = layout.upper.plot.w;
                if w <= 0.0 {
                    return;
                }
                let changed = self.model.pan_by(-((x - last_x) / w) as f64);
                self.view_moved(changed, cx);
            }
            Drag::Split => {
                let Some(lower) = layout.lower else {
                    return;
                };
                let upper = layout.upper.plot;
                let avail = upper.h + lower.plot.h;
                if avail <= 0.0 {
                    return;
                }
                // The band's centre is the gap's centre: the pointer's y
                // less half a gap is where the upper pane should end.
                let gap = design_px(PANE_GAP, rem_px);
                let raw = (y - upper.y - gap / 2.0) / avail;
                let split =
                    ((raw / SPLIT_QUANTUM).round() * SPLIT_QUANTUM).clamp(SPLIT_MIN, SPLIT_MAX);
                if (split - self.model.split()).abs() < SPLIT_QUANTUM / 2.0 {
                    return;
                }
                // Clamped above, so the refusal arm is unreachable; a
                // refused write would leave the notice, not panic.
                let written = self.model.set_split(split);
                let changed = self.noticed(written);
                self.apply_changed(changed, cx);
            }
        }
    }
}
