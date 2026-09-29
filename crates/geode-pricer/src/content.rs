//! Shell integration through a [`TileContent`] wrapper and [`PricerFactory`].
//! The factory shares views, template tables, pricing settings, storage, and sheet-name
//! reservations across tiles. It routes save outcomes and configuration
//! reloads to live tiles and flushes pending edits when the app quits.

use crate::core::storage::PRICER_SHEETS_DATASET;
use crate::core::template::TemplateSet;
use crate::core::views::Views;
use crate::store::SheetStore;
use crate::tile::PricerTile;
use geode_core::colour::NamedColours;
use geode_core::dimensions::DerivedDimensions;
use geode_core::document::split_key;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{
    Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant,
};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Entity, SharedString, WeakEntity, Window};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Registered actions and their palette titles. [`DEFAULT_KEYMAP`] binds
/// the keyboard subset; commands and menus also expose actions.
pub const ACTIONS: &[(&str, &str)] = &[
    ("pricer::yank_row", "Yank row"),
    ("pricer::yank_col", "Yank column"),
    ("pricer::yank", "Yank selection"),
    ("pricer::visual_rows", "Select rows"),
    ("pricer::visual_block", "Select cells"),
    ("pricer::find_next", "Find next"),
    ("pricer::find_prev", "Find previous"),
    ("pricer::escape", "Clear selection, else find and notice"),
    ("pricer::add_below", "Add lines below…"),
    ("pricer::add_above", "Add lines above…"),
    ("pricer::edit", "Edit cell…"),
    ("pricer::delete", "Delete row"),
    ("pricer::undo", "Undo"),
    ("pricer::redo", "Redo"),
    ("pricer::put_below", "Put below"),
    ("pricer::put_above", "Put above"),
    ("pricer::move_down", "Move row down"),
    ("pricer::move_up", "Move row up"),
    ("pricer::group", "Package lines"),
    ("pricer::ungroup", "Unpackage"),
    ("pricer::menu", "Pricer actions…"),
    ("pricer::toggle", "Toggle node"),
    ("pricer::expand", "Expand node"),
    ("pricer::collapse", "Collapse node"),
    ("pricer::expand_all", "Expand all"),
    ("pricer::collapse_all", "Collapse all"),
    ("pricer::price", "Reprice all lines"),
    ("pricer::open_sheet", "Open sheet…"),
    ("pricer::rename_sheet", "Rename sheet…"),
    ("pricer::new_sheet", "New sheet"),
    ("pricer::remove_sheet", "Remove sheet…"),
    ("pricer::commit", "Commit edit"),
    ("pricer::cancel", "Cancel edit"),
    ("pricer::insert_up", "Insert: up"),
    ("pricer::insert_down", "Insert: down"),
    ("pricer::insert_up_big", "Insert: up (big)"),
    ("pricer::insert_down_big", "Insert: down (big)"),
    ("pricer::menu_pick", "Menu: pick"),
    ("pricer::menu_close", "Menu: close"),
];

