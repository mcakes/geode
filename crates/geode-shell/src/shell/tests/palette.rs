//! The command palette: sequences, open/close, navigation, filtering,
//! rebinding, and dispatch of the selected row.

use super::*;

/// End-to-end: a theme picked from the palette by real keystrokes —
/// open, type its full name, `enter` — is applied through
/// `dispatch_palette_item`'s `Theme` arm. Exercises the same wiring as
/// `ctrl_v_keystroke_splits_the_active_workspace` above, through the
/// one keyboard path a theme change has now that `mod+shift+t` and its
/// light/dark toggle are retired (user ruling 2026-09-12).
#[gpui::test]
fn a_palette_theme_pick_by_keystrokes_applies_that_theme(cx: &mut gpui::TestAppContext) {
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

    assert_ne!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .theme
            .active_name()
            .to_string()),
        "Gruvbox Light",
        "sanity: the pick must change something"
    );

    cx.simulate_keystrokes("ctrl-k");
    assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));
    cx.simulate_input("Gruvbox Light");
    cx.simulate_keystrokes("enter");

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .theme
            .active_name()
            .to_string()),
        "Gruvbox Light",
        "the top-ranked row for a theme's full name is that theme, and enter applies it"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "enter closes the palette"
    );
}

/// Pressing the first `g` of a `"g g"` sequence leaves the matcher
/// pending (which the status bar renders as `"g"`) and the window still
/// draws cleanly — the status bar's pending-keystroke path is live end
/// to end through the real key-event pipeline. Task 8: the which-key
/// overlay (`whichkey-overlay`, same `debug_selector` test hook as the
/// empty-workspace hint) must be absent before any key is pressed and
/// painted with real bounds once the `g` is pending.
#[gpui::test]
fn first_key_of_a_sequence_leaves_pending_keys_and_still_draws(cx: &mut gpui::TestAppContext) {
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

    assert!(
        cx.debug_bounds("whichkey-overlay").is_none(),
        "the which-key overlay must not paint while nothing is pending"
    );

    cx.simulate_keystrokes("g");

    // The pending keystroke must not stall the render thread (spec
    // PHILOSOPHY.md): the status bar draws the same frame it renders in.
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

    let pending_len = shell.read_with(&cx, |shell, _| shell.matcher.pending().len());
    assert_eq!(
        pending_len, 1,
        "first 'g' of the 'g g' sequence should leave one pending keystroke"
    );

    let overlay_bounds = cx.debug_bounds("whichkey-overlay");
    assert!(
        overlay_bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
        "the which-key overlay should have painted with non-zero bounds while \
         pending, got {overlay_bounds:?}"
    );
}

/// Opening the palette while a keystroke sequence is pending cancels
/// that pending state (Task 6: `Matcher::cancel()` on palette open —
/// supersedes a 1b-ui deferred note that pending state would survive a
/// palette session). Pressing the first "g" of "g g", opening then
/// closing the palette, and pressing a fresh "g" must NOT complete the
/// original "g g" sequence — it starts a new one instead.
#[gpui::test]
fn opening_the_palette_cancels_a_pending_keystroke_sequence(cx: &mut gpui::TestAppContext) {
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

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    cx.simulate_keystrokes("g");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.matcher.pending().len()),
        1,
        "first 'g' of the 'g g' sequence should leave one pending keystroke"
    );

    cx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "ctrl-k should open the palette"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.matcher.pending().is_empty()),
        "opening the palette should cancel the pending 'g'"
    );

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "escape should close the palette"
    );

    cx.simulate_keystrokes("g");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.matcher.pending().len()),
        1,
        "a fresh 'g' after the palette closes should start a new pending \
         sequence, not silently complete the pre-palette one"
    );
}

