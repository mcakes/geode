//! Link groups at the shell: a group's scope change flips its visible
//! followers through the barrier, the session writer skips a snapshot whose
//! text did not change, and the shell pulls an emitting tile's emission
//! into its group. The recorder submits no query, so the barrier tests read
//! what the barrier awaits instead of waiting for an arrival.

use super::*;
use crate::frame::Frame;
use crate::module::recording::{Recorded, RecordingFactory};
use crate::shell::choicedialog::{NO_TILE_TO_LINK, TILE_GONE, Target};
use crate::tiling::WorkspaceIx;
use geode_core::document::DocumentRows;
use geode_core::link::{BoardEntry, Emission, Group};
use geode_core::query::{AsOf, QueryKey};
use geode_core::scope::Scope;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

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

    // The change is flipped once. A baseline that kept the group's old
    // number would read every later notification as the same change again:
    // the release notifies, the barrier reopens, the follower answers at
    // once, and the frame never comes to rest.
    settle(&frame, &mut vcx);
    frame.update(&mut vcx, |f, cx| {
        f.note_config_reloaded();
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        !frame.read_with(&vcx, |f, _| f.barrier_open()),
        "an unrelated notification after the flip opens nothing"
    );
}

/// A group's scope change while a lane flip is still in progress joins it.
/// Replacing the barrier would drop the tiles that do not follow the group
/// while their queries are in flight: they would then paint on arrival,
/// beside tiles still holding what they staged.
#[gpui::test]
fn a_group_scope_change_under_an_open_lane_barrier_keeps_the_other_tiles_awaited(
    cx: &mut gpui::TestAppContext,
) {
    let (_window, mut vcx, _shell, frame) = two_tiles(cx);
    follow(&frame, &mut vcx, TileId(1), Some(Group::A));
    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now())));
        cx.notify();
    });
    vcx.run_until_parked();
    let lane = frame.read_with(&vcx, |f, _| f.view(WS1).versions());
    let enrolled = frame.read_with(&vcx, |f, _| f.view_for(WS1, TileId(1)).versions());
    assert!(frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), enrolled)));
    assert!(frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(2), lane)));

    frame.update(&mut vcx, |f, cx| {
        assert!(
            f.view_mut_for(WS1, TileId(1))
                .set_scope(underlying("SPX.Z"))
        );
        cx.notify();
    });
    vcx.run_until_parked();

    let follower = frame.read_with(&vcx, |f, _| f.view_for(WS1, TileId(1)).versions());
    assert_ne!(follower.scope, enrolled.scope, "sanity: its identity moved");
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(2), lane)),
        "the tile that does not follow the group is still awaited"
    );
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), follower)),
        "the follower is awaited under its new identity"
    );
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), enrolled)));
}

/// Group generations are frame-wide, not per workspace, so a workspace
/// switch has nothing of theirs to re-seed. Re-seeded there, a group change
/// whose notification is still pending when the switch happens would be
/// taken for already seen, and the followers now on screen never flipped.
#[gpui::test]
fn a_group_change_pending_across_a_workspace_switch_still_flips(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = two_tiles(cx);
    super::occupants::dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    add_tile(&mut vcx);
    let hidden = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().tree().tiles()[0]
    });
    let ws2 = shell.read_with(&vcx, |s, _| s.active_ix());
    super::occupants::dispatch_and_draw(&shell, &mut vcx, "workspace::switch_1");
    follow(&frame, &mut vcx, hidden, Some(Group::B));
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()));

    // One update: the group's scope moves, then the workspace switches
    // before the frame's notification is delivered.
    vcx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.frame().clone().update(cx, |f, cx| {
                assert!(f.view_mut_for(ws2, hidden).set_scope(underlying("SPX.Z")));
                cx.notify();
            });
            s.dispatch(
                &ActionId("workspace::switch_2".to_string()),
                None,
                window,
                cx,
            );
        });
    });
    vcx.run_until_parked();

    let follower = frame.read_with(&vcx, |f, _| f.view_for(ws2, hidden).versions());
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(hidden.0), follower)),
        "the follower now on screen is flipped for the group's change"
    );
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
/// nothing else has to wait for it. A guard: no flip code has to exist for
/// it to pass, and it fails only if a follow starts opening a barrier.
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
    // A membership change is session state and must be written:
    // `the_session_text_carries_a_membership_change` asserts that half.
}

// --- The emission pull ----------------------------------------------

/// The cells of a recorder whose tiles can emit.
struct Emitter {
    /// What every "rec" tile answers from `emission()`.
    emission: Rc<RefCell<Emission>>,
    /// How many times the shell pulled.
    pulls: Rc<Cell<usize>>,
}

/// Services whose "rec" tiles can emit, answering `emission`.
fn emitting_services(emission: Emission) -> (ShellServices, Emitter) {
    let mut rec = RecordingFactory::new("rec");
    rec.emits = true;
    *rec.emission.borrow_mut() = emission;
    let emitter = Emitter {
        emission: rec.emission.clone(),
        pulls: rec.pulls.clone(),
    };
    (services_with_recorders(vec![rec]), emitter)
}

fn draft() -> Arc<DocumentRows> {
    Arc::new(DocumentRows {
        key: Vec::new(),
        attributes: Vec::new(),
        axes: Vec::new(),
        values: Vec::new(),
    })
}

const DRAFTS: &str = "cvi_params";

/// A cursor on `u` holding one draft for it.
fn emission_for(u: &str, rows: &Arc<DocumentRows>) -> Emission {
    Emission {
        scope: Some(underlying(u)),
        board: vec![BoardEntry {
            dataset: DRAFTS.into(),
            key: vec![u.into()],
            rows: Arc::clone(rows),
        }],
    }
}

fn on_board(frame: &Entity<Frame>, vcx: &gpui::VisualTestContext, g: Group, u: &str) -> bool {
    frame.read_with(vcx, |f, _| {
        f.board_entry(g, DRAFTS, &[u.to_string()]).is_some()
    })
}

/// The one underlying `g`'s scope names.
fn group_underlying(
    frame: &Entity<Frame>,
    vcx: &gpui::VisualTestContext,
    g: Group,
) -> Option<String> {
    frame.read_with(vcx, |f, _| {
        f.group_scope(g).sole("underlying_ref").map(str::to_owned)
    })
}

/// Say `tile`'s emission may have changed, the way a module does: its own
/// entity notifies, and whoever watches the emission hears it.
fn tile_changed(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext, tile: TileId) {
    let view = shell.read_with(vcx, |s, _| s.occupants[&tile].view.entity_id());
    vcx.update(|_, cx| cx.notify(view));
    vcx.run_until_parked();
}

/// Count the frame's notifications from here on.
fn frame_notifications(
    frame: &Entity<Frame>,
    vcx: &mut gpui::VisualTestContext,
) -> Rc<Cell<usize>> {
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    vcx.update(|_, cx| {
        cx.observe(frame, move |_, _| seen.set(seen.get() + 1))
            .detach();
    });
    vcx.run_until_parked();
    count
}

fn set_emit(
    shell: &Entity<ShellView>,
    vcx: &mut gpui::VisualTestContext,
    tile: TileId,
    group: Option<Group>,
) {
    shell.update(vcx, |s, cx| s.set_emit(tile, group, cx));
    vcx.run_until_parked();
}

fn set_follow(
    shell: &Entity<ShellView>,
    vcx: &mut gpui::VisualTestContext,
    tile: TileId,
    group: Option<Group>,
) {
    shell.update(vcx, |s, cx| s.set_follow(tile, group, cx));
    vcx.run_until_parked();
}

