//! `ShellView`'s wiring of the `Diagnostics` entity (Phase 4b §4.4): the
//! status bar's summary, an `[log]` reload's `LevelControl` apply, and
//! the overlay-toggle drain. The catalog-request drain lives entirely
//! in `geode-app::bridge` (the only crate allowed to touch `geode-data`)
//! and is tested there instead.

use super::*;
use crate::diagnostics::Health;
// Explicit: this file's sibling `mod reload;` declaration in
// `tests/mod.rs` shadows the glob-imported `crate::reload` (same
// shadowing `shell/tests/reload.rs` documents on its own copy of this
// import), so `reload::load_config` below needs this to resolve to the
// real module rather than to the test module `shell::tests::reload`.
use crate::reload;
use geode_core::config::ConfigSources;
use geode_core::log::{Level, LogLevels, Ring};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// A `LevelControl` that records every `set` call rather than touching a
/// real subscriber — `LevelControl: Send + Sync` (so a `Mutex`, not a
/// `RefCell`), the same test-double shape `geode_core::log`'s own
/// `ReloadControl` production impl mirrors.
#[derive(Default)]
struct RecordingLevelControl {
    calls: Mutex<Vec<LogLevels>>,
}

impl LevelControl for RecordingLevelControl {
    fn set(&self, levels: &LogLevels) -> Result<(), String> {
        self.calls.lock().unwrap().push(levels.clone());
        Ok(())
    }
}

/// The status bar's `diagnostics-summary` indicator (spec §4.4), fed
/// from `Diagnostics::summary()` after `note_health` — the replacement
/// for Phase 3's deleted `set_data_status` (see the note left in
/// `shell/tests/occupants.rs`).
#[gpui::test]
fn the_status_bar_shows_the_diagnostics_summary_after_note_health(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());

    assert!(
        cx.debug_bounds("diagnostics-summary").is_none(),
        "nothing to report yet"
    );

    diagnostics.update(&mut cx, |d, cx| {
        d.note_health(
            "risk",
            Health::Degraded { reason: "x".into() },
            "x".into(),
            SystemTime::now(),
        );
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert_eq!(
        diagnostics.read_with(&cx, |d, _| d.summary()).as_ref(),
        "sources 1 degraded"
    );
    assert!(
        cx.debug_bounds("diagnostics-summary").is_some(),
        "the status bar shows the diagnostics summary"
    );
}

/// Hovering the diagnostics summary segment (Task 4, spec §5.1) says what
/// a click on it does — the one action this segment has, since it names
/// no keyboard chord.
#[gpui::test]
fn hovering_the_diagnostics_summary_says_it_opens_the_tile(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());

    diagnostics.update(&mut vcx, |d, cx| {
        d.note_health(
            "risk",
            Health::Degraded { reason: "x".into() },
            "x".into(),
            SystemTime::now(),
        );
        cx.notify();
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let seg = vcx
        .debug_bounds("diagnostics-summary")
        .expect("summary painted");
    vcx.simulate_mouse_move(
        seg.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-diagnostics-summary").is_some());
}

/// Clicking the status bar's diagnostics summary opens the diagnostics
/// tile via `open_module("diagnostics", ..)` (Phase 4b Task 5) — the one
/// production caller since `diagnostics::open` was retired (user ruling
/// 2026-09-09). `services_with_recorder`'s roster carries no
/// "diagnostics" factory, so the split tile falls back to the default
/// ("rec") kind — this test is only about the click reaching
/// `open_module` at all, not about which factory answers it (that's
/// `shell/tests/occupants.rs`'s job).
#[gpui::test]
fn clicking_the_diagnostics_summary_opens_a_tile(cx: &mut gpui::TestAppContext) {
    // MIN-8 (fix round 1): a "diagnostics"-kind factory registered, so
    // this test can assert the click actually reached `open_module
    // ("diagnostics", ..)` — not merely that a click on the summary
    // split *something* open.
    let (mut services, _log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new(
            "diagnostics",
        )));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut cx, |d, cx| {
        d.note_health(
            "risk",
            Health::Degraded { reason: "x".into() },
            "x".into(),
            SystemTime::now(),
        );
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().is_empty()));

    let bounds = cx.debug_bounds("diagnostics-summary").unwrap();
    cx.simulate_mouse_down(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let tile = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile());
    assert!(tile.is_some(), "the click split a tile open");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile.unwrap())),
        Some("diagnostics"),
        "the click must reach open_module(\"diagnostics\", ..), not just split something"
    );
}

