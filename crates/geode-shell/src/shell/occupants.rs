//! Tile occupant lifecycle:
//! which tiles need a module occupant right now, creating them on demand
//! through the module roster, delivering async query results to the tile
//! that asked for them, and the pure `session::TileRecords` snapshot the
//! session writer serializes. This is the seam shared by `render.rs`'s
//! per-frame reconciliation and `session_io.rs`'s writer.

use std::collections::HashSet;

use gpui::{App, Context, FocusHandle, Focusable as _, Window};

use crate::frame::FrameRef;
use crate::module::Delivery;
use crate::module::ModuleFactory as _;
use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::session;
use crate::tiling::TileId;
use geode_core::link::Membership;
use geode_core::query::QueryKey;

use super::ShellView;

impl ShellView {
    /// Collect fresh serialized state from non-placeholder occupants, plus
    /// preserved records for unavailable modules. Live occupants win an ID
    /// collision. Placeholder occupants have no state of their own; retaining
    /// unplaced records lets sessions survive builds with fewer modules.
    /// Reconciliation and tile filling remove stale unplaced records.
    ///
    /// A live occupant's link groups are the frame's. An unplaced record
    /// keeps the ones it was read with: its placeholder is in no group, and
    /// taking the frame's answer would erase a membership the tile gets back
    /// when its module does.
    pub(super) fn current_tiles(&self, cx: &App) -> session::TileRecords {
        let frame = self.frame.read(cx);
        let mut tiles: session::TileRecords = self
            .occupants
            .iter()
            .filter(|(_, o)| o.kind != PLACEHOLDER_KIND)
            .map(|(id, o)| {
                (
                    id.0,
                    session::TileRecord {
                        kind: o.kind.to_string(),
                        state: o.content.serialize(cx),
                        link: frame.membership(*id),
                    },
                )
            })
            .collect();
        for (id, record) in &self.unplaced_records {
            tiles.entry(*id).or_insert_with(|| record.clone());
        }
        tiles
    }

    /// The retained page's state (open or not), plus every table still in
    /// `restored_pages`: kinds never opened, unknown kinds, and the last
    /// state of a page another kind replaced. All of them survive a save.
    pub(super) fn current_pages(&self, cx: &App) -> session::PageRecords {
        let mut pages: session::PageRecords = self.services.restored_pages.clone();
        if let Some(page) = &self.page {
            pages.insert(
                page.occupant.kind.to_string(),
                page.occupant.content.serialize(cx),
            );
        }
        pages
    }

