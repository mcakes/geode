//! Chrome (toolbar, sidebar, status bar) and the shared dialog/settings
//! modal surface: filter focus, chord swallowing, font size, find style.

use super::*;

// Chrome: toolbar, sidebar, and status bar.

/// An empty window still paints title-bar and sidebar backgrounds. A nonempty quad
/// scene verifies chrome rendering independently of any tile content.
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

/// Escape propagates from the focused filter input to the shell's guard and restores
/// root focus. Set the field's focus handle directly to exercise key routing without
/// depending on its pixel position.
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

/// A shell chord with no binding in the focused filter's GPUI context still reaches the
/// shell: the fixture's `ctrl+h` adds a tile. `scopebar.rs` separately checks that
/// Shift alone remains ordinary typing.
#[gpui::test]
fn shell_chords_fire_while_the_filter_input_has_focus(cx: &mut gpui::TestAppContext) {
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
        tile_count, 1,
        "ctrl+h (the test layer's tile::add_rec_vertical) must dispatch \
         from the focused filter input"
    );
}

/// Shell dialogs align their panels at `dialog::MODAL_TOP_RATIO` below the backdrop's
/// top. Settings and keyboard shortcuts have different heights but must share the same
/// top edge, preserving a stable location for dialog content.
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
        shell.read_with(&cx, |shell, _| !shell.modal_open()),
        "sanity: no modal is open before dispatch"
    );
    let quads_before = cx.update(|window, _cx| window.painted_quads().len());

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
        });
    });

    assert!(
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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

/// A real `ctrl+,` keystroke traverses the key-event pipeline, dispatches
/// `settings::open`, and opens the settings modal.
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
        "ctrl-, (settings::open) should have opened the settings modal"
    );
}

/// While settings is open, shell chords must not reach the workspace matcher. The
/// fixture's `ctrl+v` must leave the tile layout unchanged. `ctrl+k` is the one
/// non-dialog chord that passes: the palette opens over the dialog without closing
/// it. Modal painting alone does not prevent raw key events from reaching `ShellView`.
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
        "sanity: ctrl-, should have opened the settings modal"
    );

    cx.simulate_keystrokes("ctrl-v");
    let tile_count = shell.read_with(&cx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        tile_count, 0,
        "ctrl+v (the test layer's tile::add_rec_horizontal) must not reach \
         the matcher while the settings modal is open"
    );

    cx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_some()),
        "ctrl+k (palette::toggle) opens the command palette over the \
         settings modal"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
        "sanity: ctrl-, should have opened the settings modal"
    );

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |shell, _| !shell.modal_open()),
        "escape should have closed the modal"
    );
}

/// Settings opens in normal mode with the shared filter blurred. Bare letters are
/// commands; `/` enters filter mode and focuses the field.
#[gpui::test]
fn opening_the_settings_dialog_leaves_the_filter_blurred(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    assert!(shell.read_with(&cx, |shell, _| shell.settings.is_some()));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().mode),
        crate::dialogmode::DialogMode::Normal,
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "the filter must NOT own focus in normal mode — a focused Input \
         would eat every verb as text"
    );
    assert!(
        cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)),
        "the shell root holds the keys instead"
    );
}

/// The behaviour change the switch turns on: a bare letter in normal
/// mode does not reach the filter. `s` is chosen because it is a letter
/// neither mode's vocabulary claims — the one that would type if the
/// dialog had silently opened filter-first.
#[gpui::test]
fn the_settings_dialog_opens_in_normal_mode_and_letters_do_not_type(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("s");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().query.clone()),
        "",
        "a bare letter in normal mode must not reach the filter"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal_open()),
        "and the dialog is still open (a sanity check — the modal branch \
         returns whether or not the key was claimed, so this cannot tell \
         Drop from PassThrough; `route`'s own pure test pins that)"
    );
}

/// `/` enters filter mode and typing narrows, exactly as the dialog
/// always did once the field had focus.
#[gpui::test]
fn slash_enters_settings_filter_mode_and_typing_narrows(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("/");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().mode),
        crate::dialogmode::DialogMode::Filter,
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "entering filter mode must hand focus to the filter"
    );
    cx.simulate_keystrokes("f o n t");
    cx.run_until_parked();
    let (query, visible) = shell.read_with(&cx, |s, cx| {
        let state = s.settings.as_ref().unwrap();
        let rows = settings_view::derive_rows(
            &s.services.theme.names(),
            s.services.theme.active_name(),
            s.font_size,
            s.find_style,
            s.line_numbers,
            s.add_direction,
            s.default_source.as_deref(),
            &cx.global::<crate::series::SeriesSettings>().names(),
        );
        (
            state.query.clone(),
            settings_view::visible_rows(state, &rows).len(),
        )
    });
    assert_eq!(query, "font");
    assert_eq!(visible, 1, "the query narrows to the Font size row");
}

/// `j`/`k` move in normal mode; the arrows and ctrl-steps still work in
/// both modes, so one navigation vocabulary serves both.
#[gpui::test]
fn j_and_k_move_in_the_settings_dialogs_normal_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("j j");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        2
    );
    cx.simulate_keystrokes("k");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        1
    );
    cx.simulate_keystrokes("down");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        2,
        "the arrows keep working in normal mode"
    );
}

/// After Enter keeps the settings filter, successive Escape presses clear
/// the query and close the dialog in separate transitions.
#[gpui::test]
fn settings_escape_walks_the_ladder_one_rung_at_a_time(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("/ f o n t");
    cx.run_until_parked();

    cx.simulate_keystrokes("enter");
    let (mode, q) = shell.read_with(&cx, |s, _| {
        let st = s.settings.as_ref().unwrap();
        (st.mode, st.query.clone())
    });
    assert_eq!(mode, crate::dialogmode::DialogMode::Normal);
    assert_eq!(q, "font", "enter must keep the query applied");
    assert!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choice.is_none()),
        "and must not open the selected row's typeahead on its way out"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "and must blur the filter, or normal mode's letters would still type"
    );

    cx.simulate_keystrokes("escape");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().query.clone()),
        "",
        "the second escape clears the query"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal_open()),
        "and does not close"
    );
    // The cleared query must have reached the Input too, not just the
    // mirrored copy — the next filter session starts blank.
    cx.simulate_keystrokes("/ x");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().query.clone()),
        "x",
        "the field must have been emptied along with the mirrored query"
    );
    // Out of that filter session (`enter` keeps the `x`), then clear it.
    cx.simulate_keystrokes("enter escape");

    cx.simulate_keystrokes("escape");
    assert!(
        shell.read_with(&cx, |s, _| !s.modal_open()),
        "the third closes"
    );
}

