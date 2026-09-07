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