/// Joining a group posts what the tile holds now, with no change needed to
/// trigger it, and every later change the tile announces is pulled.
#[gpui::test]
fn an_emitting_tile_posts_at_once_and_again_when_it_says_it_changed(cx: &mut gpui::TestAppContext) {
    let (services, emitter) = emitting_services(Emission {
        scope: Some(underlying("SPX.Z")),
        board: Vec::new(),
    });
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);

    // Read inside the same update, before any notification is delivered:
    // the join itself posts. The tile's repaint also reaches the watch and
    // would post a moment later, but only for a module that watches the
    // entity the shell repaints.
    let heard = shell.update(&mut vcx, |s, cx| {
        s.set_emit(TileId(1), Some(Group::A), cx);
        let scope = s.frame().read(cx).group_scope(Group::A);
        scope.sole("underlying_ref").map(str::to_owned)
    });
    assert_eq!(
        heard.as_deref(),
        Some("SPX.Z"),
        "the group hears the tile's emission as it joins"
    );
    vcx.run_until_parked();

    emitter.emission.borrow_mut().scope = Some(underlying("NDX"));
    tile_changed(&shell, &mut vcx, TileId(1));
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("NDX"),
        "and again when the tile says it changed"
    );
}

/// A tile notifies for many reasons; the shell pulls each time and an
/// emission equal to the last is not a write. A write here would notify
/// every tile and dirty the session on each repaint of an emitter.
#[gpui::test]
fn a_change_with_the_same_emission_writes_nothing(cx: &mut gpui::TestAppContext) {
    let rows = draft();
    let (services, emitter) = emitting_services(emission_for("SPX.Z", &rows));
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    set_emit(&shell, &mut vcx, TileId(1), Some(Group::A));
    assert!(on_board(&frame, &vcx, Group::A, "SPX.Z"));

    let before = frame.read_with(&vcx, |f, _| (f.generation(), f.board_gen(Group::A)));
    let pulls = emitter.pulls.get();
    let notified = frame_notifications(&frame, &mut vcx);
    tile_changed(&shell, &mut vcx, TileId(1));

    assert!(emitter.pulls.get() > pulls, "the shell did pull");
    assert_eq!(
        frame.read_with(&vcx, |f, _| (f.generation(), f.board_gen(Group::A))),
        before,
        "an unchanged emission moves no counter"
    );
    assert_eq!(notified.get(), 0, "and notifies nobody");
}

/// An emitting tile whose cursor and draft both move is one board write
/// per move. It is not a publish, so the frame's data counter (which
/// requeries every tile watching a dataset) stays put. The group's scope
/// does advance the frame generation, the session writer's dirty signal,
/// but neither it nor the draft is session state: the snapshot's text is
/// the one already written, and nothing goes to disk at cursor speed.
#[gpui::test]
fn a_changed_draft_is_one_board_write_and_no_session_write(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (mut services, emitter) = emitting_services(emission_for("SPX.Z", &draft()));
    services.session_path = Some(dir.path().join("session.toml"));
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    set_emit(&shell, &mut vcx, TileId(1), Some(Group::A));
    assert!(
        shell
            .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
            .is_some(),
        "the baseline"
    );
    let counters = |vcx: &gpui::VisualTestContext| {
        frame.read_with(vcx, |f, _| (f.board_gen(Group::A), f.data_version()))
    };
    let (board, data) = counters(&vcx);
    let generation = frame.read_with(&vcx, |f, _| f.generation());

    // The cursor moved to another underlying, with a draft for it.
    *emitter.emission.borrow_mut() = emission_for("NDX", &draft());
    tile_changed(&shell, &mut vcx, TileId(1));
    assert_eq!(counters(&vcx), (board + 1, data));
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("NDX")
    );
    assert!(
        frame.read_with(&vcx, |f, _| f.generation()) > generation,
        "sanity: the scope change made the session writer look"
    );
    tile_changed(&shell, &mut vcx, TileId(1));
    assert_eq!(
        counters(&vcx),
        (board + 1, data),
        "a repeated pull is no write"
    );
    assert!(
        shell
            .update(&mut vcx, |s, cx| s.take_dirty_session_write(cx))
            .is_none(),
        "neither a group's scope nor a draft on its board is session state"
    );
}

/// A tile that stops emitting takes its drafts off the board at once and
/// is no longer listened to: a subscription kept past the membership would
/// pull a tile for nothing on its every repaint.
#[gpui::test]
fn leaving_the_group_drops_the_subscription_and_the_board_entry(cx: &mut gpui::TestAppContext) {
    let rows = draft();
    let (services, emitter) = emitting_services(emission_for("SPX.Z", &rows));
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    set_emit(&shell, &mut vcx, TileId(1), Some(Group::A));
    assert!(on_board(&frame, &vcx, Group::A, "SPX.Z"));
    assert_eq!(shell.read_with(&vcx, |s, _| s.emit_subs.len()), 1);

    set_emit(&shell, &mut vcx, TileId(1), None);
    assert!(
        !on_board(&frame, &vcx, Group::A, "SPX.Z"),
        "its draft leaves the board at once"
    );
    assert!(shell.read_with(&vcx, |s, _| s.emit_subs.is_empty()));
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z"),
        "the group's scope stays as last written"
    );

    let pulls = emitter.pulls.get();
    *emitter.emission.borrow_mut() = emission_for("NDX", &rows);
    tile_changed(&shell, &mut vcx, TileId(1));
    assert_eq!(emitter.pulls.get(), pulls, "nothing pulls a tile that left");
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z")
    );
    assert!(!on_board(&frame, &vcx, Group::A, "NDX"));
}

/// Switching group is leaving one and joining the other: the drafts move
/// with the tile, the new group hears it at once, and the old group keeps
/// the scope it was last given.
#[gpui::test]
fn switching_group_moves_the_drafts_and_leaves_the_old_scope(cx: &mut gpui::TestAppContext) {
    let rows = draft();
    let (services, _emitter) = emitting_services(emission_for("SPX.Z", &rows));
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    set_emit(&shell, &mut vcx, TileId(1), Some(Group::A));

    // Read inside the update that switched, before any notification is
    // delivered: the join into B posts by itself. The tile's repaint would
    // post a moment later, but only for a module that watches the entity
    // the shell repaints.
    let (left_a, joined_b) = shell.update(&mut vcx, |s, cx| {
        s.set_emit(TileId(1), Some(Group::B), cx);
        let f = s.frame().read(cx);
        let key = ["SPX.Z".to_string()];
        (
            f.board_entry(Group::A, DRAFTS, &key).is_none(),
            f.board_entry(Group::B, DRAFTS, &key).is_some(),
        )
    });
    assert!(left_a, "the draft leaves the old board with the tile");
    assert!(joined_b, "and is on the new board as the tile joins");
    vcx.run_until_parked();
    assert!(!on_board(&frame, &vcx, Group::A, "SPX.Z"));
    assert!(on_board(&frame, &vcx, Group::B, "SPX.Z"));
    assert_eq!(
        group_underlying(&frame, &vcx, Group::B).as_deref(),
        Some("SPX.Z")
    );
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z"),
        "the group it left keeps its scope"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.emit_subs.len()),
        1,
        "one subscription, for the group it emits into now"
    );
}

