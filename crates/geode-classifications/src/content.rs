//! Shell integration: [`ClassificationsFactory`] builds tiles over the
//! app's data handle and holds the configuration snapshot the app pushes on
//! startup and on every reload; [`ClassificationsContent`] is each tile's
//! door for keys, commands and deliveries.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use geode_core::config::Layer;
use geode_core::context::DimensionContext;
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;
use geode_core::view::ViewSpec;
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
use crate::tile::ClassificationsTile;

/// Registered actions and their palette titles. The palette lists every
/// one; [`DEFAULT_KEYMAP`] binds all but the configuration-level verbs
/// (new, rename, delete, revert), which are reached from the palette and
/// the `⋯` menu like the other config-level actions.
pub const ACTIONS: &[(&str, &str)] = &[
    ("classifications::switch", "Classification: Switch\u{2026}"),
    ("classifications::new", "Classification: New\u{2026}"),
    ("classifications::rename", "Classification: Rename\u{2026}"),
    ("classifications::delete", "Classification: Delete"),
    ("classifications::revert", "Classification: Revert to desk"),
    ("classifications::refresh", "Classification: Refresh values"),
    ("classifications::edit", "Classification: Set label"),
    ("classifications::clear", "Classification: Clear label"),
    ("classifications::yank", "Classification: Copy label"),
    ("classifications::paste", "Classification: Paste label"),
    ("classifications::undo", "Classification: Undo"),
    ("classifications::redo", "Classification: Redo"),
    ("classifications::menu", "Classification: Actions"),
    // The popup and field verbs: registered beside the rest so the
    // fragment binds them and the palette lists them.
    ("classifications::commit", "Commit"),
    ("classifications::cancel", "Cancel"),
    ("classifications::choice_up", "Label: previous"),
    ("classifications::choice_down", "Label: next"),
    (
        "classifications::visual_rows",
        "Classification: Select rows",
    ),
];

/// The palette title of a registered action.
pub(crate) fn action_title(id: &str) -> &'static str {
    ACTIONS
        .iter()
        .find(|(a, _)| *a == id)
        .map(|(_, t)| *t)
        .unwrap_or("")
}

/// Bindings scoped to the factory's `classifications` context. Fragment
/// validation requires each predicate to be a conjunction beginning with
/// that context. `y` alone is unbound: an exact match dispatches at once,
/// so it would make `y y` unreachable; `g` alone likewise for `g c`.
///
/// The visual block comes before the normal one: a menu or tooltip hint
/// names an action's last live binding, and the menus open from normal
/// mode.
///
/// While the label editor is open the tile is in `insert` mode: `up` and
/// `down` move the typeahead's highlight, and no bare letter is bound, so
/// typing reaches the field.
///
/// While a menu is up the tile publishes `tilelist`, so the shell's shared
/// `j`/`k` and arrows step its rows; only picking and closing are here.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "classifications && mode == visual"
[bindings.keys]
"enter" = "classifications::edit"
"c" = "classifications::edit"
"x" = "classifications::clear"
"p" = "classifications::paste"
"escape" = "classifications::cancel"

[[bindings]]
context = "classifications && mode == normal"
[bindings.keys]
"enter" = "classifications::edit"
"c" = "classifications::edit"
"x" = "classifications::clear"
"y y" = "classifications::yank"
"p" = "classifications::paste"
"u" = "classifications::undo"
"ctrl+r" = "classifications::redo"
"shift+r" = "classifications::refresh"
"." = "classifications::menu"
"g c" = "classifications::switch"
"shift+v" = "classifications::visual_rows"
"v" = "classifications::visual_rows"

[[bindings]]
context = "classifications && mode == insert"
[bindings.keys]
"enter" = "classifications::commit"
"escape" = "classifications::cancel"
"up" = "classifications::choice_up"
"down" = "classifications::choice_down"

[[bindings]]
context = "classifications && mode == menu"
[bindings.keys]
"enter" = "classifications::commit"
"escape" = "classifications::cancel"
"." = "classifications::menu"
"#;

/// What the tile reads out of the configuration, pushed whole by the app on
/// startup and on every reload so a tile never reads a half-updated one.
#[derive(Debug, Clone, Default)]
pub struct ClassificationsConfig {
    pub dims: DerivedDimensions,
    /// The schema the source columns are checked against.
    pub schema: Rc<SchemaSpec>,
    /// The loaded views, for the references a delete or rename would break.
    pub views: Vec<ViewSpec>,
    /// The layer each classification's winning definition comes from.
    pub layers: BTreeMap<String, Layer>,
    /// Classifications whose user copy shadows a definition in a lower
    /// layer: the ones a revert would restore.
    pub shadowed: BTreeSet<String>,
}