/// Escape restores the settings filter-entry query in state and Input
/// without opening a typeahead.
#[gpui::test]
fn settings_escape_puts_back_the_query_filter_mode_was_entered_with(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("/ f o n t");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    cx.simulate_keystrokes("/");
    cx.simulate_input("zz");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    let (mode, query, choosing) = shell.read_with(&cx, |s, _| {
        let st = s.settings.as_ref().unwrap();
        (st.mode, st.query.clone(), st.choice.is_some())
    });
    assert_eq!(mode, crate::dialogmode::DialogMode::Normal);
    assert_eq!(query, "font", "escape puts back the entry query");
    assert!(!choosing, "and opens no typeahead on its way out");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "font",
        "the field follows the restored query"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.modal_open()),
        "reverting a search never closes the dialog"
    );
}

/// The mode is *legible*, not just held: the pill paints in both modes,
/// in the title row, and the frozen empty filter shows its placeholder.
#[gpui::test]
fn the_settings_mode_pill_paints_the_mode_it_is_actually_in(cx: &mut gpui::TestAppContext) {
    let (_shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let normal = cx.debug_bounds("dialog-mode-pill-normal");
    assert!(
        normal.is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
        "the pill should paint, labelled 'normal', in normal mode, got {normal:?}"
    );
    assert!(
        cx.debug_bounds("dialog-mode-pill-filter").is_none(),
        "and must not be labelled 'filter' there"
    );
    let title = cx.debug_bounds("shell-modal-title").expect("title paints");
    let pill = normal.unwrap();
    assert!(
        (pill.origin.y - title.origin.y).abs() < title.size.height,
        "the pill sits in the title row, not in the dialog's content"
    );
    assert!(
        cx.debug_bounds("dialog-filter-placeholder").is_some(),
        "the frozen empty filter says how to start typing"
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
        "with the 'normal' label gone"
    );
    assert!(
        cx.debug_bounds("dialog-filter-placeholder").is_none(),
        "and the placeholder gone with the live Input in its place"
    );
}

/// Space and Shift-Space step the selected value in opposite directions during normal
/// mode. Use Font size's three values so forward and backward steps are
/// distinguishable.
#[gpui::test]
fn space_and_shift_space_step_the_selected_value_in_normal_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("j");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Medium,
        "sanity: the test shell starts at the Medium default"
    );

    cx.simulate_keystrokes("space");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Large,
        "space should step Font size forward, Medium -> Large"
    );

    cx.simulate_keystrokes("shift-space");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Medium,
        "shift+space should step it back, Large -> Medium"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "stepping never moves focus — the dialog is still in normal mode"
    );
}

/// `i` on Theme opens a typeahead in place of the settings rows. Enter applies the
/// highlighted theme through `set_theme_on`, closes the field, and returns to normal
/// mode.
#[gpui::test]
fn i_on_the_theme_row_opens_a_typeahead_and_enter_applies_the_lit_theme(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let before = shell.read_with(&cx, |s, _| s.services.theme.active_name().to_string());
    cx.simulate_keystrokes("i"); // row 0 is Theme
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert!(cx.debug_bounds("dialog-name-row").is_some());
    assert!(cx.debug_bounds("dialog-mode-pill-choose").is_some());
    assert!(cx.debug_bounds("settings-choice-list").is_some());
    assert!(
        cx.debug_bounds("settings-list").is_none(),
        "the rows give way to the options"
    );
    cx.simulate_input("gruv d");
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-choice-Gruvbox Dark").is_some());
    assert!(cx.debug_bounds("settings-choice-Nord").is_none());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().mode),
        crate::dialogmode::DialogMode::Normal
    );
    let after = shell.read_with(&cx, |s, _| s.services.theme.active_name().to_string());
    assert_eq!(after, "Gruvbox Dark");
    assert_ne!(before, after);
    assert!(
        cx.debug_bounds("settings-list").is_some(),
        "the rows are back"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().query.clone()),
        "",
        "the FILTER query is untouched by the choice field"
    );
}

