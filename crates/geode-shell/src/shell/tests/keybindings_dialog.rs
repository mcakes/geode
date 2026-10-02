//! The keybindings dialog: rows, filtering, selection, listening for a
//! new binding, and persisting it to the user keymap file.

use super::*;
use crate::dialogmode::DialogMode;

// Keybinding dialog.

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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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

/// The dialog opens in normal mode with its shared filter blurred, allowing bare-key
/// commands to reach the handler. `/` enters filter mode and focuses the input.
#[gpui::test]
fn opening_the_keybindings_dialog_leaves_the_filter_blurred(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    assert!(
        shell.read_with(&cx, |shell, _| shell.keybindings.is_some()),
        "sanity: keybindings::open should have opened the dialog"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "the filter must NOT own focus on open, or normal mode's letters \
         would all be typed instead of acted on"
    );
}

/// In filter mode, `j` types into the query rather than moving selection. Its
/// normal-mode navigation is covered separately.
#[gpui::test]
fn typing_j_in_filter_mode_filters_rather_than_moving_the_selection(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
    cx.simulate_input("j");
    let (query, selected) = shell.read_with(&cx, |shell, _| {
        let state = shell.keybindings.as_ref().unwrap();
        (state.query.clone(), state.selected)
    });
    assert_eq!(query, "j", "j must reach the filter as text");
    assert_eq!(selected, 0, "j must not move the selection any more");
}

/// Arrow and ctrl motions still move the selection *in filter mode*,
/// and do it without disturbing the filter's focus or its text — the
/// two modes share one navigation vocabulary, so the same motions also
/// work in normal mode (`j_and_k_move_in_normal_mode`).
#[gpui::test]
fn arrows_and_ctrl_motions_move_the_selection(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
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
    // Filter mode: the leak this guards against is only reachable with
    // the `Input` actually focused, which is what makes an unclaimed key
    // continue into the text-input phase.
    cx.simulate_keystrokes("/");
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
    // Start in filter mode so leaving it exercises Input blur. The first
    // Enter keeps the query; the second starts binding capture.
    cx.simulate_keystrokes("/");
    cx.simulate_keystrokes("enter");
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_none()),
        "the enter that leaves filter mode must not also start a capture"
    );
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

/// Escape cancels a capture started by a filtered-row click, restores
/// filter focus, and retains the query and dialog. Clicking can start capture
/// while filtering; filter-mode Enter only keeps the query and leaves that mode.
#[gpui::test]
fn escape_cancels_a_capture_without_closing_the_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
    cx.simulate_input("f");
    cx.run_until_parked();
    let row = top_match_bounds(&shell, &mut cx);
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(10.0), row.origin.y + gpui::px(10.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_some()),
        "sanity: the click started a capture from filter mode"
    );
    cx.simulate_input("j");
    cx.simulate_keystrokes("escape");

    let (listening, query, open) = shell.read_with(&cx, |shell, _| {
        let state = shell.keybindings.as_ref().unwrap();
        (
            state.listening.is_some(),
            state.query.clone(),
            shell.modal_open(),
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
        assert!(!shell.modal_open(), "escape should close the modal");
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

/// Filtering narrows the rendered rows and paints fuzzy highlights. Row selectors
/// retain full-list indices so assertions can identify both surviving and hidden rows.
#[gpui::test]
fn typing_a_query_narrows_the_rows_that_paint(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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

/// The `ctrl+a` reclaim is scoped to every `GeodeModal > Input`, including the
/// keybinding filter. It must be swallowed before the component's SelectAll or MoveHome
/// binding, while ordinary typing continues to reach the field.
#[gpui::test]
fn ctrl_a_is_reclaimed_inside_the_filter_but_typing_still_works(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    // The reclaim is scoped to a focused modal `Input`, so this test
    // needs the dialog in filter mode to exercise it at all.
    cx.simulate_keystrokes("/");
    cx.simulate_input("foo");
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("bar");

    let query = shell.read_with(&cx, |shell, _| {
        shell.keybindings.as_ref().unwrap().query.clone()
    });
    assert_eq!(
        query, "foobar",
        "ctrl-a must be inert (NoAction) here, not gpui-component's own \
         SelectAll (which would have made 'bar' replace 'foo') or MoveHome \
         (which would have inserted it before 'foo') — either would leave \
         something other than 'foobar'"
    );
}

/// One click on a row selects it and starts listening, matching Enter's capture action.
#[gpui::test]
fn a_single_click_on_a_row_starts_listening(cx: &mut gpui::TestAppContext) {
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
    let (selected, listening) = shell.read_with(&cx, |shell, _| {
        let k = shell.keybindings.as_ref().unwrap();
        (k.selected, k.listening.clone())
    });
    assert_eq!(selected, 1, "the click selected row 1");
    assert_eq!(
        listening,
        Some(Vec::new()),
        "and started listening on it at once"
    );

    // A click on a different row mid-capture retargets the capture.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row_2 = cx.debug_bounds("keybindings-row-2").expect("row 2 painted");
    cx.simulate_mouse_down(
        gpui::point(
            row_2.origin.x + gpui::px(10.0),
            row_2.origin.y + gpui::px(10.0),
        ),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    let (selected, listening) = shell.read_with(&cx, |shell, _| {
        let k = shell.keybindings.as_ref().unwrap();
        (k.selected, k.listening.clone())
    });
    assert_eq!(selected, 2);
    assert_eq!(listening, Some(Vec::new()));
}

// Normal and filter modes.

/// The dialog now opens in normal mode, so a bare letter is a verb
/// rather than filter text. This is the behaviour change the whole
/// interaction model turns on.
#[gpui::test]
fn the_dialog_opens_in_normal_mode_and_letters_do_not_type(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().mode),
        crate::dialogmode::DialogMode::Normal,
    );
    cx.simulate_keystrokes("s");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "",
        "a bare letter in normal mode must not reach the filter"
    );
}

/// The mode is *legible*, not just held: the pill paints in both modes.
/// A modal surface whose only tell is whether a caret happens to be
/// blinking is the failure this whole model has to avoid — and a pill
/// built but never reached by the render tree would look identical to
/// one that works, from the state assertions alone.
#[gpui::test]
fn the_mode_pill_paints_the_mode_it_is_actually_in(cx: &mut gpui::TestAppContext) {
    let (_shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    let normal = cx.debug_bounds("dialog-mode-pill-normal");
    assert!(
        normal.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "the pill should paint, labelled 'normal', in normal mode, got {normal:?}"
    );
    assert!(
        cx.debug_bounds("dialog-mode-pill-filter").is_none(),
        "and must not be labelled 'filter' there"
    );

    cx.simulate_keystrokes("/");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let filter = cx.debug_bounds("dialog-mode-pill-filter");
    assert!(
        filter.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "and 'filter' in filter mode, got {filter:?}"
    );
    assert!(
        cx.debug_bounds("dialog-mode-pill-normal").is_none(),
        "with the 'normal' label gone — a pill that paints the same label \
         in both modes, or swaps them, is the failure this catches and a \
         non-zero-bounds assertion would not"
    );
}

/// The mode pill appears in the shared title row, and an empty frozen filter displays
/// its placeholder.
#[gpui::test]
fn the_keybinding_dialogs_pill_sits_in_the_title_row(cx: &mut gpui::TestAppContext) {
    let (_shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    let pill = cx
        .debug_bounds("dialog-mode-pill-normal")
        .expect("pill paints");
    let title = cx.debug_bounds("shell-modal-title").expect("title paints");
    assert!(
        (pill.origin.y - title.origin.y).abs() < title.size.height,
        "same row as the title"
    );
    let placeholder = cx
        .debug_bounds("dialog-filter-placeholder")
        .expect("placeholder paints");
    // Width, not just presence: the selector rides the text itself, so an
    // emptied label (the mutation this test is named as covering) shrinks
    // this to zero width — a bare `.is_some()` would not notice, since
    // the row around it still paints either way.
    assert!(
        placeholder.size.width > gpui::px(0.0),
        "the placeholder text itself must paint, not just its row"
    );
}

/// Hide the `/` filter placeholder while listening for a binding: capture consumes `/`
/// as a key instead of opening the filter. Canceling capture restores the placeholder.
#[gpui::test]
fn the_filter_placeholder_is_gone_while_listening_for_a_capture(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    // Normal mode, nothing typed: the state the dialog opens in, and the
    // one the placeholder exists for.
    assert!(
        cx.debug_bounds("dialog-filter-placeholder").is_some(),
        "the placeholder should paint before the capture starts"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_some()),
        "enter should have started listening — the setup this test needs"
    );
    assert!(
        cx.debug_bounds("dialog-filter-placeholder").is_none(),
        "`/` is the binding being captured here, not the filter's key"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |shell, _| shell
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_none()),
        "escape should have cancelled the capture"
    );
    assert!(
        cx.debug_bounds("dialog-filter-placeholder").is_some(),
        "and the hint is true again the moment the capture ends"
    );
}

/// `/` enters filter mode and typing narrows, exactly as it does today.
#[gpui::test]
fn slash_enters_filter_mode_and_typing_narrows(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().mode),
        crate::dialogmode::DialogMode::Filter,
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "entering filter mode must hand focus to the filter, or typing \
         would still fall on the floor"
    );
    cx.simulate_keystrokes("t h e m e");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "theme",
    );
}

