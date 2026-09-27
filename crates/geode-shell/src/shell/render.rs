//! Window rendering: toolbar, sidebar, status bar, tiles and docks, drag
//! feedback, and overlays. Before painting, reconcile occupants, restore
//! keyboard focus, and discard interactions whose targets are no longer live.

use gpui::prelude::*;
use gpui::{
    AnyView, App, Context, Focusable as _, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Window, div, px,
};
use gpui_component::{ActiveTheme as _, Root, TITLE_BAR_HEIGHT, h_flex, v_flex};

use geode_core::query::AsOf;

use crate::fonts;
use crate::palette;
use crate::perf;
use crate::tiling::{
    DIVIDER_HIT_WIDTH, DropTarget, Orientation, Rect, divider_strips, dock_edge_strips,
    drop_highlight_rect,
};

use super::drag::{
    DividerDrag, DividerDragTarget, StripSpec, TILE_DRAG_GHOST_OFFSET, TILE_DRAG_GHOST_SIZE,
};
use super::{
    ShellView, addfilter, asof_view, choicedialog, commandline_view, dialog, objectdialog,
    perf_overlay, picker, scope_expr_view, sidebar, stacklist, status, toolbar, whichkey,
};

/// Shared hover-group name for divider strips. GPUI resolves each line
/// against its innermost enclosing group, so only that strip lights up.
const DIVIDER_GROUP: &str = "divider-strip";

/// Height, in pixels, of the as-of warning stripe painted
/// directly under the toolbar while the frame is historical.
const AS_OF_STRIPE_HEIGHT: f32 = 3.0;

