//! Occupant lifecycle: restoring, filling, ensuring, and the module
//! hosting contract's visibility handshake.

use super::*;
// Explicit: `tests`'s own `mod session;` declaration shadows the
// glob-imported `crate::session`, so the bare `session::` paths below need
// this to resolve to the real module rather than to the sibling test file.
use crate::session;
use crate::tiling::Orientation;

/// A fixture factory pairs `Diagnostics::watch` and `unwatch` through `set_visible`,
/// modeling the diagnostics module without depending on that module crate. It verifies
/// that an occupant is hidden before being dropped.
mod watching {
    use super::*;
    use crate::keymap::KeyContext;
    use crate::module::{
        Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant,
    };
    use gpui::{App, Context, FocusHandle, Render, SharedString, div};

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
        // Ignore repeated visibility values before changing the watch counter. A newly
        // created active occupant can receive `true` from both creation and visibility
        // reconciliation.
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
        fn deliver(&self, delivery: Delivery, _: &mut Window, _: &mut App) {
            match delivery {
                // This tile never queries — nothing addressed here.
                Delivery::Query(_) => {}
                // This tile never prices — nothing addressed here.
                Delivery::Price(_) => {}
                // This tile asks no series query and holds no
                // `(identity, source)` pair.
                Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}
                // This tile never uploads — nothing addressed here.
                Delivery::Upload(_) => {}
            }
        }
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
        fn set_stack(&self, _: Option<StackHandle>, _: &mut App) {}
        fn title(&self, _: &App) -> SharedString {
            "watching".into()
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
            _frame: crate::frame::FrameRef,
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

/// Closing a tile calls `set_visible(false)` before dropping its occupant, balancing
/// any resource opened by `set_visible(true)`; the fixture observes the matching
/// diagnostics unwatch.
#[gpui::test]
fn closing_a_watching_tile_unwatches_the_diagnostics_entity(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let mut roster = crate::module::ModuleRoster::new();
    roster.add(Box::new(watching::WatchingFactory));
    roster.register_actions(&mut services.registry);
    // Add the watching kind explicitly; the shared recorder binding would create a
    // placeholder because this roster has no "rec" factory.
    crate::defaults::register_add_actions(&mut services.registry, &["watching"]);
    services.keymap = test_keymap(&services.registry, &[]);
    services.roster = roster;

    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_watching");
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

/// Restore one tile whose kind the fixture roster does not register. Return its
/// original record so tests can verify that `current_tiles` preserves it verbatim.
fn services_with_an_unknown_restored_kind() -> (
    ShellServices,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    session::TileRecord,
) {
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
        module = "unregistered-kind"
        [tiles.1.state]
        last_command = "state for a different module"
    "#
    .parse()
    .unwrap();
    if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
        ws_table.insert("1".to_string(), toml::Value::Table(ws1));
    }
    let restored = session::from_toml(&table).unwrap();
    let original = restored.tiles.get(&1).unwrap().clone();

    let (mut services, log) = services_with_recorder();
    services.workspaces = restored.workspaces;
    services.restored_tiles = restored.tiles;
    (services, log, original)
}

/// An unknown restored kind displays a placeholder while preserving the original record
/// verbatim through `current_tiles`. A later flush must not replace its kind or
/// serialized state.
#[gpui::test]
fn a_restored_tile_of_an_unknown_kind_paints_the_placeholder_and_its_record_survives(
    cx: &mut gpui::TestAppContext,
) {
    let (services, log, original) = services_with_an_unknown_restored_kind();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(TileId(1))),
        Some(crate::module::placeholder::PLACEHOLDER_KIND)
    );
    assert!(
        log.borrow().is_empty(),
        "no factory was asked to host the unknown record: {:?}",
        log.borrow()
    );
    let tiles = shell.read_with(&cx, |s, cx| s.current_tiles(cx));
    assert_eq!(
        tiles.get(&1),
        Some(&original),
        "the record rides through: {tiles:?}"
    );

    // Filling it in place replaces the record with the live occupant's.
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let tiles = shell.read_with(&cx, |s, cx| s.current_tiles(cx));
    assert_eq!(tiles.get(&1).map(|r| r.kind.as_str()), Some("rec"));
    // Assert the unplaced-record map directly: the live occupant wins serialization for
    // this ID, which could otherwise hide a stale record after `add_tile` claims the
    // tile.
    assert!(
        shell.read_with(&cx, |s, _| s.unplaced_records.is_empty()),
        "filling the placeholder in place drops the record it rode in on"
    );
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