/// End-to-end command palette flow (Task 6), through the real
/// key-event pipeline exactly like the tests above: `ctrl+k` opens it,
/// typing "rec: split" filters the list down to the three rows
/// `register_add_actions` registers for the "rec" kind ("Rec: Split",
/// "Rec: Split Horizontal", "Rec: Split Vertical" — the crate's
/// `Category: Verb` pattern, user ruling 2026-09-09 superseding spec
/// 2026-09-08 add-tile §3.2's original "Add <Kind>" wording). All three
/// contain the run as a subsequence, matched entirely within the shared
/// "Rec: Split" prefix, so all three score identically; the plain row
/// leads. Which one lands at index 0 is not a fuzzy-match property; it
/// is `PaletteState::filtered`'s stable sort preserving `build_items`'
/// input order, which is `ActionRegistry::iter()`'s `BTreeMap<ActionId,
/// _>` order — and `"tile::add_rec"` sorts ahead of
/// `"tile::add_rec_horizontal"`/`"…_vertical"` (a prefix is less than
/// what extends it). That tie-break is deterministic, so this test is
/// not flaky. Enter then dispatches the selected item through the
/// normal chain, closing the palette and adding the (until then empty)
/// active workspace's first tile — an add through the palette, end to
/// end.
#[gpui::test]
fn ctrl_k_opens_types_filters_and_enter_dispatches_the_selected_action(
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

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "palette starts closed"
    );

    cx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "ctrl-k (ctrl+k = palette::toggle) should have opened the palette"
    );

    cx.simulate_input("rec: split");
    let selected_title = shell.read_with(&cx, |shell, _| {
        shell
            .palette
            .as_ref()
            .and_then(PaletteState::selected_item)
            .map(|item| item.title())
    });
    assert_eq!(
        selected_title,
        Some("Rec: Split".to_string()),
        "typing \"rec: split\" should rank the plain \"Rec: Split\" row \
         first, ahead of its Horizontal/Vertical siblings, via the \
         registry's ActionId order and filtered()'s stable sort"
    );

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("enter");

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "enter should close the palette"
    );
    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 1,
        "enter on \"Rec: Split\" should have dispatched tile::add_rec \
         through the normal chain"
    );
}

/// End-to-end: Enter on a *theme* row (not an action) changes the
/// active theme, through the same real key-event pipeline as the
/// action-dispatch test above — the brief-mandated "theme item ->
/// `ThemeService::apply`" path had no direct test coverage before
/// this one; it was previously verified only by reading
/// `dispatch_palette_item`'s source.
///
/// Query "gruvbox" ranks "Theme: Gruvbox Dark" and "Theme: Gruvbox
/// Light" identically (both match the literal, fully-consecutive run
/// "gruvbox" right after the "Theme: " word boundary — same
/// computation as any other title sharing that whole run, so same
/// score); no other registered action or bundled theme title contains
/// "gruvbox" as a subsequence at all, bundled or not, so those two are
/// the entire tied-for-first set. As in the split-horizontal test
/// above, which one lands at index 0 is a deterministic tie-break —
/// `build_items` appends themes in `ThemeService::names()`'s sorted
/// order, and `"Gruvbox Dark" < "Gruvbox Light"` alphabetically — not
/// a property of the fuzzy match itself.
#[gpui::test]
fn ctrl_k_opens_types_filters_and_enter_dispatches_the_selected_theme(
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

    let before = shell.read_with(&cx, |shell, _| {
        shell.services.theme.active_name().to_string()
    });
    assert_ne!(
        before, "Gruvbox Dark",
        "the starting theme must differ from the target so the assertion \
         below actually proves something changed"
    );

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("gruvbox");

    let selected_title = shell.read_with(&cx, |shell, _| {
        shell
            .palette
            .as_ref()
            .and_then(PaletteState::selected_item)
            .map(|item| item.title())
    });
    assert_eq!(
        selected_title,
        Some("Theme: Gruvbox Dark".to_string()),
        "typing \"gruvbox\" should rank \"Theme: Gruvbox Dark\" first, ahead of \
         the equally-scored \"Theme: Gruvbox Light\", via ThemeService::names()'s \
         alphabetical order and filtered()'s stable sort"
    );

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("enter");

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "enter should close the palette"
    );
    let after = shell.read_with(&cx, |shell, _| {
        shell.services.theme.active_name().to_string()
    });
    assert_eq!(
        after, "Gruvbox Dark",
        "enter on \"Theme: Gruvbox Dark\" should have dispatched it through \
         ThemeService::apply, changing the active theme"
    );
}

