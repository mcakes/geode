//! The module-hosting contract (foundation §9.1, Phase 3 §3). A tile is
//! still a `TileId`; what lives in it is a [`TileOccupant`] the shell
//! created through a [`ModuleFactory`] from the app's [`ModuleRoster`].
//!
//! Nothing here names `geode-data`. The factory gets shell-side handles
//! only — the tile id and the frame entity — and a module that needs
//! data carries its own handle as a field of its factory, built in
//! `geode-app` where both sides meet (§2.1). The one data type that
//! crosses is [`Delivery`], which the shell routes to the tile whose id
//! is `Delivery::key()`.

use crate::actions::{ActionId, ActionRegistry};
use crate::diagnostics::Diagnostics;
use crate::frame::Frame;
use crate::keymap::KeyContext;
use crate::keymap::fragments;
use crate::tiling::TileId;
use geode_core::config::{Diagnostic, LayerDoc};
use geode_core::query::{QueryKey, QueryOutcome};
use gpui::{AnyView, App, Entity, Window};

/// What the `/` line tells the occupant (§3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindEvent {
    Changed(String),
    Committed(String),
    Cancelled,
}

/// What the shell routes to a tile by its id (market-data spec §8.6).
/// One variant today — a query result addressed by its `QueryKey`
/// (§5.1) — but Part 4 adds `Upload(UploadOutcome)` for a document
/// upload's own outcome, carried through this same door. An enum
/// rather than a second `TileContent` method: every existing `match` on
/// `Delivery` then refuses to compile the instant a new variant lands,
/// until the occupant it belongs to grows an arm for it — an occupant
/// cannot silently ignore a delivery kind it was never taught about, the
/// way an unmatched second method could be forgotten and no compiler
/// would say a word.
#[derive(Debug)]
pub enum Delivery {
    Query(QueryOutcome),
}

impl Delivery {
    /// The tile id (as a bare `QueryKey`) this delivery is addressed to.
    /// `ShellView::deliver` routes on this alone, never on the variant.
    pub fn key(&self) -> QueryKey {
        match self {
            Delivery::Query(outcome) => outcome.key,
        }
    }
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
    /// What the shell routed to this tile (§5.1): a query result today,
    /// with more kinds to come through the same door (Part 4 adds a
    /// document upload's own outcome). [`Delivery`] is an enum rather
    /// than a second trait method precisely so this `match` — and every
    /// other occupant's — refuses to compile the moment a new variant
    /// lands, until it has an arm for it; silently ignoring a delivery
    /// kind is not an option a wildcard arm could reach for.
    fn deliver(&self, delivery: Delivery, window: &mut Window, cx: &mut App);
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
    /// Does one of THIS occupant's own text inputs hold window focus right
    /// now — its open cell editor, its picker's field? Default `false`:
    /// an occupant with no input of its own (the blotter, the
    /// diagnostics tile, the placeholder) never holds the keyboard.
    ///
    /// The ownership half of the shell's insert-focus predicate
    /// (`ShellView::occupant_insert_stack`, user ruling 2026-09-17 and its
    /// review's C-1): a module reports `mode == insert` while its editor
    /// is OPEN, and the keyboard's own rule (`note_keyboard_focus_move`)
    /// deliberately leaves an editor open when tile focus moves away — so
    /// "the focused tile claims insert" and "the focused handle is this
    /// tile's" are two different facts, and the shell must read the
    /// second from the module itself. Answered off the focus handles, not
    /// off the mode: `focus_handle(cx).is_focused(window)` on each input
    /// the occupant owns.
    fn holds_focus(&self, _window: &Window, _cx: &App) -> bool {
        false
    }
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
    /// The key contexts this module's [`TileContent::key_context`] can
    /// name (`["blotter"]`).
    ///
    /// **A fragment can never shadow a shell binding or another module's**
    /// — and the rule that delivers it is stated here rather than left
    /// implicit: a fragment binding's predicate must be a plain
    /// CONJUNCTION (`ctx`, or `ctx && key == value`) whose FIRST
    /// identifier is one of these contexts, or
    /// [`crate::keymap::fragments::check_fragment`] drops it with an error
    /// diagnostic. With `&&` as the only connective, naming one of these
    /// makes the whole predicate require that context; `!`, `||` and `(`
    /// are refused anywhere in the text precisely because they break that
    /// implication (`blotter || workspace` fires everywhere,
    /// `(!blotter)` everywhere but the blotter).
    ///
    /// Defaults to the kind, which is what every module whose context and
    /// kind are the same word wants — but it is a separate answer on
    /// purpose, because the two names are genuinely independent: the
    /// market-data panel is kind `cvi` (one roster entry per document
    /// kind) and context `marketdata` (one vocabulary shared by all of
    /// them). A `Vec` rather than the `&'static [&'static str]` the plan
    /// sketched: a default body has only `self.kind()` to work with, and
    /// a `&'static` slice cannot be built from a value without leaking.
    /// Called once per factory at startup and on each reload, so the
    /// allocation is not on any hot path.
    fn contexts(&self) -> Vec<&'static str> {
        vec![self.kind()]
    }
    /// This module's default bindings, as keymap TOML (`[[bindings]]`
    /// tables only) — the module's own copy of what used to live in the
    /// shell's `BUILTIN_KEYMAP`. `None` for a module with no keys of its
    /// own (the placeholder). See [`crate::keymap::fragments`] for where
    /// it sits in the layer order and why.
    fn default_keymap(&self) -> Option<&'static str> {
        None
    }
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
///
/// There is no default kind (spec 2026-09-08 add-tile §7.1): a tile is
/// added by naming the kind it should host, and a tile nothing claims
/// paints the [`placeholder`] instead of silently becoming whichever
/// module a config key happened to name.
#[derive(Default)]
pub struct ModuleRoster {
    factories: Vec<Box<dyn ModuleFactory>>,
}

