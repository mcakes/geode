//! `ShellView` integration with the `Diagnostics` entity: status summary, log-level
//! reloads, and overlay requests. Catalog requests are handled and tested in
//! `geode-app::bridge`, which can access `geode-data`.

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

/// The status bar's diagnostics summary reflects `Diagnostics::summary()` after a
/// health update.
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

/// The diagnostics-summary tooltip explains its click action. This segment has no
/// keyboard chord to display.
#[gpui::test]
fn hovering_the_diagnostics_summary_says_it_opens_the_page(cx: &mut gpui::TestAppContext) {
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
    // gpui's test API cannot read painted text: the title row is asserted
    // painted, and the constant it paints is asserted to name the page.
    assert!(vcx.debug_bounds("tip-diagnostics-summary-title").is_some());
    assert_eq!(
        crate::shell::status::DIAGNOSTICS_TIP_TITLE,
        "Open the diagnostics page"
    );
}

/// Clicking the diagnostics summary opens the diagnostics page through
/// `page::toggle_diagnostics`; no tile is split open.
#[gpui::test]
fn clicking_the_diagnostics_summary_opens_the_page(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let services = with_page(services, RecordingPageFactory::new("diagnostics"));
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

    shell.read_with(&cx, |s, _| {
        assert!(s.page_open(), "the click opened the page");
        assert_eq!(s.open_page_kind(), Some("diagnostics"));
        assert!(
            s.services.workspaces.active().tree().is_empty(),
            "no tile was split open"
        );
    });
}

/// A stopped data thread paints its own segment ahead of the diagnostics
/// summary: it outranks every count after it.
#[gpui::test]
fn a_stopped_thread_paints_the_stopped_segment_first(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    assert!(cx.debug_bounds("data-stopped").is_none());
    diagnostics.update(&mut cx, |d, cx| {
        d.note_health(
            "risk",
            Health::Degraded { reason: "x".into() },
            "x".into(),
            SystemTime::now(),
        );
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::now());
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let stopped = cx
        .debug_bounds("data-stopped")
        .expect("the stopped segment is painted");
    let summary = cx
        .debug_bounds("diagnostics-summary")
        .expect("the summary is painted");
    assert!(
        stopped.origin.x < summary.origin.x,
        "the stopped segment leads"
    );
}

/// Hovering the stopped segment shows the prepared reason in its tooltip.
#[gpui::test]
fn hovering_the_stopped_segment_shows_its_reason(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut vcx, |d, cx| {
        d.note_thread_stopped("geode-ingest", "boom".into(), SystemTime::now());
        cx.notify();
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let seg = vcx
        .debug_bounds("data-stopped")
        .expect("the stopped segment is painted");
    vcx.simulate_mouse_move(
        seg.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tip-data-stopped").is_some());
}

/// Clicking the stopped segment opens the diagnostics page through the
/// summary's own route. No health is reported, so the summary is absent and
/// the click can only land on the stopped segment.
#[gpui::test]
fn clicking_the_stopped_segment_opens_the_diagnostics_page(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let services = with_page(services, RecordingPageFactory::new("diagnostics"));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut cx, |d, cx| {
        d.note_thread_stopped("geode-data", "boom".into(), SystemTime::now());
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("diagnostics-summary").is_none());
    let bounds = cx
        .debug_bounds("data-stopped")
        .expect("the stopped segment is painted");
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
    shell.read_with(&cx, |s, _| {
        assert!(s.page_open(), "the click opened the page");
        assert_eq!(s.open_page_kind(), Some("diagnostics"));
        assert!(
            s.services.workspaces.active().tree().is_empty(),
            "no tile was split open"
        );
    });
}

/// A changed `[log]` table applies through `LevelControl::set` exactly once and updates
/// `Diagnostics.levels`. Reloading reads existing configuration; it must not queue
/// another persistence write. A real user directory makes this observable: `app.toml`
/// must remain absent after the reload.
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

/// `Diagnostics.levels` follows a reloaded `[log]` table even without
/// `ShellServices.log`. Only applying levels to a subscriber requires `LevelControl`.
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

/// Modules request an overlay toggle through `Diagnostics`;
/// `ShellView::on_diagnostics_changed` drains the request without exposing the shell
/// entity to the module.
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

/// A requested log level reaches persistence through
/// `ShellView::on_diagnostics_changed` and the background executor. The persistence
/// helper's unit tests cover the file edit itself.
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

/// Persisting a requested log level triggers a reload. Repeatedly receiving the same
/// standing `config_version` error must not accumulate duplicate diagnostics.
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
        restored_pinned: Default::default(),
        restored_links: Default::default(),
        restored_palette_usage: crate::palette_usage::PaletteUsage::new(),
        log: None,
        action_tail: std::sync::Arc::new(std::sync::Mutex::new(
            crate::diagnostics::ActionTail::new(),
        )),
        keymap_diagnostics: Vec::new(),
        keymap_fragments: Vec::new(),
        keymap_fragment_diagnostics: Vec::new(),
        composition_diagnostics: Vec::new(),
        pages: crate::module::PageRoster::new(),
        restored_pages: std::collections::BTreeMap::new(),
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

/// While `Diagnostics.ingest` is present, the status bar paints a loading segment and a
/// 2 px strip at its top edge. The strip is an absolute overlay and must not change the
/// bar's geometry.
#[gpui::test]
fn the_ingest_strip_and_segment_paint_only_while_a_load_is_running(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("ingest-strip").is_none(), "idle: no strip");
    assert!(
        vcx.debug_bounds("ingest-loading").is_none(),
        "idle: no segment"
    );
    let bar_before = vcx
        .debug_bounds("shell-status-bar")
        .expect("status bar painted");

    let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
    diagnostics.update(&mut vcx, |d, cx| {
        d.note_loading(
            "risk",
            "/data/risk/EOD.csv",
            2,
            std::time::SystemTime::now(),
        );
        cx.notify();
    });
    vcx.run_until_parked();
    let strip = vcx
        .debug_bounds("ingest-strip")
        .expect("loading: the strip paints");
    let seg = vcx
        .debug_bounds("ingest-loading")
        .expect("loading: the segment paints");
    let bar = vcx.debug_bounds("shell-status-bar").unwrap();
    assert_eq!(bar.origin.y, bar_before.origin.y, "the bar did not move");
    assert_eq!(
        bar.size.height, bar_before.size.height,
        "the bar did not grow"
    );
    assert_eq!(
        strip.origin.y, bar.origin.y,
        "the strip sits on the bar's top edge"
    );
    assert_eq!(strip.size.height, gpui::px(2.));
    assert!(
        strip.size.width >= bar.size.width - gpui::px(1.),
        "full width"
    );
    assert!(seg.size.width > gpui::px(0.));

    diagnostics.update(&mut vcx, |d, cx| {
        d.note_load_ended();
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("ingest-strip").is_none(),
        "ended: strip gone"
    );
    assert!(
        vcx.debug_bounds("ingest-loading").is_none(),
        "ended: segment gone"
    );
}

/// The log-level dialog selects a target, then a level, and calls
/// `Diagnostics::request_level`. Escape from the level step returns to target
/// selection.
#[gpui::test]
fn set_log_level_picks_a_target_then_a_level(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "log::level");
    assert!(vcx.debug_bounds("loglevel-choice-list").is_some());
    vcx.simulate_input("ingest");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.modal_open()),
        "step 2 is open"
    );
    assert!(vcx.debug_bounds("loglevel-choice-debug").is_some());
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        "",
        "the field is reset between steps"
    );

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        shell.read_with(&vcx, |s, _| s.modal_open()),
        "back on step 1"
    );
    assert!(vcx.debug_bounds("loglevel-choice-ingest · info").is_some());

    vcx.simulate_input("ingest");
    vcx.simulate_keystrokes("enter");
    vcx.simulate_input("debug");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
    let levels = diagnostics.read_with(&vcx, |d, _| d.levels.clone());
    assert_eq!(
        levels.targets,
        vec![("ingest".to_string(), geode_core::log::Level::DEBUG)]
    );
}

