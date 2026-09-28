//! Divider drags and mod+drag tile moves: arming, cancellation on
//! every interrupt (workspace switch, palette, modal, escape,
//! deactivation), and the epoch guard against a stale drop.

use super::*;

/// Press-drag-release on a tile divider resizes the pair without moving tile focus, and
/// dirties the session once at release. Move well outside the divider strip to exercise
/// the full-window catcher. Cursor appearance is not asserted because the pinned test
/// platform exposes no cursor-style accessor.
#[gpui::test]
fn dragging_a_main_tree_splitter_resizes_the_pair_and_dirties_the_session(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell) = dock_test_shell(cx);

    // Two tiles side by side, focus moved to the LEFT tile — so if
    // the strip's mouse-down leaked through to the right tile under
    // the boundary, click-to-focus would visibly move focus.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-h");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("divider-strip-0").is_some(),
        "the splitter strip should have painted between the two tiles"
    );

    shell.update(&mut cx, |shell, _| shell.session_dirty = false);
    let focus_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });

    // Same chrome-offset math as `Render for ShellView` (and the
    // click-to-focus test above): the divider sits at 50% of the tile
    // area's width, offset by the sidebar/toolbar.
    let (grab, drop) = cx.update(|window, _| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::width(window)).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0);
        let mid_y = toolbar_height + content_height / 2.0;
        (
            gpui::point(px(sidebar::width(window) + tile_width * 0.5), px(mid_y)),
            gpui::point(px(sidebar::width(window) + tile_width * 0.25), px(mid_y)),
        )
    });

    cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        }),
        focus_before,
        "grabbing the splitter must not change tile focus"
    );

    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
    let widths: Vec<f32> = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .workspaces
            .active()
            .tree()
            .layout(Rect::UNIT)
            .iter()
            .map(|(_, r)| r.w)
            .collect()
    });
    assert!(
        (widths[0] - 0.25).abs() < 1e-3 && (widths[1] - 0.75).abs() < 1e-3,
        "dropping the divider at 25% should relayout the pair 25/75, got {widths:?}"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "moves alone must not dirty the session — only the release does"
    );

    cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
    assert!(
        shell.read_with(&cx, |shell, _| shell.session_dirty),
        "releasing the drag should mark the session dirty (drag-resizes persist)"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
        "the drag should be over after mouse-up"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        }),
        focus_before,
        "a divider drag never changes tile focus"
    );
}

