//! Tiling key bindings end to end: adds, focus motion, resize, swap,
//! and close, dispatched through the real key pipeline. `ctrl+v` /
//! `ctrl+h` are the fixture layer's own bindings (`tests::
//! TEST_ADD_KEYMAP` — `tile::add_rec_horizontal`/`_vertical`), not
//! shipped keys: the builtin keymap has no create-a-tile chord (spec
//! 2026-09-08 add-tile §3.1).

use super::*;

/// A maximised tile reads differently from a workspace's only tile: the
/// status bar's fullscreen segment appears on `mod+f` (alt+f here, the
/// test mod alias) even for a lone tile, its tooltip names the key, and a
/// click on it restores the layout through the same action.
#[gpui::test]
fn the_fullscreen_segment_marks_a_maximised_tile_and_its_click_restores(
    cx: &mut gpui::TestAppContext,
) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("status-fullscreen").is_none(),
        "a lone tile that is not maximised shows no segment"
    );

    cx.simulate_keystrokes("alt-f");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().tree().fullscreen().is_some()
    }));
    let seg = cx
        .debug_bounds("status-fullscreen")
        .expect("a maximised lone tile shows the segment");

    cx.simulate_mouse_move(
        seg.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("tip-status-fullscreen-chord-alt+f")
            .is_some(),
        "the tooltip names the fullscreen key"
    );

    cx.simulate_mouse_down(
        seg.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        seg.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s
            .services
            .workspaces
            .active()
            .tree()
            .fullscreen()),
        None,
        "the click restores the layout"
    );
    assert!(cx.debug_bounds("status-fullscreen").is_none());
}

/// The empty-workspace hint (`"ctrl+k → Add a tile"`) paints
/// when there are no tiles. gpui's test API (`painted_quads`) has no way
/// to inspect painted *text* content directly, so this asserts what it
/// can see honestly: the hint's container div — tagged with a
/// test-only `debug_selector` (a no-op outside test builds, see the
/// comment at its call site in `Render for ShellView`) — actually
/// painted, with real (non-zero) bounds, and that painting it produced
/// at least one quad in the scene. This does not prove the glyphs
/// themselves rasterized correctly — a known limitation of gpui's
/// current test surface, not something this test can close.
#[gpui::test]
fn empty_workspace_paints_the_hint(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let tile_count = window.root(&mut cx).unwrap().read_with(&cx, |root, cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
            .read(cx)
            .services
            .workspaces
            .active()
            .tree()
            .tiles()
            .len()
    });
    assert_eq!(tile_count, 0, "sanity: workspace starts with no tiles");

    let hint_bounds = cx.debug_bounds("empty-hint");
    assert!(
        hint_bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
        "the empty-hint div should have painted with non-zero bounds, got {hint_bounds:?}"
    );

    let quads = cx.update(|window, _cx| window.painted_quads().len());
    assert!(
        quads > 0,
        "painting the empty-hint branch should have produced at least one quad"
    );
}

/// End-to-end: a real `ctrl+v` keystroke — the fixture layer's
/// `tile::add_rec_horizontal` — dispatched through gpui's own key-event
/// pipeline (not called directly), lands on `ShellView` and changes
/// workspace state. Exercises `convert_keystroke` -> `Matcher` ->
/// `ShellView::add_tile` wired the way the render path wires them.
#[gpui::test]
fn a_test_layer_add_keystroke_creates_the_first_tile(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    // Force a paint so the key-listener dispatch tree is registered
    // before we simulate a keystroke against it.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("ctrl-v");

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 1,
        "ctrl+v (the test layer's tile::add_rec_horizontal) should have \
         created the first tile on the empty starting workspace"
    );

    // The tile render path (Task 3) paints a background/border quad per
    // visible tile, not just text; a non-empty scene after the add is
    // cheap evidence the tiling surface actually drew something (the
    // geometry itself is tiling::tree's job, already unit-tested there).
    let quads_after_add = cx.update(|window, _cx| window.painted_quads().len());
    assert!(
        quads_after_add > 0,
        "expected the single tile to paint at least one quad"
    );
}

/// End-to-end: the direct focus bindings `mod+h`/`mod+l`
/// (Alt, the default `mod` alias) move focus between two tiles created
/// via the fixture layer's add bindings — `ctrl+h`
/// (`tile::add_rec_vertical`, which on the empty starting workspace
/// just opens the first tile per `Tree::split`'s documented "a split
/// doubles as open a tile" behavior) then `ctrl+v`
/// (`tile::add_rec_horizontal`, side by side), leaving focus on the new
/// (right) tile. `mod+h` must move focus to the left tile, and
/// `mod+l` back to the right one.
#[gpui::test]
fn add_then_mod_hl_moves_focus_between_tiles(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    cx.simulate_keystrokes("ctrl-h");
    cx.simulate_keystrokes("ctrl-v");

    let right_tile = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });

    cx.simulate_keystrokes("alt-h");
    let after_left = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });
    assert_ne!(
        after_left, right_tile,
        "mod+h (workspace::focus_left) should have moved focus off the right \
         tile"
    );

    cx.simulate_keystrokes("alt-l");
    let after_right = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });
    assert_eq!(
        after_right, right_tile,
        "mod+l (workspace::focus_right) should have moved focus back to the \
         right tile"
    );
}

