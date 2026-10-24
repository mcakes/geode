//! Shell integration: [`VolwatchFactory`] builds tiles and holds the
//! settings the app pushes on startup and on every reload;
//! [`VolwatchContent`] is each tile's door for keys, commands and
//! deliveries.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use geode_core::context::DimensionContext;
use geode_core::dimensions::DerivedDimensions;
use geode_core::volviews::VolViews;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{
    CloseHandle, Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant,
};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Entity, SharedString, WeakEntity, Window};

use crate::KIND;
use crate::core::views::TITLE;
use crate::tile::VolwatchTile;

/// How often a shown board is asked again when the settings name nothing.
pub const DEFAULT_REFRESH: Duration = Duration::from_secs(60);
/// The shortest refresh interval: a shorter one is raised to this.
pub const MIN_REFRESH: Duration = Duration::from_secs(10);
/// How old a row's surface time may be before it is stale, when the app
/// supplies none: a surface publishes once a day; a row is stale when a
/// day has passed without one.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// What the tile reads from the configuration, pushed whole by the app.
#[derive(Debug, Clone, PartialEq)]
pub struct VolwatchSettings {
    /// The refresh interval; `None` turns the timer off. Below
    /// [`MIN_REFRESH`] it is raised (see [`VolwatchSettings::clamp_refresh`]).
    pub refresh: Option<Duration>,
    /// The view and column book vega is read from; `None`: no vega column.
    pub vega: Option<VegaSetting>,
    pub stale_after: Duration,
    /// The tabs, from `vol_views.toml`; the builtin four when the app
    /// supplies none.
    pub views: VolViews,
}

impl Default for VolwatchSettings {
    fn default() -> Self {
        VolwatchSettings {
            refresh: Some(DEFAULT_REFRESH),
            vega: None,
            stale_after: DEFAULT_STALE_AFTER,
            views: crate::core::views::builtin(),
        }
    }
}

impl VolwatchSettings {
    /// The interval the timer runs at: never below [`MIN_REFRESH`], so a
    /// mistyped setting cannot hammer the data service.
    pub fn clamp_refresh(refresh: Option<Duration>) -> Option<Duration> {
        refresh.map(|d| d.max(MIN_REFRESH))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VegaSetting {
    pub view: String,
    pub column: String,
}

/// Registered actions and their palette titles.
pub const ACTIONS: &[(&str, &str)] = &[
    ("volwatch::view_1", "Vol: View 1"),
    ("volwatch::view_2", "Vol: View 2"),
    ("volwatch::view_3", "Vol: View 3"),
    ("volwatch::view_4", "Vol: View 4"),
    ("volwatch::view_5", "Vol: View 5"),
    ("volwatch::view_6", "Vol: View 6"),
    ("volwatch::view_7", "Vol: View 7"),
    ("volwatch::view_8", "Vol: View 8"),
    ("volwatch::view_9", "Vol: View 9"),
    ("volwatch::view_next", "Vol: Next view"),
    ("volwatch::view_prev", "Vol: Previous view"),
    ("volwatch::lens", "Vol: Toggle moves / rich-cheap"),
    ("volwatch::metric", "Vol: Toggle z-score / percentile"),
    ("volwatch::window", "Vol: Cycle window"),
    ("volwatch::shown", "Vol: Swap level / companion"),
    ("volwatch::lookback", "Vol: Cycle lookback"),
    ("volwatch::paint", "Vol: Toggle paint"),
    ("volwatch::sort", "Vol: Sort by column"),
    ("volwatch::inspect", "Vol: Inspect row"),
    ("volwatch::timeseries", "Vol: Open timeseries"),
    ("volwatch::yank", "Vol: Yank cell"),
    ("volwatch::refresh", "Vol: Refresh now"),
    ("volwatch::switch", "Vol: Switch list"),
    ("volwatch::menu", "Vol: Menu"),
    ("volwatch::toggle", "Vol: Toggle group"),
    ("volwatch::expand", "Vol: Expand group"),
    ("volwatch::collapse", "Vol: Collapse group"),
    ("volwatch::expand_all", "Vol: Expand all groups"),
    ("volwatch::collapse_all", "Vol: Collapse all groups"),
    ("volwatch::visual_rows", "Vol: Select rows"),
    ("volwatch::escape", "Vol: Escape"),
    ("volwatch::commit", "Vol: Commit"),
    ("volwatch::up", "Vol: Up"),
    ("volwatch::down", "Vol: Down"),
    ("volwatch::left", "Vol: Left"),
    ("volwatch::right", "Vol: Right"),
    ("volwatch::top", "Vol: Top"),
    ("volwatch::bottom", "Vol: Bottom"),
];

/// Bindings scoped to the factory's `volwatch` context. The context
/// publishes no `counts` flag: the digits pick the first nine tabs. `g`
/// alone is unbound, so `g g` and `g w` stay reachable. `Space` toggles
/// the cursor's group row and `z` is the fold chord prefix, as in the
/// blotter. While a menu is up the tile publishes `tilelist`, so the
/// shell's shared `j`/`k` step its rows.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "volwatch && mode == normal"
[bindings.keys]
"1" = "volwatch::view_1"
"2" = "volwatch::view_2"
"3" = "volwatch::view_3"
"4" = "volwatch::view_4"
"5" = "volwatch::view_5"
"6" = "volwatch::view_6"
"7" = "volwatch::view_7"
"8" = "volwatch::view_8"
"9" = "volwatch::view_9"
"tab" = "volwatch::view_next"
"shift+tab" = "volwatch::view_prev"
"r" = "volwatch::lens"
"q" = "volwatch::metric"
"w" = "volwatch::window"
"m" = "volwatch::shown"
"d" = "volwatch::lookback"
"p" = "volwatch::paint"
"s" = "volwatch::sort"
"enter" = "volwatch::inspect"
"t" = "volwatch::timeseries"
"y" = "volwatch::yank"
"shift+r" = "volwatch::refresh"
"g w" = "volwatch::switch"
"." = "volwatch::menu"
"space" = "volwatch::toggle"
"z a" = "volwatch::toggle"
"z o" = "volwatch::expand"
"z c" = "volwatch::collapse"
"z shift+r" = "volwatch::expand_all"
"z shift+m" = "volwatch::collapse_all"
"v" = "volwatch::visual_rows"
"shift+v" = "volwatch::visual_rows"
"j" = "volwatch::down"
"k" = "volwatch::up"
"h" = "volwatch::left"
"l" = "volwatch::right"
"down" = "volwatch::down"
"up" = "volwatch::up"
"left" = "volwatch::left"
"right" = "volwatch::right"
"g g" = "volwatch::top"
"shift+g" = "volwatch::bottom"
"escape" = "volwatch::escape"

[[bindings]]
context = "volwatch && mode == visual"
[bindings.keys]
"j" = "volwatch::down"
"k" = "volwatch::up"
"y" = "volwatch::yank"
"escape" = "volwatch::escape"
"v" = "volwatch::escape"

[[bindings]]
context = "volwatch && mode == menu"
[bindings.keys]
"enter" = "volwatch::commit"
"escape" = "volwatch::escape"
"." = "volwatch::menu"
"#;

/// What every tile of the factory shares: the settings and dimensions
/// last pushed and the live tiles they are pushed to.
pub(crate) struct Shared {
    pub(crate) settings: RefCell<VolwatchSettings>,
    /// The derived dimensions the grouping chain is resolved against:
    /// pushed at startup and on every dimensions reload, as the pricer's.
    pub(crate) dims: RefCell<DerivedDimensions>,
    tiles: RefCell<Vec<WeakEntity<VolwatchTile>>>,
}

pub struct VolwatchContent {
    tile: Entity<VolwatchTile>,
}

impl TileContent for VolwatchContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context()
    }