/// Dragging a dock edge resizes its frame live, clamps at `DOCK_MAX_SIZE`, and
/// continues tracking when the pointer crosses the limit.
#[gpui::test]
fn dragging_the_left_dock_edge_resizes_the_dock_and_pins_at_the_clamp(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-["); // show the (empty) left dock
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    shell.update(&mut cx, |shell, _| shell.session_dirty = false);

    let (sidebar_width, tile_width, mid_y) = cx.update(|window, _| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let sidebar_width = sidebar::width(window);
        let tile_width = (f32::from(viewport.width) - sidebar_width).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0);
        (
            sidebar_width,
            tile_width,
            toolbar_height + content_height / 2.0,
        )
    });
    let dock_size = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .docks()
                .get(crate::tiling::DockSide::Left)
                .size()
        })
    };
    assert!((dock_size(&shell, &cx) - crate::tiling::DOCK_DEFAULT_SIZE).abs() < 1e-4);

    // Grab the dock's inner edge (at 25% of the content width) and
    // drag it to 40%.
    cx.simulate_mouse_down(
        gpui::point(px(sidebar_width + tile_width * 0.25), px(mid_y)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.simulate_mouse_move(
        gpui::point(px(sidebar_width + tile_width * 0.4), px(mid_y)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(
        (dock_size(&shell, &cx) - 0.40).abs() < 1e-3,
        "dragging the edge to 40% should set the dock size to 0.40, got {}",
        dock_size(&shell, &cx)
    );

    // Keep dragging far past the maximum: the size pins at the clamp.
    cx.simulate_mouse_move(
        gpui::point(px(sidebar_width + tile_width * 0.9), px(mid_y)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(
        (dock_size(&shell, &cx) - crate::tiling::DOCK_MAX_SIZE).abs() < 1e-4,
        "dragging past the clamp should stop at DOCK_MAX_SIZE, got {}",
        dock_size(&shell, &cx)
    );

    cx.simulate_mouse_up(
        gpui::point(px(sidebar_width + tile_width * 0.9), px(mid_y)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.session_dirty),
        "a dock-edge drag should persist like any resize"
    );
    assert!(
        (dock_size(&shell, &cx) - crate::tiling::DOCK_MAX_SIZE).abs() < 1e-4,
        "the release must not move the edge again"
    );
}

/// Fullscreen hides divider strips along with docks and tile chrome. A second toggle
/// restores them; debug bounds establish whether each strip is painted.
#[gpui::test]
fn fullscreen_suppresses_divider_strips(cx: &mut gpui::TestAppContext) {
    let (mut cx, _shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("divider-strip-0").is_some(),
        "two tiles paint their splitter strip"
    );

    cx.simulate_keystrokes("alt-f");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("divider-strip-0").is_none(),
        "a fullscreen tile has no visible boundaries — no strips"
    );

    cx.simulate_keystrokes("alt-f");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("divider-strip-0").is_some(),
        "leaving fullscreen brings the strips back"
    );
}

/// Keyboard navigation remains active during a drag. Switching workspaces cancels the
/// divider gesture, and later mouse moves cannot resize the new workspace. Give both
/// workspaces identical layouts so an incorrectly reused divider address would visibly
/// resize the second tree.
#[gpui::test]
fn switching_workspaces_mid_drag_cancels_the_drag_without_retargeting(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell) = dock_test_shell(cx);

    // Workspace 1: two tiles. Workspace 2: two tiles, same layout.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-2");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-1");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let (grab, drop_a, drop_b) = cx.update(|window, _| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::width(window)).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0);
        let mid_y = toolbar_height + content_height / 2.0;
        (
            gpui::point(px(sidebar::width(window) + tile_width * 0.5), px(mid_y)),
            gpui::point(px(sidebar::width(window) + tile_width * 0.25), px(mid_y)),
            gpui::point(px(sidebar::width(window) + tile_width * 0.3), px(mid_y)),
        )
    });

    cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_move(drop_a, MouseButton::Left, gpui::Modifiers::none());

    // Switch to workspace 2 with the button still down, then keep
    // moving.
    cx.simulate_keystrokes("alt-2");
    let ws2_before: Vec<_> = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });
    cx.simulate_mouse_move(drop_b, MouseButton::Left, gpui::Modifiers::none());

    assert!(
        shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
        "the workspace switch should have cancelled the drag"
    );
    let ws2_after: Vec<_> = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });
    assert_eq!(
        ws2_before, ws2_after,
        "the continued move must not resize workspace 2's tree"
    );
    // The original workspace retains and persists the resize already applied before the
    // switch. Canceling stops tracking; it does not undo completed moves.
    cx.simulate_keystrokes("alt-1");
    let ws1_widths: Vec<f32> = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .workspaces
            .active()
            .tree()
            .layout(Rect::UNIT)
            .iter()
            .map(|(_, r)| r.w)
            .collect()
    });
    assert!(
        (ws1_widths[0] - 0.25).abs() < 1e-3,
        "workspace 1 keeps the applied resize, got {ws1_widths:?}"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.session_dirty),
        "a cancelled drag that had moved must still persist its resize"
    );
}

/// Opening the palette ends a divider drag while preserving its applied resize and
/// marking the session dirty for persistence.
#[gpui::test]
fn opening_the_palette_mid_drag_keeps_and_persists_the_applied_resize(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    shell.update(&mut cx, |shell, _| shell.session_dirty = false);

    let (grab, drop) = cx.update(|window, _| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::width(window)).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0);
        let mid_y = toolbar_height + content_height / 2.0;
        (
            gpui::point(px(sidebar::width(window) + tile_width * 0.5), px(mid_y)),
            gpui::point(px(sidebar::width(window) + tile_width * 0.25), px(mid_y)),
        )
    });

    cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "mid-drag, nothing is persisted yet"
    );

    cx.simulate_keystrokes("ctrl-k"); // open the palette mid-drag

    assert!(
        shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
        "opening the palette should cancel the drag"
    );
    let widths: Vec<f32> = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .workspaces
            .active()
            .tree()
            .layout(Rect::UNIT)
            .iter()
            .map(|(_, r)| r.w)
            .collect()
    });
    assert!(
        (widths[0] - 0.25).abs() < 1e-3,
        "cancel keeps the applied resize (it is not an undo), got {widths:?}"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.session_dirty),
        "the applied resize must persist even though the drag was cancelled"
    );
}

// Modifier-drag tile movement.

/// The default mod alias (Alt) held on a mouse event — matches the
/// `alt-h`-style keystrokes the e2e tests already use for `mod+`.
pub(super) fn alt_held() -> gpui::Modifiers {
    gpui::Modifiers {
        alt: true,
        ..gpui::Modifiers::none()
    }
}

/// Window-space point at fractional coordinates within a main-tree
/// tile's laid-out rect — the same chrome-offset + dock-carve math
/// `Render for ShellView` uses, so the tests track real geometry
/// instead of duplicating guesses.
pub(super) fn main_tile_point(
    cx: &mut gpui::VisualTestContext,
    shell: &Entity<ShellView>,
    id: TileId,
    fx: f32,
    fy: f32,
) -> gpui::Point<gpui::Pixels> {
    cx.update(|window, app| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: (f32::from(viewport.width) - sidebar::width(window)).max(0.0),
            h: (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0),
        };
        let shell = shell.read(app);
        let workspace = shell.services.workspaces.active();
        let (tree_area, _) = crate::tiling::dock_layout(workspace.docks(), area);
        let r = workspace
            .tree()
            .layout(tree_area)
            .into_iter()
            .find(|(t, _)| *t == id)
            .expect("tile present in the main layout")
            .1;
        gpui::point(
            px(sidebar::width(window) + r.x + r.w * fx),
            px(toolbar_height + r.y + r.h * fy),
        )
    })
}

