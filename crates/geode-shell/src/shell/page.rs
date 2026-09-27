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
    /// from the weak entity so a page never holds `ShellView`.
    pub(super) fn shell_actions(&self, cx: &Context<Self>) -> ShellActions {
        let weak = cx.entity().downgrade();
        Rc::new(
            move |action: &ActionId, window: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |view, cx| {
                    view.dispatch(action, None, window, cx);
                    cx.notify();
                });
            },
        )
    }

    /// Create on first open, then show. Focus moves to the page. Tiles beneath
    /// are hidden on the next render's `ensure_occupants` pass.
    pub(super) fn open_page(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = &self.page
            && page.occupant.kind != kind
        {
            // One page at a time: a different kind replaces the retained one.
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
