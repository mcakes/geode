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

/// 2c §6.2: a named colour is part of what a tile paints, so a
/// `colours.toml` edit must reach the modules the same way a `views`
/// edit does — through `ConfigReloaded`, which is what makes the app's
/// bridge re-read the doc and hand the blotter factory the new
/// definitions. Without it a trader's colour change would sit on disk
/// until the next restart.
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
        builtin: vec![LayerDoc::builtin("colours", "[delta]\nhue = 240\n").unwrap()],
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

/// dataset-presentation spec §6: `dataset_presentation.toml` is merged
/// under the view overlay in `load_views`, exactly the seam
/// `view_presentation` rides — so a change to it must fan out through
/// `ConfigReloaded` the same way, or a dataset-level column edit sits
/// on disk until the next restart.
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

/// §19.6: a rejected reload says so as an event carrying the errors, so
/// a dialog (or the bridge) can tell a trader the file they just wrote
/// is on disk but not live.
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

    // Sanity: `apply_reload` folds the `keymap.toml` fixture's own
    // unknown-action warning into `new_config.diagnostics` (`note_config`
    // below stores exactly that set, on every outcome — Phase 4b §4.4)
    // alongside the `app.toml` fixture's error — otherwise the assertion
    // below would hold trivially whether or not the filter it is meant
    // to pin is even there.
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

/// line-pricer §5.5: the pricer the data engine runs is chosen at startup
/// from `[pricing] adapter`, so a reload that changes that table needs a
/// restart on the same terms `sources`/`datasets` already follow —
/// reverting to the baseline the shell started with clears the message
/// again.
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

/// line-pricer §5.5, final-review finding 2: the restart baseline is
/// narrowed to `[pricing] adapter` — `refresh` becomes a live sheet
/// setting in Part 3 and must never demand a restart.
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

/// Phase 4c: `view_presentation.toml` is merged over the views
/// (`geode_core::config::load_views`), so a change to it changes the
/// `ViewSpec`s every tile runs on just as a `views` edit does. The Views
/// dialog writes it on the commonest edit a trader makes — a column
/// width — and then relies on the 500 ms mtime watcher's ordinary reload
/// to apply it. Left out of `views_changed`, that write would sit on disk
/// until the next restart, which is the one outcome the whole
/// presentation split exists to avoid.
///
/// Two reloads: the first establishes a baseline whose `views` doc is
/// already present, so the second differs in `view_presentation` and in
/// nothing else.
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

/// The reload half of keymap fragments (market-data documents §8.4): a
/// hot reload rebuilds the keymap from the freshly loaded config, which
/// knows nothing about any module, so `apply_reload` has to splice the
/// services' own fragments back in. Without that, the FIRST config write
/// of a session — a theme pick, a font-size step, any dialog save —
/// silently unbinds every module key until restart, the same class of bug
/// `ShellServices::builtin`'s own doc comment records for the builtin
/// docs.
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

/// Fix round 1, Minor 1: a dropped fragment binding's diagnostic must
/// still be in the diagnostics tile's config section after a hot reload.
/// It cannot be recomputed there — `check_fragment` removed the offending
/// binding before `build_keymap` ever saw it, so the fresh keymap build a
/// reload runs has nothing to say about it — and the reload replaces the
/// whole config batch, so without `ShellServices::
/// keymap_fragment_diagnostics` being folded back in the entry silently
/// vanished at the first reload of the session.
///
/// Also pins the other half of that fold: the reload is still APPLIED. A
/// compiled-in fragment's error is the module author's mistake, not the
/// trader's, so it must not join `new_config.diagnostics` and make
/// `reload::decide` keep last-good over something no file they can edit
/// would fix.
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

/// `[time] zone` is live (as-of dialog spec §6.1): a reload with a new
/// zone re-publishes `AppClock`, and nothing requeries — the frame's
/// data and as-of versions are untouched.
#[gpui::test]
fn a_time_zone_reload_republishes_the_clock_without_a_requery(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    let before = vcx.update(|_w, cx| cx.global::<crate::clock::AppClock>().0);
    let versions_before = shell.read_with(&vcx, |s, cx| s.frame().read(cx).versions());

    std::fs::write(
        dir.path().join("app.toml"),
        "config_version = 1\n[time]\nzone = \"Asia/Tokyo\"\n",
    )
    .unwrap();
    let builtin = shell.read_with(&vcx, |shell, _| shell.services.builtin.clone());
    let new_config = reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut vcx, |shell, cx| shell.apply_reload(new_config, cx));

    let after = vcx.update(|_w, cx| cx.global::<crate::clock::AppClock>().0);
    assert_ne!(before, after);
    assert_eq!(after.zone_name(), "Asia/Tokyo");
    let versions_after = shell.read_with(&vcx, |s, cx| s.frame().read(cx).versions());
    assert_eq!(versions_before.data, versions_after.data);
    assert_eq!(versions_before.as_of, versions_after.as_of);
}
