//! Link groups at the shell: a group's scope change flips its visible
//! followers through the barrier, and the session writer skips a snapshot
//! whose text did not change. The recorder submits no query, so these tests
//! read what the barrier awaits instead of waiting for an arrival.

use super::*;
use crate::frame::Frame;
use crate::tiling::WorkspaceIx;
use geode_core::link::Group;
use geode_core::query::{AsOf, QueryKey};
use geode_core::scope::Scope;

const WS1: WorkspaceIx = WorkspaceIx::FIRST;

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// Add a recorder tile to the active workspace and create its occupant.
fn add_tile(vcx: &mut gpui::VisualTestContext) {
    vcx.simulate_keystrokes("ctrl-v");
    draw(vcx);
}

fn underlying(u: &str) -> Scope {
    Scope::one("underlying_ref", u)
}

/// A shell over `services` with two visible recorder tiles, ids 1 and 2,
/// in workspace 1. The shared lane's scope has been written once and the
/// barrier that opened swept, so the lane's scope generation differs from
/// an unwritten group's: a follower's identity is then distinguishable
/// from the lane's.
fn two_tiles_in(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
) -> (
    gpui::WindowHandle<Root>,
    gpui::VisualTestContext,
    Entity<ShellView>,
    Entity<Frame>,
) {
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    add_tile(&mut vcx);
    add_tile(&mut vcx);
    let tiles = shell.read_with(&vcx, |s, _| s.services.workspaces.active().tree().tiles());
    assert_eq!(tiles, vec![TileId(1), TileId(2)]);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_text(Some("spx".into())));
        cx.notify();
    });
    vcx.run_until_parked();
    settle(&frame, &mut vcx);
    (window, vcx, shell, frame)
}

fn two_tiles(
    cx: &mut gpui::TestAppContext,
) -> (
    gpui::WindowHandle<Root>,
    gpui::VisualTestContext,
    Entity<ShellView>,
    Entity<Frame>,
) {
    two_tiles_in(cx, test_services())
}

/// Release whatever barrier is open, as the deadline would.
fn settle(frame: &Entity<Frame>, vcx: &mut gpui::VisualTestContext) {
    frame.update(vcx, |f, cx| {
        if f.sweep(std::time::Instant::now() + crate::frame::FLIP_DEADLINE * 2) {
            cx.notify();
        }
    });
    vcx.run_until_parked();
    assert!(!frame.read_with(vcx, |f, _| f.barrier_open()));
}

/// Make `tile` follow `group` through the frame and let the shell see it.
fn follow(
    frame: &Entity<Frame>,
    vcx: &mut gpui::VisualTestContext,
    tile: TileId,
    group: Option<Group>,
) {
    frame.update(vcx, |f, cx| {
        assert!(f.follow(tile, group));
        cx.notify();
    });
    vcx.run_until_parked();
}

/// A follower's scope generation is its group's, so its arrival carries the
/// group's identity. Enrolled under the lane's, it could never answer and
/// every lane flip beside a follower would wait out the deadline.
#[gpui::test]
fn a_lane_flip_awaits_a_follower_under_the_groups_identity(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, _shell, frame) = two_tiles(cx);
    follow(&frame, &mut vcx, TileId(1), Some(Group::A));

    let at = chrono::Utc::now();
    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_as_of(AsOf::At(at)));
        cx.notify();
    });
    vcx.run_until_parked();

    let (open, follower, lane) = frame.read_with(&vcx, |f, _| {
        (
            f.barrier_open(),
            f.view_for(WS1, TileId(1)).versions(),
            f.view(WS1).versions(),
        )
    });
    assert!(open, "an as-of change flips the visible tiles");
    assert_ne!(
        follower.scope, lane.scope,
        "sanity: the two identities differ"
    );
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), follower)),
        "the follower is awaited under its group's identity"
    );
    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), lane)),
        "and not under the lane's, which it never answers with"
    );
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(2), lane)),
        "the tile beside it is awaited under the lane's"
    );
}

