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
use std::time::Duration;

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
    stale_after: Rc<Cell<Duration>>,
}

impl BlotterFactory {
    pub fn new(
        data: DataHandle,
        views: Vec<ViewSpec>,
        find_style: FindStyle,
        stale_after: Duration,
    ) -> BlotterFactory {
        BlotterFactory {
            data,
            views: Rc::new(RefCell::new(views)),
            find_style: Rc::new(Cell::new(find_style)),
            stale_after: Rc::new(Cell::new(stale_after)),
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

    /// `[app] blotter.stale_after` (spec §6.5), read by the app in Task
    /// 8 and applied here; every existing tile picks it up immediately
    /// since they all share this `Rc<Cell<_>>`, same as `find_style`.
    pub fn set_stale_after(&self, d: Duration) {
        self.stale_after.set(d);
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
                self.stale_after.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::groupings::GroupingSlots;

    /// One tile per open window, its own `VisualTestContext`.
    fn open_tile(
        factory: &BlotterFactory,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<BlotterTile>, gpui::VisualTestContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame = cx.new(|_| Frame::new(GroupingSlots::default(), None));
                    let occupant = factory.create(TileId(1), None, frame, window, cx);
                    occupant.view.downcast::<BlotterTile>().unwrap()
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let tile = window.root(&mut vcx).unwrap();
        (tile, vcx)
    }

    /// review round 1, Finding 2: `render` used to hardcode 15 minutes;
    /// this proves the factory's own configured `stale_after` is what a
    /// tile it creates actually uses, at a short (1s) threshold and at
    /// the spec default (15m) — the very same freshness reading is
    /// stale under one and not the other.
    #[gpui::test]
    fn a_tiles_stale_threshold_is_the_factorys_configured_value(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(crate::init);

        let (short_data, _short_rx) = DataHandle::for_tests();
        let short = BlotterFactory::new(
            short_data,
            Vec::new(),
            FindStyle::Vim,
            Duration::from_secs(1),
        );
        let (short_tile, short_cx) = open_tile(&short, cx);
        assert_eq!(
            short_tile.read_with(&short_cx, |t, _| t.stale_after.get()),
            Duration::from_secs(1)
        );

        let (default_data, _default_rx) = DataHandle::for_tests();
        let default = BlotterFactory::new(
            default_data,
            Vec::new(),
            FindStyle::Vim,
            crate::tile::DEFAULT_STALE_AFTER,
        );
        let (default_tile, default_cx) = open_tile(&default, cx);
        assert_eq!(
            default_tile.read_with(&default_cx, |t, _| t.stale_after.get()),
            crate::tile::DEFAULT_STALE_AFTER
        );

        // The same reading — 5 seconds old — is stale under the 1s
        // factory's tile and not under the default (15m) factory's.
        let now = chrono::Utc::now();
        let as_of = (now - chrono::Duration::seconds(5)).to_rfc3339();
        assert!(short_tile.read_with(&short_cx, |t, _| t.is_stale(Some(&as_of), now)));
        assert!(!default_tile.read_with(&default_cx, |t, _| t.is_stale(Some(&as_of), now)));
    }
}
