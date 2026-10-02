//! The shell's doors onto link groups: who follows and emits, and the pull
//! that carries an emitting tile's emission into the frame. Modules never
//! write the frame; they answer `emission()` and say when it changed.

use std::rc::Rc;

use gpui::{App, Context};

use geode_core::link::Group;
use geode_core::query::QueryKey;

use super::ShellView;
use crate::tiling::TileId;

impl ShellView {
    /// Follow `group`, or the workspace again with `None`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no caller outside tests until the link chooser dispatches this door"
        )
    )]
    pub(super) fn set_follow(
        &mut self,
        tile: TileId,
        group: Option<Group>,
        cx: &mut Context<Self>,
    ) {
        // The tile's own workspace, the one its frame handle was bound to
        // at creation: that lane supplies the rest of its identity.
        let ws = self
            .services
            .workspaces
            .workspace_of(tile)
            .unwrap_or_else(|| self.active_ix());
        let changed = self.frame.update(cx, |f, cx| {
            let changed = f.follow(tile, group);
            if changed {
                // The follow changed the scope generation this tile answers
                // a flip under. A barrier already waiting on it must wait
                // under the new one: the tile requeries on the follow, and
                // an arrival under an identity the barrier does not hold
                // would leave the flip to its deadline.
                let versions = f.view_for(ws, tile).versions();
                f.reidentify(QueryKey(tile.0), versions);
                cx.notify();
            }
            changed
        });
        if changed {
            self.repaint_tile(tile, cx);
            cx.notify();
        }
    }

    /// Emit into `group`, or into none. A tile that cannot emit is never
    /// set emitting: the chooser does not offer it, and a membership that
    /// reached the frame some other way (a session written when the module
    /// could) is cleared instead of left subscribing to nothing.
    ///
    /// Joining pulls the tile's emission at once, which reads the tile and
    /// writes the frame: call this from the shell's own handlers, never
    /// from inside an update of that tile or of the frame.
    pub(super) fn set_emit(&mut self, tile: TileId, group: Option<Group>, cx: &mut Context<Self>) {
        let can = self.occupants.get(&tile).is_some_and(|o| o.content.emits());
        let group = group.filter(|_| can);
        let changed = self.frame.update(cx, |f, cx| {
            let changed = f.emit(tile, group);
            if changed {
                cx.notify();
            }
            changed
        });
        if changed {
            self.sync_emitter(tile, cx);
            self.repaint_tile(tile, cx);
            cx.notify();
        }
    }

    /// Hold a subscription exactly while `tile` emits, and pull once when
    /// one starts so the group hears the tile's current emission without
    /// waiting for its next change. The same caller rule as [`Self::set_emit`].
    pub(super) fn sync_emitter(&mut self, tile: TileId, cx: &mut Context<Self>) {
        self.emit_subs.remove(&tile);
        if self.frame.read(cx).membership(tile).emit.is_none() {
            return;
        }
        let Some(o) = self.occupants.get(&tile) else {
            return;
        };
        // A membership the session restored for a tile whose module can no
        // longer emit is cleared through the door, which repaints the tile.
        // Left in place, its header would show it emitting into a group
        // that never hears it.
        if !o.content.emits() {
            self.set_emit(tile, None, cx);
            return;
        }
        let weak = cx.entity().downgrade();
        let changed: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
            // The pull reads the tile and updates the frame and the shell,
            // so it waits for the update that announced the change to
            // finish. A module may then call this from inside its own
            // update without the pull re-entering it.
            let weak = weak.clone();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |view, cx| view.pull_emission(tile, cx));
            });
        });
        if let Some(sub) = o.content.watch_emission(changed, cx) {
            self.emit_subs.insert(tile, sub);
        }
        self.pull_emission(tile, cx);
    }

    /// Read what `tile` emits now and post it. An emission equal to the
    /// tile's last writes nothing and notifies nobody, so a pull is safe
    /// on every notification of an emitting tile.
    fn pull_emission(&mut self, tile: TileId, cx: &mut Context<Self>) {
        let Some(o) = self.occupants.get(&tile) else {
            return;
        };
        let emission = o.content.emission(cx);
        self.frame.update(cx, |f, cx| {
            if f.post_emission(tile, emission) {
                cx.notify();
            }
        });
    }

    /// Repaint a tile whose header shows its membership. The tile's view is
    /// its own entity; the shell re-rendering does not repaint it.
    fn repaint_tile(&self, tile: TileId, cx: &mut Context<Self>) {
        if let Some(o) = self.occupants.get(&tile) {
            App::notify(cx, o.view.entity_id());
        }
    }
}

impl ShellView {
    /// A tile is gone for good: stop listening to it and drop its
    /// membership, so its drafts leave the board and a later occupant under
    /// the same id starts in no group. Touches the frame only when the tile
    /// was in a group. Call it after `TileContent::closed`: a closing
    /// follower answers the flip barrier under its group's identity, which
    /// it reads only while still a member.
    pub(super) fn unlink_tile(&mut self, tile: TileId, cx: &mut Context<Self>) {
        self.emit_subs.remove(&tile);
        if self.frame.read(cx).membership(tile).is_empty() {
            return;
        }
        self.frame.update(cx, |f, cx| {
            if f.forget_tile(tile) {
                cx.notify();
            }
        });
    }

    /// Drop the membership of every tile no workspace holds: what the
    /// session restored for a record whose tile the layout does not have.
    /// Such a tile never gets an occupant, so nothing would ever unlink it.
    /// Writes the frame, so it runs after a render, never inside one.
    pub(super) fn prune_links(&mut self, cx: &mut Context<Self>) {
        let workspaces = &self.services.workspaces;
        self.frame.update(cx, |f, cx| {
            if f.retain_linked(|tile| workspaces.workspace_of(tile).is_some()) {
                cx.notify();
            }
        });
    }
}
