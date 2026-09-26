//! What the shell hosts (line-pricer spec §8.1): the [`TileContent`]
//! wrapper over a [`PricerTile`], and the factory that builds them. The
//! factory carries the data handle, the loaded views, the pricing
//! settings, the sheet store and the set of sheet names open across its
//! tiles (spec §7.4) — the shell sees none of them.

use crate::core::storage::PRICER_SHEETS_DATASET;
use crate::core::views::Views;
use crate::store::SheetStore;
use crate::tile::PricerTile;
use geode_core::document::split_key;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::Frame;
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
use std::time::Duration;

/// Every action this module registers, with its palette title — one list
/// that [`DEFAULT_KEYMAP`] binds and `register_actions` registers.
pub const ACTIONS: &[(&str, &str)] = &[
    ("pricer::down", "Cursor down"),
    ("pricer::up", "Cursor up"),
    ("pricer::left", "Cursor left"),
    ("pricer::right", "Cursor right"),
    ("pricer::top", "Cursor to top"),
    ("pricer::bottom", "Cursor to bottom"),
    ("pricer::first_col", "First column"),
    ("pricer::last_col", "Last column"),
    ("pricer::page_down", "Half page down"),
    ("pricer::page_up", "Half page up"),
    ("pricer::page_down_full", "Full page down"),
    ("pricer::page_up_full", "Full page up"),
    ("pricer::yank_row", "Yank row"),
    ("pricer::yank_col", "Yank column"),
    ("pricer::find_next", "Find next"),
    ("pricer::find_prev", "Find previous"),
    ("pricer::escape", "Clear find and dismissible notice"),
    ("pricer::add_below", "Add lines…"),
    ("pricer::edit", "Edit cell…"),
    ("pricer::delete", "Delete row"),
    ("pricer::undo", "Undo"),
    ("pricer::redo", "Redo"),
    ("pricer::put_below", "Put below"),
    ("pricer::put_above", "Put above"),
    ("pricer::move_down", "Move row down"),
    ("pricer::move_up", "Move row up"),
    ("pricer::group", "Group into package"),
    ("pricer::ungroup", "Ungroup package"),
    ("pricer::menu", "Pricer actions…"),
    ("pricer::toggle", "Toggle package"),
    ("pricer::expand", "Expand package"),
    ("pricer::collapse", "Collapse package"),
    ("pricer::expand_all", "Expand all packages"),
    ("pricer::collapse_all", "Collapse all packages"),
    ("pricer::price", "Reprice all lines"),
    ("pricer::commit", "Commit edit"),
    ("pricer::cancel", "Cancel edit"),
    ("pricer::insert_up", "Insert: up"),
    ("pricer::insert_down", "Insert: down"),
    ("pricer::insert_up_big", "Insert: up (big)"),
    ("pricer::insert_down_big", "Insert: down (big)"),
    ("pricer::menu_down", "Menu: next"),
    ("pricer::menu_up", "Menu: previous"),
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

/// Registered but deliberately unbound: `:price` and the menu reach it.
pub const NO_DEFAULT_KEY: &[&str] = &["pricer::price"];

/// The module's keymap fragment (spec §8.4–§8.5). Every predicate is a
/// plain conjunction whose first identifier is `pricer`. Both text fields
/// (the entry field and the cell editor) report `mode == insert` — the one
/// word the shell's insert-focus predicate reads — and share one block;
/// the tile routes `commit`/`cancel`/`insert_*` by which field is open. No
/// chord is bound there, so `ctrl+k` keeps opening the palette from inside
/// a field. `y` alone is NOT bound (planning decision 16): an
/// exact match dispatches at once, so it would make `y y` and `y c`
/// unreachable. `g` alone is not bound for the same reason (`g g`, `g p`,
/// `g u`).
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "pricer && mode == normal"
[bindings.keys]
"j" = "pricer::down"
"k" = "pricer::up"
"h" = "pricer::left"
"l" = "pricer::right"
"down" = "pricer::down"
"up" = "pricer::up"
"left" = "pricer::left"
"right" = "pricer::right"
"g g" = "pricer::top"
"shift+g" = "pricer::bottom"
"^" = "pricer::first_col"
"$" = "pricer::last_col"
"home" = "pricer::first_col"
"end" = "pricer::last_col"
"ctrl+d" = "pricer::page_down"
"ctrl+u" = "pricer::page_up"
"ctrl+f" = "pricer::page_down_full"
"ctrl+b" = "pricer::page_up_full"
"pagedown" = "pricer::page_down_full"
"pageup" = "pricer::page_up_full"
"y y" = "pricer::yank_row"
"y c" = "pricer::yank_col"
"n" = "pricer::find_next"
"shift+n" = "pricer::find_prev"
"escape" = "pricer::escape"
"o" = "pricer::add_below"
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
"." = "pricer::menu"
"space" = "pricer::toggle"
"z a" = "pricer::toggle"
"z o" = "pricer::expand"
"z c" = "pricer::collapse"
"z shift+r" = "pricer::expand_all"
"z shift+m" = "pricer::collapse_all"

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
"j" = "pricer::menu_down"
"k" = "pricer::menu_up"
"down" = "pricer::menu_down"
"up" = "pricer::menu_up"
"enter" = "pricer::menu_pick"
"escape" = "pricer::menu_close"
"." = "pricer::menu_close"
"#;

/// The app-level settings every tile reads (spec §5.5, §8.3, §9.4):
/// the pricer's name for the header and whether this binary has it, the
/// `[pricing] refresh` default (`None` = off), and the shell's
/// `stale_after`.
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

/// What the factory shares with every tile it built.
pub(crate) struct Shared {
    pub(crate) views: RefCell<Views>,
    pub(crate) settings: RefCell<PricerSettings>,
    pub(crate) store: Rc<dyn SheetStore>,
    /// Sheet names open in some tile (spec §7.4): `untitled-N` skips them,
    /// and `:e`/`:name`/`:rm` refuse them, so two writers never race.
    pub(crate) open: RefCell<BTreeSet<String>>,
    /// Names with a save queued and not yet answered. The document will
    /// exist, but the store does not know it until the save is confirmed
    /// — so `untitled-N` and `:name` treat these as taken (a closed
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
    /// So a reload reaches every open tile (planning decision 20).
    pub(crate) tiles: RefCell<Vec<WeakEntity<PricerTile>>>,
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
        self.tile.read(cx).serialize()
    }

    fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.tile.read(cx).holds_focus(window, cx)
    }
}

