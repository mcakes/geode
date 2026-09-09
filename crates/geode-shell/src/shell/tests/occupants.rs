//! Occupant lifecycle: restoring, filling, ensuring, and the module
//! hosting contract's visibility handshake.

use super::*;
// Explicit: `tests`'s own `mod session;` declaration shadows the
// glob-imported `crate::session`, so the bare `session::` paths below need
// this to resolve to the real module rather than to the sibling test file.
use crate::session;

/// A minimal `ModuleFactory` that calls `Diagnostics::watch`/`unwatch`
/// from `set_visible` — the same thing `geode_diagnostics::DiagnosticsTile`
/// does (a crate `geode-shell` cannot depend on: layering), modelling the
/// generic occupant-lifecycle contract MAJ-2 is about (`ensure_occupants`
/// must tell a vanished tile's occupant it is hidden before dropping it)
/// without needing the real module.
mod watching {
    use super::*;
    use crate::keymap::KeyContext;
    use crate::module::{FindEvent, ModuleFactory, TileContent, TileOccupant};
    use geode_core::query::QueryOutcome;
    use gpui::{App, Context, FocusHandle, Render, div};

    pub const WATCHING_KIND: &str = "watching";

    pub struct WatchingFactory;

    struct WatchingView {
        tile: TileId,
        focus: FocusHandle,
    }
    impl Render for WatchingView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .debug_selector(|| format!("tile-content-{}", self.tile.0))
        }
    }

    struct WatchingContent {
        diagnostics: Entity<Diagnostics>,
        // Mirrors `DiagnosticsTile::visible`'s own de-dup guard: `ensure_
        // occupants` tells a freshly created, active occupant `true`
        // twice (I2, final review — "harmless" for a private bool flip,
        // but `Diagnostics::watch` is a real counter, so a real occupant
        // must not double-count it either).
        visible: std::cell::Cell<bool>,
    }
    impl TileContent for WatchingContent {
        fn key_context(&self, _cx: &App) -> KeyContext {
            KeyContext::new(WATCHING_KIND)
        }
        fn dispatch(&self, _: &ActionId, _: Option<u32>, _: &mut Window, _: &mut App) -> bool {
            false
        }
        fn command(&self, _: &str, _: &mut Window, _: &mut App) -> Result<(), String> {
            Err("no commands".into())
        }
        fn completions(&self, _: &str, _: usize, _: &App) -> Vec<String> {
            Vec::new()
        }
        fn find(&self, _: FindEvent, _: &mut Window, _: &mut App) {}
        fn deliver(&self, _: QueryOutcome, _: &mut Window, _: &mut App) {}
        fn set_visible(&self, visible: bool, cx: &mut App) {
            if self.visible.get() == visible {
                return;
            }
            self.visible.set(visible);
            self.diagnostics.update(cx, |d, cx| {
                if visible {
                    d.watch();
                } else {
                    d.unwatch();
                }
                cx.notify();
            });
        }
        fn serialize(&self, _: &App) -> toml::Table {
            toml::Table::new()
        }
    }

    impl ModuleFactory for WatchingFactory {
        fn kind(&self) -> &'static str {
            WATCHING_KIND
        }
        fn register_actions(&self, _: &mut ActionRegistry) {}
        fn create(
            &self,
            tile: TileId,
            _restored: Option<&toml::Table>,
            _frame: Entity<Frame>,
            diagnostics: Entity<Diagnostics>,
            _window: &mut Window,
            cx: &mut App,
        ) -> TileOccupant {
            let focus = cx.focus_handle();
            let view = cx.new(|_| WatchingView { tile, focus });
            TileOccupant {
                kind: WATCHING_KIND,
                view: view.into(),
                content: Box::new(WatchingContent {
                    diagnostics,
                    visible: std::cell::Cell::new(false),
                }),
            }
        }
    }
}

