//! Shell integration: [`WatchlistFactory`] builds tiles and holds the
//! configuration snapshot the app pushes on startup and on every reload,
//! and the refresh hook the bridge installs; [`WatchlistContent`] is each
//! tile's door for keys, commands and deliveries.

use std::cell::RefCell;
use std::rc::Rc;

use geode_core::context::DimensionContext;
use geode_core::dimensions::DerivedDimensions;
use geode_core::named::NamedExpressions;
use geode_core::schema::SchemaSpec;
use geode_core::scopes::SavedScopes;
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
use crate::tile::WatchlistTile;

/// Registered actions and their palette titles. The palette lists every
/// one; [`DEFAULT_KEYMAP`] binds all but the configuration-level verbs
/// (new, clone, rename, delete, revert), which are reached from the
/// palette and the `⋯` menu; Rules… is `r`.
pub const ACTIONS: &[(&str, &str)] = &[
    ("watchlist::add", "Watchlist: Add name"),
    ("watchlist::remove", "Watchlist: Remove name"),
    ("watchlist::undo", "Watchlist: Undo"),
    ("watchlist::redo", "Watchlist: Redo"),
    ("watchlist::refresh", "Watchlist: Resolve now"),
    ("watchlist::rules", "Watchlist: Rules\u{2026}"),
    ("watchlist::rule_add", "Watchlist: Add rule"),
    ("watchlist::rule_remove", "Watchlist: Remove rule"),
    ("watchlist::rule_edit", "Watchlist: Edit rule"),
    ("watchlist::switch", "Watchlist: Switch"),
    ("watchlist::menu", "Watchlist: Menu"),
    // The popup and field verbs: registered beside the rest so the
    // fragment binds them and the palette lists them.
    ("watchlist::commit", "Watchlist: Commit field"),
    ("watchlist::cancel", "Watchlist: Cancel"),
    ("watchlist::choice_up", "Watchlist: Choice up"),
    ("watchlist::choice_down", "Watchlist: Choice down"),
    ("watchlist::visual_rows", "Watchlist: Select rows"),
    ("watchlist::new", "Watchlist: New\u{2026}"),
    ("watchlist::clone", "Watchlist: Clone\u{2026}"),
    ("watchlist::rename", "Watchlist: Rename\u{2026}"),
    ("watchlist::delete", "Watchlist: Delete\u{2026}"),
    ("watchlist::revert", "Watchlist: Revert\u{2026}"),
];

/// The palette title of a registered action.
pub(crate) fn action_title(id: &str) -> &'static str {
    ACTIONS
        .iter()
        .find(|(a, _)| *a == id)
        .map(|(_, t)| *t)
        .unwrap_or("")
}

/// Bindings scoped to the factory's `watchlist` context. Fragment
/// validation requires each predicate to be a conjunction beginning with
/// that context. `g` alone is unbound: an exact match dispatches at once,
/// so it would make `g w` unreachable.
///
/// The visual block comes before the normal one: a menu or tooltip hint
/// names an action's last live binding, and the menus open from normal
/// mode.
///
/// While a field is open the tile is in `insert` mode: `up` and `down`
/// move the typeahead's highlight, and no bare letter is bound, so typing
/// reaches the field.
///
/// While a menu is up the tile publishes `tilelist`, so the shell's shared
/// `j`/`k` and arrows step its rows; only picking and closing are here.
/// The rules popup (`rules` mode) steps the same way and binds its own
/// verbs.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "watchlist && mode == visual"
[bindings.keys]
"x" = "watchlist::remove"
"escape" = "watchlist::cancel"

[[bindings]]
context = "watchlist && mode == normal"
[bindings.keys]
"o" = "watchlist::add"
"enter" = "watchlist::add"
"x" = "watchlist::remove"
"u" = "watchlist::undo"
"ctrl+r" = "watchlist::redo"
"shift+r" = "watchlist::refresh"
"r" = "watchlist::rules"
"." = "watchlist::menu"
"g w" = "watchlist::switch"
"shift+v" = "watchlist::visual_rows"
"v" = "watchlist::visual_rows"
# Nothing to cancel in normal mode but the header's notices: `escape`
# dismisses the warning and danger ones, and does nothing otherwise.
"escape" = "watchlist::cancel"