/// A closed tile is in no group: its drafts leave the board, its
/// membership does not survive to a tile that later takes the id, the
/// shell stops listening to it, and the tiles reading the board are told.
#[gpui::test]
fn closing_an_emitting_tile_removes_its_membership_and_entries(cx: &mut gpui::TestAppContext) {
    let rows = draft();
    let (services, _emitter) = emitting_services(emission_for("SPX.Z", &rows));
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    let tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    set_emit(&shell, &mut vcx, tile, Some(Group::A));
    assert!(on_board(&frame, &vcx, Group::A, "SPX.Z"));
    let notified = frame_notifications(&frame, &mut vcx);

    vcx.simulate_keystrokes("ctrl-w");
    draw(&mut vcx);
    vcx.run_until_parked();

    assert_eq!(shell.read_with(&vcx, |s, _| s.occupant_kind(tile)), None);
    assert!(frame.read_with(&vcx, |f, _| f.membership(tile).is_empty()));
    assert!(!on_board(&frame, &vcx, Group::A, "SPX.Z"));
    assert!(shell.read_with(&vcx, |s, _| s.emit_subs.is_empty()));
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z"),
        "the group's scope stays as last written"
    );
    assert!(
        notified.get() >= 1,
        "a board that lost a draft is a change its readers must hear"
    );
}

/// A tile following the group it emits into hears its own emission come
/// back as a frame change. It repaints, the shell pulls again, and the
/// equal emission ends it there: one notification per actual change.
#[gpui::test]
fn a_tile_that_follows_and_emits_one_group_does_not_loop(cx: &mut gpui::TestAppContext) {
    let (services, emitter) = emitting_services(Emission {
        scope: Some(underlying("SPX.Z")),
        board: Vec::new(),
    });
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    let tile = TileId(1);
    // Stand in for a following tile: every frame change makes it notify,
    // as a requery's result would. Capped, so a loop fails the count below
    // instead of hanging the test.
    let view = shell.read_with(&vcx, |s, _| s.occupants[&tile].view.entity_id());
    let notified = Rc::new(Cell::new(0usize));
    let seen = notified.clone();
    vcx.update(|_, cx| {
        cx.observe(&frame, move |_, cx| {
            seen.set(seen.get() + 1);
            if seen.get() < 50 {
                cx.notify(view);
            }
        })
        .detach();
    });
    vcx.run_until_parked();

    set_follow(&shell, &mut vcx, tile, Some(Group::A));
    assert_eq!(notified.get(), 1, "the follow");
    set_emit(&shell, &mut vcx, tile, Some(Group::A));
    assert_eq!(notified.get(), 2, "joining and its first post are one pass");
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z")
    );

    emitter.emission.borrow_mut().scope = Some(underlying("NDX"));
    tile_changed(&shell, &mut vcx, tile);
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("NDX")
    );
    assert_eq!(notified.get(), 3, "one notification for the one change");
}

/// The tile's header shows its membership, and the tile's view is its own
/// entity: the shell repainting does not repaint it.
#[gpui::test]
fn set_follow_repaints_the_tile(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.emits = true;
    let repaints = rec.repaints.clone();
    let (_window, mut vcx, shell, _frame) = two_tiles_in(cx, services_with_recorders(vec![rec]));
    vcx.run_until_parked();
    let of = |tile: TileId| repaints.borrow().iter().filter(|t| **t == tile).count();
    let (one, two) = (of(TileId(1)), of(TileId(2)));

    set_follow(&shell, &mut vcx, TileId(1), Some(Group::A));
    assert_eq!(of(TileId(1)), one + 1, "the tile that followed repaints");
    assert_eq!(of(TileId(2)), two, "the other does not");

    set_follow(&shell, &mut vcx, TileId(1), Some(Group::A));
    assert_eq!(
        of(TileId(1)),
        one + 1,
        "an unchanged follow repaints nothing"
    );

    set_emit(&shell, &mut vcx, TileId(1), Some(Group::B));
    assert_eq!(
        of(TileId(1)),
        one + 2,
        "a tile that starts emitting repaints"
    );
}

/// A follow changes the identity a tile answers the barrier under. With a
/// barrier already waiting on it, the awaited key moves to the new identity
/// or the tile's next arrival would not count.
#[gpui::test]
fn a_follow_under_an_open_barrier_reidentifies_the_tiles_key(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = two_tiles(cx);
    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now())));
        cx.notify();
    });
    vcx.run_until_parked();
    let lane = frame.read_with(&vcx, |f, _| f.view(WS1).versions());
    assert!(frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), lane)));

    set_follow(&shell, &mut vcx, TileId(1), Some(Group::A));

    let follower = frame.read_with(&vcx, |f, _| f.view_for(WS1, TileId(1)).versions());
    assert_ne!(follower.scope, lane.scope, "sanity: its identity moved");
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), follower)),
        "the tile is now awaited under the identity it will answer with"
    );
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(1), lane)));
    assert!(
        frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(2), lane)),
        "the other tile's key is untouched"
    );
}

/// A closing follower answers the barrier under the identity it was
/// enrolled with, which is its group's while it is still a member. Told
/// after its membership was dropped, it would answer under the lane's and
/// leave its key awaited until the deadline.
#[gpui::test]
fn a_closing_follower_hears_closed_before_its_membership_is_dropped(cx: &mut gpui::TestAppContext) {
    let rec = RecordingFactory::new("rec");
    let followed_at_close = rec.followed_at_close.clone();
    let log = rec.log.clone();
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services_with_recorders(vec![rec]));
    let tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    set_follow(&shell, &mut vcx, tile, Some(Group::A));

    vcx.simulate_keystrokes("ctrl-w");
    draw(&mut vcx);
    vcx.run_until_parked();

    assert!(
        log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Closed(t) if *t == tile))
    );
    assert_eq!(
        *followed_at_close.borrow(),
        vec![(tile, Some(Group::A))],
        "the closing tile still read its group"
    );
    assert!(
        frame.read_with(&vcx, |f, _| f.membership(tile).is_empty()),
        "and the membership is gone afterwards"
    );
}

/// The shell hands every occupant a frame handle bound to its own tile. A
/// handle bound to the workspace alone keeps reading the lane's scope
/// after the tile follows a group.
#[gpui::test]
fn an_occupants_frame_handle_is_bound_to_its_own_tile(cx: &mut gpui::TestAppContext) {
    let rec = RecordingFactory::new("rec");
    let handles = rec.frame_handles.clone();
    let (_window, mut vcx, shell, _frame) = two_tiles_in(cx, services_with_recorders(vec![rec]));
    let handle = |tile: TileId| handles.borrow()[&tile].clone();
    assert_eq!(handle(TileId(1)).tile(), Some(TileId(1)));
    assert_eq!(handle(TileId(2)).tile(), Some(TileId(2)));

    set_follow(&shell, &mut vcx, TileId(1), Some(Group::A));
    let following = |tile: TileId| vcx.read(|cx| handle(tile).read(cx).following());
    assert_eq!(following(TileId(1)), Some(Group::A));
    assert_eq!(following(TileId(2)), None);
}

/// Only a tile that answers `emits()` can be set to emit. A membership
/// that reached the frame for a tile that cannot (a session written by a
/// build whose module could) is cleared instead of left subscribing to
/// nothing.
#[gpui::test]
fn a_tile_that_cannot_emit_is_never_set_to_emit(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, frame) = two_tiles(cx);
    set_emit(&shell, &mut vcx, TileId(1), Some(Group::A));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(1)).emit),
        None
    );
    assert!(shell.read_with(&vcx, |s, _| s.emit_subs.is_empty()));

    frame.update(&mut vcx, |f, cx| {
        assert!(f.emit(TileId(1), Some(Group::A)));
        cx.notify();
    });
    set_emit(&shell, &mut vcx, TileId(1), Some(Group::A));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(1)).emit),
        None,
        "a stale membership is cleared"
    );
}