/// Action title shared by palette registration and menu rows. Unknown IDs fall back to
/// their own text; the menu-title test checks all literal menu actions.
pub(crate) fn action_title(id: &'static str) -> &'static str {
    ACTIONS
        .iter()
        .find(|(a, _)| *a == id)
        .map_or(id, |(_, title)| title)
}

/// Retired action ids and their successors: a user keymap that still names
/// an old id binds the new one, with a warning (`ActionRegistry::renamed`).
pub const RENAMED_ACTIONS: &[(&str, &str)] = &[
    ("pricer::down", "motion::down"),
    ("pricer::up", "motion::up"),
    ("pricer::left", "motion::left"),
    ("pricer::right", "motion::right"),
    ("pricer::top", "motion::top"),
    ("pricer::bottom", "motion::bottom"),
    ("pricer::page_down", "motion::half_page_down"),
    ("pricer::page_up", "motion::half_page_up"),
    ("pricer::page_down_full", "motion::page_down"),
    ("pricer::page_up_full", "motion::page_up"),
    ("pricer::first_col", "motion::line_start"),
    ("pricer::last_col", "motion::line_end"),
    ("pricer::menu_down", "motion::menu_down"),
    ("pricer::menu_up", "motion::menu_up"),
];

/// Registered but deliberately unbound: `:price` and the menu reach
/// repricing; the sheet verbs are the `:e`/`:name`/`:new`/`:rm` commands'
/// pointer and palette forms (the header's sheet name, the menu).
pub const NO_DEFAULT_KEY: &[&str] = &[
    "pricer::price",
    "pricer::open_sheet",
    "pricer::rename_sheet",
    "pricer::new_sheet",
    "pricer::remove_sheet",
];

/// The module's keymap fragment. Every predicate is a
/// plain conjunction whose first identifier is `pricer`. Every field (the
/// entry field, the cell editor, the sheet picker's filter and the rename
/// field) and the armed `:rm` prompt report `mode == insert` — the one word
/// the shell's insert-focus predicate reads — and share one block; the tile
/// routes `commit`/`cancel`/`insert_*` by which field is open (the prompt
/// consumes its keys before they reach the block). No
/// chord is bound there, so `ctrl+k` keeps opening the palette from inside
/// a field. `y` alone is unbound: an
/// exact match dispatches at once, so it would make `y y` and `y c`
/// unreachable. `g` alone is not bound for the same reason (`g p`, `g u`,
/// `g m`, `g .` here, and the shell's shared `g g`).
///
/// The grid motions are not here: the tile publishes `grid`, and the
/// shell's builtin keymap binds the shared `motion::*` ids once for every
/// grid tile in normal and visual modes. The menu's steps are not here
/// either: the tile publishes `tilelist` while its action menu is open, and
/// the builtin keymap binds the shared `motion::menu_down`/`menu_up` there.
///
/// `v` and `shift+v` start a selection, and the tile then reports
/// `mode == visual`, where the shared motions move the selection's moving
/// corner. There the verbs are single keys: `y`,
/// `d`, `shift+j`/`shift+k`, `g p`, `g u`, `i` and `enter` act on the
/// whole selection, so no doubled `y y` or `d d` has to stay reachable.
/// `escape` there clears only the selection.
///
/// The visual block comes first so the normal block is the later one: a
/// menu or tooltip hint names an action's LAST live binding
/// (`effective_binding`), and the menu is opened from normal mode, where
/// delete is `d d`. With visual last, the hint would name its bare `d`.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "pricer && mode == visual"
[bindings.keys]
"y" = "pricer::yank"
"d" = "pricer::delete"
"shift+j" = "pricer::move_down"
"shift+k" = "pricer::move_up"
"g p" = "pricer::group"
"g u" = "pricer::ungroup"
"i" = "pricer::edit"
"enter" = "pricer::edit"
"v" = "pricer::visual_block"
"shift+v" = "pricer::visual_rows"
"escape" = "pricer::escape"

[[bindings]]
context = "pricer && mode == normal"
[bindings.keys]
"y y" = "pricer::yank_row"
"y c" = "pricer::yank_col"
"n" = "pricer::find_next"
"shift+n" = "pricer::find_prev"
"escape" = "pricer::escape"
"o" = "pricer::add_below"
"shift+o" = "pricer::add_above"
"i" = "pricer::edit"
"enter" = "pricer::edit"
"d d" = "pricer::delete"
"u" = "pricer::undo"
"ctrl+r" = "pricer::redo"
"p" = "pricer::put_below"
"shift+p" = "pricer::put_above"
"shift+j" = "pricer::move_down"
"shift+k" = "pricer::move_up"
"g p" = "pricer::group"
"g u" = "pricer::ungroup"
"g m" = "tile::open_with"
"g ." = "tile::context_menu"
"." = "pricer::menu"
"space" = "pricer::toggle"
"z a" = "pricer::toggle"
"z o" = "pricer::expand"
"z c" = "pricer::collapse"
"z shift+r" = "pricer::expand_all"
"z shift+m" = "pricer::collapse_all"
"v" = "pricer::visual_block"
"shift+v" = "pricer::visual_rows"

[[bindings]]
context = "pricer && mode == insert"
[bindings.keys]
"enter" = "pricer::commit"
"escape" = "pricer::cancel"
"up" = "pricer::insert_up"
"down" = "pricer::insert_down"
"shift+up" = "pricer::insert_up_big"
"shift+down" = "pricer::insert_down_big"

[[bindings]]
context = "pricer && mode == menu"
[bindings.keys]
"enter" = "pricer::menu_pick"
"escape" = "pricer::menu_close"
"." = "pricer::menu_close"
"#;

/// Shared pricing settings: the running adapter's name and availability,
/// default refresh interval (`None` disables it), and freshness threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct PricerSettings {
    pub pricer: String,
    pub pricer_missing: bool,
    pub refresh: Option<Duration>,
    pub stale_after: Duration,
}

impl Default for PricerSettings {
    fn default() -> Self {
        PricerSettings {
            pricer: "mock".into(),
            pricer_missing: false,
            refresh: Some(Duration::from_secs(30)),
            stale_after: Duration::from_secs(15 * 60),
        }
    }
}

/// Read-only source of underlying suggestions for the entry bar. The app supplies
/// an [`UnderlyingList`] populated from `[pricing] underlyings`; tiles cache its
/// values until the provider revision changes.
pub trait UnderlyingSource {
    /// In the provider's own order.
    fn underlyings(&self, cx: &App) -> Rc<[SharedString]>;
    /// Bumped whenever the list changes; a tile re-reads on a change.
    fn revision(&self, cx: &App) -> u64;
}

/// A list set from outside, e.g. from config.
#[derive(Default)]
pub struct UnderlyingList {
    list: RefCell<Rc<[SharedString]>>,
    revision: Cell<u64>,
}

