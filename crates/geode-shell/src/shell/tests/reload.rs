//! Config hot reload applied through `apply_reload` directly (the real
//! entity, the real method the watcher calls) plus the frame-slot keys.

use super::*;
// Explicit: this file's own `mod reload;` declaration in `tests/mod.rs`
// shadows the glob-imported `crate::reload`, so the bare `reload::` paths
// below need this to resolve to the real module rather than to `self`.
use crate::reload;

/// A clean reload (no error diagnostics) is applied: the mod alias
/// (and therefore the keymap built from it) updates to match the new
/// config, an open palette closes (brief: "must close on a successful
/// reload"), and the outcome is recorded as `Applied`.
#[gpui::test]
fn apply_reload_with_a_clean_config_applies_it_and_closes_the_palette(
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

    // Open the palette so we can prove a successful reload closes it.
    cx.simulate_keystrokes("ctrl-k");
    assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));

    // "cmd" (not "ctrl" — Task 4b, Phase 4a user ruling: `keymap.mod =
    // "ctrl"` is refused as invalid config) is the non-default alias
    // exercised here.
    let new_config = config_with_mod("cmd");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.mod_alias,
            Modifiers::CMD,
            "a clean reload should rebuild the mod alias from the new config"
        );
        assert!(
            shell.palette.is_none(),
            "a successful reload must close an open palette"
        );
        assert_eq!(
            shell.last_reload,
            reload::ReloadOutcome::Applied { warnings: vec![] },
            "a clean reload with no diagnostics should record Applied with no warnings"
        );
    });
}

/// Phase 4b Task 5 fix round 1, MAJ-3: `versions.config` must bump on
/// every *applied* reload, not only one that changes `views`/
/// `dimensions` — `note_config_reloaded`'s call used to sit behind the
/// same `views_changed` gate as `ShellEvent::ConfigReloaded` (which is
/// correctly scoped to what the data thread needs), so anything gating
/// on "config was just reloaded" — chiefly the diagnostics module's
/// config-section explainer — went stale on the reload `:level`'s own
/// persist write causes (an `[log]`-only `app.toml` change).
/// `config_with_mod` only touches `app.toml`'s `[keymap]` table:
/// `views`/`dimensions` are both absent, so `views_changed` is false for
/// this reload, and before the fix `versions.config` would not have
/// moved at all.
#[gpui::test]
fn a_reload_that_does_not_touch_views_or_dimensions_still_bumps_the_config_version(
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
        root.view().clone().downcast::<ShellView>().unwrap()
    });

    let v0 = shell.read_with(&cx, |s, cx| s.frame.read(cx).versions());
    let new_config = config_with_mod("cmd");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));
    let v1 = shell.read_with(&cx, |s, cx| s.frame.read(cx).versions());
    assert!(
        v1.config > v0.config,
        "views/dimensions untouched, but the config version must still bump \
         (v0={v0:?}, v1={v1:?})"
    );
}

/// An error-severity diagnostic in the new config (here: an
/// unsupported `config_version`) means the entire previous `Config`
/// (and everything built from it — mod alias, keymap) is kept
/// untouched, and the outcome records the error for the status bar.
/// A palette open at the time stays open — only a *successful* reload
/// closes it.
#[gpui::test]
fn apply_reload_with_an_error_diagnostic_keeps_last_good_config(cx: &mut gpui::TestAppContext) {
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

    let original_mod_alias = shell.read_with(&cx, |shell, _| shell.services.mod_alias);

    cx.simulate_keystrokes("ctrl-k");
    assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));

    // A desk-layer doc with an unsupported config_version is an error
    // diagnostic on `Config::load` itself (geode_core::config::load_layer).
    let desk = tempfile::tempdir().unwrap();
    std::fs::write(desk.path().join("app.toml"), "config_version = 99\n").unwrap();
    let bad_config = Config::load(&ConfigSources {
        builtin: vec![],
        desk: Some(desk.path().to_path_buf()),
        user: None,
    });
    assert!(
        bad_config
            .diagnostics
            .iter()
            .any(|d| d.severity == geode_core::config::Severity::Error),
        "sanity: the constructed config must actually carry an error diagnostic"
    );

    shell.update(&mut cx, |shell, cx| shell.apply_reload(bad_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.mod_alias, original_mod_alias,
            "an error diagnostic must keep the previous mod alias/keymap untouched"
        );
        assert!(
            shell.palette.is_some(),
            "a rejected reload must not close the palette"
        );
        match &shell.last_reload {
            reload::ReloadOutcome::KeptLastGood { errors } => {
                assert_eq!(errors.len(), 1);
                assert!(errors[0].contains("config_version"));
            }
            other => panic!("expected KeptLastGood, got {other:?}"),
        }
    });
}