/// After Enter keeps the filter, successive Escape presses clear the query
/// and close the dialog in separate transitions.
#[gpui::test]
fn escape_walks_the_ladder_one_rung_at_a_time(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/ t h e m e");
    cx.run_until_parked();

    cx.simulate_keystrokes("enter");
    let (mode, q) = shell.read_with(&cx, |s, _| {
        let k = s.keybindings.as_ref().unwrap();
        (k.mode, k.query.clone())
    });
    assert_eq!(mode, crate::dialogmode::DialogMode::Normal);
    assert_eq!(q, "theme", "enter must keep the query applied");
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "and must blur the filter, or normal mode's letters would still type"
    );
    assert!(
        shell.read_with(&cx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_none()),
        "and must not start a capture on its way out of filter mode"
    );

    cx.simulate_keystrokes("escape");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "",
        "the second escape clears the query"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal_open()),
        "and does not close"
    );

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |s, _| !s.modal_open()),
        "the third closes"
    );
}

/// Escape restores the query kept by the previous filter session in both
/// state and Input. Refocusing the field must not reveal abandoned text.
#[gpui::test]
fn escape_puts_back_the_query_filter_mode_was_entered_with(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/ t h e m e");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");

    // A second search over the first, then a change of mind.
    cx.simulate_keystrokes("/");
    cx.simulate_input("x");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "themex",
        "sanity: the second session typed onto the kept query"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let (mode, query) = shell.read_with(&cx, |s, _| {
        let k = s.keybindings.as_ref().unwrap();
        (k.mode, k.query.clone())
    });
    assert_eq!(mode, crate::dialogmode::DialogMode::Normal);
    assert_eq!(query, "theme", "escape must put back the entry query");
    assert!(
        shell.read_with(&cx, |s, _| s.modal_open()),
        "and must not close the dialog"
    );

    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "theme",
        "the field the next filter session sees is the restored query, \
         not the abandoned one"
    );
}

/// A cleared query must clear the *input* too, not just the mirrored
/// copy: the `Input` owns the text, so a query cleared only in
/// `KeybindingsState` would reappear the moment `/` refocused the field
/// — and the list would be ranked against something the user cannot see.
#[gpui::test]
fn clearing_the_query_clears_the_field_the_next_filter_session_sees(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/ t h e m e");
    cx.run_until_parked();
    // Enter keeps the query; Escape from normal mode clears it.
    // Escape directly from filter mode would instead revert the query.
    cx.simulate_keystrokes("enter escape");
    cx.run_until_parked();

    cx.simulate_keystrokes("/");
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "x",
        "the field must have been emptied along with the mirrored query"
    );
}

/// `j`/`k` move in normal mode; the arrows and ctrl-steps still work in
/// both modes, so one navigation vocabulary serves both.
#[gpui::test]
fn j_and_k_move_in_normal_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("j j");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().selected),
        2
    );
    cx.simulate_keystrokes("k");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().selected),
        1
    );
}

/// Cancelling a capture that started in normal mode must NOT hand focus
/// to the filter: the dialog is still in normal mode, and a focused
/// `Input` there would swallow the very letters normal mode exists to
/// free (the next `d` would type a `d` instead of unbinding). The
/// restore has to follow the mode, not be hardcoded to the filter.
#[gpui::test]
fn cancelling_a_capture_restores_focus_to_the_mode_that_started_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .listening
            .is_none()),
        "sanity: escape cancels the capture"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "a capture cancelled in normal mode must leave the filter blurred"
    );
    cx.simulate_keystrokes("j");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().selected),
        1,
        "and normal mode's own vocabulary must still work afterwards"
    );
}

/// A row click starts capture and blurs the shared filter, including when that filter
/// was already blurred in normal mode.
#[gpui::test]
fn a_click_in_normal_mode_leaves_the_filter_blurred_because_it_captures(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row_1 = cx
        .debug_bounds("keybindings-row-1")
        .expect("row 1 should have painted bounds to click into");
    cx.simulate_mouse_down(
        gpui::point(
            row_1.origin.x + gpui::px(10.0),
            row_1.origin.y + gpui::px(10.0),
        ),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().selected),
        1,
        "sanity: the click selected row 1"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "a click in normal mode must not focus the filter behind the user's back"
    );
}

/// Clearing the query re-expands the list under a viewport that is still
/// parked wherever the filtered list left it, so the `ClearQuery` rung
/// has to move the scroll as well as the index — the same pairing
/// `ShellView::new`'s query-change subscription makes (`set_query` then
/// `scroll_to_item(0)`), which cannot help here because `set_value` is
/// deliberately silent and never fires `InputEvent::Change`. Asserting
/// the index alone would pass with the viewport left behind and row 0
/// off screen, so this checks the row actually painted inside the list —
/// `picker`'s `keyboard_navigation_past_visible_rows_scrolls_the_
/// selection_into_view` standard, in the non-virtualizing form the
/// palette's own version uses (this list lays every row out, so bounds
/// existing is not the proof; intersecting the viewport is).
#[gpui::test]
fn clearing_the_query_scrolls_back_to_the_top(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    // A query almost every row matches, so there is still far more than
    // one screenful (VISIBLE_ROWS = 10) to scroll through under it.
    cx.simulate_keystrokes("/");
    cx.simulate_input("e");
    cx.run_until_parked();
    let matches = shell.read_with(&cx, |shell, _| {
        let rows = keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap);
        let state = shell.keybindings.as_ref().expect("dialog open");
        keybindings_view::visible_rows(state, &rows).len()
    });
    assert!(
        matches > 20,
        "this test needs more than one screenful under the filter to \
         scroll at all (got {matches})"
    );

    let downs = vec!["down"; 20].join(" ");
    cx.simulate_keystrokes(&downs);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().selected),
        20,
        "sanity: the selection walked 20 rows down the filtered list"
    );

    // Out of filter mode with the query kept (`enter`), then the
    // `ClearQuery` rung (`escape`).
    cx.simulate_keystrokes("enter escape");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().selected),
        0,
        "sanity: clearing the query resets the selection to the top"
    );

    cx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let list_bounds = cx
        .debug_bounds("keybindings-list")
        .expect("the row list container should have painted");
    let row_bounds = cx
        .debug_bounds("keybindings-row-0")
        .expect("row 0 should still be part of the layout tree (no virtualization)");
    assert!(
        list_bounds.intersects(&row_bounds),
        "row 0 {row_bounds:?} should be scrolled back into the visible \
         list viewport {list_bounds:?} when the query is cleared, not \
         left above it with only the index having changed"
    );
}

/// Normal mode consumes unrecognized keys. Modified Escape follows the
/// same exit ladder as bare Escape in both normal and filter mode.
#[gpui::test]
fn a_modified_escape_walks_the_same_ladder_as_a_bare_one(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
    cx.simulate_input("theme");
    cx.run_until_parked();

    cx.simulate_keystrokes("shift-escape");
    cx.run_until_parked();
    let (mode, query) = shell.read_with(&cx, |s, _| {
        let k = s.keybindings.as_ref().unwrap();
        (k.mode, k.query.clone())
    });
    assert_eq!(
        mode,
        crate::dialogmode::DialogMode::Normal,
        "shift+escape must leave filter mode, exactly as escape does"
    );
    assert_eq!(
        query, "",
        "and revert the query it was entered with, exactly as escape does"
    );

    // Back in with a query this time kept by `enter`, so there is
    // something for the `ClearQuery` rung to clear.
    cx.simulate_keystrokes("/");
    cx.simulate_input("theme");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");

    cx.simulate_keystrokes("ctrl-escape");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "",
        "ctrl+escape must clear the query, exactly as escape does"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal_open()),
        "and not close on that rung"
    );

    cx.simulate_keystrokes("alt-escape");
    assert!(
        shell.read_with(&cx, |s, _| !s.modal_open()),
        "alt+escape on the last rung must close the dialog, not be \
         swallowed by normal mode's claim-and-drop"
    );
}

