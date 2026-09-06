//! Chrome (toolbar, sidebar, status bar) and the shared dialog/settings
//! modal surface: filter focus, chord swallowing, font size, find style.

use super::*;

// --- Task 4: chrome (toolbar, sidebar, slimmed status bar) ----------

/// Cheap evidence the new chrome actually paints something, on a
/// window with zero tiles open — before this task, an empty workspace
/// painted no quads at all (just the "ctrl+h / ctrl+v" placeholder
/// text). The title bar and sidebar now fill their own background
/// regardless of tile state, so this is a real regression check, not a
/// tautology.
#[gpui::test]
fn chrome_paints_quads_even_with_no_tiles_open(cx: &mut gpui::TestAppContext) {
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

    let quads = cx.update(|window, _cx| window.painted_quads().len());
    assert!(
        quads > 0,
        "the title bar and sidebar backgrounds should paint quads even \
         with no tiles open"
    );
}

/// Focus interplay (brief): pressing Escape while the filter input has
/// focus hands focus back to the shell root, through the real key-event
/// pipeline — Input's own `Escape` action handler `cx.propagate()`s (no
/// popover/inline-completion/IME text to consume it), and
/// `ShellView::handle_key_down`'s filter-input guard is what actually
/// does the refocus. Focus is set directly on the input's `FocusHandle`
/// (equivalent to what a real mouse click on it would produce) rather
/// than simulating the click itself, since the filter field's on-screen
/// position depends on window/text layout this test shouldn't need to
/// know.
#[gpui::test]
fn escape_in_the_filter_input_returns_focus_to_the_shell_root(cx: &mut gpui::TestAppContext) {
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
    let filter_input = shell.read_with(&cx, |shell, _| shell.filter_input.clone());
    let input_focus_handle = filter_input.read_with(&cx, |state, cx| state.focus_handle(cx));

    cx.update(|window, cx| input_focus_handle.focus(window, cx));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.update(|window, _cx| input_focus_handle.is_focused(window)),
        "sanity: focusing the input's own handle should make it focused"
    );
    assert!(
        !cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
        "sanity: the shell root must not be focused while the input is"
    );

    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        !cx.update(|window, _cx| input_focus_handle.is_focused(window)),
        "escape should have moved focus off the filter input"
    );
    assert!(
        cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
        "escape should have returned focus to the shell root"
    );
}

/// Focus interplay (brief): while the filter input has focus, a shell
/// chord that has no key binding at all in the input's own gpui action
/// context (`ctrl+h` = `workspace::split_down`) must not reach the
/// shell's keymap `Matcher` — it stays with the input instead of
/// splitting the workspace.
#[gpui::test]
fn shell_chords_do_not_fire_while_the_filter_input_has_focus(cx: &mut gpui::TestAppContext) {
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

    let filter_input = shell.read_with(&cx, |shell, _| shell.filter_input.clone());
    let input_focus_handle = filter_input.read_with(&cx, |state, cx| state.focus_handle(cx));
    cx.update(|window, cx| input_focus_handle.focus(window, cx));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("ctrl-h");

    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 0,
        "ctrl+h (workspace::split_down) must not dispatch while the filter \
         input has focus"
    );
}

