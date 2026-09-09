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

/// MIN-7 (Phase 4b Task 5 fix round 1): two `open_module` calls for the
/// same kind within one render (a double `mod+shift+d` press, or
/// key-repeat) — before the fix, both calls miss the "existing occupant"
/// search (the first call's split tile has no occupant yet;
/// `ensure_occupants` only creates one at the top of the *next* render),
/// so a second call split a second tile and overwrote
/// `pending_kind_for_new_tile`, leaving one tile hosting the requested
/// kind and a stray second one hosting the roster's default. Two calls
/// with no render between them must still yield exactly one split, one
/// tile.
#[gpui::test]
fn two_open_module_calls_for_the_same_kind_before_any_render_split_only_once(
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
         must split only once: {tiles:?}"
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

/// MIN-6 (final review): a plain `ctrl+v` split and a same-render
/// `open_module("diagnostics")` (which itself splits again, since no
/// occupant of that kind exists yet to focus) leave TWO occupant-less
/// tiles for one `ensure_occupants` pass to fill — `pending_kind_for_
/// new_tile` is consumed by the first one the `for id in &all` loop
/// happens to visit, which used to be whichever order `HashSet<TileId>`
/// iterated in. `all` is now sorted before that loop, so the outcome is
/// deterministic — pinned here as "the lower TileId gets the pending
/// kind", not merely "the same tile every time" (a test that only
/// checked determinism would pass on a stable-but-still-arbitrary
/// order).
#[gpui::test]
fn a_pending_kind_lands_on_the_lower_tile_id_when_two_tiles_go_occupantless_in_one_pass(
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

    // Both before any render: `ensure_occupants` only ever runs inside
    // `ShellView::render`, so nothing below is visible to it until the
    // one `window.draw` at the end. `open_shell`'s harness starts the
    // active workspace's tree empty, so the first split creates only the
    // first tile (nothing to split yet — `session.rs`'s own comment on
    // `split_right` covers this); the SECOND split (`open_module`'s own,
    // since no occupant of "diagnostics" exists yet to focus) is a real
    // split of that first tile, leaving both halves occupant-less.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(
                &crate::actions::ActionId("workspace::split_right".into()),
                None,
                window,
                cx,
            );
            s.open_module("diagnostics", window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let mut tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    tiles.sort();
    assert_eq!(
        tiles.len(),
        2,
        "one real split of the first tile: {tiles:?}"
    );
    let lower = tiles[0];
    let higher = tiles[1];
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(lower)),
        Some("diagnostics"),
        "the lower TileId of the two occupant-less tiles gets the pending kind"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(higher)),
        Some("rec"),
        "the other one falls back to the roster's default kind"
    );
}
