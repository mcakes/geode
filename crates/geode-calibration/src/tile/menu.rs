//! The `.` action menu's lifecycle. The rows are `core::menu`'s over the
//! shared `geode_tile::menu` door: a pick closes the menu and re-enters
//! `dispatch` on the row's action, so a row, its key and the palette take
//! one path; a disabled row's pick is its reason, as a notice, and the
//! menu stays. It opens from `.` and the header's `⋯`, hangs from the
//! header's right edge, and closes on `.`, `escape`, a pick, a press
//! outside it, or any other verb reaching the tile. While it is up the
//! tile reports `mode == menu`, `popup == actions` and `tilelist`, so the
//! shell's shared `j`/`k` and arrows step it; the rows are rebuilt when the
//! tile changes under it, so what a row promises is true when picked.

use geode_shell::actions::ActionId;
use geode_tile::menu::{Menu, MenuHost, MenuIds, Row, live_bindings, render_menu};
use gpui::{Anchor, App, Context, Deferred, Entity, Pixels, Point, Window};

use super::CalibrationTile;
use crate::core::menu::{self, MenuInputs};

/// The open menu.
pub(super) struct MenuState {
    pub(super) menu: Menu<ActionId>,
    /// Where a right press opened it, in window pixels: the menu hangs
    /// from that point in the body. `None` for `.` and the `⋯` button,
    /// whose menu hangs from the header's right edge.
    pub(super) at: Option<Point<Pixels>>,
}

/// The menu's element names for tile `id`.
pub(super) fn menu_ids(id: u64) -> MenuIds {
    MenuIds::new(
        format!("calibration-menu-{id}"),
        format!("calibration-menu-row-{id}"),
    )
}

impl CalibrationTile {
    /// The rows over the tile as it is now.
    fn menu_rows(&self, cx: &App) -> Vec<Row<ActionId>> {
        menu::rows(&MenuInputs {
            cursor: self.cursor.as_deref(),
            // The live pipeline, not the painted one: a held, dropped
            // pipeline runs nothing, and the rows say so.
            pipeline: self.pipeline().is_some(),
            publish: self.publish_check(cx),
        })
    }

