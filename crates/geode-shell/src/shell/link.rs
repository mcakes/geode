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

use super::choicedialog::LinkChange;
use super::{ShellView, status};
use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::tiling::TileId;

/// What the status bar says when a pick would close a loop of groups.
pub(crate) fn cycle_refusal(from: Group, to: Group) -> String {
    format!("would link {} back into {}", from.letter(), to.letter())
}

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
    /// tile no module occupies is refused and the frame is not touched, and
    /// so is a follow that would close a loop of groups with what the tile
    /// emits into: the status bar says which link it refused.
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
        if let Some((from, to)) = self.link_cycle(tile, LinkChange::Follow(group), cx) {
            self.notice = Some(cycle_refusal(from, to).into());
            cx.notify();
            return;
        }
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
    /// module occupies is refused and the frame is not touched, and so is
    /// an emit that would close a loop of groups with the group the tile
    /// follows: the status bar says which link it refused.
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
        if let Some((from, to)) = self.link_cycle(tile, LinkChange::Emit(group), cx) {
            self.notice = Some(cycle_refusal(from, to).into());
            cx.notify();
            return;
        }
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

    /// The loop of groups `change` would close for `tile`, taken with the
    /// tile's other current membership: the follow change keeps its emit,
    /// the emit change keeps its follow. The doors and the chooser both
    /// ask this one question, so a row the chooser lets through is never
    /// one a door then refuses after the list has closed.
    pub(super) fn link_cycle(
        &self,
        tile: TileId,
        change: LinkChange,
        cx: &App,
    ) -> Option<(Group, Group)> {
        let frame = self.frame.read(cx);
        let m = frame.membership(tile);
        let (follow, emit) = match change {
            LinkChange::Follow(group) => (group, m.emit),
            LinkChange::Emit(group) => (m.follow, group),
        };
        frame.closing_cycle(tile, follow, emit)
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

    /// Read what `tile` emits now, compose it over the tile's base and post
    /// it. A posting equal to the tile's last writes nothing and notifies
    /// nobody, so a pull is safe on every notification of an emitting tile
    /// and on every frame move.
    fn pull_emission(&mut self, tile: TileId, cx: &mut Context<Self>) {
        let Some(o) = self.occupants.get(&tile) else {
            return;
        };
        let emission = o.content.emission(cx);
        let ws = self
            .services
            .workspaces
            .workspace_of(tile)
            .unwrap_or_else(|| self.active_ix());
        let include_layer = self.link_include_tile_filter;
        self.frame.update(cx, |f, cx| {
            let base = f.emit_base(ws, tile);
            let (posting, refused) = geode_core::link::compose(emission, &base, include_layer);
            let refusal_changed = f.set_link_refusal(tile, refused);
            if f.post_emission(tile, posting) | refusal_changed {
                cx.notify();
            }
        });
    }

    /// Pull every emitter again when the frame has moved since the last
    /// re-pull. An emitter's base is its lane's or its followed group's
    /// scope, which move without the tile announcing anything; the frame's
    /// generation advances on every such write (and on membership and pin
    /// changes). Equal postings write nothing, and a loop of groups is
    /// refused at the doors and on restore, so a chain settles after one
    /// extra pass. Emitters are re-pulled oldest posting first, so a group
    /// two tiles emit into stays on the one that moved last.
    pub(super) fn repull_emitters(&mut self, cx: &mut Context<Self>) {
        let generation = self.frame.read(cx).generation();
        if generation == self.last_emit_generation {
            return;
        }
        self.last_emit_generation = generation;
        for tile in self.frame.read(cx).emitters() {
            self.pull_emission(tile, cx);
        }
    }

    /// Re-pull every emitter now, whatever the frame's generation: a change
    /// of the composition rule moves no frame number, so the generation
    /// gate in [`Self::repull_emitters`] would leave every group on the old
    /// rule. Same oldest-posting-first order as that gate's pass.
    pub(super) fn force_repull_emitters(&mut self, cx: &mut Context<Self>) {
        for tile in self.frame.read(cx).emitters() {
            self.pull_emission(tile, cx);
        }
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
    /// the same id starts in no group, and drop the config-door notices it
    /// will never take. Notifies the frame only when the tile was in a
    /// group. Call it after `TileContent::closed`: a closing follower
    /// answers the flip barrier under its group's identity, which it reads
    /// only while still a member.
    pub(super) fn unlink_tile(&mut self, tile: TileId, cx: &mut Context<Self>) {
        self.emit_subs.remove(&tile);
        self.frame.update(cx, |f, cx| {
            if f.forget_tile(tile) {
                cx.notify();
            }
        });
    }
}