// Unbinding and resetting bindings through the dialog.

/// Open the keybinding dialog on an existing shell and draw to install its key handler.
/// Persistence tests use `open_shell_with_user_dir` for a real user directory rather
/// than the default dialog fixture.
fn open_keybindings(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("keybindings::open".to_string()), None, window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// The selected row's action and its `(rendered binding, layer)`, read
/// from the live shell rather than assumed — a test that hardcoded "row
/// 0 is bound" would silently stop testing anything the day the sort
/// order moved an unbound action to the top.
fn selected_row(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> (ActionId, Option<(String, Layer)>) {
    shell.read_with(cx, |shell, _| {
        let rows = keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap);
        let state = shell.keybindings.as_ref().expect("dialog open");
        let visible = keybindings_view::visible_rows(state, &rows);
        let row = &rows[visible[state.selected].row];
        (
            row.action.clone(),
            row.current
                .as_ref()
                .map(|b| (crate::palette::render_binding(&b.keystrokes), b.layer)),
        )
    })
}

/// The on-disk user keymap the fixture below claims to have come from:
/// the writer edits the *file*, so a fixture's in-memory keymap and its
/// file must agree, or a removal would find nothing to remove.
const USER_KEYMAP_TEXT: &str =
    "config_version = 1\n\n[[bindings]]\n\n[bindings.keys]\n\"ctrl+alt+y\" = \"palette::toggle\"\n";

/// A user-layer keymap on top of the builtin one, binding `ctrl+alt+y`
/// to `palette::toggle`. That key is in no lower layer, so the dialog's
/// `palette::toggle` row resolves to the USER's binding — the fixture
/// both "`d` removes rather than shadows" and "`r` resets" need, and the
/// one shape where getting `is_user_layer` backwards would delete or
/// entomb the wrong thing.
fn services_with_a_user_binding_for_the_palette() -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: USER_KEYMAP_TEXT.parse().unwrap(),
    };
    let (keymap, diags) = build_keymap(&[builtin, user], default_mod(), &services.registry);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = keymap;
    services
}

/// The painted bounds of the top row of the *filtered* list. A row's
/// debug selector is keyed by its index in the FULL row list, so under a
/// filter `keybindings-row-0` may not be on screen at all — the visible
/// list's first entry names its own row index.
fn top_match_bounds(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
) -> gpui::Bounds<gpui::Pixels> {
    let ix = shell.read_with(cx, |shell, _| {
        let rows = keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap);
        let state = shell.keybindings.as_ref().expect("dialog open");
        keybindings_view::visible_rows(state, &rows)
            .first()
            .expect("the filter must match at least one row")
            .row
    });
    // `debug_bounds` wants a `'static` selector; the row index is only
    // known at run time, so this one test helper leaks its formatted
    // name rather than every caller spelling out a match over indices.
    let selector: &'static str = Box::leak(format!("keybindings-row-{ix}").into_boxed_str());
    cx.debug_bounds(selector)
        .expect("the filtered list's top row should have painted")
}

/// Filter to the palette row and keep the query with Enter, selecting a
/// known nonempty binding for subsequent edit operations.
fn select_the_palette_row(cx: &mut gpui::VisualTestContext) {
    cx.simulate_keystrokes("/ p a l e t t e");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
}

/// The gap this model exists to close: before it, clearing a binding
/// meant hand-editing keymap.toml. A builtin binding cannot be *removed*
/// (this app only ever writes the user layer), so it is silenced with
/// the documented `"none"` shadow.
#[gpui::test]
fn d_unbinds_the_selected_binding(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    let (action, bound) = selected_row(&shell, &vcx);
    let (key, layer) = bound.expect("the filtered-to row must actually have a binding");
    assert_eq!(
        action.0, "palette::toggle",
        "sanity: the filter landed on the palette row"
    );
    assert_eq!(
        layer,
        Layer::Builtin,
        "sanity: with no user keymap this binding is builtin"
    );

    vcx.simulate_keystrokes("d y");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml"))
        .expect("d must write the user keymap");
    assert!(
        text.contains(&format!("\"{key}\" = \"none\"")),
        "a builtin binding is silenced, not removed: {text}"
    );
}

/// The dangerous branch, the other way round: the user's OWN binding is
/// removed outright rather than shadowed. A wrong `is_user_layer: false`
/// here would leave a redundant `"none"` over the user's own entry — the
/// key stays dead and nothing in the file says why.
#[gpui::test]
fn d_on_a_user_layer_binding_removes_it_rather_than_shadowing_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), USER_KEYMAP_TEXT).unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(
        cx,
        services_with_a_user_binding_for_the_palette(),
        dir.path(),
    );
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    let (action, bound) = selected_row(&shell, &vcx);
    let (key, layer) = bound.expect("the filtered-to row must actually have a binding");
    assert_eq!(action.0, "palette::toggle");
    assert_eq!(key, "ctrl+alt+y", "the user's binding is the effective one");
    assert_eq!(layer, Layer::User);

    vcx.simulate_keystrokes("d y");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("d must write");
    assert!(
        !text.contains("ctrl+alt+y"),
        "the user's own key must be removed outright: {text}"
    );
    assert!(
        !text.contains("none"),
        "and must not be left shadowed by a redundant none: {text}"
    );
}

/// `r` removes the user's override so the layer beneath shows through —
/// always the removal branch, never a shadow.
#[gpui::test]
fn r_resets_a_user_override_by_removing_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), USER_KEYMAP_TEXT).unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(
        cx,
        services_with_a_user_binding_for_the_palette(),
        dir.path(),
    );
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    vcx.simulate_keystrokes("r y");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("r must write");
    assert!(
        !text.contains("ctrl+alt+y"),
        "reset removes the user's key so the builtin shows through: {text}"
    );
    assert!(
        !text.contains("none"),
        "reset must never write a shadow — that would bury the layer it \
         is meant to uncover: {text}"
    );
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("r acknowledges the write it spawned");
    assert!(
        !notice.contains("no user override"),
        "a reset that had something to reset reports no complaint: {notice}"
    );
}

/// A key that visibly does nothing is exactly the defect class this
/// branch exists to remove, so `r` on a row the user has never
/// overridden says so instead of silently writing nothing.
#[gpui::test]
fn r_on_a_row_with_no_user_override_says_so_and_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    let (_, bound) = selected_row(&shell, &vcx);
    assert_eq!(
        bound.expect("bound").1,
        Layer::Builtin,
        "sanity: this row's binding is not the user's"
    );

    vcx.simulate_keystrokes("r");
    vcx.run_until_parked();

    assert!(
        !dir.path().join("keymap.toml").exists(),
        "reset with nothing to reset must not write a file at all — a \
         none shadow here would silence the key it was asked to restore"
    );
    let notice = shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("no user override")),
        "and must say why nothing happened, got {notice:?}"
    );

    // "Observable" means painted, not merely stored — a notice the user
    // cannot see is the same silent no-op this branch exists to remove.
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let bounds = vcx.debug_bounds("keybindings-notice");
    assert!(
        bounds.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "the notice must paint with non-zero bounds, got {bounds:?}"
    );
}

/// The same honesty for `d` on a row that has no binding to silence.
#[gpui::test]
fn d_on_an_unbound_row_says_so_and_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    // Walk to a row that is genuinely unbound rather than assuming one.
    let target = shell.read_with(&vcx, |shell, _| {
        let rows = keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap);
        rows.iter()
            .position(|r| r.current.is_none())
            .expect("the builtin keymap leaves plenty of actions unbound")
    });
    vcx.update(|_, cx| {
        shell.update(cx, |shell, _| {
            shell.keybindings.as_mut().unwrap().selected = target;
        });
    });

    vcx.simulate_keystrokes("d");
    vcx.run_until_parked();

    assert!(
        !dir.path().join("keymap.toml").exists(),
        "there is no key to silence, so nothing is written"
    );
    let notice = shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("already unbound")),
        "and the keystroke reports why it did nothing, got {notice:?}"
    );
}

/// A notice is a report about the *last* keystroke; leaving it up would
/// make the footer lie about the next one.
#[gpui::test]
fn a_notice_clears_on_the_next_normal_mode_keystroke(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    vcx.simulate_keystrokes("r");
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .notice
            .is_some()),
        "sanity: r on a builtin row leaves a notice"
    );
    vcx.simulate_keystrokes("j");
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .notice
            .is_none()),
        "the next keystroke clears it"
    );
}

// Binding persistence, acknowledgements, and selection changes.

