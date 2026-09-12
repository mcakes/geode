//! The scope bar: undo/redo chords (Task 3). Task 4 extends this file
//! with the painted bar itself.

use super::*;
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

/// `mod+z`/`mod+shift+z` (spec §3.6) dispatched here as literal
/// `ctrl-z`/`ctrl-shift-z` — a non-default alias, proving the bindings
/// aren't hardcoded to `alt`. Built directly with `Modifiers::CTRL`
/// rather than through `mod_alias_from_config`: config no longer offers
/// this alias at all (Task 4b, Phase 4a user ruling — `keymap.mod =
/// "ctrl"` is refused as invalid config), so this helper feeds the raw
/// `Modifiers` value straight to `build_keymap`, the same way
/// `defaults.rs`'s own `resolve` test helper exercises the matcher
/// directly. The config carries no `[keymap] mod` doc at all (an earlier
/// version of this helper loaded one, redundantly, since it was never
/// run through `mod_alias_from_config`) — an empty config proves the
/// point just as well.
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
            f.set_scope(a.clone());
            f.set_scope(b.clone());
        });
    });

    cx.simulate_keystrokes("ctrl-z");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame().read(cx).scope().clone()),
        a,
        "ctrl-z must restore the scope before the last set_scope"
    );

    cx.simulate_keystrokes("ctrl-shift-z");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame().read(cx).scope().clone()),
        b,
        "ctrl-shift-z must return to the scope ctrl-z just undid"
    );
}

#[gpui::test]
fn typing_in_the_field_sets_the_frame_text_per_keystroke_and_enter_blurs(
    cx: &mut gpui::TestAppContext,
) {
    // `test_services()` builds its keymap with `default_mod()` (spec
    // §3.1: Alt) — `test_services_with_ctrl_alias` above exists precisely
    // because that default is Alt, not Ctrl, so `mod+/` resolves to
    // `alt+/` here.
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
            .read_with(&vcx, |f, _| f.scope().text.clone())
            .as_deref(),
        Some("sp")
    );
    let v_after_two = frame.read_with(&vcx, |f, _| f.versions().scope);
    vcx.simulate_input("x");
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.versions().scope),
        v_after_two + 1
    );
    vcx.simulate_keystrokes("enter");
    assert!(!filter_is_focused(&shell, &mut vcx));
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.scope().text.clone())
            .as_deref(),
        Some("spx")
    );
    // One undo entry for the whole session.
    frame.update(&mut vcx, |f, _| assert!(f.undo_scope()));
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()), None);
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
        if f.set_text(Some("old".into())) {
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
            .read_with(&vcx, |f, _| f.scope().text.clone())
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
    // Pre-focus state: text = "old", pushed onto undo by the ordinary
    // (non-session) `set_text` path. A first cut of the escape fix (fix
    // round 1, Finding 1) let a *second*, spurious entry bury this one:
    // it called `end_scope_session` first and reverted through the
    // ordinary `set_text`, which runs outside the session and so pushed
    // the just-typed, now-abandoned "new" scope too — leaving the
    // session's own coalesced entry (this "old" scope) buried
    // underneath it. A single `undo_scope` then resurrected "new".
    frame.update(&mut vcx, |f, cx| {
        if f.set_text(Some("old".into())) {
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
            .read_with(&vcx, |f, _| f.scope().text.clone())
            .as_deref(),
        Some("old"),
        "escape must restore the pre-focus text"
    );

    // Phase 4b M2: the session coalesced its one entry (the pre-focus
    // "old" scope, recorded on the session's first divergence, when
    // typing started) rather than pushing a second one for the abandoned
    // "new" — and since the session ends exactly where it began (Escape
    // reverted all the way back to "old"), `end_scope_session` pops that
    // now-pointless entry back off. So a single `undo_scope` walks
    // straight past the whole focus/type/escape episode to the scope
    // from before "old" was ever set — "new" never appears anywhere in
    // the history, and neither does a phantom step that would have
    // landed back on "old" (a value no-op) for nothing.
    assert!(frame.update(&mut vcx, |f, _| f.undo_scope()));
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()), None);

    // Nothing further to undo.
    assert!(!frame.update(&mut vcx, |f, _| f.undo_scope()));
}

#[gpui::test]
fn a_text_set_elsewhere_shows_in_the_field_and_a_chip_close_drops_the_dimension(
    cx: &mut gpui::TestAppContext,
) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        let mut s = f.scope().clone();
        s.text = Some("from-tile".into());
        s.dimensions.push(geode_core::scope::DimensionSelection {
            column: "book".into(),
            values: vec!["BK001".into()],
        });
        f.set_scope(s);
        cx.notify();
    });
    vcx.run_until_parked();
    let value = shell.read_with(&vcx, |s, cx| s.filter_input.read(cx).value().to_string());
    assert_eq!(value, "from-tile");
    let close = vcx
        .debug_bounds("scope-chip-close-book")
        .expect("chip painted");
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    assert!(frame.read_with(&vcx, |f, _| f.scope().dimensions.is_empty()));
    assert!(vcx.debug_bounds("scope-chip-close-book").is_none());
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
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("scopes", SCOPES_DOC).unwrap()],
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
    }
}

/// F4 (final fix wave, whole-branch review): spec §3.11's `scope::<name>`
/// action per saved scope, built the same way `frame::pick_<column>` is
/// (`defaults::register_scope_actions`, dispatched in `input.rs` by
/// stripping the `scope::` prefix).
#[gpui::test]
fn dispatching_scope_name_loads_the_saved_scope(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_saved_scope());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()), None);

    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(&ActionId("scope::eu".into()), None, window, cx);
        });
    });

    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.scope().text.clone())
            .as_deref(),
        Some("eu"),
        "dispatching scope::eu must load the saved scope"
    );
}

/// A chord typed into the focused text field still dispatches its shell
/// binding (user ruling 2026-09-12: "when focused on a text field, key
/// bindings with modifier keys should still work — `ctrl+k`, `ctrl+,`").
/// A shift-only keystroke is typing, never a chord: `shift+d` is `D` in
/// the field, not `workspace::duplicate_horizontal`.
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
        shell.read_with(&vcx, |shell, _| shell.modal.is_some()),
        "ctrl+, from the focused field must open the settings dialog"
    );
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |shell, _| shell.modal.is_none()));

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
            .read_with(&vcx, |f, _| f.scope().text.clone())
            .as_deref(),
        Some("sp")
    );
    vcx.simulate_keystrokes("alt-z"); // mod+z under the default mod
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()), None);
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

/// A dialog launched from the focused text field hands focus back to
/// the field when it closes (user ruling 2026-09-12: "if I'm focused on
/// the text field, launch a dialog, and dismiss it with Esc, I'd expect
/// focus to return to the text field"). A dialog launched from the shell
/// root still returns to the root (`settings_open_opens_the_modal` and
/// friends pin that half).
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
    assert!(shell.read_with(&vcx, |shell, _| shell.modal.is_some()));
    assert!(
        !filter_is_focused(&shell, &mut vcx),
        "the open dialog owns the keyboard, not the field"
    );
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |shell, _| shell.modal.is_none()));
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
        shell.read_with(&vcx, |shell, _| shell.modal.is_some()),
        "the palette must have opened the settings dialog"
    );
    assert!(!filter_is_focused(&shell, &mut vcx));
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |shell, _| shell.modal.is_none()));
    assert!(
        filter_is_focused(&shell, &mut vcx),
        "escape must return focus to the field the whole chain started from"
    );
}
