//! The `.` action menu over the shared `geode_tile::menu` door: the verbs
//! and the parameterless `:` commands. A pick closes the menu and takes the
//! verb's own path (`dispatch` for a verb, `command` for a `:` line), so a
//! row, its key and the palette do one thing. `:filter <expr>` and
//! `:asof <time>` take text and have no row.

use super::ScenarioTile;
use crate::core::pivot::Currency;
use geode_core::scenario::Paint;
use geode_core::view::Scale;
use geode_shell::actions::ActionId;
use geode_tile::menu::{ActionRow, Hint, Menu, MenuHost, MenuIds, Row, live_bindings, render_menu};
use gpui::{Context, Deferred, SharedString, Window};

/// The open menu: its prepared rows and the highlight.
pub(crate) struct MenuState {
    pub menu: Menu<ActionId>,
}

/// A verb's row: its live chord trails the title.
fn verb(id: &'static str, title: &'static str) -> ActionRow<ActionId> {
    ActionRow::new(ActionId(id.to_string()), title).hint(Hint::chord(id))
}

/// A `:` command's row: the line trails the title. Its id is menu-only:
/// no key or palette entry reaches it, only [`ScenarioTile::menu_pick`].
fn command(id: &'static str, title: &'static str, line: &'static str) -> ActionRow<ActionId> {
    ActionRow::new(ActionId(id.to_string()), title).hint(Hint::label(line))
}

/// The `:` line a menu-only command row runs, by its id.
fn command_line(id: &str) -> Option<&'static str> {
    Some(match id {
        "scenario::unscoped" => "unscoped",
        "scenario::scale_k" => "scale k",
        "scenario::scale_m" => "scale m",
        "scenario::scale_1" => "scale 1",
        "scenario::filter_clear" => "filter clear",
        _ => return None,
    })
}

impl ScenarioTile {
    /// The rows for the current state: a verb the shape cannot take is
    /// disabled with the reason its key would give; the toggles are ticked
    /// while on.
    fn menu_rows(&self) -> Vec<Row<ActionId>> {
        let twinned = self.shape.measures().iter().any(|m| m.usd_twin.is_some());
        let currency = if twinned {
            Ok(())
        } else {
            let first = self
                .shape
                .measures()
                .first()
                .map(|m| m.column.clone())
                .unwrap_or_default();
            Err(SharedString::from(format!("no USD twin for {first}")))
        };
        let diff = if self.shape.diff.is_some() {
            Ok(())
        } else {
            Err(SharedString::new_static("this shape has no diff"))
        };
        let filter = if self.state.filter.is_empty() {
            Err(SharedString::new_static("no filter"))
        } else {
            Ok(())
        };
        vec![
            Row::Action(verb("scenario::visual_rows", "Select rows")),
            Row::Action(verb("scenario::visual_block", "Select block")),
            Row::Action(verb("scenario::yank", "Yank")),
            Row::Separator,
            Row::Action(
                verb("scenario::currency", "USD twins")
                    .enabled(currency)
                    .checked(self.state.currency == Currency::Usd),
            ),
            Row::Action(verb("scenario::paint", "Heat").checked(self.state.paint == Paint::Heat)),
            Row::Action(
                verb("scenario::diff", "Diff vs base")
                    .enabled(diff)
                    .checked(self.state.diff),
            ),
            Row::Separator,
            Row::Action(
                command("scenario::unscoped", "Unscoped", ":unscoped").checked(self.state.unscoped),
            ),
            Row::Action(
                command("scenario::scale_k", "Scale k", ":scale k")
                    .checked(self.state.scale == Some(Scale::Thousands)),
            ),
            Row::Action(
                command("scenario::scale_m", "Scale m", ":scale m")
                    .checked(self.state.scale == Some(Scale::Millions)),
            ),
            Row::Action(
                command("scenario::scale_1", "Scale 1", ":scale 1")
                    .checked(self.state.scale.is_none()),
            ),
            Row::Action(
                command("scenario::filter_clear", "Clear filter", ":filter clear").enabled(filter),
            ),
        ]
    }