/// Task 4b (Phase 4a, user ruling): `keymap.mod = "ctrl"` is refused as
/// invalid config, exactly like the unsupported-`config_version` case
/// above (`reload::decide` folds the mod-alias error diagnostic into
/// `new_config.diagnostics` the same as any other error, so "any error
/// diagnostic ⇒ keep last-good entire Config" applies unchanged) — a
/// reload carrying it keeps the previous mod alias untouched, surfaces
/// the error in the status bar's reload message, and a later reload back
/// to a real alias clears it.
#[gpui::test]
fn apply_reload_with_keymap_mod_ctrl_keeps_last_good_and_a_later_reload_clears_it(
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

    let original_mod_alias = shell.read_with(&cx, |shell, _| shell.services.mod_alias);
    assert_eq!(
        original_mod_alias,
        Modifiers::ALT,
        "sanity: test_services() starts on the default (alt) alias"
    );

    let ctrl_config = config_with_mod("ctrl");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(ctrl_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.mod_alias, original_mod_alias,
            "keymap.mod = \"ctrl\" must be refused: the previous alias stands"
        );
        let message = shell
            .last_reload
            .status_message()
            .expect("a refused keymap.mod = \"ctrl\" reload must surface a status message");
        assert!(
            message.contains("1 error"),
            "expected the one mod-alias error to be counted: {message}"
        );
        match &shell.last_reload {
            reload::ReloadOutcome::KeptLastGood { errors } => {
                assert_eq!(errors.len(), 1);
                assert!(errors[0].contains("keymap.mod"), "{}", errors[0]);
                assert!(errors[0].contains("ctrl"), "{}", errors[0]);
            }
            other => panic!("expected KeptLastGood, got {other:?}"),
        }
    });

    // A later reload back to a real alias clears the error.
    let good_config = config_with_mod("alt");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(good_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(shell.services.mod_alias, Modifiers::ALT);
        assert_eq!(
            shell.last_reload.status_message(),
            None,
            "a later clean reload must clear the error status"
        );
    });
}

fn config_with_theme(name: &str, mode: &str) -> Config {
    Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin(
                "app",
                &format!("[theme]\nname = \"{name}\"\nmode = \"{mode}\"\n"),
            )
            .unwrap(),
        ],
        desk: None,
        user: None,
    })
}

/// `apply_reload`'s theme-reapply guard, fired: when the new config's
/// `[theme]` table genuinely differs from the old one's, the reload
/// re-applies the theme, and the live `ThemeService` reflects the new
/// value.
#[gpui::test]
fn apply_reload_reapplies_the_theme_when_theme_table_changed(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let mut services = test_services();
                services.config = config_with_theme("Gruvbox", "dark");
                let view = cx.new(|cx| {
                    // Mirrors what main.rs does before opening the
                    // window: apply the theme the starting config
                    // actually names, so this test's "old" state is a
                    // real (config, active theme) pair, not just a
                    // Default-Light service that happens to hold a
                    // Gruvbox config it never applied.
                    services.theme.apply_from_config(&services.config, cx);
                    ShellView::new(services, None, None, window, cx)
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

    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .theme
            .active_name()
            .to_string()),
        "Gruvbox Dark",
        "sanity: the starting theme must actually be the one the old config names"
    );

    let new_config = config_with_theme("Default", "dark");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.theme.active_name(),
            "Default Dark",
            "a genuinely different [theme] table must be re-applied on reload"
        );
    });
}