/// MAJ-2 (Phase 4b Task 5 fix round 1): closing a tile must tell its
/// occupant it went invisible before dropping it, so an occupant that
/// opened something in `set_visible(true)` (`Diagnostics::watch`, here)
/// gets the matching `unwatch` rather than leaking a watcher forever.
#[gpui::test]
fn closing_a_watching_tile_unwatches_the_diagnostics_entity(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let mut roster = crate::module::ModuleRoster::new("watching");
    roster.add(Box::new(watching::WatchingFactory));
    roster.register_actions(&mut services.registry);
    let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
    let (keymap, diags) = build_keymap(&[doc], default_mod(), &services.registry);
    assert!(diags.is_empty(), "{diags:?}");
    services.keymap = keymap;
    services.roster = roster;

    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let diagnostics = shell.read_with(&cx, |s, _| s.diagnostics().clone());
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some(watching::WATCHING_KIND)
    );
    assert_eq!(
        diagnostics.read_with(&cx, |d, _| d.watchers()),
        1,
        "the freshly opened, visible tile watches"
    );

    cx.simulate_keystrokes("ctrl-w");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        None,
        "the occupant is gone"
    );
    assert_eq!(
        diagnostics.read_with(&cx, |d, _| d.watchers()),
        0,
        "closing the tile must unwatch, not leak the watcher"
    );
}

#[gpui::test]
fn a_restored_tile_of_an_unknown_kind_falls_back_without_its_state(cx: &mut gpui::TestAppContext) {
    // The record names a kind nothing in the roster registers, so
    // `ensure_occupants` falls back to the roster's default ("rec")
    // — but the fallback factory did not produce that state and must
    // not be handed it (fix-round finding).
    let (mut services, log) = services_with_recorder();
    let mut state = toml::Table::new();
    state.insert(
        "last_command".into(),
        toml::Value::String("state for a different module".into()),
    );
    services.restored_tiles.insert(
        1,
        crate::session::TileRecord {
            kind: "unregistered-kind".into(),
            state,
        },
    );
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(tile, TileId(1), "the first split always allocates tile 1");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec"),
        "the default factory still hosts the tile"
    );
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)
        ),
        "the fallback factory got no state: {:?}",
        log.borrow()
    );
}

/// I2, final review: `fill_all_tiles` walks every workspace, so the
/// FIRST render creates an occupant for a tile in an inactive
/// workspace too — restored here via `session::from_toml`, the same
/// real path `current_tiles_reflects_live_occupants_and_restored_
/// state_reaches_the_factory` above builds. Before the fix, only
/// tiles in the *active* set ever got a `set_visible` call at all;
/// an occupant created outside it heard nothing, ever. `RecordingFactory`
/// does not default a fresh occupant to anything — the assertion
/// below is only meaningful because `set_visible` is required to be
/// called at creation time, per its own doc comment's contract.
#[gpui::test]
fn an_occupant_created_outside_the_active_workspace_is_told_it_is_hidden(
    cx: &mut gpui::TestAppContext,
) {
    let mut table = session::to_toml(&Workspaces::new(), &session::TileRecords::new(), None);
    // Workspace 1 (the default active one) stays empty. Workspace 2
    // gets one tile, restored with the recorder's own kind — this is
    // the occupant that is created on the very first render while
    // workspace 1, not 2, is active.
    let ws2: toml::Table = r#"
        focused = 1
        [node]
        kind = "leaf"
        id = 1
        [tiles.1]
        module = "rec"
    "#
    .parse()
    .unwrap();
    if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
        ws_table.insert("2".to_string(), toml::Value::Table(ws2));
    }
    let restored = session::from_toml(&table).unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    assert_eq!(
        restored.workspaces.active_index(),
        1,
        "sanity: workspace 1, not 2, is active"
    );

    let (mut services, log) = services_with_recorder();
    services.workspaces = restored.workspaces;
    services.restored_tiles = restored.tiles;
    let (_window, mut cx) = open_shell(cx, services);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Visible(TileId(1), false)
        )),
        "an occupant created outside the active workspace must be told \
         it is hidden on its first render: {:?}",
        log.borrow()
    );
    assert!(
        log.borrow().iter().all(|r| !matches!(
            r,
            crate::module::recording::Recorded::Visible(TileId(1), true)
        )),
        "it must never have been told the opposite: {:?}",
        log.borrow()
    );
}

#[gpui::test]
fn a_split_creates_an_occupant_of_the_default_kind_and_paints_it(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)
        ),
        "{:?}",
        log.borrow()
    );
    // `debug_bounds` takes `&'static str`; leak the dynamic selector
    // (test-only, a few bytes).
    let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
    let bounds = cx.debug_bounds(selector);
    assert!(
        bounds.is_some_and(|b| b.size.width > px(0.0)),
        "the occupant's view painted: {bounds:?}"
    );
}

#[gpui::test]
fn a_key_in_the_occupants_context_reaches_its_dispatch_with_the_count(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("4 j");
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert!(
        log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(t, a, Some(4)) if *t == tile && a.0 == "rec::noop"
        )),
        "{:?}",
        log.borrow()
    );
}