/// The full-list scroll behavior this task adds: real `down` keystrokes
/// (not a direct `PaletteState::set_selected` call — this is the
/// actual key-event pipeline `handle_palette_key` drives) move the
/// selection well past `palette::VISIBLE_ROWS` (12) into rows that,
/// before this task, `render` would never have drawn (it truncated to
/// the top 12 filtered rows) and the palette's old `VISIBLE_ROWS` clamp
/// would never have let the selection reach. Also checks, via gpui's
/// test-only `debug_selector`/
/// `debug_bounds` (wired up in `palette::render`), that the selected
/// row's *painted* bounds actually land inside the scrollable list
/// container's bounds — proving the viewport followed the selection
/// (`ShellView::sync_palette_scroll`'s `ScrollHandle::scroll_to_item`)
/// rather than just moving an index nothing on screen reflects.
#[gpui::test]
fn arrow_down_past_visible_rows_advances_selection_and_scrolls_it_into_view(
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

    cx.simulate_keystrokes("ctrl-k");

    let total = shell.read_with(&cx, |shell, _| {
        shell.palette.as_ref().unwrap().filtered().len()
    });
    assert!(
        total > 20,
        "this test needs a registry+theme set with more than one \
         screenful of results (got {total}) to exercise scrolling past \
         row 12 at all"
    );

    // 20 real `down` keystrokes through the actual key-event pipeline —
    // well past the old MAX_VISIBLE=12 clamp.
    let downs = vec!["down"; 20].join(" ");
    cx.simulate_keystrokes(&downs);

    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        20,
        "20 real 'down' keystrokes should advance the selection to row \
         20, well past the old 12-row clamp"
    );

    cx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });

    let list_bounds = cx
        .debug_bounds("palette-list")
        .expect("the results list container should have painted");
    let row_bounds = cx
        .debug_bounds("palette-row-20")
        .expect("row 20 should still be part of the layout tree (no virtualization)");
    assert!(
        list_bounds.intersects(&row_bounds),
        "row 20 {row_bounds:?} should be scrolled into the visible list \
         viewport {list_bounds:?} after the selection moved onto it, not \
         left above/below it with only its index having changed"
    );
}

/// Esc closes the palette without dispatching anything — typing a
/// query that would otherwise match and select an action must not
/// leave any trace once the palette is dismissed. Also covers the
/// palette-input-polish task's focus contract: `ctrl+k` should have
/// focused `palette_input`'s real `FocusHandle` (proven directly, not
/// just inferred from typing having worked), and escape should hand
/// focus back to the shell root — the same "return focus on close"
/// story `escape_in_the_filter_input_returns_focus_to_the_shell_root`
/// proves for the toolbar's filter field.
#[gpui::test]
fn escape_closes_the_palette_without_dispatching(cx: &mut gpui::TestAppContext) {
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
    let shell_focus_handle = shell.read_with(&cx, |shell, _| shell.focus_handle.clone());
    let palette_input = shell.read_with(&cx, |shell, _| shell.palette_input.clone());
    let palette_input_focus_handle =
        palette_input.read_with(&cx, |state, cx| state.focus_handle(cx));

    cx.simulate_keystrokes("ctrl-k");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.update(|window, _cx| palette_input_focus_handle.is_focused(window)),
        "ctrl+k opening the palette should have focused its query Input"
    );

    cx.simulate_input("split");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("escape");

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "escape should close the palette"
    );
    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 0,
        "escape must not dispatch the item that was filtered/selected"
    );
    assert!(
        !cx.update(|window, _cx| palette_input_focus_handle.is_focused(window)),
        "escape should have moved focus off the palette's query input"
    );
    assert!(
        cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
        "escape should have returned focus to the shell root"
    );
}

/// Left/right arrow keys are consumed by the palette's query `Input` as
/// native caret movement (palette-input-polish task: "OS text input
/// stuff... from the component") and must not leak to the shell as
/// workspace chords — proven two ways: the caret actually moves inside
/// the input (`InputState::cursor`, not inferred from the query
/// staying the same), and the workspace stays untouched.
#[gpui::test]
fn left_and_right_arrows_move_the_input_caret_and_do_not_leak_to_the_shell(
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
    let palette_input = shell.read_with(&cx, |shell, _| shell.palette_input.clone());

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("abc");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        palette_input.read_with(&cx, |state, _cx| state.cursor()),
        3,
        "sanity: typing \"abc\" should leave the caret at the end"
    );

    cx.simulate_keystrokes("left");
    assert_eq!(
        palette_input.read_with(&cx, |state, _cx| state.cursor()),
        2,
        "left should move the caret back one position inside the input"
    );

    cx.simulate_keystrokes("right");
    assert_eq!(
        palette_input.read_with(&cx, |state, _cx| state.cursor()),
        3,
        "right should move the caret forward one position inside the input"
    );

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "the palette should still be open — arrows are caret movement, not close"
    );
    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 0,
        "left/right must not leak to the shell as workspace chords"
    );
}