/// `apply_reload`'s theme-reapply guard, holding: when the new config's
/// `[theme]` table is identical to the old one's, the reload must NOT
/// re-apply the theme — a runtime `theme::toggle_mode` done between the
/// old config being applied and this reload survives untouched, rather
/// than being silently reverted to what `[theme]` still says.
#[gpui::test]
fn apply_reload_preserves_a_runtime_toggle_when_theme_table_is_unchanged(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let mut services = test_services();
                services.config = config_with_theme("Gruvbox", "dark");
                let view = cx.new(|cx| {
                    services.theme.apply_from_config(&services.config, cx);
                    ShellView::new(services, None, None, window, cx)
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

    // A runtime toggle (mod+shift+t / theme::toggle_mode), independent
    // of config, before any reload happens.
    shell.update(&mut cx, |shell, cx| shell.services.theme.toggle_mode(cx));
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .theme
            .active_name()
            .to_string()),
        "Gruvbox Light",
        "sanity: toggling from Gruvbox Dark should flip to Gruvbox Light"
    );

    // Same [theme] table as the config already applied — a reload
    // triggered by, say, an unrelated keymap.toml edit.
    let new_config = config_with_theme("Gruvbox", "dark");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.theme.active_name(),
            "Gruvbox Light",
            "an unchanged [theme] table must not re-apply the theme, or the \
             runtime toggle above would be silently reverted"
        );
        assert_eq!(
            shell.last_reload,
            reload::ReloadOutcome::Applied { warnings: vec![] },
            "the reload itself still succeeds — only the theme re-apply is guarded"
        );
    });
}

/// Review fix round 1, Finding 2: an open palette must NOT close on a
/// reload whose `[theme]` table is the only thing that changed — the
/// palette's own items (Task 6: built from the registry + keymap
/// bindings, `toggle_palette`) don't depend on `[theme]` at all, so
/// closing it here would just be spurious churn. This is exactly the
/// situation `theme::persist_to_user_config`'s own write triggers (see
/// its doc comment): the app writes its own theme choice to disk, the
/// watcher picks that up as "config changed", and this reload must not
/// silently close a palette the user still has open.
#[gpui::test]
fn apply_reload_leaves_an_open_palette_open_when_only_the_theme_table_differs(
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
    assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));

    // `test_services()`'s starting config has no `[theme]` table at
    // all (and no `[keymap]` table either — `config_with_theme` only
    // ever writes an "app" doc's `[theme]` section, so this new
    // config's `layered_docs("keymap")` is just as empty as the
    // starting one's, and neither sets `[keymap] mod`).
    let new_config = config_with_theme("Gruvbox", "dark");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.theme.active_name(),
            "Gruvbox Dark",
            "sanity: the theme-only change must still have been applied"
        );
        assert!(
            shell.palette.is_some(),
            "a theme-only reload must leave an open palette open"
        );
    });
}

/// The contrasting half of the Finding 2 pair above: a reload whose
/// keymap docs genuinely differ (here, via `[keymap] mod`, which
/// changes the resolved mod alias and therefore the bindings the
/// palette would render) DOES close an open palette — its snapshot
/// really is stale.
#[gpui::test]
fn apply_reload_closes_an_open_palette_when_the_keymap_docs_differ(cx: &mut gpui::TestAppContext) {
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
    assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));

    // "cmd" (not "ctrl" — Task 4b, Phase 4a user ruling: `keymap.mod =
    // "ctrl"` is refused as invalid config) is the non-default alias
    // exercised here.
    let new_config = config_with_mod("cmd");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.mod_alias,
            Modifiers::CMD,
            "sanity: the mod alias must actually have changed"
        );
        assert!(
            shell.palette.is_none(),
            "a reload with genuinely different keymap docs must close an open palette"
        );
    });
}

/// Fix-round regression for the orphaned-`FocusId` finding on
/// `apply_reload`'s palette-close path (see `pending_focus_restore`'s
/// and that call site's own doc comments): a background reload closing
/// the palette while its query `Input` genuinely holds window focus
/// must still end up with focus back on the shell root — `apply_reload`
/// itself has no `Window` to do that with directly, so this proves the
/// `pending_focus_restore` flag actually gets consumed by the very next
/// render, the same "assert the shell handle is focused after" pattern
/// `escape_closes_the_palette_without_dispatching` uses for the
/// ordinary key-driven close.
#[gpui::test]
fn apply_reload_closing_a_focused_palette_restores_focus_to_the_shell_root(
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
        "sanity: ctrl+k opening the palette should have focused its query Input"
    );

    // A keymap-differing reload (not just a theme-only one — see the
    // contrasting pair of tests above) closes the palette out from
    // under that still-focused input, with no Window available to
    // `apply_reload` itself to redirect focus. "cmd" (not "ctrl" —
    // Task 4b, Phase 4a user ruling: `keymap.mod = "ctrl"` is refused as
    // invalid config) is the non-default alias exercised here.
    let new_config = config_with_mod("cmd");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    assert!(
        shell.read_with(&cx, |shell, _| shell.palette.is_none()),
        "sanity: the keymap-differing reload should have closed the palette"
    );

    // `apply_reload` already calls `cx.notify()` unconditionally, so
    // the next draw is exactly the render that should consume
    // `pending_focus_restore`.
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        !cx.update(|window, _cx| palette_input_focus_handle.is_focused(window)),
        "the closed palette's query input must not still hold window focus"
    );
    assert!(
        cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
        "a background reload closing a focused palette must still return \
         focus to the shell root — otherwise handle_key_down's on_key_down \
         listener never fires again until a mouse click claims focus"
    );
}

