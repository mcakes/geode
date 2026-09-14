//! The one door every new tile comes through (spec
//! `2026-09-08-geode-add-tile-design.md` §4): `add_tile` resolves a
//! direction, places the tile (fill a placeholder, fill an empty region,
//! or split), and records an addressed occupant request for
//! `ensure_occupants` to fill on the next render. `duplicate_tile` and
//! `open_module` are its two callers besides `dispatch`.

use gpui::{Context, Window};

use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::tileadd::AddDirection;
use crate::tiling::{DockSide, Orientation, TileId};

use super::{PendingTile, ShellView, render::content_area};

impl ShellView {
    /// Create (or fill an empty pane with) a tile of `kind`, carrying
    /// `state` as the factory's `restored` record when given. Direction:
    /// `direction` if given, else the `[tiles] add` setting, with `Auto`
    /// reading the focused tile's painted shape (§4.1). Placement (§4.2),
    /// first match wins: a focused placeholder is filled in place; an
    /// empty focused region gets the tile as its root (`Tree::split` on
    /// an empty tree); otherwise the focused tile is split. Every path
    /// dirties the session and notifies, so `ensure_occupants` sees the
    /// request on the very next render.
    pub fn add_tile(
        &mut self,
        kind: &str,
        direction: Option<Orientation>,
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
            }
            // The tile is claimed now, so a record this build could not
            // place (spec 2026-09-08 add-tile §7.2) is no longer written
            // back — the occupant about to be created owns the id.
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

    /// `workspace::duplicate_*` (§6): the focused occupant's own
    /// `serialize` output becomes the new tile's `restored` record — the
    /// session format exactly, so what survives a restart survives a
    /// duplicate and nothing else does. A placeholder or an empty region
    /// has nothing to duplicate: no-op, no notify.
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
        self.add_tile(&kind, Some(direction), Some(state), window, cx);
    }

    /// Focus an existing occupant of `kind` wherever it lives in the
    /// active workspace (the main tree or a dock), else add one through
    /// [`Self::add_tile`] with the setting's direction (§3.4). A request
    /// already pending for `kind` counts as "open": a second click on the
    /// status bar's diagnostics summary — the one production caller,
    /// since `diagnostics::open` was retired (user ruling 2026-09-09) —
    /// before the render adds nothing.
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
            // A focus move by keyboard, exactly like a directional verb's
            // (I-3): this arm focuses an existing tile, and a tile
            // occupant still holding the window's focus must give the
            // keyboard back.
            self.note_keyboard_focus_move(window, cx);
            cx.notify();
            return;
        }
        if self.pending_tiles.values().any(|p| p.kind == kind) {
            return;
        }
        self.add_tile(kind, None, None, window, cx);
    }
}
