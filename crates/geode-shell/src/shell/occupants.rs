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
use crate::session;
use crate::tiling::TileId;
use geode_core::query::QueryOutcome;

use super::ShellView;

impl ShellView {
    /// Every non-placeholder occupant's tile record, gathered fresh from
    /// `serialize` (Task 4, Phase 3 §3.5). A placeholder tile carries no
    /// module of its own — it exists only until something opens on it —
    /// so it is never written.
    pub(super) fn current_tiles(&self, cx: &App) -> session::TileRecords {
        self.occupants
            .iter()
            .filter(|(_, o)| o.kind != "placeholder")
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

    /// A data-layer diagnostic to show in the status bar (Phase 3 §5.1):
    /// a source's worst health on its last poll, or events dropped
    /// because the app bridge's bounded channel refused a `try_send`.
    /// `None` clears it. The shell cannot query for itself — it does not
    /// depend on `geode-data` (CLAUDE.md) — so `geode-app` is the only
    /// caller, the same relationship [`ShellView::set_probe`] has to the
    /// throwaway probe. Notifies unconditionally, like `set_probe`: a
    /// status the user cannot see is not surfaced.
    pub fn set_data_status(&mut self, status: Option<String>, cx: &mut Context<Self>) {
        self.data_status = status;
        cx.notify();
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

        for id in &all {
            if self.occupants.contains_key(id) {
                continue;
            }
            let restored = self.services.restored_tiles.remove(&id.0);
            // A factory found by the record's own `kind` is a real match —
            // its `kind()` equals `restored.kind` by construction, so the
            // restored state is meant for it. Any fallback (no factory
            // registered for that kind, or no record at all) hands the
            // chosen factory a tile it does not recognise, so it must not
            // see state shaped for a different module (fix-round finding).
            let matched = restored
                .as_ref()
                .and_then(|r| self.services.roster.factory(&r.kind));
            let state = matched.and(restored.as_ref()).map(|r| &r.state);
            let factory = matched.or_else(|| self.services.roster.default_factory());
            let occupant = match factory {
                Some(f) => f.create(*id, state, self.frame.clone(), window, cx),
                None => crate::module::placeholder::PlaceholderFactory.create(
                    *id,
                    None,
                    self.frame.clone(),
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
