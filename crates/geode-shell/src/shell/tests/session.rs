//! Session save/restore wiring: periodic snapshot extraction, persistence,
//! and restoring tiles with safe IDs.

use super::*;
// Explicit: this file's own `mod session;` declaration in `tests/mod.rs`
// shadows the glob-imported `crate::session`, so the bare `session::` paths
// below need this to resolve to the real module rather than to `self`.
use crate::session;

// --- Session save/restore wiring -----------------------------------

/// Two windows over one `ShellApp` read one `Workspaces`: a tile allocated
/// through the first view's writer is visible to the second view's reader,
/// and the second allocation continues the same counter. Each view keeps
/// its own window key and shows its own workspace: the first opens on the
/// session's initial index, the second on the lowest index nobody shows.
#[gpui::test]
fn two_views_over_one_app_read_the_same_workspaces(cx: &mut gpui::TestAppContext) {
    let app = test_app(cx, test_services(), None);
    let (w1, mut vcx) = open_shell_on(cx, app.clone());
    let (w2, _) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx);
    let s2 = shell_of(&w2, &mut vcx);
    vcx.update(|_, cx| {
        let first = s1.update(cx, |s, cx| s.with_workspaces(cx, |w| w.alloc_tile()));
        let second = s2.update(cx, |s, cx| s.with_workspaces(cx, |w| w.alloc_tile()));
        assert_eq!(second.0, first.0 + 1);
        assert_ne!(s1.read(cx).key(), s2.read(cx).key());
        assert_eq!(s1.read(cx).shown_ix(cx), WorkspaceIx::new(1).unwrap());
        assert_eq!(s2.read(cx).shown_ix(cx), WorkspaceIx::new(2).unwrap());
        assert_eq!(
            ws(&s2, cx).spaces().count(),
            2,
            "the second window materialised the workspace it opened on"
        );
        assert_eq!(
            ws(&s1, cx).host_of(WorkspaceIx::new(2).unwrap()),
            Some(s2.read(cx).key()),
            "a later window hosts only its own index"
        );
    });
}

/// Two windows over one `ShellApp` hold one `Frame`: the app creates it
/// and applies the session's restored lanes once, so a pin restored from
/// `[workspaces.1.frame]` (the fixture's one workspace; a pin on an absent
/// workspace is ignored) is visible through the second view as well.
#[gpui::test]
fn two_views_share_one_frame_and_a_restored_pin(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    services.restored_pinned.insert(
        WorkspaceIx::new(1).unwrap(),
        session::FrameRecord {
            scope: geode_core::scope::Scope::default(),
            active_slot: None,
            ad_hoc: None,
            ad_hoc_active: false,
            loaded_from: None,
            as_of: geode_core::query::AsOf::Live,
        },
    );
    let app = test_app(cx, services, None);
    let (w1, mut vcx) = open_shell_on(cx, app.clone());
    let (w2, _) = open_shell_on(cx, app.clone());
    let (s1, s2) = (shell_of(&w1, &mut vcx), shell_of(&w2, &mut vcx));
    vcx.update(|_, cx| {
        assert_eq!(
            s1.read(cx).frame().entity_id(),
            s2.read(cx).frame().entity_id(),
            "two views, two frames"
        );
        assert!(
            s1.read(cx)
                .frame()
                .read(cx)
                .is_pinned(WorkspaceIx::new(1).unwrap())
        );
    });
}

pub(super) fn test_services_with_session(session_path: std::path::PathBuf) -> ShellServices {
    let mut services = test_services();
    services.session_path = Some(session_path);
    services
}

/// Workspace-mutating dispatch marks the session dirty without writing to
/// disk. Repeated dispatches coalesce until the periodic flush extracts a
/// snapshot, keeping filesystem work off the input path.
#[gpui::test]
fn dispatch_marks_the_session_dirty_without_writing_synchronously(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = view_for_test(
                    test_services_with_session(session_path.clone()),
                    None,
                    window,
                    cx,
                );
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
        shell.read_with(&cx, |shell, cx| shell.app().read(cx).session_dirty),
        "a workspace-mutating dispatch must still mark the session dirty"
    );
}