[[bindings]]
context = "watchlist && mode == insert"
[bindings.keys]
"enter" = "watchlist::commit"
"escape" = "watchlist::cancel"
"up" = "watchlist::choice_up"
"down" = "watchlist::choice_down"

[[bindings]]
context = "watchlist && mode == menu"
[bindings.keys]
"enter" = "watchlist::commit"
"escape" = "watchlist::cancel"
"." = "watchlist::menu"

[[bindings]]
context = "watchlist && mode == rules"
[bindings.keys]
"o" = "watchlist::rule_add"
"x" = "watchlist::rule_remove"
"enter" = "watchlist::rule_edit"
"escape" = "watchlist::cancel"
"r" = "watchlist::rules"
"#;

/// What the tile reads out of the configuration, pushed whole by the app on
/// startup and on every reload so a tile never reads a half-updated one.
/// The lists themselves come from `WatchlistGlobal`; this is what a rule is
/// validated against.
#[derive(Debug, Clone, Default)]
pub struct WatchlistConfig {
    /// The schema a rule's dataset and scope are checked against.
    pub schema: Rc<SchemaSpec>,
    pub dims: DerivedDimensions,
    /// The saved scopes a rule may name.
    pub saved: SavedScopes,
    /// The named expressions a rule's expression may use.
    pub named: NamedExpressions,
}

/// The bridge's cache refresh, by list name: the tile's `shift+r` route.
pub type RefreshHook = Rc<dyn Fn(&str, &mut App)>;

/// What every tile of the factory shares: the latest snapshot, the live
/// tiles it is pushed to, and the refresh hook.
#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) config: RefCell<Option<WatchlistConfig>>,
    tiles: RefCell<Vec<WeakEntity<WatchlistTile>>>,
    /// Installed by the bridge after the factory is built; `None` for a
    /// tile hosted without one, whose refresh then says so.
    pub(crate) refresh: RefCell<Option<RefreshHook>>,
}

pub struct WatchlistContent {
    tile: Entity<WatchlistTile>,
}

impl TileContent for WatchlistContent {
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

    /// `:` commands (`:sort`) go to the tile. The configuration-level verbs are registered
    /// actions, so they never act from another tile's line.
    fn command(&self, line: &str, _window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, cx))
    }

    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor)
    }

    fn find(&self, event: FindEvent, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, cx))
    }

    /// This tile issues no request: its lists arrive through
    /// `WatchlistGlobal`, resolved by the bridge's cache. No answer is this
    /// tile's; one that arrived would answer nothing it asked.
    fn deliver(&self, delivery: Delivery, _window: &mut Window, _cx: &mut App) {
        match delivery {
            Delivery::Query(_) => {}
            Delivery::Price(_) => {}
            Delivery::VolSlices(_) => {}
            Delivery::Series(_) => {}
            Delivery::SeriesFetched { .. } => {}
            Delivery::Upload(_) => {}
            Delivery::Distinct(_) => {}
            Delivery::TextFile(_) => {}
        }
    }

    fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.tile.read(cx).holds_focus(window, cx)
    }

    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, _| t.set_visible(visible))
    }

    fn set_focused(&self, focused: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_focused(focused, cx))
    }

    /// A list reads no scope: its members are the definition's, whatever
    /// the frame selects.
    fn follows(&self) -> bool {
        false
    }

    /// The tile can always emit: the answer is the kind's capability, not
    /// whether a list is shown or a row is under the cursor. The shell
    /// drops a restored membership for a tile that answers `false` right
    /// after create, before the first snapshot can have arrived.
    fn emits(&self) -> bool {
        true
    }

    /// The cursor row's name as a one-value `underlying_ref` path, or
    /// nothing while no row is under the cursor.
    fn emission(&self, cx: &App) -> geode_core::link::Emission {
        self.tile.read(cx).emission(cx)
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

    fn closed(&self, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.closed(cx))
    }

    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_stack(stack, cx))
    }

    fn set_close(&self, close: CloseHandle, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_close(close, cx))
    }

    fn title(&self, cx: &App) -> SharedString {
        self.tile.read(cx).title()
    }

    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize()
    }
}