/// Window-space point at fractional coordinates within a visible
/// dock's frame rect (same math as [`main_tile_point`]).
fn dock_point(
    cx: &mut gpui::VisualTestContext,
    shell: &Entity<ShellView>,
    side: DockSide,
    fx: f32,
    fy: f32,
) -> gpui::Point<gpui::Pixels> {
    cx.update(|window, app| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: (f32::from(viewport.width) - sidebar::width(window)).max(0.0),
            h: (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0),
        };
        let shell = shell.read(app);
        let workspace = shell.services.workspaces.active();
        let (_, dock_rects) = crate::tiling::dock_layout(workspace.docks(), area);
        let r = dock_rects
            .into_iter()
            .find(|(s, _)| *s == side)
            .expect("dock visible in the layout")
            .1;
        gpui::point(
            px(sidebar::width(window) + r.x + r.w * fx),
            px(toolbar_height + r.y + r.h * fy),
        )
    })
}

/// Window-space point at fractional coordinates within a tile laid out
/// inside a visible dock's own tree — the dock counterpart of
/// [`main_tile_point`], laying `side`'s tree out inside the dock frame
/// [`dock_layout`] carves rather than treating the whole frame as one
/// drop target.
pub(super) fn dock_tile_point(
    cx: &mut gpui::VisualTestContext,
    shell: &Entity<ShellView>,
    side: DockSide,
    id: TileId,
    fx: f32,
    fy: f32,
) -> gpui::Point<gpui::Pixels> {
    cx.update(|window, app| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: (f32::from(viewport.width) - sidebar::width(window)).max(0.0),
            h: (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0),
        };
        let shell = shell.read(app);
        let workspace = shell.services.workspaces.active();
        let (_, dock_rects) = crate::tiling::dock_layout(workspace.docks(), area);
        let dock_rect = dock_rects
            .into_iter()
            .find(|(s, _)| *s == side)
            .expect("dock visible in the layout")
            .1;
        let r = workspace
            .docks()
            .get(side)
            .tree()
            .layout(dock_rect)
            .into_iter()
            .find(|(t, _)| *t == id)
            .expect("tile present in the dock layout")
            .1;
        gpui::point(
            px(sidebar::width(window) + r.x + r.w * fx),
            px(toolbar_height + r.y + r.h * fy),
        )
    })
}

/// Shared setup for the tile-drag e2e tests: two tiles side by side,
/// focus moved to the LEFT tile, session dirt reset. Returns
/// `(cx, shell, left, right)`.
fn two_tile_drag_shell(
    cx: &mut gpui::TestAppContext,
) -> (gpui::VisualTestContext, Entity<ShellView>, TileId, TileId) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-h"); // focus the left tile
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    shell.update(&mut cx, |shell, _| shell.session_dirty = false);
    let tiles: Vec<TileId> = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles()
    });
    let (left, right) = (tiles[0], tiles[1]);
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(left),
        "sanity: focus starts on the left tile"
    );
    (cx, shell, left, right)
}

/// End-to-end: a real mod+press on a tile body, dragged past the
/// movement threshold onto another tile's LEFT edge band and
/// released, split-inserts the dragged tile on that side — focus
/// follows the moved tile, the session goes dirty, and the drag is
/// over. Also pins three recorded decisions along the way: the
/// mod+down itself must NOT change focus at arm time; the mod key
/// does not need to stay held once armed (the move and release are
/// sent with no modifiers); and mid-drag the ghost + zone highlight
/// paint (via `debug_bounds`, the honest painted-or-not hook).
#[gpui::test]
fn mod_dragging_a_tile_onto_anothers_edge_moves_it_and_focus_follows(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(left),
        "mod+down must not change focus at arm time"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_some()),
        "mod+down on a tile body arms a pending drag"
    );

    // Deep in the left tile's LEFT band, far past the 5px threshold.
    // Modifiers deliberately released: the mod key only gates arming.
    let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("tile-drag-ghost").is_some(),
        "an active drag paints its cursor ghost"
    );
    assert!(
        cx.debug_bounds("tile-drop-highlight").is_some(),
        "an active drag over a target paints the zone highlight"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "nothing is applied (or persisted) until the drop"
    );

    cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .tiles()),
        vec![right, left],
        "an edge drop on the left band inserts the dragged tile before the target"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(right),
        "focus follows the moved tile"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.session_dirty),
        "an applied drop dirties the session"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "the drag is over after the drop"
    );
}

