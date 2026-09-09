//! Tile occupant lifecycle (spec section 3.2, module hosting contract):
//! which tiles need a module occupant right now, creating them on demand
//! through the module roster, delivering async query results to the tile
//! that asked for them, and the pure `session::TileRecords` snapshot the
//! session writer serializes. Split out of `shell/mod.rs` (Phase 3c
//! Task 0) as the seam `render.rs`'s per-frame reconciliation and
//! `session_io.rs`'s writer both call into.

use std::collections::HashSet;

use gpui::{App, Context, Window};

use crate::module::ModuleFactory as _;
use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::session;
use crate::tiling::TileId;
use geode_core::query::{QueryKey, QueryOutcome};

use super::ShellView;

impl ShellView {
    /// Every non-placeholder occupant's tile record, gathered fresh from
    /// `serialize` (Task 4, Phase 3 §3.5). A placeholder tile carries no
    /// module of its own — it exists only until something opens on it —
    /// so it is never written.
    pub(super) fn current_tiles(&self, cx: &App) -> session::TileRecords {
        self.occupants
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
            .collect()
    }

    /// The module kind occupying `tile`, or `None` if it has no occupant
    /// (not a tile at all, or not yet created).
    pub fn occupant_kind(&self, tile: TileId) -> Option<&'static str> {
        self.occupants.get(&tile).map(|o| o.kind)
    }

    /// Route a query outcome to the tile whose id is its key (§5.1). The
    /// app bridge calls this; an outcome for a tile that no longer exists
    /// is dropped.
    pub fn deliver(&mut self, outcome: QueryOutcome, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(o) = self.occupants.get(&TileId(outcome.key.0)) {
            o.content.deliver(outcome, window, cx);
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
            let (factory, state) = match (matched, pending_factory) {
                (Some(f), _) => (Some(f), restored_state),
                (None, Some(f)) => (Some(f), pending_state),
                (None, None) => (self.services.roster.default_factory(), None),
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
        self.scratch_all_tiles = all;

        for id in self.visible_tiles.difference(&active) {
            if let Some(o) = self.occupants.get(id) {
                o.content.set_visible(false, cx);
            }
        }
        for id in active.difference(&self.visible_tiles) {
            if let Some(o) = self.occupants.get(id) {
                o.content.set_visible(true, cx);
            }
        }
        self.visible_tiles.clear();
        self.visible_tiles.extend(active.iter().copied());
        self.scratch_active_tiles = active;
    }
}