/// End-to-end: `shift+left` (`workspace::resize_left`, a direct binding —
/// no mode) moves the divider adjacent to the focused tile leftward by
/// `tiling::RESIZE_STEP` — real key dispatch all the way to
/// `Tree::move_divider`. Focus here is the rightmost tile (no divider
/// on its right), so the only divider available is its left one; moving
/// it left widens the focused tile (the edge-flip case documented on
/// `Tree::move_divider`).
/// Moving focus must not move content. gpui sizes a box border-box, so
/// a tile whose ring grows from 1px to 2px on focus hands its occupant
/// a content box 1px smaller on every side — every row of a blotter
/// jogged a pixel whenever focus arrived or left. The chrome's
/// border + padding is constant, so the occupant's box is too.
#[gpui::test]
fn moving_focus_does_not_shift_tile_content(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let right = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    let left = shell.read_with(&cx, |s, _| {
        s.services
            .workspaces
            .active()
            .tree()
            .layout(Rect {
                x: 0.0,
                y: 0.0,
                w: 800.0,
                h: 600.0,
            })
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| *id != right)
            .expect("two tiles")
    });
    // `debug_bounds` takes `&'static str`; leak the dynamic selectors
    // (test-only, a few bytes).
    let left_sel: &'static str = Box::leak(format!("tile-content-{}", left.0).into_boxed_str());
    let right_sel: &'static str = Box::leak(format!("tile-content-{}", right.0).into_boxed_str());
    let left_before = cx.debug_bounds(left_sel).expect("left occupant painted");
    let right_before = cx.debug_bounds(right_sel).expect("right occupant painted");

    cx.simulate_keystrokes("alt-h");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile()),
        Some(left),
        "sanity: mod+h moved focus to the left tile"
    );
    let left_after = cx
        .debug_bounds(left_sel)
        .expect("left occupant still painted");
    let right_after = cx
        .debug_bounds(right_sel)
        .expect("right occupant still painted");
    assert_eq!(
        left_before, left_after,
        "the tile that GAINED focus must keep its content box"
    );
    assert_eq!(
        right_before, right_after,
        "the tile that LOST focus must keep its content box"
    );
}

#[gpui::test]
fn shift_left_keystroke_moves_the_left_divider(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    // Two tiles side by side (0.5/0.5 by default); focus lands on the
    // second (right) tile after the second split.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");

    cx.simulate_keystrokes("shift-left");

    let focused_width = shell.read_with(&cx, |shell, _| {
        let tree = shell.services.workspaces.active().tree();
        let rects = tree.layout(Rect::UNIT);
        rects
            .into_iter()
            .find(|(id, _)| Some(*id) == tree.focused())
            .unwrap()
            .1
            .w
    });
    assert!(
        (focused_width - (0.5 + crate::tiling::RESIZE_STEP)).abs() < 1e-4,
        "shift+left should have widened the focused (rightmost) tile by moving \
         its left divider left by RESIZE_STEP, got width {focused_width}"
    );
}

/// End-to-end (ledgered from 1b-ui T3): a real mouse-down at a
/// non-focused tile's on-screen coordinates focuses it, exercising the
/// `on_mouse_down` handler wired up in `Render for ShellView` (not the
/// keyboard path). Two tiles side by side; `mod+h` first moves focus
/// off the freshly-split (right) tile so the click has something to
/// change. The click point is derived from the same layout `render`
/// itself uses — `Tree::layout` over the tile area, offset by the
/// sidebar/toolbar chrome (`sidebar::width(window)`, `TITLE_BAR_HEIGHT`; see
/// CLAUDE.md's chrome-offset note) — rather than a hand-guessed pixel,
/// so the test tracks the real geometry instead of duplicating it.
#[gpui::test]
fn mouse_down_on_a_tile_focuses_it(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    // Two tiles side by side; move focus to the left tile so the right
    // tile (about to be clicked) starts out unfocused.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-h");

    let before_focus = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });

    // Same layout math as `Render for ShellView`: the tile area is the
    // viewport minus the toolbar, sidebar, and status bar.
    let (target_id, click_point) = cx.update(|window, cx| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::width(window)).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::height(window)).max(0.0);

        let rects = shell
            .read(cx)
            .services
            .workspaces
            .active()
            .tree()
            .layout(Rect {
                x: 0.0,
                y: 0.0,
                w: tile_width,
                h: content_height,
            });
        let (id, r) = rects
            .into_iter()
            .find(|(id, _)| Some(*id) != before_focus)
            .expect("a second, non-focused tile exists");
        let point = gpui::point(
            px(sidebar::width(window) + r.x + r.w / 2.0),
            px(toolbar_height + r.y + r.h / 2.0),
        );
        (id, point)
    });

    cx.simulate_mouse_down(click_point, MouseButton::Left, gpui::Modifiers::none());

    let after_focus = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });
    assert_eq!(
        after_focus,
        Some(target_id),
        "a mouse-down inside a non-focused tile should have focused it \
         (before: {before_focus:?}, clicked tile: {target_id:?}, after: {after_focus:?})"
    );
    assert_ne!(
        after_focus, before_focus,
        "the click should have changed which tile is focused"
    );
}

