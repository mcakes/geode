//! The pointer routes, each the twin of a key and going through the key's
//! own function, so neither can disagree with the other. A left press on a
//! card is the cursor (`set_cursor`, as `h j k l` end); a second press in
//! gpui's click count is `enter`: the shell is asked for
//! `config::calibration` through the frame, the route a bound `enter`
//! takes, and runs it for the focused tile. A right press on a card is the
//! cursor then `.`, the menu hung from the pointer rather than the header;
//! on the empty body, `.` for the cursor as it stands. The wheel moves the
//! tile's own scroll offset, clamped to the content on both axes. A port
//! dragged off a card's right edge and dropped on a card is `c` + `enter`
//! with the pick made by hand: `core::edits::can_feed` (the connect
//! picker's `connect_candidates`, one name at a time) decides what a card
//! may take, both in the card's `can_drop` and in the drop itself, and
//! the drop calls the same validated `connect`, so a drop the reader would
//! refuse shows the same danger notice and queues nothing. A press on a
//! header notice dismisses it and, when the notice names a stage of the
//! painted graph (a finding or the reader's refusal at `stages.<name>…`,
//! kept beside the notice when it was made), lands the cursor on the card.
//!
//! A plain left press is left to propagate so the shell's tile-focus
//! press still arrives; a modified press is the shell's (its tile drag
//! and fullscreen) and does nothing here. A right press on a card stops at
//! the card so the body's right press does not open a second menu for the
//! same event.

use geode_core::calibration::{Pipeline, StageName};
use geode_shell::actions::ActionId;
use geode_shell::tiling::TileId;
use geode_tile::notice::Notice;
use gpui::prelude::*;
use gpui::{
    Bounds, Context, MouseButton, MouseDownEvent, Pixels, Point, ScrollWheelEvent, SharedString,
    Window, div,
};
use gpui_component::ActiveTheme as _;

use super::CalibrationTile;
use crate::core::edits::{can_feed, stage_of_path};
use crate::core::menu::OPEN_STAGE;

/// What a port drag carries: the tile it left and the stage whose output
/// is being offered. The stage is a shared string so a frame's `on_drag`
/// clones a handle, not a name. Rendered as the drag's ghost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortDrag {
    pub tile: TileId,
    pub stage: SharedString,
}

impl Render for PortDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_1p5()
            .py_0p5()
            .rounded(theme.radius)
            .bg(theme.popover)
            .border_1()
            .border_color(theme.ring)
            .text_xs()
            .text_color(theme.popover_foreground)
            .child(self.stage.clone())
    }
}

impl CalibrationTile {
    /// A left press on `stage`'s card. A modified press is the shell's.
    /// One press is the cursor; a second (`click_count` 2) is `enter`.
    pub(super) fn card_pressed(
        &mut self,
        stage: &str,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left || event.modifiers.modified() {
            return;
        }
        self.set_cursor(Some(stage.to_string()), cx);
        if event.click_count >= 2 {
            self.frame
                .request_action(ActionId(OPEN_STAGE.to_string()), cx);
        }
    }

    /// A right press on `stage`'s card: the cursor, then the menu hung
    /// from the pointer.
    pub(super) fn card_right_pressed(
        &mut self,
        stage: &str,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Right {
            return;
        }
        self.set_cursor(Some(stage.to_string()), cx);
        self.open_menu(Some(event.position), cx);
    }

    /// A right press on the body off every card: the menu for the cursor
    /// as it stands, hung from the pointer.
    pub(super) fn body_right_pressed(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if event.button != MouseButton::Right {
            return;
        }
        self.open_menu(Some(event.position), cx);
    }

