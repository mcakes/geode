//! Config hot reload applied through `apply_reload` directly (the real
//! entity, the real method the watcher calls) plus the frame-slot keys.

use super::*;
// Explicit: this file's own `mod reload;` declaration in `tests/mod.rs`
// shadows the glob-imported `crate::reload`, so the bare `reload::` paths
// below need this to resolve to the real module rather than to `self`.
use crate::reload;

/// A `views` doc the app compiled in — the shape `--demo` supplies a
/// whole generated desk in (`app`, `datasets`, `groupings`, `sources`,
/// `views`), reduced to the one doc this test asserts on.
const BUILTIN_VIEWS_DOC: &str = "[risk]\ncolumns = [\"delta\"]\n";

/// `test_services()` with a compiled-in builtin layer that is more than
/// the keymap — the only fixture in which the reload's builtin handling
/// is observable at all, since every other one starts from an empty
/// `ConfigSources`.
fn services_with_a_builtin_views_doc() -> ShellServices {
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("views", BUILTIN_VIEWS_DOC).unwrap(),
        ],
        desk: None,
        user: None,
    });
    services
}

/// The bug a trader actually hit: the app writes a config file of its own
/// accord — here a theme pick's `persist_theme`, exactly as the
/// views dialog's save or a font-size change would — the mtime watcher
/// sees the change and reloads, and the views the app compiled in are
/// gone. Under `--demo` that meant the views dialog reporting "no views
/// are configured" the instant anything was saved, with only a restart to
/// bring them back.
///
/// The watcher's own timer cannot be driven from a gpui test (see
/// `apply_reload`'s doc comment), so this runs the two steps it schedules
/// verbatim: `reload::load_config` off the live `services.builtin`, then
/// `apply_reload`.
#[gpui::test]
fn a_config_write_and_the_reload_it_triggers_keep_the_apps_builtin_views(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut vcx) =
        open_shell_with_user_dir(cx, services_with_a_builtin_views_doc(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    assert!(
        shell.read_with(&vcx, |shell, _| shell
            .services
            .config
            .doc("views")
            .is_some()),
        "fixture is wrong: the shell should start with the builtin views doc"
    );

    // The user changes the theme; `persist_theme` writes `app.toml` into
    // the user config dir off the UI thread.
    vcx.update(|_window, cx| {
        shell.update(cx, |shell, cx| {
            assert!(shell.services.theme.apply("Gruvbox Light", cx));
            shell.persist_theme(cx);
        });
    });
    vcx.run_until_parked();
    assert!(
        dir.path().join("app.toml").exists(),
        "the theme pick should have written app.toml — without that \
         write there is no reload to test"
    );

    let builtin = shell.read_with(&vcx, |shell, _| shell.services.builtin.clone());
    let new_config = reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut vcx, |shell, cx| shell.apply_reload(new_config, cx));

    shell.read_with(&vcx, |shell, _| {
        assert!(
            shell.services.config.get("views", "risk.columns").is_some(),
            "the reload triggered by the app's own config write dropped the \
             builtin views — every module reading views sees an empty desk \
             until restart"
        );
    });
}

/// `tips::Chords` (the workspace's second gpui global — see its own doc
/// comment) is written at startup and refreshed on every reload
/// (`hot_reload::apply_reload`, right after `self.services.keymap =
/// keymap;`); this is the reload half of that pair. Same fixture and
/// drive as the test above — a real user config dir, `reload::
/// load_config` off the live `services.builtin`, then `apply_reload`
/// directly (the watcher's own ~500ms poll cannot be driven from a
/// `#[gpui::test]`, per `config_with_mod`'s doc comment) — except this
/// one writes `keymap.toml` rather than `app.toml`, in the same
/// `[[bindings]]` / `[bindings.keys]` shape `keymap_edit` writes and
/// `bad_config_and_shell`'s own keymap.toml fixture uses.
#[gpui::test]
fn a_keymap_reload_refreshes_the_chords_global(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);

    let before = vcx.update(|_, cx| {
        crate::tips::chord_for(&cx.global::<crate::tips::Chords>().0, "palette::toggle")
    });
    assert!(
        before.is_some(),
        "the builtin binding is visible at startup"
    );

    // Rebind in the user layer the way keymap_edit writes it: shadow the
    // builtin ctrl+k with "none" and bind ctrl+space to the same action.
    std::fs::write(
        dir.path().join("keymap.toml"),
        "config_version = 1\n[[bindings]]\n[bindings.keys]\n\"ctrl+k\" = \"none\"\n\"ctrl+space\" = \"palette::toggle\"\n",
    )
    .unwrap();

    let builtin = shell.read_with(&vcx, |shell, _| shell.services.builtin.clone());
    let new_config = reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut vcx, |shell, cx| shell.apply_reload(new_config, cx));

    let after = vcx.update(|_, cx| {
        crate::tips::chord_for(&cx.global::<crate::tips::Chords>().0, "palette::toggle")
    });
    assert_eq!(
        after,
        Some(crate::keymap::parse_binding("ctrl+space", crate::keymap::Modifiers::NONE).unwrap()),
        "the global follows the reload"
    );
}