/// An unplaced record survives only while its tile does. Closing the tile must remove
/// the record so a later flush cannot resurrect it.
#[gpui::test]
fn closing_an_unknown_kind_tile_drops_its_unplaced_record(cx: &mut gpui::TestAppContext) {
    let (services, _log, _original) = services_with_an_unknown_restored_kind();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    assert_eq!(
        shell.read_with(&cx, |s, cx| s.current_tiles(cx).len()),
        1,
        "the record rode through the first render"
    );

    cx.simulate_keystrokes("ctrl-w");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        shell
            .read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles())
            .is_empty(),
        "the tile is gone"
    );
    let tiles = shell.read_with(&cx, |s, cx| s.current_tiles(cx));
    assert!(
        !tiles.contains_key(&1),
        "a record whose tile closed is not written back: {tiles:?}"
    );
    assert!(shell.read_with(&cx, |s, _| s.unplaced_records.is_empty()));
}

/// Occupants are created for restored tiles in inactive workspaces too. Each must
/// receive its initial `set_visible(false)` call; the recorder has no default
/// visibility state, making the notification observable.
#[gpui::test]
fn an_occupant_created_outside_the_active_workspace_is_told_it_is_hidden(
    cx: &mut gpui::TestAppContext,
) {
    let mut table = session::to_toml(
        &Workspaces::new(),
        &session::TileRecords::new(),
        None,
        &crate::session::PinnedRecords::new(),
        &crate::palette_usage::PaletteUsage::new(),
        &crate::session::PageRecords::new(),
    );
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
fn an_add_creates_an_occupant_of_the_requested_kind_and_paints_it(cx: &mut gpui::TestAppContext) {
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

/// A real keypress reaches the module through its own keymap fragment. Only the roster
/// supplies `q`; the shell builtin and fixture add layer do not bind it.
#[gpui::test]
fn a_binding_from_a_modules_fragment_reaches_its_hosted_tile(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_a_module_fragment(REC_FRAGMENT);
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("q");
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert!(
        log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(t, a, None)
                if *t == tile && a.0 == "rec::noop"
        )),
        "{:?}",
        log.borrow()
    );
}

/// The other half of the layer order: a fragment sits BELOW every layer a
/// trader edits, so a user keymap binding the same key in the same context
/// wins — the property that makes a module's defaults defaults rather than
/// an unoverridable second builtin. `workspace::close_tile` is the visible
/// proof: the tile is gone, and the module was never told anything.
#[gpui::test]
fn a_user_layer_binding_wins_over_a_modules_fragment(cx: &mut gpui::TestAppContext) {
    let (mut services, log) = services_with_a_module_fragment(REC_FRAGMENT);
    let user = LayerDoc {
        layer: Layer::User,
        name: "keymap".to_string(),
        file: "<test:user>".into(),
        table:
            "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"q\" = \"workspace::close_tile\"\n"
                .parse()
                .unwrap(),
    };
    services.keymap =
        test_keymap_with_fragments(&services.registry, &services.keymap_fragments, &[user]);
    let (window, mut cx) = open_shell(cx, services);
    // The first add fills the initial placeholder in place; a second add is needed to
    // create another tile.
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-v");
    let shell = shell_of(&window, &mut cx);
    let before = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(before, 2, "fixture check: ctrl-v twice added a second tile");
    cx.simulate_keystrokes("q");
    let after = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().tree().tiles().len()
    });
    assert_eq!(
        after, 1,
        "the user's binding must win over the module's fragment"
    );
    assert!(
        !log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Dispatch(_, a, _) if a.0 == "rec::noop"
        )),
        "the module's own action must not have fired at all: {:?}",
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