/// A user layer silences both builtin palette-toggle keys with `"none"` shadows,
/// modeling two unbind operations. The resulting row has no current binding and can
/// still be reset.
const USER_KEYMAP_SILENCING_THE_PALETTE: &str = "config_version = 1\n\n[[bindings]]\n\n\
     [bindings.keys]\n\"ctrl+k\" = \"none\"\n\"ctrl+shift+p\" = \"none\"\n";

fn services_with_the_palette_silenced() -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: USER_KEYMAP_SILENCING_THE_PALETTE.parse().unwrap(),
    };
    let (keymap, diags) = build_keymap(&[builtin, user], default_mod(), &services.registry);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = keymap;
    services
}

/// A user `"none"` shadow counts as an override even when the row displays no binding.
/// Reset finds it through `user_overrides_for` across the keymap and removes the
/// shadow.
#[gpui::test]
fn r_on_a_silenced_row_lifts_the_shadow(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("keymap.toml"),
        USER_KEYMAP_SILENCING_THE_PALETTE,
    )
    .unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_the_palette_silenced(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    let (action, bound) = selected_row(&shell, &vcx);
    assert_eq!(
        action.0, "palette::toggle",
        "sanity: the filter landed right"
    );
    assert!(
        bound.is_none(),
        "sanity: a shadowed row derives as unbound — the row itself has no \
         key to name, so the override must come from the keymap"
    );

    vcx.simulate_keystrokes("r");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm),
        Some(keybindings_view::KeybindingConfirm::Reset),
        "r ARMS on the shadowed row — the set is non-empty even though the row shows no key"
    );
    let unchanged = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("still there");
    assert_eq!(
        unchanged, USER_KEYMAP_SILENCING_THE_PALETTE,
        "nothing is written while the question stands"
    );

    vcx.simulate_keystrokes("y");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("r must write");
    assert!(
        !text.contains("\"ctrl+k\" = \"none\""),
        "the shadow over the builtin key is lifted: {text}"
    );
    assert!(
        !text.contains("\"ctrl+shift+p\" = \"none\""),
        "and the one over the second builtin key: {text}"
    );
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("r acknowledges the write");
    assert!(
        !notice.contains("no user override"),
        "the shadow IS the user's override: {notice}"
    );
}

/// The user keymap a rebind of the builtin palette toggle writes
/// (`apply_rebind`: the new key, then the `"none"` shadow over the old).
const USER_KEYMAP_REBINDING_THE_PALETTE: &str = "config_version = 1\n\n[[bindings]]\n\n\
     [bindings.keys]\n\"ctrl+alt+y\" = \"palette::toggle\"\n\"ctrl+k\" = \"none\"\n";

fn services_with_the_palette_rebound() -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: USER_KEYMAP_REBINDING_THE_PALETTE.parse().unwrap(),
    };
    let (keymap, diags) = build_keymap(&[builtin, user], default_mod(), &services.registry);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = keymap;
    services
}

/// The defect that made "reset" not reset: after a rebind of a builtin,
/// `r` removed the new key and left the `"none"` shadow, so the action
/// ended up UNBOUND rather than back on its builtin key. Both halves of
/// the pair go in one write.
#[gpui::test]
fn r_on_a_rebound_builtin_removes_the_new_key_and_lifts_the_shadow(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("keymap.toml"),
        USER_KEYMAP_REBINDING_THE_PALETTE,
    )
    .unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_the_palette_rebound(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);
    let (_, bound) = selected_row(&shell, &vcx);
    assert_eq!(
        bound.map(|(_, layer)| layer),
        Some(Layer::User),
        "sanity: the row shows the user's rebind"
    );

    vcx.simulate_keystrokes("r");
    vcx.run_until_parked();
    let prompt = vcx
        .debug_bounds("keybindings-confirm")
        .map(|_| shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm));
    assert_eq!(
        prompt,
        Some(Some(keybindings_view::KeybindingConfirm::Reset)),
        "r arms the reset question"
    );
    let question = shell.read_with(&vcx, |s, _| {
        let rows = keybindings_view::derive_rows(&s.services.registry, &s.services.keymap);
        let state = s.keybindings.as_ref().unwrap();
        let visible = keybindings_view::visible_rows(state, &rows);
        keybindings_view::confirm_prompt(
            keybindings_view::KeybindingConfirm::Reset,
            Some(&rows[visible[state.selected].row]),
            s.services.keymap.bindings(),
        )
    });
    assert!(
        question.contains("2 overrides"),
        "the question counts both halves of the pair: {question}"
    );

    vcx.simulate_keystrokes("y");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("r acknowledges");
    assert!(notice.contains("2 overrides"), "{notice}");
    vcx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("r must write");
    assert!(
        !text.contains("ctrl+alt+y"),
        "the new key is removed: {text}"
    );
    assert!(
        !text.contains("none"),
        "AND the shadow over the builtin key is lifted, or the action is \
         left unbound instead of reset: {text}"
    );
}

/// Two user entries under two contexts, so reset-all has more than one
/// entry to drop and the count it reports is the KEY count, not the entry
/// count.
const USER_KEYMAP_WITH_TWO_ENTRIES: &str = "# mine\nconfig_version = 1\n\n[[bindings]]\n\n\
     [bindings.keys]\n\"ctrl+alt+y\" = \"palette::toggle\"\n\"ctrl+k\" = \"none\"\n\n\
     [[bindings]]\ncontext = \"workspace\"\n\n[bindings.keys]\n\
     \"ctrl+alt+u\" = \"workspace::focus_left\"\n";

fn services_with_two_user_entries() -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: USER_KEYMAP_WITH_TWO_ENTRIES.parse().unwrap(),
    };
    let (keymap, diags) = build_keymap(&[builtin, user], default_mod(), &services.registry);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = keymap;
    services
}

/// `shift+r` asks, names the number of user bindings it will remove, and
/// on `y` drops every `[[bindings]]` entry from the user keymap — leaving
/// the rest of the file alone. Desk and builtin layers are not this
/// app's to touch, so "all" is the user layer's whole say.
#[gpui::test]
fn shift_r_asks_and_y_removes_every_user_binding(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), USER_KEYMAP_WITH_TWO_ENTRIES).unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_two_user_entries(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    vcx.simulate_keystrokes("shift-r");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm),
        Some(keybindings_view::KeybindingConfirm::ResetAll),
        "shift+r arms the reset-all question"
    );
    assert!(
        vcx.debug_bounds("keybindings-confirm").is_some(),
        "and it paints"
    );
    let prompt = shell.read_with(&vcx, |s, _| {
        keybindings_view::confirm_prompt(
            keybindings_view::KeybindingConfirm::ResetAll,
            None,
            s.services.keymap.bindings(),
        )
    });
    assert!(
        prompt.contains('3'),
        "the question names the number of user bindings, 3 keys across 2 \
         entries: {prompt}"
    );
    let unchanged = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("still there");
    assert_eq!(
        unchanged, USER_KEYMAP_WITH_TWO_ENTRIES,
        "nothing is written while the question stands"
    );

    vcx.simulate_keystrokes("y");
    vcx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("written");
    assert!(
        !text.contains("[[bindings]]"),
        "every entry is gone: {text}"
    );
    assert!(
        text.contains("# mine"),
        "the rest of the file survives: {text}"
    );
    assert!(text.contains("config_version = 1"), "{text}");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("reset-all acknowledges the write it spawned");
    assert!(notice.contains('3'), "and names the count: {notice}");
}

#[gpui::test]
fn shift_r_n_withdraws_and_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), USER_KEYMAP_WITH_TWO_ENTRIES).unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_two_user_entries(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    vcx.simulate_keystrokes("shift-r n");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .confirm
            .is_none()),
        "n withdraws it"
    );
    let unchanged = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("still there");
    assert_eq!(unchanged, USER_KEYMAP_WITH_TWO_ENTRIES);
}

/// A key that visibly does nothing is the defect class this dialog
/// exists to remove: with no user bindings there is nothing to reset,
/// so `shift+r` says so in the footer rather than asking a question
/// about removing nothing — and the button is not painted at all.
#[gpui::test]
fn shift_r_with_no_user_bindings_says_so_and_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    assert!(
        vcx.debug_bounds("keybindings-action-shift+r").is_none(),
        "no reset-all button with nothing to reset"
    );

    vcx.simulate_keystrokes("shift-r");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .confirm
            .is_none()),
        "nothing to ask about"
    );
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("shift+r must say something");
    assert!(notice.contains("no "), "{notice}");
    assert!(
        !dir.path().join("keymap.toml").exists(),
        "nothing was written"
    );
}

