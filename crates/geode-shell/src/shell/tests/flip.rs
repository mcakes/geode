//! Shell integration for the flip barrier: frame changes open a barrier for visible
//! occupied tiles, and sweeping releases it at the deadline. The recorder submits no
//! queries; snapshot staging, promotion, and failed-query arrival are covered by
//! `geode-blotter` tests.

use super::*;
use geode_core::query::QueryKey;

#[gpui::test]
fn a_scope_change_opens_a_barrier_over_exactly_the_visible_tiles(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);

    // Tile 1: the first split, in workspace 1 (the active one).
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    // Tile 2: created in workspace 2 — hidden once we switch back to 1.
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(
                &ActionId("workspace::switch_2".to_string()),
                None,
                window,
                cx,
            );
        });
    });
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(
                &ActionId("workspace::switch_1".to_string()),
                None,
                window,
                cx,
            );
        });
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    // A scope change: opens a barrier over tile 1's key only — tile 2
    // belongs to workspace 2, not on screen right now.
    frame.update(&mut vcx, |f, cx| {
        if f.set_text(Some("spx".into())) {
            cx.notify();
        }
    });

    assert!(frame.read_with(&vcx, |f, _| f.barrier_open()));
    let v = frame.read_with(&vcx, |f, _| f.versions());
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), v)),
        "tile 1 is visible"
    );
    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(2), v)),
        "tile 2 is hidden (workspace 2, not active)"
    );

    // Nothing else ever arrives (the recorder submits no query) — the
    // deadline is what releases it.
    let flip_before = v.flip;
    assert!(frame.update(&mut vcx, |f, _| f.sweep(
        std::time::Instant::now()
            + crate::frame::FLIP_DEADLINE
            + std::time::Duration::from_millis(1)
    )));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.versions().flip),
        flip_before + 1
    );
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()));
}

/// The mutation-check entry for "only scope/grouping/as-of open a
/// barrier" targets `on_frame_changed`'s guard — this is the test named
/// there: a `data`/`config`-only bump must never open a barrier at all
/// (there is no "everyone at once" to coordinate for those; every tile
/// already requeries independently).
#[gpui::test]
fn a_data_bump_opens_no_barrier(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.note_published(crate::frame::Publish {
            dataset: "risk".into(),
            batch: "EOD".into(),
            books: 1,
            at: chrono::Utc::now(),
        });
        cx.notify();
    });
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()));

    frame.update(&mut vcx, |f, cx| {
        f.note_config_reloaded();
        cx.notify();
    });
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()));
}

/// The reload-poll tick sweeps the flip deadline. Advancing the test clock beyond one
/// tick must release a barrier even if no query arrives; the recorder deliberately
/// submits none.
#[gpui::test]
fn the_reload_poll_tick_sweeps_an_open_barrier_past_its_deadline(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

    // Observe the frame itself to prove release notifies its consumers. Barrier closure
    // and the flip version alone would pass even if staged snapshots could not be
    // promoted until an unrelated notification.
    let notified = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let n = notified.clone();
    vcx.update(|_, cx| {
        cx.observe(&frame, move |_frame, _cx| {
            n.set(n.get() + 1);
        })
        .detach();
    });

    // Backdated directly through `open_flip`'s own explicit `Instant`
    // rather than driven through a real scope mutation + a real wait:
    // gpui's test dispatcher fast-forwards its own *virtual* clock (what
    // `cx.background_executor().timer(..)` waits on) but never touches
    // real `std::time::Instant::now()`, which is what `Frame::sweep`
    // compares against — the same reason the existing `a_scope_change_
    // opens_a_barrier_...` test above hands `sweep` a manufactured later
    // `Instant` instead of actually waiting.
    let past = std::time::Instant::now()
        - crate::frame::FLIP_DEADLINE
        - std::time::Duration::from_millis(1);
    frame.update(&mut vcx, |f, _| {
        f.open_flip([QueryKey(1)], past);
    });
    assert!(frame.read_with(&vcx, |f, _| f.barrier_open()));
    let flip_before = frame.read_with(&vcx, |f, _| f.versions().flip);

    // `run_until_parked` first so the reload-poll loop (spawned in
    // `ShellView::new`) actually reaches its `.timer(..).await` and
    // registers with the test dispatcher before the clock advances past
    // it — otherwise there is nothing yet for `advance_clock` to fire.
    // The advance itself only needs to cross one tick of the *virtual*
    // clock; the barrier is already past its (real) deadline the moment
    // the tick's own sweep call runs.
    vcx.run_until_parked();
    vcx.executor().advance_clock(
        crate::shell::hot_reload::RELOAD_POLL_INTERVAL + std::time::Duration::from_millis(1),
    );
    vcx.run_until_parked();

    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_open()),
        "one reload-poll tick must sweep a barrier already past its deadline"
    );
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.versions().flip),
        flip_before + 1
    );
    assert_eq!(
        notified.get(),
        1,
        "the sweep's own cx.notify() must reach a real frame observer, \
         not just move Frame::release's own counters"
    );
}