/// Closing a tile tells its occupant it closed, after telling it it is
/// hidden; a workspace switch hides without closing. Following tiles cancel
/// only on `closed`, so a switch reported as a close would cancel a query
/// whose answer the trader expects on return.
#[gpui::test]
fn closing_a_tile_tells_its_occupant_it_closed_and_a_workspace_switch_does_not(
    cx: &mut gpui::TestAppContext,
) {
    use crate::module::recording::Recorded;
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
    cx.simulate_keystrokes("alt-1");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Visible(t, false) if *t == tile)),
        "the switch did hide the tile: {:?}",
        log.borrow()
    );
    assert!(
        !log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Closed(_))),
        "a switch hides; it never closes: {:?}",
        log.borrow()
    );

    cx.simulate_keystrokes("ctrl-w");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
        let _ = window.draw(cx);
    });
    let log = log.borrow();
    let hidden = log
        .iter()
        .rposition(|r| matches!(r, Recorded::Visible(t, false) if *t == tile))
        .expect("hidden before removal");
    let closed = log
        .iter()
        .position(|r| matches!(r, Recorded::Closed(t) if *t == tile))
        .expect("closing a tile tells its occupant");
    assert!(hidden < closed, "hidden first, then closed: {log:?}");
    assert_eq!(
        log.iter()
            .filter(|r| matches!(r, Recorded::Closed(_)))
            .count(),
        1,
        "closed once, and only the closed tile: {log:?}"
    );
}

#[gpui::test]
fn a_click_on_a_tile_leaves_the_shell_focused_on_the_next_frame(cx: &mut gpui::TestAppContext) {
    // GPUI focuses tracked elements on mouse-down. The tile click handler arms
    // root-focus restoration so an occupant's tracked handle cannot strand shell
    // chords.
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
    // Dock tile clicks also arm root-focus restoration when the occupant tracks its own
    // handle.
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

/// When a window has no focused handle, the next render restores shell-root focus so
/// key dispatch can resume. `Window::blur` constructs this state deterministically,
/// modeling a focused handle dropped when its view is unmounted.
#[gpui::test]
fn a_window_with_nothing_focused_gets_the_shell_root_back_on_the_next_frame(
    cx: &mut gpui::TestAppContext,
) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    // Blur and the check on one side of the same update: leaving the
    // block flushes gpui's effects, and a `refresh`ed window redraws
    // there — which is the net firing, so a check in a later block would
    // read the healed state and prove nothing about the fixture.
    cx.update(|window, cx| {
        window.blur(cx);
        assert!(
            window.focused(cx).is_none(),
            "the fixture really did reach the no-focus state"
        );
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| root.is_focused(window)),
        "the render-top net gave the shell root focus back"
    );
    // … and the keyboard is alive again, through the real pipeline.
    cx.simulate_keystrokes("alt-2");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active_index()),
        2,
        "keys reach the shell again once the net reclaimed focus"
    );
}

/// Workspace switching unmounts an occupant's element while preserving its entity and
/// focus handle. A nonempty focus handle can therefore refer to an element outside the
/// rendered tree, bypassing the dropped-handle recovery. Focus the occupant directly to
/// test the switch backstop independently of mouse-down's restore flag.
#[gpui::test]
fn a_focused_tile_leaving_the_visible_set_hands_focus_back_to_the_shell(
    cx: &mut gpui::TestAppContext,
) {
    let (services, rec_focus) = services_with_recorder_focus();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let tile_focus = rec_focus
        .borrow()
        .clone()
        .expect("the recorder's view took a focus handle when it was created");
    cx.update(|window, cx| tile_focus.focus(window, cx));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.update(|window, _| tile_focus.is_focused(window)),
        "sanity: the tile's own view holds keyboard focus"
    );

    // Keys still work while the tile holds focus — the shell root is an
    // ancestor of the focused node, so `alt-2` bubbles to it.
    cx.simulate_keystrokes("alt-2");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active_index()),
        2,
        "sanity: the switch itself happened"
    );

    let root = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| root.is_focused(window)),
        "the tile left the visible set holding focus, so the shell root took it back"
    );
    cx.simulate_keystrokes("alt-1");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active_index()),
        1,
        "and the keyboard still reaches the shell afterwards"
    );
}