// --- Task 6: frame keys, the readout, and config reload -------------

#[gpui::test]
fn ctrl_digits_switch_the_frame_slot_and_ctrl_0_clears_it(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    // Two slots through config, the way `new` reads them.
    let groupings = LayerDoc::builtin("groupings", "1 = [\"book\"]\n2 = [\"lhu\"]\n").unwrap();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
    )
    .unwrap();
    services.config = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            groupings,
            datasets,
        ],
        ..ConfigSources::default()
    });
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("ctrl-2");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()),
        Some(2)
    );
    let v = shell.read_with(&cx, |s, cx| s.frame.read(cx).versions());
    cx.simulate_keystrokes("ctrl-5");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()),
        Some(2),
        "an empty slot is ignored"
    );
    assert_eq!(shell.read_with(&cx, |s, cx| s.frame.read(cx).versions()), v);
    cx.simulate_keystrokes("ctrl-0");
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()),
        None
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("frame-readout").is_some(),
        "the readout painted"
    );
}

#[gpui::test]
fn a_reloaded_groupings_doc_replaces_the_slots_and_a_sources_change_asks_for_a_restart(
    cx: &mut gpui::TestAppContext,
) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&shell, move |_, event: &ShellEvent, _| {
            sink.borrow_mut().push(event.clone())
        })
        .detach();
    });

    let mut new_config = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("groupings", "3 = [\"book\"]\n").unwrap(),
            LayerDoc::builtin("datasets", "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n").unwrap(),
            LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n").unwrap(),
        ],
        ..ConfigSources::default()
    });
    let v0 = shell.read_with(&cx, |s, cx| s.frame.read(cx).versions());
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut new_config), cx)
    });
    let (slots, versions) = shell.read_with(&cx, |s, cx| {
        (
            s.frame.read(cx).slots().clone(),
            s.frame.read(cx).versions(),
        )
    });
    assert_eq!(slots.label(3).as_deref(), Some("book"));
    assert!(versions.config > v0.config);
    assert!(
        events.borrow().contains(&ShellEvent::ConfigReloaded),
        "{:?}",
        events.borrow()
    );

    // Now a sources change.
    let mut with_sources = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin(
                "sources",
                "[s]\ndataset = \"risk\"\npaths = [\"/x/*.csv\"]\n",
            )
            .unwrap(),
        ],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut with_sources), cx)
    });
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e, ShellEvent::RestartRequired(m) if m.contains("sources"))),
        "{:?}",
        events.borrow()
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("restart-required").is_some(),
        "the status bar says so"
    );
}

/// M8 (3b final review): `restart_required` compares each reload's
/// `sources` doc against the baseline the running `DataService` was
/// actually built from, not against the previous reload — so reverting
/// the offending edit back to that baseline clears the stale message
/// rather than leaving it up for the rest of the session.
#[gpui::test]
fn reverting_a_sources_edit_back_to_the_baseline_clears_restart_required(
    cx: &mut gpui::TestAppContext,
) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    let mut with_sources = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin(
                "sources",
                "[s]\ndataset = \"risk\"\npaths = [\"/x/*.csv\"]\n",
            )
            .unwrap(),
        ],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut with_sources), cx)
    });
    assert!(
        shell.read_with(&cx, |s, _| s.restart_required.is_some()),
        "a sources doc appearing where the baseline (from `test_services`, \
         which loads `ConfigSources::default()`) had none asks for a restart"
    );

    // Revert: reload with a config whose `sources` doc is absent again,
    // matching the empty baseline the shell (and the data engine) started
    // with.
    let mut reverted = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut reverted), cx)
    });
    assert!(
        shell.read_with(&cx, |s, _| s.restart_required.is_none()),
        "reverting sources.toml back to the baseline clears the message"
    );
}