#[gpui::test]
fn closing_a_tile_drops_its_occupant_and_switching_workspaces_toggles_visibility(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });

    cx.simulate_keystrokes("alt-2");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Visible(t, false) if *t == tile)
        ),
        "hidden on switch: {:?}",
        log.borrow()
    );
    cx.simulate_keystrokes("alt-1");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Visible(t, true) if *t == tile)
        ),
        "shown on return: {:?}",
        log.borrow()
    );

    cx.simulate_keystrokes("ctrl-w");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        None,
        "occupant dropped with its tile"
    );
}

#[gpui::test]
fn a_click_on_a_tile_leaves_the_shell_focused_on_the_next_frame(cx: &mut gpui::TestAppContext) {
    // gpui focuses a tracked element on mouse down; an occupant that
    // tracks its own handle (DataTable does) would take focus with it
    // and every shell chord would go dead. The tile's click handler
    // arms the same restore `apply_reload` uses (§3.3).
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    // `debug_bounds` takes `&'static str`; leak the dynamic selector
    // (test-only, a few bytes).
    let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
    let bounds = cx.debug_bounds(selector).unwrap();
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
    let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| focused.is_focused(window)),
        "the shell root has focus again"
    );
}

#[gpui::test]
fn a_click_on_a_docked_tile_leaves_the_shell_focused_on_the_next_frame(
    cx: &mut gpui::TestAppContext,
) {
    // Same hazard as the tree-tile test above, but for a tile parked
    // in a dock (fix-round finding: the dock-tile `on_mouse_down`
    // listener did not re-arm `pending_focus_restore`, so a
    // focus-tracking occupant docked instead of tiled would leave
    // shell chords dead after a click).
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-{");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    // `debug_bounds` takes `&'static str`; leak the dynamic selector
    // (test-only, a few bytes).
    let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
    let bounds = cx.debug_bounds(selector).unwrap();
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
    let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| focused.is_focused(window)),
        "the shell root has focus again"
    );
}

// The status bar's diagnostics-summary indicator (Phase 3 §5.1's
// data-status contract, replaced by Phase 4b's `Diagnostics` entity) is
// covered in `shell/tests/diagnostics.rs` now — `set_data_status` no
// longer exists; `Diagnostics::note_health` is the door.

/// `open_module` (Phase 4b Task 5): the first call splits a fresh tile and
/// hands it to the requested kind's factory; a second call with nothing
/// else changed must focus that same tile rather than splitting again —
/// "opening by kind" means at most one occupant of that kind per
/// workspace, focused, not one per press.
#[gpui::test]
fn open_module_twice_yields_one_tile_of_that_kind_focused(cx: &mut gpui::TestAppContext) {
    let (mut services, log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new(
            "diagnostics",
        )));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    cx.simulate_keystrokes("alt-shift-d");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let first_tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(first_tile)),
        Some("diagnostics")
    );

    // Focus something else, then ask for diagnostics again — it must
    // focus the tile that already exists rather than splitting a second
    // one.
    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let second_split_tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_ne!(second_split_tile, first_tile);

    cx.simulate_keystrokes("alt-shift-d");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let refocused = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        refocused, first_tile,
        "the second open_module call must focus the existing diagnostics \
         tile, not create another"
    );
    let diagnostics_tiles: Vec<TileId> = shell
        .read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles())
        .into_iter()
        .filter(|id| shell.read_with(&cx, |s, _| s.occupant_kind(*id)) == Some("diagnostics"))
        .collect();
    assert_eq!(
        diagnostics_tiles.len(),
        1,
        "exactly one diagnostics tile ever exists: {:?}",
        log.borrow()
    );
}

/// MIN-7 (Phase 4b Task 5 fix round 1), carried forward to the addressed
/// pending map (spec 2026-09-08 add-tile §4.3): two `open_module` calls
/// for the same kind within one render (a double `mod+shift+d` press, or
/// key-repeat) both miss the "existing occupant" search (the first
/// call's new tile has no occupant yet; `ensure_occupants` only creates
/// one at the top of the *next* render), so without the pending-kind
/// guard the second call would add a second tile and leave one hosting
/// the requested kind and a stray one hosting the roster's default. Two
/// presses with no render between them must still yield exactly one add,
/// one tile.
#[gpui::test]
fn two_open_module_calls_for_the_same_kind_before_any_render_add_only_once(
    cx: &mut gpui::TestAppContext,
) {
    let (mut services, _log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new(
            "diagnostics",
        )));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    // Both calls inside ONE `cx.update` block: gpui flushes effects (and
    // so redraws a dirty window) at the end of every `update`, so two
    // `simulate_keystrokes` presses would render in between and prove
    // nothing — the guard under test only fires while a request is still
    // pending.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_module("diagnostics", window, cx);
            s.open_module("diagnostics", window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(
        tiles.len(),
        1,
        "two open_module('diagnostics') calls with no render between them \
         must add only once: {tiles:?}"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tiles[0])),
        Some("diagnostics")
    );
}