/// Theme typeahead opens with the active theme highlighted even when it lies beyond the
/// initial visible rows. Enter immediately after `i` must preserve the active theme;
/// the visible window follows the selection.
#[gpui::test]
fn i_then_enter_on_the_theme_row_leaves_the_theme_alone(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let before = shell.read_with(&cx, |s, _| s.services.theme.active_name().to_string());

    cx.simulate_keystrokes("i"); // row 0 is Theme
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    let lit = shell.read_with(&cx, |s, _| {
        s.settings
            .as_ref()
            .unwrap()
            .choice
            .as_ref()
            .unwrap()
            .list
            .highlighted_text()
            .map(str::to_string)
    });
    assert_eq!(
        lit.as_deref(),
        Some(before.as_str()),
        "the typeahead opens lit on the ACTIVE theme, wherever it ranks"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let after = shell.read_with(&cx, |s, _| s.services.theme.active_name().to_string());
    assert_eq!(
        after, before,
        "enter on the row it opened on must leave the theme untouched"
    );
    assert!(!shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));

    // Enter picks the highlighted row. Move down once and check that the theme
    // following `before` in `names()` order is applied.
    let names = shell.read_with(&cx, |s, _| s.services.theme.names());
    let before_ix = names.iter().position(|n| n == &before).unwrap();
    let expected_next = names[before_ix + 1].clone();

    cx.simulate_keystrokes("i");
    cx.run_until_parked();
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let final_theme = shell.read_with(&cx, |s, _| s.services.theme.active_name().to_string());
    assert_eq!(
        final_theme, expected_next,
        "enter picked the row a step down lit, not the top-ranked match"
    );
    assert!(!shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
}

/// The choice viewport shows at most twelve rows while its scroll container holds every
/// ranked option. Mouse-wheel scrolling and keyboard `scroll_to_item` can reach options
/// beyond the initial viewport.
#[gpui::test]
fn the_choice_list_paints_every_option_in_a_scrolling_viewport(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let names = shell.read_with(&cx, |s, _| s.services.theme.names().to_vec());
    assert!(names.len() > 12, "the fixture needs more themes than fit");
    cx.simulate_keystrokes("i"); // Theme
    cx.run_until_parked();
    let last = names.last().unwrap().clone();
    // `debug_bounds` takes a `&'static str`; a test may leak its few
    // selectors.
    let sel = |name: &str| -> &'static str {
        Box::leak(format!("settings-choice-{name}").into_boxed_str())
    };
    assert!(
        cx.debug_bounds(sel(&last)).is_some(),
        "every option is painted, not only the first twelve"
    );
    let list = cx.debug_bounds("settings-choice-list").unwrap();
    let first = cx.debug_bounds(sel(&names[0])).unwrap();
    assert!(
        list.size.height <= first.size.height * 12.5,
        "the viewport is capped at twelve rows ({:?} tall for {} rows; first row {:?})",
        list.size.height,
        names.len(),
        first
    );
    // `down` past the fold: the lit row is scrolled into the viewport.
    let active = shell.read_with(&cx, |s, _| s.services.theme.active_name().to_string());
    let active_ix = names.iter().position(|n| n == &active).unwrap();
    let target_ix = (active_ix + 20) % names.len();
    for _ in 0..20 {
        cx.simulate_keystrokes("down");
    }
    cx.run_until_parked();
    let lit = shell.read_with(&cx, |s, _| {
        s.settings
            .as_ref()
            .unwrap()
            .choice
            .as_ref()
            .unwrap()
            .list
            .highlighted_text()
            .map(str::to_string)
    });
    assert_eq!(lit.as_deref(), Some(names[target_ix].as_str()));
    let list = cx.debug_bounds("settings-choice-list").unwrap();
    let row = cx.debug_bounds(sel(&names[target_ix])).unwrap();
    assert!(
        row.origin.y >= list.origin.y
            && row.origin.y + row.size.height <= list.origin.y + list.size.height + gpui::px(1.),
        "the lit row {:?} is inside the viewport {:?}",
        row,
        list
    );
}

/// A double-click selects a settings row, then opens its typeahead through the same
/// path as `i` or Enter. A single click only selects.
#[gpui::test]
fn a_double_click_on_a_settings_row_opens_its_typeahead(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let row = cx
        .debug_bounds("settings-row-1")
        .expect("the Font size row is painted");
    let at = gpui::point(row.origin.x + gpui::px(40.0), row.origin.y + gpui::px(4.0));
    cx.simulate_mouse_down(at, MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        1,
        "a single click selects the row"
    );
    assert!(
        !shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()),
        "and opens nothing"
    );
    double_click(&mut cx, at, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()),
        "the double-click opened the typeahead, as `i` would"
    );
    assert!(dialog_filter_is_focused(&shell, &mut cx));
    assert!(cx.debug_bounds("settings-choice-list").is_some());
}

/// `escape` cancels the choice field with the setting untouched.
#[gpui::test]
fn escape_cancels_a_settings_choice_field_untouched(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let before = shell.read_with(&cx, |s, _| s.font_size);
    cx.simulate_keystrokes("j enter"); // Font size, enter opens too
    cx.run_until_parked();
    assert!(shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    cx.simulate_input("lar");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().choosing()));
    assert_eq!(shell.read_with(&cx, |s, _| s.font_size), before);
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "back in normal mode"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        1,
        "the cursor stayed on Font size"
    );
}

/// Space in filter mode is text. It must not step a setting while the user types a
/// query containing spaces.
#[gpui::test]
fn space_types_in_settings_filter_mode_rather_than_stepping(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("/ f o n t");
    cx.run_until_parked();
    let before = shell.read_with(&cx, |shell, _| shell.font_size);
    cx.simulate_keystrokes("space");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        before,
        "space in filter mode must not step the value"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().query.clone()),
        "font ",
        "it is a character in the query"
    );
}

/// `tab`/`shift+tab` keep stepping in normal mode too: they were the
/// dialog's stepping keys before it went modal, and normal mode adds
/// `space` beside them rather than retiring them.
#[gpui::test]
fn tab_still_steps_in_settings_normal_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("j tab");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Large,
        "tab should step Font size forward in normal mode"
    );
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Medium,
    );
}

/// In normal mode, `l` and `h` step the selected setting through `dialogmode`. The
/// window test verifies that the routed `Step` command reaches the setting, beyond the
/// pure routing tests.
#[gpui::test]
fn l_and_h_step_the_selected_value_in_settings_normal_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("j l");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Large,
        "l should step Font size forward, Medium -> Large"
    );
    cx.simulate_keystrokes("h");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
        crate::fontsize::FontSize::Medium,
        "h should step it back, Large -> Medium"
    );
    // The change hint groups the stepping keys, including Tab. Assert both the group
    // and its Tab chip are painted.
    assert!(
        cx.debug_bounds("settings-hint-change").is_some(),
        "the normal-mode footer names the stepping group"
    );
    assert!(
        cx.debug_bounds("settings-hint-change-tab").is_some(),
        "and tab is among its chips"
    );
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-hint-change-tab").is_some(),
        "and filter mode still names it, where it is the only stepping key"
    );
}

/// Clicking the frozen filter row enters filter mode, just like `/`.
#[gpui::test]
fn clicking_the_settings_frozen_filter_row_enters_filter_mode(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
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
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().mode),
        crate::dialogmode::DialogMode::Filter,
        "a click on the field is the mouse form of /"
    );
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "the sync handed the keyboard to the Input"
    );
    // A real keystroke, not `simulate_input`: the claim is that `j` now
    // goes through dispatch to the focused field rather than to `route`
    // as a motion, and only key dispatch can show that.
    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().query.clone()),
        "j",
        "and typing now filters rather than moving"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        0,
        "the j was text, not a motion"
    );
}

