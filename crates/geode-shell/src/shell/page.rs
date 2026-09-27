//! One page at a time over the workspace. A page is created on first open
//! and retained for the window's lifetime so its state survives a round trip;
//! `open` is the only flag that changes between toggles.

use std::rc::Rc;

use gpui::{App, Context, Window};

use crate::actions::ActionId;
use crate::module::{PageOccupant, ShellActions};
use crate::shell::ShellView;

pub(super) struct OpenPage {
    pub(super) occupant: PageOccupant,
    pub(super) open: bool,
}

impl ShellView {
    pub(crate) fn page_open(&self) -> bool {
        self.page.as_ref().is_some_and(|p| p.open)
    }

    pub(crate) fn open_page_kind(&self) -> Option<&'static str> {
        self.page
            .as_ref()
            .filter(|p| p.open)
            .map(|p| p.occupant.kind)
    }

    /// The handle a page dispatches registered shell actions through. Built
    /// from the weak entity so a page never holds `ShellView`. The dispatch
    /// is deferred: a page invokes the handle from inside its own entity
    /// update, and a synchronous `page::close` would re-enter that entity
    /// through `PageContent::dispatch` and panic on the double lease.
    pub(super) fn shell_actions(&self, cx: &Context<Self>) -> ShellActions {
        let weak = cx.entity().downgrade();
        Rc::new(
            move |action: &ActionId, window: &mut Window, cx: &mut App| {
                let weak = weak.clone();
                let action = action.clone();
                window.defer(cx, move |window, cx| {
                    let _ = weak.update(cx, |view, cx| {
                        view.dispatch(&action, None, window, cx);
                        cx.notify();
                    });
                });
            },
        )
    }

    /// Where focus returns once an overlay closes or a deferred restore
    /// runs: the open page's handle, else the shell root. The page's own
    /// bindings are reachable only from inside its view.
    pub(super) fn focus_home(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.page.as_ref().filter(|p| p.open) {
            Some(page) => page.occupant.content.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
    }

    /// Create on first open, then show. Focus moves to the page. Tiles
    /// beneath are hidden by `ensure_occupants`'s visibility pass on the next
    /// render: `fill_active_tiles` yields nothing while a page is open.
    pub(super) fn open_page(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = &self.page
            && page.occupant.kind != kind
        {
            // One page at a time: a different kind replaces the retained one.
            // Its last state goes back to the restored map, so the next flush
            // still writes `[pages.<old kind>]` and a later reopen restores
            // it; dropping the page alone would lose it from the file.
            self.services.restored_pages.insert(
                page.occupant.kind.to_string(),
                page.occupant.content.serialize(cx),
            );
            page.occupant.content.set_visible(false, cx);
            self.page = None;
        }
        if self.page.is_none() {
            let Some(factory) = self.services.pages.factory(kind) else {
                tracing::warn!(target: "geode::shell", "no page registered as '{kind}'");
                return;
            };
            let restored = self.services.restored_pages.remove(kind);
            let actions = self.shell_actions(cx);
            let occupant = factory.create(
                restored.as_ref(),
                self.frame.clone(),
                self.diagnostics.clone(),
                actions,
                window,
                cx,
            );
            self.page = Some(OpenPage {
                occupant,
                open: false,
            });
        }
        let page = self.page.as_mut().expect("created above");
        if page.open {
            return;
        }
        page.open = true;
        page.occupant.content.set_visible(true, cx);
        page.occupant.content.focus_handle(cx).focus(window, cx);
        // A sidebar or status-bar click opens the page: keep that mouse-down
        // from bubbling to the tracked shell root and taking focus back. Inert
        // for a keyboard open (the flag is per event and only mouse handling
        // reads it).
        window.prevent_default();
        self.session_dirty = true;
        cx.notify();
    }

    pub(super) fn close_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.page.as_mut() else {
            return;
        };
        if !page.open {
            return;
        }
        page.open = false;
        page.occupant.content.set_visible(false, cx);
        self.focus_handle.focus(window, cx);
        self.session_dirty = true;
        cx.notify();
    }

    pub(super) fn toggle_page(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_page_kind() == Some(kind) {
            self.close_page(window, cx);
        } else {
            self.open_page(kind, window, cx);
        }
    }
}