    /// `.` and the `⋯` button: open the menu, or close it when it is up.
    pub(super) fn toggle_menu(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(cx);
        } else {
            self.open_menu(None, cx);
        }
    }

    /// Open the menu over the tile as it is now, hung from `at` (a right
    /// press) or from the header (`None`); one already up is replaced, so
    /// a second right press moves it.
    pub(super) fn open_menu(&mut self, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let rows = self.menu_rows(cx);
        self.menu = Some(MenuState {
            menu: Menu::new(rows, &live_bindings(cx)),
            at,
        });
        cx.notify();
    }

    /// Close the menu. `true` when one was up.
    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.menu.take().is_none() {
            return false;
        }
        cx.notify();
        true
    }

    /// Step the highlight over the enabled rows. `true` when a menu is up.
    pub(super) fn step_menu(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(m) = &mut self.menu else {
            return false;
        };
        m.menu.step(delta);
        cx.notify();
        true
    }

    /// `enter`: pick the highlighted row. An all-disabled menu has no
    /// highlight, and `enter` does nothing.
    pub(super) fn pick_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.menu.as_ref().and_then(|m| m.menu.highlighted()) {
            self.menu_pick(i, window, cx);
        }
    }

    /// Rebuild an open menu's rows when what they promise moved under it
    /// (a delivery, a cursor move, a reload). Runs from `rebuild_model`,
    /// only while the menu is up.
    pub(super) fn refresh_menu(&mut self, cx: &App) {
        if self.menu.is_none() {
            return;
        }
        let rows = self.menu_rows(cx);
        if let Some(m) = &mut self.menu {
            m.menu.replace_rows(rows, &live_bindings(cx));
        }
    }

    /// Re-resolve the open menu's key hints against a republished keymap.
    pub(super) fn rehint_menu(&mut self, cx: &App) {
        if let Some(m) = &mut self.menu {
            m.menu.rehint(&live_bindings(cx));
        }
    }

    /// The open header menu, painted to hang by its top-right corner from
    /// the point the caller places it at; `None` with none up or with the
    /// menu hung from a pointer press (`render_pointer_menu`).
    pub(super) fn render_menu(&self, tile: &Entity<Self>, cx: &App) -> Option<Deferred> {
        let m = self.menu.as_ref().filter(|m| m.at.is_none())?;
        Some(self.paint_menu(m, Anchor::TopRight, tile, cx))
    }

    /// The open menu a right press hung from the pointer, by its top-left
    /// corner, with the point it hangs from; `None` with none up or with
    /// the header's menu.
    pub(super) fn render_pointer_menu(
        &self,
        tile: &Entity<Self>,
        cx: &App,
    ) -> Option<(Point<Pixels>, Deferred)> {
        let m = self.menu.as_ref()?;
        let at = m.at?;
        Some((at, self.paint_menu(m, Anchor::TopLeft, tile, cx)))
    }

    fn paint_menu(&self, m: &MenuState, corner: Anchor, tile: &Entity<Self>, cx: &App) -> Deferred {
        render_menu(
            &m.menu,
            &self.menu_ids,
            corner,
            tile,
            |t: &mut CalibrationTile, _window, cx| {
                t.close_menu(cx);
            },
            cx,
        )
    }

    /// The open menu's rows by action id and its highlight, `None` with no
    /// menu up.
    #[cfg(test)]
    pub(crate) fn menu_open(&self) -> Option<(Vec<String>, Option<usize>)> {
        let m = self.menu.as_ref()?;
        Some((
            m.menu
                .rows()
                .iter()
                .filter_map(Row::action)
                .map(|r| r.pick().0.clone())
                .collect(),
            m.menu.highlighted(),
        ))
    }

    /// The open menu's resolved key lane on the row for `id`.
    #[cfg(test)]
    pub(crate) fn menu_lane(&self, id: &str) -> Option<geode_tile::menu::Lane> {
        self.menu
            .as_ref()?
            .menu
            .rows()
            .iter()
            .filter_map(Row::action)
            .find(|r| r.pick().0 == id)
            .map(|r| r.lane().clone())
    }

    /// The open menu's reason on the row for `id`; `None` when enabled or
    /// with no menu up.
    #[cfg(test)]
    pub(crate) fn menu_reason(&self, id: &str) -> Option<String> {
        self.menu
            .as_ref()?
            .menu
            .rows()
            .iter()
            .filter_map(Row::action)
            .find(|r| r.pick().0 == id)
            .and_then(|r| r.reason().map(|s| s.to_string()))
    }
}

impl MenuHost for CalibrationTile {
    /// The pointer resting on row `index`: the mouse form of `j`/`k`.
    /// Change-only: gpui fires it on every move over the row.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(m) = &mut self.menu
            && m.menu.highlight(index)
        {
            cx.notify();
        }
    }

    /// `enter` on the highlighted row, or a click on any row. A disabled
    /// row's reason becomes the notice and the menu stays; an enabled one
    /// closes the menu and dispatches its action — or, for the shell's own
    /// `Open stage`, which the tile's dispatch does not carry, asks the
    /// shell through the frame (`FrameRef::request_action`), which runs it
    /// for the focused tile as a bound `enter` would.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = &self.menu else {
            return;
        };
        match m.menu.pick(index) {
            Some(Ok(id)) => {
                self.close_menu(cx);
                if id.0 == menu::OPEN_STAGE {
                    self.frame.request_action(id, cx);
                } else {
                    self.dispatch(&id, None, window, cx);
                }
            }
            Some(Err(reason)) => self.refuse(reason, cx),
            None => {}
        }
    }
}
