//! One diagnostics tile with five sections over the shell-owned
//! `Diagnostics` entity and log ring. `:section` changes the visible section;
//! `[` and `]` cycle. Log level and overlay changes use palette actions.

pub mod commands;
pub mod sections;
mod tile;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use geode_core::config::Config;
use geode_core::log::Ring;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{
    Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant,
};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Entity, SharedString, Window};

pub use tile::DiagnosticsTile;

/// Module initialization hook. Diagnostics has no component bindings to register.
pub fn init(_cx: &mut App) {}

pub const ACTIONS: &[(&str, &str)] = &[
    ("diagnostics::next_section", "Next section"),
    ("diagnostics::prev_section", "Previous section"),
    ("diagnostics::expand", "Expand"),
    ("diagnostics::collapse", "Collapse"),
];

/// Retired action ids and their successors: a user keymap that still names
/// an old id binds the new one, with a warning (`ActionRegistry::renamed`).
pub const RENAMED_ACTIONS: &[(&str, &str)] = &[
    ("diagnostics::down", "motion::down"),
    ("diagnostics::up", "motion::up"),
    ("diagnostics::top", "motion::top"),
    ("diagnostics::bottom", "motion::bottom"),
    ("diagnostics::page_down", "motion::half_page_down"),
    ("diagnostics::page_up", "motion::half_page_up"),
    ("diagnostics::page_down_full", "motion::page_down"),
    ("diagnostics::page_up_full", "motion::page_up"),
];

/// Default bindings supplied through [`ModuleFactory::default_keymap`].
/// Binding IDs and their registrations in [`ACTIONS`] live together here,
/// keeping the shell independent of this feature crate.
///
/// The tile has one context and one mode: it publishes `grid` and
/// `mode == normal`, so its cursor takes the shell's shared `motion::*`
/// bindings, which this fragment therefore does not repeat. `[`/`]` cycle
/// sections, `z o`/`z c` fold, and `/` uses the shell's shared tile find
/// binding.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "diagnostics"
[bindings.keys]
"[" = "diagnostics::prev_section"
"]" = "diagnostics::next_section"
"z o" = "diagnostics::expand"
"z c" = "diagnostics::collapse"
"#;

struct DiagnosticsContent {
    tile: Entity<DiagnosticsTile>,
}

impl TileContent for DiagnosticsContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context()
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
        let _ = cx;
        commands::completions(line, cursor)
    }
    fn find(&self, event: FindEvent, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, cx))
    }
    fn deliver(&self, delivery: Delivery, _window: &mut Window, _cx: &mut App) {
        match delivery {
            // This tile never queries — nothing addressed to it ever
            // arrives, so there is nothing to do with the outcome itself.
            Delivery::Query(_) => {}
            // This tile never prices; an outcome addressed here is a routing bug.
            Delivery::Price(_) => {}
            // This tile asks no series query and holds no
            // `(identity, source)` pair — and a key-less
            // `SeriesFetched` DOES reach it, since it is broadcast to
            // every visible occupant.
            Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}
            // This tile never uploads; an outcome addressed here is a routing bug.
            Delivery::Upload(_) => {}
        }
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
    }
    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_stack(stack, cx))
    }
    fn title(&self, cx: &App) -> SharedString {
        self.tile.read(cx).title()
    }
    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize()
    }
}

/// Builds diagnostics tile occupants sharing the log ring and loaded config.
/// The app refreshes the config through `set_config` before tile frame
/// observers rebuild the effective-config rows.
pub struct DiagnosticsFactory {
    ring: Arc<Ring>,
    config: Rc<RefCell<Config>>,
}

impl DiagnosticsFactory {
    pub fn new(ring: Arc<Ring>, config: Config) -> DiagnosticsFactory {
        DiagnosticsFactory {
            ring,
            config: Rc::new(RefCell::new(config)),
        }
    }

