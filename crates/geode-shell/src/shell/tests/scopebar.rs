//! The scope bar: undo/redo chords (Task 3). Task 4 extends this file
//! with the painted bar itself.

use super::*;
use crate::defaults::mod_alias_from_config;
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

/// `mod+z`/`mod+shift+z` (spec §3.6) under `keymap.mod = "ctrl"`, so the
/// real chords dispatched here are `ctrl-z`/`ctrl-shift-z` — same pattern
/// `config_with_mod` uses elsewhere, built inline since this is the
/// starting config rather than a reload.
fn test_services_with_ctrl_mod() -> ShellServices {
    let config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[keymap]\nmod = \"ctrl\"\n").unwrap()],
        ..ConfigSources::default()
    });
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    let mod_alias = mod_alias_from_config(&config);
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