/// `hot_reload::apply_reload`'s `[log]`-change detection (Phase 4b
/// §4.3): a reload whose `app` doc now carries a different `[log]`
/// table applies it through `LevelControl::set` exactly once, and
/// updates `Diagnostics.levels` to match — never re-persisted (this
/// only ever *applies* what was already on disk).
///
/// MAJ-6 (Phase 4b Task 4 fix round 1): `user_dir` is now a real
/// tempdir (was `None`, which made "never re-persisted" true only
/// because nothing *could* persist) — this test now proves
/// `apply_reload`'s `[log]` handling calls `Diagnostics::set_levels`,
/// not `request_level`, by asserting `app.toml` still does not exist
/// after the reload.
#[gpui::test]
fn a_log_table_change_on_reload_applies_it_through_level_control_once(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);
    cx.update(crate::shell::dialog::init_reclaimed_keybindings);

    let control = Arc::new(RecordingLevelControl::default());
    let mut services = test_services();
    services.log = Some(LogServices {
        ring: Arc::new(Ring::new(16)),
        control: control.clone(),
        levels: LogLevels::default(),
    });

    let dir = tempfile::tempdir().unwrap();
    let user_dir = dir.path().to_path_buf();

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view =
                    cx.new(|cx| ShellView::new(services, None, Some(user_dir.clone()), window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);

    let new_config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[log]\ningest = \"debug\"\n").unwrap()],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));
    cx.run_until_parked();

    let calls = control.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "LevelControl::set called exactly once");
    assert_eq!(calls[0].targets, vec![("ingest".to_string(), Level::DEBUG)]);
    drop(calls);

    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    assert_eq!(
        diagnostics.read_with(&cx, |d, _| d.levels.targets.clone()),
        vec![("ingest".to_string(), Level::DEBUG)],
        "the entity's own levels reflect the reload"
    );
    assert!(
        !user_dir.join("app.toml").exists(),
        "a reload-driven [log] change must not write app.toml — that's request_level's job, not set_levels'"
    );

    // A second reload with the exact same `[log]` table must not call
    // `LevelControl::set` again — `Diagnostics::set_levels`'s no-op
    // guard, exercised end to end through the real reload path.
    let same_config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[log]\ningest = \"debug\"\n").unwrap()],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |shell, cx| shell.apply_reload(same_config, cx));
    cx.run_until_parked();
    assert_eq!(
        control.calls.lock().unwrap().len(),
        1,
        "an unchanged [log] table calls LevelControl::set no further times"
    );
    assert!(!user_dir.join("app.toml").exists());
}

/// MIN-9: `Diagnostics.levels` must track a reload's `[log]` table even
/// when `ShellServices.log` is `None` (every test fixture that doesn't
/// opt in, and — in principle — a real run where `install_logging`
/// somehow never wired up `LogServices`) — only `LevelControl::set`
/// needs a real subscriber to call.
#[gpui::test]
fn a_log_table_change_updates_the_entity_even_without_log_services(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services()); // services.log is None
    let shell = shell_of(&window, &mut cx);

    let new_config = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("app", "[log]\ningest = \"debug\"\n").unwrap()],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    assert_eq!(
        diagnostics.read_with(&cx, |d, _| d.levels.targets.clone()),
        vec![("ingest".to_string(), Level::DEBUG)],
        "the entity updates even with no LogServices to apply through"
    );
}

/// `Diagnostics::request_overlay_toggle`'s drain (Phase 4b §4.4): a
/// module can queue an overlay toggle but never reach `ShellView`
/// directly (spec ruling) — `ShellView::on_diagnostics_changed` is the
/// one door, exercised here without going through a real module tile.
#[gpui::test]
fn request_overlay_toggle_flips_perf_overlay_on_the_next_observe(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());

    assert!(
        !shell.read_with(&cx, |s, _| s.perf_overlay),
        "the overlay starts hidden"
    );

    diagnostics.update(&mut cx, |d, cx| {
        d.request_overlay_toggle();
        cx.notify();
    });
    cx.run_until_parked();

    assert!(
        shell.read_with(&cx, |s, _| s.perf_overlay),
        "the queued toggle flipped perf_overlay"
    );

    diagnostics.update(&mut cx, |d, cx| {
        d.request_overlay_toggle();
        cx.notify();
    });
    cx.run_until_parked();

    assert!(
        !shell.read_with(&cx, |s, _| s.perf_overlay),
        "a second toggle flips it back off"
    );
}