/// The reset-all button is the mouse form of `shift+r` and the yes
/// button the mouse form of `y`.
#[gpui::test]
fn the_reset_all_button_arms_and_the_yes_button_writes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), USER_KEYMAP_WITH_TWO_ENTRIES).unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_two_user_entries(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    let button = vcx
        .debug_bounds("keybindings-action-shift+r")
        .expect("the reset-all button paints when there is something to reset");
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm),
        Some(keybindings_view::KeybindingConfirm::ResetAll),
        "the button arms"
    );

    let yes = vcx
        .debug_bounds("keybindings-confirm-yes")
        .expect("yes paints");
    vcx.simulate_click(yes.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("written");
    assert!(!text.contains("[[bindings]]"), "{text}");
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .confirm
            .is_none()),
        "and the question is gone"
    );
}

/// After unbinding, acknowledge the affected key and explain how to restore it
/// immediately. The row label can lag until the config watcher reloads the persisted
/// change.
#[gpui::test]
fn d_acknowledges_the_write_immediately_and_names_the_way_back(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    let (_, bound) = selected_row(&shell, &vcx);
    let (key, _) = bound.expect("bound");

    vcx.simulate_keystrokes("d y");
    // Deliberately NOT `run_until_parked` first: the acknowledgement must
    // be on screen the instant the key is pressed, not after the
    // background write, and certainly not after the reload watcher.
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("d must acknowledge the write it just performed");
    assert!(
        notice.contains(&key),
        "the acknowledgement must name the key it silenced: {notice}"
    );
    assert!(
        notice.contains("press r"),
        "and how to get it back — r lifts the shadow: {notice}"
    );
    vcx.run_until_parked();
}

/// Reset removes a context-specific unbinding shadow from its original context. The
/// acknowledgement points to `r` as the recovery action.
#[gpui::test]
fn d_on_a_contexted_binding_names_r_as_the_way_back(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    vcx.simulate_keystrokes("/ f o c u s l e f t");
    vcx.run_until_parked();
    // Keep the filter with Enter. Escape would restore the empty entry
    // query and reset selection to the first row.
    vcx.simulate_keystrokes("enter");
    let (action, bound) = selected_row(&shell, &vcx);
    assert_eq!(
        action.0, "workspace::focus_left",
        "sanity: the filter must land on a row whose builtin binding \
         carries a context (`context = \"workspace\"` in defaults.rs)"
    );
    let (key, _) = bound.expect("sanity: that row is bound");

    vcx.simulate_keystrokes("d y");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("d must acknowledge the write it just spawned");
    assert!(
        notice.contains(&key),
        "it must still name the key: {notice}"
    );
    assert!(
        notice.contains("press r"),
        "r lifts the shadow from the contexted entry: {notice}"
    );
    assert!(
        !notice.contains("type that key again") && !notice.contains("keymap.toml"),
        "neither retired door is promised: {notice}"
    );
    vcx.run_until_parked();
    // That the promise holds for a CONTEXTED shadow is pinned in the pure
    // layer (`keymap::build::tests::
    // a_rebind_of_a_builtin_yields_both_halves_of_the_pair`, whose shadow
    // sits under `context = "workspace"`): a `d`-then-`r` round trip here
    // would need the ~500ms reload the fixture does not run before the
    // in-memory keymap sees the file `d` wrote.
}

/// The user keymap spelled with the `mod` alias, which `render_binding`
/// does not preserve (`mod+y` parses to alt+y under the default alias and
/// renders as `alt+y`). A `d` on the user's own binding removes the key
/// from the file, so it must name it the way the file does.
const USER_KEYMAP_SPELLED_WITH_MOD: &str =
    "config_version = 1\n\n[[bindings]]\n\n[bindings.keys]\n\"mod+y\" = \"palette::toggle\"\n";

fn services_with_a_mod_spelled_user_binding() -> ShellServices {
    let mut services = test_services();
    let builtin = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: USER_KEYMAP_SPELLED_WITH_MOD.parse().unwrap(),
    };
    let (keymap, diags) = build_keymap(&[builtin, user], default_mod(), &services.registry);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = keymap;
    services
}

#[gpui::test]
fn d_on_a_user_binding_spelled_with_mod_removes_it_by_the_files_spelling(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), USER_KEYMAP_SPELLED_WITH_MOD).unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_a_mod_spelled_user_binding(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);
    let (_, bound) = selected_row(&shell, &vcx);
    assert_eq!(
        bound,
        Some(("alt+y".to_string(), Layer::User)),
        "sanity: the row renders the alias resolved, which is not the file's spelling"
    );

    vcx.simulate_keystrokes("d y");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("d acknowledges");
    assert!(
        !notice.contains("press r"),
        "r cannot restore a removed user key, so d must not promise it: {notice}"
    );
    vcx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("written");
    assert!(
        !text.contains("mod+y"),
        "the user's key is removed under the file's own spelling: {text}"
    );
    assert!(!text.contains("none"), "and not shadowed: {text}");
}

/// Unbind acknowledgement describes a pending operation rather than confirmed success.
/// The background write can find no matching entry, so the footer must not promise that
/// the key has already been removed.
#[gpui::test]
fn d_does_not_claim_a_write_it_has_not_confirmed(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    vcx.simulate_keystrokes("d y");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("d must acknowledge");
    assert!(
        !notice.contains("silenced"),
        "the write has not reported yet, so the completed tense is a \
         claim the dialog cannot back: {notice}"
    );
    vcx.run_until_parked();
}

/// Reset acknowledges the queued operation immediately, without claiming its background
/// write has succeeded. The acknowledgement remains useful while row labels wait for
/// reload.
#[gpui::test]
fn r_acknowledges_the_write_it_spawned(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), USER_KEYMAP_TEXT).unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(
        cx,
        services_with_a_user_binding_for_the_palette(),
        dir.path(),
    );
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    vcx.simulate_keystrokes("r y");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("r must acknowledge the write it just spawned");
    assert!(
        notice.contains("ctrl+alt+y"),
        "and name the override it is removing: {notice}"
    );
    vcx.run_until_parked();
}

/// Clearing the query resets selection to row zero and must clear any notice about the
/// previously selected row.
#[gpui::test]
fn clearing_the_query_clears_a_standing_notice(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    vcx.simulate_keystrokes("r");
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .notice
            .is_some()),
        "sanity: r on a builtin row leaves a notice"
    );

    // The ClearQuery rung: still in normal mode, query non-empty.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "",
        "sanity: this escape took the ClearQuery rung"
    );
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .notice
            .is_none()),
        "a rung that moves the selection must not leave the old row's \
         complaint standing"
    );
}

/// Mouse selection changes bypass `handle_key`, so the click path must clear a notice
/// about the previously selected row too.
#[gpui::test]
fn clicking_a_row_clears_a_standing_notice(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    vcx.simulate_keystrokes("r");
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .notice
            .is_some()),
        "sanity: r on row 0 leaves a notice"
    );

    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row_1 = vcx
        .debug_bounds("keybindings-row-1")
        .expect("row 1 should have painted bounds to click into");
    vcx.simulate_mouse_down(
        gpui::point(
            row_1.origin.x + gpui::px(10.0),
            row_1.origin.y + gpui::px(10.0),
        ),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().selected),
        1,
        "sanity: the click selected row 1"
    );
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .notice
            .is_none()),
        "the complaint was about row 0; leaving it up under row 1 is the \
         stale-notice lie in its plainest form"
    );
}

/// Tab and Shift-Tab keep root focus while a modal is in normal mode. The shell root's
/// `GeodeModalOpen` context reclaims them even though the modal panel itself does not
/// own focus. Assert focus directly: an unintended focus cycle could leave the dialog
/// visibly open.
#[gpui::test]
fn tab_in_normal_mode_leaves_focus_on_the_shell_root(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    let shell_focus = shell.read_with(&cx, |s, _| s.focus_handle.clone());

    assert!(
        cx.update(|window, _| shell_focus.is_focused(window)),
        "sanity: the keybinding dialog opens in normal mode, which parks \
         focus on the shell root"
    );

    cx.simulate_keystrokes("tab");
    assert!(
        cx.update(|window, _| shell_focus.is_focused(window)),
        "tab in normal mode must leave focus on the shell root: focus \
         walking off it is how a caret lands on a field behind an open \
         modal"
    );

    cx.simulate_keystrokes("shift-tab");
    assert!(
        cx.update(|window, _| shell_focus.is_focused(window)),
        "shift+tab is the same affordance by another name and must be \
         reclaimed with it"
    );
}