/// End-to-end: a sloppy mod+click — press, a 2px wiggle (below the
/// 5px threshold), release — changes nothing at all: layout, focus
/// (the recorded no-focus-at-arm decision), and session dirt are all
/// exactly as before, and no drag remains armed.
#[gpui::test]
fn a_below_threshold_mod_click_changes_nothing_at_all(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    let wiggle = gpui::point(grab.x + px(2.0), grab.y + px(2.0));
    cx.simulate_mouse_move(wiggle, MouseButton::Left, alt_held());
    cx.simulate_mouse_up(wiggle, MouseButton::Left, alt_held());

    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "a below-threshold mod+click must never rearrange"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(left),
        "focus is untouched — the abandoned gesture leaves everything alone"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "nothing changed, so nothing is persisted"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "the pending drag is cleared on release"
    );
}

/// A center drop stacks the dragged tile onto the target, with focus following the
/// dragged tile into the stack.
#[gpui::test]
fn mod_dragging_onto_a_tiles_center_stacks_the_pair(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

    let grab = main_tile_point(&mut cx, &shell, left, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    let drop = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());

    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .tiles()),
        vec![right, left],
        "a center drop adds the dragged tile after the target"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .stack_position(left)),
        Some((2, 2)),
        "the dragged tile lands as the stack's second member"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(left),
        "focus follows the dragged tile into the stack"
    );
    assert!(shell.read_with(&cx, |shell, _| shell.session_dirty));
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "the finished drag is cleared"
    );
}

/// End-to-end: dropping a tile on a visible (empty) dock's background
/// inserts it into that dock's tree with the keyboard `dock::move_*`
/// convention, region and focus following it into the dock.
#[gpui::test]
fn mod_dragging_onto_a_dock_background_inserts_into_the_dock(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _left, right) = two_tile_drag_shell(cx);
    cx.simulate_keystrokes("ctrl-["); // show the (empty) left dock
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    shell.update(&mut cx, |shell, _| shell.session_dirty = false);

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    let drop = dock_point(&mut cx, &shell, DockSide::Left, 0.5, 0.5);
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());

    shell.read_with(&cx, |shell, _| {
        let workspace = shell.services.workspaces.active();
        assert_eq!(
            workspace.docks().get(DockSide::Left).tree().tiles(),
            vec![right],
            "the dropped tile joins the dock's tree"
        );
        assert_eq!(
            workspace.region(),
            crate::tiling::FocusRegion::Dock(DockSide::Left),
            "the region follows the moved tile into the dock"
        );
        assert!(
            !workspace.tree().contains(right),
            "the tile left the main tree"
        );
    });
    assert!(shell.read_with(&cx, |shell, _| shell.session_dirty));
}

/// End-to-end cancel guard: `mod+2` switching workspaces mid-drag
/// cancels the drag cleanly — nothing applied, nothing persisted, the
/// original workspace's layout untouched when the (now targetless)
/// release lands.
#[gpui::test]
fn switching_workspaces_mid_tile_drag_cancels_with_nothing_applied(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

    cx.simulate_keystrokes("alt-2"); // keyboard stays live mid-drag
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "a workspace switch mid-drag cancels the tile drag"
    );

    // Switching to empty workspace 2 unmounts the focused occupant but
    // retains its view and focus handle. GPUI's fallback dispatch cannot
    // reach the shell's key listeners while that unmounted handle owns
    // focus. `ensure_occupants` restores shell focus in the draw above,
    // so `alt-1` reaches the shell. The grab's own focus restoration is
    // isolated in `a_grab_leaves_the_shell_focused_on_the_next_frame`.
    cx.simulate_keystrokes("alt-1");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.services.workspaces.active_index()),
        1,
        "the keyboard survived the grab: `alt-1` still reaches the shell after \
         the workspace switch unmounted the focused occupant"
    );
    shell.update(&mut cx, |shell, _| shell.session_dirty = false);
    cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "nothing was applied by the cancelled drag"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "a cancelled tile drag persists nothing (cancel is truly free)"
    );
}

/// End-to-end cancel guard: opening the palette mid-drag (`ctrl+k`)
/// cancels the tile drag with nothing applied — unlike the divider
/// drag's palette cancel, which keeps its already-applied live
/// resize, a tile drag has applied nothing to keep.
#[gpui::test]
fn opening_the_palette_mid_tile_drag_cancels_with_nothing_applied(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

    cx.simulate_keystrokes("ctrl-k");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "opening the palette mid-drag cancels the tile drag"
    );

    cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "the release after the cancel applies nothing"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "nothing persisted"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "the palette itself stays open (the release is not a dismissing click)"
    );
}

/// Right-click focuses the target tile so context-menu keys reach its occupant, but
/// never arms a drag, whether or not the modifier is held.
#[gpui::test]
fn a_right_click_focuses_the_tile_and_never_arms_a_drag(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    let focused = |cx: &gpui::VisualTestContext| {
        shell.read_with(cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        })
    };
    assert_eq!(focused(&cx), Some(left));
    let click = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(click, MouseButton::Right, gpui::Modifiers::none());
    assert_eq!(focused(&cx), Some(right), "a right press focuses");
    assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()));
    let back = main_tile_point(&mut cx, &shell, left, 0.5, 0.5);
    cx.simulate_mouse_down(back, MouseButton::Right, alt_held());
    assert_eq!(focused(&cx), Some(left), "with the mod key too");
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "a right press never arms a drag"
    );
}