    /// The module kind occupying `tile`, or `None` if it has no occupant
    /// (not a tile at all, or not yet created).
    pub fn occupant_kind(&self, tile: TileId) -> Option<&'static str> {
        self.occupants.get(&tile).map(|o| o.kind)
    }

    /// Deliver keyed results to their tile, dropping results for closed tiles.
    /// Broadcast `SeriesFetched` to visible non-placeholder occupants, each
    /// with its own copy. Hidden tiles hold no subscription and requery when
    /// made visible. The app bridge calls this entry point.
    pub fn deliver(&mut self, delivery: Delivery, window: &mut Window, cx: &mut Context<Self>) {
        // Matched on the VARIANT, not on `key()`, and with every keyed
        // variant named rather than a wildcard: a new variant — keyed
        // or not — fails to compile here until it is given an arm, the
        // same rule every occupant's `deliver` follows.
        match delivery {
            Delivery::SeriesFetched {
                source,
                identity,
                result,
            } => {
                // `visible_tile_keys` already filters placeholders and
                // covers visible docks — the same visible set the flip
                // barrier waits on.
                let mut keys = Vec::new();
                self.visible_tile_keys(&mut keys);
                for key in keys {
                    if let Some(o) = self.occupants.get(&TileId(key.0)) {
                        o.content.deliver(
                            Delivery::SeriesFetched {
                                source: source.clone(),
                                identity: identity.clone(),
                                result: result.clone(),
                            },
                            window,
                            cx,
                        );
                    }
                }
            }
            // Every keyed variant, named: a new variant fails to compile
            // here rather than falling into a wildcard and being dropped.
            keyed @ (Delivery::Query(_)
            | Delivery::Series(_)
            | Delivery::Price(_)
            | Delivery::VolSlices(_)
            | Delivery::Upload(_)) => {
                if let Some(key) = keyed.key()
                    && let Some(o) = self.occupants.get(&TileId(key.0))
                {
                    o.content.deliver(keyed, window, cx);
                }
            }
        }
    }

    /// Every tile id in every workspace, main trees and docks, written
    /// into `out` (cleared first). A method rather than a `HashSet`
    /// return so `ensure_occupants` can reuse a scratch allocation across
    /// frames instead of allocating one every render.
    fn fill_all_tiles(&self, out: &mut HashSet<TileId>) {
        out.clear();
        for (_, ws) in self.services.workspaces.spaces() {
            out.extend(ws.tree().tiles());
            for (_, dock) in ws.docks().iter() {
                out.extend(dock.tree().tiles());
            }
        }
    }

    /// Fill `out` with tiles on screen in the active workspace, clearing it
    /// first. Hidden stack members and hidden docks are excluded. Reusing the
    /// set avoids a fresh allocation each frame.
    fn fill_active_tiles(&self, out: &mut HashSet<TileId>) {
        out.clear();
        // A page covers the tile surface: nothing beneath is visible, so no tile
        // is announced shown and no flip barrier waits on one.
        if self.page_open() {
            return;
        }
        let ws = self.services.workspaces.active();
        out.extend(ws.tree().visible_tiles());
        for (_, dock) in ws.docks().iter() {
            if dock.visible() {
                out.extend(dock.tree().visible_tiles());
            }
        }
    }

    /// Fill `out` with keys for visible, non-placeholder occupants in the active
    /// workspace, including docks. Frame flips wait on this set; tiles without
    /// an occupant cannot query or arrive. The caller retains the vector's
    /// allocation between uses.
    pub(super) fn visible_tile_keys(&self, out: &mut Vec<QueryKey>) {
        out.clear();
        // A page covers the tile surface: nothing beneath is visible, so no tile
        // is announced shown and no flip barrier waits on one.
        if self.page_open() {
            return;
        }
        // Placeholders never query or arrive; waiting on them would hold every
        // flip until its deadline.
        let has_real_occupant = |id: &TileId| {
            self.occupants
                .get(id)
                .is_some_and(|o| o.kind != PLACEHOLDER_KIND)
        };
        let ws = self.services.workspaces.active();
        out.extend(
            ws.tree()
                .visible_tiles()
                .into_iter()
                .filter(has_real_occupant)
                .map(|id| QueryKey(id.0)),
        );
        for (_, dock) in ws.docks().iter() {
            if dock.visible() {
                out.extend(
                    dock.tree()
                        .visible_tiles()
                        .into_iter()
                        .filter(has_real_occupant)
                        .map(|id| QueryKey(id.0)),
                );
            }
        }
    }

    /// Reconcile tile occupants, visibility, and stack positions at render time.
    /// Create missing occupants, tell removed occupants they are hidden and
    /// closed, then drop them. Reusable tile sets retain capacity between
    /// frames and are temporarily taken out of `self` while factory calls
    /// borrow services.
    /// A fresh `add_tile` occupant that is on screen and focused hears
    /// `TileContent::launched` once, deferred after the render.
    /// Nothing here writes the frame: every link-group write a pass finds
    /// due (a closed tile, a restored emitter, a restored follow on a tile
    /// that does not follow) is deferred until the render is over.
    pub(super) fn ensure_occupants(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut all = std::mem::take(&mut self.scratch_all_tiles);
        self.fill_all_tiles(&mut all);
        // Tell removed occupants they are hidden and then closed before
        // dropping them, so they can release subscriptions and cancel their
        // queries with a live GPUI context. The visibility diff below can only
        // reach occupants still in the map.
        let mut gone: Vec<TileId> = Vec::new();
        for (id, o) in self.occupants.iter() {
            if !all.contains(id) {
                o.content.set_visible(false, cx);
                o.content.closed(cx);
                gone.push(*id);
            }
        }
        self.occupants.retain(|id, _| all.contains(id));
        // A closed tile is in no link group. Unlinked after `closed`: a
        // closing follower answers the flip barrier under its group's
        // identity, which it reads only while still a member. The frame is
        // written only for a tile that was in a group, so an ordinary render
        // writes nothing, and that write waits until this render is over.
        // While a window draws, GPUI drops a notification for any entity
        // that window read in its last draw. The frame is one, because the
        // shell's render reads it, so notified here the tiles reading the
        // group's board would not hear a draft leave.
        for id in &gone {
            self.emit_subs.remove(id);
        }
        gone.retain(|id| !self.frame.read(cx).membership(*id).is_empty());
        if !gone.is_empty() {
            cx.defer_in(window, move |view, _, cx| {
                for id in gone {
                    view.unlink_tile(id, cx);
                }
            });
        }

        // Compute visibility before creating occupants so even initially hidden
        // ones receive their state. Newly visible occupants may receive `true`
        // here and again in the diff; visibility setters must be idempotent.
        let mut active = std::mem::take(&mut self.scratch_active_tiles);
        self.fill_active_tiles(&mut active);

        // `all` is a `HashSet<TileId>`, so its iteration order is not
        // deterministic. Sorting makes the order tiles are created in
        // (and so the order their ids are handed to factories) stable
        // rather than a coin flip on the hasher's internal state — a
        // separate `Vec`, not a reassignment of `all` itself, since
        // `all` (the `HashSet`) is still needed below
        // (`self.scratch_all_tiles = all`).
        let mut creation_order: Vec<TileId> = all.iter().copied().collect();
        creation_order.sort();

        let mut fresh: Vec<TileId> = Vec::new();
        for id in &creation_order {
            if self.occupants.contains_key(id) {
                continue;
            }
            let restored = self.services.restored_tiles.remove(&id.0);
            let pending = self.pending_tiles.remove(id);
            // Only the factory registered for this record's kind receives its state.
            // A matched restored record takes precedence over a pending add request.
            let matched = restored
                .as_ref()
                .and_then(|r| self.services.roster.factory(&r.kind));
            let restored_state = matched.and(restored.as_ref()).map(|r| &r.state);
            let pending_factory = pending.as_ref().and_then(|p| {
                let f = self.services.roster.factory(&p.kind);
                if f.is_none() {
                    tracing::warn!(
                        target: "geode::shell",
                        "add_tile asked for kind '{}', which has no registered factory — painting a placeholder",
                        p.kind
                    );
                }
                f
            });
            let pending_state = pending_factory
                .and(pending.as_ref())
                .and_then(|p| p.state.as_ref());
            // Use a matching restored factory, then a pending factory, then a
            // placeholder. Preserve unmatched restored records for later saves.
            // Restore and add-tile allocation normally use disjoint IDs.
            if let (Some(record), None) = (&restored, matched) {
                tracing::warn!(
                    target: "geode::session",
                    "tile {} was saved as '{}', which this build has no module for — painting a placeholder and keeping the record",
                    id.0, record.kind
                );
                self.unplaced_records.insert(id.0, record.clone());
            }
            let (factory, state) = match (matched, pending_factory) {
                (Some(f), _) => (Some(f), restored_state),
                (None, Some(f)) => (Some(f), pending_state),
                (None, None) => (None, None),
            };
            // Only an `add_tile` request (no matching restored record) may be
            // told it was launched: a restore must never take focus.
            let from_add = matched.is_none() && pending_factory.is_some();
            let from_restore = matched.is_some();
            // The tile's own workspace, not the active one: an occupant
            // restored into a hidden workspace reads that workspace's lane.
            // Every tile reaching here is placed in some workspace's tree;
            // the active fallback only keeps a release build running.
            debug_assert!(
                self.services.workspaces.workspace_of(*id).is_some(),
                "occupant for unplaced tile {id:?}"
            );
            let ws = self
                .services
                .workspaces
                .workspace_of(*id)
                .unwrap_or_else(|| self.services.workspaces.active_ix());
            // Bound to the tile as well, so the link group it follows
            // decides the scope it reads.
            let frame = FrameRef::for_tile(self.frame.clone(), ws, *id);
            let occupant = match factory {
                Some(f) => f.create(
                    *id,
                    state,
                    frame.clone(),
                    self.diagnostics.clone(),
                    window,
                    cx,
                ),
                None => crate::module::placeholder::PlaceholderFactory.create(
                    *id,
                    None,
                    frame.clone(),
                    self.diagnostics.clone(),
                    window,
                    cx,
                ),
            };
            // Announce initial visibility, including for occupants in hidden docks
            // or inactive workspaces that the later visibility diff cannot see.
            occupant.content.set_visible(active.contains(id), cx);
            self.occupants.insert(*id, occupant);
            if from_add {
                fresh.push(*id);
            }
            // A restored tile that emits into a group is listened to from
            // here on, without the trader touching it. After the render,
            // for ordering: the first pull writes the group's scope, and
            // written here, between two occupants of this pass, a follower
            // would start on a different scope according to whether its
            // tile id sorts before or after its emitter's. (The post's
            // notification is not what the deferral saves. On a first
            // draw GPUI delivers it: a window tracks an entity only from
            // the end of a draw.)
            let restored_link = if from_restore {
                self.frame.read(cx).membership(*id)
            } else {
                Membership::default()
            };
            if restored_link.emit.is_some() {
                let id = *id;
                cx.defer_in(window, move |view, _, cx| view.sync_emitter(id, cx));
            }
            // A follow restored for a tile whose module does not follow (a
            // session written when it did) is cleared through the door,
            // which repaints the tile. Left in place, its header would show
            // a group's chip over content the group does not select. After
            // the render, like the emitter's sync: the door notifies the
            // frame, and a notification sent while the window draws is
            // dropped for an entity that window read in its last draw.
            if restored_link.follow.is_some()
                && self.occupants.get(id).is_some_and(|o| !o.content.follows())
            {
                let id = *id;
                cx.defer_in(window, move |view, _, cx| view.set_follow(id, None, cx));
            }
            // A fresh occupant under this id must hear its stack position
            // even when a previous occupant under the SAME id already did
            // — `add_tile` fills a placeholder in place by removing its
            // occupant and letting this loop recreate one, and without
            // this the delivery loop below sees `stack_sent` still
            // holding the old occupant's last-sent value and skips the
            // new one as already told.
            self.stack_sent.remove(id);
            // Likewise for focus: a fresh occupant under the focused id
            // has not been told.
            if self.focused_sent == Some(*id) {
                self.focused_sent = None;
            }
        }
        // A request whose tile closed before this render is dropped, not
        // re-aimed.
        self.pending_tiles.retain(|id, p| {
            let live = all.contains(id);
            if !live {
                tracing::debug!(target: "geode::shell", "dropping a pending '{}' request for closed tile {}", p.kind, id.0);
            }
            live
        });
        // An unplaced record outlives only its own tile: once the tile is
        // gone from every workspace there is nothing left to write it
        // back for. Filling the tile in place drops it too —
        // `add_tile` does that, since the live occupant's own record
        // supersedes it.
        self.unplaced_records
            .retain(|id, _| all.contains(&TileId(*id)));
        self.scratch_all_tiles = all;

        let mut any_tile_left_the_screen = false;
        for id in self.visible_tiles.difference(&active) {
            any_tile_left_the_screen = true;
            if let Some(o) = self.occupants.get(id) {
                o.content.set_visible(false, cx);
            }
        }
        for id in active.difference(&self.visible_tiles) {
            if let Some(o) = self.occupants.get(id) {
                o.content.set_visible(true, cx);
            }
        }

        // Tell the focused tile it is, and the one before it that it no
        // longer is. In render, before the tiles' own renders, so the flag
        // is read in this same frame and nothing needs notifying.
        let focused_now = self
            .services
            .workspaces
            .active()
            .focused_tile()
            .filter(|id| active.contains(id));
        if focused_now != self.focused_sent {
            if let Some(o) = self.focused_sent.and_then(|id| self.occupants.get(&id)) {
                o.content.set_focused(false, cx);
            }
            if let Some(o) = focused_now.and_then(|id| self.occupants.get(&id)) {
                o.content.set_focused(true, cx);
            }
            self.focused_sent = focused_now;
        }

        // Tell a fresh `add_tile` occupant it was launched, once, if it is
        // on screen and the focused tile on this frame. Deferred: this runs
        // inside render, and `launched` may move focus (the market-data
        // panel opens its picker), which must follow any modal focus return
        // the add itself came from.
        let focused_tile = self.services.workspaces.active().focused_tile();
        for id in fresh {
            if active.contains(&id) && focused_tile == Some(id) {
                cx.defer_in(window, move |view, window, cx| {
                    if let Some(o) = view.occupants.get(&id) {
                        o.content.launched(window, cx);
                    }
                });
            }
        }

        // Send stack positions on first delivery and whenever they change.
        // Record the value only after reaching an occupant, so missing
        // occupants can receive it on a later reconciliation.
        for id in &creation_order {
            let now = self.services.workspaces.stack_position(*id);
            if self.stack_sent.get(id) == Some(&now) {
                continue;
            }
            let Some(o) = self.occupants.get(id) else {
                continue;
            };
            // The weak handle is only needed to build a member's `open_
            // list` closure, so it is downgraded here rather than once
            // per render regardless of whether any tile is stacked.
            let handle = now.map(|(index, len)| {
                let weak = cx.entity().downgrade();
                let tile = *id;
                crate::module::StackHandle::new(index, len, move |window, cx| {
                    let _ = weak.update(cx, |view, cx| view.open_stack_list(tile, window, cx));
                })
            });
            o.content.set_stack(handle, cx);
            self.stack_sent.insert(*id, now);
        }
        self.stack_sent
            .retain(|id, _| self.scratch_all_tiles.contains(id));

        // Restore focus immediately if a tile leaves the screen while an
        // occupant holds the keyboard. Hidden occupants retain their focus
        // handles, but GPUI can only route through the rendered tree. A live
        // handle alone therefore does not guarantee working key dispatch.
        //
        // This runs during the render that removes the tile; deferring through
        // `pending_focus_restore` would leave a frame with invalid focus.
        // Shell inputs retain their caret. Any new shell focusable surface must
        // be included in `holds_shell_focus`. A tile still on screen that owns
        // the focus keeps it too: a pull hides a neighbour while the focused
        // tile may be typing, and taking its input would strand the open
        // editor. Focus no painted tile claims is still taken back. An open
        // page is what hid the tiles, and it holds the focus on purpose: the
        // net must not pull it off the page (or the page's own input) on the
        // very render that hides them.
        if any_tile_left_the_screen
            && let Some(focused) = window.focused(cx)
            && !self.holds_shell_focus(&focused, cx)
            && !active.iter().any(|id| {
                self.occupants
                    .get(id)
                    .is_some_and(|o| o.content.holds_focus(window, cx))
            })
            && !self.page.as_ref().filter(|p| p.open).is_some_and(|p| {
                p.occupant
                    .content
                    .focus_handle(cx)
                    .contains_focused(window, cx)
            })
        {
            self.focus_handle.focus(window, cx);
        }
        self.visible_tiles.clear();
        self.visible_tiles.extend(active.iter().copied());
        self.scratch_active_tiles = active;
    }

    /// Focus `tile` for a press on its own header chrome, wherever it sits
    /// in the active workspace. `false` when the workspace has no such tile.
    /// Only a change of focused tile dirties the session: the focus methods
    /// report success even when the requested tile was already focused.
    fn focus_pressed_tile(&mut self, tile: TileId) -> bool {
        let ws = self.services.workspaces.active_mut();
        let was_focused = ws.focused_tile();
        let moved = match ws.region_of(tile) {
            Some(crate::tiling::FocusRegion::Main) => ws.focus_main_tile(tile),
            Some(crate::tiling::FocusRegion::Dock(side)) => ws.focus_dock_tile(side, tile),
            None => return false,
        };
        if moved && was_focused != Some(tile) {
            self.session_dirty = true;
        }
        true
    }

    /// Open the link group chooser on `tile`, whose header link chip was
    /// pressed: focus that tile first (a chip on an unfocused tile must
    /// open THAT tile's chooser), leave any command line, then run the
    /// action `mod+u` and the status bar's following segment dispatch.
    /// `Frame::request_link_chooser` queues the press; the frame observer
    /// calls this door.
    pub(super) fn open_link_chooser_on(
        &mut self,
        tile: TileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_pressed_tile(tile) {
            return;
        }
        self.leave_command_line(window, cx);
        self.dispatch(
            &crate::actions::ActionId("tile::link_group".into()),
            None,
            window,
            cx,
        );
        cx.notify();
    }

    /// Open the member list on `tile`: focus that tile first
    /// (a marker click on an unfocused tile must open THAT tile's list),
    /// refuse with the notice if it is not a member, close the palette
    /// and any command line, and highlight the active member.
    /// `StackHandle::open_list` and `stack::pick` both call this door.
    pub(super) fn open_stack_list(
        &mut self,
        tile: TileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_pressed_tile(tile) {
            return;
        }
        let Some((index, _)) = self.services.workspaces.active().stack_position(tile) else {
            self.notice = Some(super::input::NOT_IN_A_STACK.into());
            cx.notify();
            return;
        };
        let members = self.stack_members_of(tile);
        self.close_palette(window, cx);
        self.leave_command_line(window, cx);
        self.stack_list = Some(super::stacklist::StackList {
            tile,
            members,
            highlighted: index - 1,
        });
        // Give the member list shell focus immediately. Closing the palette may
        // restore the scope input, whose caret would otherwise consume list
        // commands before they reached raw key routing.
        if !window
            .focused(cx)
            .is_some_and(|focused| focused == self.focus_handle)
        {
            self.focus_handle.focus(window, cx);
        }
        self.note_keyboard_focus_move(window, cx);
        cx.notify();
    }

    /// The members of `tile`'s stack in stack order (`Tree::stack_members`
    /// over `Workspace::stack_members`).
    fn stack_members_of(&self, tile: TileId) -> Vec<TileId> {
        self.services
            .workspaces
            .active()
            .stack_members(tile)
            .unwrap_or_default()
    }

    /// Close the member list. Escape, outside clicks, dispatch, and palette
    /// opening share this path.
    pub(super) fn close_stack_list(&mut self, cx: &mut Context<Self>) {
        if self.stack_list.take().is_some() {
            cx.notify();
        }
    }

    /// Make `id` the painted, focused member and close the list.
    pub(super) fn activate_stack_member(
        &mut self,
        id: TileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ws = self.services.workspaces.active_mut();
        let moved = match ws.region_of(id) {
            Some(crate::tiling::FocusRegion::Main) => ws.focus_main_tile(id),
            Some(crate::tiling::FocusRegion::Dock(side)) => ws.focus_dock_tile(side, id),
            None => false,
        };
        if moved {
            self.session_dirty = true;
            self.note_keyboard_focus_move(window, cx);
        }
        self.close_stack_list(cx);
    }

    /// After a keyboard action moves tile focus, schedule shell focus
    /// restoration if an occupant still holds the keyboard. Otherwise an
    /// editor in the old tile could consume text intended for the new tile.
    ///
    /// Shell inputs retain their caret. The occupant's editor remains open;
    /// its module owns commit/cancel. `render` consumes the flag before the
    /// next paint, using `occupant_insert_stack` to preserve a valid editor.
    pub(super) fn note_keyboard_focus_move(&mut self, window: &Window, cx: &App) {
        if window
            .focused(cx)
            .is_some_and(|focused| !self.holds_shell_focus(&focused, cx))
        {
            self.pending_focus_restore = true;
        }
    }

    /// Return the focused tile's context stack only when that occupant owns
    /// window focus and reports insert mode. Key routing and deferred focus
    /// restoration share this predicate, so a mouse-opened editor keeps focus
    /// under the same conditions that allow it to receive text.
    ///
    /// An open editor alone is insufficient: another tile may own the focused
    /// input after a keyboard focus move. Checking ownership prevents routing
    /// against one tile's stack while typing into another tile's editor.
    ///
    /// Returning the stack lets key routing reuse it without computing it twice.
    pub(super) fn occupant_insert_stack(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<Vec<crate::keymap::KeyContext>> {
        window.focused(cx)?;
        if let Some(page) = self.page.as_ref().filter(|p| p.open) {
            // The full stack, unfiltered: `insert_contexts` applies the
            // `mode == insert` filter to bare keys itself, so a page whose
            // context carries no `mode` lets bare keys reach its input.
            if !page.occupant.content.holds_focus(window, cx) {
                return None;
            }
            return Some(self.context_stack(cx));
        }
        let tile = self.services.workspaces.active().focused_tile()?;
        if !self.occupants.get(&tile)?.content.holds_focus(window, cx) {
            return None;
        }
        let stack = self.context_stack(cx);
        stack
            .iter()
            .any(|c| c.get("mode") == Some("insert"))
            .then_some(stack)
    }

    /// [`Self::occupant_insert_stack`] as a yes/no — `render`'s door.
    pub(super) fn occupant_holds_insert_focus(&self, window: &Window, cx: &App) -> bool {
        self.occupant_insert_stack(window, cx).is_some()
    }

    /// Is `handle` one of the shell's own focusable surfaces — the root,
    /// or one of the four `Entity<InputState>`s a user can be typing
    /// into? Anything else that holds window focus belongs to a tile's
    /// occupant view. Two callers read it for that one distinction:
    /// `ensure_occupants`'s backstop above (see it for why the distinction
    /// is the whole decision) and `note_keyboard_focus_move`, which arms
    /// the restore only when a tile — never a shell surface — is holding
    /// the keyboard as tile focus moves. The insert-focus predicate
    /// (`occupant_insert_stack`) asks the module directly instead
    /// (`TileContent::holds_focus`), see its doc for why that subsumes
    /// this.
    pub(super) fn holds_shell_focus(&self, handle: &FocusHandle, cx: &App) -> bool {
        *handle == self.focus_handle
            || [
                &self.palette_input,
                &self.dialog_input,
                &self.command_input,
                &self.filter_input,
            ]
            .iter()
            .any(|input| input.read(cx).focus_handle(cx) == *handle)
    }
}
