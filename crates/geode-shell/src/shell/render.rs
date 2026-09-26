//! `ShellView`'s `Render` implementation (spec §3): chrome (toolbar, sidebar,
//! status bar), the tiling tree, dividers, drag ghosts, palette and dialogs.
//! Split out of `shell/mod.rs` (Phase 3c Task 0) because the one `render`
//! function was ~1,190 lines, far from the pure cores in `input.rs`,
//! `commandline_ctl.rs`, `palette_ctl.rs`, `drag.rs` and `occupants.rs` that
//! it calls into every frame.

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
    ShellView, asof_view, choicedialog, commandline_view, dialog, objectdialog, perf_overlay,
    picker, scope_expr_view, sidebar, stacklist, status, toolbar, whichkey,
};

/// gpui hover-group name shared by every divider strip (drag-splitters
/// task): the strip is the group, its inner 2px line is the member that
/// tints on `group_hover`. One shared name is correct — gpui resolves a
/// `group_hover` against the *innermost enclosing* group's bounds during
/// paint, so each strip's line only lights for its own strip (precedent:
/// gpui-component's `ResizeHandle` shares the name "handle" across every
/// handle the same way).
const DIVIDER_GROUP: &str = "divider-strip";

/// Height, in pixels, of the as-of warning stripe (Phase 4a §3.6) painted
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
        // Frame-time instrumentation (spec §7.4), first thing so the
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

        // Fix-round finding: consume a pending focus restore left by a
        // background path that closed the palette with no `Window` in hand
        // (see `pending_focus_restore`'s and `apply_reload`'s own doc
        // comments for the orphaned-`FocusId` bug this closes). `render`
        // is the first point downstream that actually has a `&mut Window`
        // — and the *only* point that will reliably run at all in the
        // failure state being fixed, since an orphaned focus is exactly
        // what makes `handle_key_down` stop firing until a mouse click
        // claims focus elsewhere first. Kept ahead of everything else in
        // render, per its contract.
        //
        // SKIPPED — flag cleared, focus left exactly where it is — while a
        // tile's occupant ITSELF holds the focused handle in insert mode
        // (`occupant_holds_insert_focus`, occupants.rs: the focused tile's
        // occupant answering `TileContent::holds_focus` for the focused
        // handle — its own input, never a shell surface — AND its context
        // stack carrying `mode == insert`; user ruling 2026-09-17,
        // reversing the 2026-09-14 "editing is keyboard-only" ruling —
        // the ownership half is review C-1's, see the predicate's doc).
        // Every tile mouse-down re-arms this flag, so without the skip an
        // editor a module opened from a double-click would lose the
        // keyboard on the very next frame. A module in insert mode owns
        // the keyboard, and the shell's chords still dispatch from there
        // through `handle_key_down`'s insert branch — the same predicate —
        // so nothing goes dead. The orphaned-focus case this flag was born
        // for has no occupant claiming `insert`, and the workspace-switch
        // case lives in `ensure_occupants`, so both stay covered.
        if self.pending_focus_restore {
            self.pending_focus_restore = false;
            if !self.occupant_holds_insert_focus(window, cx) {
                self.focus_handle.focus(window, cx);
            }
        }

        // The safety net under that flag: a window with NOTHING focused
        // sends keys nowhere at all, so every shell chord is dead until a
        // mouse click claims focus somewhere.
        //
        // Read the scope narrowly (review finding, Important 1). This
        // covers exactly one thing: a focused `FocusHandle` that was
        // actually DROPPED — every clone of it gone, so `Window::focused`
        // (which resolves the stored id back through the focus map and
        // refuses a zero-refcount entry — pinned rev's
        // `FocusHandle::for_id`) reports `None`. In this crate that means
        // a focus-tracking tile view destroyed while focused, i.e. its
        // tile CLOSED. It does NOT cover a workspace switch: occupants
        // are retained for tiles in every workspace (`fill_all_tiles`),
        // so a switched-away view is unmounted but very much alive,
        // focus still `Some`, and this branch never fires. That case is
        // handled where it is actually visible — `ensure_occupants`'s
        // departed-tile backstop, just below — and the two are
        // complementary, not redundant.
        //
        // The condition is EXACTLY `is_none()`, deliberately: taking
        // focus is only unambiguously right when nobody has it. Anything
        // broader — "focus isn't the shell root", say — would yank the
        // caret out of every live focused element this app has (the
        // palette's filter, a dialog's `Input`, the scope bar's text
        // field, a per-tile command line, a focus-tracking occupant that
        // is still mounted), on every frame, mid-type. `the_focus_net_
        // leaves_a_live_focused_input_alone` guards that half.
        if window.focused(cx).is_none() {
            self.focus_handle.focus(window, cx);
        }

        // The transient stack-member list's own staleness check (tile-
        // stacks spec §5.2), the same shape as the command-line one just
        // below: a list stays open only while its tile IS the workspace's
        // own focused tile AND still a stack member — a workspace switch,
        // a directional focus move that leaves the stack, or the stack
        // collapsing out from under it (its last other member unstacked)
        // all close it here rather than leaving stale chrome painted over
        // whatever tile now has focus. Ahead of `ensure_occupants` (not
        // beside the command-line check below, which needs the rem/
        // surface arithmetic that follows it): nothing this reads depends
        // on this render's own occupant reconciliation.
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

        // Create occupants for any tile that lacks one, drop occupants
        // whose tile is gone, and tell occupants when they enter/leave the
        // screen (Phase 3 §3.2) — the one place every path that can change
        // the tile set (split, close, restore, workspace switch) reliably
        // funnels through with a `Window` in hand.
        self.ensure_occupants(window, cx);

        // Cancel an in-flight divider drag when the surface it was
        // resizing is no longer the one on screen (drag-splitters task):
        // the palette or a modal opened mid-drag (keyboard stays live
        // during a drag — ctrl+k works with the button held), a tree tile
        // went fullscreen (mod+f likewise), or mod+N switched to another
        // workspace (review fix: the recorded address and bounds belong to
        // the workspace the drag started in; without this, the next move
        // would apply them to the new workspace's tree). All of these hide
        // the dragged boundary, and letting the drag keep mutating an
        // invisible layout would be a surprise on return. Cancel means
        // "stop tracking the mouse", NOT "undo": whatever the drag already
        // applied stays applied and — review fix — still persists like
        // any resize (`cancel_divider_drag` dirties the session when the
        // drag had moved; the first cut silently dropped that, leaving a
        // visible layout the next restore wouldn't reproduce). Same
        // consume-state-at-the-top-of-render precedent as
        // `pending_focus_restore` just above: render is the one place
        // every one of those paths reliably funnels through with the
        // state fresh.
        if self.divider_drag.as_ref().is_some_and(|drag| {
            self.palette.is_some()
                || self.modal.is_some()
                // Epoch, not index (finding 7) — see `DividerDrag::epoch`.
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

        // Cancel an in-flight tile drag on the same conditions (tile-drag
        // task) — palette/modal opened, workspace switched, fullscreen
        // toggled — PLUS a which-key hint appearing: unlike a divider
        // drag (which keeps its already-applied resize live behind the
        // handler-less hint), a tile drag is all about *choosing a drop
        // target among the tiles*, and doing that under a panel that
        // covers part of them would be blind targeting. PLUS (post-merge
        // review BUG 3) the dragged tile no longer existing anywhere:
        // ctrl+w can close it mid-drag (the keyboard stays hot), and
        // without this check the ghost and zone highlight kept painting
        // — promising a drop the verbs would silently refuse. All of
        // these make cancelling truly free here: a tile drag applies
        // nothing until its drop, so cancel undoes nothing, dirties
        // nothing, and needs none of the divider guard's `moved`
        // bookkeeping.
        if self.tile_drag.as_ref().is_some_and(|drag| {
            self.palette.is_some()
                || self.modal.is_some()
                || !self.matcher.pending().is_empty()
                // Epoch, not index (finding 7) — see `DividerDrag::epoch`.
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

        // The per-tile command line's own invariant (I1, final review):
        // "the focused tile IS `command_line.tile`, for as long as the
        // line is open" holds only while BOTH (a) the active workspace's
        // own focused tile is still that tile, and (b) `command_input`
        // still holds keyboard focus. Both tile mouse-down handlers
        // (`render`, below) already call `leave_command_line` before
        // acting, and that remains the fast, explicit path for the two
        // surfaces that need it — this is the generic backstop for
        // everything else that can change (a) or (b) without going
        // through one of those call sites: the sidebar's workspace-switch
        // mouse-down (`sidebar::sidebar`) changes (a) — a plain, non-
        // focusable `div`, so keyboard focus never moves — and a click
        // into the toolbar's `filter_input` changes (b) — a real `Input`
        // that takes focus on click, with no workspace-state change at
        // all. One check here, at the top of render (same "reliably
        // funnels through with fresh state" precedent as `pending_focus_
        // restore` above), covers both without a third and fourth
        // `leave_command_line` call site — and is a no-op on the tile-
        // mouse-down paths, since they already set `command_line` to
        // `None` before this runs, so there is no double cancel (no
        // second `FindEvent::Cancelled`/`Committed`). This leaves
        // (commits a find, cancels a command — `leave_command_line`,
        // spec §20.4) rather than unconditionally cancelling: the
        // sidebar switch and the filter-input click are both a click
        // away from the tile, exactly like the tile mouse-down handlers
        // below, not `escape` — a `/` line with text still commits here.
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

        // `viewport_size` is the drawable area (excludes window chrome),
        // which is what `Tree::layout` should partition (gpui/window.rs).
        // Task 4 adds a top toolbar (the native title bar,
        // `TITLE_BAR_HEIGHT`) and a left sidebar (`sidebar::WIDTH`) on top
        // of the existing bottom status bar (`status::HEIGHT`); the tile
        // area gets the viewport minus all three, and `Tree::layout` is
        // still called exactly once, over those shrunk bounds.
        //
        // Phase 4a §3.6/§4.5: while the frame is scoped to a past instant,
        // a 3px warning stripe (`AS_OF_STRIPE_HEIGHT`) sits directly under
        // the toolbar, spanning the window — the part of the historical
        // indicator that survives a maximised tile hiding the toolbar's
        // own AS OF badge and the status bar's own segment. Its height
        // comes out of `content_height` here, the one place every other
        // chrome element's height already does, so the tile area never
        // overflows underneath it.
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

        // One layout pass for the whole surface (dock-regions task,
        // generalized by dock-trees): the pure `tiling::dock_layout`
        // carves the visible docks' pixel rects out of the content area,
        // `Tree::layout` partitions what's left for the main tree, and
        // each visible dock's rect feeds that dock's own `Tree::layout` —
        // one geometry call per region (main + up to three visible docks),
        // still a single pass overall. While a tree tile is fullscreen it
        // covers the entire surface and the docks are not painted at all
        // (the docks keep their state; they're just not part of the
        // fullscreen picture).
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
        // Divider strips are painted (and their listeners armed) only
        // while no overlay is up: the palette's click-catcher and the
        // modal's backdrop both cover the whole window ABOVE the strips
        // but without occluding them, so a live strip underneath would
        // still take the same mouse-down that dismisses the overlay and
        // start a drag from under it. The which-key hint (review fix) is
        // the same leak in miniature: `whichkey::render` paints a solid
        // panel with no `.occlude()` and no handlers, so a mouse-down
        // inside its bounds would fall straight through to a strip
        // beneath — it shows exactly while a keystroke sequence is
        // pending, so that state gates too. (An already-in-flight drag is
        // NOT cancelled for which-key the way palette/modal cancel it:
        // the hint has no mouse handlers to fight the drag catcher, and
        // the dragged boundary stays visible behind it.) Gating at paint
        // time keeps the rule simple: strips exist exactly when the tiles
        // they resize are the frontmost interactive surface.
        let dividers_active =
            self.palette.is_none() && self.modal.is_none() && self.matcher.pending().is_empty();
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
            // just computed (drag-splitters task): the main tree's
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

        // The active drop target's zone highlight (tile-drag task): the
        // SAME resolution core the drop itself uses
        // (`tiling::resolve_drop_target` — post-merge review cleanup 8:
        // the first cut restated the dock-frame → dock-tile →
        // dock-background → tree-tile order here by hand, and two copies
        // of a targeting rule is how a highlight drifts from the drop it
        // promises), fed the rects the single layout pass above just
        // produced — no second layout, no I/O, and no allocation (the
        // core takes a borrowed iterator), per the render-discipline
        // constraint. An edge zone highlights the half of the target tile
        // the insert would occupy, center the whole tile, a dock
        // background the dock's frame. What stays HERE, on top of the
        // core, is the Workspace-side no-op filtering — targets whose
        // drop the verbs would refuse paint nothing, because
        // highlighting them would promise a rearrangement that won't
        // happen: the dragged tile itself (self-drops are recorded
        // no-ops for every zone), and the background of the dock the
        // tile already lives in (defensive — an occupied dock's tiles
        // cover its whole frame, so this is unreachable in practice).
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
        // (dock-trees task — a dock holds many tiles, one ring).
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
        // for the command line strip (§3.4): it paints along that tile's
        // bottom edge, not the surface's, so it must follow focus into a
        // dock exactly like the focused ring does. Captured from the same
        // per-tile `is_focused` computed in the loops below rather than a
        // second lookup — `region`/`focused`/`dock_cells` already say
        // which tile, if any, is the one ring shows.
        let mut focused_rect: Option<Rect> = None;
        if rects.is_empty() {
            // The empty hint fills the *tree's* remaining area (not the
            // whole surface — visible docks keep their columns), so it is
            // its own absolutely-positioned, internally-centered child
            // rather than turning the surface itself into a flex row.
            //
            // One hint whatever holds focus (spec 2026-09-08 add-tile
            // §7.3): the palette adds a tile into the focused region, so
            // the advice reads the same from the main tree and from a
            // focused dock — the old state-aware "return" variants, which
            // existed because the split chords could not fill this area
            // from a dock, collapse into it. One selector, one verb; a
            // dock-focused empty tree only appends where the add would
            // actually land (final-review Ruling J), because the hint
            // paints over the *tree's* area and would otherwise read as
            // an offer to fill the space the reader is looking at.
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
                    // opens the tile picker (2026-09-19), the mouse form
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
                        // (tile-drag task) — and deliberately does NOT
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
                                // Any tile mouse-down leaves an open
                                // command line first, unconditionally
                                // (fix round 1, finding 2, and spec
                                // §20.4 — `leave_command_line`'s own doc
                                // comment): this is the fast, explicit
                                // path for the one focus-stealing surface
                                // that is itself a tile click. It leaves
                                // (commits a find, cancels a command —
                                // `leave_command_line`) rather than
                                // unconditionally cancelling, so clicking
                                // away from a `/` line with text commits
                                // it. The render-time check (I1, final
                                // review — see the comment just above
                                // `ensure_occupants`'s drag-cancel
                                // neighbours) is the generic backstop
                                // that also covers surfaces that are NOT
                                // a tile mouse-down (the sidebar's
                                // workspace switch, the toolbar's filter
                                // field) — this call just means a tile
                                // click doesn't wait a frame for that
                                // backstop to notice. Together they keep
                                // "the focused tile IS the line's tile,
                                // whenever it's open" an invariant — see
                                // the comment where the strip is painted,
                                // below.
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
                                // An occupant may track its own
                                // `FocusHandle` (the recording module does
                                // — `RecordingView` — and `DataTable` will
                                // too) and take focus with this
                                // mouse-down; without re-arming the same
                                // restore `apply_reload` uses, the shell
                                // root would never get focus back and
                                // every shell chord would go dead (§3.3).
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

        // The docks, painted with the identical chrome (dock-trees task:
        // each visible dock lays its own tree's tiles into its frame —
        // same `tile_cell`, click-to-focus included; a click focuses that
        // tile *within* the dock's tree AND moves the region there). A
        // visible but empty dock renders a centered muted hint naming the
        // palette (a dock takes focus when it is shown, so `ctrl+k` adds
        // into it — spec 2026-09-08 add-tile §7.3/§8) and the *physical*
        // keys that would move an existing tile into it (the user presses
        // ctrl+shift+[ even though the binding is spelled `ctrl+{` — see
        // BUILTIN_KEYMAP's doc comment).
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
                                    // Same command-line leave as the tree-tile
                                    // listener above (commits a find, cancels a
                                    // command — `leave_command_line`), and for
                                    // the identical reason (fix round 1, finding
                                    // 2, and spec §20.4).
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
                                    // Docked tiles get real occupants too (a
                                    // focus-tracking occupant like the recording
                                    // module can steal focus on this same
                                    // mouse-down) — re-arm the identical restore
                                    // the tree-tile listener above uses (§3.3).
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
                        // A click focuses the empty dock as the region,
                        // a double-click opens the tile picker into it
                        // (`on_empty_dock_mouse_down`, user ruling
                        // 2026-09-19).
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

        // The divider strips (drag-splitters task), painted after — so
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

        // The zone highlight (tile-drag task), painted after — so above —
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
        // Computed once per frame (§4.4) rather than read field-by-field
        // from inside `toolbar::toolbar` — the frame is an entity, and this
        // is the one place `render` already has `cx` in hand to read it.
        // Moved ahead of `status_bar`'s own construction (Phase 4a §3.6):
        // its `as_of` segment reads `bar_model.as_of`, the same formatted
        // text the toolbar's own AS OF badge shows. `self.today` (Phase 4b
        // Task 1 fix round 1, MIN-9), not a fresh clock read every paint —
        // the date read moved to the ~500ms reload-poll tick, so a held key
        // no longer pays it on every repaint. `self.clock(cx)` (as-of
        // dialog spec §6.1) is the `AppClock` global.
        let bar_model = self.frame.read(cx).bar_model(self.clock(cx), self.today);
        // Phase 4b §4.4: the status bar's diagnostics indicator now reads
        // `Diagnostics::summary()` (cached there, keyed on its own
        // version, and returning an `Rc<str>` — Task 4 fix round 1,
        // MAJ-1 — so a cache hit on this render-path call clones a
        // refcount, never a buffer) rather than the deleted `data_status`
        // field — an empty summary means nothing to report, same
        // "`None` clears it" contract `data_status` had.
        let diagnostics_read = self.diagnostics.read(cx);
        let diagnostics_summary = diagnostics_read.summary();
        // The ingest activity, read alongside the summary (spec
        // 2026-09-19 §5.3) — borrowed straight through to the
        // `status_bar` call below rather than cloned: `diagnostics_read`
        // borrows `cx` (not `self`), and nothing between here and that
        // call needs `cx` mutably, so `status_bar` gets `Option<&
        // IngestActivity>` with no per-render allocation at all.
        let ingest = diagnostics_read.ingest.as_ref();
        // Clicking the summary opens the diagnostics tile (Phase 4b Task
        // 5), same `cx.entity()`-captured-into-a-closure shape as
        // `on_chip_close`/`on_chip_open` just below.
        let diagnostics_click_entity = cx.entity();
        let on_diagnostics_click = move |window: &mut Window, cx: &mut App| {
            diagnostics_click_entity.update(cx, |view, cx| {
                view.open_module("diagnostics", window, cx);
            });
        };
        let status_bar = status::status_bar(
            self.matcher.pending(),
            self.matcher.count(),
            reload_message.as_deref(),
            self.config_write_error.as_deref(),
            self.restart_required.as_deref(),
            self.notice,
            (!diagnostics_summary.is_empty()).then_some(diagnostics_summary.as_ref()),
            on_diagnostics_click,
            ingest,
            bar_model.as_of.as_deref(),
            bar_model.as_of_full.as_ref(),
            self.services.theme.active_name(),
            cx,
        );
        let sidebar = sidebar::sidebar(active_index, &non_empty, cx);
        // A chip's close glyph drops that dimension from the scope
        // (spec §3.1) — an undoable edit, same door as every other scope
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
        // The chip body opens the dimension picker on that column (Phase
        // 4a §3.3) — `picker::open` needs `&mut ShellView`, so this reaches
        // it the same way `on_chip_close` reaches `frame.update` above: a
        // fresh `cx.entity()` handle, updated inside the closure.
        let chip_open_entity = cx.entity();
        let on_chip_open = move |column: &str, window: &mut Window, cx: &mut App| {
            let column = column.to_string();
            chip_open_entity.update(cx, |view, cx| {
                picker::open(view, Some(column), window, cx);
            });
        };
        // The scope bar's `+` pick glyph (scope-save spec's amendment) —
        // the mouse form of `mod+p`, opened on the column-choice stage
        // exactly as `frame::pick` is. Same `cx.entity()`-captured shape
        // as `on_chip_open` just above.
        let pick_chip_entity = cx.entity();
        let on_pick = move |window: &mut Window, cx: &mut App| {
            pick_chip_entity.update(cx, |view, cx| {
                picker::open(view, None, window, cx);
            });
        };
        // The scope bar's save glyph — the mouse form of
        // `scope::save_current`, through the same door `input.rs`'s
        // dispatch arm uses.
        let save_chip_entity = cx.entity();
        let on_save = move |window: &mut Window, cx: &mut App| {
            save_chip_entity.update(cx, |view, cx| {
                objectdialog::render::open_save_scope(view, window, cx);
            });
        };
        // The grouping readout's click (2026-09-19) — the mouse form of
        // `frame::grouping`/`mod+g`, through the same door `input.rs`'s
        // dispatch arm uses.
        let grouping_entity = cx.entity();
        let on_grouping = move |window: &mut Window, cx: &mut App| {
            grouping_entity.update(cx, |view, cx| {
                choicedialog::open_grouping(view, window, cx);
            });
        };
        // The AS OF chip's click (toolbar restyle 2026-09-19) — the mouse
        // form of `frame::as_of`/`mod+t`, through the same door `input.rs`'s
        // dispatch arm uses.
        let as_of_entity = cx.entity();
        let on_as_of = move |window: &mut Window, cx: &mut App| {
            as_of_entity.update(cx, |view, cx| {
                asof_view::open(view, window, cx);
            });
        };
        // The expression chip's click (command-line locality 2026-09-20)
        // — the mouse form of `frame::scope_expression`.
        let expr_entity = cx.entity();
        let on_expr = move |window: &mut Window, cx: &mut App| {
            expr_entity.update(cx, |view, cx| {
                scope_expr_view::open(view, window, cx);
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
        let toolbar = toolbar::toolbar(
            &self.filter_input,
            &bar_model,
            grouping_open,
            on_chip_close,
            on_chip_open,
            on_pick,
            on_save,
            on_grouping,
            on_as_of,
            on_expr,
            cx,
        );

        let body = h_flex()
            .w_full()
            .h(px(content_height))
            .flex_none()
            .child(sidebar)
            .child(surface);

        let width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);

        // Which-key hint (Task 8): only computed while a sequence is
        // actually pending — `continuations` over an empty `pending` would
        // be well-defined (every binding "strictly extends" it) but the
        // overlay has nothing to say when no sequence is in flight, so it
        // must not appear then. Display-only: this reads `self.matcher`
        // without touching it, so it can never affect what `handle_key_down`
        // does with the next keystroke.
        //
        // M10 (3b final review): this same gate also hides `whichkey::
        // render`'s count row for a *bare* count (e.g. `4` with no chord
        // typed yet), since a bare count leaves `pending` empty. Spec §3.3
        // describes the overlay appearing "after a held prefix"; the
        // reading chosen here is that a bare count alone is not yet a held
        // prefix — nothing is "held" until a key extends it into an actual
        // sequence — so the overlay stays hidden and the status bar (a
        // separate reading: it always has a count to show, held-prefix or
        // not) is where a bare count surfaces instead.
        let pending = self.matcher.pending();
        let which_key_continuations = (!pending.is_empty()).then(|| {
            whichkey::continuations(&self.services.keymap, pending, &self.context_stack(cx))
        });
        let registry = &self.services.registry;

        // Task 9 instant-modal redesign: clone the cheap pieces (`title`:
        // `SharedString`, `title_extra`: `Option<Rc<dyn Fn>>`, `build`:
        // `Rc<dyn Fn>`) out of `self.modal` up front — the same "extract
        // into a local ahead of the render chain" move `pending`/
        // `registry` just above already make, one field further in. See
        // `ShellModal`'s own doc comment for why this step is required
        // rather than just reading `self.modal.as_ref()` inline inside the
        // `when_some` below.
        let modal = self.modal.as_ref().map(|modal| {
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
            // Two identifiers in one `KeyContext` (gpui's
            // `KeyContext::parse` takes whitespace-separated ones):
            //
            // `GeodeShell` is carried ALWAYS — the shell root's own name
            // on the dispatch stack, and `init_reclaimed_keybindings`'
            // last bullet binds `tab`/`shift-tab` to `NoAction` on it.
            // gpui-component's `Root` binds both keys window-wide to its
            // own focus cycling and gpui dispatches a matched binding
            // BEFORE any `on_key_down` listener, so without this reclaim a
            // bare `tab` never reaches `handle_key_down` at all while a
            // tile is focused — a module binding `tab` in its own context
            // (the timeseries tile's `tab = timeseries::next`, timeseries
            // spec §9.4) was dead in the real app. Permanent rather than
            // conditional: inside the shell it is the shell's keymap that
            // owns the key, in every context.
            //
            // `GeodeModalOpen` is added on top while a modal is up: "a
            // Geode modal is open", carried by the shell ROOT rather than
            // the modal panel — the one context that is on the dispatch
            // stack in normal mode, where focus is this very handle and
            // the panel's own `"GeodeModal"` context is therefore absent.
            // Kept as its own identifier rather than folded into the
            // unconditional one, since `init_reclaimed_keybindings` scopes
            // more than `tab` by it and the two answers must stay
            // separable.
            .key_context(if self.modal.is_some() {
                "GeodeShell GeodeModalOpen"
            } else {
                "GeodeShell"
            })
            .on_key_down(cx.listener(Self::handle_key_down))
            // Root-level left-release fallback (post-merge review BUG 1,
            // extended to divider drags by the fix-round should-fix):
            // ends a drag of either kind whose release landed in the
            // arm-to-first-paint gap, before its catcher's own up
            // handlers exist — see `heal_drags_on_root_release`'s doc
            // comment for the full mechanism, the four modality×state
            // combinations, and why BOTH listeners are needed
            // (`on_mouse_up` is bubble-phase and hover-gated,
            // `on_mouse_up_out` capture-phase and NOT-hovered-gated;
            // input modality and occluding overlays flip which one
            // fires, and together they cover every left release).
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
            .child(toolbar)
            // The as-of warning stripe (Phase 4a §3.6/§4.5): 3px, spanning
            // the window, directly under the toolbar — paints if and only
            // if the frame's as-of is `At`, never otherwise (spec §4.5:
            // nothing on screen may look live when it is not). Its height
            // is already subtracted from `content_height` above, so `body`
            // never overflows underneath it.
            .when(is_historical, |el| {
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
            // The divider drag catcher (drag-splitters task): while a drag
            // is active, a transparent full-window layer above the tiles
            // and status bar (but below the palette/modal overlays, which
            // cancel drags anyway — see the guard at the top of `render`)
            // owns every mouse-move and mouse-up until the button is
            // released. This is the capture mechanism: a fast drag leaves
            // the thin strip immediately, and gpui's element-level
            // `on_mouse_move` is hover-gated — but this layer's hitbox IS
            // the whole window, so hover-gating is satisfied wherever the
            // cursor goes (the div-composition equivalent of the
            // window-level `window.on_mouse_event` listeners Zed's own
            // pane-resize custom element registers in paint). `.occlude()`
            // also suppresses tile hover/click behavior for the drag's
            // duration, and the layer carries the axis resize cursor so
            // the pointer keeps its col/row-resize shape even while it's
            // off the strip — the same effect as Zed's
            // `set_window_cursor_style` during a handle drag. Mouse-up out
            // of the window (capture-phase `on_mouse_up_out`) and a move
            // arriving with the button no longer pressed (a missed
            // release) both end the drag too, so it can never get stuck.
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
                            // Post-merge review BUG 4 (platform-uniform
                            // rule, shared with the tile catcher below —
                            // see its comment for the verified platform
                            // evidence): only a BUTTONLESS move is the
                            // lost-release finish; a move reporting a
                            // non-Left button is a chorded second button
                            // and is ignored entirely.
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
            // The tile-drag catcher (tile-drag task): the same full-window
            // capture mechanism as the divider catcher above — while a
            // drag is armed or active, a transparent occluding layer owns
            // every mouse-move and the release, wherever the cursor goes.
            // Its `.occlude()` is also what keeps the divider strips (and
            // tile click-to-focus, and strip hover styling) from fighting
            // an in-flight tile drag: the catcher is painted after the
            // whole tile surface, so everything under it leaves the hover
            // chain for the drag's duration. The two catchers can never
            // coexist — each drag kind's mouse-down is unreachable while
            // the other's catcher occludes the window (and
            // `try_arm_tile_drag` checks anyway). A move arriving with
            // the button no longer pressed cancels with nothing applied
            // — see `TileDrag`'s doc for why that differs from the
            // divider catcher's finish. `on_mouse_up_out` routes through
            // the DROP, not a cancel (review blocker fix): gpui's
            // input-modality hover suppression means it fires for a
            // perfectly ordinary in-window release whenever a keystroke
            // was the last input — a KeyDown sets the window's
            // `last_input_modality` to Keyboard (pinned window.rs,
            // `dispatch_event`), a MouseUp does NOT reset it, and
            // `HitboxId::is_hovered` returns false under keyboard
            // modality, which flips the hovered-gated `on_mouse_up` off
            // and the `!is_hovered`-gated `on_mouse_up_out` on. The
            // keyboard is documented hot mid-drag, so "press any key,
            // release without moving" is a real user path and must drop,
            // not silently cancel. A genuinely outside-window release
            // still applies nothing through this route:
            // `locate_drop_target` has no target at a position outside
            // every tile and dock, and a no-target drop is a no-op.
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
                            // Post-merge review BUG 4 (recorded decision +
                            // platform evidence): the old
                            // `pressed_button != Some(Left)` test made
                            // this catcher platform-divergent. macOS
                            // translates NSRightMouseDragged /
                            // NSOtherMouseDragged into MouseMoveEvents
                            // whose `pressed_button` is that button
                            // (`gpui_macos/src/events.rs`, the
                            // `*MouseDragged` arm — buttonNumber mapped
                            // verbatim, no left-first normalization), so
                            // pressing a second button mid-drag CANCELLED
                            // here; Windows' WM_MOUSEMOVE translation
                            // (`gpui_windows/src/events.rs`,
                            // `handle_mouse_move_msg`) checks MK_LBUTTON
                            // first, so the same chord SURVIVED there.
                            // Unified rule: only a BUTTONLESS move is the
                            // lost-release cancel (every platform reports
                            // `None` once all buttons are up); a move
                            // reporting a non-Left button is a chorded
                            // second press and is IGNORED — it neither
                            // advances the drag (its position belongs to
                            // another button's stream) nor cancels it.
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
            // The drag ghost (tile-drag task): a lightweight fixed-size
            // outline following the cursor — deliberately NOT a copy of
            // the tile content (see TILE_DRAG_GHOST_SIZE's recorded
            // choice). Painted above the catcher; a plain div with no
            // handlers and no `.occlude()`, so it can never swallow the
            // catcher's events even when the cursor overlaps it.
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
            // The per-tile command line (§3.4): a one-line strip along
            // the focused tile's bottom edge, plus a completions popup
            // above it. Painted at `focused_rect` — the WORKSPACE's own
            // notion of the focused tile, computed in the loops above —
            // rather than by looking up `self.command_line.tile`'s own
            // rect. That is only correct because "the focused tile IS
            // `command_line.tile`, for as long as the line is open" is an
            // invariant, not a coincidence — but (I1, final review) a
            // tile mouse-down is NOT the only other way it could move:
            // keyboard input can't (the command-line branch in
            // `handle_key_down` intercepts every key ahead of the matcher
            // and every workspace action), and both tile mouse-down
            // handlers (tree and dock, above) cancel an open command line
            // before acting on their click — but the sidebar's
            // workspace-switch mouse-down changes the active workspace's
            // own focused tile without touching either of those, and a
            // click into the toolbar's `filter_input` steals keyboard
            // focus without touching the workspace at all. The generic
            // check at the top of render (see its own doc comment, just
            // above `ensure_occupants`'s drag-cancel neighbours) closes
            // both, so by the time this runs `command_line` is `None`
            // whenever the tile it named is no longer the workspace's
            // focused one. Painted only while both a line is open AND
            // that tile still has a rect this frame (a tile can close
            // out from under an open line — `ensure_occupants` above
            // already drops the occupant; `handle_command_line_key`'s own
            // `None` arms handle the same race for dispatch, this is
            // render's twin).
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
            // The transient stack-member list (tile-stacks spec §5.2):
            // painted from the focused tile's own rect, above the tile
            // surface and the command line but below the palette — the
            // two are never open at once (the palette's open arm closes
            // the list first), but this ordering is what would govern it
            // if that ever changed. `rows` is prepared fresh per frame
            // only while the list is open, at most nine `SharedString`
            // clones (`TileContent::title`) — accepted for now, per the
            // task brief; Task 9 caches the blotter's own title instead
            // of formatting it here every frame.
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
            // The palette overlay paints above the tiles/status bar (later
            // children paint above earlier siblings) but below gpui-
            // component's own dialog/notification layers below.
            .when_some(self.palette.as_ref(), |el, state| {
                // Row click -> select, then commit — the mouse form of
                // `enter` (user request 2026-09-12; the palette's half of
                // the interaction model's §17.1 rule 2). The select comes
                // first so `commit_selected` — the ONE door the key path
                // also goes through (`palette_ctl`) — reads the clicked
                // row as the highlighted one; a click never dispatches by
                // any other route, so the two cannot drift. A small
                // `Clone`-able closure over a `WeakEntity<Self>`, not
                // `cx.listener` directly (its returned `impl Fn` isn't
                // itself `Clone`, and `palette::render` clones this once
                // per row to close over each row's own index — see that
                // function's doc comment) — this way building it costs one
                // stack closure, not a heap allocation per row, per frame,
                // while the palette is open (PHILOSOPHY.md: "per-frame heap
                // churn is a defect").
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
                el.child(
                    div()
                        .id("palette-click-catcher")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(width))
                        .h(px(viewport_height))
                        .debug_selector(|| "palette-click-catcher".to_string())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _event, window, cx| {
                                view.close_palette(window, cx);
                            }),
                        )
                        .child(panel),
                )
            })
            // The modal overlay paints above the palette (later children
            // paint above earlier siblings) but still below gpui-
            // component's own dialog/notification layers below — Task 9
            // instant-modal redesign, see `dialog`'s module doc. `palette`
            // is always `None` by the time `modal` is `Some`
            // (`open_shell_dialog` closes it on open), so this and the
            // block above never both add a child in the same frame, but
            // the ordering here is what would govern it if that ever
            // changed.
            .when_some(modal, |el, (title, title_extra, build)| {
                let extra = title_extra.map(|f| f(self, cx));
                let content = build(self, window, cx);
                el.child(dialog::render_modal(
                    title,
                    extra,
                    content,
                    width,
                    viewport_height,
                    cx,
                ))
            })
            // Painted after (so above) the modal for the same reason as the
            // modal-vs-palette ordering above: never both `Some` in the same
            // frame, but the ordering here is what would govern it if that
            // ever changed. This one, though, is a real invariant rather
            // than an incidental one — while the modal is open, `self.
            // matcher` can never go pending at all: `open_shell_dialog`
            // cancels it on open, and `handle_key_down`'s modal branch
            // returns before ever reaching `self.matcher.press` for as long
            // as `self.modal` stays `Some`, so `which_key_continuations`
            // (computed from `self.matcher.pending()`, just above) is always
            // `None` whenever `modal` is `Some`.
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
            // The frame-time readout (spec §7.4, `perf::toggle_overlay`),
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
            // ShellView is the first-level view Root wraps; Root's own
            // Render impl does not paint these overlay layers itself, so
            // whoever it wraps must (spec: gpui-component usage.md "Overlay
            // Layers"). Task 6's palette/dialogs need this in place now.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
