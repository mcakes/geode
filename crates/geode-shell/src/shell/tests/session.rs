//! Session save/restore wiring: dirty tracking, the debounced write,
//! and restoring tiles with safe ids.

use super::*;
// Explicit: this file's own `mod session;` declaration in `tests/mod.rs`
// shadows the glob-imported `crate::session`, so the bare `session::` paths
// below need this to resolve to the real module rather than to `self`.
use crate::session;

// --- Task 3: session save/restore wiring ----------------------------

pub(super) fn test_services_with_session(session_path: std::path::PathBuf) -> ShellServices {
    let mut services = test_services();
    services.session_path = Some(session_path);
    services
}

/// Fix round 1, Finding 1's regression: a workspace-mutating dispatch
/// must mark the session dirty and return *without* touching the
/// filesystem at all — no synchronous write on the UI thread, however
/// many dispatches fire back to back (this is exactly what OS
/// key-repeat does to `shift+left`, ~20-30 dispatches/sec while held). The
/// file only appears once something actually flushes
/// `take_dirty_session_write`'s pending write — see the end-to-end test
/// below for that half.
#[gpui::test]
fn dispatch_marks_the_session_dirty_without_writing_synchronously(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| {
                    ShellView::new(
                        test_services_with_session(session_path.clone()),
                        None,
                        None,
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

    // Simulate key-repeat: many workspace-mutating dispatches in a row,
    // no flush in between.
    for _ in 0..10 {
        cx.simulate_keystrokes("ctrl-v");
    }

    assert!(
        !session_path.exists(),
        "a dispatch alone must never write the session file synchronously — \
         only a flush (the watcher tick in production, taken directly in \
         tests) does"
    );

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });
    assert!(
        shell.read_with(&cx, |shell, _| shell.session_dirty),
        "a workspace-mutating dispatch must still mark the session dirty"
    );
}

/// End-to-end: real keystrokes dispatched through `ShellView` (not
/// `apply_workspace_action` called directly) build a layout across two
/// workspaces, marking the session dirty on each workspace-mutating
/// dispatch (Task 3 fix round 1: no synchronous write — see the test
/// above). The flush path (`take_dirty_session_write` +
/// `session::write_atomic`, the same two calls the background watcher
/// makes every ~500ms in production) is invoked directly here, since
/// gpui's test executor never advances its simulated clock under
/// `run_until_parked`. Loading the written file back with
/// `session::load` — the exact function `main.rs` calls on startup —
/// must reproduce the same layouts, and a further split on the
/// restored `Workspaces` must allocate a `TileId` that collides with
/// none of the restored ones (the whole reason `Workspaces::from_parts`
/// computes `next_tile` from the restored tiles rather than resetting
/// it to 0).
#[gpui::test]
fn dispatch_saves_the_session_and_it_restores_with_safe_tile_ids(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| {
                    ShellView::new(
                        test_services_with_session(session_path.clone()),
                        None,
                        None,
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

    // Build a layout on workspace 1 (two tiles side by side, then the
    // left one split stacked — three tiles total), switch to workspace
    // 2 and add a tile there too, then land back on workspace 1. Every
    // one of these is a workspace-mutating dispatch, so each marks the
    // session dirty; none of them writes anything by itself.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-h");
    cx.simulate_keystrokes("ctrl-h");
    cx.simulate_keystrokes("alt-2");
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("alt-1");

    assert!(
        !session_path.exists(),
        "no dispatch writes synchronously — the file must not exist before a flush"
    );

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    // Invoke the flush path directly — the same two steps the
    // background watcher's ~500ms tick performs in production.
    let pending = shell.update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx));
    let (path, text) = pending.expect("a dirty session with a configured path must flush");
    session::write_atomic(&path, &text).unwrap();

    assert!(
        session_path.exists(),
        "the flush must have written the file"
    );
    // The atomic-write temp file (now pid+counter-suffixed, fix wave
    // Fix 2) must not be left behind.
    let leftover_tmp_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
        .collect();
    assert!(
        leftover_tmp_files.is_empty(),
        "no *.tmp files should remain in the session directory, found {leftover_tmp_files:?}"
    );
    assert!(
        !shell.read_with(&cx, |shell, _| shell.session_dirty),
        "taking the pending write must clear the dirty flag"
    );

    let live: Vec<(u8, Vec<(crate::tiling::TileId, Rect)>)> = shell.read_with(&cx, |shell, _| {
        shell
            .services
            .workspaces
            .spaces()
            .map(|(ix, ws)| (ix, ws.tree().layout(Rect::UNIT)))
            .collect()
    });

    let session::Restored {
        workspaces: mut restored,
        warnings,
        ..
    } = session::load(&session_path);
    assert!(warnings.is_empty(), "{warnings:?}");

    let restored_layout: Vec<(u8, Vec<(crate::tiling::TileId, Rect)>)> = restored
        .spaces()
        .map(|(ix, ws)| (ix, ws.tree().layout(Rect::UNIT)))
        .collect();
    assert_eq!(
        live, restored_layout,
        "restoring the saved session must reproduce every workspace's layout"
    );

    let before_ids: std::collections::HashSet<_> = restored
        .spaces()
        .flat_map(|(_, t)| t.tree().tiles())
        .collect();
    let new_id = restored.alloc_tile();
    assert!(
        !before_ids.contains(&new_id),
        "alloc_tile on a restored Workspaces must not collide with a restored TileId"
    );
}

