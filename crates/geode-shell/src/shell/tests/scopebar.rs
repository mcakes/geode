//! Scope-bar rendering, undo and redo, input sessions, chord dispatch, and focus
//! restoration.

use super::*;
use crate::shell::objectdialog;
use geode_core::config::ConfigSources;
use geode_core::scope::{DimensionSelection, Scope};

fn book_scope(book: &str) -> Scope {
    Scope {
        dimensions: vec![DimensionSelection {
            column: "book".into(),
            values: vec![book.into()],
        }],
        ..Scope::default()
    }
}

/// Build the keymap with raw `Modifiers::CTRL` to prove `mod+z` and `mod+shift+z` are
/// not hardcoded to Alt. This bypasses config parsing deliberately: `ctrl` is not an
/// accepted configured alias, but the matcher accepts a supplied modifier value.
fn test_services_with_ctrl_alias() -> ShellServices {
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources::default());
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    let mod_alias = Modifiers::CTRL;
    let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
    assert!(diags.is_empty(), "{diags:?}");
    let (theme, warnings) = crate::theme::load_bundled();
    assert!(warnings.is_empty(), "{warnings:?}");
    ShellServices {
        config,
        builtin,
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path: None,
        roster: crate::module::ModuleRoster::default(),
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
        restored_palette_usage: crate::palette_usage::PaletteUsage::new(),
        log: None,
        action_tail: std::sync::Arc::new(std::sync::Mutex::new(
            crate::diagnostics::ActionTail::new(),
        )),
        keymap_diagnostics: Vec::new(),
        keymap_fragments: Vec::new(),
        keymap_fragment_diagnostics: Vec::new(),
    }
}

#[gpui::test]
fn ctrl_z_and_ctrl_shift_z_undo_and_redo_the_scope(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services_with_ctrl_alias());
    let shell = shell_of(&window, &mut cx);

    let a = book_scope("A");
    let b = book_scope("B");
    shell.update(&mut cx, |shell, cx| {
        shell.frame().update(cx, |f, _| {
            f.shared_mut().set_scope(a.clone());
            f.shared_mut().set_scope(b.clone());
        });
    });

    cx.simulate_keystrokes("ctrl-z");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame().read(cx).shared().scope().clone()),
        a,
        "ctrl-z must restore the scope before the last set_scope"
    );

    cx.simulate_keystrokes("ctrl-shift-z");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame().read(cx).shared().scope().clone()),
        b,
        "ctrl-shift-z must return to the scope ctrl-z just undid"
    );
}

#[gpui::test]
fn typing_in_the_field_sets_the_frame_text_per_keystroke_and_enter_blurs(
    cx: &mut gpui::TestAppContext,
) {
    // The fixture uses `default_mod()` (Alt), so `mod+/` resolves to `alt+/`.
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    // `InputState`'s own `Focus`/`Blur` events fire from the focus-path
    // diff `Window::draw` computes (unlike `Change`/`PressEnter`, emitted
    // straight from the key handler), and that diff is only meaningful
    // for an active window (`drag.rs`'s `window_deactivation_mid_drag_
    // ends_both_drag_kinds` doc comment has the same activation-plumbing
    // note) — without this, `begin_scope_session` never runs.
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/"); // mod+/ under the default mod
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("sp");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("sp")
    );
    let v_after_two = frame.read_with(&vcx, |f, _| f.shared().versions().scope);
    vcx.simulate_input("x");
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().versions().scope),
        v_after_two + 1
    );
    vcx.simulate_keystrokes("enter");
    assert!(!filter_is_focused(&shell, &mut vcx));
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("spx")
    );
    // One undo entry for the whole session.
    frame.update(&mut vcx, |f, _| assert!(f.shared_mut().undo_scope()));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None
    );
}

#[gpui::test]
fn escape_restores_the_text_the_field_had_when_focused(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    // `cx.notify()` is required here (not `|f, _|`, discarding `cx`): a
    // `Frame` method never notifies on its own (same reasoning `frame.rs`
    // documents throughout) — without it `on_frame_changed`'s reflection
    // into `filter_input` never runs, so the field would still be empty
    // when Escape captures its base value, as the third test below does.
    frame.update(&mut vcx, |f, cx| {
        if f.shared_mut().set_text(Some("old".into())) {
            cx.notify();
        }
    });
    // See the sibling test's comment: `Focus` only fires from an active
    // window's `Window::draw` focus-path diff.
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("new");
    vcx.simulate_keystrokes("escape");
    assert!(!filter_is_focused(&shell, &mut vcx));
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("old")
    );
    let value = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).value().to_string());
    assert_eq!(value, "old");
}