/// `settings::open` (dispatched via `ctrl+,`, the palette, or the
/// sidebar profile icon) opens the real settings modal (Task 5, Task 9
/// instant-modal redesign): `shell.modal` flips `Some`, and the modal
/// Every shell dialog's panel starts at the same top edge —
/// `dialog::MODAL_TOP_RATIO` of the viewport below the backdrop's own
/// top — rather than being vertically centered (user direction:
/// differently-sized dialogs centering to different heights defeats
/// spatial memory; a shared top edge is what the eye expects). Proven
/// across two differently-sized dialogs: settings (tall) and keyboard
/// shortcuts must paint their panels at the SAME y, at exactly the
/// ratio offset.
#[gpui::test]
fn all_shell_dialogs_share_the_same_top_edge(cx: &mut gpui::TestAppContext) {
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

    let open_and_measure = |cx: &mut gpui::VisualTestContext, action: &str| {
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId(action.to_string()), None, window, cx);
            });
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let backdrop = cx
            .debug_bounds("shell-modal-backdrop")
            .expect("backdrop painted");
        let panel = cx.debug_bounds("shell-modal-panel").expect("panel painted");
        // Close again (escape path) so the next dialog can open.
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            f32::from(panel.origin.y) - f32::from(backdrop.origin.y),
            f32::from(backdrop.size.height),
            f32::from(backdrop.origin.y),
        )
    };

    let (settings_top, backdrop_height, backdrop_origin_y) =
        open_and_measure(&mut cx, "settings::open");
    let (keybindings_top, _, _) = open_and_measure(&mut cx, "keybindings::open");

    let expected = backdrop_height * dialog::MODAL_TOP_RATIO;
    assert!(
        (settings_top - expected).abs() < 1.0,
        "the settings panel should start MODAL_TOP_RATIO down the \
         backdrop, expected {expected}, got {settings_top}"
    );
    assert_eq!(
        settings_top, keybindings_top,
        "differently-sized dialogs must share the same top edge, got \
         {settings_top} vs {keybindings_top}"
    );

    // The command palette shares the line too (user direction: it's
    // dialog-like). It renders in the same coordinate space the modal
    // backdrop does (both absolute children of ShellView's root), so
    // the captured backdrop origin is the shared reference point.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("palette::toggle".to_string()), None, window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let palette_panel = cx
        .debug_bounds("palette-panel")
        .expect("palette panel painted");
    let palette_top = f32::from(palette_panel.origin.y) - backdrop_origin_y;
    assert!(
        (palette_top - expected).abs() < 1.0,
        "the palette panel should start on the same shared top edge, \
         expected {expected}, got {palette_top}"
    );
}

/// chrome (`dialog::render_modal` — backdrop + panel + title row +
/// settings content) actually paints, checked two ways: it adds quads
/// over the empty-workspace baseline, AND its backdrop/panel
/// `debug_selector`s recover real, non-zero-sized painted bounds — the
/// same "prove it painted, not just that a flag flipped" standard
/// `empty_workspace_paints_the_hint` sets. Also stands in for "the
/// sidebar paints": its click handler calling into this exact
/// `dispatch` path is what `sidebar::sidebar`'s profile icon wires up.
#[gpui::test]
fn settings_open_opens_the_modal(cx: &mut gpui::TestAppContext) {
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
        shell.read_with(&cx, |shell, _| shell.modal.is_none()),
        "sanity: no modal is open before dispatch"
    );
    let quads_before = cx.update(|window, _cx| window.painted_quads().len());

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
        });
    });

    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "settings::open should have set ShellView's own modal state"
    );

    // The workspace itself must stay untouched — settings::open is not
    // a workspace verb and must not be mistaken for one.
    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(tile_count, 0);

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let quads_after = cx.update(|window, _cx| window.painted_quads().len());
    assert!(
        quads_after > quads_before,
        "the modal overlay (backdrop + chrome + settings content) should \
         paint additional quads over the empty-workspace baseline"
    );

    let backdrop_bounds = cx.debug_bounds("shell-modal-backdrop");
    assert!(
        backdrop_bounds
            .is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "the modal backdrop should have painted with non-zero bounds, got {backdrop_bounds:?}"
    );
    let panel_bounds = cx.debug_bounds("shell-modal-panel");
    assert!(
        panel_bounds.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "the modal panel should have painted with non-zero bounds, got {panel_bounds:?}"
    );
}

/// A real `ctrl+,` keystroke, through the actual key-event pipeline,
/// dispatches `settings::open` and opens the modal — end-to-end
/// coverage of the `BUILTIN_KEYMAP` binding added in Task 5, mirroring
/// `mod_shift_t_keystroke_toggles_the_theme_mode` above.
#[gpui::test]
fn mod_comma_keystroke_opens_the_settings_modal(cx: &mut gpui::TestAppContext) {
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

    cx.simulate_keystrokes("ctrl-,");

    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "ctrl-, (settings::open) should have opened the settings modal"
    );
}