/// Switch workspaces directly to isolate `ensure_occupants`' focus backstop. Dispatch
/// normally arms `pending_focus_restore` first, which would restore focus even if the
/// backstop were missing. Calling `Workspaces::switch` bypasses that earlier guard.
#[gpui::test]
fn a_tile_leaving_the_visible_set_without_a_dispatch_hands_focus_back(
    cx: &mut gpui::TestAppContext,
) {
    let (services, rec_focus) = services_with_recorder_focus();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    cx.simulate_keystrokes("ctrl-v");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let tile_focus = rec_focus
        .borrow()
        .clone()
        .expect("the recorder's view took a focus handle when it was created");
    cx.update(|window, cx| tile_focus.focus(window, cx));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.update(|window, _| tile_focus.is_focused(window)),
        "sanity: the tile's own view holds keyboard focus"
    );

    // The switch, through the method both the chord and the sidebar click
    // reach — but not through `dispatch`, so nothing arms the restore.
    cx.update(|_window, cx| {
        shell.update(cx, |shell, cx| {
            assert!(shell.services.workspaces.switch(2), "sanity: it switched");
            cx.notify();
        });
    });
    assert!(
        !shell.read_with(&cx, |s, _| s.pending_focus_restore),
        "the premise: no dispatch ran, so `note_keyboard_focus_move` \
         armed nothing — only the backstop is left"
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let root = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        cx.update(|window, _| root.is_focused(window)),
        "the tile left the visible set holding focus, so `ensure_occupants` \
         took it back inside that very render"
    );
    cx.simulate_keystrokes("alt-1");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.services.workspaces.active_index()),
        1,
        "and the keyboard still reaches the shell afterwards"
    );
}

/// The backstop's own restraint, matched to the net's: a tile leaving the
/// visible set must NOT pull focus off a live shell surface. The palette
/// is open (its `Input` focused) when the workspace switches, and the
/// switch is dispatched rather than typed precisely because typing would
/// go to the palette.
#[gpui::test]
fn a_tile_leaving_the_visible_set_leaves_the_palette_focused(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("ctrl-k");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        palette_is_focused(&shell, &mut cx),
        "sanity: the palette's filter field is focused while it is open"
    );

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.dispatch(
                &ActionId("workspace::switch_2".to_string()),
                None,
                window,
                cx,
            );
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(
        palette_is_focused(&shell, &mut cx),
        "a tile leaving the visible set never steals focus from a live shell surface"
    );
}

/// The net's other half, and why its condition is exactly `is_none()`:
/// it must never steal focus from a LIVE focused element. The palette's
/// filter `Input` holds focus while the palette is open and redraws land
/// on every keystroke — any broader condition would yank the caret out
/// of the field mid-type.
#[gpui::test]
fn the_focus_net_leaves_a_live_focused_input_alone(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    cx.simulate_keystrokes("ctrl-k");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        palette_is_focused(&shell, &mut cx),
        "the palette's filter field takes focus when it opens"
    );

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        palette_is_focused(&shell, &mut cx),
        "a redraw with a live focused element leaves it focused"
    );
    let root = shell.read_with(&cx, |s, _| s.focus_handle.clone());
    assert!(
        !cx.update(|window, _| root.is_focused(window)),
        "the net did not pull focus back to the shell root"
    );
}

// Status-summary integration is covered in `shell/tests/diagnostics.rs` using
// `Diagnostics::note_health`.

