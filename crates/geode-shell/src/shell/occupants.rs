//! Tile occupant lifecycle:
//! which tiles need a module occupant right now, creating them on demand
//! through the module roster, delivering async query results to the tile
//! that asked for them, and the pure `session::TileRecords` snapshot the
//! session writer serializes. This is the seam shared by `render.rs`'s
//! per-frame reconciliation and `session_io.rs`'s writer.

use std::collections::HashSet;

use gpui::{App, Context, FocusHandle, Focusable as _, Window};

use crate::module::Delivery;
use crate::module::ModuleFactory as _;
use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::session;
use crate::tiling::TileId;
use geode_core::query::QueryKey;

use super::ShellView;

impl ShellView {
    /// Collect fresh serialized state from non-placeholder occupants, plus
    /// preserved records for unavailable modules. Live occupants win an ID
    /// collision. Placeholder occupants have no state of their own; retaining
    /// unplaced records lets sessions survive builds with fewer modules.
    /// Reconciliation and tile filling remove stale unplaced records.
    pub(super) fn current_tiles(&self, cx: &App) -> session::TileRecords {
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
                    },
                )
            })
            .collect();
        for (id, record) in &self.unplaced_records {
            tiles.entry(*id).or_insert_with(|| record.clone());
        }
        tiles
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
    /// Create missing occupants, notify removed occupants that they are hidden,
    /// then drop them. Reusable tile sets retain capacity between frames and
    /// are temporarily taken out of `self` while factory calls borrow services.
    /// A fresh `add_tile` occupant that is on screen and focused hears
    /// `TileContent::launched` once, deferred after the render.
    pub(super) fn ensure_occupants(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut all = std::mem::take(&mut self.scratch_all_tiles);
        self.fill_all_tiles(&mut all);
        // Tell removed occupants they are hidden before dropping them, so they
        // can release subscriptions with a live GPUI context. The visibility
        // diff below can only reach occupants still in the map.
        for (id, o) in self.occupants.iter() {
            if !all.contains(id) {
                o.content.set_visible(false, cx);
            }
        }
        self.occupants.retain(|id, _| all.contains(id));

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
            let occupant = match factory {
                Some(f) => f.create(
                    *id,
                    state,
                    self.frame.clone(),
                    self.diagnostics.clone(),
                    window,
                    cx,
                ),
                None => crate::module::placeholder::PlaceholderFactory.create(
                    *id,
                    None,
                    self.frame.clone(),
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
            // A fresh occupant under this id must hear its stack position
            // even when a previous occupant under the SAME id already did
            // — `add_tile` fills a placeholder in place by removing its
            // occupant and letting this loop recreate one, and without
            // this the delivery loop below sees `stack_sent` still
            // holding the old occupant's last-sent value and skips the
            // new one as already told.
            self.stack_sent.remove(id);
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
        // be included in `holds_shell_focus`.
        if any_tile_left_the_screen
            && let Some(focused) = window.focused(cx)
            && !self.holds_shell_focus(&focused, cx)
        {
            self.focus_handle.focus(window, cx);
        }
        self.visible_tiles.clear();
        self.visible_tiles.extend(active.iter().copied());
        self.scratch_active_tiles = active;
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
        let ws = self.services.workspaces.active_mut();
        let was_focused = ws.focused_tile();
        let moved = match ws.region_of(tile) {
            Some(crate::tiling::FocusRegion::Main) => ws.focus_main_tile(tile),
            Some(crate::tiling::FocusRegion::Dock(side)) => ws.focus_dock_tile(side, tile),
            None => return,
        };
        // Only a change of focused tile dirties the session. The focus methods
        // report success even when the requested tile was already focused.
        if moved && was_focused != Some(tile) {
            self.session_dirty = true;
        }
        let Some((index, _)) = self.services.workspaces.active().stack_position(tile) else {
            self.notice = Some(super::input::NOT_IN_A_STACK);
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