/// I2 (final review): a `views`-changing reload must queue
/// `ShellEvent::ConfigReloaded` *before* it notifies the frame. gpui
/// flushes effects FIFO, so which of the two runs first for any given
/// subscriber/observer pair is decided purely by which effect was
/// *queued* first, not by subscription order — a frame observer (a real
/// tile's `on_frame_changed`, which requeries when its followed versions
/// moved) queued ahead of the event's own subscribers (the app bridge,
/// which forwards `ConfigReloaded` as `ReplaceViews`) would otherwise
/// run its requery against the still-old views while already recording
/// the new frame version, leaving nothing to trigger the requery it
/// actually needed. This records the firing order directly rather than
/// the requery behaviour itself (which needs a real tile — a module
/// `geode-shell` cannot depend on) and RED-then-GREENs the statement
/// order in `apply_reload`'s `views_changed` branch.
#[gpui::test]
fn a_views_change_emits_config_reloaded_before_the_frame_notifies_its_observers(
    cx: &mut gpui::TestAppContext,
) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let frame = shell.read_with(&cx, |s, _| s.frame.clone());

    let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let o1 = order.clone();
    cx.update(|_, cx| {
        cx.subscribe(&shell, move |_, event: &ShellEvent, _| {
            if matches!(event, ShellEvent::ConfigReloaded) {
                o1.borrow_mut().push("event");
            }
        })
        .detach();
    });
    let o2 = order.clone();
    cx.update(|_, cx| {
        cx.observe(&frame, move |_frame, _cx| {
            o2.borrow_mut().push("frame");
        })
        .detach();
    });

    let mut new_config = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
            )
            .unwrap(),
            LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n").unwrap(),
        ],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut new_config), cx)
    });

    assert_eq!(
        order.borrow().as_slice(),
        &["event", "frame"],
        "ConfigReloaded must be queued (and therefore fire) before the \
         frame's own change notification"
    );
}

/// I2 residual (re-review after 7814fdc): the test above only exercises
/// the `views_changed` branch's own two statements — its
/// `groupings_changed` block never actually notifies, since no
/// `groupings`/`dimensions` doc is present there and `rebuild_slots`
/// reproduces the same empty `GroupingSlots`, so `replace_slots` returns
/// `false`. But `groupings_changed` and `views_changed` both key off a
/// changed `dimensions` doc (`apply_reload`'s `changed(..)` closure), and
/// `GroupingSlots::from_doc` genuinely depends on `dimensions` — a slot
/// naming a derived-dimension column resolves once that dimension exists
/// — so one reload can make BOTH branches touch the frame. This builds
/// that case: slot 1 names dimension `desk`, unresolved (and so dropped,
/// an "unknown column" diagnostic) at construction, and resolved once the
/// reload's `dimensions.toml` defines it — `replace_slots` genuinely
/// returns `true` here, which the previous fix (hoisting the emit only
/// above `views_changed`'s own `frame.update`) left free to queue the
/// frame's `Effect::Notify` first, since `groupings_changed`'s block runs
/// earlier in source order.
#[gpui::test]
fn a_dimensions_change_that_resolves_a_grouping_slot_still_emits_config_reloaded_before_the_frame_notifies(
    cx: &mut gpui::TestAppContext,
) {
    let (mut services, _log) = services_with_recorder();
    services.config = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("groupings", "1 = [\"desk\"]\n").unwrap(),
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
            )
            .unwrap(),
        ],
        ..ConfigSources::default()
    });
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let frame = shell.read_with(&cx, |s, _| s.frame.clone());

    // Slot 1 names a dimension that doesn't exist yet — dropped at
    // construction by `GroupingSlots::from_doc`'s "unknown column" branch.
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame.read(cx).slots().label(1)),
        None,
        "slot 1 starts unresolved: no `dimensions` doc defines `desk` yet"
    );

    let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let o1 = order.clone();
    cx.update(|_, cx| {
        cx.subscribe(&shell, move |_, event: &ShellEvent, _| {
            if matches!(event, ShellEvent::ConfigReloaded) {
                o1.borrow_mut().push("event");
            }
        })
        .detach();
    });
    let o2 = order.clone();
    cx.update(|_, cx| {
        cx.observe(&frame, move |_frame, _cx| {
            o2.borrow_mut().push("frame");
        })
        .detach();
    });

    let mut new_config = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("groupings", "1 = [\"desk\"]\n").unwrap(),
            LayerDoc::builtin(
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
            )
            .unwrap(),
            LayerDoc::builtin(
                "dimensions",
                "[desk]\nfrom = \"book\"\n[desk.values]\nEU = [\"BK000\"]\n",
            )
            .unwrap(),
        ],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut new_config), cx)
    });

    assert_eq!(
        shell.read_with(&cx, |s, cx| s.frame.read(cx).slots().label(1)),
        Some("desk".to_string()),
        "slot 1 must resolve once `dimensions.toml` defines `desk` — proves \
         `replace_slots` actually returned `true` here (the \
         groupings_changed branch queuing its own frame notify), not just \
         views_changed's"
    );
    assert_eq!(
        order.borrow().as_slice(),
        &["event", "frame"],
        "ConfigReloaded must still be queued (and therefore fire) before \
         the frame's own change notification, even when it's the \
         groupings_changed branch — which runs first in source order — \
         that actually notifies"
    );
}

