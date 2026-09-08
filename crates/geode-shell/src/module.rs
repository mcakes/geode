//! The module-hosting contract (foundation §9.1, Phase 3 §3). A tile is
//! still a `TileId`; what lives in it is a [`TileOccupant`] the shell
//! created through a [`ModuleFactory`] from the app's [`ModuleRoster`].
//!
//! Nothing here names `geode-data`. The factory gets shell-side handles
//! only — the tile id and the frame entity — and a module that needs
//! data carries its own handle as a field of its factory, built in
//! `geode-app` where both sides meet (§2.1). The one data type that
//! crosses is `geode_core::query::QueryOutcome`, which the shell routes
//! to the tile whose id is the outcome's key.

use crate::actions::{ActionId, ActionRegistry};
use crate::diagnostics::Diagnostics;
use crate::frame::Frame;
use crate::keymap::KeyContext;
use crate::tiling::TileId;
use geode_core::query::QueryOutcome;
use gpui::{AnyView, App, Entity, Window};

/// What the `/` line tells the occupant (§3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindEvent {
    Changed(String),
    Committed(String),
    Cancelled,
}

pub trait TileContent {
    /// Pushed onto the keymap context stack while this tile is focused,
    /// e.g. `blotter` with `mode = normal`, opted into counts.
    fn key_context(&self, cx: &App) -> KeyContext;
    /// An action the shell did not recognise. `true` if handled.
    fn dispatch(
        &self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool;
    /// A `:` line, without the colon. `Err` is shown inline on the line.
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String>;
    /// Candidates for the word under `cursor` on a `:` line. The shell
    /// ranks and shows them; the occupant only knows its vocabulary.
    /// Each candidate is the bare WORD for that position (`ingest`,
    /// never `level ingest`): the shell splices the accepted one into
    /// the line in place of the word under the cursor
    /// (`commandline::accept`), so a whole-line candidate doubles the
    /// line (`level level ingest` — seen on a display 2026-09-08). Return
    /// the position's whole vocabulary, unfiltered; the shell's ranking
    /// narrows it, and Enter refuses a partial word that ranks more than
    /// one candidate rather than guessing.
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String>;
    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App);
    /// A query result addressed to this tile (§5.1).
    fn deliver(&self, outcome: QueryOutcome, window: &mut Window, cx: &mut App);
    /// Hidden tiles may drop subscriptions; shown tiles requery if stale.
    ///
    /// **Contract (I2, final review):** an occupant is told its
    /// visibility on the first render after `ModuleFactory::create`
    /// returns it, whatever that visibility is — `ShellView::
    /// ensure_occupants` calls this once, immediately, with the tile's
    /// membership in the active workspace's visible set. Until that call
    /// arrives, a fresh occupant must treat itself as hidden: it is
    /// created for every tile in every workspace and dock on first
    /// render (`fill_all_tiles`), most of them off-screen, and holding
    /// live subscriptions for all of them until the shell speaks would be
    /// exactly the resource leak this method exists to prevent.
    fn set_visible(&self, visible: bool, cx: &mut App);
    /// State for `session.toml` (§3.5); stored opaquely by the shell.
    fn serialize(&self, cx: &App) -> toml::Table;
}

pub struct TileOccupant {
    pub kind: &'static str,
    /// What the shell paints inside the tile chrome.
    pub view: AnyView,
    pub content: Box<dyn TileContent>,
}

pub trait ModuleFactory {
    fn kind(&self) -> &'static str;
    /// Runs once, before the keymap builds — `build_keymap` drops any
    /// binding whose action is unregistered.
    fn register_actions(&self, registry: &mut ActionRegistry);
    /// Build a fresh occupant for `tile`. **Contract (I2, final
    /// review):** the occupant does not yet know whether it is on
    /// screen — `TileContent::set_visible`'s own doc comment states the
    /// other half: the shell announces that on the first render after
    /// this call returns. A factory whose occupant does anything
    /// screen-dependent (subscribing, requerying) before that first
    /// `set_visible` call should assume it is hidden.
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant;
}

/// The only place the app knows which modules exist (§9.1).
pub struct ModuleRoster {
    factories: Vec<Box<dyn ModuleFactory>>,
    default_kind: String,
}

impl ModuleRoster {
    pub fn new(default_kind: impl Into<String>) -> ModuleRoster {
        ModuleRoster {
            factories: Vec::new(),
            default_kind: default_kind.into(),
        }
    }

    pub fn add(&mut self, factory: Box<dyn ModuleFactory>) {
        self.factories.push(factory);
    }