/// `take_dirty_session_write` returns `None` (and doesn't panic) when
/// there's nothing dirty, and again on a second call right after a
/// flush — the dirty flag must actually be consumed, not just read.
#[gpui::test]
fn take_dirty_session_write_is_none_when_clean(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| {
                    ShellView::new(
                        test_services_with_session(session_path.clone()),
                        None,
                        None,
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

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    assert!(
        shell
            .update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx))
            .is_none(),
        "nothing dirty yet — no pending write"
    );

    cx.simulate_keystrokes("ctrl-v");
    let first = shell.update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx));
    assert!(
        first.is_some(),
        "the dispatch above must have marked it dirty"
    );

    assert!(
        shell
            .update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx))
            .is_none(),
        "the dirty flag must be consumed by the first take, not left set"
    );
}

/// Fix round 1 (Task 4 review): the headline write trigger —
/// `take_dirty_session_write`'s `tiles == self.last_tiles_written`
/// half of its guard, not just `session_dirty` — was untested. A
/// module state change alone (through the recording module's own
/// `command`, the same path its `serialize` reads back) never touches
/// `session_dirty`, so only that tiles comparison can notice it; this
/// pins that a flush happens exactly once per state change, carrying
/// the new state, and that a further call with nothing new returns
/// `None` again.
#[gpui::test]
fn a_module_state_change_alone_flushes_once_with_the_new_state(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");

    let (mut services, _log) = services_with_recorder();
    services.session_path = Some(session_path);
    let (window, mut vcx) = open_shell(cx, services);
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut vcx);
    let tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });

    // Drain the layout-dirty write the split above queued, so what
    // follows isolates the state-only trigger from the
    // already-covered layout one.
    let layout_flush = shell.update(&mut vcx, |shell, cx| shell.take_dirty_session_write(cx));
    assert!(
        layout_flush.is_some(),
        "the split must have marked the layout dirty"
    );
    assert!(
        shell
            .update(&mut vcx, |shell, cx| shell.take_dirty_session_write(cx))
            .is_none(),
        "nothing changed since that flush — session_dirty is clear and \
         the occupant's state hasn't moved"
    );

    // Mutate the occupant's own state through `command` — never
    // `session_dirty` — exactly the path `serialize` reads back.
    shell.update_in(&mut vcx, |view, window, cx| {
        let o = view.occupants.get(&tile).expect("the split created a tile");
        o.content
            .command("state changed", window, cx)
            .expect("the recorder's command always succeeds");
    });

    let state_flush = shell.update(&mut vcx, |shell, cx| shell.take_dirty_session_write(cx));
    let (_, text) = state_flush.expect(
        "a state-only change must still flush — `session_dirty` alone \
         would miss it, which is exactly what this test guards",
    );
    assert!(
        text.contains("last_command = \"state changed\""),
        "the flushed text must carry the new state: {text}"
    );

    assert!(
        shell
            .update(&mut vcx, |shell, cx| shell.take_dirty_session_write(cx))
            .is_none(),
        "the state hasn't changed again since the flush above"
    );
}