/// Real shell keystrokes build layouts across two workspaces. Extract and
/// write the pending snapshot directly, then load it through the startup
/// reader. Restored layouts must match, and subsequent allocation must use
/// IDs beyond the restored tiles. This drives the flush body, not its timer.
#[gpui::test]
fn dispatch_saves_the_session_and_it_restores_with_safe_tile_ids(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.toml");

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = view_for_test(
                    test_services_with_session(session_path.clone()),
                    None,
                    window,
                    cx,
                );
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
    let pending = shell.take_dirty_session_write(&mut cx);
    let (path, text) = pending.expect("a dirty session with a configured path must flush");
    session::write_atomic(&path, &text).unwrap();

    assert!(
        session_path.exists(),
        "the flush must have written the file"
    );
    // Successful replacement consumes its temporary file.
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
        !shell.read_with(&cx, |shell, cx| shell.app().read(cx).session_dirty),
        "taking the pending write must clear the dirty flag"
    );

    let live: Vec<(u8, Vec<(crate::tiling::TileId, Rect)>)> = shell.read_with(&cx, |shell, cx| {
        shell
            .workspaces(cx)
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
                let view = view_for_test(
                    test_services_with_session(session_path.clone()),
                    None,
                    window,
                    cx,
                );
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
        shell.take_dirty_session_write(&mut cx).is_none(),
        "nothing dirty yet — no pending write"
    );

    cx.simulate_keystrokes("ctrl-v");
    let first = shell.take_dirty_session_write(&mut cx);
    assert!(
        first.is_some(),
        "the dispatch above must have marked it dirty"
    );

    assert!(
        shell.take_dirty_session_write(&mut cx).is_none(),
        "the dirty flag must be consumed by the first take, not left set"
    );
}

/// A module-only state change triggers one new snapshot without setting
/// layout dirt. Repeated extraction without another change returns `None`.
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
    let tile = shell.read_with(&vcx, |s, cx| s.workspace(cx).focused_tile().unwrap());

    // Drain the layout-dirty write the split above queued, so what
    // follows isolates the state-only trigger from the
    // already-covered layout one.
    let layout_flush = shell.take_dirty_session_write(&mut vcx);
    assert!(
        layout_flush.is_some(),
        "the split must have marked the layout dirty"
    );
    assert!(
        shell.take_dirty_session_write(&mut vcx).is_none(),
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

    let state_flush = shell.take_dirty_session_write(&mut vcx);
    let (_, text) = state_flush.expect(
        "a state-only change must still flush — `session_dirty` alone \
         would miss it, which is exactly what this test guards",
    );
    assert!(
        text.contains("last_command = \"state changed\""),
        "the flushed text must carry the new state: {text}"
    );

    assert!(
        shell.take_dirty_session_write(&mut vcx).is_none(),
        "the state hasn't changed again since the flush above"
    );
}

/// Live occupants contribute their own kind and serialized state. Restored
/// records for layout tiles reach the matching factory as `Some(state)`.
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
    let tile = shell.read_with(&vcx, |s, cx| s.workspace(cx).focused_tile().unwrap());
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
        &crate::session::PinnedRecords::new(),
        &crate::palette_usage::PaletteUsage::new(),
        &crate::session::PageRecords::new(),
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

/// Applying a theme through settings persists its name to the user-layer
/// `app.toml` theme table, independently of session persistence.
#[gpui::test]
fn a_theme_pick_persists_the_new_name_to_the_user_config_file(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);

    let dir = tempfile::tempdir().unwrap();
    let user_dir = dir.path().to_path_buf();

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = view_for_test(test_services(), Some(user_dir.clone()), window, cx);
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

    // Drive the detached background persistence task before reading the file.
    cx.run_until_parked();

    assert_eq!(
        shell.read_with(&cx, |shell, cx| shell
            .services(cx)
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

// --- Frame restoration --------------------------------------------

/// Restore frame scope and as-of without an undo step back to empty scope.
/// The next extracted session snapshot must carry those values in `[frame]`.
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
        ad_hoc: None,
        ad_hoc_active: false,
        loaded_from: None,
        as_of: geode_core::query::AsOf::At(chrono::Utc::now()),
    };

    let mut services = test_services_with_session(session_path.clone());
    services.restored_frame = Some(record.clone());

    let window = cx
        .update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let view = view_for_test(services, None, window, cx);
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
        (
            frame.shared().scope().clone(),
            frame.shared().as_of().clone(),
        )
    });
    assert_eq!(scope, record.scope, "the restored scope must apply");
    assert_eq!(as_of, record.as_of, "the restored as-of must apply");

    let undid = shell.update(&mut cx, |shell, cx| {
        shell.frame().update(cx, |f, _| f.shared_mut().undo_scope())
    });
    assert!(
        !undid,
        "clear_history must leave no phantom undo entry from applying the restore"
    );

    let pending = shell.take_dirty_session_write(&mut cx);
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
                let view = view_for_test(test_services(), Some(user_dir.clone()), window, cx);
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

