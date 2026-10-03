//! The shell-hosted classifications entity. It shows one classification
//! from the factory's configuration snapshot, answers the shell's door
//! (`crate::content::ClassificationsContent`) and paints the tile: the
//! header (`header`), the switcher hung beneath its name, the `⋯` menu, and
//! an empty state while there is nothing to show.
//!
//! What paint reads is prepared in `Chrome` whenever the configuration or
//! the shown classification changes, never in render.

mod header;

use std::rc::Rc;
use std::sync::Arc;

use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::{Binding, KeyContext};
use geode_shell::module::{CloseHandle, StackHandle};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_tile::header::{HEADER_HEIGHT, Mode, link_chips};
use geode_tile::menu::{self, ActionRow, Hint, Menu, MenuHost, MenuIds, MenuPick, Row};
use geode_tile::notice::Notice;
use gpui::prelude::*;
use gpui::{Context, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::content::{ClassificationsConfig, Shared, action_title};
use crate::core::session::{self, State};
use header::HeaderModel;

/// Which menu is up: the switcher, hung under the header's name, or the
/// `⋯` action menu, hung from the header's right edge. Both share menu
/// mode, its keys and its pick door; opening one replaces the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Switch,
    Actions,
}

/// What a menu row does when picked.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pick {
    /// Show this classification.
    Show(SharedString),
    /// Run this registered action through the tile's own door.
    Action(&'static str),
}

impl MenuPick for Pick {
    fn element_name(&self) -> SharedString {
        match self {
            Pick::Show(name) => name.clone(),
            Pick::Action(id) => SharedString::new_static(id),
        }
    }
}

const NEW_ACTION: &str = "classifications::new";

/// Prepared paint input.
#[derive(Default)]
struct Chrome {
    header: HeaderModel,
    /// The body's empty state, `None` while a classification is shown.
    empty: Option<SharedString>,
    notices: Vec<Notice>,
}

pub struct ClassificationsTile {
    id: TileId,
    frame: FrameRef,
    /// The data tier's door; the grid asks it for each value's row count.
    #[allow(dead_code)]
    data: DataHandle,
    shared: Rc<Shared>,
    state: State,
    menu: Option<(MenuKind, Menu<Pick>)>,
    /// The live keymap the menus' hints are resolved against.
    chords: Arc<Vec<Binding>>,
    stack: Option<StackHandle>,
    close: Option<CloseHandle>,
    /// Restore and refusal notices, painted in the header's cluster.
    notices: Vec<String>,
    chrome: Chrome,
    menu_selector: SharedString,
    menu_tip: SharedString,
    switch_tip: SharedString,
}