/// A `colors.toml` edit emits `ConfigReloaded`, allowing the app bridge to
/// reload named colors and update module factories without a restart.
#[gpui::test]
fn a_colours_change_fires_config_reloaded(cx: &mut gpui::TestAppContext) {
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
    let shell = shell_of(&window, &mut cx);

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let sink = events.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            sink.borrow_mut().push(e.clone())
        })
        .detach();
    });

    // The fixture's config has no docs at all, so this reload's ONLY
    // difference from the running one is the `colours` doc — `views`,
    // `view_presentation` and `dimensions` are all absent before and
    // after, which is what makes the emission attributable to `colours`.
    let new_config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("colors", "[delta]\nhue = 240\n").unwrap()],
        desk: None,
        user: None,
    });
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e, ShellEvent::ConfigReloaded)),
        "a colours-only reload must fire ConfigReloaded: {:?}",
        events.borrow()
    );
}

/// The hot-reload path reads the user directory from disk: a `colors.toml`
/// edit and a still-unrenamed `colours.toml` both land in the `colors` doc,
/// so either fires `ConfigReloaded` and the modules see the definitions.
#[gpui::test]
fn a_colors_file_on_disk_reloads_under_either_name(cx: &mut gpui::TestAppContext) {
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
    let shell = shell_of(&window, &mut cx);

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let sink = events.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            sink.borrow_mut().push(e.clone())
        })
        .detach();
    });

    let user = tempfile::tempdir().unwrap();
    for (file, hue) in [("colors.toml", 240), ("colours.toml", 120)] {
        for stale in ["colors.toml", "colours.toml"] {
            let _ = std::fs::remove_file(user.path().join(stale));
        }
        std::fs::write(
            user.path().join(file),
            format!("config_version = 1\n[delta]\nhue = {hue}\n"),
        )
        .unwrap();
        events.borrow_mut().clear();
        let new_config =
            crate::reload::load_config(Vec::new(), None, Some(user.path().to_path_buf()));
        shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));
        assert!(
            events
                .borrow()
                .iter()
                .any(|e| matches!(e, ShellEvent::ConfigReloaded)),
            "{file}: a colors reload must fire ConfigReloaded: {:?}",
            events.borrow()
        );
        let hue_now = shell.read_with(&cx, |shell, _| {
            shell
                .services
                .config
                .get(geode_core::config::COLORS_DOC, "delta.hue")
                .and_then(toml::Value::as_integer)
        });
        assert_eq!(hue_now, Some(hue), "{file} is the live colors doc");
    }
}

/// Dataset presentation participates in resolved views beneath the view overlay.
/// Reloading it must emit `ConfigReloaded` so dataset-level column edits reach
/// consumers immediately.
#[gpui::test]
fn a_dataset_presentation_change_fires_config_reloaded(cx: &mut gpui::TestAppContext) {
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
    let shell = shell_of(&window, &mut cx);

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let sink = events.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            sink.borrow_mut().push(e.clone())
        })
        .detach();
    });

    // The fixture's config has no docs at all, so this reload's ONLY
    // difference from the running one is the `dataset_presentation` doc —
    // `views`, `view_presentation` and `dimensions` are all absent before
    // and after, which is what makes the emission attributable to
    // `dataset_presentation`.
    let new_config = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("dataset_presentation", "[risk.columns.npv]\nwidth = 140\n").unwrap(),
        ],
        desk: None,
        user: None,
    });
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e, ShellEvent::ConfigReloaded)),
        "a dataset_presentation-only reload must fire ConfigReloaded: {:?}",
        events.borrow()
    );
}

/// A valid reload applies the changed modifier alias and rebuilt keymap, closes the
/// palette whose bindings changed, and records an Applied outcome.
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

    // Use the nondefault `cmd` alias; `ctrl` is invalid configuration.
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