/// Filling a placeholder replaces its occupant under the same tile id. The
/// new occupant starts in no group: a membership left on the id would bind
/// a tile the trader never linked.
#[gpui::test]
fn filling_a_placeholder_drops_the_membership_its_tile_had(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    shell.update(&mut vcx, |s, cx| {
        s.services
            .workspaces
            .split_active(crate::tiling::Orientation::Horizontal);
        cx.notify();
    });
    draw(&mut vcx);
    let tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupant_kind(tile)),
        Some(crate::module::placeholder::PLACEHOLDER_KIND)
    );
    // Written through the frame: the shell's door refuses a placeholder,
    // so a membership can only be on one by some other route.
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    follow(&frame, &mut vcx, tile, Some(Group::A));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(tile).follow),
        Some(Group::A)
    );

    super::occupants::dispatch_and_draw(&shell, &mut vcx, "tile::add_rec");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
    assert!(frame.read_with(&vcx, |f, _| f.membership(tile).is_empty()));
}

// --- Membership in the session --------------------------------------

/// `services` as a restart hands them over. Each `(index, body)` is one
/// workspace's table as the session file spells it, read back through the
/// session reader; workspace 1 is the active one.
fn restored_layout(mut services: ShellServices, workspaces: &[(u8, &str)]) -> ShellServices {
    let mut table = crate::session::to_toml(
        &Workspaces::new(),
        &crate::session::TileRecords::new(),
        None,
        &crate::session::PinnedRecords::new(),
        &crate::palette_usage::PaletteUsage::new(),
        &crate::session::PageRecords::new(),
    );
    if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
        for (ix, body) in workspaces {
            let body: toml::Table = body.parse().unwrap();
            ws_table.insert(ix.to_string(), toml::Value::Table(body));
        }
    }
    let restored = crate::session::from_toml(&table).unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    services.workspaces = restored.workspaces;
    services.restored_tiles = restored.tiles;
    services
}

/// [`restored_layout`] with one leaf in workspace 1, tile 1, whose session
/// record is `record` (the lines under `[workspaces.1.tiles.1]`).
fn restored(services: ShellServices, record: &str) -> ShellServices {
    let ws1 = format!("focused = 1\n[node]\nkind = \"leaf\"\nid = 1\n[tiles.1]\n{record}\n");
    restored_layout(services, &[(1, &ws1)])
}

/// Workspace 1 as two leaves side by side, tiles 1 and 2, with these
/// session records.
fn two_restored(services: ShellServices, first: &str, second: &str) -> ShellServices {
    let ws1 = format!(
        "focused = 1\n\
         [node]\nkind = \"split\"\norientation = \"horizontal\"\nratios = [0.5, 0.5]\n\
         [[node.children]]\nkind = \"leaf\"\nid = 1\n\
         [[node.children]]\nkind = \"leaf\"\nid = 2\n\
         [tiles.1]\n{first}\n[tiles.2]\n{second}\n"
    );
    restored_layout(services, &[(1, &ws1)])
}

fn frame_of(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Entity<Frame> {
    shell.read_with(vcx, |s, _| s.frame().clone())
}

/// The next periodic session snapshot's text, if one is due.
fn session_text(shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext) -> Option<String> {
    shell
        .update(vcx, |s, cx| s.take_dirty_session_write(cx))
        .map(|(_, text)| text)
}

/// A follower's first query is scoped by its group. A membership applied
/// after the occupant exists would let that first query read the
/// workspace's scope and answer for the wrong book.
#[gpui::test]
fn a_restored_follower_reads_its_group_on_its_first_frame(cx: &mut gpui::TestAppContext) {
    let rec = RecordingFactory::new("rec");
    let at_create = rec.followed_at_create.clone();
    let services = restored(
        services_with_recorders(vec![rec]),
        "module = \"rec\"\nfollow = \"a\"",
    );
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(1))),
        geode_core::link::Membership {
            follow: Some(Group::A),
            emit: None,
        }
    );
    assert_eq!(
        *at_create.borrow(),
        vec![(TileId(1), Some(Group::A))],
        "the occupant read its group from inside `create`"
    );
}

/// A restored membership is not a group-scope change. Following or
/// emitting moves no lane version and no group's scope generation, which
/// are the two things a flip is detected by, so the first notification of
/// the frame after a restore, whatever it is for, opens no barrier.
#[gpui::test]
fn a_restored_membership_is_not_a_group_scope_change_and_opens_no_barrier(
    cx: &mut gpui::TestAppContext,
) {
    let services = restored(test_services(), "module = \"rec\"\nfollow = \"a\"");
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(1)).follow),
        Some(Group::A),
        "fixture: the follower was restored"
    );
    let notified = frame_notifications(&frame, &mut vcx);

    frame.update(&mut vcx, |_, cx| cx.notify());
    vcx.run_until_parked();

    assert_eq!(notified.get(), 1, "fixture: the notification was delivered");
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()));
    let (seen, now) = shell.read_with(&vcx, |s, cx| {
        (s.last_flip_groups, s.frame().read(cx).group_scope_gens())
    });
    assert_eq!(seen, now);
}

/// An emitter saved in a group is heard again after a restart without the
/// trader touching it: the shell subscribes and pulls once the occupant
/// exists.
#[gpui::test]
fn a_restored_emitter_subscribes_and_posts_without_a_key_press(cx: &mut gpui::TestAppContext) {
    let (services, emitter) = emitting_services(Emission {
        scope: Some(underlying("SPX.Z")),
        board: Vec::new(),
    });
    let services = restored(services, "module = \"rec\"\nemit = \"a\"");
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    draw(&mut vcx);
    vcx.run_until_parked();

    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z"),
        "the group hears the restored emitter"
    );
    assert_eq!(shell.read_with(&vcx, |s, _| s.emit_subs.len()), 1);
    // The post reached the frame's observers: a group whose scope moved
    // without a notification would leave its followers on the old scope.
    let (seen, now) = shell.read_with(&vcx, |s, cx| {
        (s.last_flip_groups, s.frame().read(cx).group_scope_gens())
    });
    assert_eq!(seen, now, "the shell heard the group's scope move");

    emitter.emission.borrow_mut().scope = Some(underlying("NDX"));
    tile_changed(&shell, &mut vcx, TileId(1));
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("NDX"),
        "and it is subscribed: a later change is pulled"
    );
}

/// A session written by a build whose module could emit, read by one whose
/// module cannot: the membership is cleared once the occupant exists,
/// instead of a tile shown as emitting that the shell never listens to.
#[gpui::test]
fn a_restored_emit_on_a_tile_that_cannot_emit_is_dropped(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = restored(
        super::session::test_services_with_session(dir.path().join("session.toml")),
        "module = \"rec\"\nfollow = \"b\"\nemit = \"a\"",
    );
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    vcx.run_until_parked();

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(1))),
        geode_core::link::Membership {
            follow: Some(Group::B),
            emit: None,
        },
        "the emit is dropped and the follow beside it kept"
    );
    assert!(shell.read_with(&vcx, |s, _| s.emit_subs.is_empty()));
    let text = session_text(&shell, &mut vcx).expect("the session is written");
    assert!(text.contains("follow = \"b\""), "{text}");
    assert!(!text.contains("emit"), "{text}");
}

