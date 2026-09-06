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
/// directly. `config`'s `[keymap] mod = "ctrl"` text is unused by this
/// helper (kept only as documentation of intent) — it is never run
/// through `mod_alias_from_config`.
fn test_services_with_ctrl_mod() -> ShellServices {
    let config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[keymap]\nmod = \"ctrl\"\n").unwrap()],
        ..ConfigSources::default()
    });
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
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path: None,
        roster: crate::module::ModuleRoster::default(),
        restored_tiles: crate::session::TileRecords::new(),
        restored_frame: None,
    }
}

#[gpui::test]
fn ctrl_z_and_ctrl_shift_z_undo_and_redo_the_scope(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services_with_ctrl_mod());
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
    // §3.1: Alt) — `test_services_with_ctrl_mod` above exists precisely
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

    // The session coalesced its one entry (the pre-focus "old" scope,
    // recorded on the session's first divergence, when typing started)
    // rather than pushing a second one for the abandoned "new" — so the
    // first undo pops that session entry, landing back on "old" (a
    // value no-op: the scope was already "old" after Escape) rather
    // than resurrecting "new".
    assert!(frame.update(&mut vcx, |f, _| f.undo_scope()));
    assert_eq!(
        frame
            .read_with(&vcx, |f, _| f.scope().text.clone())
            .as_deref(),
        Some("old"),
        "the first undo must land back on the pre-focus scope, never on the abandoned \"new\""
    );

    // A second undo walks past the session entirely, to the scope from
    // before "old" was ever set.
    assert!(frame.update(&mut vcx, |f, _| f.undo_scope()));
    assert_eq!(frame.read_with(&vcx, |f, _| f.scope().text.clone()), None);

    // Nothing further to undo — "new" never appears anywhere in the
    // history walked above.
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
