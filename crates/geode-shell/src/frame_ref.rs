//! A tile's handle on the frame: the entity plus the workspace the tile
//! lives in. Tiles never move between workspaces, so the binding is fixed
//! for the tile's life; pinning or unpinning that workspace changes which
//! lane `read` resolves to without the tile re-subscribing.

use gpui::{App, AppContext, Context, Entity};

use crate::frame::{Frame, FrameView, FrameViewMut};
use crate::tiling::WorkspaceIx;

#[derive(Clone)]
pub struct FrameRef {
    entity: Entity<Frame>,
    ws: WorkspaceIx,
}

impl FrameRef {
    pub fn new(entity: Entity<Frame>, ws: WorkspaceIx) -> FrameRef {
        FrameRef { entity, ws }
    }

    /// What a tile observes. A change in any lane notifies every observer;
    /// comparing `read(cx).versions()` filters the ones that matter.
    pub fn entity(&self) -> &Entity<Frame> {
        &self.entity
    }

    pub fn workspace(&self) -> WorkspaceIx {
        self.ws
    }

    pub fn read<'a>(&self, cx: &'a App) -> FrameView<'a> {
        self.entity.read(cx).view(self.ws)
    }

    pub fn update<R, C: AppContext>(
        &self,
        cx: &mut C,
        f: impl FnOnce(&mut FrameViewMut<'_>, &mut Context<Frame>) -> R,
    ) -> R {
        let ws = self.ws;
        self.entity
            .update(cx, |frame, cx| f(&mut frame.view_mut(ws), cx))
    }
}