/// Row clicks retain the surface that owns focus: the shell root in normal mode, the
/// field in filter mode. A plain row click changes neither mode nor query and does not
/// itself move GPUI focus, so this test catches an unconditional focus move but cannot
/// distinguish whether a redundant sync ran.
#[gpui::test]
fn a_settings_row_click_keeps_focus_where_the_mode_says(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let click_row = |cx: &mut gpui::VisualTestContext, selector: &'static str| {
        let row = cx.debug_bounds(selector).expect("row paints");
        cx.simulate_mouse_down(
            gpui::point(row.origin.x + gpui::px(20.0), row.origin.y + gpui::px(4.0)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.run_until_parked();
    };

    click_row(&mut cx, "settings-row-2");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        2,
        "the click selected the row"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "a click in normal mode leaves the filter blurred"
    );
    cx.simulate_keystrokes("k");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        1,
        "and the letters are still verbs afterwards"
    );

    cx.simulate_keystrokes("/");
    click_row(&mut cx, "settings-row-3");
    assert!(
        dialog_filter_is_focused(&shell, &mut cx),
        "a click in filter mode keeps the filter focused"
    );
}

/// Clicking the value chip steps forward; Shift-click steps back. Clicking the row
/// label only selects.
#[gpui::test]
fn the_settings_value_chip_steps_and_a_row_click_only_selects(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let font = |cx: &gpui::VisualTestContext| shell.read_with(cx, |s, _| s.font_size);
    assert_eq!(font(&cx), crate::fontsize::FontSize::Medium);

    // Row 1 is Font size. Its chip:
    let chip = cx
        .debug_bounds("settings-value-1")
        .expect("the value chip paints");
    cx.simulate_click(chip.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        font(&cx),
        crate::fontsize::FontSize::Large,
        "click steps forward"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.settings.as_ref().unwrap().selected),
        1,
        "and selects the row"
    );
    // The row this chip steps is Font size itself, so the first click's
    // own effect (a bigger rem size) reflows the whole modal — the
    // chip's bounds must be re-read, exactly as `click_row` above does
    // for every click, rather than reusing the pre-click bounds.
    let chip = cx
        .debug_bounds("settings-value-1")
        .expect("the value chip still paints after the reflow");
    cx.simulate_click(
        chip.center(),
        gpui::Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    cx.run_until_parked();
    assert_eq!(
        font(&cx),
        crate::fontsize::FontSize::Medium,
        "shift+click steps back"
    );

    // A click on the row's label, twice: select only, never a step.
    let row = cx.debug_bounds("settings-row-1").expect("row paints");
    let label = gpui::point(row.origin.x + gpui::px(20.0), row.center().y);
    cx.simulate_click(label, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.simulate_click(label, gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        font(&cx),
        crate::fontsize::FontSize::Medium,
        "a second row click no longer steps"
    );
    assert!(
        !dialog_filter_is_focused(&shell, &mut cx),
        "the chip click ended in the sync: normal mode keeps the field blurred"
    );
}

/// Typing filters; the old `h`/`l` stepping keys are now just text,
/// and must not step anything on their way into the query.
#[gpui::test]
fn typing_filters_the_settings_rows(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    let before = shell.read_with(&cx, |shell, _| shell.font_size);
    cx.simulate_keystrokes("/");
    cx.simulate_input("font");
    let (query, selected) = shell.read_with(&cx, |shell, _| {
        let state = shell.settings.as_ref().unwrap();
        (state.query.clone(), state.selected)
    });
    assert_eq!(query, "font");
    assert_eq!(selected, 0, "a query selects the top match");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.font_size),
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
    cx.simulate_keystrokes("/");
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

/// Step Theme and Find style through real Tab keystrokes to cover the
/// setting-application arms beyond Font size. Open a fresh filtered dialog for each row
/// so selection setup remains independent.
#[gpui::test]
fn tab_steps_every_remaining_apply_setting_arm(cx: &mut gpui::TestAppContext) {
    {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        let before = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        cx.simulate_keystrokes("/");
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
        let before = shell.read_with(&cx, |shell, _| shell.find_style);
        cx.simulate_keystrokes("/");
        cx.simulate_input("keyboard");
        cx.simulate_keystrokes("tab");
        let after = shell.read_with(&cx, |shell, _| shell.find_style);
        assert_ne!(
            after, before,
            "tab on the Find style row should flip vim/fzf"
        );
    }
}

/// Leaving filter mode with Enter must not step a setting or close the dialog. In
/// normal mode, an unclaimed letter (`s`) likewise changes nothing; Enter's normal-mode
/// typeahead behavior is covered separately.
#[gpui::test]
fn enter_does_nothing_in_the_settings_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("j");
    let before = shell.read_with(&cx, |shell, _| shell.font_size);

    cx.simulate_keystrokes("s");
    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.font_size, before,
            "an unclaimed key must not step the value in normal mode"
        );
        assert!(shell.modal_open(), "and must not close the dialog");
    });

    cx.simulate_keystrokes("/");
    cx.simulate_input("font");
    cx.simulate_keystrokes("enter");
    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.font_size, before,
            "enter must not step the value in filter mode"
        );
        assert!(shell.modal_open(), "and must not close the dialog");
    });
}