/// Every applied reload bumps `versions.config`, including changes outside views and
/// dimensions. This fixture changes only `[keymap]`, so it verifies the general config
/// notification independently of the narrower data-reload event.
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

/// The shared setup `apply_reload_with_an_error_diagnostic_keeps_last_good_config`
/// and `a_rejected_reload_emits_reload_rejected_with_the_errors` both need:
/// a real window and shell (palette opened, so a caller can assert a
/// rejected reload leaves it alone) plus a desk-layer `Config` whose
/// unsupported `config_version` is an error diagnostic on `Config::load`
/// itself (`geode_core::config::load_layer`) — the one thing distinguishing
/// the two tests is what each does with `bad_config` once `apply_reload`
/// has run.
fn bad_config_and_shell(
    cx: &mut gpui::TestAppContext,
) -> (Entity<ShellView>, gpui::VisualTestContext, Config) {
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

    // A desk-layer doc with an unsupported config_version is an error
    // diagnostic on `Config::load` itself (geode_core::config::load_layer).
    let desk = tempfile::tempdir().unwrap();
    std::fs::write(desk.path().join("app.toml"), "config_version = 99\n").unwrap();
    // A second desk file whose only problem is a WARNING (an unknown
    // action bound to a key, `keymap::build::unknown_action_is_warning_
    // and_skipped`'s own fixture) — added so `new_config.diagnostics`
    // carries both severities at once. `apply_reload`'s `errors` (what
    // `ReloadOutcome::KeptLastGood` carries) already filters to `Error`
    // alone, so this changes nothing either existing assertion here
    // reads; it exists for `a_rejected_reload_emits_reload_rejected_with_
    // the_errors`, which needs a mix to tell "errors only" apart from
    // "every diagnostic" at all — with `app.toml` alone, the two answers
    // are the same one-element vector and the distinction is untestable.
    std::fs::write(
        desk.path().join("keymap.toml"),
        "config_version = 1\n[[bindings]]\n[bindings.keys]\n\"mod+x\" = \"nope::nothing\"\n",
    )
    .unwrap();
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

    (shell, cx, bad_config)
}

/// An error-severity diagnostic in the new config (here: an
/// unsupported `config_version`) means the entire previous `Config`
/// (and everything built from it — mod alias, keymap) is kept
/// untouched, and the outcome records the error for the status bar.
/// A palette open at the time stays open — only a *successful* reload
/// closes it.
#[gpui::test]
fn apply_reload_with_an_error_diagnostic_keeps_last_good_config(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, bad_config) = bad_config_and_shell(cx);

    let original_mod_alias = shell.read_with(&cx, |shell, _| shell.services.mod_alias);

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

