//! The keybindings dialog: rows, filtering, selection, listening for a
//! new binding, and persisting it to the user keymap file.

use super::*;

// --- Part B: the keybinding dialog -----------------------------------

/// `keybindings::open` dispatch paints the modal with one row per
/// registered action — mirrors `settings_open_opens_the_modal`'s own
/// "prove it painted, not just that a flag flipped" standard.
#[gpui::test]
fn keybindings_open_paints_the_modal_with_rows(cx: &mut gpui::TestAppContext) {
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

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("keybindings::open".to_string()), None, window, cx);
        });
    });

    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "keybindings::open should have set ShellView's own modal state"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.keybindings.is_some()),
        "keybindings::open should have set ShellView's own keybindings state"
    );

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let list_bounds = cx.debug_bounds("keybindings-list");
    assert!(
        list_bounds.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "the row list should have painted with non-zero bounds, got {list_bounds:?}"
    );
    let row_0_bounds = cx.debug_bounds("keybindings-row-0");
    assert!(
        row_0_bounds.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "the first row should have painted with non-zero bounds, got {row_0_bounds:?}"
    );
}

/// Opening the dialog focuses the shared filter, so the first
/// character typed filters instead of falling on the floor.
#[gpui::test]
fn opening_the_keybindings_dialog_focuses_the_filter(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    assert!(
        shell.read_with(&cx, |shell, _| shell.keybindings.is_some()),
        "sanity: keybindings::open should have opened the dialog"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the filter must own focus the moment the dialog opens"
    );
}

/// The retired vim motion is now plain text: `j` types a `j` and
/// leaves the selection where it was.
#[gpui::test]
fn typing_j_filters_rather_than_moving_the_selection(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_input("j");
    let (query, selected) = shell.read_with(&cx, |shell, _| {
        let state = shell.keybindings.as_ref().unwrap();
        (state.query.clone(), state.selected)
    });
    assert_eq!(query, "j", "j must reach the filter as text");
    assert_eq!(selected, 0, "j must not move the selection any more");
}

/// Arrow and ctrl motions still move the selection, and do it without
/// disturbing the filter's focus or its text.
#[gpui::test]
fn arrows_and_ctrl_motions_move_the_selection(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("down down");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected),
        2,
        "two downs should land on the third row"
    );
    cx.simulate_keystrokes("ctrl-u");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected),
        0,
        "ctrl+u steps back 5, clamped at the top of the list"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .keybindings
            .as_ref()
            .unwrap()
            .query
            .is_empty()),
        "navigation must not put anything in the filter"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "navigation must not steal focus from the filter"
    );
}

/// `tab` is reserved here (it steps values in the settings dialog,
/// which has nothing to step) and must be genuinely inert: with a
/// focused `Input`, `handle_key` returning `false` for it would NOT
/// make it inert — the key would continue to the filter's own
/// text-input phase, and `InputState::normalize_input` strips only
/// `\n`/`\r`, not `\t`, so it would land as a literal tab character
/// and collapse the list to "no matches". `handle_key` claims it
/// instead (see its own doc comment).
#[gpui::test]
fn tab_is_reserved_and_leaves_the_keybindings_dialog_untouched(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("down down");
    let selected_before =
        shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected);
    assert_eq!(
        selected_before, 2,
        "sanity: two downs land on the third row"
    );

    cx.simulate_keystrokes("tab");

    let (query, selected) = shell.read_with(&cx, |shell, _| {
        let state = shell.keybindings.as_ref().unwrap();
        (state.query.clone(), state.selected)
    });
    assert!(
        query.is_empty(),
        "tab must not leak a literal tab character into the filter"
    );
    assert_eq!(selected, selected_before, "tab must not move the selection");
}

/// Enter blurs the filter so rebind capture sees raw keys: the letter
/// lands in the pending binding, NOT in the query.
#[gpui::test]
fn enter_starts_listening_and_a_letter_is_captured_not_typed(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("enter");
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_some()),
        "enter should start listening on the selected row"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "listening must blur the filter, or the capture can never see a letter"
    );

    cx.simulate_input("j");
    let (pending, query) = shell.read_with(&cx, |shell, _| {
        let state = shell.keybindings.as_ref().unwrap();
        (state.listening.clone(), state.query.clone())
    });
    assert_eq!(
        pending.as_deref().map(<[_]>::len),
        Some(1),
        "the letter must be captured as the new binding"
    );
    assert!(
        query.is_empty(),
        "and must NOT have been typed into the filter"
    );
}

/// Escape cancels the capture, refocuses the filter, and leaves both
/// the query and the dialog itself alone.
#[gpui::test]
fn escape_cancels_a_capture_without_closing_the_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_input("f");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("j");
    cx.simulate_keystrokes("escape");

    let (listening, query, open) = shell.read_with(&cx, |shell, _| {
        let state = shell.keybindings.as_ref().unwrap();
        (
            state.listening.is_some(),
            state.query.clone(),
            shell.modal.is_some(),
        )
    });
    assert!(!listening, "escape should cancel the capture");
    assert!(open, "and must not also close the dialog behind it");
    assert_eq!(query, "f", "the filter text survives a cancelled capture");
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "cancelling hands focus back to the filter"
    );
}

/// Escape from the resting state closes the dialog and returns focus
/// to the shell root, so shell chords work again immediately.
#[gpui::test]
fn escape_closes_the_dialog_and_restores_shell_focus(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("escape");
    shell.read_with(&cx, |shell, _| {
        assert!(shell.modal.is_none(), "escape should close the modal");
        assert!(
            shell.keybindings.is_none(),
            "close_modal must clear the dialog state too, or the shared \
             input's subscription can route into a stale dialog"
        );
    });
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "focus must leave the filter on close"
    );
    assert!(
        cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)),
        "and land back on the shell root"
    );
}