/// The filter-exit Enter must be claimed before it reaches the shared `Input`. An input
/// change event could rerank the list and reset selection, so move off the first row
/// before asserting that Enter preserves the query, selection, value, and open dialog.
#[gpui::test]
fn enter_is_reserved_and_leaves_the_settings_dialog_untouched(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    // In filter mode, where the focused `Input` is what makes an
    // unclaimed `enter` harmful (see the doc comment above).
    cx.simulate_keystrokes("/ down down");
    let selected_before =
        shell.read_with(&cx, |shell, _| shell.settings.as_ref().unwrap().selected);
    assert_eq!(
        selected_before, 2,
        "sanity: two downs land on the third row"
    );
    let style_before = shell.read_with(&cx, |shell, _| shell.find_style);

    cx.simulate_keystrokes("enter");

    let (query, selected, style_after, open) = shell.read_with(&cx, |shell, _| {
        let state = shell.settings.as_ref().unwrap();
        (
            state.query.clone(),
            state.selected,
            shell.find_style,
            shell.modal_open(),
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
    assert_eq!(style_after, style_before, "enter must not step any value");
    assert!(open, "enter must not close the dialog");
}

/// Escape from a fresh dialog (normal mode, empty query) is the ladder's
/// last rung: it closes. Focus is already on the shell root in normal
/// mode, so the focus assertion here is a sanity check, not a restore —
/// the filter-mode close path (`/`, then the full ladder) is what
/// `settings_escape_walks_the_ladder_one_rung_at_a_time` covers.
#[gpui::test]
fn escape_closes_the_settings_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    cx.simulate_keystrokes("escape");
    shell.read_with(&cx, |shell, _| {
        assert!(!shell.modal_open());
        assert!(shell.settings.is_none(), "close_modal clears dialog state");
    });
    assert!(
        cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)),
        "the shell root holds focus after the close"
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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
        shell.read_with(&cx, |shell, _| !shell.modal_open()),
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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

/// The theme setter applies the selected theme live and keeps
/// `ThemeService::active_name` synchronized. Call the shared application path directly;
/// separate tests drive row stepping through keys.
#[gpui::test]
fn settings_dialog_theme_setter_applies_live_through_theme_service(cx: &mut gpui::TestAppContext) {
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
    // See `test_services`'s own comment: mirroring `builtin` here (rather
    // than leaving it at its inherited empty vec) costs nothing and keeps
    // this fixture a real `(config, builtin)` pair.
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
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
    // See `test_services`'s own comment: mirroring `builtin` here (rather
    // than leaving it at its inherited empty vec) costs nothing and keeps
    // this fixture a real `(config, builtin)` pair.
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
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

/// Reload folds `[app] modules.default` diagnostics into the config batch and forwards
/// them to the diagnostics entity. An invalid default is a warning, so the reload must
/// still apply.
#[gpui::test]
fn a_modules_default_key_produces_a_warning_on_reload(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);

    let cfg = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[modules]\ndefault = \"blotter\"\n").unwrap()],
        desk: None,
        user: None,
    });
    shell.update(&mut cx, |shell, cx| shell.apply_reload(cfg, cx));

    let diagnostics = shell.read_with(&cx, |shell, _| shell.diagnostics().clone());
    let config_diags = diagnostics.read_with(&cx, |d, _| d.config.clone());
    let hit = config_diags
        .iter()
        .find(|d| d.message.contains("modules.default"))
        .unwrap_or_else(|| panic!("expected a modules.default diagnostic, got {config_diags:?}"));
    assert_eq!(
        hit.severity,
        geode_core::config::Severity::Warning,
        "the key is dead config, not invalid config: {hit:?}"
    );

    shell.read_with(&cx, |shell, _| match &shell.last_reload {
        crate::reload::ReloadOutcome::Applied { warnings } => assert!(
            warnings.iter().any(|w| w.contains("modules.default")),
            "a warning must not reject the reload, and must be reported: {warnings:?}"
        ),
        other => panic!("a warning-only reload must still apply, got {other:?}"),
    });
}

/// Startup folds default-module and modifier-alias diagnostics into the entity's config
/// section, including diagnostics computed outside the loaded config's own list.
#[gpui::test]
fn a_modules_default_key_is_in_the_diagnostics_entity_at_startup(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    services.config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[modules]\ndefault = \"blotter\"\n").unwrap()],
        desk: None,
        user: None,
    });
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    let diagnostics = shell.read_with(&cx, |shell, _| shell.diagnostics().clone());
    let config_diags = diagnostics.read_with(&cx, |d, _| d.config.clone());
    let hit = config_diags
        .iter()
        .find(|d| d.message.contains("modules.default"))
        .unwrap_or_else(|| panic!("expected a modules.default diagnostic, got {config_diags:?}"));
    assert_eq!(
        hit.severity,
        geode_core::config::Severity::Warning,
        "the key is dead config, not invalid config: {hit:?}"
    );
}

/// `[timeseries] default_source` resolves at startup and on hot reload. Both paths
/// report a warning when the name has no configured fetch source.
#[gpui::test]
fn a_stale_default_source_warns_at_startup_and_clears_on_reload(cx: &mut gpui::TestAppContext) {
    use crate::series::SeriesSettings;
    const DATASETS: &str = "[series]\nfamily = \"series\"\n";
    const SOURCES: &str = "[demo_kdb]\nadapter = \"demo_kdb\"\ndataset = \"series\"\n";
    let config = |app: &str| {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("datasets", DATASETS).unwrap(),
                LayerDoc::builtin("sources", SOURCES).unwrap(),
                LayerDoc::builtin("app", app).unwrap(),
            ],
            desk: None,
            user: None,
        })
    };

    let mut services = test_services();
    services.config = config("[timeseries]\ndefault_source = \"nope\"\n");
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |shell, _| shell.diagnostics().clone());

    let config_diags = diagnostics.read_with(&cx, |d, _| d.config.clone());
    let hit = config_diags
        .iter()
        .find(|d| d.message.contains("default_source"))
        .unwrap_or_else(|| panic!("expected a default_source diagnostic, got {config_diags:?}"));
    assert_eq!(
        hit.severity,
        geode_core::config::Severity::Warning,
        "a stale default costs one explicit @source, it does not invalidate config: {hit:?}"
    );

    shell.update(&mut cx, |shell, cx| {
        shell.apply_reload(config("[timeseries]\ndefault_source = \"demo_kdb\"\n"), cx)
    });
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.default_source.clone()),
        Some("demo_kdb".to_string()),
        "the reload re-resolves the field"
    );
    assert_eq!(
        cx.update(|_, cx| cx.global::<SeriesSettings>().default_source.clone()),
        Some("demo_kdb".to_string()),
        "and republishes the global"
    );
    let config_diags = diagnostics.read_with(&cx, |d, _| d.config.clone());
    assert!(
        !config_diags
            .iter()
            .any(|d| d.message.contains("default_source")),
        "the reload's own diagnostics replace the stale warning: {config_diags:?}"
    );

    // And the reload computes the diagnostic itself, rather than only
    // ever inheriting one from startup: a reload INTO a stale default
    // warns again.
    shell.update(&mut cx, |shell, cx| {
        shell.apply_reload(config("[timeseries]\ndefault_source = \"gone\"\n"), cx)
    });
    let config_diags = diagnostics.read_with(&cx, |d, _| d.config.clone());
    assert!(
        config_diags
            .iter()
            .any(|d| d.message.contains("'gone'") && d.message.contains("demo_kdb")),
        "a reload into a stale default must warn, naming it and what is configured: {config_diags:?}"
    );
    assert_eq!(
        cx.update(|_, cx| cx.global::<SeriesSettings>().default_source.clone()),
        Some("gone".to_string()),
        "the global carries what config says; the diagnostic is how the trader learns it is stale"
    );
}

