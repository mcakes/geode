//! The query chain and following. Until the chain is wired the tile asks
//! for nothing: visibility, closing and deliveries change no state, and
//! the scope it reads out is the frame's effective scope over its filter.

use super::ScenarioTile;
use geode_core::query::{DistinctOutcome, QueryOutcome};
use geode_core::scope::Scope;
use gpui::{App, Context};

impl ScenarioTile {
    /// The scope the header reads out: what the next query would run under.
    pub(crate) fn effective_scope_for_title(&self, cx: &App) -> Scope {
        if self.state.unscoped {
            return self.state.filter.clone();
        }
        self.frame
            .read(cx)
            .effective_scope(&self.state.filter)
            .unwrap_or_else(|_| self.state.filter.clone())
    }

    pub(crate) fn requery(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    pub(crate) fn rebuild_pivot(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    pub(crate) fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        // The title reads the frame's scope.
        cx.notify();
    }

    pub(crate) fn set_visible(&mut self, _visible: bool, _cx: &mut Context<Self>) {}

    /// Closing cancels anything in flight under the tile's key.
    pub(crate) fn closed(&mut self, _cx: &mut Context<Self>) {
        self.data.cancel(self.key());
    }

    pub(crate) fn deliver_query(&mut self, _outcome: QueryOutcome, _cx: &mut Context<Self>) {}

    pub(crate) fn deliver_distinct(&mut self, _outcome: DistinctOutcome, _cx: &mut Context<Self>) {}
}