/// The other half of `open_module`'s contract: a kind with no registered
/// factory (`services_with_recorder`'s roster only knows "rec") falls back
/// to the roster's default kind rather than leaving the split tile
/// occupant-less, and logs a warning naming the requested kind.
#[gpui::test]
fn open_module_with_no_matching_factory_falls_back_to_the_default_kind_and_warns(
    cx: &mut gpui::TestAppContext,
) {
    use geode_core::log::{Ring, RingLayer};
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;

    let ring = Arc::new(Ring::new(64));
    let sub = tracing_subscriber::registry().with(RingLayer::new(ring.clone()));

    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    tracing::subscriber::with_default(sub, || {
        cx.update(|window, cx| {
            shell.update(cx, |s, cx| {
                s.open_module("diagnostics", window, cx);
            });
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    });

    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec"),
        "no 'diagnostics' factory is registered, so the default kind hosts the tile"
    );

    let mut records = Vec::new();
    ring.drain_since(0, &mut records);
    assert!(
        records
            .iter()
            .any(|r| r.level == tracing::Level::WARN && r.message.contains("diagnostics")),
        "expected a warning naming the unmatched kind: {records:?}"
    );
}

/// Dispatch an action id straight into the shell and draw once — the
/// add rows are palette rows, and `dispatch` is exactly what a palette
/// `enter` calls (`palette_ctl::dispatch_palette_item`).
fn dispatch_and_draw(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext, id: &str) {
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(&ActionId(id.to_string()), None, window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn tile_rects(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> Vec<(TileId, Rect)> {
    shell.read_with(cx, |s, _| {
        s.services.workspaces.active().tree().layout(Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 1000.0,
        })
    })
}

#[gpui::test]
fn add_on_an_empty_workspace_creates_the_root_tile_of_that_kind(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 1);
    let tile = tiles[0];
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile()),
        Some(tile)
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)
        )
    );
}

#[gpui::test]
fn add_on_a_placeholder_tile_fills_it_in_place(cx: &mut gpui::TestAppContext) {
    // `test_services` has an empty roster, so the first tile is a
    // placeholder; a recorder added afterwards is what "Add Rec" fills
    // it with.
    let mut services = test_services();
    let rec = crate::module::recording::RecordingFactory::new("rec");
    let log = rec.log.clone();
    services.roster.add(Box::new(rec));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(&ActionId("workspace::split_right".into()), None, window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some(crate::module::placeholder::PLACEHOLDER_KIND)
    );

    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(
        tiles,
        vec![tile],
        "no split: the placeholder's own tile was filled"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)
        )
    );
}

#[gpui::test]
fn an_explicit_direction_beats_the_setting_and_lands_where_it_says(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let first = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });

    dispatch_and_draw(&shell, &mut cx, "tile::add_rec_vertical");
    let below = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    let rects = tile_rects(&shell, &mut cx);
    let r = |id| rects.iter().find(|(t, _)| *t == id).unwrap().1;
    assert!(
        r(below).y > r(first).y && (r(below).x - r(first).x).abs() < 1e-3,
        "{rects:?}"
    );

    dispatch_and_draw(&shell, &mut cx, "tile::add_rec_horizontal");
    let right = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    let rects = tile_rects(&shell, &mut cx);
    let r = |id| rects.iter().find(|(t, _)| *t == id).unwrap().1;
    assert!(
        r(right).x > r(below).x && (r(right).y - r(below).y).abs() < 1e-3,
        "{rects:?}"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(right)),
        Some("rec")
    );
}