/// A rejected reload emits its errors, allowing dialogs and the bridge to explain that
/// the file is on disk but its config is not live.
#[gpui::test]
fn a_rejected_reload_emits_reload_rejected_with_the_errors(cx: &mut gpui::TestAppContext) {
    let (shell, mut cx, bad_config) = bad_config_and_shell(cx);

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let sink = events.clone();
        cx.subscribe(&shell, move |_, e: &ShellEvent, _| {
            sink.borrow_mut().push(e.clone())
        })
        .detach();
    });

    shell.update(&mut cx, |shell, cx| shell.apply_reload(bad_config, cx));

    // The fixture contains both an unknown-action warning and a config error. Confirm
    // both enter the diagnostics batch so the event's error filtering is observable.
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    assert!(
        diagnostics.read_with(&cx, |d, _| d
            .config
            .iter()
            .any(|d| d.severity == geode_core::config::Severity::Warning)),
        "sanity: the fixture must carry a warning alongside its error \
         for this test to tell 'errors only' apart from 'every diagnostic'"
    );

    let rejected: Vec<_> = events
        .borrow()
        .iter()
        .filter_map(|e| match e {
            ShellEvent::ReloadRejected(d) => Some(d.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(rejected.len(), 1);
    assert!(!rejected[0].is_empty());
    assert!(
        rejected[0]
            .iter()
            .all(|d| d.severity == geode_core::config::Severity::Error),
        "a warning alongside the fixture's error must never reach \
         `ReloadRejected` — {:?}",
        rejected[0]
    );
}

/// `keymap.mod = "ctrl"` produces an error and rejects the reload, preserving the
/// entire last-good config and modifier alias. A later valid alias applies and clears
/// the error status.
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

fn config_with_theme(name: &str) -> Config {
    Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", &format!("[theme]\nname = \"{name}\"\n")).unwrap()],
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
                // `builtin` deliberately left at `test_services()`'s
                // empty vec: this test drives `apply_reload` directly
                // (below and via `new_config`), which only ever assigns
                // `self.services.config` and never reads `builtin` — see
                // `test_services`'s own comment.
                services.config = config_with_theme("Gruvbox Dark");
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

    let new_config = config_with_theme("Default Dark");
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
/// re-apply the theme — a runtime palette/settings theme pick done
/// between the old config being applied and this reload survives
/// untouched, rather than being silently reverted to what `[theme]`
/// still says.
#[gpui::test]
fn apply_reload_preserves_a_runtime_theme_pick_when_theme_table_is_unchanged(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let mut services = test_services();
                // `builtin` deliberately left at `test_services()`'s
                // empty vec: this test drives `apply_reload` directly
                // (below and via `new_config`), which only ever assigns
                // `self.services.config` and never reads `builtin` — see
                // `test_services`'s own comment.
                services.config = config_with_theme("Gruvbox Dark");
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

    // A runtime pick (palette or settings dialog), independent of
    // config, before any reload happens.
    shell.update(&mut cx, |shell, cx| {
        assert!(shell.services.theme.apply("Gruvbox Light", cx));
    });
    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .theme
            .active_name()
            .to_string()),
        "Gruvbox Light",
        "sanity: the pick applied"
    );

    // Same [theme] table as the config already applied — a reload
    // triggered by, say, an unrelated keymap.toml edit.
    let new_config = config_with_theme("Gruvbox Dark");
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    shell.read_with(&cx, |shell, _| {
        assert_eq!(
            shell.services.theme.active_name(),
            "Gruvbox Light",
            "an unchanged [theme] table must not re-apply the theme, or the \
             runtime pick above would be silently reverted"
        );
        assert_eq!(
            shell.last_reload,
            reload::ReloadOutcome::Applied { warnings: vec![] },
            "the reload itself still succeeds — only the theme re-apply is guarded"
        );
    });
}

/// A theme-only reload preserves an open palette because its registry and keymap inputs
/// have not changed. This includes the watcher reading back a theme choice persisted by
/// the app itself.
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
    let new_config = config_with_theme("Gruvbox Dark");
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

    // Use the nondefault `cmd` alias; `ctrl` is invalid configuration.
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

/// A background reload that closes a focused palette input schedules root-focus
/// restoration. The next render consumes that flag because `apply_reload` itself has no
/// Window for moving focus directly.
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

    // A keymap change closes the palette while its input holds focus. `apply_reload`
    // has no Window to redirect focus immediately, so the next render must recover it.
    // Use `cmd` as the valid nondefault alias.
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

// Frame keys, readouts, and config reload.

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
    // See `test_services`'s own comment: mirroring `builtin` here (rather
    // than leaving it at its inherited empty vec) costs nothing and keeps
    // this fixture a real `(config, builtin)` pair.
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
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

/// An `expressions` reload redefines the frame's named expressions: adding
/// an entry defines it, and editing its text replaces it.
#[gpui::test]
fn an_expressions_reload_redefines_the_frames_named_expressions(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let config = |expressions: &str| {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
                LayerDoc::builtin("expressions", expressions).unwrap(),
            ],
            ..ConfigSources::default()
        })
    };
    let liq_text = |cx: &mut gpui::VisualTestContext| {
        shell.read_with(cx, |s, cx| {
            s.frame
                .read(cx)
                .named_expressions()
                .get("liq")
                .map(|e| e.text().to_string())
        })
    };
    assert_eq!(liq_text(&mut cx), None);

    shell.update(&mut cx, |s, cx| {
        s.apply_reload(config("[liq]\nexpression = \"npv > 0\"\n"), cx)
    });
    assert_eq!(liq_text(&mut cx).as_deref(), Some("npv > 0"));

    shell.update(&mut cx, |s, cx| {
        s.apply_reload(config("[liq]\nexpression = \"npv > 5\"\n"), cx)
    });
    assert_eq!(liq_text(&mut cx).as_deref(), Some("npv > 5"));
}

/// Compare reloads against the sources baseline used to start the data service.
/// Returning to that baseline clears restart-required status, even after intervening
/// reloads.
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