/// Fix wave, Fix 1 regression, carried forward by the Task 9
/// instant-modal redesign: while the settings modal is open, `ctrl+v`
/// (`workspace::split_right`) must not reach the shell's keymap
/// `Matcher` at all — modeled on the filter-input guard this mirrors
/// (`handle_key_down`'s early return while the filter field is
/// focused). Before the original fix, `ShellView::handle_key_down`'s
/// `on_key_down` listener still received every raw keystroke regardless
/// of the dialog (dialogs paint above the tile surface but don't
/// interrupt this view's own key dispatch); the same is true of the
/// modal that replaced it, so a chord typed while e.g. picking a theme
/// in the modal would silently also mutate the workspace behind it.
/// Also checks the closed-palette case (`ctrl+k` = `palette::toggle`):
/// that must not open either, since the palette-toggle intercept sits
/// ahead of the matcher in `handle_key_down` and needs the same guard.
#[gpui::test]
fn modal_open_swallows_shell_chords(cx: &mut gpui::TestAppContext) {
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

    // Open the settings modal via the real `settings::open` dispatch
    // path (the builtin `ctrl+,` binding), not by constructing it
    // out-of-band, so this exercises the exact state `handle_key_down`
    // has to guard against.
    cx.simulate_keystrokes("ctrl-,");
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "sanity: ctrl-, should have opened the settings modal"
    );

    cx.simulate_keystrokes("ctrl-v");
    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 0,
        "ctrl+v (workspace::split_right) must not reach the matcher while \
         the settings modal is open"
    );

    cx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "ctrl+k (palette::toggle) must not open the command palette while \
         the settings modal is open"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "the settings modal should still be open — nothing here should \
         have closed it"
    );
}

/// Escape closes the modal: a real keystroke, through the actual
/// key-event pipeline, reaching `handle_key_down`'s modal branch (not
/// gpui-component's own `Cancel` action — there is no such layer for
/// this modal, see `dialog`'s module doc). Mirrors `modal_open_
/// swallows_shell_chords`'s open path, but exercises the one keystroke
/// that must NOT be swallowed.
#[gpui::test]
fn escape_keystroke_closes_the_modal(cx: &mut gpui::TestAppContext) {
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

    cx.simulate_keystrokes("ctrl-,");
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "sanity: ctrl-, should have opened the settings modal"
    );

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_none()),
        "escape should have closed the modal"
    );
}

#[gpui::test]
fn opening_the_settings_dialog_focuses_the_filter(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    assert!(shell.read_with(&cx, |shell, _| shell.settings.is_some()));
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the filter must own focus the moment the dialog opens"
    );
}

/// Typing filters; the old `h`/`l` stepping keys are now just text,
/// and must not step anything on their way into the query.
#[gpui::test]
fn typing_filters_the_settings_rows(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());
    cx.simulate_input("dark");
    let (query, selected) = shell.read_with(&cx, |shell, _| {
        let state = shell.settings.as_ref().unwrap();
        (state.query.clone(), state.selected)
    });
    assert_eq!(query, "dark");
    assert_eq!(selected, 0, "a query selects the top match");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark()),
        before,
        "typing must never apply a setting — the old h/l/enter stepping \
         keys are plain text now"
    );
}

/// tab steps the selected row's value forward and shift+tab back,
/// through the same apply path a click takes. Narrowed to the Font
/// size row rather than the (two-value) Dark mode row deliberately:
/// on a two-value row `step(2, current, Left)` and
/// `step(2, current, Right)` land on the same value, so asserting
/// only "the value changed" either direction can't tell a correct
/// `StepDirection::Left`/`Right` mapping in `handle_key` from an
/// accidentally swapped one. Font size has three values
/// (Small/Medium/Large), so asserting the EXACT target after each
/// key -- not just "it changed" -- genuinely pins the direction: a
/// swapped mapping would land tab on Small, not Large.
#[gpui::test]
fn tab_and_shift_tab_step_the_selected_value(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_input("font");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Medium,
        "sanity: the test shell starts at the Medium default"
    );

    cx.simulate_keystrokes("tab");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Large,
        "tab should step Font size forward, Medium -> Large"
    );

    cx.simulate_keystrokes("shift-tab");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Medium,
        "shift+tab should step it back, Large -> Medium"
    );
}