#[gpui::test]
fn escape_after_a_session_edit_does_not_let_undo_resurrect_the_abandoned_text(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    // Set the pre-session text through the normal history path. Canceling the later
    // editing session must not push the abandoned text as another undo entry.
    frame.update(&mut vcx, |f, cx| {
        if f.shared_mut().set_text(Some("old".into())) {
            cx.notify();
        }
    });
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("new");
    vcx.simulate_keystrokes("escape");
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("old"),
        "escape must restore the pre-focus text"
    );

    // Escape restores the session's original scope and removes its now-redundant undo
    // entry. One undo must skip the whole canceled editing session, with neither the
    // abandoned value nor a no-op step in history.
    assert!(frame.update(&mut vcx, |f, _| f.shared_mut().undo_scope()));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None
    );

    // Nothing further to undo.
    assert!(!frame.update(&mut vcx, |f, _| f.shared_mut().undo_scope()));
}

#[gpui::test]
fn a_text_set_elsewhere_shows_in_the_field_and_a_chip_close_drops_the_dimension(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        let mut s = f.shared().scope().clone();
        s.text = Some("from-tile".into());
        s.dimensions.push(geode_core::scope::DimensionSelection {
            column: "book".into(),
            values: vec!["BK001".into()],
        });
        f.shared_mut().set_scope(s);
        cx.notify();
    });
    vcx.run_until_parked();
    let value = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).value().to_string());
    assert_eq!(value, "from-tile");
    let close = vcx
        .debug_bounds("scope-chip-close-book")
        .expect("chip painted");
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    assert!(frame.read_with(&vcx, |f, _| f.shared().scope().dimensions.is_empty()));
    assert!(vcx.debug_bounds("scope-chip-close-book").is_none());
}

/// The save chip appears only for a nonempty frame scope. Clicking it opens naming
/// seeded from the frame, matching `scope::save_current`.
#[gpui::test]
fn the_save_chip_only_paints_with_a_savable_scope_and_opens_naming(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    assert!(
        vcx.debug_bounds("scope-save-chip").is_none(),
        "an empty scope has nothing to save"
    );

    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(book_scope("BK000"));
        cx.notify();
    });
    vcx.run_until_parked();

    let save = vcx
        .debug_bounds("scope-save-chip")
        .expect("the save chip should paint once the scope is non-empty");
    vcx.simulate_click(save.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .object_dialog
            .as_ref()
            .map(|d| d.stage.clone())),
        Some(objectdialog::Stage::Naming)
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s
            .object_dialog
            .as_ref()
            .map(|d| d.naming_seed.clone())),
        Some(objectdialog::NameSeed::FromFrame)
    );
    // The naming field retains focus through the opening mouse-down because the dialog
    // helper prevents the default focus transfer.
    vcx.simulate_input("eu");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "eu",
        "typing after the click reaches the naming field"
    );
}

/// The `+` chip paints regardless of the scope's own state — adding a
/// filter is how a scope starts — and its menu's "Dimension…" row opens
/// the same picker `mod+p`/`frame::pick` does, with the filter field
/// HOLDING the focus the open gave it, so typing after the click lands
/// (the mouse-opened-dialog rule; the row stops its press's propagation,
/// and `open_shell_dialog_with_key`'s `prevent_default` covers every
/// other mouse-opened dialog).
#[gpui::test]
fn the_pick_chip_is_always_present_and_its_menu_opens_the_picker(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    assert!(shell.read_with(&vcx, |s, cx| s.frame().read(cx).shared().scope().is_empty()));

    let pick = vcx
        .debug_bounds("scope-pick-chip")
        .expect("the pick chip should paint even with an empty scope");
    vcx.simulate_click(pick.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.add_filter_menu.is_some()));
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.is_none()),
        "the + opens its menu, not the picker"
    );
    let row = vcx
        .debug_bounds("scope-add-menu-row-dimension")
        .expect("the menu's Dimension row paints");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(shell.read_with(&vcx, |s, _| s.picker.is_some()));
    assert!(
        shell.read_with(&vcx, |s, _| s.add_filter_menu.is_none()),
        "a commit closes the menu"
    );
    let focused = vcx.update(|window, cx| {
        let input = shell.read(cx).dialog_input.clone();
        input.read(cx).focus_handle(cx).is_focused(window)
    });
    assert!(
        focused,
        "the picker's field must keep focus through the rest of the mouse-down"
    );
    vcx.simulate_input("bo");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.picker.as_ref().map(|p| p.query.clone())),
        Some("bo".to_string()),
        "typing after the click reaches the filter"
    );
}

