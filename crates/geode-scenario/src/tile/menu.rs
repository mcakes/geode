//! The `.` menu. Until its rows land it holds no rows: `.` opens it (the
//! key context reads `menu`), and `escape` or `.` closes it.

use super::ScenarioTile;
use geode_shell::actions::ActionId;
use gpui::{Context, Window};

/// The open menu.
#[derive(Debug, Default)]
pub(crate) struct MenuState;

impl ScenarioTile {
    pub(crate) fn toggle_menu(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.menu = match self.menu {
            Some(_) => None,
            None => Some(MenuState),
        };
        cx.notify();
    }

    /// A key while the menu is up: `Some(handled)` when the menu took it,
    /// `None` to let the tile route it.
    pub(crate) fn menu_key(
        &mut self,
        action: &ActionId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        match action.0.as_str() {
            "scenario::menu_close" | "scenario::menu_pick" => {
                self.menu = None;
                cx.notify();
                Some(true)
            }
            _ => None,
        }
    }
}