/// The tile surface's pixel area: the viewport minus the sidebar, the
/// toolbar and the status bar — what `drag.rs` hit-tests against and
/// what `add_tile` lays out for `AddDirection::Auto`. `render` subtracts
/// the as-of stripe on top of this itself.
pub(super) fn content_area(window: &Window) -> Rect {
    let viewport = window.viewport_size();
    let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
    Rect {
        x: 0.0,
        y: 0.0,
        w: (f32::from(viewport.width) - sidebar::width(window)).max(0.0),
        h: (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0),
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Frame-time instrumentation, first thing so the
        // interval is measured from the true top of each render. Records
        // the render-to-render interval — see `crate::perf`'s module doc
        // for exactly what this signal captures (consecutive renders
        // during interaction bursts) and doesn't (compositor time, the
        // last frame before idleness). O(1), allocation-free, and it
        // never notifies or schedules anything, so recording can't force
        // a frame; intervals >= IDLE_CUTOFF are counted as idle gaps,
        // not frames.
        let render_started = std::time::Instant::now();
        if let Some(prev) = self.last_render_started {
            let interval = render_started.saturating_duration_since(prev);
            if interval < perf::IDLE_CUTOFF {
                self.perf.record_micros(interval.as_micros() as u64);
            } else {
                self.perf.note_discarded_idle();
            }
        }
        self.last_render_started = Some(render_started);

        // Restore focus for deferred paths that lack a `Window`, before any
        // painting. Preserve a focused tile's own insert-mode input using the
        // same predicate as key routing, so a mouse-opened editor keeps focus.
        // Hidden-tile focus is reconciled separately by `ensure_occupants`.
        if self.pending_focus_restore {
            self.pending_focus_restore = false;
            // An open dialog owns focus: the flag must not pull it to the shell
            // root, and a palette dropped over the stack (a reload closing it)
            // must not leave it on the orphaned palette input either.
            if self.modal_open() {
                super::dialog::refocus_top(self, window, cx);
            } else if !self.occupant_holds_insert_focus(window, cx) {
                self.focus_handle.focus(window, cx);
            }
        }

        // Restore shell focus when no live focus handle remains, such as after
        // a focused occupant is destroyed. Retained hidden occupants still have
        // handles and need `ensure_occupants`'s visibility check instead.
        // Do not reclaim focus from live inputs that legitimately own a caret.
        if window.focused(cx).is_none() {
            self.focus_handle.focus(window, cx);
        }

        // Close a member list when its tile loses workspace focus or leaves
        // the stack. This check needs no occupant or geometry reconciliation.
        if self.stack_list.as_ref().is_some_and(|l| {
            self.services.workspaces.active().focused_tile() != Some(l.tile)
                || self
                    .services
                    .workspaces
                    .active()
                    .stack_position(l.tile)
                    .is_none()
        }) {
            self.stack_list = None;
        }

        // Reconcile occupants and visibility for every path that changes tiles.
        self.ensure_occupants(window, cx);

        // Stop divider tracking when an overlay, fullscreen, or workspace switch
        // hides its boundary. Keep and persist resizes already applied.
        if self.divider_drag.as_ref().is_some_and(|drag| {
            self.palette.is_some()
                || self.modal_open()
                // Epoch, not index — see `DividerDrag::epoch`.
                || drag.epoch != self.services.workspaces.switch_epoch()
                || self
                    .services
                    .workspaces
                    .active()
                    .tree()
                    .fullscreen()
                    .is_some()
        }) {
            self.cancel_divider_drag();
        }

        // Cancel tile tracking when overlays obscure its targets, the workspace
        // changes, fullscreen begins, or the grabbed tile disappears. No layout
        // change has been applied, so cancellation needs no persistence update.
        if self.tile_drag.as_ref().is_some_and(|drag| {
            self.palette.is_some()
                || self.modal_open()
                || !self.matcher.pending().is_empty()
                // Epoch, not index — see `DividerDrag::epoch`.
                || drag.epoch != self.services.workspaces.switch_epoch()
                || self
                    .services
                    .workspaces
                    .active()
                    .tree()
                    .fullscreen()
                    .is_some()
                || self
                    .services
                    .workspaces
                    .active()
                    .region_of(drag.tile)
                    .is_none()
        }) {
            self.cancel_tile_drag();
        }

        // Keep a command line open only while its tile remains focused and its
        // input owns keyboard focus. This catches sidebar switches and scope
        // input clicks as well as the explicit tile-click paths. Leaving commits
        // a find and cancels a command; it is distinct from Escape cancellation.
        if self.command_line.as_ref().is_some_and(|line| {
            self.services.workspaces.active().focused_tile() != Some(line.tile)
                || !self
                    .command_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
        }) {
            self.leave_command_line(window, cx);
        }

        // Apply the UI font size (see the `fontsize` module doc): the rem
        // size scales every rem-based text size in the shell and, since
        // `shell::scale`, every chrome length too. The guard is a compare,
        // not a real skip: gpui-component's `Root::render` sets the rem to
        // its own `Theme.font_size` (16 px, which Geode never changes) on
        // EVERY frame before this view renders, so this setter fires every
        // frame too — harmless, but it means every rem read this render
        // makes (`content_area`, `sidebar::width`, `status::height`,
        // `rem_size` below) must sit AFTER this line, and nothing rendered
        // between `Root`'s set and this one may read the rem.
        let rem = gpui::px(self.font_size.rem_px());
        if window.rem_size() != rem {
            window.set_rem_size(rem);
        }

        // Layout uses the viewport minus toolbar, sidebar, and status bar. A
        // historical frame adds a warning stripe below the toolbar; subtract its
        // height before sizing the tile surface so content fits below it.
        let is_historical = matches!(self.frame.read(cx).as_of(), AsOf::At(_));
        let stripe_height = if is_historical {
            AS_OF_STRIPE_HEIGHT
        } else {
            0.0
        };
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        // The stripe-free surface is the shared `content_area` (the same
        // one `drag.rs` hit-tests and `add_tile` lays out for `Auto`);
        // only the stripe subtraction is `render`'s own.
        let surface = content_area(window);
        let tile_width = surface.w;
        let content_height = (surface.h - stripe_height).max(0.0);
        // The rail's width at this rem, read once: `set_rem_size` above
        // ran first, so this render and `content_area` agree.
        let sidebar_width = sidebar::width(window);
        let status_height = status::height(window);
        let rem_size = window.rem_size();

        // Lay out each region once. Dock geometry carves space from the surface;
        // the main tree fills the remainder and each visible dock lays out its
        // own tree. Fullscreen uses the whole surface and hides dock rendering
        // while retaining dock state.
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: tile_width,
            h: content_height,
        };
        type DockCell = (
            crate::tiling::DockSide,
            Rect,
            Vec<(crate::tiling::TileId, Rect)>,
            Option<crate::tiling::TileId>,
        );
        // Create interactive divider strips only when no palette, modal, or
        // which-key panel covers the tiles. Those overlays do not all occlude
        // underlying hitboxes, so retaining strip listeners could start a drag
        // with the same click that dismisses an overlay. An existing divider
        // drag can continue under which-key because its catcher owns the mouse.
        let dividers_active =
            self.palette.is_none() && !self.modal_open() && self.matcher.pending().is_empty();
        // Mouse events arrive in window coordinates while the tile
        // geometry lives in surface coordinates (the surface starts below
        // the toolbar, right of the sidebar) — the drag rects captured at
        // mouse-down are pre-offset into window space so the per-move math
        // never converts.
        let to_window_space = |r: Rect| Rect {
            x: r.x + sidebar_width,
            y: r.y + toolbar_height,
            w: r.w,
            h: r.h,
        };
        let (region, focused, tree_area, rects, dock_cells, strips) = {
            let workspace = self.services.workspaces.active();
            let tree = workspace.tree();
            let (tree_area, dock_rects) = if tree.fullscreen().is_some() {
                (area, Vec::new())
            } else {
                crate::tiling::dock_layout(workspace.docks(), area)
            };
            // The frame's divider strips, from the same rects this pass
            // just computed: the main tree's
            // interior boundaries, each visible dock tree's interior
            // boundaries, then the dock frame edges — edges last so they
            // paint above a dock tree's own strips where the two meet at
            // a corner (hit-testing follows paint order). `divider_strips`
            // itself yields nothing for a fullscreen tree, and
            // `dock_rects` is already empty then, so fullscreen suppresses
            // every strip without a separate check here.
            let mut strips: Vec<StripSpec> = Vec::new();
            if dividers_active {
                let tree_bounds = to_window_space(tree_area);
                for s in divider_strips(tree, tree_area, DIVIDER_HIT_WIDTH) {
                    strips.push(StripSpec {
                        rect: s.rect,
                        axis: s.orientation,
                        target: DividerDragTarget::MainTree { address: s.address },
                        drag_bounds: tree_bounds,
                    });
                }
                for &(side, r) in &dock_rects {
                    let dock_bounds = to_window_space(r);
                    for s in
                        divider_strips(workspace.docks().get(side).tree(), r, DIVIDER_HIT_WIDTH)
                    {
                        strips.push(StripSpec {
                            rect: s.rect,
                            axis: s.orientation,
                            target: DividerDragTarget::DockTree {
                                side,
                                address: s.address,
                            },
                            drag_bounds: dock_bounds,
                        });
                    }
                }
                let area_bounds = to_window_space(area);
                for e in dock_edge_strips(&dock_rects, DIVIDER_HIT_WIDTH) {
                    strips.push(StripSpec {
                        rect: e.rect,
                        axis: e.orientation,
                        target: DividerDragTarget::DockEdge { side: e.side },
                        drag_bounds: area_bounds,
                    });
                }
            }
            // Each visible dock carries its own tile layout plus its
            // tree's focused tile (the ring shows on the focused dock's
            // focused tile only — still at most one ring per workspace,
            // region-gated below).
            let dock_cells: Vec<DockCell> = dock_rects
                .into_iter()
                .map(|(side, r)| {
                    let dock_tree = workspace.docks().get(side).tree();
                    (side, r, dock_tree.layout(r), dock_tree.focused())
                })
                .collect();
            (
                workspace.region(),
                tree.focused(),
                tree_area,
                tree.layout(tree_area),
                dock_cells,
                strips,
            )
        };

        // Resolve preview targets with the same core as the eventual drop,
        // using this frame's borrowed layout rectangles without a second layout
        // pass. Highlight split halves, stack centers, or dock backgrounds.
        // Suppress self-drops and the source dock's background, which workspace
        // operations would refuse, so the preview promises a real change.
        let drop_highlight: Option<Rect> = self
            .tile_drag
            .as_ref()
            .filter(|drag| drag.active)
            .and_then(|drag| {
                let sx = drag.cursor.0 - sidebar_width;
                let sy = drag.cursor.1 - toolbar_height;
                let dragged = drag.tile;
                let target = crate::tiling::resolve_drop_target(
                    dock_cells
                        .iter()
                        .map(|(side, r, tiles, _)| (*side, *r, tiles.as_slice())),
                    &rects,
                    sx,
                    sy,
                )?;
                match target {
                    (DropTarget::Tile { id, .. }, _) if id == dragged => None,
                    (DropTarget::Tile { id: _, zone }, tr) => Some(drop_highlight_rect(tr, zone)),
                    (DropTarget::DockBackground { side }, frame) => {
                        let already_here = self
                            .services
                            .workspaces
                            .active()
                            .docks()
                            .get(side)
                            .tree()
                            .contains(dragged);
                        (!already_here).then_some(frame)
                    }
                }
            });

        // The shared tile chrome — identical for tree tiles and docked
        // tiles (a docked tile is the same kind of tile, just parked): 1px
        // inset, themed background, `primary` 2px ring on the one focused
        // tile, mono placeholder label. At most one tile per workspace
        // shows the focused ring: the main tree's focused tile only counts
        // as focused while `region == Main`, and a dock tile only when
        // focus lives in that dock AND the dock's own tree has it focused
        // A dock holds many tiles, with one focused ring.
        let tile_cell = |id: crate::tiling::TileId,
                         r: Rect,
                         is_focused: bool,
                         view: Option<AnyView>,
                         cx: &App| {
            div()
                .absolute()
                .left(px(r.x + 1.0))
                .top(px(r.y + 1.0))
                .w(px((r.w - 2.0).max(0.0)))
                .h(px((r.h - 2.0).max(0.0)))
                .bg(cx.theme().background)
                .border_color(if is_focused {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                // Border + padding is a constant 2px in both states: gpui
                // sizes a box border-box, so a ring that simply grew from
                // 1px to 2px on focus handed the occupant a content box
                // 1px smaller on every side, and every row jogged a pixel
                // whenever focus moved (`moving_focus_does_not_shift_tile_
                // content`). The unfocused tile pads the missing pixel
                // instead, in its own background.
                .when(is_focused, |el| el.border_2())
                .when(!is_focused, |el| el.border_1().p(px(1.0)))
                .overflow_hidden()
                .map(|el| match view {
                    Some(view) => el.child(view),
                    None => el
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_family(fonts::MONO)
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("tile {}", id.0)),
                })
        };

        // Fixed-size (not `size_full`) so it never competes with the
        // sidebar/status bar for space: the tile tree is laid out over
        // exactly this rect above, and the container must match.
        let mut surface = div()
            .relative()
            .w(px(tile_width))
            .h(px(content_height))
            .flex_none();

        // The focused tile's rect, in window space (`to_window_space`),
        // for the command line strip: it paints along that tile's
        // bottom edge, not the surface's, so it must follow focus into a
        // dock exactly like the focused ring does. Captured from the same
        // per-tile `is_focused` computed in the loops below rather than a
        // second lookup — `region`/`focused`/`dock_cells` already say
        // which tile, if any, is the one ring shows.
        let mut focused_rect: Option<Rect> = None;
        if rects.is_empty() {
            // Place the empty-tree hint in the area remaining after visible docks.
            // Adding targets the focused region; name a focused dock in the hint so
            // it does not promise to fill the empty main area instead.
            let hint = match region {
                crate::tiling::FocusRegion::Main => {
                    "double-click or `ctrl+k` → Add a tile".to_string()
                }
                crate::tiling::FocusRegion::Dock(side) => {
                    let side = match side {
                        crate::tiling::DockSide::Left => "left",
                        crate::tiling::DockSide::Right => "right",
                        crate::tiling::DockSide::Bottom => "bottom",
                    };
                    format!("`ctrl+k` → Add a tile · focus is in the {side} dock")
                }
            };
            let selector = "empty-hint";
            surface = surface.child(
                div()
                    .absolute()
                    .left(px(tree_area.x))
                    .top(px(tree_area.y))
                    .w(px(tree_area.w))
                    .h(px(tree_area.h))
                    .flex()
                    .items_center()
                    .justify_center()
                    // A bare double-click anywhere on the empty tree
                    // opens the tile picker, the mouse form
                    // of the `ctrl+k` the hint names — see
                    // `try_pick_on_empty_tree_double_click`. A single
                    // click is nothing: there is no tile to focus.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, event: &MouseDownEvent, window, cx| {
                            view.try_pick_on_empty_tree_double_click(event, window, cx);
                        }),
                    )
                    .child(
                        div()
                            // Test-only hook (no-op outside test/test-support
                            // builds): lets a `#[gpui::test]` confirm this branch
                            // actually painted via `VisualTestContext::debug_bounds`
                            // — gpui's test API has no way to inspect painted text
                            // content itself, so this is the closest honest check
                            // available for "the hint painted".
                            .debug_selector(|| selector.to_string())
                            .text_color(cx.theme().muted_foreground)
                            .child(super::kbd::marked(&hint)),
                    ),
            );
        } else {
            for (id, r) in rects {
                let is_focused = region == crate::tiling::FocusRegion::Main && focused == Some(id);
                if is_focused {
                    focused_rect = Some(to_window_space(r));
                }
                let view = self.occupants.get(&id).map(|o| o.view.clone());
                surface = surface.child(
                    tile_cell(id, r, is_focused, view, cx)
                        // Click-to-focus is a convenience: keyboard (hjkl)
                        // remains the primary path through the same
                        // `Workspace::focus_main_tile` seam — a click on a
                        // tree tile also returns the region to Main, and
                        // both region and focus persist, so the session
                        // goes dirty like any workspace-mutating dispatch.
                        // With the configured mod key held, the same
                        // mouse-down instead arms a pending tile drag
                        // — and deliberately does NOT
                        // focus: see `try_arm_tile_drag` / `TileDrag`.
                        // "Does not focus" is about TILE focus, the
                        // workspace's own notion. Window focus is a
                        // separate matter: the grab re-arms
                        // `pending_focus_restore` like every other tile
                        // mouse-down, so keyboard focus is back on the
                        // shell root by the next frame regardless.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                                // Leave the old tile's command line before changing focus: commit
                                // a find, cancel a command. The render-time check covers other
                                // surfaces that move tile or input focus.
                                view.leave_command_line(window, cx);
                                // Ahead of the drag arm on purpose — see
                                // `try_fullscreen_on_double_click`; the
                                // placeholder's bare double-click opens
                                // the tile picker and must skip the tail
                                // (`try_pick_tile_on_double_click`).
                                if view.try_fullscreen_on_double_click(id, event, window, cx)
                                    || view.try_pick_tile_on_double_click(id, event, window, cx)
                                    || view.try_arm_tile_drag(id, event, cx)
                                {
                                    return;
                                }
                                if view.services.workspaces.active_mut().focus_main_tile(id) {
                                    view.session_dirty = true;
                                }
                                // An occupant can claim window focus on mouse-down. Re-arm
                                // restoration so the next render preserves a valid editor or
                                // returns the keyboard to the shell.
                                view.pending_focus_restore = true;
                                cx.notify();
                            }),
                        )
                        // Right press selects the tile so its context-menu keys route
                        // to that occupant. Reuse focus restoration without arming a
                        // drag or handling double-click gestures.
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |view, _event: &MouseDownEvent, window, cx| {
                                view.leave_command_line(window, cx);
                                if view.services.workspaces.active_mut().focus_main_tile(id) {
                                    view.session_dirty = true;
                                }
                                view.pending_focus_restore = true;
                                cx.notify();
                            }),
                        ),
                );
            }
        }

        // Dock trees use the same tile cells and gestures as the main tree.
        // Empty docks show add and move hints using physical key labels; clicks
        // focus the empty region before opening its picker.
        for (side, r, dock_tiles, dock_focused) in dock_cells {
            if !dock_tiles.is_empty() {
                for (id, tr) in dock_tiles {
                    let is_focused = region == crate::tiling::FocusRegion::Dock(side)
                        && dock_focused == Some(id);
                    if is_focused {
                        focused_rect = Some(to_window_space(tr));
                    }
                    // Same mod+down drag-arming branch as the tree tiles
                    // above — a docked tile is the same kind of tile, and
                    // drags work from any source region.
                    let view = self.occupants.get(&id).map(|o| o.view.clone());
                    surface = surface.child(
                        tile_cell(id, tr, is_focused, view, cx)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                                    // Leave the previous command line before focusing this dock tile.
                                    view.leave_command_line(window, cx);
                                    // The fullscreen door refuses a docked tile
                                    // (fullscreen is main-tree-only), but it is
                                    // called here too so both listeners read the
                                    // same gesture table.
                                    if view.try_fullscreen_on_double_click(id, event, window, cx)
                                        || view.try_pick_tile_on_double_click(id, event, window, cx)
                                        || view.try_arm_tile_drag(id, event, cx)
                                    {
                                        return;
                                    }
                                    if view
                                        .services
                                        .workspaces
                                        .active_mut()
                                        .focus_dock_tile(side, id)
                                    {
                                        view.session_dirty = true;
                                    }
                                    // Dock occupants can also claim window focus; use the same
                                    // deferred restoration as main-tree clicks.
                                    view.pending_focus_restore = true;
                                    cx.notify();
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                // The tree tile's right-press focus tail, for a
                                // docked tile (same reason, same shape).
                                cx.listener(move |view, _event: &MouseDownEvent, window, cx| {
                                    view.leave_command_line(window, cx);
                                    if view
                                        .services
                                        .workspaces
                                        .active_mut()
                                        .focus_dock_tile(side, id)
                                    {
                                        view.session_dirty = true;
                                    }
                                    view.pending_focus_restore = true;
                                    cx.notify();
                                }),
                            ),
                    );
                }
            } else {
                let (hint, selector) = match side {
                    crate::tiling::DockSide::Left => (
                        "double-click or `ctrl+k` → Add a tile here · `ctrl+shift+[` moves one",
                        "dock-empty-hint-left",
                    ),
                    crate::tiling::DockSide::Right => (
                        "double-click or `ctrl+k` → Add a tile here · `ctrl+shift+]` moves one",
                        "dock-empty-hint-right",
                    ),
                    crate::tiling::DockSide::Bottom => (
                        "double-click or `ctrl+k` → Add a tile here · `ctrl+shift+/` moves one",
                        "dock-empty-hint-bottom",
                    ),
                };
                surface = surface.child(
                    div()
                        .absolute()
                        .left(px(r.x + 1.0))
                        .top(px(r.y + 1.0))
                        .w(px((r.w - 2.0).max(0.0)))
                        .h(px((r.h - 2.0).max(0.0)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_color(cx.theme().muted_foreground)
                        // Focus the empty dock on click; double-click opens its tile picker.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                                view.on_empty_dock_mouse_down(side, event, window, cx);
                            }),
                        )
                        .child(
                            div()
                                .debug_selector(|| selector.to_string())
                                .child(super::kbd::marked(hint)),
                        ),
                );
            }
        }

        // The divider strips, painted after — so
        // above — every tile and dock cell: transparent hit areas
        // `DIVIDER_HIT_WIDTH` wide centered on each draggable boundary,
        // each carrying a 2px line that lights up `primary` on hover (and
        // stays lit on the strip being dragged, whose cursor may be far
        // away mid-drag). `.occlude()` is what makes a strip's mouse-down
        // win over the click-to-focus listener of the tile edges it
        // overlaps: an occluding hitbox removes everything painted below
        // it from the hover chain, so the tile's own `on_mouse_down`
        // (hover-gated by gpui) never fires — same mechanism
        // gpui-component's `ResizeHandle` relies on. The mouse-down only
        // *records* the drag; the moves are handled by the full-window
        // drag catcher near the end of this method, because a fast drag
        // leaves this thin strip immediately (the capture problem).
        for (i, spec) in strips.into_iter().enumerate() {
            let StripSpec {
                rect: r,
                axis,
                target,
                drag_bounds,
            } = spec;
            let is_active = self
                .divider_drag
                .as_ref()
                .is_some_and(|drag| drag.target == target);
            let line = div()
                .group_hover(DIVIDER_GROUP, |s| s.bg(cx.theme().primary))
                .when(is_active, |el| el.bg(cx.theme().primary))
                .map(|el| match axis {
                    Orientation::Horizontal => el.w(px(2.0)).h_full(),
                    Orientation::Vertical => el.h(px(2.0)).w_full(),
                });
            surface = surface.child(
                div()
                    .absolute()
                    .left(px(r.x))
                    .top(px(r.y))
                    .w(px(r.w))
                    .h(px(r.h))
                    .occlude()
                    .group(DIVIDER_GROUP)
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|el| match axis {
                        Orientation::Horizontal => el.cursor_col_resize(),
                        Orientation::Vertical => el.cursor_row_resize(),
                    })
                    // Test-only hook (no-op outside test builds), same
                    // honest-limitation story as the empty hints above:
                    // lets a `#[gpui::test]` confirm strips painted (or
                    // didn't — fullscreen) via `debug_bounds`.
                    .debug_selector(|| format!("divider-strip-{i}"))
                    // Recorded interaction with the tile-drag gesture: a
                    // mod+mouse-down landing within the 8px strip starts a
                    // divider RESIZE, never a tile drag — the strip
                    // occludes the tile body it overlaps and this handler
                    // checks no modifiers. Deterministic and accepted: a
                    // mod+drag aimed within ~4px of a tile's edge grabs
                    // the divider instead of the tile.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _event, _window, cx| {
                            view.divider_drag = Some(DividerDrag {
                                target: target.clone(),
                                bounds: drag_bounds,
                                axis,
                                // Pin the workspace era the drag belongs
                                // to (read at mouse-down, not paint): a
                                // mod+N switch mid-drag cancels rather
                                // than retargeting — see `DividerDrag`.
                                epoch: view.services.workspaces.switch_epoch(),
                                moved: false,
                            });
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(line),
            );
        }

        // The zone highlight, painted after — so above —
        // every tile, dock cell, and divider strip: a translucent
        // theme-primary wash over exactly the area the drop would occupy.
        // Instant, no animation; no handlers and no `.occlude()`, so it
        // never competes for the mouse events the tile-drag catcher below
        // owns. `primary.opacity(0.2)` is the established translucent-
        // accent pattern (sidebar workspace pill, keybindings match
        // highlight), not a raw color.
        if let Some(hr) = drop_highlight {
            surface = surface.child(
                div()
                    .absolute()
                    .left(px(hr.x))
                    .top(px(hr.y))
                    .w(px(hr.w))
                    .h(px(hr.h))
                    .bg(cx.theme().primary.opacity(0.2))
                    // Test hook, same honest-limitation story as the
                    // divider strips': painted-or-not via `debug_bounds`.
                    .debug_selector(|| "tile-drop-highlight".to_string()),
            );
        }

        let active_index = self.services.workspaces.active_index();
        let non_empty = self.services.workspaces.non_empty_indices();
        let reload_message = self.last_reload.status_message();
        // Share the cached scope bar model between toolbar and status bar.
        // Use the poll-updated date and configured clock, avoiding a fresh
        // clock read during rendering.
        let bar_model = self.frame.read(cx).bar_model(self.clock(cx), self.today);
        // Read the cached diagnostics summary. Cloning its shared string
        // increments a reference count without copying its buffer.
        let diagnostics_read = self.diagnostics.read(cx);
        let diagnostics_summary = diagnostics_read.summary();
        // Prepared when a thread stops; borrowed, never formatted here.
        let stopped = diagnostics_read.stopped_segment();
        // Borrow ingest activity through the status-bar call without cloning.
        // Nothing before that call needs a mutable context.
        let ingest = diagnostics_read.ingest.as_ref();
        // Clicking the summary opens the diagnostics tile.
        let diagnostics_click_entity = cx.entity();
        let on_diagnostics_click = move |window: &mut Window, cx: &mut App| {
            diagnostics_click_entity.update(cx, |view, cx| {
                view.open_module("diagnostics", window, cx);
            });
        };
        // Clicking the fullscreen segment restores the layout through the
        // same action `mod+f` and a tile double-click dispatch.
        let fullscreen_hidden = self.services.workspaces.active().fullscreen_hidden();
        let fullscreen_click_entity = cx.entity();
        let on_fullscreen_click = move |window: &mut Window, cx: &mut App| {
            fullscreen_click_entity.update(cx, |view, cx| {
                view.dispatch(
                    &crate::actions::ActionId("workspace::fullscreen_tile".into()),
                    None,
                    window,
                    cx,
                );
                cx.notify();
            });
        };
        let status_bar = status::status_bar(
            self.matcher.pending(),
            self.matcher.count(),
            reload_message.as_deref(),
            self.config_write_error.as_deref(),
            self.restart_required.as_deref(),
            self.notice,
            stopped,
            (!diagnostics_summary.is_empty()).then_some(diagnostics_summary.as_ref()),
            on_diagnostics_click,
            ingest,
            fullscreen_hidden,
            on_fullscreen_click,
            bar_model.as_of.as_deref(),
            bar_model.as_of_full.as_ref(),
            self.services.theme.active_name(),
            cx,
        );
        let sidebar = sidebar::sidebar(active_index, &non_empty, cx);
        // A chip's close glyph drops that dimension from the scope
        // — an undoable edit, same door as every other scope
        // mutation. Built here, not `cx.listener` (whose signature takes
        // `&Evt`, not `&str`): capture `cx.entity()` and update through
        // it, matching `on_chip_close`'s plain-`Fn(&str, ..)` shape.
        let chip_close_entity = cx.entity();
        let on_chip_close = move |column: &str, _window: &mut Window, cx: &mut App| {
            chip_close_entity.update(cx, |view, cx| {
                view.frame.update(cx, |f, cx| {
                    if f.drop_dimension(column) {
                        cx.notify();
                    }
                });
            });
        };
        // Open the dimension picker through an entity handle so the callback
        // can update `ShellView`.
        let chip_open_entity = cx.entity();
        let on_chip_open = move |column: &str, window: &mut Window, cx: &mut App| {
            let column = column.to_string();
            chip_open_entity.update(cx, |view, cx| {
                picker::open(view, Some(column), window, cx);
            });
        };
        // The scope bar's `+` opens the add-a-filter menu, whose rows
        // dispatch `frame::pick` and `frame::add_expression`. Same
        // `cx.entity()`-captured shape as `on_chip_open` just above.
        let add_entity = cx.entity();
        let on_add = move |window: &mut Window, cx: &mut App| {
            add_entity.update(cx, |view, cx| {
                view.open_add_filter_menu(window, cx);
            });
        };
        // The open menu's panel, painted from its prepared rows. A row
        // click commits through `commit_add_filter` (a dispatch); hovering
        // a row highlights it.
        let add_menu = self.add_filter_menu.as_ref().map(|menu| {
            let pick_entity = cx.entity().downgrade();
            let hover_entity = pick_entity.clone();
            addfilter::render(
                menu,
                move |entry, window: &mut Window, cx: &mut App| {
                    let _ = pick_entity
                        .update(cx, |view, cx| view.commit_add_filter(entry, window, cx));
                },
                move |i, _window: &mut Window, cx: &mut App| {
                    let _ = hover_entity.update(cx, |view, cx| view.hover_add_filter(i, cx));
                },
                cx,
            )
            .into_any_element()
        });
        // The scope bar's save glyph — the mouse form of
        // `scope::save_current`, through the same door `input.rs`'s
        // dispatch arm uses.
        let save_chip_entity = cx.entity();
        let on_save = move |window: &mut Window, cx: &mut App| {
            save_chip_entity.update(cx, |view, cx| {
                objectdialog::render::open_save_scope(view, window, cx);
            });
        };
        // The grouping readout's click — the mouse form of
        // `frame::grouping`/`mod+g`, through the same door `input.rs`'s
        // dispatch arm uses.
        let grouping_entity = cx.entity();
        let on_grouping = move |window: &mut Window, cx: &mut App| {
            grouping_entity.update(cx, |view, cx| {
                choicedialog::open_grouping(view, window, cx);
            });
        };
        // The AS OF chip's click — the mouse
        // form of `frame::as_of`/`mod+t`, through the same door `input.rs`'s
        // dispatch arm uses.
        let as_of_entity = cx.entity();
        let on_as_of = move |window: &mut Window, cx: &mut App| {
            as_of_entity.update(cx, |view, cx| {
                asof_view::open(view, window, cx);
            });
        };
        // An expression term chip's click opens the dialog on that term
        // alone; its `×` drops that term alone (an undoable edit, like a
        // dimension chip's `×`).
        let term_open_entity = cx.entity();
        let on_term_open = move |i: usize, window: &mut Window, cx: &mut App| {
            term_open_entity.update(cx, |view, cx| {
                scope_expr_view::open_term(view, i, window, cx);
            });
        };
        let term_close_entity = cx.entity();
        let on_term_close = move |i: usize, _window: &mut Window, cx: &mut App| {
            term_close_entity.update(cx, |view, cx| {
                view.frame.update(cx, |f, cx| {
                    if f.drop_expression_term(i) {
                        cx.notify();
                    }
                });
            });
        };
        // A named-expression chip's body opens the Expressions dialog on
        // that name; its `×` drops that name alone, an undoable edit like
        // the other chips' `×`.
        let named_open_entity = cx.entity();
        let on_named_open = move |name: &str, window: &mut Window, cx: &mut App| {
            named_open_entity.update(cx, |view, cx| {
                objectdialog::render::open_object(
                    view,
                    objectdialog::Domain::Expressions,
                    name,
                    window,
                    cx,
                );
            });
        };
        let named_close_entity = cx.entity();
        let on_named_close = move |name: &str, _window: &mut Window, cx: &mut App| {
            named_close_entity.update(cx, |view, cx| {
                view.frame.update(cx, |f, cx| {
                    if f.drop_named(name) {
                        cx.notify();
                    }
                });
            });
        };
        // Whether the grouping picker is up: the readout holds its pressed
        // fill for exactly as long as it is (design guide: a control that
        // owns a popup stays visibly pressed until the popup closes). The
        // tile picker shares the choice dialog and must not light it.
        let grouping_open = matches!(
            self.choice_dialog.as_ref().map(|d| &d.target),
            Some(choicedialog::Target::Grouping { .. })
        );
        // An open page replaces the workspace: toolbar, stripe, and tile
        // surface are neither built nor painted; the sidebar and status bar
        // stay. The page must be in the element tree while it holds focus:
        // gpui dispatches keys for an unrendered focus handle from the window
        // root, above this view's key listener.
        let page_open = self.page_open();
        let page_view = self
            .page
            .as_ref()
            .filter(|p| p.open)
            .map(|p| p.occupant.view.clone());
        let toolbar = if page_open {
            None
        } else {
            Some(toolbar::toolbar(
                &self.filter_input,
                &bar_model,
                grouping_open,
                add_menu,
                on_chip_close,
                on_chip_open,
                on_add,
                on_save,
                on_grouping,
                on_as_of,
                on_term_open,
                on_term_close,
                on_named_open,
                on_named_close,
                cx,
            ))
        };

        let body = match page_view {
            Some(view) => h_flex()
                .w_full()
                .h(px(content_height + toolbar_height + stripe_height))
                .flex_none()
                .child(sidebar)
                .child(
                    div()
                        .id("shell-page")
                        .debug_selector(|| "shell-page".to_string())
                        .w(px(tile_width))
                        .h(px(content_height + toolbar_height + stripe_height))
                        .flex_none()
                        .overflow_hidden()
                        .bg(cx.theme().background)
                        .child(view),
                ),
            None => h_flex()
                .w_full()
                .h(px(content_height))
                .flex_none()
                .child(sidebar)
                .child(surface),
        };

        let width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);

        // Build which-key only for a pending key sequence. A bare count leaves
        // `pending` empty and appears in the status bar instead. Reading matcher
        // state here must not change how the next keystroke resolves.
        let pending = self.matcher.pending();
        let which_key_continuations = (!pending.is_empty()).then(|| {
            whichkey::continuations(&self.services.keymap, pending, &self.context_stack(cx))
        });
        let registry = &self.services.registry;

        // Clone the modal's shared title and callbacks before building the
        // element tree, releasing the borrow of `self.modals` before closures
        // need access to the rest of the view.
        let modal = self.modals.last().map(|modal| {
            (
                modal.title.clone(),
                modal.title_extra.clone(),
                modal.build.clone(),
            )
        });

        // Extracted ahead of the render chain like `modal` above: all the
        // drag catcher below needs from the active drag is which resize
        // cursor to show — the drag itself is applied through
        // `apply_divider_drag`, which re-reads `self.divider_drag` per
        // event.
        let drag_axis = self.divider_drag.as_ref().map(|drag| drag.axis);

        // Extracted the same way for the tile-drag catcher and ghost: the
        // catcher exists from arm (so it can see the threshold-crossing
        // moves), the ghost only once the drag is active.
        let tile_drag_armed = self.tile_drag.is_some();
        let tile_drag_ghost = self
            .tile_drag
            .as_ref()
            .filter(|drag| drag.active)
            .map(|drag| drag.cursor);

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            // `GeodeShell` reclaims tab/shift-tab from Root's window-wide focus
            // cycling so they reach the shell matcher, including module bindings.
            // `GeodeModalOpen` additionally reclaims modal commands while focus is
            // on the shell root; the panel's context is absent in Normal mode.
            .key_context(if self.modal_open() {
                "GeodeShell GeodeModalOpen"
            } else {
                "GeodeShell"
            })
            .on_key_down(cx.listener(Self::handle_key_down))
            // Cover releases between drag arming and the first catcher paint.
            // Hovered and non-hovered listeners together cover mouse and keyboard
            // modality; `heal_drags_on_root_release` preserves active tile drops.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _event, _window, cx| {
                    view.heal_drags_on_root_release(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _event, _window, cx| {
                    view.heal_drags_on_root_release(cx);
                }),
            )
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when_some(toolbar, |el, toolbar| el.child(toolbar))
            // Paint the warning stripe only for `AsOf::At` and no page. Its
            // height was already subtracted from the tile surface.
            .when(is_historical && !page_open, |el| {
                el.child(
                    div()
                        .w_full()
                        .h(px(AS_OF_STRIPE_HEIGHT))
                        .bg(cx.theme().warning)
                        .debug_selector(|| "as-of-stripe".to_string()),
                )
            })
            .child(body)
            .child(status_bar)
            // Capture divider moves and release with a full-window occluding
            // layer. Fast drags leave the narrow strip, so the catcher maintains
            // hit coverage and resize cursor shape. A release through either
            // mouse-up route, or a later buttonless move, ends tracking while
            // preserving resizes already applied.
            .when_some(drag_axis, |el, axis| {
                el.child(
                    div()
                        .id("divider-drag-catcher")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(width))
                        .h(px(viewport_height))
                        .occlude()
                        .map(|el| match axis {
                            Orientation::Horizontal => el.cursor_col_resize(),
                            Orientation::Vertical => el.cursor_row_resize(),
                        })
                        .debug_selector(|| "divider-drag-catcher".to_string())
                        .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _window, cx| {
                            // Only a buttonless move means the release was lost. Ignore
                            // moves attributed to a second button during a chorded press.
                            match event.pressed_button {
                                None => {
                                    // The release happened where we
                                    // couldn't see it — treat the first
                                    // buttonless move as the mouse-up.
                                    view.finish_divider_drag(cx);
                                }
                                Some(MouseButton::Left) => {
                                    if view.apply_divider_drag(
                                        f32::from(event.position.x),
                                        f32::from(event.position.y),
                                    ) {
                                        cx.notify();
                                    }
                                }
                                Some(_) => {}
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|view, _event, _window, cx| {
                                view.finish_divider_drag(cx);
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|view, _event, _window, cx| {
                                view.finish_divider_drag(cx);
                            }),
                        ),
                )
            })
            // Capture tile moves and release across the whole window, occluding
            // tile and divider hitboxes. Only one drag kind can be active.
            // A buttonless move cancels without applying a drop. Both mouse-up
            // routes attempt the drop: keyboard modality suppresses hover and
            // can send an in-window release through `on_mouse_up_out`. A release
            // outside the layout has no target and changes nothing.
            .when(tile_drag_armed, |el| {
                el.child(
                    div()
                        .id("tile-drag-catcher")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(width))
                        .h(px(viewport_height))
                        .occlude()
                        .cursor_grabbing()
                        .debug_selector(|| "tile-drag-catcher".to_string())
                        .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _window, cx| {
                            // Ignore moves attributed to non-left buttons: platforms may
                            // report a second held button differently. Only `None` proves
                            // all buttons were released and cancels the tile drag.
                            match event.pressed_button {
                                None => {
                                    view.cancel_tile_drag();
                                    cx.notify();
                                }
                                Some(MouseButton::Left) => {
                                    view.update_tile_drag(
                                        f32::from(event.position.x),
                                        f32::from(event.position.y),
                                        cx,
                                    );
                                }
                                Some(_) => {}
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|view, event: &MouseUpEvent, window, cx| {
                                view.finish_tile_drag(
                                    f32::from(event.position.x),
                                    f32::from(event.position.y),
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|view, event: &MouseUpEvent, window, cx| {
                                view.finish_tile_drag(
                                    f32::from(event.position.x),
                                    f32::from(event.position.y),
                                    window,
                                    cx,
                                );
                            }),
                        ),
                )
            })
            // Paint the fixed-size drag outline above its catcher. With no
            // handlers or occlusion, it cannot intercept the catcher's events.
            .when_some(tile_drag_ghost, |el, (gx, gy)| {
                el.child(
                    div()
                        .absolute()
                        .left(px(gx + TILE_DRAG_GHOST_OFFSET))
                        .top(px(gy + TILE_DRAG_GHOST_OFFSET))
                        .w(px(TILE_DRAG_GHOST_SIZE.0))
                        .h(px(TILE_DRAG_GHOST_SIZE.1))
                        .border_2()
                        .border_color(cx.theme().primary)
                        .debug_selector(|| "tile-drag-ghost".to_string()),
                )
            })
            // Paint the command line at the focused tile's bottom edge, with
            // completions above. The render-time check guarantees that an open
            // line still belongs to that tile and owns input focus. A missing
            // rect suppresses painting if the tile disappeared.
            .when_some(
                self.command_line.as_ref().zip(focused_rect),
                |el, (line, rect)| {
                    el.child(commandline_view::render(
                        line,
                        &self.command_input,
                        rect,
                        rem_size,
                        cx,
                    ))
                },
            )
            // Paint the member list above its focused tile and below overlays.
            // Build row titles only while it is open; occupant titles are shared
            // strings, so each row clones a handle rather than formatting text.
            .when_some(
                self.stack_list.as_ref().zip(focused_rect),
                |el, (list, rect)| {
                    let rows: Vec<stacklist::Row> = list
                        .members
                        .iter()
                        .map(|id| match self.occupants.get(id) {
                            Some(o) => stacklist::Row {
                                title: o.content.title(cx),
                                kind: o.kind,
                            },
                            None => stacklist::Row {
                                title: "empty".into(),
                                kind: "placeholder",
                            },
                        })
                        .collect();
                    let weak = cx.entity().downgrade();
                    let members = list.members.clone();
                    let on_row_click = move |i: usize, window: &mut Window, cx: &mut App| {
                        let Some(id) = members.get(i).copied() else {
                            return;
                        };
                        let _ =
                            weak.update(cx, |view, cx| view.activate_stack_member(id, window, cx));
                    };
                    let panel = stacklist::render(list, &rows, rect, rem_size, on_row_click, cx);
                    el.child(
                        div()
                            .id("stack-list-click-catcher")
                            .absolute()
                            .left(px(0.))
                            .top(px(0.))
                            .w(px(width))
                            .h(px(viewport_height))
                            .debug_selector(|| "stack-list-click-catcher".to_string())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, _event, _window, cx| view.close_stack_list(cx)),
                            )
                            .child(panel),
                    )
                },
            )
            // The add-a-filter menu's click catcher: a press anywhere but
            // the menu closes it and goes no further. The menu panel itself
            // is deferred (painted above this) from inside the toolbar, so
            // its rows are hit first. `occlude` is what keeps the press
            // from also reaching the element beneath, the `+` included —
            // otherwise a click on the `+` would close and reopen the menu.
            // It also blocks the wheel for the tiles beneath while the menu
            // is open, which is accepted for a two-row transient menu.
            .when(self.add_filter_menu.is_some(), |el| {
                el.child(
                    div()
                        .id("scope-add-menu-click-catcher")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(width))
                        .h(px(viewport_height))
                        .debug_selector(|| "scope-add-menu-click-catcher".to_string())
                        .occlude()
                        // Every button: the catcher swallows every press, so a
                        // right or middle press must close the menu too.
                        .on_any_mouse_down(cx.listener(|view, _event, window, cx| {
                            view.dismiss_add_filter_menu(window, cx)
                        })),
                )
            })
            // Paint the modal below the palette: a palette opened over the stack
            // must be visible and take clicks above the dialog it covers.
            .when_some(modal, |el, (title, title_extra, build)| {
                let extra = title_extra.map(|f| f(self, cx));
                let show_back = dialog::back_available(self);
                let content = build(self, window, cx);
                el.child(dialog::render_modal(
                    title,
                    extra,
                    show_back,
                    content,
                    width,
                    viewport_height,
                    cx,
                ))
            })
            // The palette overlay paints above the tiles/status bar (later
            // children paint above earlier siblings) but below gpui-
            // component's own dialog/notification layers below.
            .when_some(self.palette.as_ref(), |el, state| {
                // Select the clicked row before calling the same commit path as
                // Enter. A clonable weak-entity callback can be shared by rows
                // without a separate heap allocation for each row on each render.
                let weak = cx.entity().downgrade();
                let on_row_click = move |idx: usize, window: &mut Window, cx: &mut App| {
                    let _ = weak.update(cx, |view, cx| {
                        if let Some(palette) = view.palette.as_mut() {
                            palette.set_selected(idx);
                        }
                        view.commit_selected(window, cx);
                        cx.notify();
                    });
                };
                let panel = palette::render(
                    state,
                    &self.palette_scroll,
                    &self.palette_input,
                    on_row_click,
                    palette::Viewport {
                        width,
                        height: viewport_height,
                        rem_size,
                    },
                    cx,
                );
                // Click-outside dismiss: a transparent (no dimming — the
                // palette is an overlay, not a modal) full-window click-
                // catcher behind the panel. Precedent: `dialog::
                // render_modal`'s own backdrop, minus the `.bg(overlay)`
                // dimming a real modal wants and this doesn't. The panel
                // itself stops propagation on its own `on_mouse_down` (see
                // `palette::render`'s doc comment), so a click landing
                // anywhere inside it — a row, the query input, empty space
                // — never also reaches this catcher's handler below.
                //
                // gpui fires `on_mouse_down` for every hovered hitbox, not
                // only the topmost, so without `occlude()` a click over an
                // open dialog stack would also reach whatever the catcher
                // covers: `shell-modal-backdrop` (popping the dialog) or a
                // dialog row (committing or opening it). `occlude()` is
                // conditioned on a dialog being open because with none the
                // catcher covers only tiles, which have no click handler this
                // would wrongly swallow.
                el.child(
                    div()
                        .id("palette-click-catcher")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(width))
                        .h(px(viewport_height))
                        .debug_selector(|| "palette-click-catcher".to_string())
                        .when(self.modal_open(), |d| d.occlude())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _event, window, cx| {
                                view.close_palette(window, cx);
                            }),
                        )
                        .child(panel),
                )
            })
            // Painted after (so above) the modal and the palette. Never
            // `Some` in the same frame as a modal: while the modal is open,
            // `self.matcher` can never go pending at all — `open_shell_dialog`
            // cancels it on open, and `handle_key_down`'s modal branch
            // returns before ever reaching `self.matcher.press` for as long
            // as `self.modals` stays non-empty, so `which_key_continuations`
            // (computed from `self.matcher.pending()`, just above) is always
            // `None` whenever `modal` is `Some`. Opening the palette cancels
            // the matcher too.
            .when_some(which_key_continuations, |el, continuations| {
                el.child(whichkey::render(
                    &continuations,
                    self.matcher.count(),
                    registry,
                    width,
                    status_height,
                    rem_size,
                    cx,
                ))
            })
            // The frame-time readout (`perf::toggle_overlay`),
            // painted above every other shell layer — a diagnostic that
            // must stay visible while the palette/modal/which-key it might
            // be measuring are up. Top-right, clear of the which-key panel
            // (bottom-right) and the status bar. No handlers, no timer:
            // it repaints only when something else invalidates the window,
            // showing values as-of the last invalidation (see
            // `perf_overlay`'s module doc for why that's deliberate).
            .when(self.perf_overlay, |el| {
                el.child(perf_overlay::render(
                    &self.perf,
                    &self.frame.read(cx).requery,
                    toolbar_height,
                    rem_size,
                    cx,
                ))
            })
            // Root delegates overlay painting to its child view. Paint the
            // component dialog and notification layers after shell content.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
