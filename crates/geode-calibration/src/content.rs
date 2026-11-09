//! Shell integration: [`CalibrationFactory`] builds tiles over the app's
//! data handle and holds the configuration the app pushes, and
//! [`CalibrationContent`] is each tile's door for keys, commands and
//! deliveries.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use geode_core::calibration::Pipelines;
use geode_core::config::Diagnostic;
use geode_core::context::DimensionContext;
use geode_core::schema::SchemaSpec;
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
use crate::tile::{CalibrationTile, TITLE};

/// Registered actions and their palette titles. Every one is bound in
/// [`DEFAULT_KEYMAP`]; the palette lists them all. The ids are fixed here
/// so a binding, a menu row and a footer hint can name a verb before it
/// does anything: a verb the tile does not yet carry out consumes its key
/// and changes nothing.
pub const ACTIONS: &[(&str, &str)] = &[
    ("calibration::cursor_left", "Cursor to an input"),
    ("calibration::cursor_right", "Cursor to a consumer"),
    ("calibration::cursor_up", "Cursor up"),
    ("calibration::cursor_down", "Cursor down"),
    ("calibration::jump_1", "Cursor to stage 1"),
    ("calibration::jump_2", "Cursor to stage 2"),
    ("calibration::jump_3", "Cursor to stage 3"),
    ("calibration::jump_4", "Cursor to stage 4"),
    ("calibration::jump_5", "Cursor to stage 5"),
    ("calibration::jump_6", "Cursor to stage 6"),
    ("calibration::jump_7", "Cursor to stage 7"),
    ("calibration::jump_8", "Cursor to stage 8"),
    ("calibration::jump_9", "Cursor to stage 9"),
    ("calibration::filter", "Filter stages"),
    ("calibration::commit", "Confirm"),
    ("calibration::cancel", "Cancel"),
    ("calibration::relayout", "Relayout graph"),
    ("calibration::run_from", "Run from here"),
    ("calibration::run_all", "Run pipeline"),
    ("calibration::publish", "Publish"),
    ("calibration::menu", "Actions menu"),
    ("calibration::menu_pick", "Pick menu row"),
    // The picker fields' row steps. A field types every bare key, so the
    // shared `j`/`k` list steps cannot reach it; the arrows can.
    ("calibration::list_down", "Next row"),
    ("calibration::list_up", "Previous row"),
    ("calibration::connect", "Connect an input"),
    ("calibration::disconnect", "Disconnect an input"),
    ("calibration::add", "Add a stage"),
    ("calibration::delete", "Delete stage"),
];

/// Bindings scoped to the factory's `calibration` context. Fragment
/// validation requires each predicate to be a conjunction beginning with
/// that context. Within the tile these bindings take precedence over
/// workspace bindings, so `/` filters stages here rather than opening the
/// shell's find.
///
/// The bare digits are stage jumps. They reach these bindings only because
/// the tile's key context does not opt into counts
/// (`CalibrationTile::key_context`); a counting context would read a digit
/// as a pending count and the jump would never fire.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "calibration && mode == normal"
[bindings.keys]
# The cursor walks the graph's edges: left to an input, right to a
# consumer, up and down between the stages of one depth.
"h" = "calibration::cursor_left"
"left" = "calibration::cursor_left"
"l" = "calibration::cursor_right"
"right" = "calibration::cursor_right"
"k" = "calibration::cursor_up"
"up" = "calibration::cursor_up"
"j" = "calibration::cursor_down"
"down" = "calibration::cursor_down"
"1" = "calibration::jump_1"
"2" = "calibration::jump_2"
"3" = "calibration::jump_3"
"4" = "calibration::jump_4"
"5" = "calibration::jump_5"
"6" = "calibration::jump_6"
"7" = "calibration::jump_7"
"8" = "calibration::jump_8"
"9" = "calibration::jump_9"
"/" = "calibration::filter"
"shift+l" = "calibration::relayout"
"r" = "calibration::run_from"
"shift+r" = "calibration::run_all"
"shift+p" = "calibration::publish"
"c" = "calibration::connect"
"shift+c" = "calibration::disconnect"
"a" = "calibration::add"
"d" = "calibration::delete"
"." = "calibration::menu"
# Last in line: with nothing else to cancel, `escape` dismisses the
# header's warning and danger notices.
"escape" = "calibration::cancel"