/// Task 4 (Phase 3 §3.5), two halves of the same contract:
/// `current_tiles` reports a live occupant's own kind and whatever its
/// `serialize` returns, and a `restored_tiles` record for a tile that
/// really is in the restored `Workspaces` reaches that tile's factory
/// as `Some(state)` when `ensure_occupants` creates it.
#[gpui::test]
fn current_tiles_reflects_live_occupants_and_restored_state_reaches_the_factory(
    cx: &mut gpui::TestAppContext,
) {
    // Half 1: a freshly created occupant shows up in `current_tiles`
    // under its own kind.
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut vcx);
    let tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    let tiles = shell.read_with(&vcx, |s, cx| s.current_tiles(cx));
    assert_eq!(
        tiles.get(&tile.0).map(|r| r.kind.as_str()),
        Some("rec"),
        "{tiles:?}"
    );

    // Half 2: a hand-built session table restoring one tile (id 1)
    // with a `tiles` record naming the recorder's own kind — built
    // through `session::from_toml`, exactly as `main.rs` restores a
    // real session file — must have its `state` handed to the
    // recorder's `create` as `Some(...)`.
    let mut table = session::to_toml(
        &Workspaces::new(),
        &session::TileRecords::new(),
        None,
        &crate::palette_usage::PaletteUsage::new(),
    );
    let ws1: toml::Table = r#"
        focused = 1
        [node]
        kind = "leaf"
        id = 1
        [tiles.1]
        module = "rec"
        [tiles.1.state]
        last_command = "hello"
    "#
    .parse()
    .unwrap();
    if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
        ws_table.insert("1".to_string(), toml::Value::Table(ws1));
    }
    let restored = session::from_toml(&table).unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let expected_state = restored.tiles.get(&1).unwrap().state.clone();

    let (mut services2, log2) = services_with_recorder();
    services2.workspaces = restored.workspaces;
    services2.restored_tiles = restored.tiles;
    let (_window2, mut vcx2) = open_shell(cx, services2);
    vcx2.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        log2.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Created(TileId(1), Some(state))
                if *state == expected_state
        )),
        "{:?}",
        log2.borrow()
    );
}

/// End-to-end (design doc, "Tests"): a theme pick through the settings
/// dialog's own apply seam, with a real `user_dir` wired up (a tempdir,
/// exactly like `build_shell_services` wires the real
/// `%APPDATA%`/`$HOME/.config` dir in `main.rs`), must leave the new
/// name written into `<user_dir>/app.toml`'s `[theme]` table on disk —
/// the whole point of the apply-then-persist seam
/// (`ShellView::persist_theme`) replacing the removed session
/// `theme_mode` mechanism. (Until 2026-09-12 this was a `mod+shift+t`
/// keystroke flipping a mode; the chord and the mode are both retired.)
#[gpui::test]
fn a_theme_pick_persists_the_new_name_to_the_user_config_file(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

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

    assert!(
        !user_dir.join("app.toml").exists(),
        "sanity: nothing written before the pick"
    );

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });
    cx.update(|_window, cx| settings_view::set_theme(&shell, "Gruvbox Light", cx));

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    // `persist_theme` (Finding 1, review fix round 1) hands the actual
    // file write to `cx.background_executor()` rather than running it
    // inline, so the file doesn't necessarily exist the instant the
    // keystroke's synchronous dispatch returns — `run_until_parked`
    // drives that detached background task to completion, same pattern
    // the gpui testing reference uses for any detached background/async
    // work.
    cx.run_until_parked();

    assert_eq!(
        shell.read_with(&cx, |shell, _| shell
            .services
            .theme
            .active_name()
            .to_string()),
        "Gruvbox Light",
        "sanity: the pick applied live"
    );
    let text = std::fs::read_to_string(user_dir.join("app.toml"))
        .expect("the pick must have written app.toml");
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(
        doc["theme"]["name"].as_str(),
        Some("Gruvbox Light"),
        "the persisted [theme].name must be the theme the pick applied"
    );
    assert!(
        doc["theme"].get("mode").is_none(),
        "no mode key: the light/dark axis is retired"
    );
}