    fn dispatch(
        &self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.tile
            .update(cx, |t, cx| t.dispatch(action, count, window, cx))
    }

    fn command(&self, line: &str, _window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, cx))
    }

    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor)
    }

    fn find(&self, event: FindEvent, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, cx))
    }

    /// The board and the tenor listing are this tile's, keyed by its id;
    /// a vega query lands with the vega column. Nothing else is asked.
    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::VolBoard(outcome) => {
                self.tile.update(cx, |t, cx| t.deliver_board(outcome, cx))
            }
            Delivery::HistoryKeys(outcome) => {
                self.tile.update(cx, |t, cx| t.deliver_keys(outcome, cx))
            }
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver_vega(outcome, cx)),
            Delivery::Price(_) => {}
            Delivery::VolSlices(_) => {}
            Delivery::Series(_) => {}
            Delivery::SeriesFetched { .. } => {}
            Delivery::Upload(_) => {}
            Delivery::Distinct(_) => {}
            Delivery::TextFile(_) => {}
        }
    }

    /// The tile can always emit: the answer is the kind's capability, not
    /// whether a row is under the cursor.
    fn emits(&self) -> bool {
        true
    }

    /// The cursor row's member as a one-value `underlying_ref` path, or
    /// nothing while no row is under the cursor.
    fn emission(&self, cx: &App) -> geode_core::link::Emission {
        self.tile.read(cx).emission()
    }

    /// Every route that moves the cursor or rebuilds the rows notifies the
    /// tile, so observing it covers each way the emission can change.
    fn watch_emission(
        &self,
        changed: Rc<dyn Fn(&mut App)>,
        cx: &mut App,
    ) -> Option<gpui::Subscription> {
        Some(cx.observe(&self.tile, move |_, cx| changed(cx)))
    }

    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
    }

    fn set_focused(&self, focused: bool, cx: &mut App) {
        self.tile.update(cx, |t, _| t.set_focused(focused))
    }

    fn closed(&self, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.closed(cx))
    }

    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
        self.tile.update(cx, |t, _| t.set_stack(stack))
    }

    fn set_close(&self, close: CloseHandle, cx: &mut App) {
        self.tile.update(cx, |t, _| t.set_close(close))
    }

    fn title(&self, cx: &App) -> SharedString {
        self.tile.read(cx).title()
    }

    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize(cx)
    }

    fn autosize_columns(
        &self,
        reset: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<(), &'static str> {
        self.tile
            .update(cx, |t, cx| t.autosize_columns(reset, window, cx))
    }
}