/// Egress transports are resolved at startup. A changed `egress.toml` requires restart
/// while it differs from the engine's startup baseline.
#[gpui::test]
fn an_egress_change_asks_for_a_restart(cx: &mut gpui::TestAppContext) {
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

    let mut with_egress = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin(
                "egress",
                "[sophis]\nadapter = \"demo_bus\"\n[sophis.documents]\ncvi_params = \"marketdata/cvi\"\n",
            )
            .unwrap(),
        ],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut with_egress), cx)
    });
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e, ShellEvent::RestartRequired(m) if m.contains("egress"))),
        "{:?}",
        events.borrow()
    );
}

/// The pricing adapter is selected at startup. Changing it requires restart; reverting
/// to the startup baseline clears that message.
#[gpui::test]
fn a_pricing_change_requires_a_restart_and_a_revert_clears_it(cx: &mut gpui::TestAppContext) {
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

    let mut with_pricing = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("app", "[pricing]\nadapter = \"vendor\"\n").unwrap(),
        ],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut with_pricing), cx)
    });
    assert!(
        events
            .borrow()
            .iter()
            .any(|e| matches!(e, ShellEvent::RestartRequired(m) if m.contains("pricing"))),
        "{:?}",
        events.borrow()
    );

    events.borrow_mut().clear();
    let mut reverted = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut reverted), cx)
    });
    assert!(
        !events
            .borrow()
            .iter()
            .any(|e| matches!(e, ShellEvent::RestartRequired(_))),
        "back at the baseline: {:?}",
        events.borrow()
    );
    assert!(shell.read_with(&cx, |s, _| s.restart_required.is_none()));
}

/// Only `[pricing] adapter` belongs to the restart baseline. Changes to the live
/// `refresh` setting must not request restart.
#[gpui::test]
fn a_pricing_refresh_change_needs_no_restart(cx: &mut gpui::TestAppContext) {
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

    let mut with_refresh = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("app", "[pricing]\nrefresh = \"10s\"\n").unwrap(),
        ],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut with_refresh), cx)
    });
    assert!(
        !events
            .borrow()
            .iter()
            .any(|e| matches!(e, ShellEvent::RestartRequired(_))),
        "no adapter key changed: {:?}",
        events.borrow()
    );
    assert!(shell.read_with(&cx, |s, _| s.restart_required.is_none()));
}

/// View presentation changes resolved `ViewSpec`s and must participate in
/// `ConfigReloaded`. Establish a config with a views document first, then change only
/// its presentation overlay to isolate that dependency.
#[gpui::test]
fn a_view_presentation_only_change_emits_config_reloaded(cx: &mut gpui::TestAppContext) {
    const DATASETS: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n";
    const VIEWS: &str = "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[v.columns]]\nname = \"book\"\nkind = \"dimension\"\n";

    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    let config_with = |presentation: Option<&str>| {
        let mut builtin = vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("datasets", DATASETS).unwrap(),
            LayerDoc::builtin("views", VIEWS).unwrap(),
        ];
        if let Some(text) = presentation {
            builtin.push(LayerDoc::builtin("view_presentation", text).unwrap());
        }
        Config::load(&ConfigSources {
            builtin,
            ..ConfigSources::default()
        })
    };

    // Baseline: `views` is now present in the shell's own config.
    let mut baseline = config_with(None);
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut baseline), cx)
    });

    let fired = std::rc::Rc::new(std::cell::RefCell::new(0usize));
    let f = fired.clone();
    cx.update(|_, cx| {
        cx.subscribe(&shell, move |_, event: &ShellEvent, _| {
            if matches!(event, ShellEvent::ConfigReloaded) {
                *f.borrow_mut() += 1;
            }
        })
        .detach();
    });

    let mut with_presentation = config_with(Some("[v]\norder = [\"book\"]\n"));
    shell.update(&mut cx, |s, cx| {
        s.apply_reload(std::mem::take(&mut with_presentation), cx)
    });

    assert_eq!(
        *fired.borrow(),
        1,
        "a view_presentation-only edit must trigger the same reload path a views edit does"
    );
}

/// Queue `ConfigReloaded` before notifying the frame. GPUI flushes effects in queue
/// order, so the app bridge must replace view definitions before frame observers
/// requery using the new config version. Record callback order directly; shell tests
/// cannot depend on a real module tile to observe its requery.
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