/// The three `apply_setting` arms `tab_and_shift_tab_step_the_
/// selected_value` doesn't reach (that test covers Font size) --
/// Theme, Dark mode, Find style -- each stepped once through a real
/// `tab` keystroke, so a mis-wired arm (e.g. `SettingId::Theme =>
/// set_font_size_on`) is caught here rather than nowhere: `
/// apply_setting` takes `&mut ShellView` and has no pure unit test of
/// its own. One fresh dialog per row rather than one dialog walked
/// with `j`/`k` (as the retired vim-nav version of this test did):
/// selecting a different row now means typing a different filter
/// query, and there's no key that clears the shared field back to
/// empty mid-session, so three small dialogs are simpler than one
/// that fights its own filter.
#[gpui::test]
fn tab_steps_every_remaining_apply_setting_arm(cx: &mut gpui::TestAppContext) {
    {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        let before = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        cx.simulate_input("theme");
        cx.simulate_keystrokes("tab");
        let after = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        assert_ne!(
            after, before,
            "tab on the Theme row should step to a different theme"
        );
    }

    {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        let before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());
        cx.simulate_input("dark");
        cx.simulate_keystrokes("tab");
        let after = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());
        assert_eq!(after, !before, "tab on the Dark mode row should toggle it");
    }

    {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        let before = shell.read_with(&cx, |shell, _| shell.find_style);
        cx.simulate_input("keyboard");
        cx.simulate_keystrokes("tab");
        let after = shell.read_with(&cx, |shell, _| shell.find_style);
        assert_ne!(
            after, before,
            "tab on the Find style row should flip vim/fzf"
        );
    }
}

/// Enter is inert and reserved here (spec §3): it must not step a
/// value, and must not close the dialog either.
#[gpui::test]
fn enter_does_nothing_in_the_settings_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_input("dark");
    let before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());

    cx.simulate_keystrokes("enter");
    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.theme.active_mode().is_dark(),
            before,
            "enter must not step the value"
        );
        assert!(shell.modal.is_some(), "and must not close the dialog");
    });
}

/// The full inertness contract for the reserved `enter` (spec §3):
/// with a focused `Input`, `handle_key` returning `false` for it would
/// NOT make it inert — `enter` would reach the filter, be normalized
/// away to an empty edit, but still fire an unconditional
/// `InputEvent::Change` that resets `selected` back to the top match
/// via `SettingsState::set_query`. `handle_key` claims it instead (see
/// its own doc comment). Unlike `enter_does_nothing_in_the_settings_
/// dialog` above, this moves the selection off the top row FIRST, so
/// a reset back to 0 is actually observable.
#[gpui::test]
fn enter_is_reserved_and_leaves_the_settings_dialog_untouched(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("down down");
    let selected_before =
        shell.read_with(&cx, |shell, _| shell.settings.as_ref().unwrap().selected);
    assert_eq!(
        selected_before, 2,
        "sanity: two downs land on the third row"
    );
    let dark_before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());

    cx.simulate_keystrokes("enter");

    let (query, selected, dark_after, open) = shell.read_with(&cx, |shell, _| {
        let state = shell.settings.as_ref().unwrap();
        (
            state.query.clone(),
            state.selected,
            shell.services.theme.active_mode().is_dark(),
            shell.modal.is_some(),
        )
    });
    assert!(
        query.is_empty(),
        "enter must not leave any character in the filter"
    );
    assert_eq!(
        selected, selected_before,
        "enter must not reset the selection to the top match"
    );
    assert_eq!(dark_after, dark_before, "enter must not step any value");
    assert!(open, "enter must not close the dialog");
}

#[gpui::test]
fn escape_closes_the_settings_dialog_and_restores_shell_focus(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("escape");
    shell.read_with(&cx, |shell, _| {
        assert!(shell.modal.is_none());
        assert!(shell.settings.is_none(), "close_modal clears dialog state");
    });
    assert!(
        cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)),
        "focus lands back on the shell root"
    );
}

/// Backdrop click closes the modal: a real mouse-down at a corner of
/// the window, well outside the centered panel (`dialog::render_modal`
/// centers the panel with `.items_center().justify_center()` over the
/// full-viewport backdrop, so a point near the origin always falls on
/// the backdrop, never the panel, for any viewport the test window
/// opens at). Mirrors `mouse_down_on_a_tile_focuses_it`'s real-
/// mouse-event structure above.
#[gpui::test]
fn backdrop_click_closes_the_modal(cx: &mut gpui::TestAppContext) {
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

    cx.simulate_keystrokes("ctrl-,");
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "sanity: ctrl-, should have opened the settings modal"
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_mouse_down(
        gpui::point(gpui::px(4.0), gpui::px(4.0)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );

    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_none()),
        "a mouse-down on the backdrop, well outside the centered panel, \
         should have closed the modal"
    );
}