/// A tile whose module this build lacks paints a placeholder, which is in
/// no group. Its record still carries the membership, so a build that has
/// the module again restores it.
#[gpui::test]
fn a_saved_membership_survives_a_build_without_the_module(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let services = restored(
        super::session::test_services_with_session(dir.path().join("session.toml")),
        "module = \"gone\"\nfollow = \"a\"\nemit = \"b\"",
    );
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    draw(&mut vcx);
    vcx.run_until_parked();

    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupant_kind(TileId(1))),
        Some(crate::module::placeholder::PLACEHOLDER_KIND)
    );
    assert!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(1)).is_empty()),
        "no membership is applied to a placeholder"
    );
    let saved = geode_core::link::Membership {
        follow: Some(Group::A),
        emit: Some(Group::B),
    };
    let tiles = shell.read_with(&vcx, |s, cx| s.current_tiles(cx));
    assert_eq!(tiles[&1].kind, "gone");
    assert_eq!(tiles[&1].link, saved);
    let text = session_text(&shell, &mut vcx).expect("the session is written");
    assert!(
        text.contains("follow = \"a\"") && text.contains("emit = \"b\""),
        "the next save still writes both keys: {text}"
    );
}

/// A record for a tile no workspace holds (one the layout's healing left
/// behind) restores no membership. Such a tile never has an occupant, so
/// nothing would ever unlink it and it would sit in its group for the life
/// of the shell.
#[gpui::test]
fn a_record_for_a_tile_in_no_workspace_restores_no_membership(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    services.restored_tiles.insert(
        9,
        crate::session::TileRecord {
            kind: "rec".into(),
            state: toml::Table::new(),
            link: geode_core::link::Membership {
                follow: Some(Group::A),
                emit: Some(Group::B),
            },
        },
    );
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    assert!(frame.read_with(&vcx, |f, _| f.membership(TileId(9)).is_empty()));

    add_tile(&mut vcx);
    draw(&mut vcx);
    vcx.run_until_parked();
    assert!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(9)).is_empty()),
        "and none appears later"
    );
}

/// "Placed" means in any workspace, not the one on screen: a follower in a
/// workspace the trader is not looking at is still a follower when they
/// switch to it.
#[gpui::test]
fn a_membership_in_an_inactive_workspace_survives_restore(cx: &mut gpui::TestAppContext) {
    let rec = RecordingFactory::new("rec");
    let at_create = rec.followed_at_create.clone();
    let leaf = |id: u8, record: &str| {
        format!(
            "focused = {id}\n[node]\nkind = \"leaf\"\nid = {id}\n\
             [tiles.{id}]\nmodule = \"rec\"\n{record}\n"
        )
    };
    let services = restored_layout(
        services_with_recorders(vec![rec]),
        &[(1, &leaf(1, "")), (2, &leaf(2, "follow = \"a\""))],
    );
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    assert_eq!(shell.read_with(&vcx, |s, _| s.active_ix()), WS1);
    draw(&mut vcx);
    vcx.run_until_parked();
    draw(&mut vcx);

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(2)).follow),
        Some(Group::A)
    );
    assert!(
        at_create.borrow().contains(&(TileId(2), Some(Group::A))),
        "its occupant read the group at create: {:?}",
        at_create.borrow()
    );
}

/// A tile in a hidden dock is placed too.
#[gpui::test]
fn a_membership_in_a_hidden_dock_survives_restore(cx: &mut gpui::TestAppContext) {
    let rec = RecordingFactory::new("rec");
    let at_create = rec.followed_at_create.clone();
    let ws1 = "focused = 1\n\
               [node]\nkind = \"leaf\"\nid = 1\n\
               [docks.left]\nfocused = 3\nvisible = false\nsize = 0.25\n\
               [docks.left.node]\nkind = \"leaf\"\nid = 3\n\
               [tiles.1]\nmodule = \"rec\"\n\
               [tiles.3]\nmodule = \"rec\"\nfollow = \"b\"\n";
    let services = restored_layout(services_with_recorders(vec![rec]), &[(1, ws1)]);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    assert!(
        shell.read_with(&vcx, |s, _| {
            let dock = s.services.workspaces.active().docks();
            let left = dock.get(crate::tiling::DockSide::Left);
            !left.visible() && left.tree().tiles() == vec![TileId(3)]
        }),
        "fixture: tile 3 is in the hidden left dock"
    );
    draw(&mut vcx);
    vcx.run_until_parked();
    draw(&mut vcx);

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(TileId(3)).follow),
        Some(Group::B)
    );
    assert!(
        at_create.borrow().contains(&(TileId(3), Some(Group::B))),
        "its occupant read the group at create: {:?}",
        at_create.borrow()
    );
}

/// The frame is not written while occupants are being created. A restored
/// emitter posts once the render is over, so every occupant of that first
/// pass reads the same frame whatever order the tile ids sort in: a
/// follower created after its emitter does not start on a scope the one
/// created before it never saw.
#[gpui::test]
fn a_restored_emitter_posts_after_every_occupant_exists(cx: &mut gpui::TestAppContext) {
    let mut rec = RecordingFactory::new("rec");
    rec.emits = true;
    *rec.emission.borrow_mut() = Emission {
        scope: Some(underlying("SPX.Z")),
        board: Vec::new(),
    };
    let at_create = rec.generation_at_create.clone();
    let services = two_restored(
        services_with_recorders(vec![rec]),
        "module = \"rec\"\nemit = \"a\"",
        "module = \"rec\"\nfollow = \"a\"",
    );
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = frame_of(&shell, &vcx);
    vcx.run_until_parked();

    let seen = at_create.borrow().clone();
    assert_eq!(
        seen.iter().map(|(tile, _)| *tile).collect::<Vec<_>>(),
        vec![TileId(1), TileId(2)],
        "fixture: the emitter is created first"
    );
    assert_eq!(
        seen[0].1, seen[1].1,
        "both occupants read the same frame generation at create"
    );
    assert!(
        frame.read_with(&vcx, |f, _| f.generation()) > seen[0].1,
        "the post came afterwards"
    );
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z")
    );
}

/// The doors act on a tile a module occupies. An id with no occupant, or a
/// placeholder's, is refused before the frame is touched: nothing would
/// ever unlink a membership written for it.
#[gpui::test]
fn the_doors_refuse_a_tile_with_no_occupant(cx: &mut gpui::TestAppContext) {
    let (services, _emitter) = emitting_services(Emission::default());
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    shell.update(&mut vcx, |s, cx| {
        s.services
            .workspaces
            .split_active(crate::tiling::Orientation::Horizontal);
        cx.notify();
    });
    draw(&mut vcx);
    vcx.run_until_parked();
    let placeholder = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupant_kind(placeholder)),
        Some(crate::module::placeholder::PLACEHOLDER_KIND)
    );
    let unknown = TileId(99);
    assert_eq!(shell.read_with(&vcx, |s, _| s.occupant_kind(unknown)), None);
    let generation = frame.read_with(&vcx, |f, _| f.generation());
    let notified = frame_notifications(&frame, &mut vcx);

    for tile in [unknown, placeholder] {
        set_follow(&shell, &mut vcx, tile, Some(Group::A));
        set_emit(&shell, &mut vcx, tile, Some(Group::B));
        assert!(
            frame.read_with(&vcx, |f, _| f.membership(tile).is_empty()),
            "{tile:?}"
        );
    }
    assert_eq!(frame.read_with(&vcx, |f, _| f.generation()), generation);
    assert_eq!(notified.get(), 0);
}