/// A `[scopes]` doc with one entry, "eu" — no `[datasets]` doc at all,
/// so `saved_scopes_from_doc`'s own per-dataset validation (`schema.
/// datasets.iter().map(...).min_by_key(...).unwrap_or_default()`) sees
/// zero datasets and accepts every scope trivially; a bare `text` scope
/// needs nothing else to load.
const SCOPES_DOC: &str = "[eu]\ntext = \"eu\"\n";

/// `test_services()` with a real `scopes` doc (so `scope::eu` is a real,
/// dispatchable action id) and `register_scope_actions` run over it — the
/// two-step registration `main.rs` itself does (`register_pick_actions`
/// then `register_scope_actions`), reproduced here rather than through
/// `test_services()` (whose whole point is an *empty* config — see that
/// function's own comment on why it still calls `register_scope_actions`
/// anyway, over nothing).
fn services_with_saved_scope() -> ShellServices {
    services_with_builtin_docs(vec![LayerDoc::builtin("scopes", SCOPES_DOC).unwrap()])
}

/// [`services_with_saved_scope`]'s registration over any builtin docs.
pub(super) fn services_with_builtin_docs(docs: Vec<LayerDoc>) -> ShellServices {
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: docs,
        ..ConfigSources::default()
    });
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    register_scope_actions(&mut registry, &crate::shell::saved_scopes(&config, false));
    let mod_alias = default_mod();
    let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
    assert!(diags.is_empty(), "{diags:?}");
    let (theme, warnings) = crate::theme::load_bundled();
    assert!(warnings.is_empty(), "{warnings:?}");
    ShellServices {
        config,
        builtin,
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path: None,
        roster: crate::module::ModuleRoster::default(),
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
        restored_palette_usage: crate::palette_usage::PaletteUsage::new(),
        log: None,
        action_tail: std::sync::Arc::new(std::sync::Mutex::new(
            crate::diagnostics::ActionTail::new(),
        )),
        keymap_diagnostics: Vec::new(),
        keymap_fragments: Vec::new(),
        keymap_fragment_diagnostics: Vec::new(),
    }
}

/// Each saved scope has a `scope::<name>` action registered by `register_scope_actions`
/// and dispatched through the shell's scope-action path.
#[gpui::test]
fn dispatching_scope_name_loads_the_saved_scope(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_saved_scope());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None
    );

    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(&ActionId("scope::eu".into()), None, window, cx);
        });
    });

    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("eu"),
        "dispatching scope::eu must load the saved scope"
    );
}

/// Chords still dispatch from the focused text field. Shift alone remains typing:
/// `shift+d` enters `D` instead of duplicating the workspace tile.
#[gpui::test]
fn a_chord_typed_into_the_focused_field_dispatches_and_a_shifted_letter_types(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    // A tile to duplicate — on an empty workspace `duplicate_horizontal`
    // is a no-op and the shift+d check below would prove nothing.
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/"); // mod+/ under the default mod
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let tiles_before = shell.read_with(&vcx, |shell, _| {
        shell.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(tiles_before, 1);
    vcx.simulate_keystrokes("shift-d");
    assert_eq!(
        shell.read_with(&vcx, |shell, _| {
            shell.services.workspaces.active().tree().tiles().len()
        }),
        tiles_before,
        "shift+d in the field is typing, not duplicate_horizontal"
    );
    assert!(filter_is_focused(&shell, &mut vcx));

    vcx.simulate_keystrokes("ctrl-,");
    assert!(
        shell.read_with(&vcx, |shell, _| shell.modal_open()),
        "ctrl+, from the focused field must open the settings dialog"
    );
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |shell, _| !shell.modal_open()));

    vcx.simulate_keystrokes("alt-/");
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("ctrl-k");
    assert!(
        shell.read_with(&vcx, |shell, _| shell.palette.is_some()),
        "ctrl+k from the focused field must open the palette"
    );
}