/// ctrl+a is consumed by the palette's query `Input` (native "OS text
/// input stuff") rather than leaking to the shell — there is no
/// `ctrl+a` shell binding at all (checked against `defaults.rs`'s
/// `BUILTIN_KEYMAP`), so the meaningful proof is that the input
/// actually reacts to it and the query/palette are otherwise
/// untouched. Platform quirk, asserted directly rather than assumed
/// (gpui-component's own hardcoded bindings, `gpui-base-0.6.2/src/input/
/// base/state.rs`, not this crate's configurable mod-alias): on macOS
/// `ctrl+a` is bound to `MoveHome` (Emacs-style — `cmd+a` is
/// `SelectAll` there instead), everywhere else `ctrl+a` *is*
/// `SelectAll`. Both handlers fully consume the keystroke (neither
/// calls `cx.propagate()` — checked against the pinned release), so
/// "does not leak" holds on every platform CI builds this on (spec: “CI
/// runs on both macOS and Windows”); only the resulting caret/selection
/// differs.
#[gpui::test]
fn ctrl_a_is_consumed_by_the_input_and_does_not_leak_to_the_shell(cx: &mut gpui::TestAppContext) {
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
    let palette_input = shell.read_with(&cx, |shell, _| shell.palette_input.clone());

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("split");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("ctrl-a");

    #[cfg(target_os = "macos")]
    assert_eq!(
        palette_input.read_with(&cx, |state, _cx| state.cursor()),
        0,
        "on macOS, ctrl+a inside a gpui-component Input is MoveHome, not SelectAll"
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        palette_input.read_with(&cx, |state, _cx| state.selected_range()),
        0..5,
        "ctrl+a should select the whole \"split\" query inside the input"
    );

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "ctrl+a must not close the palette"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .palette
            .as_ref()
            .unwrap()
            .query()
            .to_string()),
        "split",
        "ctrl+a must not itself change the query text"
    );
}

/// A row click DISPATCHES that row — the mouse form of `enter` (user
/// request 2026-09-12, the palette's own half of §17.1 rule 2), through
/// the same `commit_selected` door the key uses. Real mouse coordinates,
/// recovered from `palette::render`'s `"palette-row-{i}"` debug selector
/// (same pattern `arrow_down_past_visible_rows_advances_selection_and_
/// scrolls_it_into_view` uses) rather than a direct `PaletteState` call,
/// so this exercises the real click -> `ShellView::render`'s
/// `on_row_click` -> dispatch path end to end.
///
/// The clicked row is deliberately NOT the highlighted one: with
/// "gruvbox" typed, row 0 is `Theme: Gruvbox Dark` (highlighted) and row
/// 1 is `Theme: Gruvbox Light`, so asserting the active theme became
/// Gruvbox *Light* proves the click dispatched the row under the mouse,
/// not whatever the keyboard had selected.
#[gpui::test]
fn click_on_a_result_row_dispatches_it_like_enter(cx: &mut gpui::TestAppContext) {
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

    let before = shell.read_with(&cx, |shell, _| {
        shell.services.theme.active_name().to_string()
    });
    assert_ne!(
        before, "Gruvbox Light",
        "the starting theme must differ from the target"
    );

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("gruvbox");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        0,
        "sanity: the highlight is on row 0 (Gruvbox Dark), not the row we click"
    );

    let row_bounds = cx
        .debug_bounds("palette-row-1")
        .expect("row 1 (Theme: Gruvbox Light) should have painted bounds to click into");
    let inside_row_1 = gpui::point(
        row_bounds.origin.x + gpui::px(10.0),
        row_bounds.origin.y + gpui::px(10.0),
    );
    cx.simulate_mouse_down(inside_row_1, MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "a row click dispatches, so the palette closes as it does on enter"
    );
    let after = shell.read_with(&cx, |shell, _| {
        shell.services.theme.active_name().to_string()
    });
    assert_eq!(
        after, "Gruvbox Light",
        "the click dispatched the row under the mouse (row 1), not the highlighted row 0"
    );
}

/// A mouse-down well outside the palette panel — on the transparent
/// click-catcher `ShellView::render` wraps the panel in — dismisses the
/// palette (design brief: "click anywhere outside the palette panel ->
/// dismisses the palette"). Same real-mouse-event structure and corner
/// point as `backdrop_click_closes_the_modal` (the panel is centered,
/// starting at least a third of the way down and inset horizontally,
/// so a point near the window's origin always falls on the catcher).
#[gpui::test]
fn click_outside_the_palette_panel_closes_it(cx: &mut gpui::TestAppContext) {
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

    cx.simulate_keystrokes("ctrl-k");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "sanity: ctrl-k should have opened the palette"
    );

    cx.simulate_mouse_down(
        gpui::point(gpui::px(4.0), gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "a mouse-down on the click-catcher, well outside the centered \
         panel, should have closed the palette"
    );
}

