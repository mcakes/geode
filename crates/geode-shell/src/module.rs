//! The contract between the shell and hosted modules. A [`TileOccupant`]
//! contains the view and content for a `TileId`, created by a [`ModuleFactory`]
//! registered in the app's [`ModuleRoster`].
//!
//! Factories receive shell-side handles: tile id, frame, and diagnostics.
//! A factory needing data carries its own handle, supplied by `geode-app`.
//! This crate does not depend on `geode-data`; asynchronous outcomes cross
//! through [`Delivery`]. Keyed outcomes go to their tile, while keyless
//! outcomes go to every visible occupant.

use crate::actions::{ActionId, ActionRegistry};
use crate::diagnostics::Diagnostics;
use crate::frame::Frame;
use crate::keymap::KeyContext;
use crate::keymap::fragments;
use crate::shell::control::{self, PointerStates as _};
use crate::tiling::TileId;
use geode_core::config::{Diagnostic, LayerDoc};
use geode_core::launch::{ContextField, LaunchContext};
use geode_core::pricing::PriceOutcome;
use geode_core::query::{QueryKey, QueryOutcome};
use geode_core::series::SeriesOutcome;
use gpui::{AnyView, App, Entity, SharedString, Window};
use std::rc::Rc;

/// Changes, commits, and cancellation from the tile's `/` find line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindEvent {
    Changed(String),
    Committed(String),
    Cancelled,
}

/// An asynchronous outcome routed by the shell. A keyed delivery goes to
/// its tile; a keyless delivery goes to every visible tile. This is an enum
/// so a new outcome forces every occupant to make an explicit compile-time
/// decision. Matches must remain exhaustive and avoid wildcard arms.
#[derive(Debug)]
pub enum Delivery {
    Query(QueryOutcome),
    Price(PriceOutcome),
    /// A series query's answer, routed by tile key like a `Query`.
    Series(SeriesOutcome),
    /// A fetch finished. Keyed by the
    /// `(identity, source)` pair, not a tile: `ShellView::deliver` hands
    /// one to EVERY visible occupant, each its own copy, and a tile
    /// holding the pair requeries on `Ok` (an `Ok(0)` too — the span is
    /// covered, whether just now or already) or marks the slot on `Err`.
    /// A tile holding nothing of the kind ignores it. Plain strings so
    /// the shell, which never names `geode-data`, can carry it.
    SeriesFetched {
        source: String,
        identity: String,
        result: Result<u64, String>,
    },
    /// Document-upload outcome routed to the submitting tile. Plain fields keep
    /// the shell independent of `geode-data`; the market-data tile checks the
    /// upload tag before changing draft state or reporting the result.
    Upload(UploadDelivery),
}

/// [`Delivery::Upload`]'s fields, mirroring `geode_data::egress::
/// UploadOutcome` one for one.
#[derive(Debug)]
pub struct UploadDelivery {
    pub key: QueryKey,
    /// The tile's upload counter, echoed back so a stale outcome (an
    /// earlier upload from the same tile, still in flight when a second
    /// one was sent) can be told from the current one.
    pub tag: u64,
    pub target: String,
    pub result: Result<(), String>,
}

impl Delivery {
    /// The tile id (as a bare `QueryKey`) this delivery is addressed to,
    /// or `None` for one addressed to every visible tile.
    /// `ShellView::deliver` matches the variant first — the key-less
    /// `SeriesFetched` is broadcast — and routes the keyed ones on this.
    pub fn key(&self) -> Option<QueryKey> {
        match self {
            Delivery::Query(outcome) => Some(outcome.key),
            Delivery::Price(outcome) => Some(outcome.key),
            Delivery::Series(outcome) => Some(outcome.key),
            Delivery::SeriesFetched { .. } => None,
            Delivery::Upload(u) => Some(u.key),
        }
    }
}

/// The shape of [`StackHandle`]'s own `open` closure, named for the same
/// reason `shell::dialog::StepHandler` and its siblings are: a bare
/// `Rc<dyn Fn(&mut Window, &mut App)>` field trips `clippy::type_
/// complexity`.
type OpenStackList = Rc<dyn Fn(&mut Window, &mut App)>;

/// What the shell hands a stack member: its
/// one-based `index` and the stack's `len`, `text` prepared once
/// (`"2/4"`) so no module formats it per frame, and `open_list`, a
/// closure over the shell's own weak entity, so a module opens the
/// shell's list without a path to `ShellView`.
#[derive(Clone)]
pub struct StackHandle {
    pub index: usize,
    pub len: usize,
    pub text: SharedString,
    open: OpenStackList,
}

impl StackHandle {
    pub fn new(
        index: usize,
        len: usize,
        open: impl Fn(&mut Window, &mut App) + 'static,
    ) -> StackHandle {
        StackHandle {
            index,
            len,
            text: format!("{index}/{len}").into(),
            open: Rc::new(open),
        }
    }

    /// Open the shell's transient member list on this tile.
    pub fn open_list(&self, window: &mut Window, cx: &mut App) {
        (self.open)(window, cx)
    }

