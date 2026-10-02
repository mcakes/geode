//! The diagnostics page: five sections over the shell-owned `Diagnostics`
//! entity, the log ring, the loaded config, and the frame's requery stats.
//! Registered by the app as a `PageFactory`; reached from the sidebar, the
//! palette, the status-bar summary, and `mod+d`.

mod config_view;
mod levels;
pub mod log;
mod log_view;
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
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{PageContent, PageFactory, PageOccupant, ShellActions};
use gpui::{App, AppContext as _, Entity, SharedString, Window};

pub use page::DiagnosticsPage;

/// Module initialization hook. Diagnostics has no component bindings to register.
pub fn init(_cx: &mut App) {}

pub const ACTIONS: &[(&str, &str)] = &[
    ("diagnostics::next_section", "Next section"),
    ("diagnostics::prev_section", "Previous section"),
    ("diagnostics::expand", "Expand"),
    ("diagnostics::collapse", "Collapse"),
    ("diagnostics::activate", "Toggle expansion"),
    ("diagnostics::filter", "Filter"),
    ("diagnostics::reset_filters", "Reset section filters"),
    ("diagnostics::blur", "Leave the filter"),
    ("diagnostics::sources", "Show sources"),
    ("diagnostics::data", "Show data"),
    ("diagnostics::config", "Show configuration"),
    ("diagnostics::log", "Show log"),
    ("diagnostics::perf", "Show performance"),
    ("diagnostics::copy", "Copy row details"),
    ("diagnostics::next_view", "Next configuration view"),
    ("diagnostics::prev_view", "Previous configuration view"),
    ("diagnostics::follow", "Toggle log following"),
    ("diagnostics::refresh", "Refresh catalog"),
    ("diagnostics::expand_all", "Expand all datasets"),
    ("diagnostics::collapse_all", "Collapse all datasets"),
    ("diagnostics::more_levels", "Show one more log level"),
    ("diagnostics::fewer_levels", "Show one fewer log level"),
    ("diagnostics::next_target", "Next log target"),
    ("diagnostics::prev_target", "Previous log target"),
    ("diagnostics::clear_log", "Clear log"),
    ("diagnostics::log_levels", "Set log levels…"),
    ("diagnostics::open_config_dir", "Open config directory"),
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

/// Default bindings supplied through [`PageFactory::default_keymap`].
/// Binding IDs and their registrations in [`ACTIONS`] live together here,
/// keeping the shell independent of this feature crate.
///
/// One context with two modes. The page publishes `grid` beside its mode,
/// so its cursor takes the shell's shared `motion::*` bindings, which this
/// fragment therefore does not repeat: it binds `[`/`]` to cycle sections,
/// `z o`/`z c`/`enter`/`space` to fold, `tab`/`shift+tab` to step a
/// section's views, `/` to focus the filter, and a key for every toolbar
/// control; in insert mode `escape` leaves it. A modified key is spelled
/// with `+` and shift is explicit (`z shift+r`): `alt-backspace` or `z R`
/// would parse as a key no keyboard sends. The page's toggle binding is the factory's
/// `toggle_binding`, emitted by the roster.
///
/// The bare keys carry `mode == normal` for the reason a module's do: with
/// the filter focused the shell's insert route resolves bare keys against
/// every context that carries `mode == insert`, and the page's context does
/// then, so a table without the mode clause would fire `G` or `enter`
/// instead of typing them.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "diagnostics && mode == normal"
[bindings.keys]
"[" = "diagnostics::prev_section"
"]" = "diagnostics::next_section"
"z o" = "diagnostics::expand"
"z c" = "diagnostics::collapse"
"enter" = "diagnostics::activate"
"space" = "diagnostics::activate"
"/" = "diagnostics::filter"
"alt+backspace" = "diagnostics::reset_filters"
"g s" = "diagnostics::sources"
"g d" = "diagnostics::data"
"g c" = "diagnostics::config"
"g l" = "diagnostics::log"
"g p" = "diagnostics::perf"
"y" = "diagnostics::copy"
"ctrl+tab" = "diagnostics::next_view"
"ctrl+shift+tab" = "diagnostics::prev_view"
"tab" = "diagnostics::next_view"
"shift+tab" = "diagnostics::prev_view"
"o" = "diagnostics::open_config_dir"
"f" = "diagnostics::follow"
"r" = "diagnostics::refresh"
"z shift+r" = "diagnostics::expand_all"
"z shift+m" = "diagnostics::collapse_all"
"=" = "diagnostics::more_levels"
"-" = "diagnostics::fewer_levels"
"t" = "diagnostics::next_target"
"shift+t" = "diagnostics::prev_target"
"ctrl+l" = "diagnostics::clear_log"
"shift+l" = "diagnostics::log_levels"

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
        for (old, new) in RENAMED_ACTIONS {
            registry
                .register_rename(old, new)
                .expect("diagnostics retired ids are unique");
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
        frame: FrameRef,
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

    /// Every table in the fragment names a mode. A table on the bare
    /// `diagnostics` context passes the fragment check and binds every
    /// action, yet its bare keys fire inside the focused filter: the insert
    /// route keeps every context carrying `mode == insert`, which the page's
    /// does while an input holds focus.
    #[test]
    fn every_default_keymap_table_names_a_mode() {
        let doc: toml::Table = toml::from_str(DEFAULT_KEYMAP).expect("the fragment parses");
        let tables = doc
            .get("bindings")
            .and_then(|b| b.as_array())
            .expect("[[bindings]] tables");
        assert!(!tables.is_empty());
        for table in tables {
            let context = table.get("context").and_then(|c| c.as_str());
            assert!(
                matches!(
                    context,
                    Some("diagnostics && mode == normal") | Some("diagnostics && mode == insert")
                ),
                "a bindings table without a mode clause fires its bare keys inside the filter: {context:?}"
            );
        }
    }

    /// Every key in the fragment is one a keyboard sends: the parser
    /// lowercases and splits on `+` only, so `alt-backspace` would compile
    /// to a key named `alt-backspace` and `z R` to a plain `z r`, both
    /// silently dead.
    #[test]
    fn every_default_key_is_spelled_as_a_keyboard_sends_it() {
        const NAMED: &[&str] = &["enter", "space", "tab", "backspace", "escape"];
        let doc: toml::Table = toml::from_str(DEFAULT_KEYMAP).expect("the fragment parses");
        for table in doc["bindings"].as_array().unwrap() {
            for spec in table["keys"].as_table().unwrap().keys() {
                for part in spec.split_whitespace() {
                    let key = part.rsplit('+').next().unwrap();
                    assert!(
                        !part.chars().any(|c| c.is_ascii_uppercase()),
                        "{spec}: shift is spelled `shift+`, not by case"
                    );
                    assert!(
                        key.chars().count() == 1 || NAMED.contains(&key),
                        "{spec}: `{key}` is no key a keyboard sends"
                    );
                }
            }
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

    /// A user keymap written against a retired motion id keeps binding the
    /// shared id it became.
    #[test]
    fn every_retired_motion_id_renames_to_its_shared_id() {
        let factory = DiagnosticsPageFactory::new(Arc::new(Ring::new(4)), Config::default());
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