impl UnderlyingList {
    /// Upper-cases, drops blanks and repeats (first wins), and bumps the
    /// revision only when the list actually changed — an unrelated reload
    /// sets the same list again, and a bump would make every tile re-read.
    pub fn set(&self, names: &[String]) {
        let mut next: Vec<SharedString> = Vec::with_capacity(names.len());
        for name in names {
            let name = name.trim().to_uppercase();
            if name.is_empty() || next.iter().any(|n| n.as_ref() == name) {
                continue;
            }
            next.push(name.into());
        }
        if *self.list.borrow().as_ref() == next[..] {
            return;
        }
        *self.list.borrow_mut() = next.into();
        self.revision.set(self.revision.get() + 1);
    }
}

impl UnderlyingSource for UnderlyingList {
    fn underlyings(&self, _cx: &App) -> Rc<[SharedString]> {
        self.list.borrow().clone()
    }

    fn revision(&self, _cx: &App) -> u64 {
        self.revision.get()
    }
}

/// What the factory shares with every tile it built.
pub(crate) struct Shared {
    pub(crate) views: RefCell<Views>,
    /// The `pricer_templates` tables every tile's sheet parses and prints
    /// against; replaced whole on a reload (`PricerTile::adopt_templates`).
    pub(crate) templates: RefCell<Arc<TemplateSet>>,
    pub(crate) settings: RefCell<PricerSettings>,
    pub(crate) store: Rc<dyn SheetStore>,
    /// Names held by this factory's tiles. Allocation and sheet commands
    /// check this set to prevent two tiles from writing the same sheet.
    pub(crate) open: RefCell<BTreeSet<String>>,
    /// Names with a save queued and not yet answered. A write may fail, so
    /// the store adds a new name only after confirmation.
    /// `untitled-N` and `:name` treat these as taken (a closed
    /// tile's queued save must not have its name handed to a new sheet,
    /// whose first save would land on top of it), and a load of one
    /// waits for the answer (reads and saves run on different lanes, so
    /// a read submitted now could return the previous generation).
    ///
    /// Counted per name: one per `save` that answered `true` (a save the
    /// store refused at submission is never counted), less one per
    /// outcome. The data tier answers every admitted local publish exactly
    /// once and the app's mailbox delivers every answer, so the count is
    /// exact: the name is released, and a load waiting on it starts, only
    /// once the LAST queued save has answered — a read after an earlier
    /// one could return that older generation.
    pub(crate) pending_saves: RefCell<BTreeMap<String, usize>>,
    /// The tile that queued each name's latest save: its outcome is that
    /// tile's, not the current holder's (a tile that moved on via `:e`
    /// leaves its name to another). Latest origin wins per name; kept
    /// after the outcome so a later outcome still routes.
    pub(crate) save_origins: RefCell<BTreeMap<String, TileId>>,
    /// Names whose document is about to be forgotten — a `:name`'s old
    /// name from the rename until its forget is answered, and a confirmed
    /// `:rm`'s until its forget is answered. Taken for `untitled-N`; `:e`,
    /// `:name` and `:rm` refuse them, so no tile opens a sheet the forget
    /// would then delete under it.
    pub(crate) retiring: RefCell<BTreeSet<String>>,
    /// Weak tile references for reload, save-outcome routing, and quit flushing.
    pub(crate) tiles: RefCell<Vec<WeakEntity<PricerTile>>>,
    /// The entry bar's underlying suggestions; an empty list until the
    /// app hands one over (`PricerFactory::with_underlyings`).
    pub(crate) underlyings: RefCell<Rc<dyn UnderlyingSource>>,
    /// The `colors.toml` definitions a view's named `color` resolves
    /// against, one `Arc` per loaded doc: every tile's delegate holds the
    /// same pointer, so a reload's new `Arc` is what tells it to drop its
    /// resolved colours (`SheetDelegate::set_colours`).
    pub(crate) colours: RefCell<Arc<NamedColours>>,
    /// The `dimensions` doc's derived dimensions a frame scope may name
    /// (`region = 'US'` over `underlying_ref`). Replaced by the app on a
    /// reload before `PricerFactory::reload`, whose rebuild re-applies
    /// every tile's scope against them.
    pub(crate) dims: RefCell<DerivedDimensions>,
}

impl Shared {
    /// The source the entry bar reads its underlyings from.
    pub(crate) fn underlying_source(&self) -> Rc<dyn UnderlyingSource> {
        self.underlyings.borrow().clone()
    }
}

pub struct PricerContent {
    tile: Entity<PricerTile>,
}