# The filter and the picker fields: a field, so only these keys are
# claimed and every other bare key types.
[[bindings]]
context = "calibration && mode == insert"
[bindings.keys]
"enter" = "calibration::commit"
"escape" = "calibration::cancel"
"down" = "calibration::list_down"
"up" = "calibration::list_up"

# The action menu: a fieldless list. The tile publishes `tilelist` while
# it is up, so the shell's shared `j`/`k` and arrows step its rows. `.`
# is its own toggle, as `⋯` is.
[[bindings]]
context = "calibration && mode == menu && popup == actions"
[bindings.keys]
"enter" = "calibration::menu_pick"
"escape" = "calibration::cancel"
"." = "calibration::menu"

# A fieldless chooser (a stage to connect, a kind to add): the shared
# steps move it, `enter` applies and `escape` discards.
[[bindings]]
context = "calibration && mode == menu && popup == list"
[bindings.keys]
"enter" = "calibration::commit"
"escape" = "calibration::cancel"
"#;

/// What the tile reads out of the configuration, pushed whole by the app on
/// startup and on every reload so a tile never reads a half-updated one.
#[derive(Clone)]
pub struct CalibrationConfig {
    /// The pipelines `from_doc` accepted and `check_against` kept.
    pub pipelines: Arc<Pipelines>,
    /// The schema the service serves: decodes the source documents and
    /// names the datasets a source stage reads.
    pub schema: Arc<SchemaSpec>,
    /// Target name → the documents it accepts, in `egress.toml` order, as
    /// the market-data factories hold it.
    pub egress: Arc<Vec<(String, Vec<String>)>>,
    /// Every `from_doc` and `check_against` finding, for the header
    /// notice: a dropped pipeline is said in the tile, not only in the log.
    pub diagnostics: Arc<Vec<Diagnostic>>,
}

/// What every tile of the factory shares: the data handle, the config last
/// pushed and the live tiles it is pushed to.
pub(crate) struct Shared {
    /// The tile's door to the data tier: document reads, runs and
    /// publishes go through it.
    pub(crate) data: DataHandle,
    pub(crate) config: RefCell<Option<CalibrationConfig>>,
    tiles: RefCell<Vec<WeakEntity<CalibrationTile>>>,
}

pub struct CalibrationContent {
    tile: Entity<CalibrationTile>,
}

impl TileContent for CalibrationContent {
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

    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, window, cx))
    }

    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor)
    }

    /// The tile has no rows to search; `/` is its own stage filter.
    fn find(&self, _event: FindEvent, _window: &mut Window, _cx: &mut App) {}

    fn deliver(&self, delivery: Delivery, window: &mut Window, cx: &mut App) {
        let _ = window;
        match delivery {
            // The source documents' snapshots, a run's answer and a
            // publish's echo.
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver_query(outcome, cx)),
            Delivery::Calibration(outcome) => self
                .tile
                .update(cx, |t, cx| t.deliver_calibration(outcome, cx)),
            Delivery::Upload(outcome) => {
                self.tile.update(cx, |t, cx| t.deliver_upload(outcome, cx))
            }
            // This tile prices nothing, asks no series, no vol and no
            // board, so none of these is ever addressed to it; one that
            // were would answer nothing it asked.
            Delivery::Price(_) => {}
            Delivery::VolSlices(_) => {}
            Delivery::Series(_) => {}
            Delivery::SeriesFetched { .. } => {}
            Delivery::VolBoard(_) => {}
            Delivery::HistoryKeys(_) => {}
            // This tile asks for no distinct values and reads no files.
            Delivery::Distinct(_) => {}
            Delivery::TextFile(_) => {}
        }
    }

    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
    }

    /// The tile is being removed: its reads are cancelled and any flip
    /// waiting on it answered.
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

    /// A calibration tile reads its underlying from the group it follows,
    /// so following changes what it shows. The answer is the kind's
    /// capability, not whether an underlying is set: the shell asks right
    /// after create and drops a restored membership on `false`.
    fn follows(&self) -> bool {
        true
    }
}