/// While the text field has focus, the keyboard belongs to the field and
/// the shell's chrome, not to the tile that happens to be focused in the
/// layout: a chord bound in the focused occupant's own context does not
/// reach it.
#[gpui::test]
fn a_chord_in_the_focused_tiles_own_context_does_not_fire_from_the_field(
    cx: &mut gpui::TestAppContext,
) {
    let (mut services, log) = services_with_recorder();
    let module_doc = LayerDoc::builtin(
        "keymap",
        "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"ctrl+g\" = \"rec::noop\"\n",
    )
    .unwrap();
    services.keymap = test_keymap(&services.registry, &[module_doc]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();

    // Sanity: from the tile, the chord reaches the occupant.
    vcx.simulate_keystrokes("ctrl-g");
    assert!(
        log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(_, a, _) if a.0 == "rec::noop"
        )),
        "{:?}",
        log.borrow()
    );
    log.borrow_mut().clear();

    vcx.simulate_keystrokes("alt-/");
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("ctrl-g");
    assert!(
        !log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(_, a, _) if a.0 == "rec::noop"
        )),
        "a tile-context chord must not fire while the field is focused: {:?}",
        log.borrow()
    );
    assert!(filter_is_focused(&shell, &mut vcx));
}

/// A chord that moves the frame's text while the field keeps focus —
/// `mod+z` undoing the session's own typing — is reflected back into the
/// field, so what the trader sees is what the frame holds.
#[gpui::test]
fn a_scope_undo_chord_from_the_field_reflects_the_frames_text_into_it(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/");
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("sp");
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.shared().scope().text.clone())
            .as_deref(),
        Some("sp")
    );
    vcx.simulate_keystrokes("alt-z"); // mod+z under the default mod
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None
    );
    assert!(filter_is_focused(&shell, &mut vcx));
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).value().to_string()),
        "",
        "the field must show the undone (empty) text, not the typed one"
    );
}

/// An unbind in a higher layer governs the field's chords exactly as it
/// governs the shell's: `"ctrl+k" = "none"` swallows the chord rather than
/// letting the builtin palette toggle through — and swallowed means
/// swallowed: `none` is not dispatched to the focused occupant either
/// (`dispatch` hands every id it does not know to the focused tile).
#[gpui::test]
fn an_unbound_chord_typed_into_the_field_is_swallowed(cx: &mut gpui::TestAppContext) {
    let (mut services, log) = services_with_recorder();
    let unbind = LayerDoc::builtin(
        "keymap",
        "[[bindings]]\n[bindings.keys]\n\"ctrl+k\" = \"none\"\n",
    )
    .unwrap();
    services.keymap = test_keymap(&services.registry, &[unbind]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/");
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("ctrl-k");
    assert!(shell.read_with(&vcx, |shell, _| shell.palette.is_none()));
    assert!(filter_is_focused(&shell, &mut vcx));
    assert!(
        !log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(_, a, _) if a.0 == "none"
        )),
        "a swallowed chord must not reach the occupant as `none`: {:?}",
        log.borrow()
    );
}

/// Closing a dialog restores focus to the surface that launched it. A dialog opened
/// from the scope field returns there; root-launched dialogs are covered separately.
#[gpui::test]
fn a_dialog_opened_from_the_field_returns_focus_to_it_when_closed(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/");
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("ctrl-,");
    assert!(shell.read_with(&vcx, |shell, _| shell.modal_open()));
    assert!(
        !filter_is_focused(&shell, &mut vcx),
        "the open dialog owns the keyboard, not the field"
    );
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |shell, _| !shell.modal_open()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "escape must return focus to the field the dialog was opened from"
    );
}

/// The palette is an overlay like any dialog: opened from the field,
/// escape returns to the field. Opened from the root it still returns to
/// the root (`palette.rs`'s escape test pins that half).
#[gpui::test]
fn the_palette_opened_from_the_field_returns_focus_to_it_on_escape(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/");
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("ctrl-k");
    assert!(shell.read_with(&vcx, |shell, _| shell.palette.is_some()));
    assert!(!filter_is_focused(&shell, &mut vcx));
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |shell, _| shell.palette.is_none()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "escape must return focus to the field the palette was opened from"
    );
}