/// `Diagnostics::request_level`'s drain persists into the tempdir's
/// `app.toml` (Phase 4b §4.3) — `log_persist::persist_log_level_to_
/// user_config`'s own unit tests cover the write itself; this proves
/// `ShellView::on_diagnostics_changed` actually calls it, on the
/// background executor, from a real `user_dir`.
#[gpui::test]
fn request_level_persists_into_the_tempdirs_app_toml(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(crate::shell::dialog::init_reclaimed_keybindings);

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
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());

    diagnostics.update(&mut cx, |d, cx| {
        d.request_level("ingest", Level::DEBUG);
        cx.notify();
    });
    cx.run_until_parked();

    let text = std::fs::read_to_string(user_dir.join("app.toml")).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(doc["log"]["ingest"].as_str(), Some("debug"));
}

/// MAJ-5's exact amplification path, end to end: a standing
/// `config_version` error (reproduced by every load from this desk
/// dir, independent of anything `:level` touches) must not inflate when
/// `request_level`'s own persist write triggers a reload that hands
/// `note_config` the *same* diagnostics batch again.
#[gpui::test]
fn a_level_persist_and_reload_leaves_the_config_error_count_unchanged(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);
    cx.update(crate::shell::dialog::init_reclaimed_keybindings);

    let desk_dir = tempfile::tempdir().unwrap();
    let user_dir = tempfile::tempdir().unwrap();
    // A config_version error every load from this desk dir reproduces.
    std::fs::write(desk_dir.path().join("app.toml"), "config_version = 99\n").unwrap();
    let desk_path = desk_dir.path().to_path_buf();
    let user_path = user_dir.path().to_path_buf();

    // Through `ShellServices::config_and_builtin`, so `config` and
    // `builtin` are derived together and cannot disagree — and so the
    // reload below is handed the *same* builtin docs the shell was built
    // from, which is the whole point of the parameter (`reload::
    // load_config`'s doc comment). Rebuilding the builtin layer at the
    // reload site instead is the bug that silently deleted every
    // non-keymap builtin doc on the first config write of a session.
    let builtin = vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()];
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin,
        desk: Some(desk_path.clone()),
        user: Some(user_path.clone()),
    });
    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    let mod_alias = default_mod();
    let (keymap, diags) = build_keymap(&builtin, mod_alias, &registry);
    assert!(diags.is_empty(), "{diags:?}");
    let (theme, warnings) = crate::theme::load_bundled();
    assert!(warnings.is_empty(), "{warnings:?}");
    let reload_builtin = builtin.clone();
    let services = ShellServices {
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
    };

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| {
                    ShellView::new(
                        services,
                        Some(desk_path.clone()),
                        Some(user_path.clone()),
                        window,
                        cx,
                    )
                });
                cx.new(|cx| Root::new(view, window, cx))
            })
        })
        .unwrap();
    let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());

    let error_count = |cx: &gpui::VisualTestContext| {
        diagnostics.read_with(cx, |d, _| {
            d.config
                .iter()
                .filter(|diag| diag.severity == geode_core::config::Severity::Error)
                .count()
        })
    };
    let before = error_count(&cx);
    assert!(
        before > 0,
        "the config_version error must be present to start"
    );
    let history_len_before = diagnostics.read_with(&cx, |d, _| d.config_history.len());

    diagnostics.update(&mut cx, |d, cx| {
        d.request_level("ingest", Level::DEBUG);
        cx.notify();
    });
    cx.run_until_parked();

    let new_config = reload::load_config(
        reload_builtin,
        Some(desk_path.clone()),
        Some(user_path.clone()),
    );
    shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));
    cx.run_until_parked();

    assert_eq!(error_count(&cx), before, "the error count must not inflate");
    assert_eq!(
        diagnostics.read_with(&cx, |d, _| d.config_history.len()),
        history_len_before,
        "an identical diagnostics batch must not grow the history either"
    );
}