/// The other half of `a_group_scope_change_writes_no_session_file`: who
/// follows and emits is session state, and a change to it is written.
#[gpui::test]
fn the_session_text_carries_a_membership_change(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (mut services, _emitter) = emitting_services(Emission::default());
    services.session_path = Some(dir.path().join("session.toml"));
    let (_window, mut vcx, shell, _frame) = two_tiles_in(cx, services);
    set_follow(&shell, &mut vcx, TileId(1), Some(Group::A));
    let text = session_text(&shell, &mut vcx).expect("the baseline");
    assert!(text.contains("follow = \"a\""), "{text}");
    assert!(!text.contains("emit"), "{text}");

    set_follow(&shell, &mut vcx, TileId(1), Some(Group::B));
    let text = session_text(&shell, &mut vcx).expect("a follow is session state");
    assert!(text.contains("follow = \"b\""), "{text}");
    assert!(!text.contains("follow = \"a\""), "{text}");

    set_emit(&shell, &mut vcx, TileId(2), Some(Group::C));
    let text = session_text(&shell, &mut vcx).expect("so is an emit");
    assert!(text.contains("emit = \"c\""), "{text}");

    set_follow(&shell, &mut vcx, TileId(1), None);
    set_emit(&shell, &mut vcx, TileId(2), None);
    let text = session_text(&shell, &mut vcx).expect("and leaving");
    assert!(!text.contains("follow") && !text.contains("emit"), "{text}");
}

/// A closed tile's membership goes with it: nothing of it is written, so
/// no later tile inherits a group from the file.
#[gpui::test]
fn closing_a_tile_and_reusing_nothing_leaves_no_membership_in_the_session(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let services = super::session::test_services_with_session(dir.path().join("session.toml"));
    let (_window, mut vcx, shell, _frame) = two_tiles_in(cx, services);
    let tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    set_follow(&shell, &mut vcx, tile, Some(Group::A));
    let text = session_text(&shell, &mut vcx).expect("the baseline");
    assert!(text.contains("follow = \"a\""), "{text}");

    vcx.simulate_keystrokes("ctrl-w");
    draw(&mut vcx);
    vcx.run_until_parked();

    let text = session_text(&shell, &mut vcx).expect("the close is written");
    assert!(!text.contains("follow"), "{text}");
}

// --- The chooser ----------------------------------------------------

/// A shell over `services` with one recorder tile, focused.
fn one_tile_in(
    cx: &mut gpui::TestAppContext,
    services: ShellServices,
) -> (
    gpui::WindowHandle<Root>,
    gpui::VisualTestContext,
    Entity<ShellView>,
    TileId,
) {
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    add_tile(&mut vcx);
    let tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupant_kind(tile)),
        Some("rec")
    );
    (window, vcx, shell, tile)
}

/// Press the chord and paint what it opened.
fn press_mod_u(vcx: &mut gpui::VisualTestContext) {
    vcx.simulate_keystrokes("alt-u");
    draw(vcx);
    vcx.run_until_parked();
    draw(vcx);
}

/// The tile the open chooser is for, when the open choice dialog is one.
fn chooser_tile(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Option<TileId> {
    shell.read_with(vcx, |s, _| {
        match s.choice_dialog.as_ref().map(|d| &d.target) {
            Some(Target::LinkGroup { tile, .. }) => Some(*tile),
            _ => None,
        }
    })
}

fn notice(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(vcx, |s, _| s.notice.as_ref().map(|n| n.to_string()))
}

/// The keyboard route end to end: the chord opens the chooser on the
/// focused tile with the filter focused, typed words narrow the rows, and
/// Enter follows the group the lit row names.
#[gpui::test]
fn mod_u_opens_the_chooser_for_the_focused_tile_and_enter_follows(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, test_services());
    let frame = frame_of(&shell, &vcx);
    press_mod_u(&mut vcx);

    assert_eq!(chooser_tile(&shell, &vcx), Some(tile));
    assert!(vcx.debug_bounds("link-choice-list").is_some());
    assert!(vcx.debug_bounds("link-hints").is_some());
    assert!(
        dialog_filter_is_focused(&shell, &mut vcx),
        "typing reaches the filter"
    );
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.modals.last().map(|m| m.title.to_string())),
        Some("Link group".to_string())
    );

    // The opening row, `follow workspace`, still matches these words. The
    // typed query lights the row it ranks first, so Enter follows A.
    vcx.simulate_input("follow a");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(tile).follow),
        Some(Group::A)
    );
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));

    // Opened again it says where the tile is: the title names the group
    // and the lit row is the one Enter would leave unchanged.
    press_mod_u(&mut vcx);
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.modals.last().map(|m| m.title.to_string())),
        Some("Link group \u{00b7} following A".to_string())
    );
    assert_eq!(
        shell
            .read_with(&vcx, |s, _| {
                let list = &s.choice_dialog.as_ref().unwrap().list;
                list.highlighted_text().map(str::to_owned)
            })
            .as_deref(),
        Some("follow \u{00b7} A")
    );

    // A second pick, typed over a different opening row.
    vcx.simulate_input("follow b");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(tile).follow),
        Some(Group::B)
    );
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// A tile that cannot emit (a viewer) is offered the follow rows alone.
#[gpui::test]
fn a_viewer_like_tile_sees_no_emit_rows(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, test_services());
    press_mod_u(&mut vcx);
    assert_eq!(chooser_tile(&shell, &vcx), Some(tile));
    assert!(vcx.debug_bounds("link-choice-follow \u{00b7} A").is_some());
    assert!(
        vcx.debug_bounds("link-choice-follow \u{00b7} workspace")
            .is_some()
    );
    assert!(vcx.debug_bounds("link-choice-emit \u{00b7} A").is_none());
    assert!(vcx.debug_bounds("link-choice-emit \u{00b7} none").is_none());
}

/// Picking an emit row goes through the shell's door: the tile is
/// subscribed and its emission reaches the group at once.
#[gpui::test]
fn picking_an_emit_row_subscribes_and_posts(cx: &mut gpui::TestAppContext) {
    let (services, emitter) = emitting_services(Emission {
        scope: Some(underlying("SPX.Z")),
        board: Vec::new(),
    });
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, services);
    let frame = frame_of(&shell, &vcx);
    press_mod_u(&mut vcx);
    assert!(
        vcx.debug_bounds("link-choice-emit \u{00b7} A").is_some(),
        "an emitting tile is offered the emit rows"
    );

    vcx.simulate_input("emit a");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(tile)),
        geode_core::link::Membership {
            follow: None,
            emit: Some(Group::A),
        }
    );
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z")
    );
    assert_eq!(shell.read_with(&vcx, |s, _| s.emit_subs.len()), 1);
    emitter.emission.borrow_mut().scope = Some(underlying("NDX"));
    tile_changed(&shell, &mut vcx, tile);
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("NDX")
    );

    // The `emit none` row leaves the group again.
    press_mod_u(&mut vcx);
    vcx.simulate_input("emit none");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.membership(tile).emit), None);
    assert!(shell.read_with(&vcx, |s, _| s.emit_subs.is_empty()));
}

/// With nothing to link there is no chooser to open: the status bar says
/// why instead of a dialog whose every row would do nothing.
#[gpui::test]
fn the_chooser_refuses_without_a_tile(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    press_mod_u(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()));
    assert_eq!(notice(&shell, &vcx).as_deref(), Some("no tile to link"));
}