/// A group's scope is part of what its followers show, so its change flips
/// them together; a tile that does not follow the group requeries nothing
/// and is not awaited.
#[gpui::test]
fn a_group_scope_change_opens_a_barrier_over_its_visible_followers_only(
    cx: &mut gpui::TestAppContext,
) {
    let (_window, mut vcx, _shell, frame) = two_tiles(cx);
    follow(&frame, &mut vcx, TileId(1), Some(Group::A));
    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_open()),
        "sanity: nothing is open before the group's scope moves"
    );

    frame.update(&mut vcx, |f, cx| {
        assert!(
            f.view_mut_for(WS1, TileId(1))
                .set_scope(underlying("SPX.Z"))
        );
        cx.notify();
    });
    vcx.run_until_parked();

    let (open, follower, lane) = frame.read_with(&vcx, |f, _| {
        (
            f.barrier_open(),
            f.view_for(WS1, TileId(1)).versions(),
            f.view(WS1).versions(),
        )
    });
    assert!(open, "a group scope change opens a barrier");
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), follower)),
        "over the group's follower, under the follower's identity"
    );
    for identity in [follower, lane] {
        assert!(
            !frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(2), identity)),
            "the tile that does not follow the group is not awaited"
        );
    }
}

/// A group whose followers are all off screen has nobody to flip. Opening
/// over an empty set would also clear a lane barrier that is still waiting
/// on the visible tiles.
#[gpui::test]
fn a_group_nobody_visible_follows_opens_no_barrier(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = two_tiles(cx);
    follow(&frame, &mut vcx, TileId(1), Some(Group::A));
    // A third tile in workspace 2 follows B; workspace 1 is shown.
    super::occupants::dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    add_tile(&mut vcx);
    let hidden = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().tree().tiles()[0]
    });
    let ws2 = shell.read_with(&vcx, |s, _| s.active_ix());
    super::occupants::dispatch_and_draw(&shell, &mut vcx, "workspace::switch_1");
    follow(&frame, &mut vcx, hidden, Some(Group::B));

    frame.update(&mut vcx, |f, cx| {
        assert!(f.view_mut_for(ws2, hidden).set_scope(underlying("SPX.Z")));
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_open()),
        "B's only follower is hidden"
    );

    // With a lane barrier open over the visible tiles, the same change
    // leaves it waiting.
    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_text(Some("ndx".into())));
        cx.notify();
    });
    vcx.run_until_parked();
    let lane = frame.read_with(&vcx, |f, _| f.view(WS1).versions());
    assert!(frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(2), lane)));
    frame.update(&mut vcx, |f, cx| {
        assert!(f.view_mut_for(ws2, hidden).set_scope(underlying("NDX")));
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(2), lane)),
        "a hidden group's change does not clear the lane's barrier"
    );
}

/// Following changes one tile's scope; that tile requeries by itself and
/// nothing else has to wait for it.
#[gpui::test]
fn a_follow_alone_opens_no_barrier(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, _shell, frame) = two_tiles(cx);
    follow(&frame, &mut vcx, TileId(1), Some(Group::A));
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()));
}

/// A group's scope lives in no session file, yet moving it advances the
/// frame generation the writer treats as dirt. The snapshot it then takes
/// is the text already written, and is not written again: an emitting
/// tile's cursor would otherwise rewrite the session on every move.
#[gpui::test]
fn a_group_scope_change_writes_no_session_file(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = super::session::test_services_with_session(dir.path().join("session.toml"));
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    follow(&frame, &mut vcx, TileId(1), Some(Group::A));
    assert!(
        shell
            .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
            .is_some(),
        "the baseline: the layout and the follow are written once"
    );

    frame.update(&mut vcx, |f, cx| {
        assert!(
            f.view_mut_for(WS1, TileId(1))
                .set_scope(underlying("SPX.Z"))
        );
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        shell
            .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
            .is_none(),
        "a group's scope is not session state"
    );
    // A membership change is session state and must be written. That half
    // is asserted by `the_session_text_carries_a_membership_change`, once
    // the session file carries `follow` and `emit`.
}