/// The invalid `keymap.mod = "ctrl"` alias produces an error at startup. The
/// diagnostics entity must receive it so the user can see why their configured modifier
/// was refused.
#[gpui::test]
fn a_refused_keymap_mod_is_in_the_diagnostics_entity_at_startup(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    services.config = config_with_mod("ctrl");
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    let diagnostics = shell.read_with(&cx, |shell, _| shell.diagnostics().clone());
    let config_diags = diagnostics.read_with(&cx, |d, _| d.config.clone());
    let hit = config_diags
        .iter()
        .find(|d| d.message.contains("keymap.mod"))
        .unwrap_or_else(|| panic!("expected a keymap.mod diagnostic, got {config_diags:?}"));
    assert_eq!(
        hit.severity,
        geode_core::config::Severity::Error,
        "a refused alias is an error, not a warning: {hit:?}"
    );
}

/// Startup keymap diagnostics arrive through `ShellServices::keymap_diagnostics`,
/// populated by the app after building the keymap. `ShellView::new` must forward them
/// immediately; an unknown action should be visible before any hot reload.
#[gpui::test]
fn startup_keymap_diagnostics_are_in_the_diagnostics_entity(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let user_doc = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table: "[[bindings]]\n[bindings.keys]\n\"ctrl+q\" = \"nosuch::action\"\n"
            .parse()
            .unwrap(),
    };
    let (keymap, keymap_diags) = crate::keymap::build_keymap(
        &[
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            user_doc,
        ],
        default_mod(),
        &services.registry,
    );
    assert!(
        keymap_diags
            .iter()
            .any(|d| d.message.contains("nosuch::action")),
        "fixture check: build_keymap must have diagnosed the unknown action, got {keymap_diags:?}"
    );
    // Exactly what `main.rs` does with the pair.
    services.keymap = keymap;
    services.keymap_diagnostics = keymap_diags;

    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |shell, _| shell.diagnostics().clone());
    let config_diags = diagnostics.read_with(&cx, |d, _| d.config.clone());
    assert!(
        config_diags
            .iter()
            .any(|d| d.message.contains("nosuch::action")),
        "the keymap diagnostic must reach the entity at startup, got {config_diags:?}"
    );
}

/// The add-direction setter updates `ShellView::add_direction` immediately and persists
/// `[tiles] add` to the user `app.toml` on the background executor. Call the handler
/// directly to isolate state publication and persistence from row navigation.
#[gpui::test]
fn set_add_direction_updates_the_shell_state_and_persists(cx: &mut gpui::TestAppContext) {
    use crate::tileadd::AddDirection;
    let dir = tempfile::tempdir().unwrap();
    let (window, mut cx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.add_direction),
        AddDirection::Auto
    );
    cx.update(|_window, cx| settings_view::set_add_direction(&shell, AddDirection::Vertical, cx));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.add_direction),
        AddDirection::Vertical
    );
    cx.run_until_parked();
    let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
    assert!(
        text.contains("[tiles]") && text.contains("add = \"vertical\""),
        "{text}"
    );
}

/// `[tiles] add` resolves at startup and re-resolves on hot reload — the
/// same two paths `find_style` rides just above.
#[gpui::test]
fn a_configured_add_direction_resolves_at_startup_and_on_reload(cx: &mut gpui::TestAppContext) {
    use crate::tileadd::AddDirection;
    let mut services = test_services();
    services.config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[tiles]\nadd = \"horizontal\"\n").unwrap()],
        desk: None,
        user: None,
    });
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.add_direction),
        AddDirection::Horizontal,
        "[tiles] add = \"horizontal\" should resolve at startup"
    );
    let new_config = config_with_mod("alt");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));
    assert_eq!(
        shell.read_with(&cx, |s, _| s.add_direction),
        AddDirection::Auto,
        "a reload without [tiles] add should re-resolve to the default"
    );
}

/// The settings dialog's `tab` stepping reaches the fifth (last) row
/// too: with no filter narrowing the list, four `down`s land on Add
/// tile, and `tab` steps `Auto → Horizontal` — [`step`]'s wrap, since
/// `Auto` is `AddDirection::ALL`'s last value. Companion to `tab_steps_
/// every_remaining_apply_setting_arm` above, which drives the earlier
/// rows through a filtered query each; this one exercises the row
/// `apply_setting`'s `SettingId::AddDirection` arm dispatches to.
#[gpui::test]
fn tab_steps_the_add_direction_row_and_wraps(cx: &mut gpui::TestAppContext) {
    use crate::tileadd::AddDirection;
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.add_direction),
        AddDirection::Auto,
        "sanity: Auto is the default"
    );
    cx.simulate_keystrokes("down down down down");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.settings.as_ref().unwrap().selected),
        4,
        "sanity: four downs land on the fifth row"
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.add_direction),
        AddDirection::Horizontal,
        "tab on the Add tile row should wrap Auto -> Horizontal"
    );
}