/// One writer for every window: a save made through the app carries the
/// live tile of the window that hosts it and a parked record for a tile
/// no window has an occupant for (here: a tile in workspace 3's tree,
/// which workspace 1 never paints). The record sits in
/// `ShellApp::parked_tiles`; the save reaches it through the union of
/// every live view's records and the parked ones.
#[gpui::test]
fn a_session_save_includes_every_window_and_the_parked_records(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut services = services_with_recorder().0;
    services.session_path = Some(dir.path().join("session.toml"));
    let app = test_app(cx, services, None);
    let (w1, mut vcx) = open_shell_on(cx, app.clone());
    let s1 = shell_of(&w1, &mut vcx);
    // One live tile in window 1.
    super::occupants::dispatch_and_draw(&s1, &mut vcx, "tile::add_rec");
    let live = s1.read_with(&vcx, |s, cx| s.workspace(cx).focused_tile().unwrap());
    // A tile placed in workspace 3's tree, with a parked record for it:
    // the writer emits only tiles that some tree holds.
    let parked = vcx.update(|_, cx| {
        s1.update(cx, |s, cx| {
            s.with_workspaces(cx, |w| {
                w.switch(s.key(), 3);
                let id = w.split_in(s.key(), crate::tiling::Orientation::Horizontal);
                w.switch(s.key(), 1);
                id
            })
        })
    });
    vcx.update(|_, cx| {
        app.update(cx, |a, _| {
            a.parked_tiles.insert(
                parked.0,
                session::TileRecord {
                    kind: "rec".into(),
                    state: toml::Table::new(),
                    link: geode_core::link::Membership::default(),
                },
            );
        })
    });
    vcx.update(|_, cx| app.read(cx).save_session(cx));
    let text = std::fs::read_to_string(dir.path().join("session.toml")).unwrap();
    assert!(
        text.contains(&format!("[workspaces.1.tiles.{}]", live.0)),
        "the live occupant's record: {text}"
    );
    assert!(
        text.contains(&format!("tiles.{}]", parked.0)),
        "the parked record: {text}"
    );
}

/// With every window closed and its view dropped, `save_session` writes
/// nothing: gpui removes a closing window before the quit hooks run, and a
/// save then would write the layout without the closed window's tile
/// records over the should-close hook's save. The sanity save before the
/// close proves the path is configured and the file is the guard's doing.
#[gpui::test]
fn save_session_writes_nothing_with_every_window_closed(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.toml");
    let mut services = test_services();
    services.session_path = Some(path.clone());
    let app = test_app(cx, services, None);
    let (_window, mut vcx) = open_shell_on(cx, app.clone());
    vcx.update(|_, cx| app.read(cx).save_session(cx));
    assert!(path.exists(), "sanity: a save with the window open writes");
    std::fs::remove_file(&path).unwrap();

    // Nothing here holds the view: the window was its only owner.
    vcx.update(|window, _| window.remove_window());
    vcx.run_until_parked();
    assert!(
        app.read_with(&vcx, |a, _| a.views().next().is_none()),
        "sanity: the closed window's view is gone"
    );
    app.read_with(&vcx, |a, cx| a.save_session(cx));
    assert!(
        !path.exists(),
        "a save with no live view wrote: {}",
        std::fs::read_to_string(&path).unwrap_or_default()
    );
}
