//! Chart pointer gestures: a dominant horizontal wheel pans, a vertical
//! wheel zooms about the pointer, a press on a plot arms a pan drag and a
//! press on the divider a split drag. Every move goes through the x axis's
//! own scale (`LinearX::{about, pan_sign}`), so a reversed delta axis pans
//! and zooms the way it reads. View moves keep the model; a split change
//! is the same slots under a new version.
//!
//! Hit testing solves the chart's layout over the surface bounds the last
//! prepaint recorded; before the first paint no gesture can start. An armed
//! drag paints an occluding catcher over the surface that takes moves and
//! releases, ends on an outside release, and treats a buttonless move as
//! the release it missed.

use std::cell::Cell;
use std::rc::Rc;

use chrono::NaiveDate;
use geode_chart::core::{PANE_GAP, Rect, design_px};
use geode_chart::{Hit, Layout, divider_band, hit_test};
use gpui::{
    Bounds, Context, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, ScrollWheelEvent, Window,
};

use super::{VolsliceTile, ZOOM_FACTOR};

/// The surface's last painted bounds, shared between the canvas that
/// records them and the listeners that read them.
pub(crate) type ChartBounds = Rc<Cell<Option<Bounds<Pixels>>>>;

/// Vertical wheel pixels per `ZOOM_FACTOR` step.
pub(crate) const WHEEL_ZOOM_PX: f32 = 48.0;

/// The split moves in hundredths while dragged, so a small pointer move
/// does not mint a new model version.
pub(crate) const SPLIT_QUANTUM: f32 = 0.01;

/// An armed drag. `Pan` carries the pointer's last x, so each move pans by
/// the distance since the previous one and the picture follows the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Drag {
    Pan { last_x: f32 },
    Split,
}

impl VolsliceTile {
    /// The chart's bounds, its layout at a zero origin and that origin's
    /// rect, from the last frame; `None` before the first paint.
    fn geometry(&self, rem_px: f32) -> Option<(Bounds<Pixels>, Layout, Rect)> {
        let bounds = self.chart_bounds.get()?;
        let rect = Rect::new(
            0.0,
            0.0,
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
        );
        let layout = Layout::solve(rect, self.model.layout_options(rem_px));
        Some((bounds, layout, rect))
    }

    /// The divider's grab band in the surface's space, for the resize
    /// cursor painted over it.
    pub(crate) fn divider_rect(&self, rem_px: f32) -> Option<Rect> {
        let (_, layout, rect) = self.geometry(rem_px)?;
        divider_band(&layout, rect, rem_px)
    }

    /// Move the view and keep the saved view in step. The model is not
    /// rebuilt; the element repaints from the view.
    fn view_moved(&mut self, cx: &mut Context<Self>) {
        self.state.view = self.view.map(|v| (v.lo, v.hi));
        cx.notify();
    }

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
        let scale = self.model.x.scale();
        let full = self.full;
        let Some(view) = self.view.as_mut() else {
            return;
        };
        let delta = event.delta.pixel_delta(window.line_height());
        let (dx, dy) = (delta.x.as_f32(), delta.y.as_f32());
        if dx.abs() > dy.abs() {
            let w = layout.upper.plot.w;
            if w <= 0.0 {
                return;
            }
            view.pan(-(dx / w) as f64 * scale.pan_sign(), full);
        } else {
            if dy == 0.0 {
                return;
            }
            view.zoom(
                ZOOM_FACTOR.powf((dy / WHEEL_ZOOM_PX) as f64),
                scale.about(fraction as f64),
                full,
            );
        }
        self.view_moved(cx);
    }

    /// Arm a pan over a plot or a split drag over the divider. A modified
    /// press or a second click is the shell's (drag, fullscreen), and the
    /// press is left to propagate so the shell still focuses the tile.
    pub(crate) fn chart_pressed(
        &mut self,
        event: &MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left || event.click_count > 1 || event.modifiers.modified()
        {
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

    /// A move while a drag is armed. A buttonless move is the missed
    /// release; another button's move is a chord and is ignored.
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
                let scale = self.model.x.scale();
                let full = self.full;
                let Some(view) = self.view.as_mut() else {
                    return;
                };
                if w <= 0.0 {
                    return;
                }
                view.pan(-((x - last_x) / w) as f64 * scale.pan_sign(), full);
                self.view_moved(cx);
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
                // The band's centre is the gap's: the pointer's y less half
                // a gap is where the upper pane should end.
                let gap = design_px(PANE_GAP, rem_px);
                let raw = (y - upper.y - gap / 2.0) / avail;
                self.set_split((raw / SPLIT_QUANTUM).round() * SPLIT_QUANTUM, cx);
            }
        }
    }

    /// A left press on a strip row. A press that only focuses the tile does
    /// nothing else: the row would otherwise change under a click meant to
    /// pick the tile. Focused, a plain press solos the row and a ctrl press
    /// toggles it. Control on every platform, macOS included, where cmd is
    /// not control: the shell's tile-drag modifier is alt or cmd, never
    /// control, so the press is free. The cursor moves to the row either way,
    /// so the keyboard carries on from it.
    pub(crate) fn strip_pressed(&mut self, expiry: NaiveDate, ctrl: bool, cx: &mut Context<Self>) {
        if !self.focused {
            return;
        }
        let Some(row) = self.strip.iter().position(|r| r.expiry == expiry) else {
            return;
        };
        self.state.cursor = row;
        let changed = if ctrl {
            self.state.toggle(&self.strip, row)
        } else {
            self.state.solo(&self.strip, row)
        };
        if changed {
            self.resubmit(cx);
        } else {
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn drag(&self) -> Option<Drag> {
        self.drag
    }
}