/// A mouse-down INSIDE the panel must NOT close the palette — the
/// panel's own `on_mouse_down` (`palette::render`) stops propagation
/// before the same bubbling event ever reaches the click-catcher's
/// close handler underneath it. Mirrors `panel_click_does_not_close_
/// the_modal` exactly, one layer down (palette panel vs. modal panel).
#[gpui::test]
fn click_on_the_palette_panel_does_not_close_it(cx: &mut gpui::TestAppContext) {
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

    cx.simulate_keystrokes("ctrl-k");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "sanity: ctrl-k should have opened the palette"
    );

    let panel_bounds = cx
        .debug_bounds("palette-panel")
        .expect("the palette panel should have painted bounds to click inside");
    let inside_panel = gpui::point(
        panel_bounds.origin.x + gpui::px(10.0),
        panel_bounds.origin.y + gpui::px(10.0),
    );

    cx.simulate_mouse_down(inside_panel, MouseButton::Left, gpui::Modifiers::none());

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "a mouse-down inside the panel must not close the palette"
    );
}

/// A shell chord (`ctrl+w` = `workspace::close_tile`) must not
/// fire while the palette is open — proven with a real tile actually
/// present to close (an empty workspace closing "a tile" that was
/// never there wouldn't distinguish "correctly swallowed" from
/// "there was nothing to close anyway").
#[gpui::test]
fn shell_chord_does_not_fire_while_the_palette_is_open(cx: &mut gpui::TestAppContext) {
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

    // Create a real tile (the test layer's ctrl+v = tile::add_rec_
    // horizontal) so there is something for a leaked ctrl+w to close.
    cx.simulate_keystrokes("ctrl-v");
    let tile_count_before = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count_before, 1,
        "sanity: ctrl+v should have added a tile"
    );

    cx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "sanity: ctrl-k should have opened the palette"
    );

    cx.simulate_keystrokes("ctrl-w");

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "ctrl+w must not close the palette either"
    );
    let tile_count_after = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count_after, 1,
        "ctrl+w (workspace::close_tile) must not fire while the \
         palette is open — the tile from before must still be there"
    );
}

/// End-to-end: palette selection wraps at both ends. Opening the palette
/// and pressing up once (from index 0) wraps to the last filtered item.
#[gpui::test]
fn palette_selection_wraps_up_from_index_zero(cx: &mut gpui::TestAppContext) {
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

    // Open the palette with ctrl+k
    cx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "ctrl-k should open the palette"
    );

    // Get the filtered list length
    let filtered_len = shell.read_with(&cx, |shell, _| {
        shell
            .palette
            .as_ref()
            .map(|p| p.filtered().len())
            .unwrap_or(0)
    });
    assert!(
        filtered_len > 0,
        "palette should have at least one item when no filter is active"
    );

    // Press up once from index 0
    cx.simulate_keystrokes("up");

    // Verify we wrapped to the last item
    let selected = shell.read_with(&cx, |shell, _| {
        shell.palette.as_ref().map(|p| p.selected()).unwrap_or(0)
    });
    assert_eq!(
        selected,
        filtered_len - 1,
        "pressing up at index 0 should wrap to the last filtered item"
    );
}

/// The palette gains the dialogs' larger steps (spec §3): ctrl+d/u
/// move ±5, ctrl+f/b and pageup/pagedown ±10.
#[gpui::test]
fn the_palette_takes_the_larger_navigation_steps(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "palette::toggle");
    let len = shell.read_with(&cx, |shell, _| {
        shell.palette.as_ref().unwrap().filtered().len()
    });
    assert!(
        len >= 16,
        "sanity: the last assertion below (ctrl+d then ctrl+f, landing \
         at 15) needs at least 16 rows or it fails on ITS OWN clamp \
         instead of proving the step size — a looser bound here would \
         fail at the wrong assertion with a confusing message, got {len}"
    );

    cx.simulate_keystrokes("ctrl-d");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        5,
        "ctrl+d moves down 5"
    );
    cx.simulate_keystrokes("ctrl-f");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        15,
        "ctrl+f moves down 10 more"
    );
    cx.simulate_keystrokes("ctrl-u");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        10,
        "ctrl+u moves back 5"
    );
    cx.simulate_keystrokes("pageup");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        0,
        "pageup is ctrl+b's alias: back 10"
    );
}

