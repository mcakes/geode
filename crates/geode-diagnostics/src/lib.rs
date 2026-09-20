//! The diagnostics module (Phase 4b Task 5, spec §4.6): one tile, five
//! sections over the shell-owned `Diagnostics` entity and the log ring —
//! `:section`/`:level`/`:overlay`, `[`/`]` to cycle. Opened via the status
//! bar's diagnostics-summary click (`ShellView::open_module`) or the
//! palette's `Diagnostics: Split` rows — `diagnostics::open` was retired
//! by user ruling 2026-09-09.

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
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{
    Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant,
};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Entity, SharedString, Window};

pub use tile::DiagnosticsTile;

/// Nothing to reclaim today (`DataTable` is not used here) — kept for
/// symmetry with `geode_blotter::init`, which every other module-hosting
/// call site (`geode-app::main`) calls unconditionally.
pub fn init(_cx: &mut App) {}

pub const ACTIONS: &[(&str, &str)] = &[
    ("diagnostics::down", "Cursor down"),
    ("diagnostics::up", "Cursor up"),
    ("diagnostics::top", "Cursor to top"),
    ("diagnostics::bottom", "Cursor to bottom"),
    ("diagnostics::page_down", "Half page down"),
    ("diagnostics::page_up", "Half page up"),
    ("diagnostics::page_down_full", "Page down"),
    ("diagnostics::page_up_full", "Page up"),
    ("diagnostics::next_section", "Next section"),
    ("diagnostics::prev_section", "Previous section"),
    ("diagnostics::expand", "Expand"),
    ("diagnostics::collapse", "Collapse"),
];

/// This module's default bindings (market-data documents §8.4), handed to
/// the app through [`ModuleFactory::default_keymap`]. Until Part 3 this
/// `[[bindings]]` section lived in the shell's own `BUILTIN_KEYMAP`,
/// beside a mirrored copy of [`ACTIONS`] the shell had to carry because it
/// cannot depend on this crate; both copies are gone, and the ids a
/// binding names are now the same list `register_actions` registers, in
/// the same crate.
///
/// One context, and no `mode` pair: this tile has no modes — `[`/`]`
/// cycle its five sections and `/` (the shell's own `tile` binding)
/// filters.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "diagnostics"
[bindings.keys]
"j" = "diagnostics::down"
"k" = "diagnostics::up"
"g g" = "diagnostics::top"
"shift+g" = "diagnostics::bottom"
"ctrl+d" = "diagnostics::page_down"
"ctrl+u" = "diagnostics::page_up"
"ctrl+f" = "diagnostics::page_down_full"
"ctrl+b" = "diagnostics::page_up_full"
"pagedown" = "diagnostics::page_down_full"
"pageup" = "diagnostics::page_up_full"
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

/// Builds `diagnostics` tile occupants (§9.1). Holds the log ring and a
/// refreshed copy of the loaded `Config` (for the config section's
/// effective-config explainer) — `set_config` is called by the app bridge
/// on every `ShellEvent::ConfigReloaded`, the same door `BlotterFactory::
/// set_views` uses.
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
    }

    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
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

    /// What the retired `the_shells_reserved_diagnostics_actions_match_ours`
    /// and the shell's own `every_diagnostics_binding_target_is_reserved`
    /// together used to guarantee, now provable inside this crate with no
    /// mirrored copy of anything — the twin of
    /// `geode_blotter::content`'s own fragment test, and the same two
    /// directions: every id the fragment binds is registered here (a
    /// `build_keymap` warning is exactly that failure), and every
    /// registered action is reachable from some key.
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
}
