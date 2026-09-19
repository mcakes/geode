//! Divider and tile drag state and math (drag-splitters, tile-drag tasks):
//! the drag target/state types the render pass's mouse catchers populate
//! and consume, and the pure apply/finish/cancel verbs `render` calls into
//! every mouse move and release. Split out of `shell/mod.rs` (Phase 3c
//! Task 0) because these are geometry-heavy, self-contained, and reached
//! only from `render.rs`'s mouse handlers.

use gpui::{Context, MouseDownEvent, Window};
use gpui_component::TITLE_BAR_HEIGHT;

use crate::keymap::Modifiers;
use crate::tiling::{
    DividerAddress, DockSide, DropTarget, DropZone, Orientation, Rect, TileId, locate_drop_target,
};

use super::{ShellView, sidebar};

/// What an in-flight divider drag is resizing (drag-splitters task): a
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

/// An active divider drag, recorded by the strip's mouse-down and consumed
/// by the full-window drag catcher's mouse-move/up (see `render`). Holds
/// everything the position→ratio math needs so a mouse-move never has to
/// re-derive layout outside the render pass's single geometry walk:
/// `bounds` is the containing rect in *window* coordinates (the laid-out
/// tree/dock rect for tree dividers, the whole content area for dock
/// edges — mouse events arrive in window space, so the rect is stored
/// pre-offset by the sidebar/toolbar chrome rather than converting every
/// event), `axis` picks the resize cursor while dragging, `epoch` is
/// [`Workspaces::switch_epoch`] at mouse-down (review fix: the keyboard
/// stays live during a drag, so mod+N can switch workspaces mid-drag —
/// without this pin the next move would walk the NEW workspace's tree
/// with the OLD one's address and bounds, and a structurally-valid
/// address would apply to the wrong tree; a mismatch cancels the drag
/// instead. Post-merge review finding 7 upgraded the pin from the
/// workspace INDEX to the switch epoch: index equality let a
/// switch-away-and-back with no render between look like "never left" —
/// the one-frame ABA — while the epoch bumps on every actual switch, so
/// it can only compare equal when no switch happened at all), and
/// `moved` records whether any move actually changed the
/// layout. `moved` latches on the first actual change and stays latched:
/// a drag that wanders and returns to its exact starting position still
/// dirties the session on release — accepted, recorded honestly, since
/// the write is a cheap coalesced no-op and un-latching would need
/// per-drag snapshots for a case nobody will notice.
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

/// Movement (in px) a mod+mouse-down must travel before it becomes a real
/// tile drag, measured per-axis (Chebyshev — `max(|dx|, |dy|)`, the
/// cheapest metric and indistinguishable from Euclidean at this radius).
/// Recorded choice from the approved design's 4–6px range: 5px, the
/// middle — small enough that a deliberate drag never feels gated, large
/// enough that the hand tremor of a sloppy mod+click can never rearrange
/// the layout.
const TILE_DRAG_THRESHOLD: f32 = 5.0;

/// The drag ghost's fixed outline size and its offset from the cursor
/// (recorded choice: a small fixed-size 96×64 outline rect — theme
/// `primary` border, no fill — NOT a copy of the tile content and not
/// scaled to the tile: the ghost only needs to say "a tile is in hand",
/// and a fixed size keeps it legible whether the grabbed tile was a
/// full-height column or a thin dock sliver). Offset down-right so the
/// cursor tip — the thing doing the zone targeting — stays unobscured.
/// Window pixels, not `shell::scale`: the ghost is a bare outline with
/// no text inside it to keep in step with the rem.
pub(super) const TILE_DRAG_GHOST_SIZE: (f32, f32) = (96.0, 64.0);
pub(super) const TILE_DRAG_GHOST_OFFSET: f32 = 12.0;

