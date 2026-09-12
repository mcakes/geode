//! The keybindings dialog: rows, filtering, selection, listening for a
//! new binding, and persisting it to the user keymap file.

use super::*;
use crate::dialogmode::DialogMode;

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

/// Opening the dialog leaves the shared filter BLURRED (it used to be
/// focused): the dialog opens in normal mode, where a bare letter is a
/// verb, and a focused `Input` would eat every one of them as text. `/`
/// is what hands the field focus.
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

/// Inside filter mode the retired vim motion is plain text again: `j`
/// types a `j` and leaves the selection where it was. (Outside it, `j`
/// moves — `j_and_k_move_in_normal_mode` below.)
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
    // From filter mode, where the blur is a real transition rather than
    // the state the dialog already sits in.
    cx.simulate_keystrokes("/");
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

/// Escape cancels the capture, refocuses the filter (the capture was
/// started from filter mode, so that is the surface it hands focus back
/// to — see `cancelling_a_capture_restores_focus_to_the_mode_that_
/// started_it` for the normal-mode half), and leaves both the query and
/// the dialog itself alone.
#[gpui::test]
fn escape_cancels_a_capture_without_closing_the_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
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

/// Fix round 1, Finding 2: `dialog::init_reclaimed_keybindings`'s `ctrl-a`
/// reclaim is scoped to `"GeodeModal > Input"` — every Geode modal's
/// `Input`, not just the picker's own (that reclaim's doc comment used to
/// overclaim the opposite). This dialog's shared filter is exactly such
/// an `Input`, so `ctrl-a` here must be swallowed (never reaching
/// gpui-component's own `SelectAll`/`MoveHome` binding, which would
/// otherwise select-all-then-overtype or jump the caret home) while the
/// filter keeps accepting ordinary typed text around it.
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

/// §17.1 rule 2: a row click does what `enter` would. One click on a row
/// that is not the selected one both selects it and starts listening —
/// the second click the old rule required was one step short of
/// everything a mouse user came for.
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

// --- The two-mode interaction model (spec
// `2026-09-08-geode-dialog-interaction-model-design.md`) --------------

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

/// §18.1: the mode pill lives in the modal's title row, not in the
/// dialog's content, and the frozen empty filter shows a placeholder.
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

/// §18.1's placeholder claims `/` opens the filter. While this dialog is
/// *listening* for a capture that is false — `press_while_listening`
/// takes every keystroke, so `/` would become the new binding — and the
/// footer beside it already reads "Listening — type keys". Two
/// contradictory claims on one panel is worse than the bare search icon
/// that state showed before §18.1, so the placeholder goes away for the
/// length of the capture and comes back when `escape` ends it.
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

/// The ladder, one visible step at a time: filter → normal keeping the
/// query, → clear the query, → close. A dialog that skipped a rung would
/// close on the first escape and lose the user's filter with it.
#[gpui::test]
fn escape_walks_the_ladder_one_rung_at_a_time(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/ t h e m e");
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    let (mode, q) = shell.read_with(&cx, |s, _| {
        let k = s.keybindings.as_ref().unwrap();
        (k.mode, k.query.clone())
    });
    assert_eq!(mode, crate::dialogmode::DialogMode::Normal);
    assert_eq!(q, "theme", "leaving filter must keep the query applied");
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "and must blur the filter, or normal mode's letters would still type"
    );

    cx.simulate_keystrokes("escape");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "",
        "the second escape clears the query"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "and does not close"
    );

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_none()),
        "the third closes"
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
    cx.simulate_keystrokes("escape escape");
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

/// The same rule for the mouse: a click that only selects a row hands
/// focus back to the filter *in filter mode*, and leaves it blurred in
/// normal mode.
#[gpui::test]
fn a_selecting_click_in_normal_mode_leaves_the_filter_blurred(cx: &mut gpui::TestAppContext) {
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

    // Escape twice: out of filter mode (query kept), then the
    // `ClearQuery` rung.
    cx.simulate_keystrokes("escape escape");
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

/// Escape means escape whatever else is held down. Normal mode claims
/// every key it does not understand, so a `shift+escape` that only the
/// *bare* guard recognised would be swallowed and do nothing at all —
/// where before this dialog went modal it fell through to
/// `handle_key_down`'s close, which never looked at modifiers
/// (`input.rs`: `event.keystroke.key == "escape"`). A key that visibly
/// does nothing is the defect class this whole interaction model exists
/// to remove, so both rungs reachable from here take a modified escape
/// exactly as they take a bare one.
#[gpui::test]
fn a_modified_escape_walks_the_same_ladder_as_a_bare_one(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
    cx.simulate_keystrokes("/");
    cx.simulate_input("theme");
    cx.run_until_parked();

    cx.simulate_keystrokes("shift-escape");
    let (mode, query) = shell.read_with(&cx, |s, _| {
        let k = s.keybindings.as_ref().unwrap();
        (k.mode, k.query.clone())
    });
    assert_eq!(
        mode,
        crate::dialogmode::DialogMode::Normal,
        "shift+escape must leave filter mode, exactly as escape does"
    );
    assert_eq!(query, "theme", "and keep the query applied");

    cx.simulate_keystrokes("ctrl-escape");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.keybindings.as_ref().unwrap().query.clone()),
        "",
        "ctrl+escape must clear the query, exactly as escape does"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_some()),
        "and not close on that rung"
    );

    cx.simulate_keystrokes("alt-escape");
    assert!(
        shell.read_with(&cx, |s, _| s.modal.is_none()),
        "alt+escape on the last rung must close the dialog, not be \
         swallowed by normal mode's claim-and-drop"
    );
}