    pub fn register_actions(&self, registry: &mut ActionRegistry) {
        for f in &self.factories {
            f.register_actions(registry);
        }
    }

    pub fn factory(&self, kind: &str) -> Option<&dyn ModuleFactory> {
        self.factories
            .iter()
            .find(|f| f.kind() == kind)
            .map(|f| f.as_ref())
    }

    pub fn default_factory(&self) -> Option<&dyn ModuleFactory> {
        self.factory(&self.default_kind)
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.factories.iter().map(|f| f.kind()).collect()
    }
}

impl Default for ModuleRoster {
    fn default() -> Self {
        ModuleRoster::new("placeholder")
    }
}

/// The occupant of a tile nothing else claims: an unknown session kind,
/// or a roster with no default. Paints a hint naming the palette; never
/// a blank, never a panic.
pub mod placeholder {
    use super::*;
    use gpui::prelude::*;
    use gpui::{Context, Render, div};
    use gpui_component::ActiveTheme as _;

    /// The kind string a placeholder occupant's `TileOccupant::kind`
    /// carries (Phase 4b Task 1 fix round 1, MIN-7) — named here, next
    /// to the factory that is its one source of truth, so every other
    /// site that must recognize a placeholder occupant (`PlaceholderFactory
    /// ::kind`, `PlaceholderFactory::create`'s `TileOccupant`, and
    /// `shell::occupants`'s two `visible_tile_keys`/`current_tiles`
    /// filters) names this constant instead of repeating the bare string
    /// literal `"placeholder"` — a rename of one becomes a compile error
    /// everywhere else instead of a silent behaviour change.
    pub const PLACEHOLDER_KIND: &str = "placeholder";

    pub struct PlaceholderFactory;

    struct PlaceholderView {
        tile: TileId,
    }

    impl Render for PlaceholderView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .debug_selector(|| format!("tile-content-{}", self.tile.0))
                .child("ctrl+k → open a view")
        }
    }

    struct PlaceholderContent;

    impl TileContent for PlaceholderContent {
        fn key_context(&self, _cx: &App) -> KeyContext {
            KeyContext::new("placeholder")
        }
        fn dispatch(&self, _: &ActionId, _: Option<u32>, _: &mut Window, _: &mut App) -> bool {
            false
        }
        fn command(&self, _: &str, _: &mut Window, _: &mut App) -> Result<(), String> {
            Err("this tile has no module; open one from the palette".into())
        }
        fn completions(&self, _: &str, _: usize, _: &App) -> Vec<String> {
            Vec::new()
        }
        fn find(&self, _: FindEvent, _: &mut Window, _: &mut App) {}
        fn deliver(&self, _: QueryOutcome, _: &mut Window, _: &mut App) {}
        fn set_visible(&self, _: bool, _: &mut App) {}
        fn serialize(&self, _: &App) -> toml::Table {
            toml::Table::new()
        }
    }

    impl ModuleFactory for PlaceholderFactory {
        fn kind(&self) -> &'static str {
            PLACEHOLDER_KIND
        }
        fn register_actions(&self, _: &mut ActionRegistry) {}
        fn create(
            &self,
            tile: TileId,
            _: Option<&toml::Table>,
            _: Entity<Frame>,
            _: Entity<Diagnostics>,
            _: &mut Window,
            cx: &mut App,
        ) -> TileOccupant {
            let view = cx.new(|_| PlaceholderView { tile });
            TileOccupant {
                kind: PLACEHOLDER_KIND,
                view: view.into(),
                content: Box::new(PlaceholderContent),
            }
        }
    }
}

/// A module that records everything the shell does to it, for the
/// shell's own hosting tests and for module tests that need a neighbour.
#[cfg(any(test, feature = "test-support"))]
pub mod recording {
    use super::*;
    use gpui::prelude::*;
    use gpui::{Context, FocusHandle, Render, div};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Recorded {
        Created(TileId, Option<toml::Table>),
        Dispatch(TileId, ActionId, Option<u32>),
        Command(TileId, String),
        Find(TileId, FindEvent),
        Visible(TileId, bool),
        Delivered(TileId, u64),
    }

    pub struct RecordingFactory {
        kind: &'static str,
        pub log: Rc<RefCell<Vec<Recorded>>>,
        /// What `completions` answers, regardless of the line.
        pub completions: Vec<String>,
        /// What `command` answers.
        pub command_result: Result<(), String>,
    }

    impl RecordingFactory {
        pub fn new(kind: &'static str) -> RecordingFactory {
            RecordingFactory {
                kind,
                log: Rc::new(RefCell::new(Vec::new())),
                completions: vec!["delta01".into(), "gamma01".into()],
                command_result: Ok(()),
            }
        }
    }