    /// The marker chip every module paints first in its header strip:
    /// `2/4` in the mono face, `Tone::Neutral` through
    /// `chip_paint` (a state the trader chose, like `pinned`), the
    /// theme's small radius, id `("stack-marker", tile)`, selector
    /// `stack-marker-{tile}`, and a mouse-down that stops propagation and
    /// opens the list. `None` while the stack has one member or fewer,
    /// so the gate lives here and not at four call sites: a caller
    /// writes `.children(stack.as_ref().and_then(|s| s.marker(theme,
    /// tile)))` and gets the same chip the other modules paint. The
    /// text is `self.text`, prepared once — nothing here formats per
    /// frame.
    pub fn marker(
        &self,
        theme: &gpui_component::Theme,
        tile: TileId,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        use gpui::prelude::*;
        if self.len <= 1 {
            return None;
        }
        let neutral = crate::shell::chip::chip_paint(theme, crate::shell::chip::Tone::Neutral);
        let open = self.clone();
        Some(
            gpui::div()
                .id(gpui::ElementId::NamedInteger(
                    SharedString::new_static("stack-marker"),
                    tile.0,
                ))
                .text_color(neutral.text)
                .when_some(neutral.fill, |el, fill| el.bg(fill))
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .font_family(crate::fonts::MONO)
                .debug_selector(move || format!("stack-marker-{}", tile.0))
                .child(self.text.clone())
                // A clickable chip on the tile surface: pointer states
                // (hover, pressed) through the control door
                // (`control::for_chip`), the affordance rule every
                // clickable chrome follows.
                .pointer_states(control::for_chip(theme, &neutral, theme.background))
                .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    open.open_list(window, cx);
                }),
        )
    }
}

impl std::fmt::Debug for StackHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StackHandle({}/{})", self.index, self.len)
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
    /// Execute a tile-local `:` command, without the colon. `Err` is displayed
    /// inline. Commands may change this tile's query, presentation, cursor, or
    /// draft; shared frame, configuration, shell, and log-level changes belong
    /// to palette actions.
    ///
    /// Modules with a command vocabulary test that every command leaves shared
    /// frame counters, pending persistence, and diagnostics requests untouched.
    /// Unsupported shared-state commands return an error naming the appropriate
    /// palette action.
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String>;
    /// Return the unfiltered vocabulary for the word under `cursor` on a
    /// `:` line. Each candidate is a bare word, such as `ingest`, because the
    /// shell replaces only that word through `commandline::accept`. A full-line
    /// candidate would duplicate the surrounding words.
    ///
    /// The shell ranks candidates and rejects an ambiguous partial word on
    /// Enter instead of choosing arbitrarily.
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String>;
    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App);
    /// Handle an asynchronous outcome routed to this tile. Match [`Delivery`]
    /// exhaustively so adding an outcome requires every occupant to handle or
    /// explicitly ignore it.
    fn deliver(&self, delivery: Delivery, window: &mut Window, cx: &mut App);
    /// Set whether this tile is visible. Hidden tiles may drop subscriptions;
    /// shown tiles requery if stale.
    ///
    /// A fresh occupant must assume it is hidden until the first call.
    /// `ShellView::ensure_occupants` creates occupants across workspaces and
    /// docks, then announces visibility on the first render after creation.
    /// Subscribing before that announcement would retain resources for tiles
    /// that may never appear on screen.
    fn set_visible(&self, visible: bool, cx: &mut App);
    /// Set this tile's stack position, or `None` outside a stack.
    /// `ShellView::ensure_occupants` calls this after creation and whenever
    /// `(index, len)` changes. Modules paint the marker first in their header
    /// when `len > 1` and open the member list when it is clicked.
    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App);
    /// The row this tile paints as in the stack list: the
    /// same words its own header leads with (`risk · book, lhu`,
    /// `CVI · SPX.Z`, `diagnostics · log`).
    fn title(&self, cx: &App) -> SharedString;
    /// State for `session.toml`; stored opaquely by the shell.
    fn serialize(&self, cx: &App) -> toml::Table;
    /// Whether an input owned by this occupant currently holds window focus.
    /// The default is `false` for occupants without text inputs.
    ///
    /// An open editor may report insert mode while focus belongs to another
    /// tile. The shell's `occupant_insert_stack` therefore needs this ownership
    /// check as well as the mode. Implement it by testing each owned input's
    /// `focus_handle(cx).is_focused(window)`, not by reading the editor mode.
    fn holds_focus(&self, _window: &Window, _cx: &App) -> bool {
        false
    }
    /// The context at this tile's cursor, for `tile::open_with`. Pulled by
    /// the shell when the action runs, so a module needs no handle into the
    /// shell. Empty (the default) whenever the cursor names no single
    /// value; the shell then opens the plain tile-kind picker.
    fn launch_context(&self, _cx: &App) -> LaunchContext {
        LaunchContext::default()
    }
    /// Called once, deferred after the first render, for an occupant that
    /// `ShellView::add_tile` created (not a session restore) and that is the
    /// focused tile on that render. A module that is useless without some
    /// state asks for it here; the default does nothing.
    fn launched(&self, _window: &mut Window, _cx: &mut App) {}
    /// Fit every column of this tile's table to its content, or with
    /// `reset` drop the fitted widths and return to the configured ones.
    /// The shell's `tile::autosize_columns` action calls this on the
    /// focused tile; a table module's own `:autosize [reset]` command runs
    /// the same code. Measure on the UI thread here, never in render (see
    /// [`crate::colfit`]), and read the rem from `window`.
    ///
    /// `Err` is a refusal the shell paints as a status notice. The default
    /// refuses with [`crate::colfit::NO_TABLE`], so an occupant without a
    /// table needs no override. A table with no rows to measure refuses a
    /// fit with [`crate::colfit::NOTHING_TO_FIT`] and keeps its widths; a
    /// `reset` never refuses.
    fn autosize_columns(
        &self,
        _reset: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Result<(), &'static str> {
        Err(crate::colfit::NO_TABLE)
    }
    /// Expose the last stack handle to hosting tests. Content is stored as
    /// `Box<dyn TileContent>`, so tests cannot access the concrete occupant's
    /// fields. Defaults to `None`; [`recording::RecordingContent`] returns its
    /// stored handle.
    #[cfg(any(test, feature = "test-support"))]
    fn stack_handle_for_test(&self) -> Option<StackHandle> {
        None
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
    /// The contexts this module's [`TileContent::key_context`] can name.
    /// Defaults to its kind; override when several kinds share a vocabulary,
    /// such as market-data kinds using the `marketdata` context.
    ///
    /// [`crate::keymap::fragments::check_fragment`] requires each default
    /// binding's predicate to be a plain conjunction beginning with one of
    /// these contexts. `!`, `||`, and parentheses are rejected because they
    /// could let the binding match outside the module's context.
    ///
    /// Returning a `Vec` lets the default use `self.kind()` without leaking a
    /// static slice. The app validates fragments at startup; reload reuses
    /// those validated documents without calling the factories again.
    fn contexts(&self) -> Vec<&'static str> {
        vec![self.kind()]
    }
    /// Default bindings as keymap TOML containing only `[[bindings]]` tables.
    /// `None` for a module without its own bindings. See
    /// [`crate::keymap::fragments`] for validation and layer order.
    fn default_keymap(&self) -> Option<&'static str> {
        None
    }
    /// Context fields this kind can open on. Empty (the default) keeps the
    /// kind out of `tile::open_with`'s list.
    fn accepts(&self) -> &'static [ContextField] {
        &[]
    }
    /// Translate a launch context into the table [`Self::create`] reads as
    /// its restored record. The factory owns the translation so the shell
    /// never learns a module's state format. `None` (the default) creates
    /// the tile as a plain add would.
    fn launch_state(&self, _ctx: &LaunchContext) -> Option<toml::Table> {
        None
    }
    /// Build an occupant for `tile`, optionally restoring its opaque state.
    /// The occupant must assume it is hidden until the shell calls
    /// [`TileContent::set_visible`] on the first render after creation. Defer
    /// screen-dependent subscriptions and queries until that announcement.
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