/// The split this change deliberately preserves (spec §3): the new
/// larger steps clamp, while the ±1 keys keep wrapping. This test and
/// its partner above (`the_palette_takes_the_larger_navigation_steps`)
/// are jointly, not individually, sufficient: that one alone would
/// pass against a `nav_command` that returned `Move(0)` for every key
/// (every assertion there stays put or moves by the size actually
/// under test, never wraps), and this one alone would pass against a
/// palette that ignored the new keys entirely (every clamp assertion
/// here is also satisfied by "nothing moved"). Together they pin both
/// that the new keys move the selection by the right amount AND that
/// the amount clamps rather than wraps — do not delete one believing
/// the other still covers navigation.
#[gpui::test]
fn palette_big_steps_clamp_while_arrows_still_wrap(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "palette::toggle");
    let len = shell.read_with(&cx, |shell, _| {
        shell.palette.as_ref().unwrap().filtered().len()
    });

    cx.simulate_keystrokes("ctrl-u");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        0,
        "ctrl+u at the top clamps — a page jump must not teleport to the end"
    );

    cx.simulate_keystrokes("up");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        len - 1,
        "up at the top still wraps to the last result, exactly as before"
    );

    cx.simulate_keystrokes("ctrl-f");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
        len - 1,
        "and ctrl+f at the bottom clamps"
    );
}

/// Typing still reaches the query field: the new arm must not swallow
/// characters on their way to the input.
#[gpui::test]
fn the_new_palette_arm_does_not_intercept_typing(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "palette::toggle");
    cx.simulate_input("theme");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .palette
            .as_ref()
            .unwrap()
            .query()
            .to_string()),
        "theme"
    );
}

/// Layers a user binding on top of the fixture keymap that rebinds
/// `ctrl+k` (BUILTIN_KEYMAP's `palette::toggle` key) to
/// `workspace::close_tile` instead. Per the layering contract
/// (last-exact-match-wins), this must fully shadow the builtin
/// `palette::toggle` binding for that key.
fn test_services_with_ctrl_k_rebound_to_close_tile() -> ShellServices {
    let mut services = test_services();
    let user_doc = LayerDoc {
        layer: geode_core::config::Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: "[[bindings]]\n[bindings.keys]\n\"ctrl+k\" = \"workspace::close_tile\"\n"
            .parse()
            .unwrap(),
    };
    services.keymap = test_keymap(&services.registry, &[user_doc]);
    services
}

/// Regression for `is_palette_toggle` respecting keymap layering
/// (last-exact-match-wins, spec §3.4): a user layer rebinding `ctrl+k`
/// away from `palette::toggle` must mean pressing it does NOT open the
/// palette — the pre-matcher intercept in `handle_key_down` must not
/// fire just because *some* binding for that key, anywhere in the
/// keymap, happens to be `palette::toggle`. The rebound action
/// (`workspace::close_tile`) must dispatch instead, through the
/// normal matcher path, proving the key was fully handed over rather
/// than merely swallowed.
#[gpui::test]
fn user_layer_rebinding_ctrl_k_prevents_palette_open_and_dispatches_rebound_action(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| {
                    ShellView::new(
                        test_services_with_ctrl_k_rebound_to_close_tile(),
                        None,
                        None,
                        window,
                        cx,
                    )
                });
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    // Something for the rebound close-tile to actually close (the test
    // layer's ctrl+v = tile::add_rec_horizontal).
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
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .workspaces
            .active()
            .tree()
            .tiles()
            .len()),
        1,
        "sanity: ctrl+v should have added a tile"
    );

    cx.simulate_keystrokes("ctrl-k");

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "a user layer rebinding ctrl+k away from palette::toggle must shadow the \
         builtin binding — the palette must not open"
    );
    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 0,
        "ctrl-k should have dispatched the rebound workspace::close_tile \
         action through the normal matcher path"
    );
}

/// Regression for `dispatch_palette_item`: selecting the
/// `palette::toggle` row from inside the palette itself is a true
/// toggle — the palette closes (Enter already did that) and must stay
/// closed, not reopen. Filters straight down to that one row via its
/// exact title so the test doesn't depend on where it ranks unfiltered.
#[gpui::test]
fn enter_on_the_palette_toggle_row_closes_the_palette_without_reopening(
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

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("Toggle command palette");

    let selected_title = shell.read_with(&cx, |shell, _| {
        shell
            .palette
            .as_ref()
            .and_then(PaletteState::selected_item)
            .map(|item| item.title())
    });
    assert_eq!(
        selected_title,
        Some("Toggle command palette".to_string()),
        "the query should have filtered down to exactly that row"
    );

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("enter");

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "enter on the palette::toggle row must leave the palette closed, not \
         reopen it"
    );
}