/// Placeholder occupants submit no queries and cannot arrive at a flip barrier.
/// `visible_tile_keys` excludes them rather than delaying every scope change until the
/// deadline.
#[gpui::test]
fn a_placeholder_occupant_is_excluded_from_the_barriers_key_set(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);

    // The first `ctrl-v` only creates the root tile (nothing exists yet
    // to split); the second actually splits it in two, both real "rec"
    // occupants (the recorder is this fixture's default kind) once
    // `ensure_occupants` runs on the next render.
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let tiles: Vec<TileId> =
        shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles.len(), 2, "one split makes two tiles");

    // Downgrade the second tile's occupant to "placeholder" in place —
    // `visible_tile_keys` only ever reads `TileOccupant::kind`, so this
    // is exactly the state "nothing has opened on this tile yet" without
    // needing to reconstruct a real `PlaceholderFactory` occupant.
    let placeholder_tile = tiles[1];
    shell.update(&mut vcx, |s, _cx| {
        s.occupants.get_mut(&placeholder_tile).unwrap().kind = "placeholder";
    });

    let mut keys = Vec::new();
    shell.read_with(&vcx, |s, _| s.visible_tile_keys(&mut keys));
    assert_eq!(
        keys,
        vec![QueryKey(tiles[0].0)],
        "the placeholder tile must not be in the barrier's key set"
    );
}

/// Placeholder exclusion also applies to visible docks. Keep a real occupant in the
/// main tree and a placeholder in the left dock to exercise that branch separately.
#[gpui::test]
fn a_placeholder_occupant_in_a_visible_dock_is_excluded_from_the_barriers_key_set(
    cx: &mut gpui::TestAppContext,
) {
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);

    // Two real tiles in the main tree, same as the tree-branch test.
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let main_tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().tree().tiles()[0]
    });

    // Park the (focused, second) tile in the left dock — `dock::
    // move_left` shows the dock and moves the focused tile into it in
    // one action (`tiling::workspaces`'s own
    // `move_from_main_parks_the_tile_shows_the_dock_and_focuses_it`).
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.dispatch(&ActionId("dock::move_left".to_string()), None, window, cx);
        });
    });
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let (dock_visible, dock_tile) = shell.read_with(&vcx, |s, _| {
        let ws = s.services.workspaces.active();
        let dock = ws.docks().get(DockSide::Left);
        (dock.visible(), dock.tree().tiles()[0])
    });
    assert!(dock_visible, "dock::move_left must show the dock");

    // Downgrade the dock's occupant to "placeholder" in place — same
    // trick the tree-branch test above uses.
    shell.update(&mut vcx, |s, _cx| {
        s.occupants.get_mut(&dock_tile).unwrap().kind = "placeholder";
    });

    let mut keys = Vec::new();
    shell.read_with(&vcx, |s, _| s.visible_tile_keys(&mut keys));
    assert_eq!(
        keys,
        vec![QueryKey(main_tile.0)],
        "a placeholder occupant parked in a visible dock must not be in \
         the barrier's key set either"
    );
}

/// The shell registers its frame observer before occupants so it opens the barrier
/// before their frame callbacks can deliver or self-arrive. The recorder has no query
/// observer, so verify the barrier is already open after a settled scope change; module
/// tests separately cover pinned-tile self-arrival.
#[gpui::test]
fn barrier_opens_before_any_occupant_could_deliver(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    vcx.simulate_keystrokes("ctrl-v");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        if f.set_text(Some("spx".into())) {
            cx.notify();
        }
    });
    vcx.run_until_parked();

    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_open()),
        "the barrier must already be open by the time anything settles"
    );
    assert!(
        !log.borrow()
            .iter()
            .any(|r| matches!(r, crate::module::recording::Recorded::Delivered(..))),
        "nothing has been delivered — the recorder submits no query, so an open barrier here \
         cannot be explained by an occupant having already acted"
    );
}