/// A mouse-down INSIDE the panel must not close the modal — the panel's
/// own `on_mouse_down` (`dialog::render_modal`) stops propagation before
/// the same bubbling event ever reaches the backdrop's close handler
/// underneath it. The click lands on the panel's title row (top-left
/// corner of the panel, which `dialog::render_modal` centers over the
/// backdrop): recovered via `debug_bounds("shell-modal-panel")`, the
/// real painted bounds, rather than recomputing the centering math by
/// hand.
#[gpui::test]
fn panel_click_does_not_close_the_modal(cx: &mut gpui::TestAppContext) {
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

    cx.simulate_keystrokes("ctrl-,");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "sanity: ctrl-, should have opened the settings modal"
    );

    let panel_bounds = cx
        .debug_bounds("shell-modal-panel")
        .expect("the modal panel should have painted bounds to click inside");
    let inside_panel = gpui::point(
        panel_bounds.origin.x + gpui::px(10.0),
        panel_bounds.origin.y + gpui::px(10.0),
    );

    cx.simulate_mouse_down(inside_panel, MouseButton::Left, gpui::Modifiers::none());

    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "a mouse-down inside the panel must not close the modal"
    );
}

/// Content-collapse guard, retargeted at the home-rolled dialog:
/// `settings_open_opens_the_modal` only proves the backdrop/panel/
/// title chrome painted with non-zero bounds — all painted directly by
/// `dialog::render_modal` itself, so it would stay green even if the
/// row list nested inside the panel rendered at zero height (the exact
/// failure mode the old gpui-component `Settings` composite hit when
/// its percentage-height root met a parent with no definite height).
/// This test checks the thing that test doesn't: the row list
/// (`debug_selector("settings-list")`) must paint at its full derived
/// height — `settings_view` has four rows (Theme, Dark mode, Font
/// size, Find style), each `ROW_HEIGHT` (44px) tall — and every one of
/// those rows must itself have painted bounds.
///
/// Uses `WindowOptions::default()`, same as every other modal test in
/// this file — gpui's own `default_bounds` gives that a realistic
/// 1536x1095 test window, not a cramped one, so a collapse here is not
/// an artifact of an unrealistically small test viewport.
#[gpui::test]
fn settings_content_paints_with_a_meaningful_height(cx: &mut gpui::TestAppContext) {
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

    // settings::open is bound to ctrl+, (rebound from mod+, in commit
    // 87aa731; this test merged in concurrently and carried the old key).
    cx.simulate_keystrokes("ctrl-,");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "sanity: ctrl-, should have opened the settings modal"
    );

    let list_bounds = cx
        .debug_bounds("settings-list")
        .expect("the settings row list should have painted bounds");
    assert!(
        list_bounds.size.height >= px(4.0 * 44.0),
        "the settings row list should paint at its full four-row height \
         (4 × ROW_HEIGHT = 176px) — got {:?}. A sliver here means the \
         list collapsed inside the modal panel instead of laying out \
         its rows.",
        list_bounds.size
    );
    // debug_bounds takes &'static str, so the four selectors are spelled
    // out rather than formatted.
    for selector in [
        "settings-row-0",
        "settings-row-1",
        "settings-row-2",
        "settings-row-3",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should have painted bounds (Theme, Dark mode, \
             Font size, Find style rows must all lay out)"
        );
    }
}