impl TileContent for PricerContent {
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

    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, window, cx))
    }

    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::Price(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
            // This tile asks no vol slices; an outcome addressed here is a routing bug.
            Delivery::VolSlices(_) => {}
            // A sheet load's answer; the tile drops any but its latest.
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.query_answered(outcome, cx)),
            // The tile fetches no series, so either is a routing bug.
            Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}
            // The pricer uploads no document.
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
        self.tile.read(cx).serialize(cx)
    }
    /// The same route as `:autosize [reset]`.
    fn autosize_columns(
        &self,
        reset: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<(), &'static str> {
        self.tile
            .update(cx, |t, cx| t.autosize_columns(reset, window, cx))
    }

    fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.tile.read(cx).holds_focus(window, cx)
    }

    fn dimension_context(&self, cx: &App) -> Option<geode_core::context::DimensionContext> {
        self.tile.read(cx).dimension_context()
    }

    fn tile_columns(&self, cx: &App) -> Option<geode_core::tile_columns::TileColumns> {
        self.tile.read(cx).tile_columns()
    }
}

impl Shared {
    /// Whether a name is reserved by a known document, a pending save, or
    /// retirement. Open-tile ownership is checked separately through `open`.
    pub(crate) fn taken(&self, name: &str) -> bool {
        self.store.contains(name)
            || self.save_pending(name)
            || self.retiring.borrow().contains(name)
    }

    /// Whether a new sheet may not take `name`: open in any tile (the
    /// asking tile's own included) or reserved (`taken`). `:name` and a
    /// named `:new` refuse such a name.
    pub(crate) fn exists(&self, name: &str) -> bool {
        self.open.borrow().contains(name) || self.taken(name)
    }

    /// Whether a save of `name` is queued and unanswered.
    pub(crate) fn save_pending(&self, name: &str) -> bool {
        self.pending_saves.borrow().contains_key(name)
    }

    /// A save of `name` was queued (`save` answered `true`).
    pub(crate) fn save_queued(&self, name: &str) {
        *self
            .pending_saves
            .borrow_mut()
            .entry(name.to_string())
            .or_default() += 1;
    }

    /// One queued save of `name` answered; the name leaves the set with its
    /// last. An answer with none counted (nothing this app queued) leaves
    /// none.
    pub(crate) fn save_settled(&self, name: &str) {
        let mut pending = self.pending_saves.borrow_mut();
        match pending.get_mut(name) {
            Some(n) if *n > 1 => *n -= 1,
            _ => {
                pending.remove(name);
            }
        }
    }

    /// `Err("sheet 'x' is being removed")` for a retiring name.
    pub(crate) fn refuse_retiring(&self, name: &str) -> Result<(), String> {
        if self.retiring.borrow().contains(name) {
            Err(format!("sheet '{name}' is being removed"))
        } else {
            Ok(())
        }
    }

    /// Sorted command vocabulary: known and pending-save names, excluding
    /// names reserved for retirement.
    pub(crate) fn sheet_names(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = self.store.names().into_iter().collect();
        names.extend(self.pending_saves.borrow().keys().cloned());
        for gone in self.retiring.borrow().iter() {
            names.remove(gone);
        }
        names.into_iter().collect()
    }
}

/// The sheet names the diagnostics catalog holds: one `pricer_sheets`
/// partition per document, its batch the joined one-part key.
fn catalog_sheets(d: &Diagnostics) -> Vec<String> {
    d.catalog
        .as_ref()
        .and_then(|c| {
            c.datasets
                .iter()
                .find(|ds| ds.name == PRICER_SHEETS_DATASET)
        })
        .map(|ds| {
            ds.partitions
                .iter()
                .filter_map(|p| split_key(&p.batch).into_iter().next())
                .collect()
        })
        .unwrap_or_default()
}

pub struct PricerFactory {
    data: DataHandle,
    shared: Rc<Shared>,
    /// Whether the shared diagnostics catalog is already observed for known
    /// sheet names. The first `create` installs the observer; subsequent tiles
    /// use the same app-wide `Diagnostics` entity and subscription.
    catalog_watched: Cell<bool>,
}

impl PricerFactory {
    pub fn new(
        data: DataHandle,
        store: Rc<dyn SheetStore>,
        views: Views,
        templates: TemplateSet,
        settings: PricerSettings,
    ) -> Self {
        PricerFactory {
            data,
            shared: Rc::new(Shared {
                views: RefCell::new(views),
                templates: RefCell::new(Arc::new(templates)),
                settings: RefCell::new(settings),
                store,
                open: RefCell::new(BTreeSet::new()),
                pending_saves: RefCell::new(BTreeMap::new()),
                save_origins: RefCell::new(BTreeMap::new()),
                retiring: RefCell::new(BTreeSet::new()),
                tiles: RefCell::new(Vec::new()),
                underlyings: RefCell::new(Rc::new(UnderlyingList::default())),
                colours: RefCell::new(Arc::new(NamedColours::default())),
                dims: RefCell::new(DerivedDimensions::default()),
            }),
            catalog_watched: Cell::new(false),
        }
    }

    /// The entry bar's underlying suggestions. Without this the list is
    /// empty and the bar says so.
    pub fn with_underlyings(self, source: Rc<dyn UnderlyingSource>) -> Self {
        *self.shared.underlyings.borrow_mut() = source;
        self
    }