/// Dialog synchronization owns input text and focus after each transition: entering and
/// leaving filter mode, clearing a query, starting capture, and canceling it. Assert
/// that the visible input and focused surface match the state at every step.
#[gpui::test]
fn focus_and_text_follow_the_pure_state_through_every_transition(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "opens in normal mode: shell root holds the keys"
    );
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    cx.simulate_input("pal");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter"); // leave filter, keep the query
    cx.run_until_parked();
    assert!(!dialog_filter_is_focused(&shell, &mut cx));
    let text = shell.read_with(&cx, |shell, cx| {
        shell.dialog_input.read(cx).value().to_string()
    });
    assert_eq!(
        text, "pal",
        "leaving filter mode by enter keeps the query in the field"
    );
    cx.simulate_keystrokes("escape"); // clear the query
    cx.run_until_parked();
    let text = shell.read_with(&cx, |shell, cx| {
        shell.dialog_input.read(cx).value().to_string()
    });
    assert_eq!(
        text, "",
        "clearing the query empties the field through the sync"
    );
    cx.simulate_keystrokes("/");
    cx.simulate_input("pal");
    cx.run_until_parked();
    // A row click starts capture while retaining Filter as the underlying
    // mode. Enter would leave filter mode instead of starting capture.
    let row = top_match_bounds(&shell, &mut cx);
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(10.0), row.origin.y + gpui::px(10.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "listening: the shell root reads raw keys"
    );
    cx.simulate_keystrokes("escape"); // cancel the capture
    cx.run_until_parked();
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "back to the mode underneath: filter"
    );
}

// Mouse parity.

/// Clicking the frozen filter row enters filter mode and focuses the shared input,
/// matching `/`.
#[gpui::test]
fn clicking_the_frozen_filter_row_enters_filter_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell_with(cx, test_services(), "keybindings::open");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().mode),
        DialogMode::Normal
    );
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("the frozen filter row paints in normal mode");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().mode),
        DialogMode::Filter,
        "a click on the field is the mouse form of /"
    );
    let input_focused = cx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.read(cx).focus_handle(cx).is_focused(window)
    });
    assert!(input_focused, "the sync handed the keyboard to the Input");
    // And typing now filters rather than acting as a verb.
    cx.simulate_input("j");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "j"
    );
}

/// The one frozen state where `/` is NOT the filter's key: a capture in
/// progress. A click on the field there cancels the capture and enters
/// filter mode — a click on a text field is never a keystroke to bind.
#[gpui::test]
fn clicking_the_frozen_filter_row_while_listening_cancels_the_capture(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx) = dialog_test_shell_with(cx, test_services(), "keybindings::open");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| {
        s.keybindings.as_ref().unwrap().listening.is_some()
    }));
    let row = cx
        .debug_bounds("dialog-filter-frozen")
        .expect("the row is frozen while listening");
    cx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    let (listening, mode) = shell.read_with(&cx, |s, _| {
        let k = s.keybindings.as_ref().unwrap();
        (k.listening.is_some(), k.mode)
    });
    assert!(!listening, "the capture was cancelled");
    assert_eq!(mode, DialogMode::Filter);
}

/// Filter down to the recording module's own row and leave filter mode —
/// the fragment twin of [`select_the_palette_row`].
fn select_the_module_row(cx: &mut gpui::VisualTestContext) {
    // Use `noop` to select the intended action. Several recorder actions have titles
    // beginning "Recording", but only the no-op title matches this query, so selection
    // cannot drift to an unbound editor action.
    cx.simulate_keystrokes("/ n o o p");
    cx.run_until_parked();
    // Keep the filter with Enter, as in select_the_palette_row.
    cx.simulate_keystrokes("enter");
}

/// Module fragments supply builtin bindings. Unbinding one writes a user-layer `"none"`
/// shadow, because the dialog cannot remove the compiled-in fragment.
#[gpui::test]
fn d_over_a_modules_fragment_binding_writes_a_user_layer_shadow(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (services, _log) = services_with_a_module_fragment(REC_FRAGMENT);
    let (window, mut vcx) = open_shell_with_user_dir(cx, services, dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_module_row(&mut vcx);
    let (action, bound) = selected_row(&shell, &vcx);
    let (key, layer) = bound.expect("the module's row must carry its fragment binding");
    assert_eq!(action.0, "rec::noop", "sanity: the filter landed on it");
    assert_eq!(key, "q");
    assert_eq!(
        layer,
        Layer::Builtin,
        "a fragment binding reports Builtin, which is what selects `d`'s shadow branch"
    );

    vcx.simulate_keystrokes("d y");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml"))
        .expect("d must write the user keymap");
    // `toml_edit` writes a bare key unquoted, so the shadow reads
    // `q = "none"` — the same document `build_keymap` reads back.
    assert!(
        text.contains("q = \"none\""),
        "a module's fragment binding is silenced, not removed: {text}"
    );
    assert!(
        text.contains("context = \"rec\""),
        "and the shadow must carry the fragment's own context, or it would \
         silence `q` everywhere: {text}"
    );
}

/// Reset removes the user's binding override and reveals the module fragment beneath
/// it, verifying that fragment defaults remain below editable user config.
#[gpui::test]
fn r_removes_a_user_override_and_the_modules_fragment_shows_through(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    // The file and the in-memory keymap must agree: the writer edits the
    // file, so a fixture whose keymap claims an override the file lacks
    // would find nothing to remove (see `USER_KEYMAP_TEXT`'s own comment).
    let user_text = "config_version = 1\n\n[[bindings]]\ncontext = \"rec\"\n\n[bindings.keys]\n\"ctrl+alt+y\" = \"rec::noop\"\n";
    std::fs::write(dir.path().join("keymap.toml"), user_text).unwrap();
    let (mut services, _log) = services_with_a_module_fragment(REC_FRAGMENT);
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: dir.path().join("keymap.toml"),
        table: user_text.parse().unwrap(),
    };
    services.keymap =
        test_keymap_with_fragments(&services.registry, &services.keymap_fragments, &[user]);
    let (window, mut vcx) = open_shell_with_user_dir(cx, services, dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_module_row(&mut vcx);
    let (action, bound) = selected_row(&shell, &vcx);
    let (key, layer) = bound.expect("bound");
    assert_eq!(action.0, "rec::noop");
    assert_eq!(
        (key.as_str(), layer),
        ("ctrl+alt+y", Layer::User),
        "fixture check: the user's override is the effective binding, over the fragment"
    );

    vcx.simulate_keystrokes("r y");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("r must write");
    assert!(
        !text.contains("ctrl+alt+y"),
        "reset removes the user's key so the module's fragment shows through: {text}"
    );
    assert!(
        !text.contains("none"),
        "reset must never write a shadow — that would bury the fragment it is \
         meant to uncover: {text}"
    );
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("r acknowledges the write it spawned");
    assert!(
        !notice.contains("no user override"),
        "a reset that had something to reset reports no complaint: {notice}"
    );
}

/// `d` arms confirmation without writing; `y` confirms and `n` cancels. The
/// confirmation row replaces the action bar while armed.
#[gpui::test]
fn d_asks_before_writing_and_n_withdraws(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);

    vcx.simulate_keystrokes("d");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .confirm
            .is_some()),
        "d arms the question"
    );
    assert!(
        vcx.debug_bounds("keybindings-confirm").is_some(),
        "and it paints"
    );
    assert!(
        vcx.debug_bounds("keybindings-action-d").is_none(),
        "the action bar is replaced by the question"
    );
    assert!(
        !dir.path().join("keymap.toml").exists(),
        "nothing is written while the question stands"
    );

    // A stray verb is claimed and dropped while armed.
    vcx.simulate_keystrokes("r");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm
            == Some(keybindings_view::KeybindingConfirm::Unbind)),
        "r under an armed d neither re-arms nor acts"
    );

    vcx.simulate_keystrokes("n");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .confirm
            .is_none()),
        "n withdraws it"
    );
    assert!(vcx.debug_bounds("keybindings-confirm").is_none());
    assert!(!dir.path().join("keymap.toml").exists());
}

/// The `r` half of [`d_asks_before_writing_and_n_withdraws`]: a bare `r`
/// on a row whose binding IS the user's own arms `Reset` and writes
/// nothing until `y`; `n` withdraws it and the file is untouched either
/// way. Opened on `services_with_a_user_binding_for_the_palette` (rather
/// than a builtin row, which would only give the unarmed notice) so a
/// write really would be observable if the arm regressed to a
/// write-through.
#[gpui::test]
fn r_asks_before_writing_and_n_withdraws(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), USER_KEYMAP_TEXT).unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(
        cx,
        services_with_a_user_binding_for_the_palette(),
        dir.path(),
    );
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);

    vcx.simulate_keystrokes("r");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().confirm
            == Some(keybindings_view::KeybindingConfirm::Reset)),
        "r arms the question"
    );
    assert!(
        vcx.debug_bounds("keybindings-confirm").is_some(),
        "and it paints"
    );
    assert!(
        vcx.debug_bounds("keybindings-action-r").is_none(),
        "the action bar is replaced by the question"
    );
    let unchanged = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("still there");
    assert_eq!(
        unchanged, USER_KEYMAP_TEXT,
        "nothing is written while the question stands"
    );

    vcx.simulate_keystrokes("n");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .confirm
            .is_none()),
        "n withdraws it"
    );
    assert!(vcx.debug_bounds("keybindings-confirm").is_none());
    let still_unchanged =
        std::fs::read_to_string(dir.path().join("keymap.toml")).expect("still there");
    assert_eq!(still_unchanged, USER_KEYMAP_TEXT, "n writes nothing either");
}

