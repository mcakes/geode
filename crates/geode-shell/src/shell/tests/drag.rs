//! Divider drags and mod+drag tile moves: arming, cancellation on
//! every interrupt (workspace switch, palette, modal, escape,
//! deactivation), and the epoch guard against a stale drop.

use super::*;

/// End-to-end (drag-splitters task): a real press-drag-release on the
/// splitter between two tiles resizes the pair proportionally to
/// where the cursor was dropped, never touches tile focus (the strip
/// occludes the tile edges it overlaps, so the mouse-down that starts
/// the drag must NOT fire the tiles' click-to-focus), and dirties the
/// session exactly once, at mouse-up — not per move. The drop point is
/// deliberately far off the 8px strip: the moves land on the
/// full-window drag catcher, which is the whole capture mechanism
/// under test. Cursor appearance (col-resize) is NOT asserted —
/// gpui's `TestPlatform` records `set_cursor_style` into a private
/// field with no accessor at the pinned rev, so there is no honest way
/// to check it from a test.
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
        let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
        let mid_y = toolbar_height + content_height / 2.0;
        (
            gpui::point(px(sidebar::WIDTH + tile_width * 0.5), px(mid_y)),
            gpui::point(px(sidebar::WIDTH + tile_width * 0.25), px(mid_y)),
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

/// End-to-end (drag-splitters task): dragging the left dock's frame
/// edge resizes the dock frame itself, live per move, pinning at
/// `DOCK_MAX_SIZE` when dragged past the clamp instead of failing —
/// the same press keeps working after crossing the limit.
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

    let (tile_width, mid_y) = cx.update(|window, _| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
        (tile_width, toolbar_height + content_height / 2.0)
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
        gpui::point(px(sidebar::WIDTH + tile_width * 0.25), px(mid_y)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.simulate_mouse_move(
        gpui::point(px(sidebar::WIDTH + tile_width * 0.4), px(mid_y)),
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
        gpui::point(px(sidebar::WIDTH + tile_width * 0.9), px(mid_y)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(
        (dock_size(&shell, &cx) - crate::tiling::DOCK_MAX_SIZE).abs() < 1e-4,
        "dragging past the clamp should stop at DOCK_MAX_SIZE, got {}",
        dock_size(&shell, &cx)
    );

    cx.simulate_mouse_up(
        gpui::point(px(sidebar::WIDTH + tile_width * 0.9), px(mid_y)),
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

/// Drag-splitters task: fullscreen already suppresses docks and tile
/// chrome, and the divider strips must follow — `mod+f` (alt+f here,
/// the test mod alias) makes the strips disappear and a second toggle
/// brings them back. Asserted via `debug_bounds` (presence of the
/// painted strip element), the same honest limitation as the hint
/// tests above.
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

/// Review fix 1, end-to-end: the keyboard stays live during a drag,
/// so `mod+2` mid-drag switches workspaces — the drag must cancel
/// (the recorded address and bounds belong to workspace 1), and a
/// continued mouse-move must NOT resize workspace 2's tree even
/// though the same address is structurally valid there. Both
/// workspaces are set up with the identical two-tile layout precisely
/// so a wrongly-retargeted move WOULD visibly change workspace 2.
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
        let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
        let mid_y = toolbar_height + content_height / 2.0;
        (
            gpui::point(px(sidebar::WIDTH + tile_width * 0.5), px(mid_y)),
            gpui::point(px(sidebar::WIDTH + tile_width * 0.25), px(mid_y)),
            gpui::point(px(sidebar::WIDTH + tile_width * 0.3), px(mid_y)),
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
    // Workspace 1 keeps the part of the drag that was applied before
    // the switch (cancel is not undo), and — review fix 2 — that
    // applied resize persists: the cancel dirtied the session.
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

/// Review fix 2, end-to-end: opening the palette mid-drag cancels the
/// drag but keeps — and persists — what it already applied. The first
/// cut dropped the drag without dirtying the session, so the visible
/// resize silently diverged from the next restore.
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
        let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
        let mid_y = toolbar_height + content_height / 2.0;
        (
            gpui::point(px(sidebar::WIDTH + tile_width * 0.5), px(mid_y)),
            gpui::point(px(sidebar::WIDTH + tile_width * 0.25), px(mid_y)),
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

// --- mod+drag tile movement (tile-drag task) ------------------------

/// The default mod alias (Alt) held on a mouse event — matches the
/// `alt-h`-style keystrokes the e2e tests already use for `mod+`.
fn alt_held() -> gpui::Modifiers {
    gpui::Modifiers {
        alt: true,
        ..gpui::Modifiers::none()
    }
}

/// Window-space point at fractional coordinates within a main-tree
/// tile's laid-out rect — the same chrome-offset + dock-carve math
/// `Render for ShellView` uses, so the tests track real geometry
/// instead of duplicating guesses.
fn main_tile_point(
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
            w: (f32::from(viewport.width) - sidebar::WIDTH).max(0.0),
            h: (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0),
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
            px(sidebar::WIDTH + r.x + r.w * fx),
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
            w: (f32::from(viewport.width) - sidebar::WIDTH).max(0.0),
            h: (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0),
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
            px(sidebar::WIDTH + r.x + r.w * fx),
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

/// End-to-end: a center drop swaps the two tiles in place (today's
/// recorded keyboard-parity semantics), focus following the dragged
/// tile into its new slot.
#[gpui::test]
fn mod_dragging_onto_a_tiles_center_swaps_the_pair(cx: &mut gpui::TestAppContext) {
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
        "a center drop swaps the two tiles"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .focused()),
        Some(left),
        "focus follows the dragged tile to its new slot"
    );
    assert!(shell.read_with(&cx, |shell, _| shell.session_dirty));
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

    // The grab focused the tile's own occupant (the fixture's recorder
    // tracks its focus handle, as `DataTable` does), and switching to
    // the empty workspace 2 unmounted it — the orphaned-`FocusId` state
    // `pending_focus_restore` exists for, in which `handle_key_down`
    // stops firing until something claims focus again. Claim it through
    // the shell's own recovery path so the `alt-1` below still travels
    // the real key pipeline.
    //
    // TODO(focus-trap): production never re-arms this when the focused
    // occupant is unmounted by a workspace switch (see progress ledger /
    // follow-up); the RecordingView tracks its own focus, so this test
    // arms it by hand.
    shell.update(&mut cx, |shell, _| shell.pending_focus_restore = true);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("alt-1");
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

/// Review blocker regression: a keystroke mid-drag flips gpui's
/// input modality to Keyboard, `MouseUp` does not flip it back, and
/// `HitboxId::is_hovered` is false under keyboard modality — so a
/// stationary release after ANY keypress reaches the catcher through
/// `on_mouse_up_out`, not `on_mouse_up`. The keyboard is documented
/// hot mid-drag, so that release must still DROP (the fix routes
/// `up_out` through `finish_tile_drag`); before the fix it silently
/// cancelled.
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

/// Review should-fix regression: in production, input events arrive
/// between frames — the palette-toggle keystroke and the release can
/// both land before any render runs the cancel guard (the test
/// harness draws at the end of every simulated event's update, so
/// the two events are dispatched inside ONE `cx.update` here, the
/// same one-frame window real platforms produce; the mid-update
/// asserts verify the guard genuinely hasn't run). The drop-time
/// re-check in `finish_tile_drag` must refuse to apply the drop
/// underneath the just-opened palette.
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

/// Post-merge review BUG 1 regression (phantom armed drag): gpui
/// dispatches multiple input events between frames, so a fast
/// mod+click can land its mouse-DOWN and mouse-UP inside one frame
/// window — before any draw registers the tile-drag catcher's up
/// handlers. Before the fix the armed (never-activated) drag survived
/// that release forever: the next frame painted the full-window
/// grabbing catcher, the user's next stationary click was eaten, and
/// an unmodified press-drag-release could be APPLIED as a
/// rearrangement without the mod key held. The fix (root-level
/// mouse-up fallback) must clear the armed drag on that same-frame
/// release, and a subsequent unmodified press-drag-release must
/// change nothing.
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

/// Fix-round should-fix (the divider-drag twin of BUG 1): a strip's
/// mouse-down arms `divider_drag`, but the divider catcher's up
/// handlers only enter the hitbox tree at the next paint — so a
/// sub-frame click on a strip (down + up before any draw) left a
/// phantom armed divider drag: the full-window resize-cursor catcher
/// painted, the next mouse-down was swallowed, and an unmodified
/// press-drag (no intervening buttonless move) live-RESIZED the
/// phantom's divider. The root-element release fallback must clear
/// it, and a subsequent unmodified press-drag must resize nothing.
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

/// Post-merge review BUG 2: Escape mid-tile-drag cancels the drag —
/// nothing applied when the (now targetless) release lands, nothing
/// persisted, and the keystroke never reaches the matcher.
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

/// Post-merge review BUG 2 (armed-but-inactive arm): Escape also
/// clears a drag that never crossed the movement threshold, so the
/// release afterwards is a plain unarmed release.
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

/// Post-merge review BUG 2 (divider consistency, recorded decision):
/// Escape mid-divider-drag ENDS the drag — finish, not revert,
/// because a divider drag's resizes were already applied live and
/// cancel means "stop tracking the mouse", never "undo". Applied
/// moves persist (session dirty) and further mouse moves resize
/// nothing.
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

/// Post-merge review BUG 3: ctrl+w can close the dragged tile
/// mid-drag (the keyboard stays hot), and neither the render-top
/// guard nor `finish_tile_drag` checked the tile still exists —
/// leaving a ghost + zone highlight promising a drop that would
/// silently no-op. The dragged tile's existence must join the shared
/// cancel conditions: the drag cancels at the next paint, no
/// highlight paints, and the release applies nothing.
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

/// Post-merge review BUG 4: platform-uniform chorded-button handling.
/// macOS delivers a right/middle-dragged event as a MouseMoveEvent
/// with `pressed_button: Some(Right/Middle)` (gpui_macos events.rs
/// translates NSRightMouseDragged/NSOtherMouseDragged verbatim, no
/// left-first normalization), so before the fix a chorded second
/// button CANCELLED a mid-flight tile drag on macOS while Windows
/// (whose WM_MOUSEMOVE translation checks MK_LBUTTON first) let it
/// survive. The unified rule: a non-Left-button move is IGNORED
/// (neither advances nor cancels); only a buttonless move is the
/// lost-release cancel.
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

/// Post-merge review BUG 4, divider side: the divider catcher had the
/// same `pressed_button != Some(Left)` branch, so a chorded second
/// button FINISHED an in-flight divider drag on macOS. Same unified
/// rule: non-Left moves are ignored, buttonless moves finish.
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

/// Post-merge review finding 6: cmd+tab away with the button held,
/// release elsewhere — without an activation observer the stale
/// ACTIVE drag persisted and the re-activation click could advance
/// and apply it. `ShellView::new` now registers
/// `cx.observe_window_activation` (verified available at the pinned
/// gpui rev) and ends both drag kinds on deactivation (tile: cancel;
/// divider: finish). The test drives the harness's real activation
/// plumbing: `activate_window` marks the test window active, and
/// `deactivate_window` fires the platform active-status callback.
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

/// Post-merge review finding 7 (one-frame workspace ABA): the drop
/// re-check used to compare workspace INDEX equality only, so a
/// switch away and back with no render between satisfied the letter
/// of the check while violating its intent. The switch-epoch pin
/// closes it: any actual switch bumps the epoch, so away-and-back
/// can never look like "never left".
///
/// Honesty note on how the state is built: at the pinned gpui rev
/// this gap is NOT reachable through the real key pipeline — traced
/// while writing this test: `Window::dispatch_key_event` draws first
/// whenever the window is dirty, and the first switch's notify makes
/// it dirty, so the second switch's keystroke always runs the
/// render-top cancel guard (index mismatch) before dispatching.
/// `dispatch_mouse_event` does NOT draw-when-dirty, but the only
/// mouse path to a switch (a sidebar pill click) is occluded by the
/// drag catcher mid-drag. The epoch re-check is defense in depth for
/// exactly that reason — it must hold even if gpui's dispatch-order
/// details change under an upgrade — so the test dispatches the
/// switch ACTIONS directly (no key dispatch, no draw), constructing
/// the letter-of-the-rule state the guard can't otherwise see.
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