/// Field → palette → a dialog the palette opens → escape lands back in
/// the field: the palette closes before it dispatches, so the dialog's
/// door sees the field focused again and records it for itself.
#[gpui::test]
fn a_dialog_opened_through_the_palette_from_the_field_returns_focus_to_the_field(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.update(|window, _cx| window.activate_window());
    vcx.run_until_parked();
    vcx.simulate_keystrokes("alt-/");
    assert!(filter_is_focused(&shell, &mut vcx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("ctrl-k");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_input("Open settings");
    vcx.simulate_keystrokes("enter");
    assert!(shell.read_with(&vcx, |shell, _| shell.palette.is_none()));
    assert!(
        shell.read_with(&vcx, |shell, _| shell.modal_open()),
        "the palette must have opened the settings dialog"
    );
    assert!(!filter_is_focused(&shell, &mut vcx));
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |shell, _| !shell.modal_open()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "escape must return focus to the field the whole chain started from"
    );
}

/// The selection chip contains its close glyph within the same frame. Clicking the
/// glyph removes the dimension without also opening the picker through the chip body's
/// handler.
#[gpui::test]
fn the_close_glyph_lives_inside_its_chip_and_drops_without_opening_the_picker(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(book_scope("BK000"));
        cx.notify();
    });
    vcx.run_until_parked();

    let chip = vcx.debug_bounds("scope-chip-book").expect("chip painted");
    let close = vcx
        .debug_bounds("scope-chip-close-book")
        .expect("close glyph painted");
    // Edge-inclusive on purpose (`Bounds::contains` is far-edge
    // exclusive): a flush-edge × would still be inside its chip.
    assert!(
        close.left() >= chip.left()
            && close.top() >= chip.top()
            && close.right() <= chip.right()
            && close.bottom() <= chip.bottom(),
        "the × sits inside its chip: chip {chip:?}, × {close:?}"
    );

    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(frame.read_with(&vcx, |f, _| f.shared().scope().dimensions.is_empty()));
    assert!(
        shell.read_with(&vcx, |s, _| s.picker.is_none()),
        "dropping a chip must not also open the picker its body opens"
    );
}

/// The bar is segments with a hairline between neighbours: the grouping
/// readout, then the scope (chips and verbs). The divider between them
/// lies strictly between the readout's right edge and the first scope
/// element's left edge; the as-of divider is not painted while live.
#[gpui::test]
fn the_bar_divides_grouping_from_scope_with_a_hairline(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let _shell = shell_of(&window, &mut vcx);

    let readout = vcx.debug_bounds("scope-grouping").expect("readout painted");
    let pick = vcx
        .debug_bounds("scope-pick-chip")
        .expect("pick verb painted");
    let divider = vcx
        .debug_bounds("scope-divider-scope")
        .expect("the grouping/scope divider is painted");
    assert!(
        readout.right() <= divider.left() && divider.right() <= pick.left(),
        "readout {readout:?} | divider {divider:?} | + {pick:?}"
    );
    assert!(
        vcx.debug_bounds("scope-divider-asof").is_none(),
        "no as-of segment while live"
    );
}

/// The unfocused scope field mirrors the frame's text. Its clear glyph drops the text
/// layer through the same subscription used for typing.
#[gpui::test]
fn the_text_layer_lives_in_the_field_and_its_clear_glyph_drops_it(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        let mut s = book_scope("BK000");
        s.text = Some("spx".into());
        f.shared_mut().set_scope(s);
        cx.notify();
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("scope-text-chip").is_none(),
        "no text chip"
    );
    let value = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).value().to_string());
    assert_eq!(value, "spx", "the unfocused field shows the frame's text");

    // The component's clear button sits at the trailing edge of the
    // field, inside its padding; click just inside that edge.
    let field = vcx.debug_bounds("scope-field").expect("field painted");
    let at = gpui::point(field.right() - gpui::px(14.), field.center().y);
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()),
        None,
        "the clear glyph drops the text layer"
    );
    let value = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).value().to_string());
    assert_eq!(value, "");
    assert!(
        frame.read_with(&vcx, |f, _| !f.shared().scope().dimensions.is_empty()),
        "only the text layer went"
    );
}