/// `open_module` creates the requested kind when absent, then focuses the existing tile
/// on subsequent calls in that workspace.
#[gpui::test]
fn open_module_twice_yields_one_tile_of_that_kind_focused(cx: &mut gpui::TestAppContext) {
    let (mut services, log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new(
            "probe",
        )));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);

    // Call `open_module` directly: nothing in the shipped shell calls it for
    // this kind, so the door itself is what is under test.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_module("probe", window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let first_tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(first_tile)),
        Some("probe")
    );

    // Focus something else, then ask for the probe again — it must
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

    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_module("probe", window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let refocused = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        refocused, first_tile,
        "the second open_module call must focus the existing probe \
         tile, not create another"
    );
    let probe_tiles: Vec<TileId> = shell
        .read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles())
        .into_iter()
        .filter(|id| shell.read_with(&cx, |s, _| s.occupant_kind(*id)) == Some("probe"))
        .collect();
    assert_eq!(
        probe_tiles.len(),
        1,
        "exactly one probe tile ever exists: {:?}",
        log.borrow()
    );
}

/// Two `open_module` calls before a render still add only one tile of the requested
/// kind. The first occupant does not exist until `ensure_occupants` runs, so duplicate
/// suppression must include pending requests.
#[gpui::test]
fn two_open_module_calls_for_the_same_kind_before_any_render_add_only_once(
    cx: &mut gpui::TestAppContext,
) {
    let (mut services, _log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new(
            "probe",
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
            s.open_module("probe", window, cx);
            s.open_module("probe", window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(
        tiles.len(),
        1,
        "two open_module('probe') calls with no render between them \
         must add only once: {tiles:?}"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(tiles[0])),
        Some("probe")
    );
}

/// The other half of `open_module`'s contract: a kind with no registered
/// factory (`services_with_recorder`'s roster only knows "rec") paints a
/// placeholder rather than leaving the added tile occupant-less, and logs
/// a warning naming the requested kind.
#[gpui::test]
fn open_module_with_no_matching_factory_paints_a_placeholder_and_warns(
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
                s.open_module("nonesuch", window, cx);
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
        Some(crate::module::placeholder::PLACEHOLDER_KIND),
        "no 'nonesuch' factory is registered, so the tile is a placeholder"
    );

    let mut records = Vec::new();
    ring.drain_since(0, &mut records);
    assert!(
        records
            .iter()
            .any(|r| r.level == tracing::Level::WARN && r.message.contains("nonesuch")),
        "expected a warning naming the unmatched kind: {records:?}"
    );
}

/// Dispatch an action id straight into the shell and draw once — the
/// add rows are palette rows, and `dispatch` is exactly what a palette
/// `enter` calls (`palette_ctl::dispatch_palette_item`).
pub(super) fn dispatch_and_draw(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
    id: &str,
) {
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
    // `test_services`'s default kind is "placeholder", so a tile created
    // by a bare `split_active` — no pending request of its own — is one;
    // the fixture's recorder is what "Rec: Split" fills it with.
    let (services, log) = test_services_with_log();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    shell.update(&mut cx, |s, cx| {
        s.services.workspaces.split_active(Orientation::Horizontal);
        cx.notify();
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
    cx.simulate_input("sort delta01"); // an exact completion word runs as typed
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
    // A placeholder — what a bare `split_active` leaves behind, since
    // the fixture's default kind is "placeholder" — has nothing to
    // duplicate either.
    shell.update(&mut cx, |s, cx| {
        s.services.workspaces.split_active(Orientation::Horizontal);
        cx.notify();
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

/// Pending occupant requests are addressed to tile IDs. Multiple unfilled tiles in one
/// render must each receive their own requested kind, independent of ID ordering.
#[gpui::test]
fn a_pending_request_lands_on_exactly_the_tile_that_asked(cx: &mut gpui::TestAppContext) {
    let (mut services, _log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new(
            "probe",
        )));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            // A plain split (no request of its own), then an
            // `open_module` that splits again: two tiles go
            // occupant-less in one render pass.
            s.services.workspaces.split_active(Orientation::Horizontal);
            s.open_module("probe", window, cx);
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
        Some("probe")
    );
    let other = tiles.into_iter().find(|t| *t != asked).unwrap();
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(other)),
        Some(crate::module::placeholder::PLACEHOLDER_KIND),
        "the plain split asked for nothing, and there is no default kind \
         to guess with — it gets a placeholder"
    );
}

/// Drop a pending request if its tile closes before being filled. It must not target
/// another tile or keep the duplicate-kind guard active for the rest of the session.
#[gpui::test]
fn a_pending_request_for_a_closed_tile_is_dropped_and_does_not_latch_open_module(
    cx: &mut gpui::TestAppContext,
) {
    let (mut services, _log) = services_with_recorder();
    services
        .roster
        .add(Box::new(crate::module::recording::RecordingFactory::new(
            "probe",
        )));
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let kept = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });

    // Both inside ONE `cx.update` block, so no render happens between
    // them: `open_module` splits and records a request under the new
    // (now focused) id, and `close_tile` removes that very tile before
    // `ensure_occupants` ever sees it.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_module("probe", window, cx);
            s.services.workspaces.active_mut().close_tile();
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let tiles = shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles, vec![kept], "only the original tile is left");
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(kept)),
        Some("rec"),
        "the survivor keeps its own occupant — the request was not re-aimed"
    );
    assert!(
        shell.read_with(&cx, |s, _| s.pending_tiles.is_empty()),
        "the request for the closed tile is dropped"
    );

    // …and the guard did not latch: asking again really does open one.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_module("probe", window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let probes: Vec<TileId> = shell
        .read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles())
        .into_iter()
        .filter(|id| shell.read_with(&cx, |s, _| s.occupant_kind(*id)) == Some("probe"))
        .collect();
    assert_eq!(
        probes.len(),
        1,
        "a dropped request leaves the kind openable: {probes:?}"
    );
}

