//! Divider and tile drag state, geometry, and lifecycle. Rendered mouse
//! catchers call these operations; divider moves apply immediately while
//! tile movement only previews the operation applied on drop.

use gpui::{Context, MouseDownEvent, Window};
use gpui_component::TITLE_BAR_HEIGHT;

use crate::keymap::Modifiers;
use crate::tiling::{
    DividerAddress, DockSide, DropTarget, DropZone, Orientation, Rect, TileId, locate_drop_target,
};

use super::{ShellView, sidebar};

/// What an in-flight divider drag is resizing: a
/// divider inside the main tree, a divider inside one dock's tree, or a
/// dock's frame edge. Tree dividers are named by the pure, stable
/// [`DividerAddress`] captured at mouse-down — never a reference into the
/// tree, because the tree can change between the mouse-down and the moves
/// that apply the drag (`Tree::drag_divider` no-ops on a stale address).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum DividerDragTarget {
    MainTree {
        address: DividerAddress,
    },
    DockTree {
        side: DockSide,
        address: DividerAddress,
    },
    DockEdge {
        side: DockSide,
    },
}

/// Divider resize state captured at mouse-down. Bounds use window
/// coordinates so moves can calculate ratios without another layout walk.
/// The workspace switch epoch invalidates the drag even after switching
/// away and back between renders. `moved` latches on any actual resize;
/// release or cancellation then marks the session dirty, including when
/// the cursor returns to its original position.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DividerDrag {
    pub(super) target: DividerDragTarget,
    pub(super) bounds: Rect,
    pub(super) axis: Orientation,
    pub(super) epoch: u64,
    pub(super) moved: bool,
}

/// One paintable divider strip for the current frame, produced inside
/// `render`'s single layout pass: the hit rect in surface coordinates
/// (the strips are absolutely-positioned children of the tile surface),
/// plus the ready-made [`DividerDrag`] ingredients its mouse-down
/// captures.
pub(super) struct StripSpec {
    pub(super) rect: Rect,
    pub(super) axis: Orientation,
    pub(super) target: DividerDragTarget,
    pub(super) drag_bounds: Rect,
}

/// Minimum movement in window pixels before a mod+click becomes a tile
/// drag. Measured per axis as `max(|dx|, |dy|)` to tolerate small hand
/// movements without rearranging the layout.
const TILE_DRAG_THRESHOLD: f32 = 5.0;

/// Fixed size and cursor offset for the tile drag outline. Keeping the
/// outline independent of tile size makes thin dock tiles visible during
/// a drag. The down-right offset leaves the targeting cursor unobscured.
/// These are window pixels; the outline contains no text to scale with rem.
pub(super) const TILE_DRAG_GHOST_SIZE: (f32, f32) = (96.0, 64.0);
pub(super) const TILE_DRAG_GHOST_OFFSET: f32 = 12.0;

/// Tile drag preview, captured on mod+mouse-down. No layout or tile focus
/// changes until a successful drop. The workspace epoch invalidates moves
/// across workspace switches, including switches away and back.
///
/// `active` latches after crossing [`TILE_DRAG_THRESHOLD`]. Releasing the
/// modifier does not end the drag; mouse release does. A buttonless move
/// cancels because the actual release position is unknown. Both mouse-up
/// routes attempt a drop: keyboard modality can trigger `on_mouse_up_out`
/// even for an in-window release. A release outside all tiles and docks
/// has no target and changes nothing.
///
/// Arming also requests window focus restoration, independently of the
/// workspace's tile focus, so a grabbed occupant cannot strand key routing.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TileDrag {
    pub(super) tile: TileId,
    pub(super) epoch: u64,
    /// Window-space mouse-down position the threshold is measured from.
    pub(super) origin: (f32, f32),
    /// Latest window-space cursor position — what the ghost follows and
    /// the zone highlight classifies against each frame.
    pub(super) cursor: (f32, f32),
    pub(super) active: bool,
}