/// A placeholder is in no group and cannot be put in one: its membership
/// would be dropped the moment a module filled it.
#[gpui::test]
fn the_chooser_refuses_on_a_placeholder(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    shell.update(&mut vcx, |s, cx| {
        s.services
            .workspaces
            .split_active(crate::tiling::Orientation::Horizontal);
        cx.notify();
    });
    draw(&mut vcx);
    let tile = shell.read_with(&vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupant_kind(tile)),
        Some(crate::module::placeholder::PLACEHOLDER_KIND)
    );

    press_mod_u(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(notice(&shell, &vcx).as_deref(), Some(NO_TILE_TO_LINK));
}

/// The chooser acts on a tile, so it is refused while a page covers the
/// tiles. It opens a dialog, so its chord reaches the shell over another
/// dialog and stacks; over a choice dialog already open it changes nothing.
#[gpui::test]
fn the_chooser_is_refused_over_a_page_and_listed_as_a_dialog_opener(cx: &mut gpui::TestAppContext) {
    let page = crate::module::recording::RecordingPageFactory::new("diagnostics");
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, services_with_page(page));

    dispatch_action(&shell, "page::toggle_diagnostics", &mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.page_open()));
    dispatch_action(&shell, "tile::link_group", &mut vcx);
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(
        notice(&shell, &vcx).as_deref(),
        Some(crate::shell::input::CLOSE_PAGE_FIRST)
    );
    dispatch_action(&shell, "page::toggle_diagnostics", &mut vcx);
    assert!(shell.read_with(&vcx, |s, _| !s.page_open()));
    draw(&mut vcx);

    assert!(crate::shell::dialog::opens_dialog(&ActionId(
        "tile::link_group".into()
    )));
    let kinds = |shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| {
            s.modals.iter().map(|m| m.kind).collect::<Vec<_>>()
        })
    };
    use crate::shell::dialog::DialogKind;

    // Over another dialog the chord still reaches the shell, and stacks.
    dispatch_action(&shell, "settings::open", &mut vcx);
    draw(&mut vcx);
    press_mod_u(&mut vcx);
    assert_eq!(
        kinds(&shell, &vcx),
        vec![DialogKind::Settings, DialogKind::Choice]
    );
    assert_eq!(chooser_tile(&shell, &vcx), Some(tile));
    shell.update_in(&mut vcx, |s, window, cx| {
        while s.modal_open() {
            s.close_modal(window, cx);
        }
    });
    draw(&mut vcx);

    // Over the tile picker, itself a choice dialog, it replaces nothing.
    vcx.simulate_keystrokes("alt-n");
    draw(&mut vcx);
    press_mod_u(&mut vcx);
    assert_eq!(kinds(&shell, &vcx), vec![DialogKind::Choice]);
    assert!(
        shell.read_with(&vcx, |s, _| matches!(
            s.choice_dialog.as_ref().map(|d| &d.target),
            Some(Target::TileKind { .. })
        )),
        "the tile picker is still the open choice dialog"
    );
}

/// The pointer route: a click on a row commits that row.
#[gpui::test]
fn a_click_on_a_row_commits_it(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, test_services());
    let frame = frame_of(&shell, &vcx);
    press_mod_u(&mut vcx);
    let row = vcx
        .debug_bounds("link-choice-follow \u{00b7} A")
        .expect("the row is painted");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(tile).follow),
        Some(Group::A)
    );
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

/// The palette still reaches `Close tile` over an open dialog. A pick for a
/// tile closed under the chooser links nothing: a follow written for it
/// would sit in the frame for a tile nothing ever unlinks.
#[gpui::test]
fn a_pick_for_a_tile_closed_under_the_chooser_links_nothing(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, test_services());
    let frame = frame_of(&shell, &vcx);
    press_mod_u(&mut vcx);
    assert_eq!(chooser_tile(&shell, &vcx), Some(tile));

    dispatch_action(&shell, "workspace::close_tile", &mut vcx);
    draw(&mut vcx);
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.occupant_kind(tile)),
        None,
        "fixture: the tile closed under the chooser"
    );
    assert_eq!(
        chooser_tile(&shell, &vcx),
        Some(tile),
        "fixture: the chooser is still open on it"
    );

    vcx.simulate_input("follow b");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    assert!(frame.read_with(&vcx, |f, _| f.membership(tile).is_empty()));
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert_eq!(notice(&shell, &vcx).as_deref(), Some(TILE_GONE));
}

/// The chord is bound in no context, so it still dispatches over a page.
/// There the tiles are covered: no chooser opens, and the status bar says
/// to close the page.
#[gpui::test]
fn mod_u_over_a_page_is_refused_with_the_page_notice(cx: &mut gpui::TestAppContext) {
    let page = crate::module::recording::RecordingPageFactory::new("diagnostics");
    let (_window, mut vcx, shell, _tile) = one_tile_in(cx, services_with_page(page));
    dispatch_action(&shell, "page::toggle_diagnostics", &mut vcx);
    draw(&mut vcx);
    assert!(shell.read_with(&vcx, |s, _| s.page_open()));

    press_mod_u(&mut vcx);

    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
    assert!(shell.read_with(&vcx, |s, _| s.choice_dialog.is_none()));
    assert!(shell.read_with(&vcx, |s, _| s.page_open()));
    assert_eq!(
        notice(&shell, &vcx).as_deref(),
        Some(crate::shell::input::CLOSE_PAGE_FIRST)
    );
}

/// The pointer route to the emit door. A row's click handler holds no
/// update of the tile or of the frame, so the pull that joining makes can
/// read the one and write the other.
#[gpui::test]
fn a_click_on_an_emit_row_emits(cx: &mut gpui::TestAppContext) {
    let (services, _emitter) = emitting_services(Emission {
        scope: Some(underlying("SPX.Z")),
        board: Vec::new(),
    });
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, services);
    let frame = frame_of(&shell, &vcx);
    press_mod_u(&mut vcx);
    let row = vcx
        .debug_bounds("link-choice-emit \u{00b7} A")
        .expect("the row is painted");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(tile).emit),
        Some(Group::A)
    );
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z")
    );
    assert!(shell.read_with(&vcx, |s, _| !s.modal_open()));
}

// --- The status segment ---------------------------------------------
//
// These tests never force a draw after the change they make. The test
// window repaints only when something marked it dirty, so a segment that
// would go stale in the app (the shell not told to repaint) fails here.