    pub fn set_config(&self, config: Config) {
        *self.config.borrow_mut() = config;
    }
}

impl ModuleFactory for DiagnosticsFactory {
    fn kind(&self) -> &'static str {
        "diagnostics"
    }

    /// The one context `DiagnosticsTile::key_context` names.
    fn contexts(&self) -> Vec<&'static str> {
        vec!["diagnostics"]
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Diagnostics".to_string(),
            });
        }
        // A second diagnostics factory's repeat answers "renamed twice",
        // which is the correct outcome, like the ids above.
        for (old, new) in RENAMED_ACTIONS {
            let _ = registry.register_rename(old, new);
        }
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
        let entity = cx.new(|cx| {
            DiagnosticsTile::new(
                tile,
                frame,
                diagnostics,
                self.ring.clone(),
                self.config.clone(),
                restored,
                window,
                cx,
            )
        });
        TileOccupant {
            kind: "diagnostics",
            view: entity.clone().into(),
            content: Box::new(DiagnosticsContent { tile: entity }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::keymap::fragments::{check_fragment, fragment_doc};

    /// Every default binding names a registered action, and every registered
    /// action is reachable through the module's keymap fragment.
    #[test]
    fn the_default_keymap_binds_exactly_the_actions_this_module_registers() {
        let doc = fragment_doc("diagnostics", DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &["diagnostics"]);
        assert!(
            diags.is_empty(),
            "every fragment binding must name this module's own context: {diags:?}"
        );
        let mut registry = ActionRegistry::default();
        for (id, title) in ACTIONS {
            registry
                .register(ActionDef {
                    id: ActionId((*id).to_string()),
                    title: (*title).to_string(),
                    category: "Diagnostics".to_string(),
                })
                .expect("no duplicate ids");
        }
        let (keymap, diags) = geode_shell::keymap::build_keymap(
            &[doc],
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(
            diags.is_empty(),
            "the fragment must bind only registered actions: {diags:?}"
        );
        let bound: std::collections::BTreeSet<&str> = keymap
            .bindings()
            .iter()
            .map(|b| b.action.0.as_str())
            .collect();
        for (id, _) in ACTIONS {
            assert!(
                bound.contains(id),
                "{id} is registered but the default keymap binds nothing to it"
            );
        }
    }

    /// The factory is what the app asks: a `DEFAULT_KEYMAP` the factory
    /// does not return is a diagnostics tile with no keys, and nothing
    /// would report it.
    #[test]
    fn the_factory_ships_the_fragment_and_declares_the_diagnostics_context() {
        let factory = DiagnosticsFactory::new(
            Arc::new(Ring::new(8)),
            geode_core::config::Config::load(&geode_core::config::ConfigSources::default()),
        );
        assert_eq!(factory.default_keymap(), Some(DEFAULT_KEYMAP));
        assert_eq!(factory.contexts(), vec!["diagnostics"]);
    }

    /// A user keymap written against a retired motion id keeps binding the
    /// shared id it became.
    #[test]
    fn every_retired_motion_id_renames_to_its_shared_id() {
        let factory = DiagnosticsFactory::new(
            Arc::new(Ring::new(4)),
            geode_core::config::Config::load(&geode_core::config::ConfigSources::default()),
        );
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        factory.register_actions(&mut registry);
        for (old, new) in [
            ("diagnostics::down", "motion::down"),
            ("diagnostics::up", "motion::up"),
            ("diagnostics::top", "motion::top"),
            ("diagnostics::bottom", "motion::bottom"),
            ("diagnostics::page_down", "motion::half_page_down"),
            ("diagnostics::page_up", "motion::half_page_up"),
            ("diagnostics::page_down_full", "motion::page_down"),
            ("diagnostics::page_up_full", "motion::page_up"),
        ] {
            assert_eq!(
                registry.renamed(&ActionId(old.into())),
                Some(&ActionId(new.into())),
                "{old}"
            );
        }
    }
}