/// `settings_view::set_theme`/`set_dark_mode` are the exact handlers the
/// dialog's theme dropdown/dark-mode switch invoke on selection/click
/// (see those functions' doc comments: simulating a real click through
/// the dropdown's popup-menu overlay, or the switch's own mouse
/// handling, is impractical from a `#[gpui::test]` — this drives the
/// identical path instead). Exercises both live-apply and the
/// `ThemeService` bookkeeping (`active_name`/`active_mode`) staying in
/// sync, the same contract `theme::toggle_mode` already has coverage
/// for elsewhere in this file.
#[gpui::test]
fn settings_dialog_theme_and_mode_setters_apply_live_through_theme_service(
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

    // test_services() never calls apply_from_config, so the starting
    // state is exactly load_bundled()'s own default: "Default Light",
    // mode Light (matches gpui_component::init's own initial theme).
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.services.theme.active_mode()),
        crate::theme::Mode::Light,
        "sanity: the starting mode must be Light, or the assertions below \
         wouldn't prove set_dark_mode actually flipped anything"
    );

    // Fully qualified name, matching what the real dropdown passes
    // (its options come from `ThemeService::names()`, already
    // fully-qualified) — exercises `resolve`'s exact-name path
    // (`find_exact`), not the bare-family fallback (`find_family`),
    // which has its own direct coverage in `theme.rs`'s own tests.
    cx.update(|_window, cx| settings_view::set_theme(&shell, "Gruvbox Light", cx));
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .theme
            .active_name()
            .to_string()),
        "Gruvbox Light",
        "set_theme should apply the exact fully-qualified name through \
         ThemeService::apply, regardless of the currently active mode \
         argument (find_exact ignores it)"
    );

    cx.update(|_window, cx| settings_view::set_dark_mode(&shell, true, cx));
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .theme
            .active_name()
            .to_string()),
        "Gruvbox Dark",
        "set_dark_mode(true) should flip to the dark variant through \
         ThemeService::set_mode, staying within the same family"
    );

    cx.update(|_window, cx| settings_view::set_dark_mode(&shell, false, cx));
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.services.theme.active_mode()),
        crate::theme::Mode::Light,
        "set_dark_mode(false) should flip back to light"
    );
}

/// `settings_view::set_font_size` (the font-size button group's setter,
/// driven directly for the same reason `set_theme`'s test drives the
/// handler rather than the control) updates `ShellView::font_size`, and
/// the next render applies it as the window's rem size — the one
/// mechanism every path shares (see the `fontsize` module doc).
#[gpui::test]
fn set_font_size_applies_the_rem_size_on_the_next_render(cx: &mut gpui::TestAppContext) {
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

    assert_eq!(
        cx.update(|window, _cx| window.rem_size()),
        px(12.0),
        "sanity: with no [ui] font_size configured, medium (12px — well \
         below gpui's own 16px rem default, per the fontsize \
         module doc) must be in effect after the first render"
    );

    cx.update(|_window, cx| {
        settings_view::set_font_size(&shell, crate::fontsize::FontSize::Large, cx)
    });
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Large,
        "set_font_size should update the shell's state immediately"
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        cx.update(|window, _cx| window.rem_size()),
        px(14.0),
        "the render after set_font_size(Large) should apply 14px as the \
         window rem size"
    );
}

/// End-to-end: `ctrl+=` / `ctrl+-` (`fontsize::increase`/`decrease`)
/// step the UI font size through real keystrokes, clamped at both ends
/// — the keyboard path onto the same state the settings toggle group
/// drives.
#[gpui::test]
fn ctrl_equals_and_minus_step_the_font_size_with_clamping(cx: &mut gpui::TestAppContext) {
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
    let font_size = |cx: &gpui::VisualTestContext| shell.read_with(cx, |shell, _| shell.font_size);

    assert_eq!(font_size(&cx), crate::fontsize::FontSize::Medium);

    cx.simulate_keystrokes("ctrl-=");
    assert_eq!(font_size(&cx), crate::fontsize::FontSize::Large);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        cx.update(|window, _cx| window.rem_size()),
        px(14.0),
        "ctrl+= should have applied Large's 14px rem size"
    );

    cx.simulate_keystrokes("ctrl-=");
    assert_eq!(
        font_size(&cx),
        crate::fontsize::FontSize::Large,
        "increase clamps at Large"
    );

    cx.simulate_keystrokes("ctrl--");
    cx.simulate_keystrokes("ctrl--");
    assert_eq!(font_size(&cx), crate::fontsize::FontSize::Small);
    cx.simulate_keystrokes("ctrl--");
    assert_eq!(
        font_size(&cx),
        crate::fontsize::FontSize::Small,
        "decrease clamps at Small"
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        cx.update(|window, _cx| window.rem_size()),
        px(10.0),
        "two decreases from Large should land on Small's 10px rem size"
    );
}

/// A `[ui] font_size` key already present in the layered config at
/// startup is applied by the first render — the same read
/// (`FontSize::from_config`) `apply_reload` re-runs on hot reload.
#[gpui::test]
fn a_configured_font_size_applies_from_the_first_render(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let mut services = test_services();
    services.config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[ui]\nfont_size = \"small\"\n").unwrap()],
        desk: None,
        user: None,
    });

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();

    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert_eq!(
        cx.update(|window, _cx| window.rem_size()),
        px(10.0),
        "[ui] font_size = \"small\" should render at a 10px rem size \
         from the very first frame"
    );
}

