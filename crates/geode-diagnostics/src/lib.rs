//! The diagnostics page: five sections over the shell-owned `Diagnostics`
//! entity, the log ring, the loaded config, and the frame's requery stats.
//! Registered by the app as a `PageFactory`; reached from the sidebar, the
//! palette, the status-bar summary, and `mod+d`.

mod config_view;
mod levels;
pub mod log;
pub mod model;
mod page;
mod page_chrome;
mod perf_view;
pub mod prepared;
pub mod section;
mod table;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use geode_core::config::Config;
use geode_core::log::Ring;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::{DIAGNOSTICS_PAGE_KIND, Diagnostics};
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{PageContent, PageFactory, PageOccupant, ShellActions};
use gpui::{App, AppContext as _, Entity, SharedString, Window};

pub use page::DiagnosticsPage;

/// Module initialization hook. Diagnostics has no component bindings to register.
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
    ("diagnostics::activate", "Toggle expansion"),
    ("diagnostics::filter", "Filter"),
    ("diagnostics::blur", "Leave the filter"),
];

/// Default bindings supplied through [`PageFactory::default_keymap`].
/// Binding IDs and their registrations in [`ACTIONS`] live together here,
/// keeping the shell independent of this feature crate.
///
/// One context with two modes: `[`/`]` cycle sections and `/` focuses the
/// filter; in insert mode `escape` leaves it. The page's toggle binding is
/// the factory's `toggle_binding`, emitted by the roster.
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
"enter" = "diagnostics::activate"
"/" = "diagnostics::filter"

[[bindings]]
context = "diagnostics && mode == insert"
[bindings.keys]
"escape" = "diagnostics::blur"
"#;

struct DiagnosticsContent {
    page: Entity<DiagnosticsPage>,
}

impl PageContent for DiagnosticsContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.page.read(cx).key_context()
    }
    fn dispatch(
        &self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.page
            .update(cx, |p, cx| p.dispatch(action, count, window, cx))
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.page.update(cx, |p, cx| p.set_visible(visible, cx))
    }
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.page.read(cx).focus_handle()
    }
    fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.page.read(cx).holds_focus(window, cx)
    }
    fn title(&self, cx: &App) -> SharedString {
        self.page.read(cx).title()
    }
    fn serialize(&self, cx: &App) -> toml::Table {
        self.page.read(cx).serialize()
    }
}

/// Builds the diagnostics page sharing the log ring and loaded config.
/// The app refreshes the config through `set_config` before the page's
/// frame observer rebuilds the effective-config rows.
pub struct DiagnosticsPageFactory {
    ring: Arc<Ring>,
    config: Rc<RefCell<Config>>,
}

impl DiagnosticsPageFactory {
    pub fn new(ring: Arc<Ring>, config: Config) -> DiagnosticsPageFactory {
        DiagnosticsPageFactory {
            ring,
            config: Rc::new(RefCell::new(config)),
        }
    }

    /// The app refreshes this before page frame observers rebuild config rows.
    pub fn set_config(&self, config: Config) {
        *self.config.borrow_mut() = config;
    }
}

impl PageFactory for DiagnosticsPageFactory {
    fn kind(&self) -> &'static str {
        DIAGNOSTICS_PAGE_KIND
    }

    fn title(&self) -> &'static str {
        "Diagnostics"
    }

    fn icon(&self) -> gpui_kit_assets::IconName {
        gpui_kit_assets::IconName::Activity
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            registry
                .register(ActionDef {
                    id: ActionId((*id).to_string()),
                    title: (*title).to_string(),
                    category: "Diagnostics".to_string(),
                })
                .expect("diagnostics action ids are unique");
        }
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }

    fn toggle_binding(&self) -> Option<&'static str> {
        Some("mod+d")
    }

    fn create(
        &self,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        actions: ShellActions,
        window: &mut Window,
        cx: &mut App,
    ) -> PageOccupant {
        let (ring, config) = (self.ring.clone(), self.config.clone());
        let page = cx.new(|cx| {
            DiagnosticsPage::new(
                frame,
                diagnostics,
                ring,
                config,
                actions,
                restored,
                window,
                cx,
            )
        });
        PageOccupant {
            kind: DIAGNOSTICS_PAGE_KIND,
            view: page.clone().into(),
            content: Box::new(DiagnosticsContent { page }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::keymap::fragments::{check_fragment, fragment_doc};

    /// Every default binding names a registered action, and every registered
    /// action is reachable through the page's keymap fragment.
    #[test]
    fn the_default_keymap_binds_exactly_the_actions_this_page_registers() {
        let doc = fragment_doc("diagnostics", DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &["diagnostics"]);
        assert!(
            diags.is_empty(),
            "every fragment binding must name this page's own context: {diags:?}"
        );
        let mut registry = ActionRegistry::default();
        DiagnosticsPageFactory::new(Arc::new(Ring::new(8)), Config::default())
            .register_actions(&mut registry);
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

    /// The factory is what the app asks: a fragment or toggle binding the
    /// factory does not return is a page with no keys, and nothing would
    /// report it.
    #[test]
    fn the_factory_ships_the_fragment_the_toggle_and_the_page_kind() {
        let factory = DiagnosticsPageFactory::new(Arc::new(Ring::new(8)), Config::default());
        assert_eq!(factory.kind(), DIAGNOSTICS_PAGE_KIND);
        assert_eq!(factory.default_keymap(), Some(DEFAULT_KEYMAP));
        assert_eq!(factory.toggle_binding(), Some("mod+d"));
        assert_eq!(factory.contexts(), vec!["diagnostics"]);
    }
}