/// Creates watchlist tiles sharing the pushed configuration and the
/// bridge's refresh hook. No data handle: the tile issues no request.
#[derive(Default)]
pub struct WatchlistFactory {
    shared: Rc<Shared>,
}

impl WatchlistFactory {
    pub fn new() -> WatchlistFactory {
        WatchlistFactory::default()
    }

    /// The snapshot last pushed; `None` before the first.
    pub fn config(&self) -> Option<WatchlistConfig> {
        self.shared.config.borrow().clone()
    }

    /// Store `config` and tell every live tile. A tile closed since its
    /// last push is pruned here. No borrow is held across a tile's update:
    /// the tile reads the snapshot just stored.
    pub fn set_config(&self, config: WatchlistConfig, cx: &mut App) {
        *self.shared.config.borrow_mut() = Some(config);
        let live: Vec<Entity<WatchlistTile>> = {
            let mut tiles = self.shared.tiles.borrow_mut();
            tiles.retain(|t| t.upgrade().is_some());
            tiles.iter().filter_map(WeakEntity::upgrade).collect()
        };
        for tile in live {
            tile.update(cx, |t, cx| t.config_changed(cx));
        }
    }

    /// Install the bridge's cache refresh: `watchlist::refresh` on a tile
    /// calls it with the shown list's name.
    pub fn set_refresh(&self, hook: RefreshHook) {
        *self.shared.refresh.borrow_mut() = Some(hook);
    }
}

impl ModuleFactory for WatchlistFactory {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Watchlist".to_string(),
            });
        }
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }

    /// A watchlist is chosen inside the tile, never launched from a row's
    /// dimension.
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
        _diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        let shared = self.shared.clone();
        let entity = cx.new(|cx| WatchlistTile::new(tile, restored, frame, shared, window, cx));
        self.shared.tiles.borrow_mut().push(entity.downgrade());
        TileOccupant {
            kind: KIND,
            view: entity.clone().into(),
            content: Box::new(WatchlistContent { tile: entity }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fragment passes the shell's validator as the roster runs it, and
    /// every binding names an action this module registers: `build_keymap`
    /// drops a binding to an unregistered id.
    #[test]
    fn the_default_keymap_is_a_valid_fragment() {
        use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
        let doc = fragment_doc(crate::KIND, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &[crate::KIND]);
        assert!(diags.is_empty(), "{diags:?}");
        let ids: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        let bindings = doc.table["bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 5, "visual, normal, insert, menu and rules");
        let mut bound = std::collections::BTreeSet::new();
        let mut count = 0;
        for b in bindings {
            for (key, action) in b["keys"].as_table().unwrap() {
                let action = action.as_str().unwrap();
                assert!(ids.contains(&action), "{key} = {action} is not registered");
                bound.insert(action);
                count += 1;
            }
        }
        assert_eq!(count, 26);
        assert_eq!(bound.get("watchlist::switch"), Some(&"watchlist::switch"));
        // The config-level verbs are reached from the palette and the
        // menu; Rules… has `r`.
        for unbound in ["new", "clone", "rename", "delete", "revert"] {
            assert!(
                !bound.contains(format!("watchlist::{unbound}").as_str()),
                "{unbound} has no default chord"
            );
        }
        // And the registry the app builds accepts every binding.
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        WatchlistFactory::new().register_actions(&mut registry);
        let (_, diags) = geode_shell::keymap::build_keymap(
            &[doc],
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn every_action_has_a_title() {
        for (id, title) in ACTIONS {
            assert!(!title.is_empty(), "{id}");
            assert_eq!(action_title(id), *title);
        }
    }

    /// The palette lists every action across every tile kind: a bare
    /// "Commit" or "Cancel" there would not say whose field it acts on.
    #[test]
    fn the_field_verbs_name_their_tile_in_the_palette() {
        assert_eq!(action_title("watchlist::commit"), "Watchlist: Commit field");
        assert_eq!(action_title("watchlist::cancel"), "Watchlist: Cancel");
        assert_eq!(action_title("watchlist::choice_up"), "Watchlist: Choice up");
        assert_eq!(
            action_title("watchlist::choice_down"),
            "Watchlist: Choice down"
        );
    }
}