/// Creates calibration tiles sharing the app's data handle and the config
/// the app pushes.
pub struct CalibrationFactory {
    shared: Rc<Shared>,
}

impl CalibrationFactory {
    pub fn new(data: DataHandle) -> CalibrationFactory {
        CalibrationFactory {
            shared: Rc::new(Shared {
                data,
                config: RefCell::new(None),
                tiles: RefCell::new(Vec::new()),
            }),
        }
    }

    /// The config last pushed; `None` before the first push.
    pub fn config(&self) -> Option<CalibrationConfig> {
        self.shared.config.borrow().clone()
    }

    /// Store `config` and tell every live tile. A tile closed since its
    /// last push is pruned here. No borrow is held across a tile's update:
    /// the tile reads the config just stored.
    pub fn set_config(&self, config: CalibrationConfig, cx: &mut App) {
        *self.shared.config.borrow_mut() = Some(config);
        let live: Vec<Entity<CalibrationTile>> = {
            let mut tiles = self.shared.tiles.borrow_mut();
            tiles.retain(|t| t.upgrade().is_some());
            tiles.iter().filter_map(WeakEntity::upgrade).collect()
        };
        for tile in live {
            tile.update(cx, |t, cx| t.config_changed(cx));
        }
    }
}

impl ModuleFactory for CalibrationFactory {
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

    /// A pipeline runs over one underlying's documents, so the tile opens
    /// on one.
    fn accepts(&self) -> &'static [&'static str] {
        &[geode_core::link::UNDERLYING]
    }

    /// `{ underlying = "<u>" }`: the key the tile restores its underlying
    /// from, so a launched tile starts exactly as a restored one would.
    fn launch_state(&self, ctx: &DimensionContext) -> Option<toml::Table> {
        let u = ctx.get(geode_core::link::UNDERLYING)?.to_string();
        let mut t = toml::Table::new();
        t.insert("underlying".into(), toml::Value::String(u));
        Some(t)
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
        let entity = cx
            .new(|cx| CalibrationTile::new(tile, restored, frame, shared, diagnostics, window, cx));
        self.shared.tiles.borrow_mut().push(entity.downgrade());
        TileOccupant {
            kind: KIND,
            view: entity.clone().into(),
            content: Box::new(CalibrationContent { tile: entity }),
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
    fn the_keymap_fragment_parses_and_names_only_registered_actions() {
        use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
        let doc = fragment_doc(crate::KIND, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &[crate::KIND]);
        assert!(diags.is_empty(), "{diags:?}");
        let ids: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        let bindings = doc.table["bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 4, "normal, insert, action menu and chooser");
        let mut bound = 0;
        for b in bindings {
            for (key, action) in b["keys"].as_table().unwrap() {
                let action = action.as_str().unwrap();
                assert!(ids.contains(&action), "{key} = {action} is not registered");
                bound += 1;
            }
        }
        assert_eq!(bound, 37);
        // Every registered action has a key somewhere in the fragment.
        for id in &ids {
            assert!(
                DEFAULT_KEYMAP.contains(&format!("\"{id}\"")),
                "{id} has no default key"
            );
        }
        // And the registry the app builds accepts every binding.
        let (data, _rx) = DataHandle::for_tests();
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        CalibrationFactory::new(data).register_actions(&mut registry);
        let (_, diags) = geode_shell::keymap::build_keymap(
            &[doc],
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// The launch table a row's underlying opens the tile with is the
    /// session key the tile restores from; a context without one launches
    /// nothing.
    #[test]
    fn the_launch_state_is_the_underlying_session_key() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = CalibrationFactory::new(data);
        assert_eq!(factory.accepts(), &[geode_core::link::UNDERLYING]);
        let table = factory
            .launch_state(&DimensionContext::of(&[(
                geode_core::link::UNDERLYING,
                "SPX",
            )]))
            .expect("an underlying launches");
        assert_eq!(table["underlying"].as_str(), Some("SPX"));
        assert_eq!(factory.launch_state(&DimensionContext::default()), None);
    }
}