    /// The named colours a view's `color` may name. Without this every
    /// named colour is undefined and its column paints as plain.
    pub fn with_colours(self, colours: NamedColours) -> Self {
        *self.shared.colours.borrow_mut() = Arc::new(colours);
        self
    }

    /// The derived dimensions a frame scope over the pricer may name.
    /// Without this none is defined, and a scope naming one refuses.
    pub fn with_dims(self, dims: DerivedDimensions) -> Self {
        *self.shared.dims.borrow_mut() = dims;
        self
    }

    /// Replace the derived dimensions. Tiles read them at their next
    /// rebuild; the app calls this before [`Self::reload`], which rebuilds
    /// every tile.
    pub fn set_dims(&self, dims: DerivedDimensions) {
        *self.shared.dims.borrow_mut() = dims;
    }

    /// The derived dimensions the tiles apply scopes against.
    pub fn dims(&self) -> DerivedDimensions {
        self.shared.dims.borrow().clone()
    }

    /// The named colours the tiles paint from.
    pub fn colours(&self) -> Arc<NamedColours> {
        self.shared.colours.borrow().clone()
    }

    /// The source every tile's entry bar reads its underlyings from.
    pub fn underlying_source(&self) -> Rc<dyn UnderlyingSource> {
        self.shared.underlying_source()
    }

    /// Seed the store's known names from `diagnostics`' catalog now and on
    /// every later catalog, and ask for one when none is held. Once per
    /// factory. A later catalog only ADDS names (`SheetStore::set_known`):
    /// this app writes its own sheets, and a catalog read before a save
    /// landed must not drop that save's name. The observer fires on every
    /// diagnostics notification, so it compares the data version first.
    fn watch_catalog(&self, diagnostics: &Entity<Diagnostics>, cx: &mut App) {
        if self.catalog_watched.replace(true) {
            return;
        }
        let seen = Rc::new(Cell::new(None::<u64>));
        let seed = {
            let shared = Rc::downgrade(&self.shared);
            let seen = seen.clone();
            move |d: &Diagnostics| {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                let version = d.versions().data;
                if seen.get() == Some(version) {
                    return;
                }
                seen.set(Some(version));
                let names = catalog_sheets(d);
                if !names.is_empty() {
                    shared.store.set_known(names);
                }
            }
        };
        seed(diagnostics.read(cx));
        if diagnostics.read(cx).catalog.is_none() {
            // `request_catalog` bumps no version: the notify in this same
            // update is what wakes the bridge's drain to submit it.
            diagnostics.update(cx, |d, cx| {
                d.request_catalog();
                cx.notify();
            });
        }
        // Detached, holding the factory's state weakly: the factory is
        // itself owned by app-held callbacks (the bridge's drain and reload
        // observer), so a `Subscription` stored on it would tie the
        // diagnostics entity's observer list to those callbacks, and the
        // entities they capture would outlive the app's teardown. After the
        // factory is gone the observer does nothing.
        cx.observe(diagnostics, move |d, cx| seed(d.read(cx)))
            .detach();
    }

    /// Replace views, template tables, named colours, and live pricing settings.
    /// Every open tile adopts the tables, re-resolves its view, repaints from the
    /// colours, and restarts its timer. Adapter name and availability describe
    /// the running data engine and change only on restart.
    pub fn reload(
        &self,
        views: Views,
        templates: TemplateSet,
        colours: NamedColours,
        refresh: Option<Duration>,
        stale_after: Duration,
        cx: &mut App,
    ) {
        *self.shared.views.borrow_mut() = views;
        *self.shared.templates.borrow_mut() = Arc::new(templates);
        *self.shared.colours.borrow_mut() = Arc::new(colours);
        {
            let mut s = self.shared.settings.borrow_mut();
            s.refresh = refresh;
            s.stale_after = stale_after;
        }
        let tiles: Vec<WeakEntity<PricerTile>> = {
            let mut t = self.shared.tiles.borrow_mut();
            t.retain(|w| w.upgrade().is_some());
            t.clone()
        };
        for tile in tiles {
            if let Some(tile) = tile.upgrade() {
                tile.update(cx, |t, cx| t.config_changed(cx));
            }
        }
    }

    pub fn view_names(&self) -> Vec<String> {
        self.shared
            .views
            .borrow()
            .names()
            .map(str::to_string)
            .collect()
    }

    /// The loaded template names, in doc order.
    pub fn template_names(&self) -> Vec<String> {
        self.shared
            .templates
            .borrow()
            .iter()
            .map(|d| d.name.clone())
            .collect()
    }

    /// The running template set: what a reload's bad entries fall back to
    /// (`TemplateSet::from_doc_over`).
    pub fn templates(&self) -> Arc<TemplateSet> {
        self.shared.templates.borrow().clone()
    }

    pub fn settings(&self) -> PricerSettings {
        self.shared.settings.borrow().clone()
    }