impl ModuleRoster {
    pub fn new() -> ModuleRoster {
        ModuleRoster::default()
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

    pub fn kinds(&self) -> Vec<&'static str> {
        self.factories.iter().map(|f| f.kind()).collect()
    }

    /// Every module's default bindings as keymap docs, in roster order,
    /// each already checked against its own factory's
    /// [`ModuleFactory::contexts`] — the app splices these into the layer
    /// stack with [`crate::keymap::fragments::splice`] and folds the
    /// diagnostics into the ones `build_keymap` reports.
    ///
    /// The pairing of a fragment with the contexts it is checked against
    /// happens HERE, inside the loop that reads both off the same
    /// factory, rather than at the call site: a caller collecting docs
    /// and contexts into two lists and zipping them is one reordering
    /// away from checking a fragment against another module's contexts,
    /// which would silently drop exactly the bindings it should keep.
    pub fn keymap_fragments(&self) -> (Vec<LayerDoc>, Vec<Diagnostic>) {
        let mut docs = Vec::new();
        let mut diags = Vec::new();
        for factory in &self.factories {
            let Some(text) = factory.default_keymap() else {
                continue;
            };
            match fragments::fragment_doc(factory.kind(), text) {
                Ok(doc) => {
                    let (doc, d) = fragments::check_fragment(doc, &factory.contexts());
                    docs.push(doc);
                    diags.extend(d);
                }
                Err(d) => diags.push(d),
            }
        }
        (docs, diags)
    }
}