/// Phase 4b Task 1 fix round 1, MIN-8: M15's `pub use` re-export gave
/// `main.rs` a second startup caller of `rebuild_saved_scopes` alongside
/// `ShellView::new`'s own — printing diagnostics from both
/// unconditionally would mean one malformed `scopes.toml` entry prints
/// twice at every launch. `report_diagnostics: false` must not print;
/// `true` must — pinned via `hot_reload::SAVED_SCOPES_REPORT_CALLS`
/// (a test-only counter incremented once per printing call) rather than
/// by capturing `stderr`, which `eprintln!` gives no in-process hook for.
#[test]
fn rebuild_saved_scopes_prints_only_when_asked() {
    use crate::shell::hot_reload::{SAVED_SCOPES_REPORT_CALLS, rebuild_saved_scopes};

    // "bad" is a string, not a table — `saved_scopes_from_doc` always
    // produces at least one diagnostic for it, so this exercises the
    // branch that actually has something to print.
    let config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("scopes", "bad = \"not a table\"\n").unwrap()],
        ..ConfigSources::default()
    });
    SAVED_SCOPES_REPORT_CALLS.with(|c| c.set(0));

    rebuild_saved_scopes(&config, false);
    assert_eq!(
        SAVED_SCOPES_REPORT_CALLS.with(|c| c.get()),
        0,
        "report_diagnostics: false must not print — this is main.rs's own call"
    );

    rebuild_saved_scopes(&config, true);
    assert_eq!(
        SAVED_SCOPES_REPORT_CALLS.with(|c| c.get()),
        1,
        "report_diagnostics: true must print exactly once — this is \
         ShellView::new's (and apply_reload's) own call"
    );
}

/// Phase 4b Task 1 fix round 1, MIN-9: `ShellView::today` is read fresh
/// from the clock once per ~500ms reload-poll tick (alongside the flip
/// sweep and the dirty-session flush), not on every paint. A stale value
/// set directly here stands in for "yesterday" — the test executor's
/// virtual clock (what `advance_clock` moves) never touches the real
/// `chrono::Local::now()` this reads, the same limitation `shell::
/// tests::flip`'s own reload-poll-tick test documents — so this proves
/// the tick corrects a wrong value rather than proving a date rollover
/// specifically.
#[gpui::test]
fn the_reload_poll_tick_refreshes_today(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);

    let real_today = chrono::Local::now().date_naive();
    let stale = real_today - chrono::Duration::days(1);
    shell.update(&mut vcx, |s, _cx| s.today = stale);
    assert_eq!(shell.read_with(&vcx, |s, _| s.today), stale);

    vcx.run_until_parked();
    vcx.executor().advance_clock(
        crate::shell::hot_reload::RELOAD_POLL_INTERVAL + std::time::Duration::from_millis(1),
    );
    vcx.run_until_parked();

    assert_eq!(
        shell.read_with(&vcx, |s, _| s.today),
        real_today,
        "one reload-poll tick must refresh `today` from the clock"
    );
}