/// Whether the configured `mod` alias's key is held in a mouse event's
/// modifier set. The alias is exactly one of `CTRL`/`ALT`/`CMD`
/// (`defaults::mod_alias_from_config` — the same source of truth the
/// keymap engine resolves `mod+` bindings through), mapped onto gpui's
/// `control`/`alt`/`platform` the same way `convert_keystroke` maps
/// keyboard modifiers. Extra held modifiers don't disqualify (matching
/// how a chorded mouse gesture is usually read); only the aliased key
/// matters.
fn mod_alias_held(alias: Modifiers, mods: &gpui::Modifiers) -> bool {
    (alias.ctrl && mods.control) || (alias.alt && mods.alt) || (alias.cmd && mods.platform)
}

impl ShellView {
    /// Apply a divider move using captured geometry. Return whether layout
    /// changed; stale addresses and moves already at a clamp are no-ops.
    /// Cancel if the workspace epoch changed, independently of render timing.
    /// A real change latches `moved`; the caller handles notification.
    pub(super) fn apply_divider_drag(&mut self, x: f32, y: f32) -> bool {
        let Some(epoch) = self.divider_drag.as_ref().map(|drag| drag.epoch) else {
            return false;
        };
        if epoch != self.services.workspaces.switch_epoch() {
            self.cancel_divider_drag();
            return false;
        }
        let Some(drag) = &self.divider_drag else {
            return false;
        };
        let ws = self.services.workspaces.active_mut();
        let changed = match &drag.target {
            DividerDragTarget::MainTree { address } => {
                ws.drag_main_divider(address, x, y, drag.bounds)
            }
            DividerDragTarget::DockTree { side, address } => {
                ws.drag_dock_divider(*side, address, x, y, drag.bounds)
            }
            DividerDragTarget::DockEdge { side } => ws.drag_dock_edge(*side, x, y, drag.bounds),
        };
        if changed && let Some(drag) = &mut self.divider_drag {
            drag.moved = true;
        }
        changed
    }

    /// End the active divider drag (mouse-up, wherever it lands). The
    /// session goes dirty here — once per drag, not per move — and only
    /// when the drag actually resized something, so a click-and-release
    /// on a strip writes nothing. Mirrors how keyboard resizes persist:
    /// the dirty flag coalesces onto the watcher's ~500ms background
    /// flush, never a synchronous write on the UI thread.
    pub(super) fn finish_divider_drag(&mut self, cx: &mut Context<Self>) {
        self.cancel_divider_drag();
        cx.notify();
    }

    /// Stop divider tracking while preserving live resizes. Mark the session
    /// dirty if anything moved. Shared by release and cancellation; callers
    /// notify when needed, keeping render-time invalidation notify-free.
    pub(super) fn cancel_divider_drag(&mut self) {
        if let Some(drag) = self.divider_drag.take()
            && drag.moved
        {
            self.session_dirty = true;
        }
    }

    /// Open the tile picker on a bare double-click of the focused placeholder.
    /// The focused-tile guard ensures the eventual pick fills this placeholder,
    /// even if the first click landed on an overlapping element. Real tile
    /// contents keep their own double-click behavior.
    ///
    /// Returning true requires the caller to skip drag arming and focus
    /// restoration, preserving the newly opened picker's input focus.
    pub(super) fn try_pick_tile_on_double_click(
        &mut self,
        id: TileId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.occupant_kind(id) != Some(crate::module::placeholder::PLACEHOLDER_KIND)
            || self.services.workspaces.active().focused_tile() != Some(id)
        {
            return false;
        }
        self.try_pick_on_double_click(event, window, cx)
    }

    /// Open the tile picker from the empty-tree hint. With no tile to address,
    /// the selection is added to the focused region. Empty dock clicks first
    /// focus their region through `on_empty_dock_mouse_down`.
    pub(super) fn try_pick_on_empty_tree_double_click(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.try_pick_on_double_click(event, window, cx)
    }

