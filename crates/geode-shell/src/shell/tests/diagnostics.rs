//! `ShellView`'s wiring of the `Diagnostics` entity (Phase 4b §4.4): the
//! status bar's summary, an `[log]` reload's `LevelControl` apply, and
//! the overlay-toggle drain. The catalog-request drain lives entirely
//! in `geode-app::bridge` (the only crate allowed to touch `geode-data`)
//! and is tested there instead.

use super::*;
use crate::diagnostics::Health;
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
        diagnostics.read_with(&cx, |d, _| d.summary()),
        "sources 1 degraded"
    );
    assert!(
        cx.debug_bounds("diagnostics-summary").is_some(),
        "the status bar shows the diagnostics summary"
    );
}

/// `hot_reload::apply_reload`'s `[log]`-change detection (Phase 4b
/// §4.3): a reload whose `app` doc now carries a different `[log]`
/// table applies it through `LevelControl::set` exactly once, and
/// updates `Diagnostics.levels` to match — never re-persisted (this
/// only ever *applies* what was already on disk).
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