/// A shared factory registers as itself. The app keeps an `Rc` of each
/// factory for its reload handlers and hands the roster a clone.
///
/// Every trait method is forwarded, defaulted ones included. A forwarder
/// that inherits a default answers for itself instead of the factory it
/// wraps: a missing `contexts` drops the module's whole keymap, and a
/// missing `accepts` leaves `tile::open_with` with no kind to offer. Add
/// every new `ModuleFactory` method here too.
impl<F: ModuleFactory + ?Sized> ModuleFactory for Rc<F> {
    fn kind(&self) -> &'static str {
        (**self).kind()
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        (**self).register_actions(registry)
    }
    fn contexts(&self) -> Vec<&'static str> {
        (**self).contexts()
    }
    fn default_keymap(&self) -> Option<&'static str> {
        (**self).default_keymap()
    }
    fn accepts(&self) -> &'static [ContextField] {
        (**self).accepts()
    }
    fn launch_state(&self, ctx: &LaunchContext) -> Option<toml::Table> {
        (**self).launch_state(ctx)
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
        (**self).create(tile, restored, frame, diagnostics, window, cx)
    }
}

/// The app's registered module factories. Adding a tile requires an
/// explicit kind. If no factory claims that kind, the shell paints a
/// [`placeholder`] with a recovery hint instead of selecting another module.
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