// --- Task 4: `d` unbinds, `r` resets ---------------------------------
//
// The capability the whole modal model exists to prove: before it, the
// only way to clear a binding was to hand-edit `keymap.toml`.

/// Open the keybinding dialog on an already-open shell and draw, so the
/// modal's key handler is live. The preamble every Task 4 test below
/// shares (`dialog_test_shell` can't be used: these need a real
/// `user_dir`, which only `open_shell_with_user_dir` supplies).
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

/// Filter down to the palette row and leave filter mode, so the tests
/// below act on a row with a known, non-empty binding rather than
/// whatever happens to sort first.
fn select_the_palette_row(cx: &mut gpui::VisualTestContext) {
    cx.simulate_keystrokes("/ p a l e t t e");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
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

    vcx.simulate_keystrokes("d");
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

    vcx.simulate_keystrokes("d");
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
    vcx.simulate_keystrokes("r");
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
        .expect("r acknowledges the write it spawned (whole-branch review, Minor 3)");
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

// --- Task 4 fix round 1 ----------------------------------------------

/// A user layer that silences BOTH of `palette::toggle`'s builtin keys
/// with the `"none"` shadow `d` writes — i.e. the exact on-disk state a
/// user reaches by pressing `d` on that row twice. The row then derives
/// `current: None`, which is where `r`'s message used to lie.
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

/// Fix round 1, Important 1. A row silenced by the user's own `"none"`
/// shadow HAS a user override — the shadow is one — so telling the user
/// there is none is false, and it steers them away from the one
/// recovery that works (`enter`, then retyping the key, overwrites the
/// shadow in place). The row no longer shows the key, so the message is
/// the only place that recovery can come from.
#[gpui::test]
fn r_on_a_silenced_row_names_the_recovery_instead_of_denying_the_override(
    cx: &mut gpui::TestAppContext,
) {
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
        "sanity: a shadowed row derives as unbound — that is why the \
         message is the only thing left to guide the user"
    );

    vcx.simulate_keystrokes("r");
    vcx.run_until_parked();

    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("r must say something");
    assert!(
        !notice.contains("no user override"),
        "the `\"none\"` shadow IS the user's override; denying it is a \
         lie: {notice}"
    );
    assert!(
        notice.contains("enter"),
        "and the message must name the recovery — enter, then retyping \
         the key: {notice}"
    );
}

/// Fix round 1, Important 3. `d` is one bare, unmodified key performing
/// an immediate destructive disk write, and the row does not relabel
/// until the ~500ms config watcher gets to it — so the acknowledgement
/// cannot wait on the reload. It names the key that was silenced and how
/// to bring it back.
#[gpui::test]
fn d_acknowledges_the_write_immediately_and_names_the_way_back(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    let (_, bound) = selected_row(&shell, &vcx);
    let (key, _) = bound.expect("bound");

    vcx.simulate_keystrokes("d");
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
    assert!(notice.contains("enter"), "and how to get it back: {notice}");
    vcx.run_until_parked();
}

/// Whole-branch review, Important 2. The `enter`-then-retype recovery is
/// exact only for a binding with NO context. Roughly 60 of the ~80
/// builtin bindings carry one, and for those `d` writes `"none"` into the
/// *contexted* entry while the recovery rebind — whose row is unbound by
/// then, so it passes `context: None` — writes the no-context entry
/// instead. Different table, not an overwrite: recovery is silently
/// defeated when array order puts the new entry first, and escalates the
/// binding from contexted to global even when it appears to work.
///
/// The ruling was to fix the HONESTY, not the mechanism (making contexted
/// recovery work needs the row to carry its pre-shadow context — the same
/// row-vocabulary change as the parked `suppressed_by` follow-up). So the
/// contract this pins is negative: on a contexted row the message must
/// not promise the retype.
#[gpui::test]
fn d_on_a_contexted_binding_does_not_promise_the_retype_recovery(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    vcx.simulate_keystrokes("/ f o c u s l e f t");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("escape");
    let (action, bound) = selected_row(&shell, &vcx);
    assert_eq!(
        action.0, "workspace::focus_left",
        "sanity: the filter must land on a row whose builtin binding \
         carries a context (`context = \"workspace\"` in defaults.rs)"
    );
    let (key, _) = bound.expect("sanity: that row is bound");

    vcx.simulate_keystrokes("d");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("d must acknowledge the write it just spawned");
    assert!(
        notice.contains(&key),
        "it must still name the key: {notice}"
    );
    assert!(
        !notice.contains("type that key again"),
        "the retype recovery does not hold for a contexted binding — it \
         writes the no-context entry, leaving the `\"none\"` shadow \
         standing in the contexted one: {notice}"
    );
    assert!(
        notice.contains("keymap.toml"),
        "so the message must point at the one way back that does work: \
         {notice}"
    );
    vcx.run_until_parked();
}