/// End-to-end: `ctrl+alt+right` (`workspace::move_right`, a direct
/// binding) swaps the focused tile with
/// its right neighbor, focus following the moved tile.
#[gpui::test]
fn ctrl_alt_right_keystroke_swaps_the_focused_tile_with_its_right_neighbor(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    // Two tiles side by side; focus is on the second (right) tile.
    // Move focus to the left tile first, then swap it rightward.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-h");

    let focused = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });
    let before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });

    cx.simulate_keystrokes("ctrl-alt-right");

    let after_focused = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });
    let after = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().layout(Rect::UNIT)
    });
    assert_eq!(
        after_focused, focused,
        "move_right keeps focus on the same TileId"
    );
    assert_ne!(
        before, after,
        "ctrl+alt+right should have swapped the two tiles' positions"
    );
}

/// End-to-end: `ctrl+w` (`workspace::close_tile`) closes the focused
/// tile.
#[gpui::test]
fn ctrl_w_keystroke_closes_the_focused_tile(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .tiles()
            .len()),
        2
    );

    cx.simulate_keystrokes("ctrl-w");

    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 1,
        "ctrl+w (workspace::close_tile) should have closed the focused tile"
    );
}

/// E2E test: closing a tile focuses the adjacent sibling (next in tree order),
/// not the first leaf. Build three side-by-side tiles by splitting right twice,
/// focus the middle one, close it, and assert focus is on the adjacent tile
/// (which would be different from the first leaf if the old rule applied).
#[gpui::test]
fn close_tile_focuses_adjacent_sibling(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    // Create three tiles with the fixture layer's ctrl+v
    // (tile::add_rec_horizontal, side by side).
    // First ctrl+v on the empty tree creates tile 1 and focuses it.
    // Second ctrl+v creates tile 2 right of tile 1 and focuses it.
    // Third ctrl+v creates tile 3 right of tile 2 and focuses it.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");

    // Verify we have three tiles.
    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(tile_count, 3, "should have created three tiles");

    // Record the tile ids in tree order before focusing the middle one.
    let tiles_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles()
    });
    assert_eq!(tiles_before.len(), 3);

    // After three splits, the focused tile is the last one (tiles_before[2]).
    // Focus the middle tile (at index 1) using focus_left (mod+h).
    cx.simulate_keystrokes("alt-h");

    let focused_tile = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });
    assert_eq!(
        focused_tile,
        Some(tiles_before[1]),
        "should have focused the middle tile (one position left)"
    );

    // Close the middle tile (ctrl+w).
    cx.simulate_keystrokes("ctrl-w");

    // Verify we have two tiles left.
    let remaining_tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(remaining_tile_count, 2, "should have two tiles after close");

    let tiles_after = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles()
    });
    // tiles_after should be [tiles_before[0], tiles_before[2]]
    assert_eq!(tiles_after, vec![tiles_before[0], tiles_before[2]]);

    // Assert that the focused tile is tiles_before[2] (the adjacent sibling in tree order).
    let focused_after_close = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused()
    });

    assert_eq!(
        focused_after_close,
        Some(tiles_before[2]),
        "closing the middle tile should focus the adjacent sibling (tiles_before[2]), \
         not the first leaf (tiles_before[0])"
    );
}

/// Spec 2026-09-08 add-tile §3.1: the shipped keymap has no split chord.
/// This shell is built on `BUILTIN_KEYMAP` alone (no test layer), so
/// `ctrl+v`/`ctrl+h` reach the matcher and match nothing.
#[gpui::test]
fn the_shipped_keymap_has_no_split_chord(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let (keymap, diags) = build_keymap(&[doc], default_mod(), &services.registry);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = keymap;
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-h");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().is_empty()),
        "neither key creates a tile any more"
    );
}