/// The content contract for a page: a surface that replaces the workspace
/// (toolbar, tile surface, command line) while open. Pages own their own
/// inputs, have no `:` line, and receive no deliveries; they persist through
/// `[pages.<kind>]` rather than the layout tree.
pub trait PageContent {
    /// Pushed innermost on the key context stack while the page is open.
    /// Carries `mode = insert` while one of the page's inputs is focused.
    fn key_context(&self, cx: &App) -> KeyContext;
    /// An action the shell did not recognise. `true` if handled. For
    /// `page::close`, `true` means the page consumed the close (it had
    /// something of its own to dismiss) and the shell must not close it.
    fn dispatch(
        &self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool;
    /// Opening announces `true`; closing `false`. A fresh occupant assumes it
    /// is hidden until the first call.
    fn set_visible(&self, visible: bool, cx: &mut App);
    /// The handle the shell focuses on open; the page view tracks it.
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle;
    /// `true` while one of the page's own text inputs owns keyboard focus,
    /// the same contract as [`TileContent::holds_focus`]: the shell then
    /// routes bare keys to the input and only chords to the keymap.
    fn holds_focus(&self, window: &Window, cx: &App) -> bool;
    fn title(&self, cx: &App) -> SharedString;
    /// Opaque state for `[pages.<kind>]` in the session file.
    fn serialize(&self, cx: &App) -> toml::Table;
}

pub struct PageOccupant {
    pub kind: &'static str,
    pub view: AnyView,
    pub content: Box<dyn PageContent>,
}

/// Dispatches a registered shell action on the page's behalf. The shell
/// builds it from its own weak entity; a page never holds `ShellView`.
pub type ShellActions = Rc<dyn Fn(&ActionId, &mut Window, &mut App)>;

pub trait PageFactory {
    fn kind(&self) -> &'static str;
    /// Sidebar tooltip and palette row text, e.g. "Diagnostics".
    fn title(&self) -> &'static str;
    /// Sidebar glyph. Catalog icons outside gpui-component's default set
    /// must be listed in the app's `ExtraIcons`.
    fn icon(&self) -> gpui_kit_assets::IconName;
    /// Runs once, before the keymap builds.
    fn register_actions(&self, registry: &mut ActionRegistry);
    /// The contexts this page's [`PageContent::key_context`] can name. The
    /// fragment checker requires each default binding's predicate to begin
    /// with one of these.
    fn contexts(&self) -> Vec<&'static str> {
        vec![self.kind()]
    }
    /// Default bindings as keymap TOML containing only `[[bindings]]` tables.
    fn default_keymap(&self) -> Option<&'static str> {
        None
    }
    /// A context-free default binding for `page::toggle_<kind>`, such as
    /// `"mod+d"`. The roster emits it as a shell-generated doc; a module
    /// fragment cannot carry a context-free binding.
    fn toggle_binding(&self) -> Option<&'static str> {
        None
    }
    /// Build the page, optionally restoring its `[pages.<kind>]` state. The
    /// occupant must assume it is hidden until [`PageContent::set_visible`].
    fn create(
        &self,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        actions: ShellActions,
        window: &mut Window,
        cx: &mut App,
    ) -> PageOccupant;
}

/// Every trait method is forwarded, defaulted ones included, for the same
/// reason the `ModuleFactory` forwarder does: a forwarder inheriting a
/// default answers for itself, not the factory it wraps.
impl<F: PageFactory + ?Sized> PageFactory for Rc<F> {
    fn kind(&self) -> &'static str {
        (**self).kind()
    }
    fn title(&self) -> &'static str {
        (**self).title()
    }
    fn icon(&self) -> gpui_kit_assets::IconName {
        (**self).icon()
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        (**self).register_actions(registry)
    }
    fn contexts(&self) -> Vec<&'static str> {
        (**self).contexts()
    }
    fn default_keymap(&self) -> Option<&'static str> {
        (**self).default_keymap()
    }
    fn toggle_binding(&self) -> Option<&'static str> {
        (**self).toggle_binding()
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
        (**self).create(restored, frame, diagnostics, actions, window, cx)
    }
}

/// What the sidebar needs to paint one page button. The strings are leaked
/// once at [`PageRoster::add`] so the sidebar formats nothing per render.
#[derive(Debug, Clone, Copy)]
pub struct PageEntry {
    pub kind: &'static str,
    pub title: &'static str,
    pub icon: gpui_kit_assets::IconName,
    /// `page::toggle_<kind>`, the action the button dispatches.
    pub toggle_action: &'static str,
    /// `sidebar-page-<kind>`, the button's element id and debug selector.
    pub selector: &'static str,
    /// `tip-sidebar-page-<kind>`, the button's tooltip selector.
    pub tip_selector: &'static str,
}

/// A registered page factory with its sidebar strings, leaked once at
/// [`PageRoster::add`] so [`PageRoster::entries`] never allocates: the
/// sidebar paints from it on every render.
struct RegisteredPage {
    factory: Box<dyn PageFactory>,
    toggle_action: &'static str,
    selector: &'static str,
    tip_selector: &'static str,
}

/// The app's registered page factories, in sidebar order.
#[derive(Default)]
pub struct PageRoster {
    pages: Vec<RegisteredPage>,
}

impl PageRoster {
    pub fn new() -> PageRoster {
        PageRoster::default()
    }

    pub fn add(&mut self, factory: Box<dyn PageFactory>) {
        let kind = factory.kind();
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        self.pages.push(RegisteredPage {
            factory,
            toggle_action: leak(format!("page::toggle_{kind}")),
            selector: leak(format!("sidebar-page-{kind}")),
            tip_selector: leak(format!("tip-sidebar-page-{kind}")),
        });
    }

    pub fn factory(&self, kind: &str) -> Option<&dyn PageFactory> {
        self.pages
            .iter()
            .find(|p| p.factory.kind() == kind)
            .map(|p| p.factory.as_ref())
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.pages.iter().map(|p| p.factory.kind()).collect()
    }