impl ClassificationsTile {
    pub(crate) fn new(
        id: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        data: DataHandle,
        shared: Rc<Shared>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ClassificationsTile {
        let (state, notices) = restored.map(session::from_table).unwrap_or_default();
        // A keymap reload re-resolves an open menu's hints at once.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.chords = menu::live_bindings(cx);
            if let Some((_, m)) = this.menu.as_mut() {
                m.rehint(&this.chords);
                cx.notify();
            }
        })
        .detach();
        let mut tile = ClassificationsTile {
            id,
            frame,
            data,
            shared,
            state,
            menu: None,
            chords: menu::live_bindings(cx),
            stack: None,
            close: None,
            notices,
            chrome: Chrome::default(),
            menu_selector: format!("classifications-menu-button-{}", id.0).into(),
            menu_tip: format!("tip-classifications-menu-{}", id.0).into(),
            switch_tip: format!("tip-classifications-switch-{}", id.0).into(),
        };
        tile.settle();
        tile
    }

    /// The factory stored a new configuration snapshot.
    pub fn config_changed(&mut self, cx: &mut Context<Self>) {
        self.settle();
        cx.notify();
    }

    /// Bring the tile in line with the snapshot: a tile showing nothing
    /// (new, or its classification gone) offers the switcher at once when
    /// there is something to pick; an open switcher takes the new names.
    fn settle(&mut self) {
        let shown = self.shown().is_some();
        match self.menu.as_ref().map(|(k, _)| *k) {
            Some(MenuKind::Switch) => {
                let rows = self.switch_rows();
                if rows.is_empty() {
                    self.menu = None;
                } else if let Some((_, m)) = self.menu.as_mut() {
                    m.replace_rows(rows, &self.chords);
                }
            }
            _ if !shown => {
                let rows = self.switch_rows();
                if !rows.is_empty() {
                    self.menu = Some((MenuKind::Switch, Menu::new(rows, &self.chords)));
                }
            }
            _ => {}
        }
        self.rebuild_chrome();
    }

    fn rebuild_chrome(&mut self) {
        let config = self.shared.config.borrow();
        let dim = self
            .state
            .name
            .as_deref()
            .and_then(|n| config.as_ref()?.dims.get(n));
        let layer = dim.and_then(|d| config.as_ref()?.layers.get(&d.name).copied());
        self.chrome.header = HeaderModel::prepare(dim, layer);
        let none_defined = config
            .as_ref()
            .is_none_or(|c| c.dims.all().next().is_none());
        let new = action_title(NEW_ACTION);
        self.chrome.empty = match (&self.state.name, dim) {
            (_, Some(_)) => None,
            (Some(gone), None) if none_defined => {
                Some(format!("{gone} no longer exists. {new} makes a classification.").into())
            }
            (Some(gone), None) => {
                Some(format!("{gone} no longer exists. Pick another classification.").into())
            }
            (None, None) if none_defined => {
                Some(format!("No classifications yet. {new} makes one.").into())
            }
            (None, None) => Some(SharedString::new_static("Pick a classification.")),
        };
        self.chrome.notices = self.notices.iter().cloned().map(Notice::danger).collect();
    }

    /// The classification on screen: the one the tile names, while the
    /// snapshot still defines it.
    fn shown(&self) -> Option<String> {
        let name = self.state.name.as_deref()?;
        let config = self.shared.config.borrow();
        config
            .as_ref()
            .and_then(|c| c.dims.get(name))
            .map(|d| d.name.clone())
    }

    /// One row per classification, alphabetical, the shown one ticked.
    fn switch_rows(&self) -> Vec<Row<Pick>> {
        let config = self.shared.config.borrow();
        let mut names: Vec<&str> = config
            .iter()
            .flat_map(|c: &ClassificationsConfig| c.dims.all().map(|d| d.name.as_str()))
            .collect();
        names.sort_unstable();
        names
            .into_iter()
            .map(|name| {
                let checked = self.state.name.as_deref() == Some(name);
                let label = SharedString::from(name.to_string());
                Row::Action(ActionRow::new(Pick::Show(label.clone()), label).checked(checked))
            })
            .collect()
    }

    /// The `⋯` menu: the tile's actions with their live chords.
    fn action_rows(&self) -> Vec<Row<Pick>> {
        let action = |id: &'static str| {
            Row::Action(ActionRow::new(Pick::Action(id), action_title(id)).hint(Hint::chord(id)))
        };
        vec![action("classifications::switch")]
    }

    /// Open the `kind` menu, or close it when it is the one open. The other
    /// kind's open menu is replaced, not stacked. A switcher with nothing to
    /// list refuses into the header rather than paint an empty list.
    fn toggle_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_some_and(|(k, _)| *k == kind) {
            self.menu = None;
        } else {
            let rows = match kind {
                MenuKind::Switch => self.switch_rows(),
                MenuKind::Actions => self.action_rows(),
            };
            if rows.is_empty() {
                self.menu = None;
                self.notice("no classifications to switch to".into());
            } else {
                // The switcher opens on the row in force.
                let at = rows
                    .iter()
                    .position(|r| r.action().is_some_and(|a| a.tick() == Some(true)));
                self.menu = Some((kind, Menu::new(rows, &self.chords).open_at(at)));
            }
        }
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn notice(&mut self, text: String) {
        if !self.notices.contains(&text) {
            self.notices.push(text);
            self.rebuild_chrome();
        }
    }

    /// Show `name`. The cursor belonged to the previous classification's
    /// values, so it is dropped.
    fn show(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.state.name.as_deref() != Some(name) {
            self.state.name = Some(name.to_string());
            self.state.cursor = None;
        }
        self.rebuild_chrome();
        cx.notify();
    }

    /// `menu` while a menu is up, `normal` otherwise.
    fn mode(&self) -> &'static str {
        if self.menu.is_some() {
            "menu"
        } else {
            "normal"
        }
    }

    /// `tilelist` only while a menu is up, so the shell's shared `j`/`k`
    /// and arrows step its rows.
    pub fn key_context(&self) -> KeyContext {
        let ctx = KeyContext::new(crate::KIND).pair("mode", self.mode());
        if self.menu.is_some() {
            ctx.tilelist()
        } else {
            ctx
        }
    }

    /// `true` for the actions this tile carries out and, while a menu is
    /// up, the shell's shared list steps. A registered action the tile does
    /// not carry out yet falls through to the shell.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        _count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match action.0.as_str() {
            geode_tile::motion::MENU_DOWN | geode_tile::motion::MENU_UP if self.menu.is_some() => {
                let delta = if action.0 == geode_tile::motion::MENU_DOWN {
                    1
                } else {
                    -1
                };
                if let Some((_, m)) = self.menu.as_mut() {
                    m.step(delta);
                }
                cx.notify();
            }
            "classifications::switch" => self.toggle_menu(MenuKind::Switch, cx),
            "classifications::menu" => self.toggle_menu(MenuKind::Actions, cx),
            "classifications::commit" => {
                let Some(at) = self.menu.as_ref().and_then(|(_, m)| m.highlighted()) else {
                    return false;
                };
                self.menu_pick(at, window, cx);
            }
            "classifications::cancel" if self.menu.is_some() => self.close_menu(cx),
            _ => return false,
        }
        true
    }

    /// Nothing is asked of the data tier yet, so being shown or hidden
    /// changes nothing.
    pub fn set_visible(&mut self, _visible: bool) {}

    pub fn closed(&mut self) {
        self.menu = None;
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    pub fn set_close(&mut self, close: CloseHandle, cx: &mut Context<Self>) {
        self.close = Some(close);
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        match &self.chrome.header.name {
            Some(name) => format!("Classification: {name}").into(),
            None => SharedString::new_static(header::NONE_SHOWN),
        }
    }

    pub fn serialize(&self) -> toml::Table {
        session::to_table(&self.state)
    }

    #[cfg(test)]
    fn title_text(&self) -> String {
        self.chrome.header.text()
    }

    /// The open switcher's rows and ticks; `None` while it is closed.
    #[cfg(test)]
    fn switcher_rows(&self) -> Option<Vec<(String, bool)>> {
        let (MenuKind::Switch, m) = self.menu.as_ref()? else {
            return None;
        };
        Some(
            m.rows()
                .iter()
                .filter_map(Row::action)
                .map(|a| (a.title().to_string(), a.tick() == Some(true)))
                .collect(),
        )
    }

    #[cfg(test)]
    fn empty_text(&self) -> Option<String> {
        self.chrome.empty.as_ref().map(|s| s.to_string())
    }
}

