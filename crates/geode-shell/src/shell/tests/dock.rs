//! Dock regions: the which-key gate on the divider strips, moving a
//! tile into and out of a dock, and dock-aware layout carve-up.

use super::*;

/// Review fix 4: the which-key hint paints a solid panel with no
/// occlusion and no handlers, so a mouse-down through it would fall
/// onto a strip beneath — a pending keystroke sequence must therefore
/// gate the strips off exactly like the palette/modal overlays do,
/// and completing the sequence brings them back.
#[gpui::test]
fn a_pending_key_sequence_gates_the_divider_strips(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);
    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| {
                    ShellView::new(test_services_with_gg_binding(), None, None, window, cx)
                });
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("divider-strip-0").is_some(),
        "two tiles paint their splitter strip"
    );

    cx.simulate_keystrokes("g"); // first key of the "g g" sequence
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("whichkey-overlay").is_some(),
        "sanity: the which-key overlay is up while the sequence is pending"
    );
    assert!(
        cx.debug_bounds("divider-strip-0").is_none(),
        "a pending sequence (which-key showing) must gate the strips off"
    );

    cx.simulate_keystrokes("g"); // completes the sequence
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("divider-strip-0").is_some(),
        "resolving the sequence brings the strips back"
    );
}

/// End-to-end: `ctrl+[` (`dock::toggle_left`) through gpui's real key
/// pipeline toggles the left dock's visibility both ways, and the
/// visible-but-empty dock paints its "move a tile here" hint (asserted
/// via `debug_bounds`, same honest limitation as the empty-workspace
/// hint test above — text content itself can't be inspected).
#[gpui::test]
fn ctrl_bracket_keystroke_toggles_the_left_dock_and_paints_its_hint(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);

    cx.simulate_keystrokes("ctrl-v"); // one tile so the workspace isn't bare
    cx.simulate_keystrokes("ctrl-[");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let (visible, region) = shell.read_with(&cx, |shell, _| {
        let ws = shell.services.workspaces.active();
        (
            ws.docks().get(crate::tiling::DockSide::Left).visible(),
            ws.region(),
        )
    });
    assert!(visible, "ctrl+[ should have shown the left dock");
    assert_eq!(
        region,
        crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left),
        "showing a dock focuses it (spec 2026-09-08 add-tile §8), empty or not"
    );

    let hint_bounds = cx.debug_bounds("dock-empty-hint-left");
    assert!(
        hint_bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
        "the empty left dock should have painted its hint, got {hint_bounds:?}"
    );

    cx.simulate_keystrokes("ctrl-[");
    let visible = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .workspaces
            .active()
            .docks()
            .get(crate::tiling::DockSide::Left)
            .visible()
    });
    assert!(
        !visible,
        "a second ctrl+[ should have hidden the dock again"
    );
}

/// End-to-end: the move-to-dock chord. The user presses ctrl+shift+[,
/// but both real platforms deliver that as key `{` with the shift
/// modifier CLEARED (see BUILTIN_KEYMAP's doc comment for the verified
/// platform-source evidence), so the simulated keystroke is `ctrl-{` —
/// which gpui's test parser produces in exactly that platform shape
/// (key `{`, no shift). This test pins that the `"ctrl+{"` binding
/// matches it end to end: the focused tile leaves the tree, parks in
/// the left dock, focus follows, and the session goes dirty. A second
/// ctrl+{ sends it back into the tree and auto-hides the dock.
#[gpui::test]
fn ctrl_brace_keystroke_moves_the_tile_to_the_left_dock_and_back(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);

    cx.simulate_keystrokes("ctrl-v");
    let tile = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().focused().unwrap()
    });
    // Clear the dirty flag left by the split so the assertion below
    // isolates the dock move's own dirtying.
    shell.update(&mut cx, |shell, _| shell.session_dirty = false);

    cx.simulate_keystrokes("ctrl-{");

    shell.read_with(&cx, |shell, _| {
        let ws = shell.services.workspaces.active();
        assert!(ws.tree().is_empty(), "the tile should have left the tree");
        let dock = ws.docks().get(crate::tiling::DockSide::Left);
        assert_eq!(dock.tree().tiles(), vec![tile]);
        assert!(dock.visible(), "the dock auto-shows");
        assert_eq!(
            ws.region(),
            crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left)
        );
        assert!(
            shell.session_dirty,
            "a handled dock action must mark the session dirty"
        );
    });

    cx.simulate_keystrokes("ctrl-{");

    shell.read_with(&cx, |shell, _| {
        let ws = shell.services.workspaces.active();
        assert_eq!(ws.tree().tiles(), vec![tile], "the tile returned");
        assert_eq!(ws.tree().focused(), Some(tile));
        assert_eq!(ws.region(), crate::tiling::FocusRegion::Main);
        let dock = ws.docks().get(crate::tiling::DockSide::Left);
        assert!(dock.tree().is_empty());
        assert!(!dock.visible(), "the emptied dock auto-hides");
    });
}

