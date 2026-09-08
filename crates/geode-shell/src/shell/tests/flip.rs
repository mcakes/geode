//! The flip barrier (Phase 4 §3.10), shell half: `on_frame_changed`
//! opens a barrier over exactly the visible tiles' keys on a scope/
//! grouping/as-of change, and `sweep` releases it at the deadline. The
//! blotter-side half (staging, promotion, a failure counting as arrival)
//! is exercised end to end in `geode-blotter`'s own test suite
//! (`tile::tests::two_tiles_promote_in_the_same_pass_and_a_failure_
//! releases_the_barrier`) — the recorder module here carries no query of
//! its own, so it can only stand in for "a tile with an occupant",
//! nothing about deliver/staging.

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

/// Phase 4b M7: `on_frame_changed` used to spawn a fresh detached
/// `cx.spawn` timer per scope/grouping/as-of mutation just to sweep this
/// same barrier's deadline — a burst of keystrokes spawned a burst of
/// timers. The existing ~500ms reload-poll loop (`ShellView::new`) now
/// sweeps on every tick instead, so advancing the test clock past one
/// tick — comfortably past `FLIP_DEADLINE` (250ms), with nothing else
/// ever arriving (the recorder submits no query) — must release the
/// barrier exactly the way the old per-mutation timer used to.
#[gpui::test]
fn the_reload_poll_tick_sweeps_an_open_barrier_past_its_deadline(cx: &mut gpui::TestAppContext) {
    let (services, _log) = services_with_recorder();
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());

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
}

/// Phase 4b M8: a placeholder occupant (nothing has opened on that tile
/// yet) never submits a query and never arrives — a barrier that waited
/// on it would sit open until `FLIP_DEADLINE` on every single scope
/// change, for a tile that was never going to answer. `visible_tile_keys`
/// must skip any occupant whose `kind` is `"placeholder"`.
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

/// F2 (final fix wave): the observer-order invariant stated in
/// `ShellView::new`'s `cx.observe_in` comment, `on_frame_changed`'s
/// `open_flip` branch, and `Frame::open_flip`'s own doc — the shell's
/// frame observer is registered before any occupant's, so `open_flip`
/// always finishes before a single occupant's own `on_frame_changed`
/// runs for the same notify. That is what lets a non-following tile
/// self-arrive from its own `on_frame_changed` without ever requerying
/// (`BlotterTile`'s `barrier_wants`/`arrived` branch, exercised end to
/// end in `geode-blotter`'s
/// `a_pinned_tile_arrives_from_on_frame_changed_without_requerying`).
/// The recorder module here has no frame observer of its own (it cannot
/// express pinning), so this proves the weaker, sufficient fact
/// directly instead: after a scope change, the barrier is open
/// immediately — `run_until_parked` settles with nothing delivered —
/// so nothing an occupant's own observer could do (deliver, or
/// self-arrive) races the barrier's opening.
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