/// Creates vol watchlist tiles sharing the data handle and the settings.
pub struct VolwatchFactory {
    data: DataHandle,
    shared: Rc<Shared>,
}

impl VolwatchFactory {
    pub fn new(data: DataHandle, settings: VolwatchSettings) -> Self {
        VolwatchFactory {
            data,
            shared: Rc::new(Shared {
                settings: RefCell::new(settings),
                dims: RefCell::new(DerivedDimensions::default()),
                tiles: RefCell::new(Vec::new()),
            }),
        }
    }

    /// The startup dimensions. Call before the first tile is created.
    pub fn with_dims(self, dims: DerivedDimensions) -> Self {
        *self.shared.dims.borrow_mut() = dims;
        self
    }

    /// Replace the dimensions and regroup every live tile; a closed tile is
    /// pruned here. No borrow is held across a tile's update.
    pub fn set_dims(&self, dims: DerivedDimensions, cx: &mut App) {
        *self.shared.dims.borrow_mut() = dims;
        let live: Vec<Entity<VolwatchTile>> = {
            let mut tiles = self.shared.tiles.borrow_mut();
            tiles.retain(|t| t.upgrade().is_some());
            tiles.iter().filter_map(WeakEntity::upgrade).collect()
        };
        for tile in live {
            tile.update(cx, |t, cx| t.dims_changed(cx));
        }
    }

    /// The dimensions the tiles group by.
    pub fn dims(&self) -> DerivedDimensions {
        self.shared.dims.borrow().clone()
    }

    /// Store `settings` and tell every live tile; a closed tile is pruned
    /// here. No borrow is held across a tile's update.
    pub fn set_settings(&self, settings: VolwatchSettings, cx: &mut App) {
        *self.shared.settings.borrow_mut() = settings;
        let live: Vec<Entity<VolwatchTile>> = {
            let mut tiles = self.shared.tiles.borrow_mut();
            tiles.retain(|t| t.upgrade().is_some());
            tiles.iter().filter_map(WeakEntity::upgrade).collect()
        };
        for tile in live {
            tile.update(cx, |t, cx| t.settings_changed(cx));
        }
    }
}

impl ModuleFactory for VolwatchFactory {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn title(&self) -> String {
        TITLE.to_string()
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: TITLE.to_string(),
            });
        }
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }

    /// A list is chosen inside the tile, never launched from a dimension.
    fn accepts(&self) -> &'static [&'static str] {
        &[]
    }

    fn launch_state(&self, _ctx: &DimensionContext) -> Option<toml::Table> {
        None
    }

    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        let shared = self.shared.clone();
        let data = self.data.clone();
        let entity = cx.new(|cx| {
            VolwatchTile::new(tile, restored, frame, data, shared, diagnostics, window, cx)
        });
        self.shared.tiles.borrow_mut().push(entity.downgrade());
        TileOccupant {
            kind: KIND,
            view: entity.clone().into(),
            content: Box::new(VolwatchContent { tile: entity }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fragment passes the shell's validator as the roster runs it,
    /// every binding names a registered action, and the registry the app
    /// builds accepts every binding.
    #[test]
    fn the_default_keymap_is_a_valid_fragment() {
        use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
        let doc = fragment_doc(crate::KIND, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &[crate::KIND]);
        assert!(diags.is_empty(), "{diags:?}");
        let ids: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        let bindings = doc.table["bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 3, "normal, visual and menu");
        let mut count = 0;
        for b in bindings {
            for (key, action) in b["keys"].as_table().unwrap() {
                let action = action.as_str().unwrap();
                assert!(ids.contains(&action), "{key} = {action} is not registered");
                count += 1;
            }
        }
        assert_eq!(count, 43 + 5 + 3);
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        let (data, _rx) = DataHandle::for_tests();
        VolwatchFactory::new(data, VolwatchSettings::default()).register_actions(&mut registry);
        let (_, diags) = geode_shell::keymap::build_keymap(
            &[doc],
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn the_factory_holds_the_dimensions_it_is_given() {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::dimensions::DerivedDimensions;
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin(
                "dimensions",
                "[region]\nfrom = \"underlying_ref\"\n[region.values]\nCore = [\"DAX\"]\n",
            )
            .unwrap()],
        );
        let (dims, _) = DerivedDimensions::from_doc(&doc);
        let (data, _rx) = DataHandle::for_tests();
        let f = VolwatchFactory::new(data, VolwatchSettings::default()).with_dims(dims);
        assert!(f.dims().get("region").is_some());
        assert_eq!(f.dims().get("region").unwrap().from, "underlying_ref");
    }

    #[test]
    fn the_refresh_never_runs_below_ten_seconds() {
        assert_eq!(
            VolwatchSettings::clamp_refresh(Some(Duration::from_secs(3))),
            Some(MIN_REFRESH)
        );
        assert_eq!(
            VolwatchSettings::clamp_refresh(Some(Duration::from_secs(90))),
            Some(Duration::from_secs(90))
        );
        assert_eq!(VolwatchSettings::clamp_refresh(None), None);
    }
}
