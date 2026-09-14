//! What the shell hosts (Phase 3 spec §3.1, §3.2): the `TileContent`
//! wrapper over a `BlotterTile` entity, and the factory the app puts in
//! the roster. The factory carries the data handle (§2.1); the shell
//! never sees it.

use crate::tile::{ACTIONS, BlotterTile};
use geode_core::colour::NamedColours;
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;
use geode_core::view::ViewSpec;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{Delivery, FindEvent, ModuleFactory, TileContent, TileOccupant};
use geode_shell::tiling::TileId;
use geode_shell::vimfind::FindStyle;
use gpui::prelude::*;
use gpui::{App, Entity, Window};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
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
    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
        }
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
    /// `colours.toml`'s definitions (Part 2c §6.2), shared with every
    /// tile exactly as `views` is and refreshed by the same
    /// `ConfigReloaded` handler in the app's bridge. An `Arc` inside the
    /// cell rather than the value itself: a tile hands it straight to
    /// its delegate, where a pointer compare is what tells "same
    /// definitions" from "reloaded" without a deep compare per snapshot.
    colours: Rc<RefCell<Arc<NamedColours>>>,
    /// The schema and derived dimensions `:filter`/`:scope` validate
    /// against (Phase 4a §3.7) — set alongside `views` and refreshed the
    /// same way on `ConfigReloaded`.
    schema: Rc<RefCell<SchemaSpec>>,
    dims: Rc<RefCell<DerivedDimensions>>,
    find_style: Rc<Cell<FindStyle>>,
    stale_after: Rc<Cell<Duration>>,
}

impl BlotterFactory {
    pub fn new(
        data: DataHandle,
        views: Vec<ViewSpec>,
        colours: NamedColours,
        schema: SchemaSpec,
        dims: DerivedDimensions,
        find_style: FindStyle,
        stale_after: Duration,
    ) -> BlotterFactory {
        BlotterFactory {
            data,
            views: Rc::new(RefCell::new(views)),
            colours: Rc::new(RefCell::new(Arc::new(colours))),
            schema: Rc::new(RefCell::new(schema)),
            dims: Rc::new(RefCell::new(dims)),
            find_style: Rc::new(Cell::new(find_style)),
            stale_after: Rc::new(Cell::new(stale_after)),
        }
    }

    /// A safe reload (foundation §8): every tile sees the new set on its
    /// next requery, which the frame's config counter triggers.
    pub fn set_views(&self, views: Vec<ViewSpec>) {
        *self.views.borrow_mut() = views;
    }

    /// A reloaded `colours` doc (Part 2c §6.2), same sharing as
    /// `set_views` — every tile picks the new definitions up on its next
    /// applied snapshot, which the reload's own config bump already
    /// triggers. A fresh `Arc` every time, deliberately: that is the
    /// pointer change a delegate reads as "these are new definitions,
    /// drop what you resolved from the old ones".
    pub fn set_colours(&self, colours: NamedColours) {
        *self.colours.borrow_mut() = Arc::new(colours);
    }

    /// What the factory is currently handing new tiles — the app's
    /// bridge tests read it to prove a reload actually landed.
    pub fn colours(&self) -> Arc<NamedColours> {
        Arc::clone(&self.colours.borrow())
    }

    /// A reloaded `datasets` doc (Phase 4a §3.7): every open tile's next
    /// `:filter`/`:scope` validates against the new schema, same sharing
    /// as `set_views`.
    pub fn set_schema(&self, schema: SchemaSpec) {
        *self.schema.borrow_mut() = schema;
    }

    /// A reloaded `dimensions` doc, same sharing as `set_schema`.
    pub fn set_dims(&self, dims: DerivedDimensions) {
        *self.dims.borrow_mut() = dims;
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
        _diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        let entity = cx.new(|cx| {
            BlotterTile::new(
                tile,
                frame,
                self.data.clone(),
                self.views.clone(),
                self.colours.clone(),
                self.schema.clone(),
                self.dims.clone(),
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
    use geode_core::log::LogLevels;
    use geode_core::scopes::SavedScopes;

    /// One tile per open window, its own `VisualTestContext`.
    fn open_tile(
        factory: &BlotterFactory,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<BlotterTile>, gpui::VisualTestContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let occupant = factory.create(TileId(1), None, frame, diagnostics, window, cx);
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
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
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
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
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