/// What every tile of the factory shares: the latest snapshot and the live
/// tiles it is pushed to.
#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) config: RefCell<Option<ClassificationsConfig>>,
    tiles: RefCell<Vec<WeakEntity<ClassificationsTile>>>,
}

pub struct ClassificationsContent {
    tile: Entity<ClassificationsTile>,
}

impl TileContent for ClassificationsContent {
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

    /// Only `:sort`, which orders this tile's grid. The configuration-level
    /// verbs are registered actions, so they never act from another tile's
    /// line.
    fn command(&self, line: &str, _window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, cx))
    }

    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor)
    }

    /// `/` narrows the grid's rows over source and label.
    fn find(&self, event: FindEvent, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, cx))
    }

    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            // The source column's values, routed by this tile's key; the
            // tile drops one a later read overtook.
            Delivery::Distinct(outcome) => self.tile.update(cx, |t, cx| t.on_distinct(outcome, cx)),
            // The tile asks the data tier for nothing else, so no other
            // answer is this tile's; one that arrived would answer nothing
            // it asked.
            Delivery::Query(_) => {}
            Delivery::Price(_) => {}
            Delivery::Series(_) => {}
            Delivery::SeriesFetched { .. } => {}
            Delivery::Upload(_) => {}
            Delivery::VolSlices(_) => {}
            Delivery::TextFile(_) => {}
        }
    }

    /// The label editor's field, while it owns window focus.
    fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.tile.read(cx).holds_focus(window, cx)
    }

    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, _| t.set_visible(visible))
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

/// Creates classification tiles sharing the app's data handle and the
/// pushed configuration.
pub struct ClassificationsFactory {
    data: DataHandle,
    shared: Rc<Shared>,
}

impl ClassificationsFactory {
    pub fn new(data: DataHandle) -> ClassificationsFactory {
        ClassificationsFactory {
            data,
            shared: Rc::new(Shared::default()),
        }
    }

    /// The snapshot last pushed; `None` before the first.
    pub fn config(&self) -> Option<ClassificationsConfig> {
        self.shared.config.borrow().clone()
    }

    /// Store `config` and tell every live tile. A tile closed since its
    /// last push is pruned here. No borrow is held across a tile's update:
    /// the tile reads the snapshot just stored.
    pub fn set_config(&self, config: ClassificationsConfig, cx: &mut App) {
        *self.shared.config.borrow_mut() = Some(config);
        let live: Vec<Entity<ClassificationsTile>> = {
            let mut tiles = self.shared.tiles.borrow_mut();
            tiles.retain(|t| t.upgrade().is_some());
            tiles.iter().filter_map(WeakEntity::upgrade).collect()
        };
        for tile in live {
            tile.update(cx, |t, cx| t.config_changed(cx));
        }
    }
}

impl ModuleFactory for ClassificationsFactory {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Classifications".to_string(),
            });
        }
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }

    /// A classification is chosen inside the tile, never launched from a
    /// row's dimension.
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
        let entity = cx.new(|cx| {
            ClassificationsTile::new(
                tile,
                restored,
                frame,
                self.data.clone(),
                self.shared.clone(),
                window,
                cx,
            )
        });
        self.shared.tiles.borrow_mut().push(entity.downgrade());
        TileOccupant {
            kind: KIND,
            view: entity.clone().into(),
            content: Box::new(ClassificationsContent { tile: entity }),
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
    fn the_fragment_parses_and_every_binding_names_a_registered_action() {
        use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
        let doc = fragment_doc(crate::KIND, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &[crate::KIND]);
        assert!(diags.is_empty(), "{diags:?}");
        let ids: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        let bindings = doc.table["bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 4, "visual, normal, insert and menu");
        let mut bound = BTreeSet::new();
        let mut count = 0;
        for b in bindings {
            for (key, action) in b["keys"].as_table().unwrap() {
                let action = action.as_str().unwrap();
                assert!(ids.contains(&action), "{key} = {action} is not registered");
                bound.insert(action);
                count += 1;
            }
        }
        assert_eq!(count, 24);
        // The config-level verbs are reached from the palette and the menu.
        for unbound in ["new", "rename", "delete", "revert"] {
            assert!(
                !bound.contains(format!("classifications::{unbound}").as_str()),
                "{unbound} has no default chord"
            );
        }
        // And the registry the app builds accepts every binding.
        let (data, _rx) = DataHandle::for_tests();
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        ClassificationsFactory::new(data).register_actions(&mut registry);
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
}
