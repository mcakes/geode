//! A tile's handle on the frame: the entity, the workspace the tile lives
//! in, and the tile itself. Tiles never move between workspaces, so the
//! binding is fixed for the tile's life; pinning or unpinning that workspace
//! changes which lane `read` resolves to, and following a link group
//! changes whose scope it resolves to, without the tile re-subscribing.
//!
//! A handle bound to no tile (`FrameRef::new`) reads the workspace alone:
//! pages, the shell's own doors and tests hold one.

use gpui::{App, AppContext, Context, Entity};

use crate::frame::{Frame, FrameView, FrameViewMut};
use crate::tiling::{TileId, WorkspaceIx};

#[derive(Clone)]
pub struct FrameRef {
    entity: Entity<Frame>,
    ws: WorkspaceIx,
    /// The tile whose link-group membership `read` and `update` honor;
    /// `None` reads and writes the workspace's lane only.
    tile: Option<TileId>,
}

impl FrameRef {
    /// A workspace's handle, bound to no tile.
    pub fn new(entity: Entity<Frame>, ws: WorkspaceIx) -> FrameRef {
        FrameRef {
            entity,
            ws,
            tile: None,
        }
    }

    /// A tile's handle. The shell hands every occupant one of these: a
    /// handle without its tile would keep reading the workspace's scope
    /// after the tile followed a link group.
    pub fn for_tile(entity: Entity<Frame>, ws: WorkspaceIx, tile: TileId) -> FrameRef {
        FrameRef {
            entity,
            ws,
            tile: Some(tile),
        }
    }

    /// What a tile observes. A change in any lane or link group notifies
    /// every observer; comparing `read(cx).versions()` filters the ones
    /// that matter.
    pub fn entity(&self) -> &Entity<Frame> {
        &self.entity
    }

    pub fn workspace(&self) -> WorkspaceIx {
        self.ws
    }

    pub fn tile(&self) -> Option<TileId> {
        self.tile
    }

    pub fn read<'a>(&self, cx: &'a App) -> FrameView<'a> {
        let frame = self.entity.read(cx);
        match self.tile {
            Some(tile) => frame.view_for(self.ws, tile),
            None => frame.view(self.ws),
        }
    }

    pub fn update<R, C: AppContext>(
        &self,
        cx: &mut C,
        f: impl FnOnce(&mut FrameViewMut<'_>, &mut Context<Frame>) -> R,
    ) -> R {
        let (ws, tile) = (self.ws, self.tile);
        self.entity.update(cx, |frame, cx| {
            let mut view = match tile {
                Some(tile) => frame.view_mut_for(ws, tile),
                None => frame.view_mut(ws),
            };
            f(&mut view, cx)
        })
    }
}