/// The filter narrows what actually *paints*, and the narrowed list
/// renders its fuzzy highlights without blowing up — the painted half
/// of this dialog's conversion, re-expressing what the retired fzf
/// find test used to prove about its own narrowed list. Row selectors
/// stay keyed by full-list index, so a surviving row and a hidden one
/// can be addressed by identity.
#[gpui::test]
fn typing_a_query_narrows_the_rows_that_paint(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_input("focus");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let (visible, row_count) = shell.read_with(&cx, |shell, _| {
        let rows = keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap);
        let state = shell.keybindings.as_ref().expect("dialog open");
        let visible: Vec<usize> = keybindings_view::visible_rows(state, &rows)
            .iter()
            .map(|m| m.row)
            .collect();
        (visible, rows.len())
    });
    assert!(
        !visible.is_empty() && visible.len() < row_count,
        "sanity: 'focus' should match some rows but not all, got {visible:?} of {row_count}"
    );

    let first_hidden = (0..row_count).find(|ix| !visible.contains(ix)).unwrap();
    // `debug_bounds` takes `&'static str`; leak the two dynamic
    // selectors (test-only, a few bytes).
    let match_selector: &'static str =
        Box::leak(format!("keybindings-row-{}", visible[0]).into_boxed_str());
    let hidden_selector: &'static str =
        Box::leak(format!("keybindings-row-{first_hidden}").into_boxed_str());
    assert!(
        cx.debug_bounds(match_selector).is_some(),
        "a matching row must still paint under the filter"
    );
    assert!(
        cx.debug_bounds(hidden_selector).is_none(),
        "a non-matching row (index {first_hidden}) must not paint under the filter"
    );
}

/// End-to-end (design doc, "Tests"): listening, typing a two-keystroke
/// sequence, then `enter` writes the new binding into the real user
/// `keymap.toml` — verified by re-parsing the written file through the
/// REAL production path (`LayerDoc` + `keymap::build_keymap`), the same
/// precedent `keymap_edit`'s own round-trip tests and `shell::mod`'s
/// `mod_shift_t_keystroke_persists_the_new_mode_to_the_user_config_file`
/// both use for a background-executor write.
#[gpui::test]
fn listening_then_enter_persists_the_new_binding_to_the_user_keymap_file(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);

    let dir = tempfile::tempdir().unwrap();
    let user_dir = dir.path().to_path_buf();

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| {
                    ShellView::new(test_services(), None, Some(user_dir.clone()), window, cx)
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

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("keybindings::open".to_string()), None, window, cx);
        });
    });

    // The action bound at row 0 (top of sort order) at the moment the
    // dialog opened — what the capture below should end up bound to.
    let target_action = shell.read_with(&cx, |shell, _| {
        keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap)[0]
            .action
            .clone()
    });

    cx.simulate_keystrokes("enter");
    // A two-keystroke sequence, proving multi-keystroke capture works,
    // not just a single chord.
    cx.simulate_keystrokes("ctrl-alt-x");
    cx.simulate_keystrokes("y");
    cx.simulate_keystrokes("enter");

    assert!(
        shell.read_with(&cx, |shell, _| shell
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_none()),
        "enter should have committed and left listening mode"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "committing a rebind must not close the dialog"
    );

    // `spawn_rebind` hands the actual write to the background executor
    // (philosophy: no I/O on the UI thread) — drive it to completion.
    cx.run_until_parked();

    let text = std::fs::read_to_string(user_dir.join("keymap.toml"))
        .expect("committing the capture must have written keymap.toml");
    let table: toml::Table = text.parse().unwrap();
    let doc = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: user_dir.join("keymap.toml"),
        table,
    };

    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry);
    assert!(
        diags.is_empty(),
        "the written keymap.toml must build clean: {diags:?}"
    );

    let binding = keymap
        .bindings()
        .iter()
        .find(|b| b.action == target_action && b.keystrokes.len() == 2)
        .expect(
            "the two-keystroke capture must have been written and resolve to the target action",
        );
    assert_eq!(binding.keystrokes[0].key, "x");
    assert!(binding.keystrokes[0].mods.ctrl && binding.keystrokes[0].mods.alt);
    assert_eq!(binding.keystrokes[1].key, "y");
    assert_eq!(binding.keystrokes[1].mods, Modifiers::NONE);
}

/// A real mouse click selects a different row (`debug_bounds` gives the
/// row's real painted coordinates, same technique
/// `mouse_down_on_a_tile_focuses_it` and the settings-panel click tests
/// already use in this file) — then clicking that SAME, now-selected
/// row again starts listening.
#[gpui::test]
fn click_selects_a_row_and_clicking_it_again_starts_listening(cx: &mut gpui::TestAppContext) {
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

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("keybindings::open".to_string()), None, window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let row_1_bounds = cx
        .debug_bounds("keybindings-row-1")
        .expect("row 1 should have painted bounds to click into");
    let inside_row_1 = gpui::point(
        row_1_bounds.origin.x + gpui::px(10.0),
        row_1_bounds.origin.y + gpui::px(10.0),
    );

    cx.simulate_mouse_down(inside_row_1, MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected),
        1,
        "clicking row 1 should have selected it"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_none()),
        "the first click on a different row must not start listening"
    );

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row_1_bounds_again = cx
        .debug_bounds("keybindings-row-1")
        .expect("row 1 should still have painted bounds");
    let inside_row_1_again = gpui::point(
        row_1_bounds_again.origin.x + gpui::px(10.0),
        row_1_bounds_again.origin.y + gpui::px(10.0),
    );
    cx.simulate_mouse_down(
        inside_row_1_again,
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_some()),
        "clicking the already-selected row again should start listening"
    );
}