    /// The wheel over the body: the offset moves against the delta (the
    /// content follows the fingers) and clamps to `content - viewport` on
    /// each axis, never below zero. A horizontal delta scrolls x; so does
    /// a vertical one with shift held, for a mouse wheel with one axis.
    pub(super) fn wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(window.line_height());
        let (mut dx, mut dy) = (delta.x.as_f32(), delta.y.as_f32());
        if event.modifiers.shift && dx == 0.0 {
            dx = dy;
            dy = 0.0;
        }
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        let Some(model) = self.model.as_ref() else {
            return;
        };
        let layout = &model.layout;
        let max_x = (layout.content_w - self.viewport.width).max(0.0);
        let max_y = (layout.content_h - self.viewport.height).max(0.0);
        let (sx, sy) = self.scroll;
        let next = ((sx - dx).clamp(0.0, max_x), (sy - dy).clamp(0.0, max_y));
        if next != self.scroll {
            self.scroll = next;
            cx.notify();
        }
    }

    /// A port drag passed gpui's threshold: the popups leave, as they do
    /// for any verb but their own.
    pub(super) fn drag_started(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(window, cx);
        self.close_menu(cx);
    }

    /// Every move of a live port drag over the window (`on_drag_move`
    /// fires whether or not the body is under the pointer; `bounds` is
    /// the body's): the preview edge is the dragged stage into the card
    /// under the pointer when that card may take it, else none. The model
    /// is rebuilt on a change only.
    pub(super) fn drag_moved(
        &mut self,
        drag: &PortDrag,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if drag.tile != self.id {
            return;
        }
        let next = self
            .card_at(position, bounds)
            .filter(|stage| self.eligible(drag, stage))
            .map(|stage| (drag.stage.to_string(), stage));
        self.set_preview(next, cx);
    }

    /// The drag ended somewhere other than a card that took it (a drop on
    /// the empty body, a release off the tile, `escape`): the preview
    /// leaves.
    pub(super) fn drag_ended(&mut self, cx: &mut Context<Self>) {
        self.set_preview(None, cx);
    }

    /// A buttonless move over the body with no drag live: a preview a
    /// cancelled drag left behind goes.
    pub(super) fn pointer_moved(&mut self, cx: &mut Context<Self>) {
        if self.preview_edge.is_some() && self.picker.is_none() && !cx.has_active_drag() {
            self.set_preview(None, cx);
        }
    }

    /// [`eligible`] over the painted pipeline.
    fn eligible(&self, drag: &PortDrag, stage: &str) -> bool {
        eligible(self.model_pipeline.as_deref(), self.id, drag, stage)
    }

    /// `drag` released on `stage`'s card: the same connect the picker's
    /// `enter` runs, once the card is eligible. A drop on a card that is
    /// not (one `can_drop` refused, so this does not run; or a stale
    /// pipeline under the drag) is nothing, since it was never offered.
    pub(super) fn port_dropped(&mut self, stage: &str, drag: &PortDrag, cx: &mut Context<Self>) {
        self.set_preview(None, cx);
        if !self.eligible(drag, stage) {
            return;
        }
        if let Err(reason) = self.connect(stage, &drag.stage, cx) {
            self.refuse(reason, cx);
        }
    }

    /// The stage whose card is under `position`, from the layout less the
    /// scroll at the body's painted origin; `None` off every card.
    fn card_at(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) -> Option<StageName> {
        if !bounds.contains(&position) {
            return None;
        }
        let model = self.model.as_ref()?;
        let layout = &model.layout;
        let x = (position.x - bounds.origin.x).as_f32() + self.scroll.0;
        let y = (position.y - bounds.origin.y).as_f32() + self.scroll.1;
        layout
            .cards
            .iter()
            .find(|c| x >= c.x && x < c.x + layout.card_w && y >= c.y && y < c.y + layout.card_h)
            .map(|c| c.stage.clone())
    }

    /// A press on a header notice: the cursor lands on the stage the
    /// notice names when the painted graph has its card, and the notice is
    /// dismissed as a press always dismissed it. Whether anything changed.
    pub(super) fn notice_pressed(&mut self, notice: &Notice, cx: &mut Context<Self>) -> bool {
        let stage = self
            .notice_stages
            .iter()
            .find(|(n, _)| n == notice)
            .map(|(_, s)| s.clone())
            .filter(|s| {
                self.model
                    .as_ref()
                    .is_some_and(|m| m.layout.cards.iter().any(|c| c.stage == *s))
            });
        let moved = stage.is_some() && self.cursor != stage;
        if moved {
            self.set_cursor(stage, cx);
        }
        self.dismissed.dismiss(notice) || moved
    }

    /// Keep the stage `path` names (`stages.<name>…`) beside `notice`, so
    /// a press on the notice can land the cursor on it. A path naming no
    /// stage keeps nothing.
    pub(super) fn note_stage(&mut self, notice: Notice, path: Option<&str>) {
        let Some(stage) = path.and_then(stage_of_path) else {
            return;
        };
        if !self.notice_stages.iter().any(|(n, _)| *n == notice) {
            self.notice_stages.push((notice, stage.to_string()));
        }
    }

    /// Forget the stages of notices no longer reported.
    pub(super) fn prune_stages(&mut self, reported: &[Notice]) {
        self.notice_stages.retain(|(n, _)| reported.contains(n));
    }
}

/// Whether `stage`'s card may take `drag`: the drag is `tile`'s own (a
/// port from another tile is never eligible) and the dragged stage is a
/// connect candidate of `stage` over `pipeline` (`can_feed`). The card's
/// `can_drop` asks this at paint and the drop asks it again, so the two
/// cannot disagree; with no pipeline nothing is eligible.
pub(super) fn eligible(
    pipeline: Option<&Pipeline>,
    tile: TileId,
    drag: &PortDrag,
    stage: &str,
) -> bool {
    drag.tile == tile && pipeline.is_some_and(|p| can_feed(p, &drag.stage, stage))
}