// --- Phase 4a: [frame] restore --------------------------------------

/// `ShellServices::restored_frame` (Phase 4a §3.6) is applied to the
/// just-built frame — scope and as-of land on it directly, with no
/// leftover undo entry back to the empty scope nobody chose — and the
/// first watcher-tick flush after that writes it straight back out to
/// `[frame]`.
#[gpui::test]
fn a_restored_frame_applies_to_the_frame_with_clean_history_and_the_first_flush_writes_it_back(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);

    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");

    let record = session::FrameRecord {
        scope: geode_core::scope::Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "book".into(),
                values: vec!["BK001".into()],
            }],
            ..geode_core::scope::Scope::default()
        },
        active_slot: None,
        as_of: geode_core::query::AsOf::At(chrono::Utc::now()),
    };

    let mut services = test_services_with_session(session_path.clone());
    services.restored_frame = Some(record.clone());

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

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });

    let (scope, as_of) = shell.read_with(&cx, |shell, cx| {
        let frame = shell.frame().read(cx);
        (frame.scope().clone(), frame.as_of().clone())
    });
    assert_eq!(scope, record.scope, "the restored scope must apply");
    assert_eq!(as_of, record.as_of, "the restored as-of must apply");

    let undid = shell.update(&mut cx, |shell, cx| {
        shell.frame().update(cx, |f, _| f.undo_scope())
    });
    assert!(
        !undid,
        "clear_history must leave no phantom undo entry from applying the restore"
    );

    let pending = shell.update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx));
    let (path, text) = pending.expect("the first tick after a restore must flush [frame]");
    session::write_atomic(&path, &text).unwrap();

    let restored = session::load(&session_path);
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    assert_eq!(restored.frame, Some(record));
}

#[gpui::test]
fn rapid_settings_changes_persist_in_order_without_losing_other_keys(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);

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

    assert!(
        !user_dir.join("app.toml").exists(),
        "sanity: nothing written before the pick"
    );

    let root = window.root(&mut cx).unwrap();
    let shell = root.read_with(&cx, |root, _cx| {
        root.view()
            .clone()
            .downcast::<ShellView>()
            .unwrap_or_else(|_| panic!("root view is not a ShellView"))
    });
    // All actions happen in one UI turn, before background tasks can run.
    cx.update(|_window, cx| {
        settings_view::set_theme(&shell, "Ayu Dark", cx);
        settings_view::set_font_size(&shell, FontSize::Large, cx);
        settings_view::set_theme(&shell, "Gruvbox Light", cx);
        settings_view::set_find_style(&shell, FindStyle::Fzf, cx);
        settings_view::set_font_size(&shell, FontSize::Small, cx);
        shell.update(cx, |shell, cx| {
            shell.set_line_numbers(crate::linenumbers::LineNumbers::Relative, cx);
            shell.set_default_source(Some("demo_kdb".into()), cx);
            shell.set_default_source(None, cx);
        });
    });
    cx.run_until_parked();
    let text = std::fs::read_to_string(user_dir.join("app.toml")).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(doc["theme"]["name"].as_str(), Some("Gruvbox Light"));
    assert_eq!(doc["ui"]["font_size"].as_str(), Some("small"));
    assert_eq!(doc["ui"]["find_style"].as_str(), Some("fzf"));
    assert_eq!(doc["ui"]["line_numbers"].as_str(), Some("rel"));
    assert!(doc["timeseries"].get("default_source").is_none());
}