/// End-to-end: a plain (no-mod) click on a tile still focuses it and
/// never arms a drag — the tile-drag feature leaves click-to-focus
/// byte-for-byte in behavior.
#[gpui::test]
fn a_plain_click_still_focuses_and_never_arms_a_drag(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(left)
    );
    let click = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(click, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(right),
        "plain click-to-focus is unchanged"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "no drag arms without the mod key"
    );
}

/// A tile grab restores shell focus on the next frame, just as a plain
/// tile mouse-down does. The fixture's recorder takes focus on the press
/// as `DataTable` does. No tile leaves the visible set, so
/// `ensure_occupants`'s departed-tile backstop cannot mask a missing
/// restore from the grab itself.
#[gpui::test]
fn a_grab_leaves_the_shell_focused_on_the_next_frame(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _left, right) = two_tile_drag_shell(cx);
    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_some()),
        "sanity: the grab armed a drag, so the click-to-focus tail was skipped"
    );

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let root = shell.read_with(&cx, |shell, _| shell.focus_handle.clone());
    assert!(
        cx.update(|window, _| root.is_focused(window)),
        "the grab re-armed the focus restore, so the shell root has focus again"
    );

    // Leave no drag in flight for the fixture's teardown.
    cx.simulate_mouse_up(grab, MouseButton::Left, gpui::Modifiers::none());
}

/// After a keystroke changes GPUI's input modality to Keyboard, a stationary release
/// reaches the drag catcher's `on_mouse_up_out` handler. That path must still finish
/// the tile drop, matching a release through `on_mouse_up`.
#[gpui::test]
fn a_keystroke_mid_drag_does_not_turn_a_stationary_release_into_a_cancel(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

    // An unbound key — hits the matcher, matches nothing, changes no
    // shell state, but flips the window's input modality to Keyboard.
    cx.simulate_keystrokes("x");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .tile_drag
            .as_ref()
            .is_some_and(|drag| drag.active)),
        "an unbound keystroke mid-drag must not cancel the drag"
    );

    // Release without moving: under keyboard modality this dispatches
    // through the catcher's `on_mouse_up_out` gate.
    cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .tiles()),
        vec![right, left],
        "the stationary release after a keystroke must still apply the edge drop"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(right),
        "focus follows the moved tile"
    );
    assert!(shell.read_with(&cx, |shell, _| shell.session_dirty));
    assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()));
}

/// Opening the palette and releasing a drag can occur before another render. Dispatch
/// both events inside one update to keep the render guard from running between them;
/// the drop-time check must reject the drop under the newly opened palette.
#[gpui::test]
fn a_release_in_the_same_frame_as_the_palette_opening_applies_nothing(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

    cx.update(|window, cx| {
        window.dispatch_keystroke(gpui::Keystroke::parse("ctrl-k").unwrap(), cx);
        assert!(
            shell.read(cx).palette.is_some(),
            "the keystroke opened the palette"
        );
        assert!(
            shell.read(cx).tile_drag.is_some(),
            "no render has run since the keystroke, so the render-top guard has \
             not cancelled the drag — the drop-time re-check is the only defense"
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: drop,
                modifiers: gpui::Modifiers::none(),
                click_count: 1,
            }),
            cx,
        );
    });
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "the release must not apply the drop underneath the just-opened palette"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "nothing applied, nothing persisted"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "the drag is over either way"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "the palette stays open"
    );
}

/// A modifier-click can press and release before a frame installs drag-catcher
/// handlers. The root mouse-up fallback must clear that armed drag immediately. A later
/// unmodified press-drag-release must neither activate the old gesture nor rearrange
/// tiles.
#[gpui::test]
fn a_mod_click_released_in_the_arm_frame_leaves_no_phantom_drag(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    // Down + up dispatched inside ONE `cx.update`, no draw between —
    // the same one-frame window real platforms produce (technique
    // from the same-frame palette test above).
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: grab,
                modifiers: alt_held(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        assert!(
            shell.read(cx).tile_drag.is_some(),
            "sanity: the mod+down armed a pending drag"
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: grab,
                modifiers: alt_held(),
                click_count: 1,
            }),
            cx,
        );
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "a release in the same frame as the arm must clear the armed drag \
         (no phantom drag survives to the catcher's first paint)"
    );

    // An unmodified press-drag(>5px)-release afterwards must behave
    // like the plain gesture it is: click-to-focus, no rearrangement.
    cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
    let far = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_move(far, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(far, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "an unmodified press-drag-release after the phantom window must not \
         rearrange the layout"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(right),
        "the plain click focused the tile it landed on (click-to-focus intact)"
    );
}