#[gpui::test]
fn auto_splits_a_wide_tile_to_the_right_and_a_tall_one_below(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    // 1500×700 viewport → a 1460×640 content area (sidebar 40, toolbar
    // ~34, status 26): the first tile is wide → the second lands to the
    // right; each half (730×640) is still wide → the third lands right
    // again (equalised thirds, 487×640); a third is taller than wide →
    // the fourth lands below it. Every comparison has >100px of margin.
    cx.simulate_resize(gpui::size(px(1500.0), px(700.0)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let shell = shell_of(&window, &mut cx);
    let viewport = cx.update(|window, _| window.viewport_size());
    assert_eq!(viewport.width, px(1500.0), "sanity: the resize took");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.add_direction),
        crate::tileadd::AddDirection::Auto
    );
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let a = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let b = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let c = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let d = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    let rects = tile_rects(&shell, &mut cx);
    let r = |id| rects.iter().find(|(t, _)| *t == id).unwrap().1;
    assert!(r(b).x > r(a).x, "second add: side by side, {rects:?}");
    assert!(r(c).x > r(b).x, "third add: still side by side, {rects:?}");
    assert!(
        r(d).y > r(c).y && (r(d).x - r(c).x).abs() < 1e-3,
        "fourth add: below, {rects:?}"
    );
}

#[gpui::test]
fn shift_d_duplicates_the_focused_tile_with_its_state_and_ctrl_shift_d_stacks_it(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let original = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    // Give the recorder some state through its own `:` command.
    cx.simulate_keystrokes(":");
    cx.simulate_input("sort delta01"); // an exact completion word runs as typed (commandline.rs §3.4)
    cx.simulate_keystrokes("enter");

    cx.simulate_keystrokes("shift-d");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let copy = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_ne!(copy, original);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(copy)),
        Some("rec")
    );
    let carried = log.borrow().iter().find_map(|r| match r {
        crate::module::recording::Recorded::Created(t, Some(state)) if *t == copy => {
            Some(state.clone())
        }
        _ => None,
    });
    assert_eq!(
        carried.and_then(|s| s
            .get("last_command")
            .and_then(|v| v.as_str().map(str::to_string))),
        Some("sort delta01".to_string()),
        "the duplicate's factory received the original's serialized state: {:?}",
        log.borrow()
    );
    let rects = tile_rects(&shell, &mut cx);
    let r = |id| rects.iter().find(|(t, _)| *t == id).unwrap().1;
    assert!(
        r(copy).x > r(original).x,
        "shift+d: side by side, {rects:?}"
    );

    cx.simulate_keystrokes("ctrl-shift-d");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let stacked = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    let rects = tile_rects(&shell, &mut cx);
    let r = |id| rects.iter().find(|(t, _)| *t == id).unwrap().1;
    assert!(r(stacked).y > r(copy).y, "ctrl+shift+d: below, {rects:?}");
}

#[gpui::test]
fn duplicate_on_an_empty_workspace_or_a_placeholder_is_a_no_op(cx: &mut gpui::TestAppContext) {
    let (window, mut cx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut cx);
    cx.simulate_keystrokes("shift-d");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(shell.read_with(&cx, |s, _| s.services.workspaces.active().is_empty()));
    // A placeholder (empty roster) has nothing to duplicate either.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(&ActionId("workspace::split_right".into()), None, window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("shift-d");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s
            .services
            .workspaces
            .active()
            .tree()
            .tiles()
            .len()),
        1
    );
}

#[gpui::test]
fn add_into_a_focused_empty_dock_lands_in_the_dock(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    cx.simulate_keystrokes("ctrl-[");
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    shell.read_with(&cx, |s, _| {
        let ws = s.services.workspaces.active();
        assert_eq!(
            ws.region(),
            crate::tiling::FocusRegion::Dock(DockSide::Left)
        );
        assert_eq!(ws.docks().get(DockSide::Left).tree().tiles().len(), 1);
        assert_eq!(ws.tree().tiles().len(), 1, "the main tree did not grow");
        assert_eq!(s.occupant_kind(ws.focused_tile().unwrap()), Some("rec"));
    });
}

/// Spec 2026-09-08 add-tile §4.3: a pending request is addressed to
/// the tile that asked, so two tiles going occupant-less in one render
/// (a plain split, then an `open_module` that splits again) each get
/// exactly what was asked of them — no "lower id wins" rule.
#[gpui::test]
fn a_pending_request_lands_on_exactly_the_tile_that_asked(cx: &mut gpui::TestAppContext) {
    let (mut services, _log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new(
            "diagnostics",
        )));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(&ActionId("workspace::split_right".into()), None, window, cx);
            s.open_module("diagnostics", window, cx);
        });
    });
    let asked = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(asked)),
        Some("diagnostics")
    );
    let other = tiles.into_iter().find(|t| *t != asked).unwrap();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(other)),
        Some("rec")
    );
}