/// The label the status bar last painted from, when it painted one. Tests
/// cannot read painted text; this cached string is what the segment shows.
fn following(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> Option<String> {
    shell.read_with(vcx, |s, _| {
        s.link_label.as_ref().map(|(_, label)| label.to_string())
    })
}

/// The focused tile of a two-tile shell, and the tile beside it.
fn focused_and_other(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> (TileId, TileId) {
    let focused = shell.read_with(vcx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    let other = if focused == TileId(1) {
        TileId(2)
    } else {
        TileId(1)
    };
    (focused, other)
}

/// The segment is about the focused tile: it shows while that tile follows
/// a group, in the bar's right-hand section, and moving focus to a tile
/// that follows nothing removes it. Focus moves by its keys.
#[gpui::test]
fn the_status_bar_names_the_group_the_focused_tile_follows(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, _frame) = two_tiles(cx);
    let (focused, other) = focused_and_other(&shell, &vcx);
    assert_eq!((focused, other), (TileId(2), TileId(1)), "fixture");
    assert!(
        vcx.debug_bounds("status-following").is_none(),
        "nothing followed: no segment"
    );
    assert_eq!(following(&shell, &vcx), None);

    set_follow(&shell, &mut vcx, focused, Some(Group::A));
    let seg = vcx
        .debug_bounds("status-following")
        .expect("the focused tile follows A: the segment shows");
    let bar = vcx
        .debug_bounds("shell-status-bar")
        .expect("status bar painted");
    assert!(
        seg.left() > bar.center().x,
        "the segment sits in the bar's right-hand section"
    );
    assert_eq!(following(&shell, &vcx).as_deref(), Some("following A"));

    vcx.simulate_keystrokes("alt-h");
    vcx.run_until_parked();
    assert_eq!(focused_and_other(&shell, &vcx).0, other, "focus moved");
    assert!(
        vcx.debug_bounds("status-following").is_none(),
        "the focused tile follows nothing: no segment"
    );
    assert_eq!(following(&shell, &vcx), None);

    vcx.simulate_keystrokes("alt-l");
    vcx.run_until_parked();
    assert_eq!(focused_and_other(&shell, &vcx).0, focused, "focus is back");
    assert!(vcx.debug_bounds("status-following").is_some());
    assert_eq!(following(&shell, &vcx).as_deref(), Some("following A"));

    set_follow(&shell, &mut vcx, focused, None);
    assert!(
        vcx.debug_bounds("status-following").is_none(),
        "following the workspace again removes it"
    );
}

/// The segment names what the tile follows. A tile that only emits takes
/// nothing from its group, so the bar says nothing about it.
#[gpui::test]
fn emitting_alone_shows_no_following_segment(cx: &mut gpui::TestAppContext) {
    let (services, _emitter) = emitting_services(Emission::default());
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    let (focused, _) = focused_and_other(&shell, &vcx);

    set_emit(&shell, &mut vcx, focused, Some(Group::B));
    assert_eq!(
        frame.read_with(&vcx, |f, _| f.membership(focused).emit),
        Some(Group::B),
        "fixture: the focused tile emits"
    );
    assert!(vcx.debug_bounds("status-following").is_none());
    assert_eq!(following(&shell, &vcx), None);
}

/// The label carries the single underlying the group's scope names, and
/// follows it. The second change reaches the frame from the emitting tile
/// with no shell handler in between: the frame's notification alone has to
/// repaint the bar.
#[gpui::test]
fn the_following_segment_tracks_the_groups_scope(cx: &mut gpui::TestAppContext) {
    let (services, emitter) = emitting_services(Emission {
        scope: Some(underlying("SPX.Z")),
        board: Vec::new(),
    });
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    let (focused, other) = focused_and_other(&shell, &vcx);
    set_follow(&shell, &mut vcx, focused, Some(Group::A));
    assert_eq!(
        following(&shell, &vcx).as_deref(),
        Some("following A"),
        "an unwritten group names no underlying"
    );

    set_emit(&shell, &mut vcx, other, Some(Group::A));
    assert_eq!(
        group_underlying(&frame, &vcx, Group::A).as_deref(),
        Some("SPX.Z"),
        "fixture: the emitter moved the group"
    );
    assert_eq!(
        following(&shell, &vcx).as_deref(),
        Some("following A \u{00b7} SPX.Z")
    );

    emitter.emission.borrow_mut().scope = Some(underlying("NDX"));
    tile_changed(&shell, &mut vcx, other);
    assert_eq!(
        following(&shell, &vcx).as_deref(),
        Some("following A \u{00b7} NDX")
    );

    // Another group's scope is not this tile's.
    set_emit(&shell, &mut vcx, other, Some(Group::B));
    assert_eq!(
        group_underlying(&frame, &vcx, Group::B).as_deref(),
        Some("NDX"),
        "fixture: the emitter now moves B"
    );
    emitter.emission.borrow_mut().scope = Some(underlying("RTY"));
    tile_changed(&shell, &mut vcx, other);
    assert_eq!(
        following(&shell, &vcx).as_deref(),
        Some("following A \u{00b7} NDX"),
        "A keeps its scope as last written"
    );
}

/// The label is formatted when its tile, group or the group's scope
/// changes, never per paint: repaints, and a frame change that moves none
/// of the three, keep the very string. The test holds the first string, so
/// a rebuilt one could not land at the same address. The underlying is long
/// on purpose: a `SharedString` of 23 bytes or fewer lives inline, and its
/// address would say nothing about a rebuild.
#[gpui::test]
fn an_unchanged_frame_rebuilds_no_label(cx: &mut gpui::TestAppContext) {
    let (services, _emitter) = emitting_services(Emission {
        scope: Some(underlying("STOXX50E.EUREX")),
        board: Vec::new(),
    });
    let (_window, mut vcx, shell, frame) = two_tiles_in(cx, services);
    let (focused, other) = focused_and_other(&shell, &vcx);
    set_follow(&shell, &mut vcx, focused, Some(Group::A));
    set_emit(&shell, &mut vcx, other, Some(Group::A));
    let label = |vcx: &gpui::VisualTestContext| {
        shell.read_with(vcx, |s, _| {
            s.link_label
                .as_ref()
                .map(|(_, label)| label.clone())
                .unwrap()
        })
    };
    let first = label(&vcx);
    assert_eq!(first.as_ref(), "following A \u{00b7} STOXX50E.EUREX");
    assert!(first.len() > 23, "fixture: the label is heap-backed");

    draw(&mut vcx);
    draw(&mut vcx);
    assert_eq!(
        label(&vcx).as_ptr(),
        first.as_ptr(),
        "a repaint formats nothing"
    );

    frame.update(&mut vcx, |f, cx| {
        assert!(f.shared_mut().set_text(Some("ndx".into())));
        cx.notify();
    });
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(
        label(&vcx).as_ptr(),
        first.as_ptr(),
        "the workspace's own scope is not the group's"
    );
}

/// The segment is the pointer route to the chooser, on the tile it names.
#[gpui::test]
fn a_click_on_the_segment_opens_the_chooser(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, test_services());
    set_follow(&shell, &mut vcx, tile, Some(Group::C));
    let seg = vcx
        .debug_bounds("status-following")
        .expect("the segment shows");
    assert_eq!(chooser_tile(&shell, &vcx), None);

    vcx.simulate_click(seg.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(chooser_tile(&shell, &vcx), Some(tile));
}

/// The tooltip names the chooser's key, so the segment teaches the
/// keyboard route it stands in for.
#[gpui::test]
fn hovering_the_segment_shows_the_chord(cx: &mut gpui::TestAppContext) {
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, test_services());
    set_follow(&shell, &mut vcx, tile, Some(Group::A));
    let seg = vcx
        .debug_bounds("status-following")
        .expect("the segment shows");

    vcx.simulate_mouse_move(
        seg.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("tip-status-following-chord-alt+u")
            .is_some(),
        "the tooltip names the link group key"
    );
    assert!(vcx.debug_bounds("tip-status-following-title").is_some());
}

/// A page covers the tiles and the chooser is refused over one, so the
/// segment (whose click opens the chooser) is not offered there.
#[gpui::test]
fn a_page_hides_the_following_segment(cx: &mut gpui::TestAppContext) {
    let page = crate::module::recording::RecordingPageFactory::new("diagnostics");
    let (_window, mut vcx, shell, tile) = one_tile_in(cx, services_with_page(page));
    set_follow(&shell, &mut vcx, tile, Some(Group::A));
    assert!(vcx.debug_bounds("status-following").is_some());

    // `mod+d`, the page's own key.
    vcx.simulate_keystrokes("alt-d");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.page_open()), "fixture");
    assert!(vcx.debug_bounds("status-following").is_none());
    assert_eq!(following(&shell, &vcx), None);

    vcx.simulate_keystrokes("alt-d");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| !s.page_open()), "fixture");
    assert!(vcx.debug_bounds("status-following").is_some());
}