/// A divider press and release can arrive before its catcher is rendered. The root
/// mouse-up fallback must clear the gesture within that frame, and a later unmodified
/// press-drag must not resize the stale divider.
#[gpui::test]
fn a_strip_click_released_in_the_arm_frame_leaves_no_phantom_divider_drag(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell, _left, right) = two_tile_drag_shell(cx);
    let widths_before: Vec<f32> = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .workspaces
            .active()
            .tree()
            .layout(Rect::UNIT)
            .into_iter()
            .map(|(_, r)| r.w)
            .collect()
    });

    let strip = cx
        .debug_bounds("divider-strip-0")
        .expect("two tiles paint their splitter strip");
    let grab = strip.center();
    // Down + up dispatched inside ONE `cx.update`, no draw between —
    // the same technique as the tile-drag phantom test above.
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position: grab,
                modifiers: gpui::Modifiers::none(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        assert!(
            shell.read(cx).divider_drag.is_some(),
            "sanity: the strip's mouse-down armed a divider drag"
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: grab,
                modifiers: gpui::Modifiers::none(),
                click_count: 1,
            }),
            cx,
        );
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
        "a release in the same frame as the arm must clear the armed \
         divider drag (no phantom survives to the catcher's first paint)"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "a phantom that moved nothing persists nothing"
    );

    // An unmodified press-drag afterwards must be the plain gesture it
    // is (click-to-focus on the tile it lands on), never a live resize
    // of the phantom's divider.
    let press = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(press, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_move(
        gpui::point(press.x - px(120.0), press.y),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.simulate_mouse_up(
        gpui::point(press.x - px(120.0), press.y),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect::UNIT)
                .into_iter()
                .map(|(_, r)| r.w)
                .collect::<Vec<f32>>()
        }),
        widths_before,
        "an unmodified press-drag after the phantom window must not resize \
         any divider"
    );
}

/// Escape cancels an active tile drag without reaching the matcher. The later release
/// applies and persists nothing.
#[gpui::test]
fn escape_mid_tile_drag_cancels_with_nothing_applied(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .tile_drag
            .as_ref()
            .is_some_and(|drag| drag.active)),
        "sanity: the drag is active before Escape"
    );

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "Escape mid-drag must cancel the tile drag"
    );

    cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "the release after the Escape cancel applies nothing"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "a cancelled tile drag persists nothing"
    );
}

/// Escape also clears an armed tile drag before it crosses the movement threshold. Its
/// later release is unarmed.
#[gpui::test]
fn escape_clears_an_armed_but_inactive_tile_drag(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _left, right) = two_tile_drag_shell(cx);
    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_some()));

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "Escape must clear an armed-but-inactive drag too"
    );
}

/// Escape ends a divider drag and persists its already-applied resize. Further moves
/// change nothing; stopping tracking does not undo live divider changes.
#[gpui::test]
fn escape_mid_divider_drag_finishes_it_keeping_applied_resizes(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _left, _right) = two_tile_drag_shell(cx);

    // Grab the divider between the two tiles and drag it left.
    let strip = cx
        .debug_bounds("divider-strip-0")
        .expect("two tiles paint their splitter strip");
    let grab = strip.center();
    cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
    assert!(shell.read_with(&cx, |shell, _| shell.divider_drag.is_some()));
    let target = gpui::point(grab.x - px(100.0), grab.y);
    cx.simulate_mouse_move(target, MouseButton::Left, gpui::Modifiers::none());
    let widths_after_move: Vec<f32> = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .workspaces
            .active()
            .tree()
            .layout(Rect::UNIT)
            .into_iter()
            .map(|(_, r)| r.w)
            .collect()
    });

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
        "Escape mid-divider-drag must end the drag"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.session_dirty),
        "the applied resize persists (finish, not revert)"
    );

    // Further moves with the button still (nominally) held must no
    // longer resize anything — the drag is over.
    let farther = gpui::point(grab.x - px(200.0), grab.y);
    cx.simulate_mouse_move(farther, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect::UNIT)
                .into_iter()
                .map(|(_, r)| r.w)
                .collect::<Vec<f32>>()
        }),
        widths_after_move,
        "no further tracking after Escape ended the divider drag"
    );
}

/// Closing the dragged tile through the keyboard cancels the gesture at the next
/// render. No ghost or drop highlight remains, and release applies nothing.
#[gpui::test]
fn closing_the_dragged_tile_mid_drag_cancels_the_drag(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

    // Drag the LEFT tile (the focused one — ctrl+w closes the focused
    // tile, so dragging it is what makes the close hit the drag).
    let grab = main_tile_point(&mut cx, &shell, left, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    let over = main_tile_point(&mut cx, &shell, right, 0.05, 0.5);
    cx.simulate_mouse_move(over, MouseButton::Left, gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("tile-drop-highlight").is_some(),
        "sanity: the active drag paints its zone highlight before the close"
    );

    cx.simulate_keystrokes("ctrl-w"); // closes the focused (= dragged) tile
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "closing the dragged tile mid-drag must cancel the drag"
    );
    assert!(
        cx.debug_bounds("tile-drop-highlight").is_none(),
        "no zone highlight may keep painting for a tile that no longer exists"
    );
    assert!(
        cx.debug_bounds("tile-drag-ghost").is_none(),
        "no ghost may keep painting for a tile that no longer exists"
    );

    shell.update(&mut cx, |shell, _| shell.session_dirty = false);
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });
    cx.simulate_mouse_up(over, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "the release after the cancel applies nothing"
    );
    assert!(!shell.read_with(&cx, |shell, _| shell.session_dirty));
}

