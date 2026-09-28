//! Tile creation and module activation.
//!
//! [`ShellView::add_tile`] resolves placement, fills a placeholder or empty
//! region, stacks onto the focused tile, or splits it. It records an addressed
//! occupant request for `ensure_occupants` to fill on the next render.
//! Duplication and opening a module use the same creation path.

use gpui::{Context, Window};

use crate::defaults::AddPlacement;
use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::tileadd::AddDirection;
use crate::tiling::{DockSide, Orientation, TileId};

use super::{PendingTile, ShellView, render::content_area};

impl ShellView {
    /// Create a tile of `kind`, passing `state` to the factory as its restored
    /// record. Placement follows this order: fill a focused placeholder;
    /// stack after a real focused tile for `Stacked`; fill an empty focused
    /// region; otherwise split the focused tile. A stacked add with no focus
    /// falls back to a split, creating a single tile in an empty region.
    ///
    /// Splits use an explicit direction when supplied, otherwise the add
    /// setting. `Auto` uses the focused tile's shape. Every path marks the
    /// session dirty and notifies so the next render creates the occupant.
    pub fn add_tile(
        &mut self,
        kind: &str,
        placement: AddPlacement,
        state: Option<toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.services.workspaces.active().focused_tile();
        if let Some(tile) = focused
            && self.occupant_kind(tile) == Some(PLACEHOLDER_KIND)
        {
            if let Some(o) = self.occupants.remove(&tile) {
                o.content.set_visible(false, cx);
                o.content.closed(cx);
            }
            // The new occupant owns this tile ID. Stop preserving any unrestorable
            // session record that previously occupied its placeholder.
            self.unplaced_records.remove(&tile.0);
            self.pending_tiles.insert(
                tile,
                PendingTile {
                    kind: kind.to_string(),
                    state,
                },
            );
            self.session_dirty = true;
            cx.notify();
            return;
        }
        if placement == AddPlacement::Stacked
            && let Some(id) = self.services.workspaces.stack_active()
        {
            self.pending_tiles.insert(
                id,
                PendingTile {
                    kind: kind.to_string(),
                    state,
                },
            );
            self.session_dirty = true;
            cx.notify();
            return;
        }
        let direction = match placement {
            AddPlacement::Split(d) => d,
            AddPlacement::Stacked => None,
        };
        // The rect is only computed when the setting actually needs it —
        // a layout pass on dispatch is cheap but not free.
        let rect = if direction.is_none() && self.add_direction == AddDirection::Auto {
            focused.and_then(|_| {
                self.services
                    .workspaces
                    .active()
                    .focused_tile_rect(content_area(window))
            })
        } else {
            None
        };
        let orientation = self.add_direction.resolve(direction, rect);
        let id = self.services.workspaces.split_active(orientation);
        self.pending_tiles.insert(
            id,
            PendingTile {
                kind: kind.to_string(),
                state,
            },
        );
        self.session_dirty = true;
        cx.notify();
    }

    /// Duplicate the focused occupant using its serialized session state as
    /// the new tile's restored record. Only state that survives a restart
    /// survives duplication. A placeholder or empty region is a no-op and
    /// sends no notification.
    pub fn duplicate_tile(
        &mut self,
        direction: Orientation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.services.workspaces.active().focused_tile() else {
            return;
        };
        let Some(o) = self.occupants.get(&tile) else {
            return;
        };
        if o.kind == PLACEHOLDER_KIND {
            return;
        }
        let kind = o.kind.to_string();
        let state = o.content.serialize(cx);
        self.add_tile(
            &kind,
            AddPlacement::Split(Some(direction)),
            Some(state),
            window,
            cx,
        );
    }

    /// Focus an existing occupant of `kind` in the active workspace's main
    /// tree or a dock; otherwise add one through [`Self::add_tile`] with the
    /// configured direction. A pending request for `kind` also counts as open,
    /// so repeated requests before the next render create at most one tile.
    pub fn open_module(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        let ws = self.services.workspaces.active();
        let found: Option<(TileId, Option<DockSide>)> = ws
            .tree()
            .tiles()
            .into_iter()
            .find(|id| self.occupant_kind(*id) == Some(kind))
            .map(|id| (id, None))
            .or_else(|| {
                ws.docks().iter().find_map(|(side, dock)| {
                    dock.tree()
                        .tiles()
                        .into_iter()
                        .find(|id| self.occupant_kind(*id) == Some(kind))
                        .map(|id| (id, Some(side)))
                })
            });
        if let Some((tile, side)) = found {
            let ws = self.services.workspaces.active_mut();
            match side {
                Some(side) => {
                    ws.focus_dock_tile(side, tile);
                }
                None => {
                    ws.focus_main_tile(tile);
                }
            }
            self.session_dirty = true;
            // Release keyboard focus from an occupant when moving to the existing
            // tile, as for directional focus actions.
            self.note_keyboard_focus_move(window, cx);
            cx.notify();
            return;
        }
        if self.pending_tiles.values().any(|p| p.kind == kind) {
            return;
        }
        self.add_tile(kind, AddPlacement::Split(None), None, window, cx);
    }
}