impl MenuHost for ClassificationsTile {
    /// A disabled row says why and keeps the menu open; an enabled one
    /// closes it and acts.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picked) = self.menu.as_ref().and_then(|(_, m)| m.pick(index)) else {
            return;
        };
        match picked {
            Err(why) => {
                self.notice(why.to_string());
                cx.notify();
            }
            Ok(Pick::Show(name)) => {
                self.menu = None;
                self.show(&name, cx);
            }
            Ok(Pick::Action(id)) => {
                self.menu = None;
                self.dispatch(&ActionId(id.to_string()), None, window, cx);
                cx.notify();
            }
        }
    }

    /// Change-only: gpui fires this on every pointer move over a row.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.menu.as_mut().is_some_and(|(_, m)| m.highlight(index)) {
            cx.notify();
        }
    }
}

impl Render for ClassificationsTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.0;
        let tile = cx.entity();
        let ids = MenuIds::new("classifications-menu", "classifications-menu-row");
        // Each menu's outside press closes only that menu: a press on the
        // other menu's trigger has already swapped it in (the triggers act
        // in the capture phase), and must not see it closed again.
        let close = |kind: MenuKind| {
            move |t: &mut ClassificationsTile, _: &mut Window, cx: &mut Context<Self>| {
                if t.menu.as_ref().is_some_and(|(k, _)| *k == kind) {
                    t.close_menu(cx);
                }
            }
        };
        let switcher = self
            .menu
            .as_ref()
            .filter(|(k, _)| *k == MenuKind::Switch)
            .map(|(_, m)| {
                div()
                    .debug_selector(|| "classifications-switcher".into())
                    .child(menu::render_menu(
                        m,
                        &ids,
                        gpui::Anchor::TopLeft,
                        &tile,
                        close(MenuKind::Switch),
                        cx,
                    ))
                    .into_any_element()
            });
        let actions = self
            .menu
            .as_ref()
            .filter(|(k, _)| *k == MenuKind::Actions)
            .map(|(_, m)| {
                menu::render_menu(
                    m,
                    &ids,
                    gpui::Anchor::TopRight,
                    &tile,
                    close(MenuKind::Actions),
                    cx,
                )
            });
        let theme = cx.theme();
        let header = header::render(
            &self.chrome.header,
            header::HeaderChrome {
                tile: &tile,
                tile_id: self.id,
                stack: self.stack.as_ref(),
                close: self.close.as_ref(),
                mode: Mode::from_key_mode(self.mode()),
                links: link_chips(&self.frame, cx),
                notices: self.chrome.notices.clone(),
                actions_open: actions.is_some(),
                switcher,
                menu_selector: self.menu_selector.clone(),
                menu_tip: self.menu_tip.clone(),
                switch_tip: self.switch_tip.clone(),
            },
            theme,
        );
        let body = match &self.chrome.empty {
            Some(text) => v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .px_4()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .debug_selector(move || format!("classifications-empty-{id}"))
                        .child(text.clone()),
                ),
            None => v_flex().flex_1().min_h_0(),
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(header)
                    .when_some(actions, |el, m| {
                        el.child(
                            div()
                                .absolute()
                                .right_0()
                                .top(scale::design(HEADER_HEIGHT))
                                .child(m),
                        )
                    }),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests;
