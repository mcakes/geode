//! The diagnostics module (Phase 4b Task 5, spec §4.6): one tile, five
//! sections over the shell-owned `Diagnostics` entity and the log ring —
//! `:section`/`:level`/`:overlay`, `[`/`]` to cycle, `mod+shift+d` to open.

pub mod commands;
pub mod sections;
mod tile;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use geode_core::config::Config;
use geode_core::log::Ring;
use geode_core::query::QueryOutcome;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, ModuleFactory, TileContent, TileOccupant};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Entity, Window};

pub use tile::DiagnosticsTile;

/// Nothing to reclaim today (`DataTable` is not used here) — kept for
/// symmetry with `geode_blotter::init`, which every other module-hosting
/// call site (`geode-app::main`) calls unconditionally.
pub fn init(_cx: &mut App) {}

pub const ACTIONS: &[(&str, &str)] = &[
    ("diagnostics::open", "Open diagnostics"),
    ("diagnostics::down", "Cursor down"),
    ("diagnostics::up", "Cursor up"),
    ("diagnostics::top", "Cursor to top"),
    ("diagnostics::bottom", "Cursor to bottom"),
    ("diagnostics::page_down", "Half page down"),
    ("diagnostics::page_up", "Half page up"),
    ("diagnostics::next_section", "Next section"),
    ("diagnostics::prev_section", "Previous section"),
    ("diagnostics::expand", "Expand"),
    ("diagnostics::collapse", "Collapse"),
];

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
    fn deliver(&self, _outcome: QueryOutcome, _window: &mut Window, _cx: &mut App) {
        // This tile never queries — nothing addressed to it ever arrives.
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
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

    /// The shell cannot depend on `geode-diagnostics` (layering: shell
    /// never depends on a module), so `geode_shell::defaults` carries its
    /// own copy of these ids and titles to reserve, ahead of
    /// `DiagnosticsFactory::register_actions`, so `BUILTIN_KEYMAP`'s
    /// `diagnostics::*` bindings (including `mod+shift+d`) are never
    /// dropped as unregistered and the palette shows the same title
    /// either way — same shape as `geode_blotter::tile::tests::
    /// the_shells_reserved_blotter_actions_match_ours`.
    #[test]
    fn the_shells_reserved_diagnostics_actions_match_ours() {
        let ours: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ours, geode_shell::defaults::DIAGNOSTICS_ACTIONS.to_vec());
        assert_eq!(
            ACTIONS,
            geode_shell::defaults::DIAGNOSTICS_ACTION_DEFS,
            "titles must match too, not just ids"
        );
    }
}