/// Moves attributed to a second mouse button are ignored without advancing or canceling
/// a tile drag. A buttonless move cancels as a lost release. This keeps behavior
/// consistent when platforms report chorded-button moves differently.
#[gpui::test]
fn a_chorded_second_button_move_mid_drag_neither_cancels_nor_advances(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    let over = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_move(over, MouseButton::Left, gpui::Modifiers::none());
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .tile_drag
            .as_ref()
            .is_some_and(|drag| drag.active)),
        "sanity: the drag is active"
    );

    // A chorded right-button move (the macOS NSRightMouseDragged
    // shape) at a DIFFERENT position: the drag must survive AND not
    // track it (ignored entirely).
    let elsewhere = main_tile_point(&mut cx, &shell, right, 0.9, 0.9);
    cx.simulate_mouse_move(elsewhere, MouseButton::Right, gpui::Modifiers::none());
    shell.read_with(&cx, |shell, _| {
        let drag = shell
            .tile_drag
            .as_ref()
            .expect("a chorded second-button move must not cancel the drag");
        assert_eq!(
            drag.cursor,
            (f32::from(over.x), f32::from(over.y)),
            "an ignored move must not advance the drag's cursor either"
        );
    });

    // A buttonless move IS the lost-release signal: cancel.
    cx.simulate_mouse_move(elsewhere, None, gpui::Modifiers::none());
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "a buttonless move (lost release) must cancel the drag"
    );
}

/// Divider drags ignore moves attributed to another mouse button; a buttonless move
/// finishes the drag.
#[gpui::test]
fn a_chorded_second_button_move_mid_divider_drag_does_not_finish_it(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _left, _right) = two_tile_drag_shell(cx);
    let strip = cx
        .debug_bounds("divider-strip-0")
        .expect("two tiles paint their splitter strip");
    let grab = strip.center();
    cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
    assert!(shell.read_with(&cx, |shell, _| shell.divider_drag.is_some()));

    cx.simulate_mouse_move(
        gpui::point(grab.x - px(50.0), grab.y),
        MouseButton::Right,
        gpui::Modifiers::none(),
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.divider_drag.is_some()),
        "a chorded second-button move must not finish the divider drag"
    );

    cx.simulate_mouse_move(
        gpui::point(grab.x - px(50.0), grab.y),
        None,
        gpui::Modifiers::none(),
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
        "a buttonless move (lost release) must finish the divider drag"
    );
}

/// Window deactivation cancels tile drags and finishes divider drags, so releasing the
/// button elsewhere cannot leave an active gesture for the reactivation click. Drive
/// the test window's real activation callbacks to exercise the observer.
#[gpui::test]
fn window_deactivation_mid_drag_ends_both_drag_kinds(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    cx.update(|window, _cx| window.activate_window());
    cx.run_until_parked();

    // Tile drag: deactivation cancels with nothing applied.
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });
    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    let over = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    cx.simulate_mouse_move(over, MouseButton::Left, gpui::Modifiers::none());
    assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_some()));

    cx.deactivate_window();
    assert!(
        shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
        "window deactivation mid-tile-drag must cancel the drag"
    );
    cx.simulate_mouse_up(over, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "the release after re-activation applies nothing"
    );

    // Divider drag: deactivation finishes it (applied moves persist).
    cx.update(|window, _cx| window.activate_window());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let strip = cx
        .debug_bounds("divider-strip-0")
        .expect("two tiles paint their splitter strip");
    let dgrab = strip.center();
    cx.simulate_mouse_down(dgrab, MouseButton::Left, gpui::Modifiers::none());
    assert!(shell.read_with(&cx, |shell, _| shell.divider_drag.is_some()));
    cx.deactivate_window();
    assert!(
        shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
        "window deactivation mid-divider-drag must end the drag"
    );
}