/// The line-number setting cycles off → on → rel and publishes every step through
/// `UiSettings`, allowing observing modules to repaint immediately.
#[gpui::test]
fn tab_steps_the_line_numbers_row_and_publishes_the_global(cx: &mut gpui::TestAppContext) {
    use crate::linenumbers::{LineNumbers, UiSettings};
    let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
    assert_eq!(
        cx.update(|_, cx| cx.global::<UiSettings>().line_numbers),
        LineNumbers::Off,
        "sanity: the global is seeded Off at startup"
    );
    cx.simulate_keystrokes("down down down");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.settings.as_ref().unwrap().selected),
        3,
        "sanity: three downs land on the Line numbers row"
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.line_numbers),
        LineNumbers::On
    );
    assert_eq!(
        cx.update(|_, cx| cx.global::<UiSettings>().line_numbers),
        LineNumbers::On,
        "the step reached the global"
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(
        cx.update(|_, cx| cx.global::<UiSettings>().line_numbers),
        LineNumbers::Relative
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(
        cx.update(|_, cx| cx.global::<UiSettings>().line_numbers),
        LineNumbers::Off,
        "the row wraps"
    );
}

/// The timeseries default-source row cycles through `(none)` and configured fetch
/// sources, publishing each step through `SeriesSettings` for observing tiles.
#[gpui::test]
fn the_default_source_row_steps_over_the_fetch_sources_and_publishes_the_global(
    cx: &mut gpui::TestAppContext,
) {
    use crate::series::SeriesSettings;
    // Two fetch sources (a non-directory adapter over a `series`
    // dataset) plus one directory source that must NOT show up.
    let mut services = test_services();
    services.config = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin(
                "datasets",
                "[series]\nfamily = \"series\"\n[risk]\n[risk.columns]\n\
                 book = { type = \"utf8\", role = \"key\" }\npv = { type = \"f64\", role = \"value\" }\n",
            )
            .unwrap(),
            LayerDoc::builtin(
                "sources",
                "[demo_kdb]\nadapter = \"demo_kdb\"\ndataset = \"series\"\n\
                 [demo_rest]\nadapter = \"demo_rest\"\ndataset = \"series\"\n\
                 [files]\ndataset = \"risk\"\npaths = [\"/tmp/*.csv\"]\n",
            )
            .unwrap(),
        ],
        desk: None,
        user: None,
    });
    let (shell, mut cx) = dialog_test_shell_with(cx, services, "settings::open");
    cx.update(|_, cx| {
        let series = cx.global::<SeriesSettings>();
        assert_eq!(
            series.default_source, None,
            "sanity: nothing configured, so the global is seeded with no default"
        );
        assert_eq!(
            series.names(),
            vec!["demo_kdb", "demo_rest"],
            "the directory source is not a fetch source"
        );
    });

    cx.simulate_keystrokes("down down down down down");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.settings.as_ref().unwrap().selected),
        5,
        "sanity: five downs land on the Default series source row"
    );

    cx.simulate_keystrokes("space");
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.default_source.clone()),
        Some("demo_kdb".to_string())
    );
    assert_eq!(
        cx.update(|_, cx| cx.global::<SeriesSettings>().default_source.clone()),
        Some("demo_kdb".to_string()),
        "the step reached the global"
    );
    cx.simulate_keystrokes("space");
    assert_eq!(
        cx.update(|_, cx| cx.global::<SeriesSettings>().default_source.clone()),
        Some("demo_rest".to_string())
    );
    cx.simulate_keystrokes("space");
    assert_eq!(
        cx.update(|_, cx| cx.global::<SeriesSettings>().default_source.clone()),
        None,
        "the row wraps back to (none)"
    );
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.default_source.clone()),
        None,
        "and the view's own field with it"
    );
    assert_eq!(
        cx.update(|_, cx| cx.global::<SeriesSettings>().names()),
        vec!["demo_kdb", "demo_rest"],
        "stepping the default never disturbs the fetch-source list"
    );
}

/// The palette action is the same setter as the row: one dispatch
/// steps once and publishes the global.
#[gpui::test]
fn the_line_numbers_cycle_action_steps_the_setting_once(cx: &mut gpui::TestAppContext) {
    use crate::linenumbers::{LineNumbers, UiSettings};
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    let cycle = |cx: &mut gpui::VisualTestContext| {
        cx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.dispatch(
                    &ActionId("ui::line_numbers_cycle".to_string()),
                    None,
                    window,
                    cx,
                );
            });
        });
    };
    cycle(&mut cx);
    assert_eq!(
        cx.update(|_, cx| cx.global::<UiSettings>().line_numbers),
        LineNumbers::On
    );
    cycle(&mut cx);
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell.line_numbers),
        LineNumbers::Relative
    );
}

/// Opening a modal through `dialog::open_shell_dialog` cancels any pending keymap
/// sequence and clears the which-key overlay. The fixture adds `g g` because the
/// builtin keymap has no sequence binding.
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
        shell.read_with(&cx, |shell, _| shell.modal_open()),
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

/// `open_shell_dialog` closes an open palette. Call the utility directly: selecting a
/// settings action through the palette already closes the palette before dispatch,
/// which would hide the utility's own cleanup behavior.
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
            dialog::open_shell_dialog(
                shell,
                window,
                cx,
                dialog::DialogKind::Plain,
                "Test modal",
                |_shell, _window, _cx| div().into_any_element(),
            );
        });
    });

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "open_shell_dialog should have closed the open palette"
    );
    assert!(
        shell.read_with(&cx, |shell, _| shell.modal_open()),
        "open_shell_dialog should have set ShellView's own modal state"
    );
}

// Tooltips: sidebar discs and the profile icon.

