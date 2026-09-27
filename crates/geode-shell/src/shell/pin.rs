//! Workspace pinning at the shell: the toggle, the switch hook, and the
//! scope field's rebinding. A text-editing session belongs to one lane;
//! whenever the lane the field edits changes (a switch, a pin, an unpin)
//! the old session ends there and the field re-reads the new lane.

use gpui::{Context, Window};

use super::ShellView;
use crate::tiling::WorkspaceIx;

impl ShellView {
    /// Pin the active workspace to a lane of its own, or return it to the
    /// shared frame. The open text session ends on the lane it started in
    /// first, so an unpin cannot carry a pinned session's base into the
    /// shared lane's history.
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
    pub(super) fn on_workspace_switched(
        &mut self,
        prev: WorkspaceIx,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.frame
            .update(cx, |f, _| f.view_mut(prev).end_scope_session());
        self.last_flip_versions = self.active_frame().read(cx).versions();
        self.rebind_scope_field(window, cx);
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
