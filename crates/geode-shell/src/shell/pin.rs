//! Workspace pinning at the shell: the toggle, the switch hook, and the
//! scope field's rebinding. A text-editing session belongs to one lane;
//! whenever the lane the field edits changes (a switch, a pin, an unpin)
//! the old session ends there and the field re-reads the new lane.

use gpui::{Context, Window};

use super::ShellView;
use crate::tiling::WorkspaceIx;

impl ShellView {
    /// Pin the active workspace to a lane of its own, or return it to the
    /// shared frame. On a pin the field's open session is the shared lane's:
    /// left open, it would keep its pushed base (a no-op undo entry when the
    /// edits returned to it) and keep coalescing later shared edits, so it
    /// ends before the field moves to the new lane. On an unpin the pinned
    /// lane, session included, is dropped anyway.
    pub(super) fn toggle_workspace_pin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ws = self.active_ix();
        self.frame.update(cx, |f, cx| {
            f.view_mut(ws).end_scope_session();
            if !f.unpin(ws) {
                f.pin(ws);
            }
            cx.notify();
        });
        self.session_dirty = true;
        self.rebind_scope_field(window, cx);
    }

    /// The active workspace changed from `prev`. Showing another lane is not
    /// a frame change: re-seed the flip baseline instead of opening a barrier.
    /// Between two unpinned workspaces the field still edits the shared
    /// lane, so its session is left whole: ending it would split one edit
    /// into two undo entries and lose the pre-focus text Escape restores.
    pub(super) fn on_workspace_switched(
        &mut self,
        prev: WorkspaceIx,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active_ix();
        let lane_changed = {
            let f = self.frame.read(cx);
            f.is_pinned(prev) || f.is_pinned(active)
        };
        self.last_flip_versions = self.active_frame().read(cx).versions();
        if lane_changed {
            self.frame
                .update(cx, |f, _| f.view_mut(prev).end_scope_session());
            self.rebind_scope_field(window, cx);
        }
        cx.notify();
    }

    /// Show the active lane's text in the scope field. A focused field keeps
    /// focus and starts a fresh session on the new lane, so the next
    /// keystroke coalesces there and Escape restores the new lane's text.
    fn rebind_scope_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.active_frame();
        let text = here.read(cx).scope().text.clone().unwrap_or_default();
        let focused = self.filter_field_focused(window, cx);
        // `set_value` emits no Change event, so this cannot feed the session.
        self.filter_input
            .update(cx, |i, cx| i.set_value(text.clone(), window, cx));
        if focused {
            self.filter_session_base = Some(text);
            here.update(cx, |f, _| f.begin_scope_session());
        } else {
            self.filter_session_base = None;
        }
    }
}