    pub(crate) fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(window, cx);
        } else {
            self.menu = Some(MenuState {
                menu: Menu::new(self.menu_rows(), &live_bindings(cx)),
            });
            cx.notify();
        }
    }

    pub(crate) fn close_menu(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Ticks and enablement follow the state while the menu is up.
    pub(crate) fn refresh_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.is_none() {
            return;
        }
        let rows = self.menu_rows();
        let bindings = live_bindings(cx);
        if let Some(m) = &mut self.menu
            && m.menu.replace_rows(rows, &bindings)
        {
            cx.notify();
        }
    }

    /// A key while the menu is up: the shared steps, pick and close.
    /// `None` hands the action back to `dispatch`.
    pub(crate) fn menu_key(
        &mut self,
        action: &ActionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        match action.0.as_str() {
            geode_tile::motion::MENU_DOWN | geode_tile::motion::MENU_UP => {
                let by = if action.0 == geode_tile::motion::MENU_DOWN {
                    1
                } else {
                    -1
                };
                if let Some(m) = &mut self.menu {
                    m.menu.step(by);
                    cx.notify();
                }
                Some(true)
            }
            "scenario::menu_pick" => {
                if let Some(i) = self.menu.as_ref().and_then(|m| m.menu.highlighted()) {
                    self.menu_pick(i, window, cx);
                }
                Some(true)
            }
            "scenario::menu_close" => {
                self.close_menu(window, cx);
                Some(true)
            }
            _ => None,
        }
    }

    pub(crate) fn render_menu(&self, cx: &Context<Self>) -> Option<Deferred> {
        let m = self.menu.as_ref()?;
        Some(render_menu(
            &m.menu,
            &self.menu_ids,
            gpui::Anchor::TopRight,
            &cx.entity(),
            |t, window, cx| t.close_menu(window, cx),
            cx,
        ))
    }

    /// The open menu's pick ids in order and its highlight.
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

    /// Row `id`'s tick (`None` for an untickable row); `None` with no menu.
    #[cfg(test)]
    pub(crate) fn menu_tick(&self, id: &str) -> Option<Option<bool>> {
        let m = self.menu.as_ref()?;
        Some(
            m.menu
                .rows()
                .iter()
                .filter_map(Row::action)
                .find(|r| r.pick().0 == id)
                .and_then(|r| r.tick()),
        )
    }

    #[cfg(test)]
    pub(crate) fn menu_enabled(&self, id: &str) -> Option<bool> {
        let m = self.menu.as_ref()?;
        m.menu
            .rows()
            .iter()
            .filter_map(Row::action)
            .find(|r| r.pick().0 == id)
            .map(|r| r.is_enabled())
    }
}

impl MenuHost for ScenarioTile {
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(m) = &mut self.menu
            && m.menu.highlight(index)
        {
            cx.notify();
        }
    }

    /// A row press and `enter`: close, then run the row. A disabled row
    /// says why, as its key would.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picked) = self.menu.as_ref().and_then(|m| m.menu.pick(index)) else {
            return;
        };
        self.close_menu(window, cx);
        match picked {
            Ok(id) => match command_line(&id.0) {
                Some(line) => {
                    if let Err(e) = self.command(line, window, cx) {
                        self.refuse(e, cx);
                    }
                }
                None => {
                    self.dispatch(&id, None, window, cx);
                }
            },
            Err(reason) => self.refuse(reason.to_string(), cx),
        }
    }
}

/// The `⋯` control's selector and the menu's element names for tile
/// `tile`, prepared once at construction. Distinct, so a test finding the
/// menu never finds the control.
pub(crate) fn menu_ids(tile: u64) -> (SharedString, MenuIds) {
    let trigger: SharedString = format!("scenario-menu-button-{tile}").into();
    let ids = MenuIds::new(
        format!("scenario-menu-{tile}"),
        format!("scenario-menu-row-{tile}"),
    );
    (trigger, ids)
}
