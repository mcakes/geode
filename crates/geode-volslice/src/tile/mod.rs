//! The shell-hosted slice viewer entity. It holds the tile's frame handle,
//! its data handle and the catalog it picks underlyings from, answers the
//! shell's door (`crate::content::VolsliceContent`) and paints the tile.
//! With no underlying it paints only its empty state.

use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::StackHandle;
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Context, Entity, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::content::ACTIONS;

/// What the tile paints while it reads no underlying.
const EMPTY: &str = "no underlying";
const TITLE: &str = "vol slice";

pub struct VolsliceTile {
    id: TileId,
    // The data path reads these three: the frame for the link group and
    // the board, the data handle for documents and vol batches, the
    // diagnostics catalog for the underlying picker.
    #[allow(dead_code)]
    frame: FrameRef,
    #[allow(dead_code)]
    data: DataHandle,
    #[allow(dead_code)]
    diagnostics: Entity<Diagnostics>,
    stack: Option<StackHandle>,
    /// Every action id the dispatch door received, so a test proves a key
    /// reached the tile through the real keymap rather than calling a verb.
    #[cfg(test)]
    pub(crate) dispatch_log: Vec<ActionId>,
}

impl VolsliceTile {
    pub fn new(
        id: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        data: DataHandle,
        diagnostics: Entity<Diagnostics>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> VolsliceTile {
        let _ = restored;
        VolsliceTile {
            id,
            frame,
            data,
            diagnostics,
            stack: None,
            #[cfg(test)]
            dispatch_log: Vec::new(),
        }
    }

    /// No `.counts()`: the bare digits are kind toggles, and a counting
    /// context would make the matcher swallow them as a pending count.
    pub fn key_context(&self) -> KeyContext {
        KeyContext::new(crate::KIND).pair("mode", "normal")
    }

    /// `true` for this module's own registered actions, which the tile
    /// owns whatever state it is in; anything else falls through to the
    /// shell.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let _ = (count, window, cx);
        if !ACTIONS.iter().any(|(id, _)| *id == action.0) {
            return false;
        }
        #[cfg(test)]
        self.dispatch_log.push(action.clone());
        true
    }

    pub fn command(
        &mut self,
        line: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Result<(), String> {
        Err(format!("unknown command: {}", line.trim()))
    }

    pub fn completions(&self, _line: &str, _cursor: usize) -> Vec<String> {
        Vec::new()
    }

    /// The tile asks nothing yet, so showing or hiding it changes nothing.
    pub fn set_visible(&mut self, _visible: bool, _cx: &mut Context<Self>) {}

    /// Nothing is in flight to cancel and no barrier waits on this tile.
    pub fn closed(&mut self, _cx: &mut Context<Self>) {}

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        SharedString::new_static(TITLE)
    }

    /// Nothing the tile holds survives a restart yet.
    pub fn serialize(&self) -> toml::Table {
        toml::Table::new()
    }

    pub fn holds_focus(&self, _window: &Window, _cx: &App) -> bool {
        false
    }

    #[cfg(test)]
    pub(crate) fn empty_text(&self) -> SharedString {
        SharedString::new_static(EMPTY)
    }
}

impl Render for VolsliceTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.0;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .debug_selector(move || format!("volslice-empty-{id}"))
                    .child(EMPTY),
            )
    }
}

#[cfg(test)]
mod tests;