/// While confirmation is armed, row clicks and frozen-filter clicks are claimed without
/// changing selection or mode.
#[gpui::test]
fn a_row_click_while_a_confirm_is_armed_is_dropped(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);
    vcx.simulate_keystrokes("d");
    vcx.run_until_parked();

    // `keybindings-row-{n}` is keyed by the row's index in the FULL,
    // unfiltered row list (see `build`'s render loop), not by its
    // position under the "palette" filter — so the row to click is the
    // one actually visible under that filter (the selected row itself,
    // per this test's own doc comment), not literally row 0.
    let row_ix = shell.read_with(&vcx, |s, _| {
        let rows = keybindings_view::derive_rows(&s.services.registry, &s.services.keymap);
        let state = s.keybindings.as_ref().unwrap();
        let visible = keybindings_view::visible_rows(state, &rows);
        visible[state.selected].row
    });
    let row_selector: &'static str =
        Box::leak(format!("keybindings-row-{row_ix}").into_boxed_str());
    let row = vcx.debug_bounds(row_selector).expect("a row paints");
    vcx.simulate_mouse_down(
        gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| {
            let state = s.keybindings.as_ref().unwrap();
            state.confirm.is_some() && state.listening.is_none()
        }),
        "the click neither retargeted nor started a capture"
    );

    let frozen = vcx
        .debug_bounds("dialog-filter-frozen")
        .expect("frozen row paints");
    vcx.simulate_mouse_down(
        gpui::point(
            frozen.origin.x + gpui::px(20.0),
            frozen.origin.y + gpui::px(4.0),
        ),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| {
            let state = s.keybindings.as_ref().unwrap();
            state.confirm.is_some() && state.mode == DialogMode::Normal
        }),
        "the frozen-row click did not enter filter mode over an open question"
    );
}

/// The two verbs are buttons too, and the button arms exactly as the
/// key does; the yes button writes.
#[gpui::test]
fn the_unbind_button_arms_and_the_yes_button_writes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_the_palette_row(&mut vcx);
    let (_, bound) = selected_row(&shell, &vcx);
    let (key, _) = bound.expect("bound");

    let button = vcx
        .debug_bounds("keybindings-action-d")
        .expect("the unbind button paints");
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("keybindings-confirm").is_some(),
        "the button arms"
    );

    let yes = vcx
        .debug_bounds("keybindings-confirm-yes")
        .expect("yes paints");
    vcx.simulate_click(yes.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("written");
    assert!(text.contains(&format!("\"{key}\" = \"none\"")), "{text}");
    assert!(
        shell.read_with(&vcx, |s, _| s
            .keybindings
            .as_ref()
            .unwrap()
            .confirm
            .is_none()),
        "and the question is gone"
    );
}

// Shared motions: an edit of a Motion row is global.

/// Old dialog rebinds of the retired `blotter::down` and `pricer::down`,
/// each written into its module's context with a `"none"` over the `j`
/// the module shipped then. `j` now ships once under the grid context, so
/// both shadows are orphans, and both old keys bind `motion::down`.
const OLD_DOWN_OVERRIDES: &str = "config_version = 1\n\n\
     [[bindings]]\ncontext = \"blotter && mode == visual\"\n[bindings.keys]\n\
     \"n\" = \"blotter::down\"\n\"j\" = \"none\"\n\n\
     [[bindings]]\ncontext = \"pricer && mode == normal\"\n[bindings.keys]\n\
     \"m\" = \"pricer::down\"\n\"j\" = \"none\"\n";

/// The renames the blotter and pricer factories register for `down`.
fn register_down_renames(reg: &mut ActionRegistry) {
    reg.register_rename("blotter::down", "motion::down")
        .unwrap();
    reg.register_rename("pricer::down", "motion::down").unwrap();
}

/// The shipped keymap under the user `text`, with the down renames, as the
/// app builds it. Returns the build diagnostics beside the keymap.
fn motion_keymap_under(text: &str) -> (crate::keymap::Keymap, Vec<geode_core::config::Diagnostic>) {
    let mut reg = ActionRegistry::default();
    register_builtin_actions(&mut reg);
    register_down_renames(&mut reg);
    let builtin = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: text.parse().unwrap(),
    };
    build_keymap(&[builtin, user], default_mod(), &reg)
}

fn services_with_old_down_overrides() -> ShellServices {
    let mut services = test_services();
    register_down_renames(&mut services.registry);
    let builtin = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: OLD_DOWN_OVERRIDES.parse().unwrap(),
    };
    let (keymap, diags) = build_keymap(&[builtin, user], default_mod(), &services.registry);
    assert_eq!(diags.len(), 2, "one rename warning per old id: {diags:?}");
    services.keymap = keymap;
    services
}

/// Filter to the Motion: down row ("Cursor down") and keep the query.
fn select_motion_down(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext) {
    vcx.simulate_keystrokes("/ c u r s o r space d o w n");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("enter");
    let (action, _) = selected_row(shell, vcx);
    assert_eq!(action.0, "motion::down", "fixture: the Motion: down row");
}

/// What `key` does in a grid tile of `module` in `mode` under `keymap`.
fn press_in_grid(
    keymap: &crate::keymap::Keymap,
    module: &str,
    mode: &str,
    key: &str,
) -> crate::keymap::MatchResult {
    use crate::keymap::{KeyContext, Matcher, parse_keystroke};
    let stack = vec![
        KeyContext::new("workspace"),
        KeyContext::new("tile"),
        KeyContext::new(module).grid().pair("mode", mode),
    ];
    let ks = parse_keystroke(key, default_mod()).unwrap();
    Matcher::default().press(keymap, ks, &stack)
}

fn moves_down() -> crate::keymap::MatchResult {
    crate::keymap::MatchResult::Matched {
        action: ActionId("motion::down".to_string()),
        count: None,
    }
}

/// One `r` on the Motion row reaches old overrides in every module context
/// they were written under, orphan `"none"` shadows included, and `j`
/// moves both grids again afterwards.
#[gpui::test]
fn r_on_a_motion_row_removes_old_overrides_from_every_module_context(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), OLD_DOWN_OVERRIDES).unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_old_down_overrides(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_motion_down(&shell, &mut vcx);

    vcx.simulate_keystrokes("r y");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("r acknowledges");
    assert!(notice.contains("4 overrides"), "{notice}");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("r must write");
    for gone in ["blotter::down", "pricer::down", "none"] {
        assert!(!text.contains(gone), "{gone} survived the reset: {text}");
    }
    let (keymap, diags) = motion_keymap_under(&text);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        press_in_grid(&keymap, "blotter", "visual", "j"),
        moves_down()
    );
    assert_eq!(
        press_in_grid(&keymap, "pricer", "normal", "j"),
        moves_down()
    );
}

/// Rebinding the Motion row while old module overrides are displayed
/// writes the shared grid context, not the displayed module one, and
/// clears the old overrides in the same write: the new key moves every
/// grid, and the old ids' keys and shadows are gone.
#[gpui::test]
fn rebinding_a_motion_row_writes_the_shared_context_and_clears_old_overrides(
    cx: &mut gpui::TestAppContext,
) {
    use crate::defaults::GRID_MOTION_CONTEXT;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), OLD_DOWN_OVERRIDES).unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_old_down_overrides(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_motion_down(&shell, &mut vcx);
    let (_, shown) = selected_row(&shell, &vcx);
    assert_eq!(
        shown.map(|(_, layer)| layer),
        Some(Layer::User),
        "fixture: an old module override is what the row displays"
    );

    vcx.simulate_keystrokes("enter n enter");
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("must write");
    let table: toml::Table = text.parse().unwrap();
    let entries = table["bindings"].as_array().unwrap();
    let keys_under = |ctx: &str| -> Vec<(String, String)> {
        entries
            .iter()
            .filter(|e| e.get("context").and_then(|c| c.as_str()) == Some(ctx))
            .flat_map(|e| e["keys"].as_table().unwrap().clone())
            .map(|(k, v)| (k, v.as_str().unwrap().to_string()))
            .collect()
    };
    let mut grid = keys_under(GRID_MOTION_CONTEXT);
    grid.sort();
    assert_eq!(
        grid,
        vec![
            ("j".to_string(), "none".to_string()),
            ("n".to_string(), "motion::down".to_string()),
        ],
        "{text}"
    );
    for module in ["blotter && mode == visual", "pricer && mode == normal"] {
        assert!(keys_under(module).is_empty(), "{module} kept: {text}");
    }

    let (keymap, diags) = motion_keymap_under(&text);
    assert!(diags.is_empty(), "no old id left to warn about: {diags:?}");
    for (module, mode) in [
        ("blotter", "visual"),
        ("blotter", "normal"),
        ("pricer", "normal"),
        ("marketdata", "normal"),
        ("diagnostics", "normal"),
    ] {
        assert_eq!(
            press_in_grid(&keymap, module, mode, "n"),
            moves_down(),
            "{module} {mode}"
        );
    }
}