    /// Tracks its own focus handle on purpose: that is what `DataTable`
    /// does, and the shell's click-to-focus restore is tested against it.
    struct RecordingView {
        tile: TileId,
        focus: FocusHandle,
    }

    impl Render for RecordingView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .debug_selector(|| format!("tile-content-{}", self.tile.0))
                .child(format!("rec {}", self.tile.0))
        }
    }

    struct RecordingContent {
        tile: TileId,
        log: Rc<RefCell<Vec<Recorded>>>,
        completions: Vec<String>,
        command_result: Result<(), String>,
        pub state: RefCell<toml::Table>,
    }

    impl TileContent for RecordingContent {
        fn key_context(&self, _cx: &App) -> KeyContext {
            KeyContext::new("rec").pair("mode", "normal").counts()
        }
        fn dispatch(
            &self,
            action: &ActionId,
            count: Option<u32>,
            _: &mut Window,
            _: &mut App,
        ) -> bool {
            self.log
                .borrow_mut()
                .push(Recorded::Dispatch(self.tile, action.clone(), count));
            action.0.starts_with("rec::")
        }
        fn command(&self, line: &str, _: &mut Window, _: &mut App) -> Result<(), String> {
            self.log
                .borrow_mut()
                .push(Recorded::Command(self.tile, line.to_string()));
            self.state
                .borrow_mut()
                .insert("last_command".into(), toml::Value::String(line.to_string()));
            self.command_result.clone()
        }
        fn completions(&self, _: &str, _: usize, _: &App) -> Vec<String> {
            self.completions.clone()
        }
        fn find(&self, event: FindEvent, _: &mut Window, _: &mut App) {
            self.log.borrow_mut().push(Recorded::Find(self.tile, event));
        }
        fn deliver(&self, outcome: QueryOutcome, _: &mut Window, _: &mut App) {
            self.log
                .borrow_mut()
                .push(Recorded::Delivered(self.tile, outcome.tag));
        }
        fn set_visible(&self, visible: bool, _: &mut App) {
            self.log
                .borrow_mut()
                .push(Recorded::Visible(self.tile, visible));
        }
        fn serialize(&self, _: &App) -> toml::Table {
            self.state.borrow().clone()
        }
    }

    impl ModuleFactory for RecordingFactory {
        fn kind(&self) -> &'static str {
            self.kind
        }
        fn register_actions(&self, registry: &mut ActionRegistry) {
            let _ = registry.register(crate::actions::ActionDef {
                id: ActionId(format!("{}::noop", self.kind)),
                title: "Recording no-op".into(),
                category: "Test".into(),
            });
        }
        fn create(
            &self,
            tile: TileId,
            restored: Option<&toml::Table>,
            _: Entity<Frame>,
            _: Entity<Diagnostics>,
            _: &mut Window,
            cx: &mut App,
        ) -> TileOccupant {
            self.log
                .borrow_mut()
                .push(Recorded::Created(tile, restored.cloned()));
            let focus = cx.focus_handle();
            let view = cx.new(|_| RecordingView { tile, focus });
            TileOccupant {
                kind: self.kind,
                view: view.into(),
                content: Box::new(RecordingContent {
                    tile,
                    log: self.log.clone(),
                    completions: self.completions.clone(),
                    command_result: self.command_result.clone(),
                    state: RefCell::new(restored.cloned().unwrap_or_default()),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_roster_finds_factories_by_kind_and_names_the_default() {
        let mut roster = ModuleRoster::new("rec");
        assert!(roster.default_factory().is_none(), "nothing added yet");
        roster.add(Box::new(recording::RecordingFactory::new("rec")));
        roster.add(Box::new(placeholder::PlaceholderFactory));
        assert_eq!(roster.kinds(), vec!["rec", "placeholder"]);
        assert_eq!(roster.factory("rec").map(|f| f.kind()), Some("rec"));
        assert_eq!(roster.default_factory().map(|f| f.kind()), Some("rec"));
        assert!(roster.factory("nonesuch").is_none());
    }

    #[test]
    fn registering_actions_delegates_to_every_factory_once() {
        let mut roster = ModuleRoster::new("rec");
        roster.add(Box::new(recording::RecordingFactory::new("rec")));
        let mut registry = ActionRegistry::default();
        roster.register_actions(&mut registry);
        assert!(registry.contains(&ActionId("rec::noop".into())));
        // Registering twice is the roster's caller's mistake, and the
        // registry says so rather than silently duplicating.
        let mut again = ActionRegistry::default();
        roster.register_actions(&mut again);
        assert_eq!(again.iter().count(), 1);
    }
}