impl Shared {
    /// Whether a document under `name` exists or is about to: known to
    /// the store, or with a save queued and unconfirmed.
    pub(crate) fn taken(&self, name: &str) -> bool {
        self.store.contains(name)
            || self.save_pending(name)
            || self.retiring.borrow().contains(name)
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

    /// The known sheet names, sorted — the `:e`/`:rm` vocabulary. A
    /// queued save's name is included: its document is on its way.
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
        settings: PricerSettings,
    ) -> Self {
        PricerFactory {
            data,
            shared: Rc::new(Shared {
                views: RefCell::new(views),
                settings: RefCell::new(settings),
                store,
                open: RefCell::new(BTreeSet::new()),
                pending_saves: RefCell::new(BTreeMap::new()),
                save_origins: RefCell::new(BTreeMap::new()),
                retiring: RefCell::new(BTreeSet::new()),
                tiles: RefCell::new(Vec::new()),
            }),
            catalog_watched: Cell::new(false),
        }
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

    /// A reload (planning decision 20): the new views and the live pricing
    /// settings, then every open tile re-resolves its view and restarts
    /// its timer. The pricer's name and presence are the running data
    /// engine's and change only with a restart (`[pricing] adapter`).
    pub fn reload(
        &self,
        views: Views,
        refresh: Option<Duration>,
        stale_after: Duration,
        cx: &mut App,
    ) {
        *self.shared.views.borrow_mut() = views;
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

    pub fn settings(&self) -> PricerSettings {
        self.shared.settings.borrow().clone()
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

    /// Save every open tile's unsaved sheet now (`PricerTile::flush_save`).
    /// The app calls this at quit, before it stops the data service, so the
    /// saves are queued ahead of the shutdown, which runs queued local
    /// writes before it stops.
    pub fn flush_all(&self, cx: &mut App) {
        for tile in self.live_tiles() {
            tile.update(cx, |t, cx| {
                t.flush_save();
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
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
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
        self.watch_catalog(&diagnostics, cx);
        let entity = cx.new(|cx| {
            PricerTile::new(
                tile,
                frame,
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

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::default();
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

    fn keymap() -> geode_shell::keymap::Keymap {
        let doc = fragment_doc("pricer", DEFAULT_KEYMAP).unwrap();
        let (doc, diags) = check_fragment(doc, &["pricer"]);
        assert!(diags.is_empty(), "{diags:?}");
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
        assert!(
            diags.is_empty(),
            "every key spells and every action is registered: {diags:?}"
        );
        keymap
    }

    fn resolve(spec: &str, mode: &str) -> Option<String> {
        let keymap = keymap();
        let stack = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("pricer").pair("mode", mode).counts(),
        ];
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
        assert_eq!(resolve("g g", "normal").as_deref(), Some("pricer::top"));
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

    #[test]
    fn shift_o_is_unbound_and_add_above_is_gone() {
        assert!(!DEFAULT_KEYMAP.contains("shift+o"));
        assert!(!ACTIONS.iter().any(|(id, _)| *id == "pricer::add_above"));
    }
}