/// A keyed delivery reaches exactly the addressed tile's occupant, even when another
/// live tile could receive it.
#[gpui::test]
fn a_delivery_reaches_the_tile_addressed_by_its_key_and_no_other(cx: &mut gpui::TestAppContext) {
    use crate::module::Delivery;
    use geode_core::query::{QueryKey, QueryOutcome};
    use std::time::Instant;

    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let first = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let second = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_ne!(first, second, "sanity: two distinct tiles are live");

    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.deliver(
                Delivery::Query(QueryOutcome {
                    key: QueryKey(first.0),
                    tag: 42,
                    snapshot: Err("test outcome".into()),
                    submitted: Instant::now(),
                }),
                window,
                cx,
            );
        });
    });

    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Delivered(t, 42) if *t == first)
        ),
        "the addressed tile must be delivered to: {:?}",
        log.borrow()
    );
    assert!(
        !log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Delivered(t, _) if *t == second
        )),
        "the other tile must not be delivered to: {:?}",
        log.borrow()
    );
}

/// The keyed variant egress adds: a `Delivery::
/// Upload` reaches the tile that submitted it and no other, exactly as
/// `Query`/`Price`/`Series` already do — `deliver`'s keyed arm must name
/// it beside them, not drop it into the broadcast one.
#[gpui::test]
fn an_upload_delivery_reaches_its_tile_and_no_other(cx: &mut gpui::TestAppContext) {
    use crate::module::{Delivery, UploadDelivery};
    use geode_core::query::QueryKey;

    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let first = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let second = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_ne!(first, second, "sanity: two distinct tiles are live");

    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.deliver(
                Delivery::Upload(UploadDelivery {
                    key: QueryKey(first.0),
                    tag: 42,
                    target: "sophis".into(),
                    result: Ok(()),
                }),
                window,
                cx,
            );
        });
    });

    assert!(
        log.borrow().iter().any(
            |r| matches!(r, crate::module::recording::Recorded::Delivered(t, 42) if *t == first)
        ),
        "the addressed tile must be delivered to: {:?}",
        log.borrow()
    );
    assert!(
        !log.borrow().iter().any(|r| matches!(
            r,
            crate::module::recording::Recorded::Delivered(t, _) if *t == second
        )),
        "the other tile must not be delivered to: {:?}",
        log.borrow()
    );
}