    /// The loaded views, for a test that reloads with them unchanged.
    #[cfg(test)]
    pub(crate) fn views_for_tests(&self) -> Views {
        self.shared.views.borrow().clone()
    }

    /// Every live tile this factory built.
    fn live_tiles(&self) -> Vec<Entity<PricerTile>> {
        self.shared
            .tiles
            .borrow()
            .iter()
            .filter_map(WeakEntity::upgrade)
            .collect()
    }

    /// Attempt to queue unsaved sheets from every live tile. The app calls
    /// this before stopping the data service, whose writer drains admitted
    /// local writes. Admission failures remain visible in each tile's save state.
    pub fn flush_all(&self, cx: &mut App) {
        for tile in self.live_tiles() {
            tile.update(cx, |t, cx| {
                if t.flush_save(cx) {
                    t.rebuild(cx);
                }
                t.rebuild_chrome();
                cx.notify();
            });
        }
    }

    /// A local save of `sheet` landed (`Ok`) or failed (`Err(reason)`),
    /// from the data tier's local-publish outcomes. Only a confirmed save
    /// makes the name known to the store (never `save` answering `true`: a
    /// queued write may still fail). The outcome goes to the tile that
    /// queued the save (`Shared::save_origins`), not to whichever tile
    /// holds the name now: if it still holds the name it reads the outcome
    /// as its latest queued save's; if it moved on, a failure is painted
    /// on it and an `Ok` is nothing to it. A closed origin leaves the
    /// outcome to the store (the data tier's error diagnostic is the
    /// record of a failure). Once no save of `sheet` is left queued, any
    /// tile waiting to load it starts its load.
    pub fn save_answered(&self, sheet: &str, answer: Result<(), String>, cx: &mut App) {
        self.shared.save_settled(sheet);
        if answer.is_ok() {
            self.shared.store.note_saved(sheet);
        }
        let origin = self.shared.save_origins.borrow().get(sheet).copied();
        let tiles = self.live_tiles();
        if let Some(tile) = origin.and_then(|id| tiles.iter().find(|t| t.read(cx).id == id)) {
            tile.update(cx, |t, cx| {
                if t.sheet.name == sheet && !t.load_waiting {
                    t.save_answered(answer, cx)
                } else {
                    t.left_save_answered(sheet, answer, cx)
                }
            });
        }
        // A waiting tile re-checks: with a save of `sheet` still queued it
        // keeps waiting (`start_load` defers again).
        for tile in tiles {
            tile.update(cx, |t, cx| {
                if t.load_waiting && t.sheet.name == sheet {
                    t.start_load(cx);
                }
            });
        }
    }

    /// A forget of `sheet` landed or failed. Only a confirmed forget drops
    /// the name from the store's known names; a failure changes nothing
    /// (the document is still there) and is logged — the data tier has
    /// already reported it as an error diagnostic. The tile that asked
    /// (`:rm`, or a `:name` retiring its old name) paints the outcome.
    pub fn forget_answered(&self, sheet: &str, answer: Result<(), String>, cx: &mut App) {
        // Answered either way: the name is no longer about to go.
        self.shared.retiring.borrow_mut().remove(sheet);
        match &answer {
            Ok(()) => self.shared.store.note_forgotten(sheet),
            Err(reason) => tracing::warn!(
                target: "geode::pricer",
                sheet, reason = %reason,
                "sheet forget failed"
            ),
        }
        for tile in self.live_tiles() {
            tile.update(cx, |t, cx| t.forget_answered(sheet, &answer, cx));
        }
    }
}