/// Press at `at` and drag a few pixels with the left button held, the
/// gesture `TitleBar` turns into a window move: its bubble-phase
/// mouse-down arms a move and its next mouse move calls
/// `start_window_move`, which the test platform leaves `unimplemented!`
/// — so a press the title bar still sees panics here.
fn press_and_drag(vcx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    vcx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(
        at + gpui::point(gpui::px(6.), gpui::px(0.)),
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::default(),
    );
    vcx.simulate_mouse_up(
        at + gpui::point(gpui::px(6.), gpui::px(0.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
}

/// A point on bare title-bar space: left of the readout's first control
/// (the pin glyph), level with the scope field.
fn bare_title_bar(vcx: &mut gpui::VisualTestContext) -> gpui::Point<gpui::Pixels> {
    let field = vcx.debug_bounds("scope-field").expect("field painted");
    let first = vcx.debug_bounds("scope-pin").expect("pin glyph painted");
    gpui::point(first.left() - gpui::px(24.), field.center().y)
}

/// A drag that starts on a title-bar control belongs to the control —
/// text selection in the field, nothing on a chip or verb — never to the
/// window: each control occludes the title bar's drag surface. Nor may
/// the press leave the title bar's move armed: a control whose press
/// opens an occluding popup hides the drag and the release from the
/// title bar, and an armed move then fires on the next plain hover over
/// bare title-bar space.
#[gpui::test]
fn dragging_from_a_toolbar_control_does_not_move_the_window(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.shared_mut().set_scope(book_scope("BK000"));
        cx.notify();
    });
    vcx.run_until_parked();

    for selector in [
        "scope-field",
        "scope-chip-book",
        "scope-pick-chip",
        "scope-grouping",
        "scope-pin",
    ] {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} painted"));
        press_and_drag(&mut vcx, bounds.center());
        // Close whatever the press opened before the next control.
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        let bare = bare_title_bar(&mut vcx);
        vcx.simulate_mouse_move(bare, None, gpui::Modifiers::default());
        vcx.run_until_parked();
    }
}

/// The probe above is live: the same drag from bare title-bar space
/// does reach `start_window_move`.
#[gpui::test]
#[should_panic(expected = "not implemented")]
fn dragging_from_bare_title_bar_space_moves_the_window(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx) = open_shell(cx, test_services());
    vcx.run_until_parked();
    let bare = bare_title_bar(&mut vcx);
    press_and_drag(&mut vcx, bare);
}

/// A saved scope `mine` naming `named`, beside an `expressions` doc that
/// defines `liq` alone.
fn services_with_a_named_scope(named: &str) -> ShellServices {
    services_with_builtin_docs(vec![
        LayerDoc::builtin("scopes", &format!("[mine]\nnamed = [\"{named}\"]\n")).unwrap(),
        LayerDoc::builtin("expressions", "[liq]\nexpression = \"npv > 0\"\n").unwrap(),
    ])
}

fn click_selector(vcx: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} paints"));
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
}

/// Loading a saved scope that names an expression paints its `≡ liq`
/// chip, and the scope is savable on that name alone (the save glyph
/// paints). The narrowing is on screen, not only in the totals.
#[gpui::test]
fn a_loaded_scope_paints_a_chip_for_its_named_expression(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_a_named_scope("liq"));
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "scope::mine", &mut vcx);
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-named-chip-liq").is_some(),
        "the ≡ liq chip paints"
    );
    assert!(
        vcx.debug_bounds("scope-named-chip-broken-liq").is_none(),
        "a defined name is not painted broken"
    );
    assert!(
        vcx.debug_bounds("scope-save-chip").is_some(),
        "a scope of one name is savable"
    );
}

/// A click on a named chip's `×` drops that name through `set_scope`:
/// `frame::scope_undo` brings it back.
#[gpui::test]
fn a_named_chips_close_glyph_drops_the_name_undoably(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_a_named_scope("liq"));
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    dispatch_action(&shell, "scope::mine", &mut vcx);
    vcx.run_until_parked();
    let chip = vcx.debug_bounds("scope-named-chip-liq").expect("chip");
    let close = vcx
        .debug_bounds("scope-named-chip-close-liq")
        .expect("× paints");
    assert!(
        close.left() >= chip.left() && close.right() <= chip.right(),
        "the × sits inside its chip: chip {chip:?}, × {close:?}"
    );
    click_selector(&mut vcx, "scope-named-chip-close-liq");
    assert!(frame.read_with(&vcx, |f, _| f.shared().scope().named.is_empty()));
    assert!(vcx.debug_bounds("scope-named-chip-liq").is_none());
    dispatch_action(&shell, "frame::scope_undo", &mut vcx);
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.shared().scope().named.clone()),
        vec!["liq".to_string()],
        "the drop went through set_scope"
    );
}

