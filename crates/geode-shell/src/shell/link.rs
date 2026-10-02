//! The shell's doors onto link groups: who follows and emits, and the pull
//! that carries an emitting tile's emission into the frame. A module has no
//! door to a group: the frame's membership and emission writes are private
//! to this crate, and a module answers `emission()` and says when it
//! changed. The status bar's `following` label for the focused tile is
//! cached here.

use std::rc::Rc;

use gpui::{App, Context};

use geode_core::link::Group;
use geode_core::query::QueryKey;

use super::{ShellView, status};
use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::tiling::TileId;

/// What the status bar's `following` label was built from: the focused
/// tile, the group it follows and that group's scope generation.
pub(super) type LinkLabelKey = (TileId, Group, u64);

impl ShellView {
    /// Bring the status bar's `following` label up to date with the focused
    /// tile. The label is rebuilt only when the tile, its
    /// followed group or that group's scope generation changed, so a
    /// repaint with none of them moved formats nothing. Under a page there
    /// is no label: the page covers the tiles and the chooser the segment
    /// opens is refused there.
    ///
    /// Called while preparing a render. It writes this one field and
    /// notifies nobody: a notification sent while the window draws is
    /// dropped, and none is needed, since the label is read in the same
    /// pass.
    pub(super) fn refresh_link_label(&mut self, cx: &App) {
        let frame = self.frame.read(cx);
        let focused = if self.page_open() {
            None
        } else {
            self.services.workspaces.active().focused_tile()
        };
        let key: Option<LinkLabelKey> = focused.and_then(|tile| {
            let group = frame.membership(tile).follow?;
            Some((tile, group, frame.group_scope_gens()[group.index()]))
        });
        if self.link_label.as_ref().map(|(held, _)| *held) == key {
            return;
        }
        self.link_label = key.map(|key| {
            let underlying = geode_core::link::underlying_of(frame.group_scope(key.1));
            (key, status::following_label(key.1, underlying))
        });
    }

    /// Why the doors below refuse `tile`, or `None` when a module occupies
    /// it: the only tiles they link. A membership lives until its occupant
    /// closes or is replaced, so one written for an id with no occupant, or
    /// for a placeholder, would have nothing to end it.
    fn door_refusal(&self, tile: TileId) -> Option<&'static str> {
        match self.occupants.get(&tile) {
            None => Some("no occupant"),
            Some(o) if o.kind == PLACEHOLDER_KIND => Some("a placeholder"),
            Some(_) => None,
        }
    }

    /// Follow `group`, or the workspace again with `None`. A tile whose
    /// module does not follow is never set following: the chooser does not
    /// offer it, and a membership that reached the frame some other way (a
    /// session written when the module did) is cleared instead of left
    /// showing a group's chip over content the group does not select. A
    /// tile no module occupies is refused and the frame is not touched.
    ///
    /// Writes the frame: call this from the shell's own handlers, never
    /// from inside an update of the frame.
    pub(super) fn set_follow(
        &mut self,
        tile: TileId,
        group: Option<Group>,
        cx: &mut Context<Self>,
    ) {
        if let Some(why) = self.door_refusal(tile) {
            tracing::debug!(target: "geode::shell", "follow refused for tile {}: {why}", tile.0);
            return;
        }
        let follows = self
            .occupants
            .get(&tile)
            .is_some_and(|o| o.content.follows());
        if group.is_some() && !follows {
            tracing::debug!(
                target: "geode::shell",
                "follow refused for tile {}: its module does not follow",
                tile.0
            );
        }
        let group = group.filter(|_| follows);
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
    /// could) is cleared instead of left subscribing to nothing. A tile no
    /// module occupies is refused and the frame is not touched.
    ///
    /// Joining pulls the tile's emission at once, which reads the tile and
    /// writes the frame: call this from the shell's own handlers, never
    /// from inside an update of that tile or of the frame.
    pub(super) fn set_emit(&mut self, tile: TileId, group: Option<Group>, cx: &mut Context<Self>) {
        // Kept as defence. No test can tell from the frame that it is here:
        // with it gone, the `emits()` filter below still turns every such
        // request into an emit of none, which changes nothing for a tile in
        // no group. It states the rule once for both doors, and only the
        // reason it logs is observable.
        if let Some(why) = self.door_refusal(tile) {
            tracing::debug!(target: "geode::shell", "emit refused for tile {}: {why}", tile.0);
            return;
        }
        let can = self.occupants.get(&tile).is_some_and(|o| o.content.emits());
        if group.is_some() && !can {
            tracing::debug!(
                target: "geode::shell",
                "emit refused for tile {}: its module does not emit",
                tile.0
            );
        }
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

    /// Notify a tile's own view when its membership changes: its header
    /// shows the membership, and whatever observes the view hears that it
    /// changed. Tile views are not cached, so the shell's own repaint
    /// re-renders every tile and the chip would appear without this. The
    /// notification keeps the header right independently of that: a cached
    /// view repaints only when the view itself is notified.
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
}