/// An in-flight mod+drag of a tile (tile-drag task), recorded by a tile
/// body's mod+mouse-down and consumed by its own full-window drag catcher
/// (see `render` — the same capture mechanism as [`DividerDrag`]'s
/// catcher). Nothing is applied until the drop: the drag holds only the
/// grabbed tile's id, the workspace switch epoch at mouse-down (pinned
/// for the same switch-mid-drag reason as `DividerDrag::epoch`, ABA-
/// proofing included — see that field's doc), and
/// cursor positions in window space. That makes cancel truly free —
/// clearing this state undoes nothing and dirties nothing, unlike a
/// divider drag whose cancel must preserve already-applied resizes.
///
/// `active` is the movement threshold latch: false from mouse-down until
/// the cursor travels [`TILE_DRAG_THRESHOLD`] px from `origin`, so a
/// sloppy mod+click can never rearrange the layout. Recorded decisions:
/// - mod+down does NOT change focus at arm time — focus follows the moved
///   tile only on a successful drop, and an abandoned below-threshold
///   mod+click leaves everything untouched, focus included. That is about
///   TILE focus (the workspace's own notion). WINDOW focus is separate:
///   the grab re-arms `pending_focus_restore` exactly as a plain tile
///   click does, so keyboard focus is back on the shell root by the next
///   frame either way — see the comment at that assignment for the trap
///   this closes.
/// - The mod key does NOT need to stay held once the drag is armed
///   (standard WM behavior — releasing the modifier mid-drag continues
///   the drag; only the mouse button's release ends it). Modifier state
///   is read once, from the `MouseDownEvent`'s own `modifiers` field —
///   verified against the pinned platform sources: macOS fills it from
///   the native event's `modifierFlags` (`gpui_macos/src/events.rs`,
///   `read_modifiers` — `NSAlternateKeyMask` is the Option/Alt key) and
///   Windows samples the live key state (`gpui_windows/src/events.rs`,
///   `current_modifiers` — `VK_MENU` is Alt), so a `mod+down` arrives
///   with `alt: true` on both platforms.
/// - A truly lost release — a move arriving with the button no longer
///   pressed — *cancels* rather than drops: the actual release point is
///   unknown, and applying the drop at wherever the cursor happens to be
///   next would rearrange from a position the user never released at.
///   (The divider catcher's missed release *finishes* instead — correct
///   there because its effects were already applied live; here nothing
///   is applied until an actual drop.) The catcher's `on_mouse_up_out`,
///   by contrast, routes through the drop like `on_mouse_up` does
///   (review blocker fix): gpui's keyboard-modality hover suppression
///   makes `up_out` fire for an ordinary in-window release whenever a
///   keystroke was the last input, and the keyboard is hot mid-drag —
///   see the catcher's own comment in `render` for the full mechanism.
///   An actually-outside-window release still applies nothing that way,
///   because no drop target exists outside every tile and dock.
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
    /// Apply the active divider drag at a window-space cursor position
    /// (drag-splitters task): route to the matching pure verb with the
    /// geometry captured at mouse-down. Returns whether the layout
    /// actually changed — and since the review round that is literal: the
    /// pure verbs compare against the current value, so a stale address,
    /// a hidden dock, and a move pinned at a clamp the divider is already
    /// sitting at all report false, and the caller skips the notify (no
    /// re-render for an identical layout). `moved` latches on the first
    /// actual change (see [`DividerDrag`]'s doc for the
    /// returns-to-start caveat). Refuses — and cancels — a drag whose
    /// recorded workspace is no longer active (review fix): the render-top
    /// guard normally cancels first, but the check here is what makes the
    /// guarantee independent of event/render ordering. Pure math only —
    /// the caller notifies.
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

    /// Drop the active drag, keeping everything it already applied
    /// (cancel means "stop tracking the mouse", never "undo") and — review
    /// fix — dirtying the session if the drag had resized anything: the
    /// first cut's cancel paths discarded `moved`, leaving a visible
    /// resize the next session restore would silently lose. Shared by the
    /// mouse-up finish, the render-top invalidation guard, and
    /// `apply_divider_drag`'s workspace check; deliberately notify-free so
    /// the render-path callers don't schedule a frame from inside one.
    pub(super) fn cancel_divider_drag(&mut self) {
        if let Some(drag) = self.divider_drag.take()
            && drag.moved
        {
            self.session_dirty = true;
        }
    }

    /// The mouse form of `mod+f` (2026-09-19): a mod+double-click on a
    /// main-tree tile focuses it and toggles fullscreen on it. Returns
    /// true when it acted — the caller's drag-arm and click-to-focus
    /// branches must then NOT run. Both tile listeners call this AHEAD
    /// of `try_arm_tile_drag`: the second click of the pair carries the
    /// same mod+down the drag arm claims, and the arm refuses while a
    /// tile is fullscreen, which is exactly when the toggle has to run
    /// to restore the layout. The pair's FIRST click is an ordinary
    /// mod+down — it arms a pending drag its own release cancels with
    /// nothing applied (a drag applies only after `TILE_DRAG_THRESHOLD`
    /// of movement), so the two gestures never contend.
    ///
    /// `click_count == 2` rather than `>= 2`, so a triple-click toggles
    /// once rather than toggling and toggling back. Refused on a dock
    /// tile: fullscreen is main-tree-only (`Workspace::toggle_fullscreen`
    /// is a claimed no-op while a dock is focused), and a mod+down
    /// changes no focus (the `TileDrag` decision), so nothing happens at
    /// all — the same answer `mod+f` gives there. The overlay and
    /// in-flight-drag gates mirror `try_arm_tile_drag`'s, defence in
    /// depth for the same reasons.
    ///
    /// The toggle itself goes through `dispatch` with the keyboard verb's
    /// own id, so the action tail, the log line, session dirt and the
    /// focus-move re-arm all come from the one dispatch chain (spec: "one
    /// keymap, ours") rather than a second path that would drift from it.
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
            || self.modal.is_some()
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
        // reason (§3.3).
        self.pending_focus_restore = true;
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// Arm a pending tile drag from a tile body's mouse-down, if the
    /// gesture and the shell's state allow it (tile-drag task). Returns
    /// true when armed — the caller's plain click-to-focus branch must
    /// then NOT run (recorded decision on [`TileDrag`]: mod+down changes
    /// no focus at arm time). Refused — falling back to plain-click
    /// behavior — when the configured mod key isn't held, or in any state
    /// where a drag couldn't legitimately run: an overlay is up (the
    /// palette/modal catchers normally occlude tiles anyway — defense in
    /// depth), a keystroke sequence is pending (the which-key hint paints
    /// over the tiles, same gate as `dividers_active`), a tree tile is
    /// fullscreen (only one tile visible — nothing to rearrange; same
    /// gate that suppresses the divider strips), or another drag of
    /// either kind is already in flight (their catchers occlude tiles,
    /// so this is unreachable — but checking costs nothing and makes the
    /// exclusivity explicit).
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
            || self.modal.is_some()
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
        // A grab is a tile mouse-down like any other, so an occupant that
        // tracks its own `FocusHandle` (`RecordingView`, and `DataTable`
        // if it were ever focusable) takes window focus on it — but this
        // branch returns before the caller's click-to-focus tail, which is
        // where the plain click re-arms the restore. Arming it here closes
        // the focus trap that gap opened: grab a focus-tracking tile, then
        // switch workspaces mid-drag (the keyboard stays live during a
        // drag), and the render-top cancel unmounts the occupant while
        // gpui's focus still points at it — an orphaned `FocusId`, in
        // which `handle_key_down` stops firing for EVERY key until a
        // mouse click claims focus somewhere. Same restore, same reason
        // as the plain-click path (§3.3).
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

    /// Drop the in-flight tile drag with nothing applied (tile-drag task):
    /// the render-top cancel guard, a mouse-up outside the window, and a
    /// missed release all land here. Truly free — a tile drag applies
    /// nothing until its drop, so unlike `cancel_divider_drag` there is
    /// no already-applied state to keep and no session-dirty bookkeeping
    /// to do. Notify-free for the same render-path reason as its divider
    /// sibling; event-path callers notify themselves.
    pub(super) fn cancel_tile_drag(&mut self) {
        self.tile_drag = None;
    }

    /// End a drag — of either kind — on a left release its own catcher
    /// could not see (post-merge review BUG 1 for the tile drag; the
    /// fix-round should-fix extended it to the divider drag, whose arm
    /// has the identical gap. Recorded choice among the three candidates:
    /// this window-level fallback, rather than arming only on a
    /// catcher-observed move or a special-case in the catcher's own up
    /// path, because it is the only one that closes the gap at its root).
    /// The gap: a tile's `on_mouse_down` (`try_arm_tile_drag`) and a
    /// strip's `on_mouse_down` (the `DividerDrag` arm) both run from
    /// listeners in the CURRENT frame, but each drag's only up/up_out
    /// listeners belong to its catcher element, which enters the hitbox
    /// tree at the NEXT paint — and gpui dispatches every queued input
    /// event between frames, so a fast click's down and up can both land
    /// before any draw. Before this fix that release hit no listener at
    /// all and the armed drag survived indefinitely: the next frame
    /// painted the full-window catcher (grabbing cursor for tiles,
    /// resize cursor for dividers), the user's next stationary
    /// mouse-down was swallowed (neither catcher registers a down
    /// handler), and an unmodified press-drag could be APPLIED — a
    /// rearrangement with no mod key held, or a live resize of a divider
    /// the user never grabbed.
    ///
    /// Called from a pair of listeners on the root element (`render`),
    /// which is painted every frame: `on_mouse_up` (bubble, hovered) and
    /// `on_mouse_up_out` (capture, not-hovered) — between them every
    /// left release reaches this method, for BOTH drag kinds, across all
    /// four modality×state combinations:
    /// - gap frame (no catcher painted), mouse modality: the root is
    ///   hovered, its bubble `on_mouse_up` fires — the phantom clears;
    /// - gap frame, keyboard modality: nothing is hovered
    ///   (`HitboxId::is_hovered` is false under keyboard modality), the
    ///   root's capture `on_mouse_up_out` fires — same heal;
    /// - post-paint with a catcher up, mouse modality: the occluding
    ///   catcher removes the root from the hover chain, so the root's
    ///   `up_out` fires (capture phase — BEFORE the catcher's own bubble
    ///   `on_mouse_up`);
    /// - post-paint, keyboard modality: both the root's and the
    ///   catcher's `up_out` fire in capture phase, root (outermost)
    ///   first.
    ///
    /// In the two post-paint cases this method acting FIRST must not
    /// change what the catcher's own release path would have done, and
    /// the two drag kinds guarantee that differently:
    /// - tile drag: guarded to the never-activated state only — an
    ///   ACTIVE drag's release must keep routing through the catcher's
    ///   `finish_tile_drag` drop path, so an early cancel here would eat
    ///   real drops. A drag can only be active once a catcher-observed
    ///   move latched it (a catcher exists only after a paint), so in
    ///   the gap frame the guard is always met, and after a paint the
    ///   cancel equals the below-threshold no-op finish the catcher
    ///   would have performed anyway.
    /// - divider drag: cancelled UNconditionally, safe because for
    ///   dividers finish IS cancel-plus-notify — every resize was
    ///   already applied live by the moves, the catcher's own up applies
    ///   nothing positional, and `cancel_divider_drag` keeps the applied
    ///   state and dirties the session iff the drag moved. (It has no
    ///   `active` latch to guard on, and needs none.) The catcher's
    ///   subsequent finish finds the drag already gone and no-ops.
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

    /// Apply the drop that ends a tile drag (mouse-up on the tile-drag
    /// catcher). A below-threshold drag — a sloppy mod+click — applies
    /// nothing at all, focus included (recorded decision on [`TileDrag`]).
    /// The full set of render-top cancel conditions is re-checked at drop
    /// time too — workspace mismatch, palette/modal open, pending
    /// keystroke sequence (review should-fix: gpui dispatches multiple
    /// input events between frames, so `ctrl+k` followed by the release
    /// within one frame window would otherwise apply the drop underneath
    /// the just-opened palette; the render-top guard normally cancels
    /// first, but re-checking here keeps the guarantee independent of
    /// event/render ordering, the same standard `apply_divider_drag`'s
    /// workspace check sets). Fullscreen needs no shell-side re-check:
    /// [`locate_drop_target`] itself resolves no target for a fullscreen
    /// layout. Otherwise the
    /// cursor resolves through the pure [`locate_drop_target`] against
    /// *live* state — the same `dock_layout`/`Tree::layout` authorities
    /// the render pass uses, re-derived once here rather than snapshotted
    /// at mouse-down, so a keyboard split mid-drag can't make the drop
    /// land beside a tile the user isn't seeing — and routes to the
    /// matching pure `Workspace` drop verb: center → swap, edge → split-
    /// insert, dock background → move-to-dock-convention insert. The
    /// verbs own every focus/region/auto-hide rule and report whether the
    /// layout changed; only a real change dirties the session (no-op
    /// drops — self-drops, a release over nothing — don't).
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
            // Epoch, not index (post-merge review finding 7): index
            // equality let a switch-away-and-back with no render between
            // satisfy the letter of the re-check — the epoch bumps on
            // every actual switch, so it only matches when no switch
            // happened at all.
            && drag.epoch == self.services.workspaces.switch_epoch()
            && self.palette.is_none()
            && self.modal.is_none()
            && self.matcher.pending().is_empty()
            // The dragged tile must still exist (post-merge review BUG 3
            // — same event/render-ordering independence as the checks
            // above: ctrl+w and the release can land in one frame
            // window). The drop verbs would refuse a vanished id anyway;
            // checking here keeps the shared cancel conditions one list.
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
                }) => ws.drop_swap(drag.tile, id),
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