/// A scope naming an expression that no longer exists paints the
/// danger-toned `≡ gone · missing` chip, so the trader can see why the
/// tile refuses and remove the name.
#[gpui::test]
fn a_missing_name_paints_the_broken_chip(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_a_named_scope("gone"));
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "scope::mine", &mut vcx);
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-named-chip-broken-gone").is_some(),
        "the missing name paints in the danger tone"
    );
    assert!(vcx.debug_bounds("scope-named-chip-close-gone").is_some());
}

/// The object dialog's state, read through the shell.
fn object_dialog_state<T>(
    shell: &Entity<ShellView>,
    vcx: &gpui::VisualTestContext,
    f: impl FnOnce(&objectdialog::ObjectDialogState) -> T,
) -> T {
    shell.read_with(vcx, |s, _| {
        f(s.object_dialog
            .as_ref()
            .expect("the object dialog should be open"))
    })
}

/// A click on a named chip's body opens the Expressions dialog on that
/// object, in its edit stage; the dialog then owns the keyboard, so
/// `escape` steps back to its browse list.
#[gpui::test]
fn a_named_chips_body_opens_its_expression(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_a_named_scope("liq"));
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "scope::mine", &mut vcx);
    vcx.run_until_parked();
    click_selector(&mut vcx, "scope-named-chip-liq");
    let (domain, stage) = object_dialog_state(&shell, &vcx, |s| (s.domain, s.stage.clone()));
    assert_eq!(domain, objectdialog::Domain::Expressions);
    assert_eq!(
        stage,
        objectdialog::Stage::Edit {
            object: "liq".to_string()
        }
    );
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(
        object_dialog_state(&shell, &vcx, |s| s.stage.clone()),
        objectdialog::Stage::Browse,
        "the keyboard reaches the dialog the click opened"
    );
}

/// A missing name has no object to edit: the click opens the browse list
/// with a notice rather than an edit stage over a phantom empty draft.
#[gpui::test]
fn a_missing_named_chips_body_opens_browse_with_a_notice(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_a_named_scope("gone"));
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "scope::mine", &mut vcx);
    vcx.run_until_parked();
    click_selector(&mut vcx, "scope-named-chip-gone");
    let (domain, stage, notice, has_draft) = object_dialog_state(&shell, &vcx, |s| {
        (
            s.domain,
            s.stage.clone(),
            s.notice.clone(),
            s.draft.is_some(),
        )
    });
    assert_eq!(domain, objectdialog::Domain::Expressions);
    assert_eq!(stage, objectdialog::Stage::Browse);
    assert_eq!(notice.as_deref(), Some("'gone' is not defined"));
    assert!(!has_draft, "no edit draft was built");
}

/// An invalid name is still defined: its chip opens the edit stage, which
/// is where the broken text gets fixed.
#[gpui::test]
fn an_invalid_named_chips_body_opens_its_edit_stage(cx: &mut gpui::TestAppContext) {
    let services = services_with_builtin_docs(vec![
        LayerDoc::builtin("scopes", "[mine]\nnamed = [\"bad\"]\n").unwrap(),
        LayerDoc::builtin("expressions", "[bad]\nexpression = \"npv >\"\n").unwrap(),
    ]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    dispatch_action(&shell, "scope::mine", &mut vcx);
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("scope-named-chip-broken-bad").is_some(),
        "the fixture's name is broken"
    );
    click_selector(&mut vcx, "scope-named-chip-bad");
    assert_eq!(
        object_dialog_state(&shell, &vcx, |s| s.stage.clone()),
        objectdialog::Stage::Edit {
            object: "bad".to_string()
        }
    );
}

/// The `×` occludes the body: clicking it drops the name and opens
/// nothing.
#[gpui::test]
fn a_named_chips_close_glyph_opens_nothing(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_a_named_scope("liq"));
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    dispatch_action(&shell, "scope::mine", &mut vcx);
    vcx.run_until_parked();
    click_selector(&mut vcx, "scope-named-chip-close-liq");
    assert!(frame.read_with(&vcx, |f, _| f.shared().scope().named.is_empty()));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open() && s.object_dialog.is_none()));
}