/// `settings_view::set_find_style` (the find-style button group's
/// setter, driven directly for the same reason `set_theme`'s and
/// `set_font_size`'s tests drive the handler rather than the control)
/// updates `ShellView::find_style` — the state `keybindings_view`
/// reads fresh on every keystroke and render, so there is nothing
/// further to apply.
#[gpui::test]
fn set_find_style_updates_the_shell_state(cx: &mut gpui::TestAppContext) {
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

    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.find_style),
        FindStyle::Vim,
        "sanity: with no [ui] find_style configured, vim is the default"
    );

    cx.update(|_window, cx| settings_view::set_find_style(&shell, FindStyle::Fzf, cx));
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.find_style),
        FindStyle::Fzf,
        "set_find_style should update the shell's state immediately"
    );
}

/// `[ui] find_style` resolves at startup and re-resolves on hot reload
/// — the same two paths `font_size` rides (`ShellView::new` /
/// `apply_reload`).
#[gpui::test]
fn a_configured_find_style_resolves_at_startup_and_on_reload(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let mut services = test_services();
    services.config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[ui]\nfind_style = \"fzf\"\n").unwrap()],
        desk: None,
        user: None,
    });

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
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

    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.find_style),
        FindStyle::Fzf,
        "[ui] find_style = \"fzf\" should resolve at startup"
    );

    // A reload whose config lacks the key falls back to vim — the
    // same lenient re-derive `font_size` gets in `apply_reload`.
    let new_config = config_with_mod("alt");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.find_style),
        FindStyle::Vim,
        "a reload without [ui] find_style should re-resolve to the default"
    );
}

/// Task 9: opening a modal through `dialog::open_shell_dialog` (here,
/// `settings::open` — the only current call site, migrated onto the
/// utility) must cancel a pending keymap sequence, the same hygiene
/// `toggle_palette` already gives palette-open. A real `g` keystroke
/// starts the test-only `"g g"` sequence (`test_services_with_gg_binding`
/// — the builtin keymap has no sequences of its own anymore), which
/// leaves one pending keystroke and paints the
/// which-key overlay (Task 8); opening the settings modal must clear
/// both.
#[gpui::test]
fn modal_open_through_the_utility_clears_a_pending_sequence(cx: &mut gpui::TestAppContext) {
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

    // Start (but don't finish) the "g g" sequence.
    cx.simulate_keystrokes("g");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.matcher.pending().len()),
        1,
        "sanity: g alone should leave the test sequence pending"
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("whichkey-overlay").is_some(),
        "sanity: the which-key overlay should paint while g is pending"
    );

    // Open the settings modal through the real dispatch path —
    // `settings_view::open` routes through `open_shell_dialog_with_key`
    // (the one standard dialog door, keyed since the row-list rewrite).
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
        });
    });

    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "settings::open should have opened the modal"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.matcher.pending().is_empty()),
        "opening a modal through open_shell_dialog should cancel the \
         pending g sequence, same as palette-open does"
    );

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("whichkey-overlay").is_none(),
        "the which-key overlay must not paint once the pending sequence \
         has been cancelled"
    );
}

/// Task 9: `open_shell_dialog` must close an open palette. `settings::
/// open` can't be reached with the palette open through its own Enter
/// path (`dispatch_palette_item` closes the palette before dispatching
/// anything, and `dispatch`'s `palette::toggle` arm is the only one that
/// re-touches `self.palette` — there is no route from an open palette
/// back into `dispatch`'s `settings::open` arm while it's still open),
/// so this drives `dialog::open_shell_dialog` directly to exercise the
/// utility's own hygiene in isolation from any one call site.
#[gpui::test]
fn open_shell_dialog_closes_an_open_palette(cx: &mut gpui::TestAppContext) {
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
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "sanity: ctrl+k should open the palette"
    );

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            dialog::open_shell_dialog(shell, window, cx, "Test modal", |_shell, _window, _cx| {
                div().into_any_element()
            });
        });
    });

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "open_shell_dialog should have closed the open palette"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal.is_some()),
        "open_shell_dialog should have set ShellView's own modal state"
    );
}