/// `d` on the Motion row is global too: the old module overrides go, and
/// the shipped key is silenced under the shared context, where `r` can
/// lift it.
#[gpui::test]
fn d_on_a_motion_row_clears_old_overrides_and_silences_the_shared_key(
    cx: &mut gpui::TestAppContext,
) {
    use crate::defaults::GRID_MOTION_CONTEXT;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("keymap.toml"), OLD_DOWN_OVERRIDES).unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_old_down_overrides(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    select_motion_down(&shell, &mut vcx);

    vcx.simulate_keystrokes("d y");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("d acknowledges");
    assert!(
        notice.contains("silencing j") && notice.contains("press r"),
        "{notice}"
    );
    vcx.run_until_parked();

    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).expect("d must write");
    assert!(
        !text.contains("blotter::down") && !text.contains("pricer::down"),
        "{text}"
    );
    let (keymap, diags) = motion_keymap_under(&text);
    assert!(diags.is_empty(), "{diags:?}");
    for (module, mode) in [("blotter", "visual"), ("pricer", "normal")] {
        assert_eq!(
            press_in_grid(&keymap, module, mode, "j"),
            crate::keymap::MatchResult::NoMatch,
            "{module}: {text}"
        );
    }
    let down = ActionId("motion::down".to_string());
    assert_eq!(
        crate::keymap::user_overrides_for(keymap.bindings(), &down),
        vec![crate::keymap::UserOverride {
            context_source: Some(GRID_MOTION_CONTEXT.to_string()),
            key: "j".to_string(),
        }],
        "only the shared shadow is left for r to lift"
    );
}

// ---- Prepared rows ----------------------------------------------------

/// Painted row indices, top to bottom, read from the `keybindings-row-{ix}`
/// selectors (`ix` is the row's index in the full list).
fn painted_keybinding_rows(cx: &mut gpui::VisualTestContext, total: usize) -> Vec<usize> {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let mut painted: Vec<(gpui::Pixels, usize)> = (0..total)
        .filter_map(|ix| {
            let selector: &'static str =
                Box::leak(format!("keybindings-row-{ix}").into_boxed_str());
            cx.debug_bounds(selector).map(|b| (b.origin.y, ix))
        })
        .collect();
    painted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    painted.into_iter().map(|(_, ix)| ix).collect()
}

/// The rows a fresh derivation would show now, as full-list indices in display
/// order, and their rows.
fn fresh_keybinding_rows(
    shell: &Entity<ShellView>,
    cx: &gpui::VisualTestContext,
) -> (Vec<usize>, Vec<keybindings_view::KeybindingRow>) {
    shell.read_with(cx, |shell, _| {
        let rows = keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap);
        let state = shell.keybindings.as_ref().expect("dialog open");
        let visible = keybindings_view::visible_rows(state, &rows);
        (visible.iter().map(|m| m.row).collect(), rows)
    })
}

fn assert_keybinding_rows_are_fresh(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) {
    let (order, rows) = fresh_keybinding_rows(shell, cx);
    let prepared = shell.read_with(cx, |shell, _| {
        shell.keybindings.as_ref().unwrap().rows.rows().to_vec()
    });
    assert_eq!(prepared, rows, "the prepared rows are a fresh derivation");
    assert_eq!(
        painted_keybinding_rows(cx, rows.len()),
        order,
        "the painted rows are the fresh ranking"
    );
}

#[gpui::test]
fn typing_reranks_keybinding_rows_without_re_deriving(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    let derives = shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().rows.derives);
    cx.simulate_keystrokes("/");
    cx.simulate_input("focus");
    cx.run_until_parked();
    assert_keybinding_rows_are_fresh(&shell, &mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().rows.derives),
        derives,
        "typing re-ranks the rows already derived"
    );
}

#[gpui::test]
fn escape_clears_the_keybinding_query_and_the_rows_follow(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
    cx.simulate_input("focus");
    cx.simulate_keystrokes("enter escape");
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.is_empty()));
    assert_keybinding_rows_are_fresh(&shell, &mut cx);
}

/// An external `keymap.toml` write while the dialog is open: the reload the
/// watcher runs must repaint the rows with the new binding.
#[gpui::test]
fn a_keymap_reload_repaints_the_keybinding_rows(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);
    std::fs::write(
        dir.path().join("keymap.toml"),
        "config_version = 1\n[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+shift+z\" = \"palette::toggle\"\n",
    )
    .unwrap();
    let builtin = shell.read_with(&vcx, |s, _| s.services.builtin.clone());
    let config = crate::reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut vcx, |s, cx| s.apply_reload(config, cx));
    vcx.run_until_parked();
    assert_keybinding_rows_are_fresh(&shell, &mut vcx);
    let rebound = shell.read_with(&vcx, |s, _| {
        s.keybindings
            .as_ref()
            .unwrap()
            .rows
            .rows()
            .iter()
            .find(|r| r.action.0 == "palette::toggle")
            .and_then(|r| r.current.as_ref())
            .map(|b| crate::palette::render_binding(&b.keystrokes))
    });
    assert_eq!(
        rebound.as_deref(),
        Some("alt+shift+z"),
        "the row shows the reloaded binding"
    );
}

/// A missed refresh is refused at render, never repaired there.
#[gpui::test]
#[should_panic(expected = "prepared rows are stale")]
fn render_refuses_keybinding_rows_a_refresh_missed(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    shell.update(&mut cx, |s, _| s.config_revision += 1);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// A rebind captured inside the open dialog writes `keymap.toml` through the
/// ordered writer and changes nothing in memory; the reload that write
/// triggers bumps the config revision, and the very next paint shows the new
/// binding with no other event.
#[gpui::test]
fn a_rebind_inside_the_dialog_repaints_its_row_on_the_reload(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, test_services(), dir.path(), "keybindings::open");
    select_the_palette_row(&mut cx);
    let (action, _) = selected_row(&shell, &cx);
    assert_eq!(action.0, "palette::toggle");
    let revision = shell.read_with(&cx, |s, _| s.config_revision);
    cx.simulate_keystrokes("enter ctrl-alt-q enter");
    cx.run_until_parked();
    assert!(
        std::fs::read_to_string(dir.path().join("keymap.toml"))
            .expect("the capture wrote keymap.toml")
            .contains("palette::toggle"),
        "the rebind went through the writer"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.config_revision),
        revision,
        "a write changes nothing in memory until its reload"
    );
    // The reload the watcher runs once it sees the write.
    let builtin = shell.read_with(&cx, |s, _| s.services.builtin.clone());
    let config = crate::reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut cx, |s, cx| s.apply_reload(config, cx));
    assert_keybinding_rows_are_fresh(&shell, &mut cx);
    let shown = shell.read_with(&cx, |s, _| {
        let state = s.keybindings.as_ref().unwrap();
        state
            .rows
            .at(state.selected)
            .and_then(|r| r.current.as_ref())
            .map(|b| crate::palette::render_binding(&b.keystrokes))
    });
    assert_eq!(
        shown.as_deref(),
        Some("ctrl+alt+q"),
        "the selected row shows the binding just captured"
    );
}

/// A row click resolves against the painted list: under a filter, clicking the
/// painted top row selects that row's action.
#[gpui::test]
fn a_click_selects_the_painted_keybinding_row(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    select_the_palette_row(&mut cx);
    cx.simulate_keystrokes("j");
    let bounds = top_match_bounds(&shell, &mut cx);
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let (order, rows) = fresh_keybinding_rows(&shell, &cx);
    let (selected_action, _) = selected_row(&shell, &cx);
    assert_eq!(
        selected_action, rows[order[0]].action,
        "the click selected the painted top row"
    );
    assert_keybinding_rows_are_fresh(&shell, &mut cx);
}