    /// Focus an empty dock on click, marking the session dirty when focus
    /// changes. A bare double-click then opens the tile picker; selection
    /// lands in this dock because its region is already focused.
    pub(super) fn on_empty_dock_mouse_down(
        &mut self,
        side: DockSide,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.services.workspaces.active_mut().focus_empty_dock(side) {
            self.session_dirty = true;
            cx.notify();
        }
        self.try_pick_on_double_click(event, window, cx);
    }

    /// The gesture and overlay table both tile-picker doors share: the
    /// pair's second click, no modifier, no overlay or drag in flight.
    /// Opens the picker and stops propagation (the shell root's own
    /// bubble-phase focus grab must not follow the open — the dialog
    /// door's `prevent_default` covers it too, belt and braces).
    fn try_pick_on_double_click(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.click_count != 2 || event.modifiers.modified() {
            return false;
        }
        if self.palette.is_some()
            || self.modal_open()
            || !self.matcher.pending().is_empty()
            || self.divider_drag.is_some()
            || self.tile_drag.is_some()
        {
            return false;
        }
        super::choicedialog::open_tile_kinds(self, window, cx);
        cx.stop_propagation();
        true
    }

    /// Focus a main-tree tile and toggle fullscreen on mod+double-click.
    /// Run this before drag arming so the gesture can also leave fullscreen.
    /// Exactly the second click triggers it; a triple-click toggles only once.
    /// Dock tiles, overlays, pending key sequences, and active drags refuse it.
    ///
    /// The first mod+click can arm a below-threshold drag, whose release changes
    /// nothing. A successful toggle uses normal action dispatch for logging,
    /// action history, persistence, and keyboard focus handling. Returning true
    /// requires the caller to skip its drag and plain-click paths.
    pub(super) fn try_fullscreen_on_double_click(
        &mut self,
        id: TileId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.click_count != 2 || !mod_alias_held(self.services.mod_alias, &event.modifiers) {
            return false;
        }
        if self.palette.is_some()
            || self.modal_open()
            || !self.matcher.pending().is_empty()
            || self.divider_drag.is_some()
            || self.tile_drag.is_some()
        {
            return false;
        }
        // `focus_main_tile` answers false for a tile the main tree does
        // not hold — a docked one — which is the dock refusal.
        if !self.services.workspaces.active_mut().focus_main_tile(id) {
            return false;
        }
        self.session_dirty = true;
        self.dispatch(
            &crate::actions::ActionId("workspace::fullscreen_tile".into()),
            None,
            window,
            cx,
        );
        // A tile mouse-down like any other: re-arm the restore the
        // plain-click tail arms, for the same focus-tracking-occupant
        // reason.
        self.pending_focus_restore = true;
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// Arm a tile drag when the configured mod key is held and no overlay,
    /// pending key sequence, fullscreen tile, or other drag blocks it. Return
    /// true when armed; the caller must then skip plain click-to-focus because
    /// tile focus changes only on successful drop.
    pub(super) fn try_arm_tile_drag(
        &mut self,
        id: TileId,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if !mod_alias_held(self.services.mod_alias, &event.modifiers) {
            return false;
        }
        if self.palette.is_some()
            || self.modal_open()
            || !self.matcher.pending().is_empty()
            || self.divider_drag.is_some()
            || self.tile_drag.is_some()
            || self
                .services
                .workspaces
                .active()
                .tree()
                .fullscreen()
                .is_some()
        {
            return false;
        }
        let position = (f32::from(event.position.x), f32::from(event.position.y));
        self.tile_drag = Some(TileDrag {
            tile: id,
            epoch: self.services.workspaces.switch_epoch(),
            origin: position,
            cursor: position,
            active: false,
        });
        // Request the same focus restoration as a plain click. This branch
        // skips the caller's click tail, but a grabbed occupant may still have
        // taken window focus and must not strand it after a workspace switch.
        self.pending_focus_restore = true;
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// Advance the pending/active tile drag to a new cursor position
    /// (mouse-move on the tile-drag catcher). Below the movement
    /// threshold nothing visible exists yet, so no repaint is scheduled;
    /// crossing [`TILE_DRAG_THRESHOLD`] latches `active` (one-way — a
    /// drag that wanders back within 5px of its origin is still a drag),
    /// and every active-drag move repaints so the ghost and zone
    /// highlight track the cursor. Same per-move notify cost as the
    /// divider catcher's live resize — accepted while a button is held.
    pub(super) fn update_tile_drag(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.tile_drag.as_mut() else {
            return;
        };
        drag.cursor = (x, y);
        if !drag.active {
            if (x - drag.origin.0).abs().max((y - drag.origin.1).abs()) < TILE_DRAG_THRESHOLD {
                return;
            }
            drag.active = true;
        }
        cx.notify();
    }

    /// Cancel tile tracking without applying layout or persistence changes.
    /// Render invalidation and missed-release paths share this operation;
    /// event handlers notify separately.
    pub(super) fn cancel_tile_drag(&mut self) {
        self.tile_drag = None;
    }

    /// Handle a release between drag arming and the first paint of its catcher.
    /// GPUI may dispatch mouse-down and mouse-up before rendering again, so
    /// the root supplies permanent hovered and non-hovered release listeners.
    ///
    /// The root can also run before a painted catcher's handler. Cancel only
    /// inactive tile drags so active drops still reach the catcher. Divider
    /// drags can always end here: moves already applied their resizes, and
    /// cancellation preserves them and marks the session dirty.
    pub(super) fn heal_drags_on_root_release(&mut self, cx: &mut Context<Self>) {
        let heal_tile = self.tile_drag.as_ref().is_some_and(|drag| !drag.active);
        if heal_tile {
            self.cancel_tile_drag();
        }
        let heal_divider = self.divider_drag.is_some();
        if heal_divider {
            self.cancel_divider_drag();
        }
        if heal_tile || heal_divider {
            cx.notify();
        }
    }

    /// Finish a tile drag using current layout geometry. Below-threshold
    /// drags change nothing. Recheck workspace epoch, overlays, pending keys,
    /// and tile existence because input events can arrive between renders.
    /// Fullscreen layouts produce no target.
    ///
    /// Resolve the release to a stack center, split edge, or dock background
    /// and invoke the corresponding workspace operation. Only a layout change
    /// dirties the session; invalid targets and self-drops remain no-ops.
    pub(super) fn finish_tile_drag(
        &mut self,
        x: f32,
        y: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.tile_drag.take() else {
            return;
        };
        if drag.active
            // The epoch catches switches away and back between renders.
            && drag.epoch == self.services.workspaces.switch_epoch()
            && self.palette.is_none()
            && !self.modal_open()
            && self.matcher.pending().is_empty()
            // A close and release can arrive before another render.
            && self
                .services
                .workspaces
                .active()
                .region_of(drag.tile)
                .is_some()
        {
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let area = super::render::content_area(window);
            // Mouse events arrive in window coordinates; the tile surface
            // starts below the toolbar, right of the sidebar (same
            // conversion `render` bakes into its drag rects).
            let sx = x - sidebar::width(window);
            let sy = y - toolbar_height;
            let target = locate_drop_target(self.services.workspaces.active(), area, sx, sy);
            let ws = self.services.workspaces.active_mut();
            let changed = match target {
                Some(DropTarget::Tile {
                    id,
                    zone: DropZone::Center,
                }) => ws.drop_stack(drag.tile, id),
                Some(DropTarget::Tile {
                    id,
                    zone: DropZone::Edge(edge),
                }) => ws.drop_split(drag.tile, id, edge),
                Some(DropTarget::DockBackground { side }) => ws.drop_to_dock(drag.tile, side),
                None => false,
            };
            if changed {
                self.session_dirty = true;
            }
        }
        cx.notify();
    }
}