// --- Phase 4a §3.9: saved scopes in the palette ----------------------

/// A saved scope appears as `Scope: {name}` (category "Scope") and
/// selecting it loads it onto the frame via `Frame::load_scope`, bumping
/// the scope version exactly once.
#[gpui::test]
fn a_saved_scope_appears_in_the_palette_and_selecting_it_loads_it(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
    )
    .unwrap();
    let scopes =
        LayerDoc::builtin("scopes", "[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n").unwrap();
    // Finding 1 (post-display-fixes piece 1): this fixture used to load
    // `config` from a non-empty builtin while leaving `test_services()`'s
    // empty `builtin` in place — harmless today (nothing here reloads),
    // but a false pairing all the same. `config_and_builtin` makes it
    // impossible to get wrong.
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
            scopes,
        ],
        ..ConfigSources::default()
    });

    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    let eu_scope = geode_core::scope::Scope {
        dimensions: vec![geode_core::scope::DimensionSelection {
            column: "book".into(),
            values: vec!["BK001".into()],
        }],
        ..geode_core::scope::Scope::default()
    };
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame().read(cx).saved_scopes().clone())["eu"],
        eu_scope,
        "sanity: the scope loaded from config before the palette is even opened"
    );

    let v0 = shell.read_with(&cx, |s, cx| s.frame().read(cx).versions().scope);

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("Scope: eu");

    let selected_title = shell.read_with(&cx, |shell, _| {
        shell
            .palette
            .as_ref()
            .and_then(PaletteState::selected_item)
            .map(|item| item.title())
    });
    assert_eq!(selected_title, Some("Scope: eu".to_string()));

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("enter");

    assert!(shell.read_with(&cx, |shell, _| shell.palette.is_none()));
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame().read(cx).scope().clone()),
        eu_scope,
        "selecting the row must load the saved scope onto the frame"
    );
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame().read(cx).versions().scope),
        v0 + 1,
        "loading the scope must bump the scope version exactly once"
    );
}

// -- usage ranking ------------------------------------------------------

/// [`open_shell`] plus the downcast shell entity, for the tests below.
fn open_ranked_shell(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
) -> (gpui::VisualTestContext, Entity<ShellView>) {
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    (cx, shell)
}

fn first_palette_title(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(cx, |shell, _| {
        shell
            .palette
            .as_ref()
            .and_then(PaletteState::selected_item)
            .map(|item| item.title())
    })
}

/// Dispatching a row from the palette records a use of it, and the next
/// open lists it first on an empty query — the whole "brain-reading"
/// loop, through the real key pipeline.
#[gpui::test]
fn a_palette_dispatch_is_recorded_and_ranks_first_on_the_next_open(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = open_ranked_shell(cx, test_services());

    cx.simulate_keystrokes("ctrl-k");
    assert_ne!(
        first_palette_title(&shell, &cx).as_deref(),
        Some("Rec: Split Vertical"),
        "the fixture's registry order must not already lead with the row under test"
    );
    cx.simulate_input("rec: split vertical");
    assert_eq!(
        first_palette_title(&shell, &cx).as_deref(),
        Some("Rec: Split Vertical")
    );
    cx.simulate_keystrokes("enter");
    assert!(shell.read_with(&cx, |shell, _| shell.palette.is_none()));

    let record = shell.read_with(&cx, |shell, _| {
        shell
            .palette_usage
            .get("action:tile::add_rec_vertical")
            .copied()
    });
    assert_eq!(record.map(|r| r.count), Some(1), "{record:?}");

    cx.simulate_keystrokes("ctrl-k");
    assert_eq!(
        first_palette_title(&shell, &cx).as_deref(),
        Some("Rec: Split Vertical"),
        "the row just used must lead an empty query"
    );
}

/// Closing the palette with `palette::toggle` itself (its own row, or
/// `ctrl+k` again) is not a use of anything.
#[gpui::test]
fn choosing_the_palette_toggle_row_records_nothing(cx: &mut gpui::TestAppContext) {
    let (mut cx, shell) = open_ranked_shell(cx, test_services());
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("toggle command palette");
    assert_eq!(
        first_palette_title(&shell, &cx).as_deref(),
        Some("Toggle command palette")
    );
    cx.simulate_keystrokes("enter");
    assert!(shell.read_with(&cx, |shell, _| shell.palette_usage.is_empty()));
}