/// The hover mechanism end to end: no tooltip before hover, the disc's
/// own bounds paint a tooltip after the show delay, and the chord chip
/// names the live binding for `workspace::switch_1` (`mod+1`, which
/// resolves to `alt+1` under `default_mod()` — deterministic, not
/// platform-dependent, so only that arm need hold; both are checked for
/// safety against a future default-mod change).
#[gpui::test]
fn hovering_a_workspace_disc_shows_its_name_and_chord(cx: &mut gpui::TestAppContext) {
    let (mut vcx, _view) = dock_test_shell(cx);
    // Workspace 1 is always painted (it is the active one at startup).
    let disc = vcx
        .debug_bounds("sidebar-workspace-1")
        .expect("workspace 1's disc is painted");
    assert!(
        vcx.debug_bounds("tip-sidebar-workspace-1").is_none(),
        "no tooltip before hover"
    );
    vcx.simulate_mouse_move(
        disc.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    let tip = vcx
        .debug_bounds("tip-sidebar-workspace-1")
        .expect("the tooltip is painted after the show delay");
    assert!(tip.size.width > gpui::px(0.));
    // The chord chip carries the builtin binding's text.
    assert!(
        vcx.debug_bounds("tip-sidebar-workspace-1-chord-mod+1")
            .is_some()
            || vcx
                .debug_bounds("tip-sidebar-workspace-1-chord-alt+1")
                .is_some(),
        "the chord chip names mod+1 (spelled with the configured mod alias)"
    );
}

/// The profile icon's tooltip names `settings::open` and its builtin
/// chord.
#[gpui::test]
fn hovering_the_profile_icon_names_settings_and_its_chord(cx: &mut gpui::TestAppContext) {
    let (mut vcx, _view) = dock_test_shell(cx);
    let icon = vcx
        .debug_bounds("sidebar-profile")
        .expect("profile icon painted");
    vcx.simulate_mouse_move(
        icon.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-sidebar-profile").is_some());
    assert!(
        vcx.debug_bounds("tip-sidebar-profile-chord-ctrl+,")
            .is_some(),
        "ctrl+, is the builtin"
    );
}

/// Idle costs nothing: no tooltip without hover, and the tooltip goes
/// away once the mouse leaves the disc.
#[gpui::test]
fn a_tooltip_goes_away_when_the_mouse_leaves(cx: &mut gpui::TestAppContext) {
    let (mut vcx, _view) = dock_test_shell(cx);
    let disc = vcx.debug_bounds("sidebar-workspace-1").unwrap();
    vcx.simulate_mouse_move(
        disc.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-sidebar-workspace-1").is_some());
    // Somewhere far from the rail — the window's far corner.
    vcx.simulate_mouse_move(
        gpui::point(gpui::px(900.), gpui::px(500.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("tip-sidebar-workspace-1").is_none(),
        "the tooltip is gone"
    );
}

/// Large font size scales the status bar, sidebar, and dialog rows by the same 14/12
/// ratio as the text. The tile surface gives up the corresponding space so the enlarged
/// status bar cannot overlap the bottom tile.
#[gpui::test]
fn chrome_and_dialog_rows_follow_the_font_size(cx: &mut gpui::TestAppContext) {
    use crate::fontsize::FontSize;
    use crate::shell::{sidebar, status};

    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);

    let measure = |cx: &mut gpui::VisualTestContext| {
        cx.simulate_keystrokes("ctrl-,");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let list = cx
            .debug_bounds("settings-list")
            .expect("settings list painted");
        let bar = cx
            .debug_bounds("shell-status-bar")
            .expect("status bar painted");
        let (viewport_h, sidebar_w, status_h) = cx.update(|window, _| {
            (
                f32::from(window.viewport_size().height),
                sidebar::width(window),
                status::height(window),
            )
        });
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            f32::from(list.size.height),
            f32::from(bar.size.height),
            f32::from(bar.origin.y),
            viewport_h,
            sidebar_w,
            status_h,
        )
    };

    let at_medium = measure(&mut cx);
    assert_eq!(
        at_medium.1,
        status::HEIGHT,
        "medium IS the design rem: the bar is its literal"
    );
    assert_eq!(at_medium.4, sidebar::WIDTH);

    cx.update(|_, cx| crate::shell::settings_view::set_font_size(&shell, FontSize::Large, cx));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let at_large = measure(&mut cx);

    let ratio = FontSize::Large.rem_px() / FontSize::Medium.rem_px();
    let close = |a: f32, b: f32| (a - b).abs() < 0.5;
    assert!(
        close(at_large.0, at_medium.0 * ratio),
        "the dialog list should scale with the rem: {} → {} (expected ×{ratio})",
        at_medium.0,
        at_large.0
    );
    assert!(
        close(at_large.1, at_medium.1 * ratio),
        "the status bar should scale with the rem: {} → {}",
        at_medium.1,
        at_large.1
    );
    assert!(
        close(at_large.4, at_medium.4 * ratio),
        "sidebar {} → {}",
        at_medium.4,
        at_large.4
    );
    assert!(
        close(at_large.5, at_large.1),
        "status::height agrees with the painted bar"
    );
    // The bar still sits flush at the bottom, and the tile surface has
    // given up its extra height rather than being painted over.
    assert!(
        close(at_large.2 + at_large.1, at_large.3),
        "status bar bottom {} + {} should meet the viewport bottom {}",
        at_large.2,
        at_large.1,
        at_large.3
    );

    // Measure the painted sidebar rail as well as its reserved width; checking the
    // width helper alone would miss a render mismatch. The scale is still Large.
    let rail = cx
        .debug_bounds("shell-sidebar")
        .expect("sidebar rail painted");
    assert!(
        close(f32::from(rail.size.width), at_large.4),
        "the painted rail ({:?}) should be the width the surface reserved ({})",
        rail.size.width,
        at_large.4
    );

    // At Large scale, the command strip uses its scaled height and ends exactly on the
    // focused tile's bottom border.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes(":");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let strip = cx.debug_bounds("command-line").expect("the strip painted");
    let (strip_height, tile_bottom) = cx.update(|window, app| {
        let expected = crate::shell::scale::design_px(
            crate::shell::commandline_view::HEIGHT,
            window.rem_size(),
        );
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
        let focused = workspace.tree().focused().expect("a focused tile");
        let (tree_area, _) = crate::tiling::dock_layout(workspace.docks(), area);
        let r = workspace
            .tree()
            .layout(tree_area)
            .into_iter()
            .find(|(t, _)| *t == focused)
            .expect("focused tile laid out")
            .1;
        (expected, toolbar_height + r.y + r.h)
    });
    assert!(
        close(f32::from(strip.size.height), strip_height),
        "the strip should paint at its scaled height {strip_height}, got {:?}",
        strip.size.height
    );
    assert!(
        close(
            f32::from(strip.origin.y + strip.size.height),
            tile_bottom - 1.0
        ),
        "the strip's bottom {:?} should sit on the tile's bottom border ({} - 1)",
        strip.origin.y + strip.size.height,
        tile_bottom
    );
}