/// End-to-end (dock-trees task): an add lands *inside* a focused dock
/// through gpui's real key pipeline. A first tile (the test layer's
/// ctrl+v = `tile::add_rec_horizontal`) is parked via ctrl+{, then a
/// second add splits within the dock's tree (the old build refused
/// this) — two tiles in the dock, session dirty — and ctrl+w closes
/// one, leaving the dock visible with the survivor.
#[gpui::test]
fn adds_and_close_operate_inside_a_focused_dock(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);

    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-{"); // tile → left dock, dock focused
    shell.update(&mut cx, |shell, _| shell.session_dirty = false);

    cx.simulate_keystrokes("ctrl-v"); // add inside the dock
    shell.read_with(&cx, |shell, _| {
        let ws = shell.services.workspaces.active();
        let dock = ws.docks().get(crate::tiling::DockSide::Left);
        assert_eq!(
            dock.tree().tiles().len(),
            2,
            "the add must land within the focused dock's tree"
        );
        assert_eq!(
            ws.region(),
            crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left)
        );
        assert!(ws.tree().is_empty(), "the main tree must stay untouched");
        assert!(
            shell.session_dirty,
            "a dock-tree split must mark the session dirty"
        );
    });

    cx.simulate_keystrokes("ctrl-w"); // close the focused dock tile
    shell.read_with(&cx, |shell, _| {
        let ws = shell.services.workspaces.active();
        let dock = ws.docks().get(crate::tiling::DockSide::Left);
        assert_eq!(dock.tree().tiles().len(), 1);
        assert!(dock.visible(), "a still-occupied dock must not auto-hide");
        assert_eq!(
            ws.region(),
            crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left),
            "focus stays in the dock while it has tiles"
        );
    });
}

/// End-to-end: a literal shift-held `[` must NOT trigger the move
/// binding — the platforms never deliver that shape (they deliver
/// `{`), and gpui's test dispatcher faithfully reproduces whatever
/// shape it's given, so this pins that the binding was NOT written as
/// `"ctrl+shift+["` (which would match only this never-occurring
/// event and nothing real).
#[gpui::test]
fn a_literal_ctrl_shift_bracket_shape_does_not_move_the_tile(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-shift-["); // key "[", shift=true: not a real platform shape
    shell.read_with(&cx, |shell, _| {
        let ws = shell.services.workspaces.active();
        assert!(
            !ws.tree().is_empty(),
            "the unmatched keystroke must not have moved the tile"
        );
        assert!(
            ws.docks()
                .get(crate::tiling::DockSide::Left)
                .tree()
                .is_empty()
        );
    });
}

/// Spec 2026-09-08 add-tile §7.3: the empty-tree hint is now one hint
/// whatever holds focus — `ctrl+k` adds a tile into the focused region,
/// so the same advice is true from a focused dock as from the main
/// tree, and the old state-aware "return" variants are gone. Same
/// `debug_bounds` honesty limits as the other hint tests: selectors,
/// not text.
#[gpui::test]
fn empty_tree_hint_paints_whichever_region_holds_focus(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-{"); // only tile → left dock, tree empty, dock focused
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    shell.read_with(&cx, |shell, _| {
        let ws = shell.services.workspaces.active();
        assert!(ws.tree().is_empty(), "sanity: the tree emptied");
        assert_eq!(
            ws.region(),
            crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left)
        );
    });

    let hint = cx.debug_bounds("empty-hint");
    assert!(
        hint.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
        "the dock-focused empty tree still paints the add-a-tile hint, got {hint:?}"
    );

    // Back in Main over a truly empty workspace (move the tile back,
    // then close it), the same hint paints.
    cx.simulate_keystrokes("ctrl-{"); // tile returns to the tree
    cx.simulate_keystrokes("ctrl-w"); // close it: empty workspace, Main
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let hint = cx.debug_bounds("empty-hint");
    assert!(
        hint.is_some_and(|b| b.size.width > px(0.0)),
        "with Main focused the same hint paints, got {hint:?}"
    );
}

/// End-to-end geometry: with a tile parked in the left dock and one in
/// the tree, the surface carves the dock column out of the tree's area
/// — the tree's layout (the same call `render` makes) starts at the
/// dock's right edge, and the whole pass still calls `Tree::layout`
/// once (structural: this asserts the observable carve-up, the
/// call-count discipline is by construction in `render`).
#[gpui::test]
fn a_visible_left_dock_carves_its_column_out_of_the_tree_area(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = dock_test_shell(cx);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-{"); // right tile → left dock
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.update(|window, cx| {
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: tile_width,
            h: content_height,
        };
        let shell = shell.read(cx);
        let ws = shell.services.workspaces.active();
        let (tree_area, dock_rects) = crate::tiling::dock_layout(ws.docks(), area);
        assert_eq!(dock_rects.len(), 1);
        let (side, dock_rect) = dock_rects[0];
        assert_eq!(side, crate::tiling::DockSide::Left);
        let expected_w = crate::tiling::DOCK_DEFAULT_SIZE * tile_width;
        assert!(
            (dock_rect.w - expected_w).abs() < 1e-3,
            "dock width {} should be size*area_width {}",
            dock_rect.w,
            expected_w
        );
        assert!((dock_rect.h - content_height).abs() < 1e-3, "full height");
        let rects = ws.tree().layout(tree_area);
        assert_eq!(rects.len(), 1);
        let tree_tile = rects[0].1;
        assert!(
            (tree_tile.x - dock_rect.w).abs() < 1e-3,
            "the tree starts where the dock column ends: {} vs {}",
            tree_tile.x,
            dock_rect.w
        );
        assert!(
            (tree_tile.w - (tile_width - dock_rect.w)).abs() < 1e-3,
            "the tree gets the rest of the width"
        );
    });
}