/// Whole-branch review, Minor 3. `d`'s acknowledgement was past tense
/// ("silenced") while the write it describes is still on the background
/// executor and can come back `removed: false` — a stale row, or a key
/// the user file spells differently from `render_binding` — in which case
/// only stderr ever says otherwise. The footer must not assert an outcome
/// it has not confirmed.
#[gpui::test]
fn d_does_not_claim_a_write_it_has_not_confirmed(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    open_keybindings(&shell, &mut vcx);

    select_the_palette_row(&mut vcx);
    vcx.simulate_keystrokes("d");
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

/// Whole-branch review, Minor 3, the other half: `r`'s success path said
/// nothing at all, so a reset whose `apply_unbind` came back
/// `removed: false` looked exactly like one that worked — and even a
/// reset that DID work is invisible until the ~500ms watcher relabels the
/// row. It acknowledges, in the same unconfirmed tense `d` uses.
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
    vcx.simulate_keystrokes("r");
    let notice = shell
        .read_with(&vcx, |s, _| s.keybindings.as_ref().unwrap().notice.clone())
        .expect("r must acknowledge the write it just spawned");
    assert!(
        notice.contains("ctrl+alt+y"),
        "and name the override it is removing: {notice}"
    );
    vcx.run_until_parked();
}

/// Fix round 1, Important 2 (path 1 of 2): the `EscapeStep::ClearQuery`
/// rung resets the selection to row 0, so a notice about the row the
/// user *was* on becomes a complaint pointing at a different row.
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

/// Fix round 1, Important 2 (path 2 of 2): a mouse click is the other
/// door into this dialog's state, and it changes the selected row
/// without going through `handle_key` at all.
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

/// Whole-branch review, Important 1: `tab` must not walk focus off the
/// shell root while a modal is open in **normal mode**.
///
/// The `tab`/`shift+tab` → `NoAction` reclaim
/// (`dialog::init_reclaimed_keybindings`, bullet 1) was scoped to
/// `"GeodeModal"`, the context the modal *panel* carries — which is only
/// on the dispatch stack when something inside that panel holds focus.
/// Normal mode focuses `shell.focus_handle` (the window root) precisely
/// so bare letters reach `handle_key` as verbs, so `"GeodeModal"` was
/// absent from the stack, gpui-component `Root`'s own window-wide `Tab`
/// binding won, and `window.focus_next` moved focus off the shell root —
/// onto whatever focusable sits behind the modal, where `Input`-context
/// bindings go live and the dialog's own vocabulary stops arriving. The
/// fix is `render`'s `"GeodeModalOpen"` context on the shell root itself
/// (see `init_reclaimed_keybindings`'s bullet 5).
///
/// Asserts the focus state itself, not merely that the modal survived: a
/// stray `focus_next` leaves the modal untouched, so "still open" is
/// exactly the assertion that could not see this bug.
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

/// §16.1: the sync, not the transition site, owns focus and text. Enter
/// filter mode, type, leave it, clear the query, start a capture, cancel
/// it — and after every step the focused surface and the Input's text
/// are what the pure state says, with no site in `keybindings_view`
/// touching either directly (Task 2 deletes them all; this test is what
/// proves the sync reproduces them).
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
    cx.simulate_keystrokes("escape"); // leave filter, keep the query
    cx.run_until_parked();
    assert!(!dialog_filter_is_focused(&shell, &mut cx));
    let text = shell.read_with(&cx, |shell, cx| {
        shell.dialog_input.read(cx).value().to_string()
    });
    assert_eq!(
        text, "pal",
        "leaving filter mode keeps the query in the field"
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
    cx.simulate_keystrokes("enter"); // begin a capture from filter mode
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

// --- Mouse parity (interaction-model spec §17) --------------------------

/// §17.1 rule 1: the frozen filter row is the mouse form of `/`. The
/// dialog opens in normal mode with the row frozen; a mouse-down on it
/// must leave the pill reading `filter` with the shared `Input` focused.
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