impl ModuleFactory for PricerFactory {
    fn kind(&self) -> &'static str {
        "pricer"
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Pricer".to_string(),
            });
        }
        // A second pricer factory's repeat answers "renamed twice", which is
        // the correct outcome, like the ids above.
        for (old, new) in RENAMED_ACTIONS {
            let _ = registry.register_rename(old, new);
        }
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
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
        self.watch_catalog(&diagnostics, cx);
        let entity = cx.new(|cx| {
            PricerTile::new(
                tile,
                frame,
                diagnostics.clone(),
                self.data.clone(),
                self.shared.clone(),
                restored,
                window,
                cx,
            )
        });
        self.shared.tiles.borrow_mut().push(entity.downgrade());
        TileOccupant {
            kind: self.kind(),
            view: entity.clone().into(),
            content: Box::new(PricerContent { tile: entity }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::defaults::default_mod;
    use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
    use geode_shell::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};

    #[test]
    fn an_underlying_list_normalises_and_bumps_only_on_change() {
        let l = UnderlyingList::default();
        let cx_free = |l: &UnderlyingList| (l.list.borrow().clone(), l.revision.get());
        l.set(&["spx".into(), "SX5E".into(), "SPX".into(), "  ".into()]);
        let (list, rev) = cx_free(&l);
        assert_eq!(
            list.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
            ["SPX", "SX5E"]
        );
        assert_eq!(rev, 1);
        l.set(&["SPX".into(), "sx5e".into()]);
        assert_eq!(cx_free(&l).1, 1, "the same list after normalising: no bump");
        l.set(&["NDX".into()]);
        assert_eq!(cx_free(&l).1, 2);
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut r);
        for (id, title) in ACTIONS {
            r.register(ActionDef {
                id: ActionId(id.to_string()),
                title: title.to_string(),
                category: "Pricer".into(),
            })
            .unwrap();
        }
        r
    }

    /// The shell's builtin keymap spliced under this fragment, over the
    /// builtin and module actions: the grid motions live only in the former.
    fn keymap() -> geode_shell::keymap::Keymap {
        let doc = fragment_doc("pricer", DEFAULT_KEYMAP).unwrap();
        let (doc, diags) = check_fragment(doc, &["pricer"]);
        assert!(diags.is_empty(), "{diags:?}");
        let builtin =
            geode_core::config::LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP)
                .unwrap();
        let docs = geode_shell::keymap::fragments::splice(&[builtin], &[doc]);
        let (keymap, diags) = build_keymap(&docs, default_mod(), &registry());
        assert!(
            diags.is_empty(),
            "every key spells and every action is registered: {diags:?}"
        );
        keymap
    }

    /// The stack the shell publishes over a focused pricer in `mode`.
    fn pricer_stack(mode: &str) -> [KeyContext; 3] {
        [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("pricer").grid().pair("mode", mode).counts(),
        ]
    }

    fn resolve(spec: &str, mode: &str) -> Option<String> {
        let keymap = keymap();
        let stack = pricer_stack(mode);
        let mut m = Matcher::default();
        let mut out = None;
        for part in spec.split(' ') {
            let ks = parse_keystroke(part, default_mod()).unwrap();
            if let MatchResult::Matched { action, .. } = m.press(&keymap, ks, &stack) {
                out = Some(action.0);
            }
        }
        out
    }

    #[test]
    fn the_fragment_binds_every_action_but_the_palette_only_ones() {
        let fragment: toml::Table = toml::from_str(DEFAULT_KEYMAP).unwrap();
        let mut bound = std::collections::BTreeSet::new();
        for group in fragment["bindings"].as_array().unwrap() {
            for (_, action) in group["keys"].as_table().unwrap() {
                bound.insert(action.as_str().unwrap().to_string());
            }
        }
        let unbound: Vec<&str> = ACTIONS
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !bound.contains(*id))
            .collect();
        assert_eq!(unbound, NO_DEFAULT_KEY);
        let _ = keymap();
    }

    #[test]
    fn sequences_resolve_and_y_y_is_reachable() {
        assert_eq!(
            resolve("y y", "normal").as_deref(),
            Some("pricer::yank_row")
        );
        assert_eq!(
            resolve("y c", "normal").as_deref(),
            Some("pricer::yank_col")
        );
        assert_eq!(resolve("g p", "normal").as_deref(), Some("pricer::group"));
        assert_eq!(resolve("g g", "normal").as_deref(), Some("motion::top"));
        assert_eq!(resolve("d d", "normal").as_deref(), Some("pricer::delete"));
        assert_eq!(
            resolve("z shift+r", "normal").as_deref(),
            Some("pricer::expand_all")
        );
        assert_eq!(
            resolve("enter", "insert").as_deref(),
            Some("pricer::commit")
        );
        assert_eq!(
            resolve("up", "insert").as_deref(),
            Some("pricer::insert_up")
        );
        assert_eq!(
            resolve("shift+up", "insert").as_deref(),
            Some("pricer::insert_up_big")
        );
        assert_eq!(resolve(".", "menu").as_deref(), Some("pricer::menu_close"));
    }

    /// With the menu open the builtin keymap's shared menu steps take j/k
    /// and the arrows (the grid's motions stay out under `mode == menu`),
    /// and the fragment keeps Enter, Escape, and dot.
    #[test]
    fn the_menu_steps_are_the_shared_keys_under_tilelist() {
        let keymap = keymap();
        let stack = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("pricer")
                .grid()
                .tilelist()
                .pair("mode", "menu")
                .counts(),
        ];
        for (key, expected) in [
            ("j", "motion::menu_down"),
            ("k", "motion::menu_up"),
            ("down", "motion::menu_down"),
            ("up", "motion::menu_up"),
            ("enter", "pricer::menu_pick"),
            ("escape", "pricer::menu_close"),
            (".", "pricer::menu_close"),
        ] {
            let ks = parse_keystroke(key, default_mod()).unwrap();
            match Matcher::default().press(&keymap, ks, &stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{key}"),
                other => panic!("{key}: expected a match, got {other:?}"),
            }
        }
    }

    #[test]
    fn v_and_shift_v_start_selections_in_normal_mode() {
        assert_eq!(
            resolve("v", "normal").as_deref(),
            Some("pricer::visual_block")
        );
        assert_eq!(
            resolve("shift+v", "normal").as_deref(),
            Some("pricer::visual_rows")
        );
    }

    /// Visual mode binds the module's single-key verbs; the motions come
    /// from the shell's shared grid bindings over a live row selection.
    #[test]
    fn visual_mode_binds_single_key_verbs_and_the_motions() {
        let keymap = keymap();
        let stack = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("pricer")
                .grid()
                .pair("mode", "visual")
                .pair("select", "rows")
                .counts(),
        ];
        for (key, action) in [
            ("j", "motion::down"),
            ("down", "motion::down"),
            ("^", "motion::line_start"),
            ("y", "pricer::yank"),
            ("d", "pricer::delete"),
            ("shift+j", "pricer::move_down"),
            ("shift+k", "pricer::move_up"),
            ("g p", "pricer::group"),
            ("g u", "pricer::ungroup"),
            ("g g", "motion::top"),
            ("i", "pricer::edit"),
            ("enter", "pricer::edit"),
            ("v", "pricer::visual_block"),
            ("shift+v", "pricer::visual_rows"),
            ("escape", "pricer::escape"),
        ] {
            let mut m = Matcher::default();
            let mut out = None;
            for part in key.split(' ') {
                let ks = parse_keystroke(part, default_mod()).unwrap();
                if let MatchResult::Matched { action, .. } = m.press(&keymap, ks, &stack) {
                    out = Some(action.0);
                }
            }
            assert_eq!(out.as_deref(), Some(action), "{key}");
        }
    }

    /// The shell's `g g` and the fragment's `g m` share the `g` prefix: a
    /// first `g` waits, and the second key picks between them.
    #[test]
    fn g_waits_then_g_is_top_and_m_opens_with_context() {
        let keymap = keymap();
        let stack = pricer_stack("normal");
        for (second, expected) in [("g", "motion::top"), ("m", "tile::open_with")] {
            let mut m = Matcher::default();
            let g = parse_keystroke("g", default_mod()).unwrap();
            assert_eq!(m.press(&keymap, g, &stack), MatchResult::Pending);
            let ks = parse_keystroke(second, default_mod()).unwrap();
            match m.press(&keymap, ks, &stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected),
                other => panic!("g {second}: {other:?}"),
            }
        }
    }

    /// A user keymap written against a retired motion id keeps binding the
    /// shared id it became.
    #[test]
    fn every_retired_motion_id_renames_to_its_shared_id() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = PricerFactory::new(
            data,
            Rc::new(crate::store::MemorySheetStore::default()),
            Views::builtin(),
            TemplateSet::builtin(),
            PricerSettings::default(),
        );
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        factory.register_actions(&mut registry);
        for (old, new) in [
            ("pricer::down", "motion::down"),
            ("pricer::up", "motion::up"),
            ("pricer::left", "motion::left"),
            ("pricer::right", "motion::right"),
            ("pricer::top", "motion::top"),
            ("pricer::bottom", "motion::bottom"),
            ("pricer::page_down", "motion::half_page_down"),
            ("pricer::page_up", "motion::half_page_up"),
            ("pricer::page_down_full", "motion::page_down"),
            ("pricer::page_up_full", "motion::page_up"),
            ("pricer::first_col", "motion::line_start"),
            ("pricer::last_col", "motion::line_end"),
            ("pricer::menu_down", "motion::menu_down"),
            ("pricer::menu_up", "motion::menu_up"),
        ] {
            assert_eq!(
                registry.renamed(&ActionId(old.into())),
                Some(&ActionId(new.into())),
                "{old}"
            );
        }
    }

    #[test]
    fn o_adds_below_and_shift_o_adds_above() {
        assert_eq!(resolve("o", "normal").as_deref(), Some("pricer::add_below"));
        assert_eq!(
            resolve("shift+o", "normal").as_deref(),
            Some("pricer::add_above")
        );
        assert!(ACTIONS.iter().any(|(id, _)| *id == "pricer::add_above"));
    }

    #[test]
    fn g_m_opens_with_context_in_normal_mode() {
        let t: toml::Table = DEFAULT_KEYMAP.parse().unwrap();
        let normal = t["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["context"].as_str() == Some("pricer && mode == normal"))
            .unwrap();
        assert_eq!(normal["keys"]["g m"].as_str(), Some("tile::open_with"));
        assert_eq!(normal["keys"]["g u"].as_str(), Some("pricer::ungroup"));
    }

    /// `g .` opens the shell's row menu, under the same context as `g m`,
    /// and resolves through the spliced keymap.
    #[test]
    fn g_dot_opens_the_row_menu_in_normal_mode() {
        let t: toml::Table = DEFAULT_KEYMAP.parse().unwrap();
        let normal = t["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["context"].as_str() == Some("pricer && mode == normal"))
            .unwrap();
        assert_eq!(normal["keys"]["g ."].as_str(), Some("tile::context_menu"));
        assert_eq!(
            resolve("g .", "normal").as_deref(),
            Some("tile::context_menu")
        );
    }
}