/// The occupant of a tile nothing else claims: an unknown session kind,
/// or a pending request for a kind with no factory. Paints a hint naming
/// the palette; never a blank, never a panic.
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
                .child("ctrl+k → Add a tile")
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
        fn deliver(&self, delivery: Delivery, _: &mut Window, _: &mut App) {
            match delivery {
                Delivery::Query(_) => {}
            }
        }
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
    use gpui::{Context, FocusHandle, Focusable as _, Render, div};
    use gpui_component::input::{Input, InputState};
    use std::cell::{Cell, RefCell};
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
        /// The `FocusHandle` of the most recently created view. A test
        /// that needs to put keyboard focus INSIDE a tile without going
        /// through a mouse-down — the one route that does not re-arm
        /// `pending_focus_restore` — has no other way to reach it: the
        /// view type is private and the roster hands back `&dyn
        /// ModuleFactory`. `None` until the first `create`.
        pub last_focus: Rc<RefCell<Option<FocusHandle>>>,
        /// What [`ModuleFactory::default_keymap`] answers — the keymap
        /// fragment this factory ships. `None` by default, so every
        /// existing fixture is a module with no default bindings; a test
        /// that wants one sets it before boxing the factory.
        pub fragment: Option<&'static str>,
        /// What [`ModuleFactory::contexts`] answers, when non-empty.
        /// Empty (the default) means "just my kind", the trait's own
        /// default — so a fixture opts into a context that differs from
        /// its kind (the market-data panel's shape) by setting this, and
        /// every other fixture keeps the ordinary behaviour without
        /// naming a string twice.
        pub contexts: &'static [&'static str],
        /// The `InputState` the hosted view owns while insert mode is on,
        /// `None` otherwise — the fixture's window into the cell editor,
        /// for the same reason `last_focus` exists: the view type is
        /// private and the roster hands back `&dyn ModuleFactory`, so a
        /// test has no other way to read what typing actually landed in
        /// the input.
        ///
        /// **Cleared on commit/cancel in the same `dispatch` that drops
        /// the view's own handle**, never left holding the last clone: the
        /// entity keeps its `FocusHandle` alive, so a clone parked here
        /// would leave a live `Input` for the window to hand focus back to.
        /// The `blur` in that same arm is the half that actually makes
        /// `Window::focused` `None` — see it for why dropping is not
        /// enough on its own.
        pub input: Rc<RefCell<Option<Entity<InputState>>>>,
    }

    impl RecordingFactory {
        pub fn new(kind: &'static str) -> RecordingFactory {
            RecordingFactory {
                kind,
                log: Rc::new(RefCell::new(Vec::new())),
                completions: vec!["delta01".into(), "gamma01".into()],
                command_result: Ok(()),
                last_focus: Rc::new(RefCell::new(None)),
                fragment: None,
                contexts: &[],
                input: Rc::new(RefCell::new(None)),
            }
        }
    }

    /// Tracks its own focus handle on purpose: that is what `DataTable`
    /// does, and the shell's click-to-focus restore is tested against it.
    struct RecordingView {
        tile: TileId,
        focus: FocusHandle,
        /// The tile-owned `Input` of insert mode (market-data spec §8.6),
        /// standing in for the panel's cell editor: created and focused
        /// on `<kind>::edit`, dropped on `<kind>::commit`/`::cancel`. The
        /// VIEW holds it, not the content, because it has to be rendered
        /// for typing to reach it at all — gpui installs an input handler
        /// only for a focused, painted `Input`, and only a painted one
        /// sits on the dispatch path a keystroke bubbles up through the
        /// shell's own root listener.
        input: Option<Entity<InputState>>,
    }

    impl Render for RecordingView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .debug_selector(|| format!("tile-content-{}", self.tile.0))
                .child(format!("rec {}", self.tile.0))
                .children(self.input.as_ref().map(Input::new))
        }
    }

    struct RecordingContent {
        tile: TileId,
        log: Rc<RefCell<Vec<Recorded>>>,
        completions: Vec<String>,
        command_result: Result<(), String>,
        pub state: RefCell<toml::Table>,
        /// The view this content speaks for — `<kind>::edit` has to put an
        /// `InputState` somewhere that is PAINTED, and the view is the only
        /// thing the shell renders.
        view: Entity<RecordingView>,
        /// The insert-mode toggle (market-data spec §8.6): `true` while
        /// THIS tile's view owns a focused `InputState` — the fixture's
        /// stand-in for the panel's cell editor. [`TileContent::
        /// key_context`] reports `mode == insert` instead of `mode ==
        /// normal` while it is set, which is the whole of what the shell's
        /// insert branch keys on. Flipped in `dispatch` (`<kind>::edit`
        /// sets it, `<kind>::commit` and `<kind>::cancel` clear it), so a
        /// test drives it through a real keypress rather than poking at
        /// it. **Per tile, never per factory** (2026-09-17): a real module's
        /// editor belongs to one tile, and the shell's insert-focus
        /// predicate (`occupant_insert_stack`) reads the FOCUSED tile's
        /// context — a flag shared across a factory's tiles made a second
        /// tile claim insert mode for an editor it did not own, which is
        /// a state no real module can reach.
        insert: Cell<bool>,
        /// Shared with [`RecordingFactory::input`]; see it for what a test
        /// reads it for.
        input: Rc<RefCell<Option<Entity<InputState>>>>,
    }

    impl TileContent for RecordingContent {
        fn key_context(&self, _cx: &App) -> KeyContext {
            let mode = if self.insert.get() {
                "insert"
            } else {
                "normal"
            };
            // `counts()` stays on in insert mode deliberately: the shell's
            // insert branch, not this context, is what must stop a typed
            // `3` from becoming a count prefix (spec §8.6), and a fixture
            // that quietly dropped the flag would let a shell with no
            // branch at all pass that test.
            KeyContext::new("rec").pair("mode", mode).counts()
        }
        fn dispatch(
            &self,
            action: &ActionId,
            count: Option<u32>,
            window: &mut Window,
            cx: &mut App,
        ) -> bool {
            self.log
                .borrow_mut()
                .push(Recorded::Dispatch(self.tile, action.clone(), count));
            // Matched on the VERB, never on the whole id: this factory's
            // kind is a constructor argument, so `rec::edit` and
            // `other::edit` are the same verb to the same fixture.
            match action.0.rsplit_once("::").map(|(_, verb)| verb) {
                Some("edit") => {
                    let state = cx.new(|cx| InputState::new(window, cx));
                    let handle = state.read(cx).focus_handle(cx);
                    handle.focus(window, cx);
                    self.insert.set(true);
                    *self.input.borrow_mut() = Some(state.clone());
                    self.view.update(cx, |view, cx| {
                        view.input = Some(state);
                        cx.notify();
                    });
                }
                Some("commit") | Some("cancel") => {
                    self.insert.set(false);
                    // Give the keyboard up, then drop the input — in that
                    // order, and BOTH halves (a real panel's cell editor
                    // must do the same, market-data spec §8.6):
                    //
                    // `blur` is what the shell's dropped-focus net
                    // (`render`'s `window.focused(cx).is_none()`) is
                    // waiting for, and dropping the entity is NOT enough to
                    // produce it at this pinned gpui-component rev —
                    // `Root` registers the focused input as a strong
                    // `AnyInputState` (`input::state::sync_focused_input_
                    // registry`) and only ever unregisters it from the
                    // Input's own render, which an input removed from the
                    // tree never reaches. So the last clone would outlive
                    // this call, `Window::focused` would stay `Some`, and
                    // the net could never fire. Blurring is a module
                    // GIVING UP focus, never taking the shell's — no module
                    // touches the shell's own handle (CLAUDE.md's focus
                    // rule); the shell decides where focus lands next.
                    //
                    // Dropping both clones still matters: the fixture's own
                    // cell is a test's window into "is an editor open", and
                    // the view must stop painting the `Input` or it would
                    // simply re-focus itself on the next frame.
                    window.blur(cx);
                    *self.input.borrow_mut() = None;
                    self.view.update(cx, |view, cx| {
                        view.input = None;
                        cx.notify();
                    });
                }
                _ => {}
            }
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
        fn deliver(&self, delivery: Delivery, _: &mut Window, _: &mut App) {
            match delivery {
                Delivery::Query(outcome) => {
                    self.log
                        .borrow_mut()
                        .push(Recorded::Delivered(self.tile, outcome.tag));
                }
            }
        }
        fn set_visible(&self, visible: bool, _: &mut App) {
            self.log
                .borrow_mut()
                .push(Recorded::Visible(self.tile, visible));
        }
        fn serialize(&self, _: &App) -> toml::Table {
            self.state.borrow().clone()
        }
        /// The panel's own answer, off THIS tile's view's own input —
        /// never the factory-shared `input` cell, which holds whichever
        /// tile's editor opened last and would make a tile whose editor
        /// was abandoned (I-3) claim another tile's field as its own.
        fn holds_focus(&self, window: &Window, cx: &App) -> bool {
            self.view
                .read(cx)
                .input
                .as_ref()
                .is_some_and(|state| state.read(cx).focus_handle(cx).is_focused(window))
        }
    }

    impl ModuleFactory for RecordingFactory {
        fn kind(&self) -> &'static str {
            self.kind
        }
        fn contexts(&self) -> Vec<&'static str> {
            if self.contexts.is_empty() {
                vec![self.kind]
            } else {
                self.contexts.to_vec()
            }
        }
        fn default_keymap(&self) -> Option<&'static str> {
            self.fragment
        }
        /// `noop`, plus the four verbs a keymap fragment needs to drive
        /// insert mode through real keypresses (market-data spec §8.6):
        /// `edit` opens the tile-owned input, `commit`/`cancel` drop it,
        /// and `down` is the normal-mode motion that must NOT fire while a
        /// trader is typing. Registered here because `build_keymap` drops
        /// any binding whose action nothing registered, so a fragment
        /// naming them would otherwise compile away to nothing.
        fn register_actions(&self, registry: &mut ActionRegistry) {
            for (verb, title) in [
                ("noop", "Recording no-op"),
                ("edit", "Recording edit cell"),
                ("commit", "Recording commit edit"),
                ("cancel", "Recording cancel edit"),
                ("down", "Recording cursor down"),
            ] {
                let _ = registry.register(crate::actions::ActionDef {
                    id: ActionId(format!("{}::{verb}", self.kind)),
                    title: title.into(),
                    category: "Test".into(),
                });
            }
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
            *self.last_focus.borrow_mut() = Some(focus.clone());
            let view = cx.new(|_| RecordingView {
                tile,
                focus,
                input: None,
            });
            TileOccupant {
                kind: self.kind,
                view: view.clone().into(),
                content: Box::new(RecordingContent {
                    tile,
                    log: self.log.clone(),
                    completions: self.completions.clone(),
                    command_result: self.command_result.clone(),
                    state: RefCell::new(restored.cloned().unwrap_or_default()),
                    view,
                    insert: Cell::new(false),
                    input: self.input.clone(),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_roster_finds_factories_by_kind_and_lists_them() {
        let mut roster = ModuleRoster::new();
        assert!(roster.kinds().is_empty());
        roster.add(Box::new(recording::RecordingFactory::new("rec")));
        roster.add(Box::new(placeholder::PlaceholderFactory));
        assert_eq!(roster.kinds(), vec!["rec", "placeholder"]);
        assert_eq!(roster.factory("rec").map(|f| f.kind()), Some("rec"));
        assert!(roster.factory("nonesuch").is_none());
    }

    #[test]
    fn registering_actions_delegates_to_every_factory_once() {
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(recording::RecordingFactory::new("rec")));
        let mut registry = ActionRegistry::default();
        roster.register_actions(&mut registry);
        assert!(registry.contains(&ActionId("rec::noop".into())));
        assert!(registry.contains(&ActionId("rec::edit".into())));
        // Registering twice is the roster's caller's mistake, and the
        // registry says so rather than silently duplicating: one entry per
        // verb the factory registers (`noop` plus the four insert-mode
        // verbs), not two sets of them.
        let mut again = ActionRegistry::default();
        roster.register_actions(&mut again);
        assert_eq!(again.iter().count(), 5);
    }

    /// One doc per factory that ships a fragment, in roster order, each
    /// already through `check_fragment` — the roster is where the app
    /// collects them, so this is the one place that pairing can be got
    /// wrong (a fragment checked against the wrong factory's contexts
    /// would drop exactly the bindings it should keep).
    #[test]
    fn keymap_fragments_are_one_checked_doc_per_factory_that_ships_one() {
        let mut first = recording::RecordingFactory::new("rec");
        first.fragment =
            Some("[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"q\" = \"rec::noop\"\n");
        let mut second = recording::RecordingFactory::new("other");
        // Declares a context that is NOT its kind (the market-data
        // panel's own shape: kind `cvi`, context `marketdata`), so a
        // roster that checked against `kind()` would drop this binding.
        second.contexts = &["othercontext"];
        second.fragment = Some(
            "[[bindings]]\ncontext = \"othercontext\"\n[bindings.keys]\n\"q\" = \"other::noop\"\n",
        );
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(first));
        roster.add(Box::new(second));
        // Ships no fragment at all: contributes no doc, not an empty one.
        roster.add(Box::new(placeholder::PlaceholderFactory));

        let (docs, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        let files: Vec<String> = docs
            .iter()
            .map(|d| d.file.to_string_lossy().to_string())
            .collect();
        assert_eq!(files, vec!["<module:rec>", "<module:other>"]);
        for doc in &docs {
            assert_eq!(doc.name, "keymap");
            assert_eq!(doc.layer, geode_core::config::Layer::Builtin);
            assert_eq!(doc.table["bindings"].as_array().unwrap().len(), 1);
        }
    }

    /// The check really runs here: a fragment binding outside its own
    /// factory's contexts is dropped with an error diagnostic by the
    /// roster, not merely by whoever remembers to call `check_fragment`.
    #[test]
    fn a_factorys_fragment_is_checked_against_its_own_contexts() {
        let mut factory = recording::RecordingFactory::new("rec");
        factory.fragment = Some(
            "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"q\" = \"rec::noop\"\n\n[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"ctrl+q\" = \"rec::noop\"\n",
        );
        let mut roster = ModuleRoster::new();
        roster.add(Box::new(factory));
        let (docs, diags) = roster.keymap_fragments();
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs[0].table["bindings"].as_array().unwrap().len(),
            1,
            "the workspace binding must be gone: {:?}",
            docs[0].table
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, geode_core::config::Severity::Error);
        assert!(diags[0].message.contains("workspace"), "{}", diags[0]);
    }

    /// The default `contexts()` is the factory's kind, so the common case
    /// (a module whose context and kind are the same word) ships a
    /// fragment with nothing extra to declare.
    #[test]
    fn a_factory_declares_its_kind_as_its_context_by_default() {
        let factory = recording::RecordingFactory::new("rec");
        assert_eq!(factory.contexts(), vec!["rec"]);
        assert_eq!(
            placeholder::PlaceholderFactory.contexts(),
            vec!["placeholder"]
        );
        assert_eq!(placeholder::PlaceholderFactory.default_keymap(), None);
    }
}