/// A shell with `count` recording tiles on the active workspace, all
/// writing into the one log — the fixture the two delivery-routing
/// tests below share. Hands the `VisualTestContext` back with it: a
/// delivery is driven through `cx.update`, which needs the window.
fn shell_with_recording_tiles(
    cx: &mut gpui::TestAppContext,
    count: usize,
) -> (
    Entity<ShellView>,
    gpui::VisualTestContext,
    std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    for _ in 0..count {
        dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    }
    (shell, cx, log)
}

/// One more recording tile, on workspace 2, with workspace 1 left
/// active again: a live occupant (`ensure_occupants` walks every
/// workspace) that nothing on screen can see. Answers its `TileId`.
fn add_recording_tile_on_workspace_two(
    shell: &Entity<ShellView>,
    cx: &mut gpui::VisualTestContext,
) -> TileId {
    dispatch_and_draw(shell, cx, "workspace::switch_2");
    dispatch_and_draw(shell, cx, "tile::add_rec");
    let hidden = shell.read_with(cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    dispatch_and_draw(shell, cx, "workspace::switch_1");
    hidden
}

/// `SeriesFetched` identifies an identity/source pair rather than a tile. Deliver it to
/// every visible occupant and no hidden occupants; hidden tiles have no subscription
/// and requery when made visible.
#[gpui::test]
fn a_key_less_delivery_reaches_every_visible_occupant_and_no_hidden_one(
    cx: &mut gpui::TestAppContext,
) {
    use crate::module::Delivery;
    use crate::module::recording::Recorded;

    let (shell, mut cx, log) = shell_with_recording_tiles(cx, 2);
    let hidden = add_recording_tile_on_workspace_two(&shell, &mut cx);
    let visible: Vec<TileId> = shell.read_with(&cx, |s, _| {
        let mut keys = Vec::new();
        s.visible_tile_keys(&mut keys);
        keys.into_iter().map(|k| TileId(k.0)).collect()
    });
    assert_eq!(visible.len(), 2, "sanity: two tiles are on screen");
    assert!(
        !visible.contains(&hidden),
        "sanity: the workspace-2 tile is not on screen"
    );
    assert_eq!(
        shell.read_with(&cx, |s, _| s.occupant_kind(hidden)),
        Some("rec"),
        "sanity: the hidden tile has a live occupant that COULD be told"
    );

    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.deliver(
                Delivery::SeriesFetched {
                    source: "demo_kdb".into(),
                    identity: "SPX.close".into(),
                    result: Ok(3),
                },
                window,
                cx,
            );
        });
    });

    let mut seen: Vec<TileId> = log
        .borrow()
        .iter()
        .filter_map(|r| match r {
            Recorded::SeriesFetched(tile, pair) if pair == "SPX.close@demo_kdb" => Some(*tile),
            _ => None,
        })
        .collect();
    seen.sort();
    let mut expected = visible.clone();
    expected.sort();
    assert_eq!(
        seen, expected,
        "exactly the two visible tiles, once each, and no other"
    );
    assert!(
        !seen.contains(&hidden),
        "a tile on a switched-away workspace is not told: {seen:?}"
    );
}

/// The other half of the same routing rule: a `Series` outcome carries
/// a key like a `Query` does, so it reaches that one tile and no other
/// — the broadcast arm must not swallow it.
#[gpui::test]
fn a_series_outcome_is_routed_to_its_key_alone(cx: &mut gpui::TestAppContext) {
    use crate::module::Delivery;
    use crate::module::recording::Recorded;
    use geode_core::query::QueryKey;
    use geode_core::series::{SeriesOutcome, SeriesResult};
    use std::time::Instant;

    let (shell, mut cx, log) = shell_with_recording_tiles(cx, 2);
    let live: Vec<TileId> =
        shell.read_with(&cx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(live.len(), 2, "sanity: two distinct tiles are live");
    let target = live[0];

    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.deliver(
                Delivery::Series(SeriesOutcome {
                    key: QueryKey(target.0),
                    tag: 42,
                    submitted: Instant::now(),
                    result: Ok(SeriesResult::default()),
                }),
                window,
                cx,
            );
        });
    });

    let delivered: Vec<(TileId, u64)> = log
        .borrow()
        .iter()
        .filter_map(|r| match r {
            Recorded::Delivered(t, tag) => Some((*t, *tag)),
            _ => None,
        })
        .collect();
    assert_eq!(delivered, vec![(target, 42)]);
}