    pub fn entries(&self) -> impl Iterator<Item = PageEntry> + '_ {
        self.pages.iter().map(|p| PageEntry {
            kind: p.factory.kind(),
            title: p.factory.title(),
            icon: p.factory.icon(),
            toggle_action: p.toggle_action,
            selector: p.selector,
            tip_selector: p.tip_selector,
        })
    }

    pub fn register_actions(&self, registry: &mut ActionRegistry) {
        for p in &self.pages {
            p.factory.register_actions(registry);
        }
    }

    /// Each factory's default keymap checked against its own contexts (the
    /// pairing happens here, as in `ModuleRoster::keymap_fragments`), plus
    /// one shell-generated, unchecked doc per `toggle_binding` named
    /// `<page:kind>` — the shell wrote it, so the module checker's context
    /// rule does not apply.
    pub fn keymap_fragments(&self) -> (Vec<LayerDoc>, Vec<Diagnostic>) {
        let mut docs = Vec::new();
        let mut diags = Vec::new();
        for p in &self.pages {
            let factory = &p.factory;
            if let Some(text) = factory.default_keymap() {
                match fragments::fragment_doc(factory.kind(), text) {
                    Ok(doc) => {
                        let (doc, d) = fragments::check_fragment(doc, &factory.contexts());
                        docs.push(doc);
                        diags.extend(d);
                    }
                    Err(d) => diags.push(d),
                }
            }
            if let Some(key) = factory.toggle_binding() {
                // `{key:?}` prints the key double-quoted and escaped, which
                // is a valid TOML key.
                let text = format!(
                    "[[bindings]]\n[bindings.keys]\n{key:?} = {:?}\n",
                    p.toggle_action
                );
                match fragments::fragment_doc(&format!("page:{}", factory.kind()), &text) {
                    Ok(doc) => docs.push(doc),
                    Err(d) => diags.push(d),
                }
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
    use gpui_component::{ActiveTheme as _, v_flex};

    /// The kind used by the placeholder factory and occupant. Shell filters
    /// use this constant to recognize placeholders, keeping their identity
    /// consistent with the factory that creates them.
    pub const PLACEHOLDER_KIND: &str = "placeholder";

    pub struct PlaceholderFactory;

    struct PlaceholderView {
        tile: TileId,
        stack: Option<StackHandle>,
    }

    impl Render for PlaceholderView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme();
            // The marker rides above the hint through the one builder
            // every module uses (`StackHandle::marker`).
            let content = v_flex()
                .items_center()
                .justify_center()
                .gap_1()
                .children(self.stack.as_ref().and_then(|s| s.marker(theme, self.tile)))
                .child(crate::shell::kbd::marked(
                    "double-click or `ctrl+k` → Add a tile",
                ));
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .debug_selector(|| format!("tile-content-{}", self.tile.0))
                .child(content)
        }
    }

    struct PlaceholderContent {
        view: Entity<PlaceholderView>,
    }

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
                // This tile has no module; nothing is ever addressed here.
                Delivery::Query(_) => {}
                // This tile has no module; nothing is ever addressed here.
                Delivery::Price(_) => {}
                // This tile asks no series query and holds no
                // `(identity, source)` pair.
                Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}
                // This tile has no module; nothing is ever addressed here.
                Delivery::Upload(_) => {}
            }
        }
        fn set_visible(&self, _: bool, _: &mut App) {}
        fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
            self.view.update(cx, |v, cx| {
                v.stack = stack;
                cx.notify();
            });
        }
        fn title(&self, _: &App) -> SharedString {
            SharedString::new_static("empty")
        }
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
            let view = cx.new(|_| PlaceholderView { tile, stack: None });
            TileOccupant {
                kind: PLACEHOLDER_KIND,
                view: view.clone().into(),
                content: Box::new(PlaceholderContent { view }),
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
    use gpui_component::input::{Input, InputEvent, InputState};
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
        Priced(TileId, u64),
        /// A key-less [`Delivery::SeriesFetched`] this tile was handed,
        /// as `"{identity}@{source}"` — the pair spelled the way a
        /// timeseries slot names it, so a broadcast test can assert on
        /// WHICH fetch each tile heard about, not merely that one
        /// arrived.
        SeriesFetched(TileId, String),
        Stack(TileId, Option<(usize, usize)>),
        Launched(TileId),
        /// `autosize_columns(reset)` reached this tile.
        Autosize(TileId, bool),
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
        /// What every occupant this factory creates answers from
        /// `launch_context`. Shared and mutable so a test can change the
        /// source's context AFTER `tile::open_with` has opened its dialog,
        /// and so prove the shell captured it at open.
        pub launch_context: Rc<RefCell<LaunchContext>>,
        /// What `accepts` answers. Empty by default, so every existing
        /// fixture stays out of `tile::open_with`'s list.
        pub accepts: &'static [ContextField],
        /// When set, `launched` opens the insert-mode input exactly as
        /// `<kind>::edit` does — the stand-in for a panel that opens its own
        /// picker when launched, so a shell test can prove the input still
        /// holds the keyboard after the modal's focus return and the next
        /// frame's focus restore.
        pub edit_on_launch: bool,
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
                launch_context: Rc::new(RefCell::new(LaunchContext::default())),
                accepts: &[],
                edit_on_launch: false,
            }
        }
    }

    /// Tracks its own focus handle on purpose: that is what `DataTable`
    /// does, and the shell's click-to-focus restore is tested against it.
    struct RecordingView {
        tile: TileId,
        focus: FocusHandle,
        /// The tile-owned `Input` of insert mode,
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
        /// Whether this tile owns an open editor. `key_context` reports insert
        /// mode while set; `dispatch` opens it on `<kind>::edit` and closes it
        /// on `<kind>::commit` or `<kind>::cancel`.
        ///
        /// Keep this flag per tile: the shell inspects the focused tile's context,
        /// and another tile's editor must not put it into insert mode. Input focus
        /// is checked separately by `holds_focus`.
        insert: Cell<bool>,
        /// Shared with [`RecordingFactory::input`]; see it for what a test
        /// reads it for.
        input: Rc<RefCell<Option<Entity<InputState>>>>,
        /// What the shell last told this tile about its stack membership
        /// — a test's window into `set_stack`, since the field itself is
        /// only ever written by the trait method.
        pub stack: RefCell<Option<StackHandle>>,
        /// Shared with [`RecordingFactory::launch_context`]; see it for why
        /// it is mutable after creation.
        launch_context: Rc<RefCell<LaunchContext>>,
        /// Shared with [`RecordingFactory::edit_on_launch`]; see it for what
        /// `launched` does with it.
        edit_on_launch: bool,
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
            // `3` from becoming a count prefix, and a fixture
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
                    // must do the same):
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
                Delivery::Price(outcome) => {
                    self.log
                        .borrow_mut()
                        .push(Recorded::Priced(self.tile, outcome.tag));
                }
                // A series outcome is routed by key exactly as a query's
                // is, so it is recorded the same way — the tag is what
                // tells the two apart at the call site.
                Delivery::Series(outcome) => {
                    self.log
                        .borrow_mut()
                        .push(Recorded::Delivered(self.tile, outcome.tag));
                }
                Delivery::SeriesFetched {
                    source, identity, ..
                } => {
                    self.log.borrow_mut().push(Recorded::SeriesFetched(
                        self.tile,
                        format!("{identity}@{source}"),
                    ));
                }
                // Recorded the same way a `Query`/`Series` outcome is:
                // the tag is what a test tells them apart by.
                Delivery::Upload(u) => {
                    self.log
                        .borrow_mut()
                        .push(Recorded::Delivered(self.tile, u.tag));
                }
            }
        }
        fn set_visible(&self, visible: bool, _: &mut App) {
            self.log
                .borrow_mut()
                .push(Recorded::Visible(self.tile, visible));
        }
        fn set_stack(&self, stack: Option<StackHandle>, _: &mut App) {
            self.log.borrow_mut().push(Recorded::Stack(
                self.tile,
                stack.as_ref().map(|s| (s.index, s.len)),
            ));
            *self.stack.borrow_mut() = stack;
        }
        fn title(&self, _: &App) -> SharedString {
            format!("rec {}", self.tile.0).into()
        }
        fn serialize(&self, _: &App) -> toml::Table {
            self.state.borrow().clone()
        }
        /// Check this tile's input. The factory-shared input cell points to the
        /// most recently opened editor and could belong to another tile.
        fn holds_focus(&self, window: &Window, cx: &App) -> bool {
            self.view
                .read(cx)
                .input
                .as_ref()
                .is_some_and(|state| state.read(cx).focus_handle(cx).is_focused(window))
        }
        fn stack_handle_for_test(&self) -> Option<StackHandle> {
            self.stack.borrow().clone()
        }
        fn launch_context(&self, _cx: &App) -> LaunchContext {
            self.launch_context.borrow().clone()
        }
        /// Recorded and accepted: the fixture stands in for a table
        /// module. The trait default's refusal is tested on the
        /// placeholder occupant, which does not override it.
        fn autosize_columns(
            &self,
            reset: bool,
            _: &mut Window,
            _: &mut App,
        ) -> Result<(), &'static str> {
            self.log
                .borrow_mut()
                .push(Recorded::Autosize(self.tile, reset));
            Ok(())
        }
        fn launched(&self, window: &mut Window, cx: &mut App) {
            self.log.borrow_mut().push(Recorded::Launched(self.tile));
            if self.edit_on_launch {
                // The verb is what `dispatch` matches; the kind is irrelevant.
                self.dispatch(&ActionId("launch::edit".into()), None, window, cx);
            }
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
        fn accepts(&self) -> &'static [ContextField] {
            self.accepts
        }
        /// `{ underlying = ["<u>"] }`, the market-data panel's own shape,
        /// when this fixture accepts the underlying field.
        fn launch_state(&self, ctx: &LaunchContext) -> Option<toml::Table> {
            if !self.accepts.contains(&ContextField::Underlying) {
                return None;
            }
            let u = ctx.underlying.clone()?;
            let mut t = toml::Table::new();
            t.insert(
                "underlying".into(),
                toml::Value::Array(vec![toml::Value::String(u)]),
            );
            Some(t)
        }
        /// `noop`, plus the four verbs a keymap fragment needs to drive
        /// insert mode through real keypresses:
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
                    stack: RefCell::new(None),
                    launch_context: self.launch_context.clone(),
                    edit_on_launch: self.edit_on_launch,
                }),
            }
        }
    }

    /// What a recording page saw, in order.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PageRecorded {
        Created,
        Visible(bool),
        Action(String),
    }

    /// The recording page's view. Public so a test can `update` it and
    /// invoke the page's `ShellActions` handle from inside, the way a real
    /// page's own handler would.
    pub struct RecordingPageView {
        focus_handle: FocusHandle,
        kind: &'static str,
        input: Entity<InputState>,
        /// The input holds focus, tracked from its `Focus`/`Blur` events
        /// as the diagnostics page tracks its own: `key_context` is asked
        /// without a `Window`, so the flag is what carries `mode`.
        insert: bool,
    }

    impl Render for RecordingPageView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let kind = self.kind;
            div()
                .size_full()
                .track_focus(&self.focus_handle)
                .debug_selector(move || format!("page-{kind}"))
                .child(Input::new(&self.input))
        }
    }

    struct RecordingPageContent {
        view: Entity<RecordingPageView>,
        log: Rc<RefCell<Vec<PageRecorded>>>,
        consume_close: Rc<Cell<bool>>,
        serialized: Rc<RefCell<toml::Table>>,
    }

    impl PageContent for RecordingPageContent {
        /// `mode = insert` while the page's input holds focus, as a real
        /// page reports it: the shell's insert route keeps every context
        /// carrying `mode == insert` for bare keys, so a page fragment
        /// without a `mode == normal` clause would fire inside the input.
        fn key_context(&self, cx: &App) -> KeyContext {
            let view = self.view.read(cx);
            let mode = if view.insert { "insert" } else { "normal" };
            KeyContext::new(view.kind).pair("mode", mode)
        }
        fn dispatch(
            &self,
            action: &ActionId,
            _count: Option<u32>,
            _window: &mut Window,
            cx: &mut App,
        ) -> bool {
            // Read the view first, as a real page's handler would: a
            // `ShellActions` call made from inside the view's `update` and
            // dispatched synchronously would re-enter here and panic on the
            // double lease. The shell defers the handle for that reason.
            let _kind = self.view.read(cx).kind;
            self.log
                .borrow_mut()
                .push(PageRecorded::Action(action.0.clone()));
            action.0 == "page::close" && self.consume_close.replace(false)
        }
        fn set_visible(&self, visible: bool, _cx: &mut App) {
            self.log.borrow_mut().push(PageRecorded::Visible(visible));
        }
        fn focus_handle(&self, cx: &App) -> FocusHandle {
            self.view.read(cx).focus_handle.clone()
        }
        fn holds_focus(&self, window: &Window, cx: &App) -> bool {
            self.view
                .read(cx)
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        }
        fn title(&self, cx: &App) -> SharedString {
            SharedString::from(self.view.read(cx).kind)
        }
        fn serialize(&self, _cx: &App) -> toml::Table {
            self.serialized.borrow().clone()
        }
    }

    /// A page factory for shell tests: records lifecycle and actions, paints
    /// one real `Input` so `holds_focus` can be exercised, can be told to
    /// consume the next `page::close` (standing in for a page with a
    /// dismissable surface of its own), serializes a table the test can
    /// change, records the table `create` was handed, and keeps the
    /// `ShellActions` handle and the view it was created with.
    pub struct RecordingPageFactory {
        kind: &'static str,
        title: &'static str,
        log: Rc<RefCell<Vec<PageRecorded>>>,
        consume_close: Rc<Cell<bool>>,
        toggle_binding: Option<&'static str>,
        created_input: Rc<RefCell<Option<Entity<InputState>>>>,
        created_view: Rc<RefCell<Option<Entity<RecordingPageView>>>>,
        actions: Rc<RefCell<Option<ShellActions>>>,
        serialized: Rc<RefCell<toml::Table>>,
        restored: Rc<RefCell<Option<toml::Table>>>,
    }

    impl RecordingPageFactory {
        pub fn new(kind: &'static str) -> RecordingPageFactory {
            let mut serialized = toml::Table::new();
            serialized.insert("recorded".into(), toml::Value::Boolean(true));
            RecordingPageFactory {
                kind,
                title: Box::leak(crate::defaults::capitalize(kind).into_boxed_str()),
                log: Rc::new(RefCell::new(Vec::new())),
                consume_close: Rc::new(Cell::new(false)),
                toggle_binding: Some("mod+d"),
                created_input: Rc::new(RefCell::new(None)),
                created_view: Rc::new(RefCell::new(None)),
                actions: Rc::new(RefCell::new(None)),
                serialized: Rc::new(RefCell::new(serialized)),
                restored: Rc::new(RefCell::new(None)),
            }
        }
        /// The table the created page's `serialize` returns; starts as
        /// `recorded = true`. A test changes it to stand in for a page
        /// whose state moved without any shell action.
        pub fn serialized(&self) -> Rc<RefCell<toml::Table>> {
            self.serialized.clone()
        }
        /// The `restored` table `create` received, once created.
        pub fn restored(&self) -> Rc<RefCell<Option<toml::Table>>> {
            self.restored.clone()
        }
        pub fn without_toggle_binding(mut self) -> Self {
            self.toggle_binding = None;
            self
        }
        pub fn log(&self) -> Rc<RefCell<Vec<PageRecorded>>> {
            self.log.clone()
        }
        pub fn consume_next_close(&self) -> Rc<Cell<bool>> {
            self.consume_close.clone()
        }
        /// The input the created page paints, once created.
        pub fn input(&self) -> Rc<RefCell<Option<Entity<InputState>>>> {
            self.created_input.clone()
        }
        /// The created page's view, once created.
        pub fn view(&self) -> Rc<RefCell<Option<Entity<RecordingPageView>>>> {
            self.created_view.clone()
        }
        /// The `ShellActions` handle `create` received, once created.
        pub fn actions(&self) -> Rc<RefCell<Option<ShellActions>>> {
            self.actions.clone()
        }
    }

    impl PageFactory for RecordingPageFactory {
        fn kind(&self) -> &'static str {
            self.kind
        }
        fn title(&self) -> &'static str {
            self.title
        }
        fn icon(&self) -> gpui_kit_assets::IconName {
            gpui_kit_assets::IconName::Activity
        }
        fn register_actions(&self, registry: &mut ActionRegistry) {
            let _ = registry.register(crate::actions::ActionDef {
                id: ActionId(format!("{}::noop", self.kind)),
                title: "Recording page no-op".to_string(),
                category: "Test".to_string(),
            });
        }
        fn default_keymap(&self) -> Option<&'static str> {
            // Leaked once per factory: the fragment text must be 'static.
            // `n` on the bare context and `j` on `mode == normal`, so a
            // test can tell a binding that ignores the mode from one that
            // honours it.
            Some(Box::leak(
                format!(
                    "[[bindings]]\ncontext = {:?}\n[bindings.keys]\n\"n\" = \"{}::noop\"\n\n[[bindings]]\ncontext = \"{} && mode == normal\"\n[bindings.keys]\n\"j\" = \"{}::noop\"\n",
                    self.kind, self.kind, self.kind, self.kind
                )
                .into_boxed_str(),
            ))
        }
        fn toggle_binding(&self) -> Option<&'static str> {
            self.toggle_binding
        }
        fn create(
            &self,
            restored: Option<&toml::Table>,
            _frame: Entity<Frame>,
            _diagnostics: Entity<Diagnostics>,
            actions: ShellActions,
            window: &mut Window,
            cx: &mut App,
        ) -> PageOccupant {
            self.log.borrow_mut().push(PageRecorded::Created);
            *self.restored.borrow_mut() = restored.cloned();
            *self.actions.borrow_mut() = Some(actions);
            let kind = self.kind;
            let input = cx.new(|cx| InputState::new(window, cx));
            *self.created_input.borrow_mut() = Some(input.clone());
            let view = cx.new(|cx| {
                // The flag follows what the window says, not the last
                // event, so a blur delivered after a refocus cannot clear it.
                cx.subscribe_in(
                    &input,
                    window,
                    |this: &mut RecordingPageView, input, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::Focus | InputEvent::Blur) {
                            this.insert = input.read(cx).focus_handle(cx).is_focused(window);
                            cx.notify();
                        }
                    },
                )
                .detach();
                RecordingPageView {
                    focus_handle: cx.focus_handle(),
                    kind,
                    input,
                    insert: false,
                }
            });
            *self.created_view.borrow_mut() = Some(view.clone());
            PageOccupant {
                kind,
                view: view.clone().into(),
                content: Box::new(RecordingPageContent {
                    view,
                    log: self.log.clone(),
                    consume_close: self.consume_close.clone(),
                    serialized: self.serialized.clone(),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_handle_prepares_its_text_once() {
        // This unit test checks prepared text. `open_list` needs a window;
        // `shell/tests/stacks.rs` covers opening the member list.
        let h = StackHandle::new(2, 4, |_w, _cx| {});
        assert_eq!(h.index, 2);
        assert_eq!(h.len, 4);
        assert_eq!(h.text.as_ref(), "2/4");
    }

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

    #[test]
    fn page_roster_emits_checked_fragments_and_an_unchecked_toggle_doc() {
        let mut roster = PageRoster::new();
        roster.add(Box::new(recording::RecordingPageFactory::new(
            "diagnostics",
        )));
        assert_eq!(roster.kinds(), vec!["diagnostics"]);
        let entries: Vec<_> = roster
            .entries()
            .map(|e| (e.kind, e.title, e.toggle_action, e.selector, e.tip_selector))
            .collect();
        assert_eq!(
            entries,
            vec![(
                "diagnostics",
                "Diagnostics",
                "page::toggle_diagnostics",
                "sidebar-page-diagnostics",
                "tip-sidebar-page-diagnostics"
            )]
        );
        let (docs, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        // One doc from `default_keymap` (checked against `contexts`) and one
        // generated from `toggle_binding` (context-free, unchecked).
        assert_eq!(docs.len(), 2);
        let toggle = docs
            .iter()
            .find(|d| d.file.to_string_lossy().contains("page:diagnostics"))
            .expect("the toggle doc is named after the page");
        let text = toml::to_string(&toggle.table).unwrap();
        assert!(
            text.contains("\"mod+d\" = \"page::toggle_diagnostics\""),
            "{text}"
        );
    }
}