/// A restored history (`ShellServices::restored_palette_usage`, from
/// `session.toml`) ranks the very first open of a new process.
#[gpui::test]
fn restored_usage_ranks_the_first_open(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    services
        .restored_palette_usage
        .record("action:tile::add_rec_vertical", 1_800_000_000);
    let (mut cx, shell) = open_ranked_shell(cx, services);
    cx.simulate_keystrokes("ctrl-k");
    assert_eq!(
        first_palette_title(&shell, &cx).as_deref(),
        Some("Rec: Split Vertical")
    );
}

/// A palette dispatch that mutates nothing else (a theme row) still
/// dirties the session flush, and the flushed text carries the usage
/// table — so a restart forgets nothing.
#[gpui::test]
fn a_palette_dispatch_reaches_the_session_flush(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");
    let (mut cx, shell) =
        open_ranked_shell(cx, super::session::test_services_with_session(session_path));

    let clean = shell.update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx));
    assert!(clean.is_none(), "a fresh shell has nothing to flush");

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("theme: gruvbox dark");
    assert_eq!(
        first_palette_title(&shell, &cx).as_deref(),
        Some("Theme: Gruvbox Dark")
    );
    cx.simulate_keystrokes("enter");

    let pending = shell.update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx));
    let (_, text) = pending.expect("a palette dispatch must dirty the session flush");
    assert!(text.contains("[palette.usage"), "{text}");
    assert!(text.contains("\"theme:Gruvbox Dark\""), "{text}");

    let again = shell.update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx));
    assert!(again.is_none(), "one dispatch flushes once");
}

/// Spec §20.5: `tab` inside the palette is reclaimed so gpui-component's
/// `Root` cannot cycle focus off the query field while the palette is
/// open — `dialog::init_reclaimed_keybindings` binds it to `NoAction` in
/// the `GeodePalette` context, the modal's own treatment.
#[gpui::test]
fn tab_in_the_palette_leaves_the_query_field_focused(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    let focused = |cx: &mut gpui::VisualTestContext| {
        cx.update(|window, cx| {
            shell
                .read(cx)
                .palette_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        })
    };
    assert!(focused(&mut cx), "the palette opens with its field focused");
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(
        focused(&mut cx),
        "tab must not move focus off the palette's field"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.palette.is_some()),
        "and the palette is still open"
    );
}

/// Keys on screen paint as gpui-component's `Kbd` (`shell::kbd`), whose
/// `kbd:{keystroke}` selector is the probe. A `q w` binding keeps the
/// routes apart: with `q` pending, the status strip alone paints `kbd:q`
/// (the pending key) and the which-key overlay alone paints `kbd:w` (the
/// continuation); the palette's binding column paints both. Nothing else
/// binds a `q` sequence, so no other surface paints either chip.
#[gpui::test]
fn pending_keys_which_key_and_palette_bindings_paint_as_kbd(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let mut services = test_services();
    services
        .registry
        .register(crate::actions::ActionDef {
            id: crate::actions::ActionId("test::qw".to_string()),
            title: "Test qw".to_string(),
            category: "Test".to_string(),
        })
        .unwrap();
    let user_doc = geode_core::config::LayerDoc {
        layer: geode_core::config::Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: "[[bindings]]\n[bindings.keys]\n\"q w\" = \"test::qw\"\n"
            .parse()
            .unwrap(),
    };
    services.keymap = test_keymap(&services.registry, &[user_doc]);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        })
    };
    draw(&mut cx);
    assert!(
        cx.debug_bounds("kbd:q").is_none() && cx.debug_bounds("kbd:w").is_none(),
        "sanity: no key painted yet"
    );

    cx.simulate_keystrokes("q");
    draw(&mut cx);
    assert!(
        cx.debug_bounds("kbd:q").is_some(),
        "the status strip paints the pending `q` as a Kbd chip"
    );
    assert!(
        cx.debug_bounds("kbd:w").is_some(),
        "the which-key overlay paints the continuation `w` as a Kbd chip"
    );
    cx.simulate_keystrokes("escape");
    draw(&mut cx);
    assert!(
        cx.debug_bounds("kbd:q").is_none() && cx.debug_bounds("kbd:w").is_none(),
        "sanity: both chips are gone once the sequence is cancelled"
    );

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("Test qw");
    draw(&mut cx);
    assert!(
        cx.debug_bounds("kbd:q").is_some() && cx.debug_bounds("kbd:w").is_some(),
        "the palette's binding column paints `q w` as Kbd chips"
    );
}