/// The log-level dialog's Back button returns the level step to the targets, as
/// `escape` does, and paints only on the level step. Typing afterwards lands in the
/// focused field and picks a target again.
#[gpui::test]
fn the_back_button_returns_log_levels_to_targets(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "log::level");
    assert!(
        vcx.debug_bounds("shell-modal-back").is_none(),
        "the target step is the first screen"
    );
    vcx.simulate_input("ingest");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("loglevel-choice-debug").is_some());

    let back = vcx
        .debug_bounds("shell-modal-back")
        .expect("the level step paints a Back button");
    vcx.simulate_click(back.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()), "still open");
    assert!(vcx.debug_bounds("loglevel-choice-ingest · info").is_some());
    assert!(vcx.debug_bounds("shell-modal-back").is_none());
    assert_eq!(
        shell.read_with(&vcx, |s, cx| s.dialog_input.read(cx).value().to_string()),
        ""
    );

    vcx.simulate_input("ingest");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("loglevel-choice-debug").is_some(),
        "the field still hears the keyboard"
    );
}

/// Choice dialogs with one step never paint a Back button.
#[gpui::test]
fn one_step_choice_dialogs_have_no_back_button(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = dialog_test_shell(cx, "tile::add");
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
    assert!(
        shell.read_with(&vcx, |s, _| s.choice_dialog.is_some()),
        "fixture: a one-step choice dialog is up"
    );
    assert!(vcx.debug_bounds("shell-modal-back").is_none());
}

/// The reload poll samples and logs process memory on every tick but copies
/// it into diagnostics only while a page watches. Proven through the production
/// timer loop, not by calling `refresh_memory` directly.
#[cfg(any(target_os = "macos", windows))]
#[gpui::test]
fn the_reload_poll_copies_process_memory_only_while_watched(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
    let tick = |vcx: &mut gpui::VisualTestContext| {
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
    };

    // Unwatched: the tick still samples and logs the baseline on
    // `geode::memory`, so the log keeps its record with the page closed.
    let ring = Arc::new(Ring::new(64));
    let sub = {
        use tracing_subscriber::layer::SubscriberExt;
        tracing_subscriber::registry().with(geode_core::log::RingLayer::new(ring.clone()))
    };
    tracing::subscriber::with_default(sub, || tick(&mut vcx));
    let mut records = Vec::new();
    ring.drain_since(0, &mut records);
    assert!(
        records
            .iter()
            .any(|r| r.target == crate::memory::LOG_TARGET && r.level == Level::INFO),
        "the baseline logs with no watcher: {records:?}"
    );
    assert_eq!(
        diagnostics.read_with(&vcx, |d, _| d.memory),
        None,
        "unwatched: sampled and logged, never copied"
    );

    diagnostics.update(&mut vcx, |d, cx| {
        d.watch();
        cx.notify();
    });
    tick(&mut vcx);
    let reading = diagnostics
        .read_with(&vcx, |d, _| d.memory)
        .expect("watched: the next tick copies the reading");
    assert!(reading.current_bytes > 0);
    assert!(reading.peak_bytes >= reading.current_bytes);
}
