//! Tile occupant lifecycle (spec section 3.2, module hosting contract):
//! which tiles need a module occupant right now, creating them on demand
//! through the module roster, delivering async query results to the tile
//! that asked for them, and the pure `session::TileRecords` snapshot the
//! session writer serializes. Split out of `shell/mod.rs` (Phase 3c
//! Task 0) as the seam `render.rs`'s per-frame reconciliation and
//! `session_io.rs`'s writer both call into.

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
    /// Every non-placeholder occupant's tile record, gathered fresh from
    /// `serialize` (Task 4, Phase 3 §3.5), plus every *unplaced* record —
    /// one this build had no factory for, so the tile paints a
    /// placeholder while the record it was restored from rides through
    /// untouched (spec 2026-09-08 add-tile §7.2). A placeholder tile
    /// carries no module state of its own, so it is never written from
    /// the occupant side; without the unplaced records the very next
    /// flush would drop a session saved by a build with more modules.
    /// A live occupant always wins the id — `or_insert_with` never
    /// overwrites one — and `ensure_occupants`/`add_tile` retain the map
    /// to tiles that are still both live and unfilled.
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

    /// Route a delivery to the tile whose id is `delivery.key()` (§5.1).
    /// The app bridge calls this; a delivery for a tile that no longer
    /// exists is dropped.
    pub fn deliver(&mut self, delivery: Delivery, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(o) = self.occupants.get(&TileId(delivery.key().0)) {
            o.content.deliver(delivery, window, cx);
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

    /// The tiles of the active workspace: what is on screen. Same
    /// out-parameter shape as `fill_all_tiles`, same reason.
    fn fill_active_tiles(&self, out: &mut HashSet<TileId>) {
        out.clear();
        let ws = self.services.workspaces.active();
        out.extend(ws.tree().tiles());
        for (_, dock) in ws.docks().iter() {
            if dock.visible() {
                out.extend(dock.tree().tiles());
            }
        }
    }

    /// The `QueryKey`s of every tile in the active workspace that has an
    /// occupant right now (Phase 4 §3.10) — what `ShellView::
    /// on_frame_changed` opens a flip barrier over on a scope/grouping/
    /// as-of change, so every tile that is actually going to requery
    /// (rather than one still waiting on `ensure_occupants`, or one in a
    /// workspace/dock nobody can see) is exactly what the barrier waits
    /// on. `out` is cleared and refilled, same reason `fill_all_tiles`/
    /// `fill_active_tiles` take an out-parameter — a flip opens on a user
    /// mutation, not every render, but there's no reason to allocate
    /// fresh every time either (the caller passes its own scratch `Vec`).
    pub(super) fn visible_tile_keys(&self, out: &mut Vec<QueryKey>) {
        out.clear();
        // Phase 4b M8: a placeholder occupant (nothing has opened on
        // this tile yet) never submits a query and never arrives, so a
        // barrier that waited on it would sit open until `FLIP_DEADLINE`
        // every single time — the placeholder is filtered out here
        // rather than counted as a tile the barrier should wait for.
        let has_real_occupant = |id: &TileId| {
            self.occupants
                .get(id)
                .is_some_and(|o| o.kind != PLACEHOLDER_KIND)
        };
        let ws = self.services.workspaces.active();
        out.extend(
            ws.tree()
                .tiles()
                .into_iter()
                .filter(has_real_occupant)
                .map(|id| QueryKey(id.0)),
        );
        for (_, dock) in ws.docks().iter() {
            if dock.visible() {
                out.extend(
                    dock.tree()
                        .tiles()
                        .into_iter()
                        .filter(has_real_occupant)
                        .map(|id| QueryKey(id.0)),
                );
            }
        }
    }

    /// Create occupants for tiles that lack one, drop occupants whose tile
    /// is gone, and tell occupants when they enter or leave the screen.
    /// Runs at the top of `render`, the one place with a `Window` on every
    /// path that can change the tile set (a split, a close, a restore, a
    /// workspace switch).
    ///
    /// The all-tiles/active-tiles sets are computed into `scratch_all_tiles`
    /// / `scratch_active_tiles`, reused every frame so nothing is allocated
    /// once warm (fix-round finding: this used to allocate two fresh
    /// `HashSet`s per render). Each is taken out of `self` for the
    /// duration of the borrow-heavy loop below (`f.create` needs `&mut
    /// self.services`/`cx`, which a live borrow of a `self` field would
    /// block) and put back before returning.
    pub(super) fn ensure_occupants(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut all = std::mem::take(&mut self.scratch_all_tiles);
        self.fill_all_tiles(&mut all);
        // Phase 4b Task 5 fix round 1, MAJ-2: tell a vanished tile's
        // occupant it is no longer visible BEFORE dropping it — the
        // visibility diff further down only ever compares
        // `self.visible_tiles` against `active` (the *live* set), so a
        // tile that closed between one render and the next was never in
        // `active` to begin with and that diff's `self.occupants.get(id)`
        // would already be `None` by the time it got there, silently
        // skipping the `set_visible(false)` call every occupant is owed
        // (`TileContent::set_visible`'s own doc comment: "hidden tiles
        // may drop subscriptions"). No `Drop` impl can do this instead —
        // it has no `Context` to call back into gpui with — and this is
        // generic over every module, not diagnostics-specific: any
        // occupant that opens something in `set_visible(true)` (a
        // `Diagnostics::watch()`, a future module's own equivalent) leaks
        // it forever otherwise.
        for (id, o) in self.occupants.iter() {
            if !all.contains(id) {
                o.content.set_visible(false, cx);
            }
        }
        self.occupants.retain(|id, _| all.contains(id));

        // Computed here, ahead of the creation loop (I2, final review),
        // so a newly created occupant can be told its own starting
        // visibility below — see `TileContent::set_visible`'s doc
        // comment for the contract this satisfies. The diff loop further
        // down is unchanged: a tile created active is told `true` twice
        // (once here, once there, since it is also new to
        // `self.visible_tiles`) — harmless, and simpler than teaching
        // that loop to skip ids this loop already announced.
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

        for id in &creation_order {
            if self.occupants.contains_key(id) {
                continue;
            }
            let restored = self.services.restored_tiles.remove(&id.0);
            let pending = self.pending_tiles.remove(id);
            // A factory found by the record's own `kind` is a real match —
            // its `kind()` equals `restored.kind` by construction, so the
            // restored state is meant for it (fix-round finding). A
            // restored record outranks a pending request (they cannot
            // coexist for one id in practice — restore never allocates a
            // new id and `add_tile` never targets a restored one).
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
            // Restored beats pending beats placeholder (spec 2026-09-08
            // add-tile §7.2). There is no default kind to fall back to:
            // a tile nothing claims paints a placeholder, and if the
            // reason is a restored record this build has no module for,
            // that record is kept verbatim so the next session flush
            // cannot forget it. (A pending request cannot coexist with a
            // restored record for one id — see `matched`'s comment above
            // — so "unmatched restored record" really does mean "this
            // tile is about to be a placeholder".)
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
            // I2 (final review): an occupant created outside the active
            // set (a different workspace, a collapsed dock) was never
            // told anything — it is never in `self.visible_tiles`, so
            // the diff loop below never sees it either, and it would
            // hold whatever it defaults to (visible, per `TileContent::
            // set_visible`'s doc comment) for the rest of the process.
            occupant.content.set_visible(active.contains(id), cx);
            self.occupants.insert(*id, occupant);
        }
        // A request whose tile closed before this render is dropped, not
        // re-aimed (spec 2026-09-08 add-tile §4.3).
        self.pending_tiles.retain(|id, p| {
            let live = all.contains(id);
            if !live {
                tracing::debug!(target: "geode::shell", "dropping a pending '{}' request for closed tile {}", p.kind, id.0);
            }
            live
        });
        // An unplaced record outlives only its own tile: once the tile is
        // gone from every workspace there is nothing left to write it
        // back for (§7.2). Filling the tile in place drops it too —
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
        // The focus backstop (review finding, Important 1). A tile that
        // leaves the visible set is unmounted as an *element* but keeps
        // its occupant: `fill_all_tiles` spans every workspace, so a
        // `mod+2` switch retains the view entity — and with it the
        // `FocusHandle` a focus-tracking view holds as a field. The
        // handle's refcount never reaches zero, so `Window::focused` is
        // still `Some` and `render`'s `is_none()` net cannot see this at
        // all. What actually breaks is dispatch: gpui resolves the
        // focused id against the RENDERED tree and falls back to
        // `root_node_id` when it is absent (`Window::focused_node_id`,
        // pinned rev), and that node carries none of `ShellView`'s
        // element key listeners — so `handle_key_down` stops firing and
        // every shell chord is dead until a click claims focus.
        //
        // Focus is taken back HERE rather than through
        // `pending_focus_restore`, because the flag is consumed at the
        // TOP of render: setting it now would leave one whole frame in
        // which the keyboard is dead, and this runs inside the very
        // render that unmounts the tile.
        //
        // The condition mirrors the net's restraint. Focus on any handle
        // that is not one of the shell's OWN — the root, plus the four
        // `Entity<InputState>` surfaces a user can be typing into — is,
        // by this crate's design, a tile view's, and a tile view's focus
        // is never meant to outlive a render (every tile mouse-down
        // re-arms the restore for exactly that reason). A shell surface
        // that is legitimately focused across the switch — the palette
        // filter, a dialog field, a per-tile command line, the scope bar
        // — keeps its caret. If `ShellView` ever gains another focusable
        // field, it belongs in `holds_shell_focus` below.
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

    /// Is `handle` one of the shell's own focusable surfaces — the root,
    /// or one of the four `Entity<InputState>`s a user can be typing
    /// into? Anything else that holds window focus belongs to a tile's
    /// occupant view. Two callers read it for that one distinction:
    /// `ensure_occupants`'s backstop above (see it for why the distinction
    /// is the whole decision) and `handle_key_down`'s insert-mode branch,
    /// which routes typing at a tile only while a tile — never a shell
    /// surface — actually holds the keyboard.
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