/// A dimensions change can rebuild both grouping slots and views in one reload. Resolve
/// a previously unknown grouping dimension so slot replacement really notifies the
/// frame, then verify that `ConfigReloaded` is queued before that notification too.
#[gpui::test]
fn a_dimensions_change_that_resolves_a_grouping_slot_still_emits_config_reloaded_before_the_frame_notifies(
    cx: &mut gpui::TestAppContext,
) {
    let (mut services, _log) = services_with_recorder();
    // See `test_services`'s own comment: mirroring `builtin` here (rather
    // than leaving it at its inherited empty vec) costs nothing and keeps
    // this fixture a real `(config, builtin)` pair.
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
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

/// Saved-scope rebuilding can run at both app and shell startup. `report_diagnostics`
/// controls which caller reports errors, avoiding duplicate messages. The test counter
/// verifies reporting without capturing the logging output.
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

/// The reload-poll tick refreshes `ShellView::today` from `AppClock`. Seed a stale date
/// and advance the executor clock to prove the poll corrects it. The virtual executor
/// clock does not move the application clock, so this verifies refresh rather than a
/// real midnight rollover.
#[gpui::test]
fn the_reload_poll_tick_refreshes_today(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);

    let real_today = shell.read_with(&vcx, |s, cx| s.clock(cx).today(chrono::Utc::now()));
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

/// Hot reload splices module fragments back into the rebuilt keymap. The loaded files
/// alone do not contain them; module bindings must survive any config reload.
#[gpui::test]
fn a_reload_keeps_the_modules_fragment_bindings(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_a_module_fragment(REC_FRAGMENT);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");

    shell.update(&mut vcx, |shell, cx| {
        shell.apply_reload(config_with_theme("Gruvbox Dark"), cx)
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    shell.read_with(&vcx, |shell, _| {
        assert_eq!(
            shell.services.theme.active_name(),
            "Gruvbox Dark",
            "fixture check: the reload really was applied"
        );
    });

    log.borrow_mut().clear();
    vcx.simulate_keystrokes("q");
    assert!(
        log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(_, a, _) if a.0 == "rec::noop"
        )),
        "the module's fragment binding must survive the reload: {:?}",
        log.borrow()
    );
}

/// Diagnostics for dropped fragment bindings survive reload through
/// `keymap_fragment_diagnostics`; the filtered bindings cannot be diagnosed again by
/// keymap building. These module-authored errors remain visible without rejecting an
/// otherwise valid user config reload.
#[gpui::test]
fn a_dropped_fragment_bindings_diagnostic_survives_a_reload(cx: &mut gpui::TestAppContext) {
    let services = services_with_a_dropped_fragment_binding();
    let expected = services.keymap_fragment_diagnostics[0].clone();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());

    shell.update(&mut vcx, |shell, cx| {
        shell.apply_reload(config_with_theme("Gruvbox Dark"), cx)
    });

    assert_eq!(
        shell.read_with(&vcx, |s, _| s.services.theme.active_name().to_string()),
        "Gruvbox Dark",
        "a fragment's own error must not reject the trader's reload"
    );
    assert!(
        diagnostics.read_with(&vcx, |d, _| d.config.contains(&expected)),
        "the fragment diagnostic must still be in the config section: {:?}",
        diagnostics.read_with(&vcx, |d, _| d.config.clone())
    );
}

/// Reloading `[time] zone` republishes `AppClock` without changing frame data or as-of
/// versions, so display-time changes do not trigger queries.
#[gpui::test]
fn a_time_zone_reload_republishes_the_clock_without_a_requery(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    let before = vcx.update(|_w, cx| cx.global::<crate::clock::AppClock>().0);
    let versions_before = shell.read_with(&vcx, |s, cx| s.frame().read(cx).versions());

    // Choose a zone different from the machine's current zone so the test observes a
    // change on any host.
    let target = if before.zone_name() == "Asia/Tokyo" {
        "Europe/London"
    } else {
        "Asia/Tokyo"
    };
    std::fs::write(
        dir.path().join("app.toml"),
        format!("config_version = 1\n[time]\nzone = \"{target}\"\n"),
    )
    .unwrap();
    let builtin = shell.read_with(&vcx, |shell, _| shell.services.builtin.clone());
    let new_config = reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut vcx, |shell, cx| shell.apply_reload(new_config, cx));

    let after = vcx.update(|_w, cx| cx.global::<crate::clock::AppClock>().0);
    assert_ne!(before, after);
    assert_eq!(after.zone_name(), target);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.today),
        after.today(chrono::Utc::now())
    );
    let versions_after = shell.read_with(&vcx, |s, cx| s.frame().read(cx).versions());
    assert_eq!(versions_before.data, versions_after.data);
    assert_eq!(versions_before.as_of, versions_after.as_of);
}