/// A workspace switch away and back invalidates an active drag even when the final
/// workspace index matches its origin. The switch epoch distinguishes this from never
/// leaving.
///
/// Dispatch switch actions directly with no intervening draw. At the pinned GPUI
/// revision, key dispatch draws a dirty window first, which normally runs the render
/// cancellation guard before the second switch; the drag catcher also occludes sidebar
/// clicks. Direct actions isolate the epoch check from those additional guards.
#[gpui::test]
fn switching_away_and_back_within_one_frame_voids_the_drop(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    let layout_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });

    let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
    cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
    cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

    // Switch away, switch back, and release — all inside ONE
    // `cx.update`, no draw between: the index is back to where the
    // drag started by release time, so only the epoch comparison can
    // refuse the drop.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(
                &ActionId("workspace::switch_2".to_string()),
                None,
                window,
                cx,
            );
            shell.dispatch(
                &ActionId("workspace::switch_1".to_string()),
                None,
                window,
                cx,
            );
        });
        assert_eq!(
            shell.read(cx).services.workspaces.active_index(),
            1,
            "sanity: back on the original workspace before the release"
        );
        assert!(
            shell.read(cx).tile_drag.is_some(),
            "no render has run, so the render-top guard has not cancelled \
             the drag — the drop-time epoch re-check is the only defense"
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: drop,
                modifiers: gpui::Modifiers::none(),
                click_count: 1,
            }),
            cx,
        );
    });
    assert_eq!(
        shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        }),
        layout_before,
        "a release after an away-and-back switch inside one frame must \
         apply nothing"
    );
    assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()));
}

// Modifier-double-click fullscreen.

/// mod+double-click on a main-tree tile focuses it and makes it
/// fullscreen (the mouse form of `mod+f`, TODO "Mod + doubleclick to
/// maximize/minimize tile"); a second mod+double-click on the now
/// fullscreen tile restores the layout. Neither leaves a drag armed,
/// and both go dirty like the keyboard verb.
#[gpui::test]
fn mod_double_click_toggles_fullscreen_on_that_tile(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(left),
        "sanity: focus starts on the left tile"
    );

    let at = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    double_click(&mut cx, at, alt_held());
    shell.read_with(&cx, |shell, _| {
        let tree = shell.services.workspaces.active().tree();
        assert_eq!(
            tree.fullscreen(),
            Some(right),
            "the double-clicked tile went fullscreen"
        );
        assert_eq!(
            tree.focused(),
            Some(right),
            "and took tile focus on the way"
        );
        assert!(shell.tile_drag.is_none(), "no drag is left armed");
        assert!(
            shell.session_dirty,
            "a fullscreen toggle persists like mod+f"
        );
    });

    shell.update(&mut cx, |shell, _| shell.session_dirty = false);
    // While fullscreen the tile fills the tree area, so the same point
    // is on it.
    double_click(&mut cx, at, alt_held());
    shell.read_with(&cx, |shell, _| {
        let tree = shell.services.workspaces.active().tree();
        assert_eq!(
            tree.fullscreen(),
            None,
            "the second double-click restores the layout"
        );
        assert_eq!(tree.focused(), Some(right));
        assert!(shell.tile_drag.is_none());
        assert!(shell.session_dirty);
    });
}

/// An unmodified double-click is two ordinary clicks: it focuses the
/// tile and nothing more.
#[gpui::test]
fn a_plain_double_click_does_not_fullscreen(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, _left, right) = two_tile_drag_shell(cx);
    let at = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
    double_click(&mut cx, at, gpui::Modifiers::none());
    shell.read_with(&cx, |shell, _| {
        let tree = shell.services.workspaces.active().tree();
        assert_eq!(tree.fullscreen(), None, "no mod, no fullscreen");
        assert_eq!(tree.focused(), Some(right), "click-to-focus still ran");
    });
}

/// Fullscreen is main-tree-only (`Workspace::toggle_fullscreen` is a
/// claimed no-op while a dock is focused), so mod+double-click on a
/// docked tile changes nothing — exactly as `mod+f` there.
#[gpui::test]
fn mod_double_click_on_a_docked_tile_changes_nothing(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
    cx.simulate_keystrokes("ctrl-{"); // move the focused (left) tile to the left dock
    cx.simulate_keystrokes("alt-l"); // and put focus back on the main tree
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    shell.read_with(&cx, |shell, _| {
        let workspace = shell.services.workspaces.active();
        assert_eq!(
            workspace.docks().get(DockSide::Left).tree().tiles(),
            vec![left]
        );
        assert_eq!(workspace.region(), crate::tiling::FocusRegion::Main);
        assert_eq!(workspace.tree().focused(), Some(right));
    });
    shell.update(&mut cx, |shell, _| shell.session_dirty = false);

    let at = dock_point(&mut cx, &shell, DockSide::Left, 0.5, 0.5);
    double_click(&mut cx, at, alt_held());
    shell.read_with(&cx, |shell, _| {
        let workspace = shell.services.workspaces.active();
        assert_eq!(
            workspace.tree().fullscreen(),
            None,
            "no fullscreen on the main tree"
        );
        assert_eq!(
            workspace.docks().get(DockSide::Left).tree().fullscreen(),
            None
        );
        assert_eq!(
            workspace.region(),
            crate::tiling::FocusRegion::Main,
            "a mod+down changes no focus, and the door refuses on a dock"
        );
        assert!(shell.tile_drag.is_none());
        assert!(!shell.session_dirty, "nothing changed, nothing persisted");
    });
}