/// Price deliveries use the keyed router and reach only the addressed tile.
#[gpui::test]
fn a_price_delivery_is_routed_by_key_like_a_query(cx: &mut gpui::TestAppContext) {
    use crate::module::Delivery;
    use geode_core::pricing::PriceOutcome;
    use geode_core::query::QueryKey;
    use std::time::Instant;

    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let first = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let second = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });

    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.deliver(
                Delivery::Price(PriceOutcome {
                    key: QueryKey(second.0),
                    tag: 7,
                    submitted: Instant::now(),
                    results: Vec::new(),
                }),
                window,
                cx,
            );
        });
    });

    let log = log.borrow();
    assert!(
        log.iter()
            .any(|r| matches!(r, crate::module::recording::Recorded::Priced(t, 7) if *t == second)),
        "{log:?}"
    );
    assert!(
        !log.iter()
            .any(|r| matches!(r, crate::module::recording::Recorded::Priced(t, _) if *t == first)),
        "{log:?}"
    );
}

/// Does the command palette's own filter `Input` hold focus? (The
/// toolbar's scope-bar field has `filter_is_focused` in `tests/mod.rs`;
/// this is the palette's, used by the two focus-restraint tests above.)
fn palette_is_focused(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> bool {
    cx.update(|window, cx| {
        shell
            .read(cx)
            .palette_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    })
}

/// A shared motion key reaches the focused grid occupant through the shell's
/// fall-through, with its count; the shell claims none of `motion::*`.
#[gpui::test]
fn a_shared_motion_key_reaches_a_grid_occupant_with_its_count(cx: &mut gpui::TestAppContext) {
    let mut recorder = crate::module::recording::RecordingFactory::new("rec");
    recorder.grid = true;
    let log = recorder.log.clone();
    let services = services_with_recorders(vec![recorder]);
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_keystrokes("5 j");
    cx.simulate_keystrokes("down");
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    let got: Vec<(String, Option<u32>)> = log
        .borrow()
        .iter()
        .filter_map(|r| match r {
            crate::module::recording::Recorded::Dispatch(t, a, n) if *t == tile => {
                Some((a.0.clone(), *n))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("motion::down".to_string(), Some(5)),
            ("motion::down".to_string(), None),
        ]
    );
}

/// Each occupant is handed its own workspace's frame: a tile added in
/// workspace 2 reads workspace 2's lane, never the active one at some
/// later moment.
#[gpui::test]
fn an_occupant_is_created_with_its_own_workspaces_frame(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    dispatch_and_draw(&shell, &mut cx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let framed: Vec<u8> = log
        .borrow()
        .iter()
        .filter_map(|r| match r {
            crate::module::recording::Recorded::Framed(_, ws) => Some(ws.get()),
            _ => None,
        })
        .collect();
    assert_eq!(framed, vec![1, 2]);
}

/// A tile restored into a hidden workspace is framed by that workspace,
/// not by the one active when its occupant is created on the first render.
#[gpui::test]
fn a_tile_restored_into_a_hidden_workspace_is_framed_by_it(cx: &mut gpui::TestAppContext) {
    let mut table = session::to_toml(
        &Workspaces::new(),
        &session::TileRecords::new(),
        None,
        &crate::session::PinnedRecords::new(),
        &crate::palette_usage::PaletteUsage::new(),
        &crate::session::PageRecords::new(),
    );
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

    let framed: Vec<(TileId, u8)> = log
        .borrow()
        .iter()
        .filter_map(|r| match r {
            crate::module::recording::Recorded::Framed(t, ws) => Some((*t, ws.get())),
            _ => None,
        })
        .collect();
    assert_eq!(framed, vec![(TileId(1), 2)]);
}
