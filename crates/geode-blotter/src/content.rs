//! What the shell hosts (Phase 3 spec §3.1, §3.2): the `TileContent`
//! wrapper over a `BlotterTile` entity, and the factory the app puts in
//! the roster. The factory carries the data handle (§2.1); the shell
//! never sees it.

use crate::tile::{ACTIONS, BlotterTile};
use geode_core::query::QueryOutcome;
use geode_core::view::ViewSpec;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, ModuleFactory, TileContent, TileOccupant};
use geode_shell::tiling::TileId;
use geode_shell::vimfind::FindStyle;
use gpui::prelude::*;
use gpui::{App, Entity, Window};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct BlotterContent {
    tile: Entity<BlotterTile>,
}

impl TileContent for BlotterContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context(cx)
    }
    fn dispatch(
        &self,
        action: &ActionId,
        count: Option<u32>,
        _window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.tile.update(cx, |t, cx| t.dispatch(action, count, cx))
    }
    fn command(&self, line: &str, _window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, cx))
    }
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor, cx)
    }
    fn find(&self, event: FindEvent, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, cx))
    }
    fn deliver(&self, outcome: QueryOutcome, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.deliver(outcome, cx))
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
    }
    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize()
    }
}

pub struct BlotterFactory {
    data: DataHandle,
    views: Rc<RefCell<Vec<ViewSpec>>>,
    find_style: Rc<Cell<FindStyle>>,
}

impl BlotterFactory {
    pub fn new(data: DataHandle, views: Vec<ViewSpec>, find_style: FindStyle) -> BlotterFactory {
        BlotterFactory {
            data,
            views: Rc::new(RefCell::new(views)),
            find_style: Rc::new(Cell::new(find_style)),
        }
    }

    /// A safe reload (foundation §8): every tile sees the new set on its
    /// next requery, which the frame's config counter triggers.
    pub fn set_views(&self, views: Vec<ViewSpec>) {
        *self.views.borrow_mut() = views;
    }

    pub fn set_find_style(&self, style: FindStyle) {
        self.find_style.set(style);
    }
}

impl ModuleFactory for BlotterFactory {
    fn kind(&self) -> &'static str {
        "blotter"
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Blotter".to_string(),
            });
        }
    }

    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        let entity = cx.new(|cx| {
            BlotterTile::new(
                tile,
                frame,
                self.data.clone(),
                self.views.clone(),
                self.find_style.clone(),
                restored,
                window,
                cx,
            )
        });
        TileOccupant {
            kind: "blotter",
            view: entity.clone().into(),
            content: Box::new(BlotterContent { tile: entity }),
        }
    }
}
