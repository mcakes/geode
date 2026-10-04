//! Shell integration for document panels. Each PanelSpec has its own roster
//! kind while all factories declare the shared marketdata key context. The
//! content wrapper forwards focus-sensitive operations to its MarketDataTile;
//! the factory supplies the data handle, shared staleness threshold, and eligible
//! egress targets without exposing them to the shell.

use crate::core::PanelSpec;
use crate::tile::MarketDataTile;
use geode_core::context::DimensionContext;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{
    CloseHandle, Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant,
};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Entity, SharedString, Window};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Shared registered action IDs and palette titles. DEFAULT_KEYMAP binds a
/// subset; the remaining actions are reachable through menus, commands, or the
/// palette. PanelSpec contributes kind-specific actions separately.
pub const ACTIONS: &[(&str, &str)] = &[
    ("marketdata::yank", "Yank cell"),
    ("marketdata::yank_row", "Yank row"),
    ("marketdata::yank_col", "Yank column"),
    ("marketdata::edit", "Edit cell"),
    ("marketdata::edit_select", "Edit cell, text selected…"),
    ("marketdata::commit", "Commit edit"),
    ("marketdata::cancel", "Cancel edit"),
    ("marketdata::find_next", "Find next"),
    ("marketdata::find_prev", "Find previous"),
    ("marketdata::escape", "Escape"),
    ("marketdata::menu", "Actions menu"),
    ("marketdata::menu_pick", "Menu: pick"),
    ("marketdata::menu_close", "Menu: close"),
    ("marketdata::insert_up", "Insert: up"),
    ("marketdata::insert_down", "Insert: down"),
    ("marketdata::insert_up_big", "Insert: up (big)"),
    ("marketdata::insert_down_big", "Insert: down (big)"),
    ("marketdata::load_underlying", "Load underlying…"),
    // Step a Choice cell in place without opening its picker.
    ("marketdata::step", "Step value"),
    ("marketdata::step_back", "Step value back"),
    // Insert above/below and delete through the normal-mode row commands.
    ("marketdata::insert_below", "Insert row below"),
    ("marketdata::insert_above", "Insert row above"),
    ("marketdata::delete_row", "Delete row"),
    ("marketdata::visual_rows", "Select rows"),
    ("marketdata::visual_block", "Select cells"),
    ("marketdata::upload", "Upload"),
    ("marketdata::revert", "Revert edits"),
    ("marketdata::rebase", "Rebase"),
    // New-document policies are exposed through the menu and palette, with no
    // default key assignment.
    ("marketdata::auto_hold", "Auto: hold edits"),
    ("marketdata::auto_rebase", "Auto: rebase edits"),
    ("marketdata::auto_replace", "Auto: replace edits"),
];

/// Retired action ids and their successors: a user keymap that still names
/// an old id binds the new one, with a warning (`ActionRegistry::renamed`).
pub const RENAMED_ACTIONS: &[(&str, &str)] = &[
    ("marketdata::down", "motion::down"),
    ("marketdata::up", "motion::up"),
    ("marketdata::left", "motion::left"),
    ("marketdata::right", "motion::right"),
    ("marketdata::top", "motion::top"),
    ("marketdata::bottom", "motion::bottom"),
    ("marketdata::page_down", "motion::half_page_down"),
    ("marketdata::page_up", "motion::half_page_up"),
    ("marketdata::page_down_full", "motion::page_down"),
    ("marketdata::page_up_full", "motion::page_up"),
    ("marketdata::first_col", "motion::line_start"),
    ("marketdata::last_col", "motion::line_end"),
    ("marketdata::menu_down", "motion::menu_down"),
    ("marketdata::menu_up", "motion::menu_up"),
];

/// Module bindings supplied above shell defaults and below desk/user overrides.
/// Normal, visual, insert, and menu contexts match MarketDataTile's current input
/// state.
///
/// The grid motions are not here: the panel publishes `grid` in every mode,
/// and the shell's builtin keymap binds the shared `motion::*` ids once for
/// every grid tile under `grid` with `mode == normal || mode == visual`. The
/// menu's steps are not here either: the panel publishes `tilelist` while its
/// action menu is open, and the builtin keymap binds the shared
/// `motion::menu_down`/`menu_up` there.
///
/// Visual mode is a live `V`/`v` selection. Its consuming verbs are single
/// keys (`y`, `d`): the doubled normal-mode forms would leave the first
/// press waiting for a second there.
///
/// Insert bindings handle commit, cancel, and small/large vertical steps. The
/// tile interprets steps as picker navigation or numeric editing according to the
/// active surface. Bare keys use insert-carrying contexts; Control/Alt/Command
/// chords still resolve against the full shell stack. No chord is bound in this
/// insert block, keeping shared commands such as the palette available.
///
/// The visual block comes first so the normal block is the later one: a
/// palette, menu or tooltip hint names an action's LAST live binding
/// (`effective_binding`), and delete is `d d` in normal mode. With visual
/// last, the hint would name its bare `d`.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "marketdata && mode == visual"
[bindings.keys]
"shift+i" = "marketdata::edit_select"
"ctrl+c" = "marketdata::yank"
"y" = "marketdata::yank"
"d" = "marketdata::delete_row"
"i" = "marketdata::edit"
"enter" = "marketdata::edit"
"v" = "marketdata::visual_block"
"shift+v" = "marketdata::visual_rows"
"escape" = "marketdata::escape"

[[bindings]]
context = "marketdata && mode == normal"
[bindings.keys]
"shift+i" = "marketdata::edit_select"
"ctrl+c" = "marketdata::yank"
"y" = "marketdata::yank"
"y y" = "marketdata::yank_row"
"y c" = "marketdata::yank_col"
"i" = "marketdata::edit"
"enter" = "marketdata::edit"
"n" = "marketdata::find_next"
"shift+n" = "marketdata::find_prev"
"escape" = "marketdata::escape"
"." = "marketdata::menu"
"u" = "marketdata::load_underlying"
"space" = "marketdata::step"
"shift+space" = "marketdata::step_back"
"o" = "marketdata::insert_below"
"shift+o" = "marketdata::insert_above"
"d d" = "marketdata::delete_row"
"v" = "marketdata::visual_block"
"shift+v" = "marketdata::visual_rows"

[[bindings]]
context = "marketdata && mode == insert"
[bindings.keys]
"enter" = "marketdata::commit"
"escape" = "marketdata::cancel"
"up" = "marketdata::insert_up"
"down" = "marketdata::insert_down"
"shift+up" = "marketdata::insert_up_big"
"shift+down" = "marketdata::insert_down_big"

[[bindings]]
context = "marketdata && mode == menu"
[bindings.keys]
"enter" = "marketdata::menu_pick"
"escape" = "marketdata::menu_close"
"." = "marketdata::menu_close"
"#;

pub struct MarketDataContent {
    tile: Entity<MarketDataTile>,
}

impl TileContent for MarketDataContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context()
    }
    /// Forward the window because edit, commit, and cancel manage input focus.
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
    /// `window` is forwarded for the same reason `dispatch`'s is:
    /// `:menu` opens the action list, which — like `marketdata::edit` —
    /// may need to close the cell editor first.
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, window, cx))
    }
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor, cx)
    }
    /// `window` is forwarded because `find` closes whatever popup is
    /// open first — a Picker included, whose close blurs its field.
    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, window, cx))
    }
    fn start_fuzzy_find(
        &self,
        results: gpui::WeakEntity<geode_shell::fuzzyfind::FuzzyFind>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.tile
            .update(cx, |t, cx| t.start_fuzzy_find(results, window, cx));
        true
    }
    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
            // This tile never prices; an outcome addressed here is a routing bug.
            Delivery::Price(_) => {}
            // This tile asks no vol slices; an outcome addressed here is a routing bug.
            Delivery::VolSlices(_) => {}
            // This tile asks no series query and holds no
            // `(identity, source)` pair.
            Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}
            // The tile checks upload tags and draft identity before applying outcomes.
            Delivery::Upload(u) => self.tile.update(cx, |t, cx| t.deliver_upload(u, cx)),
            // This tile asks for no distinct values.
            Delivery::Distinct(_) => {}
            // This tile reads and writes no files.
            Delivery::TextFile(_) => {}
        }
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
    }
    fn closed(&self, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.closed(cx))
    }
    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_stack(stack, cx))
    }
    fn set_close(&self, close: CloseHandle, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_close(close, cx))
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
    fn launched(&self, window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.launched(window, cx))
    }
    /// A panel takes its underlying from a group it follows: the group
    /// scope's one `underlying_ref`, or none.
    fn follows(&self) -> bool {
        true
    }
    /// A panel can always emit: the answer is the kind's capability, not
    /// whether an underlying is named or a document has arrived. The shell
    /// drops a restored membership for a tile that answers `false` right
    /// after create, before either can have happened.
    fn emits(&self) -> bool {
        true
    }
    /// The panel's underlying as a one-value scope and, while its draft is
    /// not clean (`Editing`, `Behind` or `Sent`) and the upload builder can
    /// assemble it, that draft document.
    fn emission(&self, cx: &App) -> geode_core::link::Emission {
        self.tile.read(cx).emission()
    }
    /// Every route that moves the underlying, the draft or the painted
    /// document notifies the tile, so observing it covers each way the
    /// emission can change.
    fn watch_emission(
        &self,
        changed: Rc<dyn Fn(&mut App)>,
        cx: &mut App,
    ) -> Option<gpui::Subscription> {
        Some(cx.observe(&self.tile, move |_, cx| changed(cx)))
    }
}

/// Builds one panel's tiles. One factory per accepted [`PanelSpec`] — the
/// roster keys on the kind, and every panel is a factory over the same tile
/// and the same context.
pub struct MarketDataFactory {
    data: DataHandle,
    /// The spec's kind, leaked once in [`Self::new`]: the shell keys tiles,
    /// add-tile actions and session records by a `&'static str`.
    kind: &'static str,
    spec: Arc<PanelSpec>,
    /// Shared staleness threshold. Updating this Cell changes what existing
    /// tiles read without recreating them.
    stale_after: Rc<Cell<Duration>>,
    /// Whether [`default_keymap`](ModuleFactory::default_keymap) answers
    /// [`DEFAULT_KEYMAP`] or `None` (see [`Self::without_keymap`]).
    /// `true` by default: the first, and ordinarily the only, factory
    /// over this crate's one shared `marketdata` context ships it.
    ships_keymap: bool,
    /// Every resolved egress target, as name → accepted document names in
    /// `egress.toml` order — empty until `geode-app` calls
    /// [`Self::with_egress`]. `Arc`, not `Rc`: `geode-app` builds one list
    /// from `egress.toml` and shares it, unmodified, between every panel
    /// factory built over the same resolved targets. `create`
    /// narrows it to this spec's own document with [`targets_for`];
    /// `:upload` resolves and completes its target from the narrowed list.
    egress: Arc<Vec<(String, Vec<String>)>>,
}

impl MarketDataFactory {
    pub fn new(data: DataHandle, spec: Arc<PanelSpec>, stale_after: Duration) -> MarketDataFactory {
        // The shell keys tiles, add-tile actions and session records by a
        // `&'static str` kind. Panels are fixed for a launch, so factories are
        // built once per launch and this leaks one short name per panel,
        // never again.
        let kind: &'static str = Box::leak(spec.kind.clone().into_boxed_str());
        MarketDataFactory {
            data,
            kind,
            spec,
            stale_after: Rc::new(Cell::new(stale_after)),
            ships_keymap: true,
            egress: Arc::new(Vec::new()),
        }
    }

    /// Every egress target resolved from `egress.toml`, shared with every
    /// other factory built over the same resolved list (see the field's
    /// own doc comment).
    pub fn with_egress(mut self, egress: Arc<Vec<(String, Vec<String>)>>) -> MarketDataFactory {
        self.egress = egress;
        self
    }

    /// Suppress the shared fragment. `geode-app` calls this for every panel
    /// factory after the first: they still declare the marketdata context and
    /// register shared and kind-specific actions; only one factory needs to
    /// contribute identical default bindings.
    pub fn without_keymap(mut self) -> MarketDataFactory {
        self.ships_keymap = false;
        self
    }

    /// Update the threshold shared with every tile created by this factory.
    /// This setter does not notify tiles; they read the new value when rendered.
    pub fn set_stale_after(&self, d: Duration) {
        self.stale_after.set(d);
    }

    /// The threshold every tile from this factory reads (one shared cell).
    pub fn stale_after(&self) -> Duration {
        self.stale_after.get()
    }

    pub fn spec(&self) -> &Arc<PanelSpec> {
        &self.spec
    }
}

/// The eligible upload targets for one document (`:upload` resolves and
/// completes its target from this list): every `egress` entry
/// whose accepted documents name `document`, in `egress.toml`'s own
/// order — the same order `resolve` and `from_doc` both preserve.
fn targets_for(egress: &[(String, Vec<String>)], document: &str) -> Vec<SharedString> {
    egress
        .iter()
        .filter(|(_, documents)| documents.iter().any(|d| d == document))
        .map(|(name, _)| SharedString::from(name.clone()))
        .collect()
}

impl ModuleFactory for MarketDataFactory {
    fn kind(&self) -> &'static str {
        self.kind
    }

    /// Declare the shared keymap context independently of this panel's roster kind.
    /// Bindings in an undeclared module context are rejected during keymap building.
    fn contexts(&self) -> Vec<&'static str> {
        vec!["marketdata"]
    }

    fn default_keymap(&self) -> Option<&'static str> {
        self.ships_keymap.then_some(DEFAULT_KEYMAP)
    }

    /// Every panel is one document per underlying, so every panel kind
    /// opens on one.
    fn accepts(&self) -> &'static [&'static str] {
        &["underlying_ref"]
    }

    /// `{ underlying = ["<u>"] }`: the one-element display key
    /// `MarketDataTile::new` already restores from, so a launched panel
    /// starts exactly as a restored one on that key would.
    fn launch_state(&self, ctx: &DimensionContext) -> Option<toml::Table> {
        let u = ctx.get("underlying_ref")?.to_string();
        let mut t = toml::Table::new();
        t.insert(
            "underlying".into(),
            toml::Value::Array(vec![toml::Value::String(u)]),
        );
        Some(t)
    }

    /// Registered once per factory. `let _ =`: a second panel spec would
    /// register the same `marketdata::*` ids, and the second registration
    /// answering "already registered" is the correct outcome, not an
    /// error to surface — the actions belong to the vocabulary, not to one
    /// panel.
    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Market data".to_string(),
            });
        }
        // Register per-kind operations alongside the shared vocabulary so menus,
        // palette, and custom bindings can address them.
        for a in &self.spec.actions {
            let _ = registry.register(ActionDef {
                id: ActionId(a.id.to_string()),
                title: a.title.to_string(),
                category: "Market data".to_string(),
            });
        }
        // A second panel factory's repeat answers "renamed twice", which is
        // the correct outcome for the shared vocabulary, like the ids above.
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
        let egress_targets = targets_for(&self.egress, &self.spec.document);
        let entity = cx.new(|cx| {
            MarketDataTile::new(
                tile,
                Arc::clone(&self.spec),
                frame,
                diagnostics,
                self.data.clone(),
                self.stale_after.clone(),
                egress_targets,
                restored,
                window,
                cx,
            )
        });
        TileOccupant {
            kind: self.kind,
            view: entity.clone().into(),
            content: Box::new(MarketDataContent { tile: entity }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_fixtures::{CVI, DIVIDEND};
    use geode_data::DataHandle;
    use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
    use geode_shell::defaults::default_mod;
    use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
    use geode_shell::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};
    use geode_shell::module::ModuleFactory;
    use std::time::Duration;

    fn registry() -> ActionRegistry {
        let mut registry = ActionRegistry::default();
        for (id, title) in ACTIONS {
            registry
                .register(ActionDef {
                    id: ActionId((*id).to_string()),
                    title: (*title).to_string(),
                    category: "Market data".to_string(),
                })
                .expect("no duplicate ids");
        }
        registry
    }

    /// The shell's builtin keymap spliced under this fragment, over the
    /// builtin and module actions: the grid motions live only in the former.
    fn keymap_with_builtins() -> geode_shell::keymap::Keymap {
        let builtin =
            geode_core::config::LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP)
                .unwrap();
        let fragment = fragment_doc(&CVI.kind, DEFAULT_KEYMAP).unwrap();
        let docs = geode_shell::keymap::fragments::splice(&[builtin], &[fragment]);
        let mut reg = registry();
        geode_shell::defaults::register_builtin_actions(&mut reg);
        let (keymap, diags) = build_keymap(&docs, default_mod(), &reg);
        assert!(diags.is_empty(), "{diags:?}");
        keymap
    }

    /// The stack the shell publishes over a focused grid-mode panel.
    fn panel_stack(mode: &str) -> [KeyContext; 3] {
        [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("marketdata")
                .grid()
                .pair("mode", mode)
                .counts(),
        ]
    }

    /// Registered actions intentionally absent from the default keymap. This list
    /// includes shared draft/policy actions and the unimplemented CVI operations;
    /// registration alone does not mean the operation can currently succeed.
    const NO_DEFAULT_KEY: &[&str] = &[
        "marketdata::upload",
        "marketdata::revert",
        "marketdata::rebase",
        "marketdata::auto_hold",
        "marketdata::auto_rebase",
        "marketdata::auto_replace",
        "marketdata::cvi_reanchor",
        "marketdata::cvi_recalc_forward",
    ];

    /// Every bound ID must be registered, and the registered IDs left unbound
    /// must equal NO_DEFAULT_KEY. This catches both missing bindings and stale
    /// exceptions when the action vocabulary changes.
    #[test]
    fn the_default_keymap_binds_exactly_the_actions_this_module_registers() {
        use std::collections::BTreeSet;
        let doc = fragment_doc(&CVI.kind, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &["marketdata"]);
        assert!(
            diags.is_empty(),
            "every fragment binding must name this module's own context: {diags:?}"
        );
        let mut reg = registry();
        let (data, _rx) = DataHandle::for_tests();
        MarketDataFactory::new(data, Arc::clone(&CVI), Duration::from_secs(60))
            .register_actions(&mut reg);
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &reg);
        assert!(
            diags.is_empty(),
            "the fragment must bind only registered actions: {diags:?}"
        );
        let registered: BTreeSet<&str> = ACTIONS
            .iter()
            .map(|(id, _)| *id)
            .chain(CVI.actions.iter().map(|a| a.id))
            .collect();
        let bound: BTreeSet<&str> = keymap
            .bindings()
            .iter()
            .map(|b| b.action.0.as_str())
            .collect();
        let no_default_key: BTreeSet<&str> = NO_DEFAULT_KEY.iter().copied().collect();
        assert!(
            no_default_key.is_subset(&registered),
            "every NO_DEFAULT_KEY id must be registered: {:?}",
            no_default_key.difference(&registered).collect::<Vec<_>>()
        );
        let unbound: BTreeSet<&str> = registered.difference(&bound).copied().collect();
        assert_eq!(
            unbound, no_default_key,
            "the registered ids with no default key must be exactly NO_DEFAULT_KEY"
        );
    }

    /// The kind is `cvi` and the context is `marketdata` — two different
    /// words on purpose (one roster entry per document kind, one
    /// vocabulary shared by all of them). The trait's default
    /// `contexts()` answers the KIND, so a factory that forgot to
    /// override it would have every one of its fragment's bindings
    /// dropped with a diagnostic and no keys at all.
    #[test]
    fn the_factory_ships_the_fragment_and_declares_the_marketdata_context() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = MarketDataFactory::new(data, Arc::clone(&CVI), Duration::from_secs(60));
        assert_eq!(factory.kind(), "cvi");
        assert_eq!(factory.contexts(), vec!["marketdata"]);
        assert_eq!(factory.default_keymap(), Some(DEFAULT_KEYMAP));
    }

    /// Every panel kind opens on an underlying, and translates it to the
    /// same one-element `underlying` key `MarketDataTile::new` restores
    /// from — a context launch starts exactly where a restore would.
    #[test]
    fn a_panel_accepts_an_underlying_and_translates_it_to_its_restored_key() {
        let (data, _rx) = DataHandle::for_tests();
        let f = MarketDataFactory::new(data, Arc::clone(&CVI), Duration::from_secs(900));
        assert_eq!(f.accepts(), &["underlying_ref"]);
        let state = f
            .launch_state(&DimensionContext::of(&[("underlying_ref", "SPX")]))
            .expect("a state for an underlying");
        assert_eq!(
            state.get("underlying"),
            Some(&toml::Value::Array(vec![toml::Value::String("SPX".into())]))
        );
        assert_eq!(f.launch_state(&DimensionContext::default()), None);
    }

    /// An additional panel factory contributes no duplicate keymap fragment,
    /// while preserving its roster kind, context declaration, and registrations.
    #[test]
    fn without_keymap_ships_no_fragment_and_still_registers_actions() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = MarketDataFactory::new(data, Arc::clone(&DIVIDEND), Duration::from_secs(60))
            .without_keymap();
        assert_eq!(factory.kind(), "dividend");
        assert_eq!(factory.contexts(), vec!["marketdata"]);
        assert_eq!(factory.default_keymap(), None);

        let mut reg = ActionRegistry::default();
        factory.register_actions(&mut reg);
        for (id, _) in ACTIONS {
            assert!(
                reg.get(&ActionId(id.to_string())).is_some(),
                "{id} must still be registered with no fragment shipped"
            );
        }
    }

    /// `targets_for` narrows the resolved `egress.toml` list to
    /// the targets that actually accept ONE document, preserving the
    /// order `egress.toml` declared the targets in — a target that lists
    /// a different document only must not appear, and a target that
    /// happens to sort earlier by name must not jump ahead of one that
    /// was declared first.
    #[test]
    fn targets_for_narrows_to_one_document_and_keeps_egress_toml_order() {
        let egress = vec![
            ("bbg".to_string(), vec!["dividend_schedule".to_string()]),
            (
                "sophis".to_string(),
                vec!["cvi_params".to_string(), "dividend_schedule".to_string()],
            ),
            ("other".to_string(), vec!["risk_snapshot".to_string()]),
        ];
        assert_eq!(
            targets_for(&egress, "dividend_schedule"),
            vec![SharedString::from("bbg"), SharedString::from("sophis")],
            "both targets that accept it, in egress.toml order"
        );
        assert_eq!(
            targets_for(&egress, "cvi_params"),
            vec![SharedString::from("sophis")],
            "only the one target that accepts it"
        );
        assert!(
            targets_for(&egress, "nonesuch").is_empty(),
            "a document no target accepts has no eligible target"
        );
    }

    /// A hint (palette, menu, tooltip) names an action's last live binding,
    /// so delete must resolve to the normal-mode `d d`, not the visual
    /// block's bare `d`, which does nothing outside a selection.
    #[test]
    fn delete_rows_hint_names_the_normal_mode_chord() {
        let doc = fragment_doc(&CVI.kind, DEFAULT_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let b = geode_shell::keymap::effective_binding(
            keymap.bindings(),
            &ActionId("marketdata::delete_row".into()),
        )
        .expect("delete_row is bound");
        let dd: Vec<_> = ["d", "d"]
            .iter()
            .map(|k| parse_keystroke(k, default_mod()).unwrap())
            .collect();
        assert_eq!(b.keystrokes, dd);
    }

    /// Column-extreme punctuation matches as bare characters even when numeric
    /// counts are enabled on the context, in normal mode and while a
    /// selection is live. The keys come from the shell's shared motions,
    /// reached through the panel's `grid` flag.
    #[test]
    fn caret_and_dollar_resolve_to_the_column_extremes() {
        let keymap = keymap_with_builtins();
        for mode in ["normal", "visual"] {
            let stack = panel_stack(mode);
            for (spec, expected) in [("^", "motion::line_start"), ("$", "motion::line_end")] {
                let keystroke = parse_keystroke(spec, default_mod()).unwrap();
                match Matcher::default().press(&keymap, keystroke, &stack) {
                    MatchResult::Matched { action, .. } => {
                        assert_eq!(action.0, expected, "{mode}: {spec}")
                    }
                    other => panic!("{mode}: {spec}: expected a match, got {other:?}"),
                }
            }
        }
    }

    /// Bare commit/cancel and vertical editing steps resolve in insert context.
    /// The fragment adds no insert-mode chords that could shadow shared shell actions.
    #[test]
    fn enter_and_escape_resolve_in_insert_mode() {
        let doc = fragment_doc(&CVI.kind, DEFAULT_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let stack = [KeyContext::new("marketdata")
            .pair("mode", "insert")
            .counts()];
        for (spec, expected) in [
            ("enter", "marketdata::commit"),
            ("escape", "marketdata::cancel"),
            ("up", "marketdata::insert_up"),
            ("down", "marketdata::insert_down"),
            ("shift+up", "marketdata::insert_up_big"),
            ("shift+down", "marketdata::insert_down_big"),
        ] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, &stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }

    /// With the builtin shell keymap present, the palette chord remains available
    /// under the picker/editor insert context. A fragment-only test would not detect
    /// a shadowed workspace binding.
    #[test]
    fn ctrl_k_still_opens_the_palette_from_the_open_picker() {
        use geode_core::config::LayerDoc;
        let builtin = LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP)
            .expect("builtin keymap TOML is well-formed");
        let fragment = fragment_doc(&CVI.kind, DEFAULT_KEYMAP).unwrap();
        let layered = geode_shell::keymap::fragments::splice(&[builtin], &[fragment]);
        let mut reg = registry();
        geode_shell::defaults::register_builtin_actions(&mut reg);
        let (keymap, diags) = build_keymap(&layered, default_mod(), &reg);
        assert!(diags.is_empty(), "{diags:?}");

        // `key_context()` reports the identical stack for the picker and
        // the plain cell editor (no `popup` pair — see `key_context`'s
        // own doc comment), so one stack stands for both.
        let picker_or_editor_open = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("marketdata")
                .pair("mode", "insert")
                .counts(),
        ];
        let keystroke = parse_keystroke("ctrl+k", default_mod()).unwrap();
        match Matcher::default().press(&keymap, keystroke, &picker_or_editor_open) {
            MatchResult::Matched { action, .. } => assert_eq!(
                action.0, "palette::toggle",
                "the picker must not shadow a shell chord either"
            ),
            other => panic!("ctrl+k: expected a match, got {other:?}"),
        }
    }

    /// Normal mode opens the menu with dot and the picker with u. With the
    /// menu open, the builtin keymap's shared menu steps take j/k and the
    /// arrows (the grid's motions stay out under `mode == menu`), and the
    /// fragment keeps Enter, Escape, and dot to close.
    #[test]
    fn dot_and_u_bind_in_normal_mode_and_the_menu_keys_in_menu_mode() {
        let keymap = keymap_with_builtins();
        let normal = panel_stack("normal");
        let menu = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("marketdata")
                .grid()
                .tilelist()
                .pair("mode", "menu")
                .counts(),
        ];
        for (stack, spec, expected) in [
            (&normal, ".", "marketdata::menu"),
            (&normal, "u", "marketdata::load_underlying"),
            (&menu, "j", "motion::menu_down"),
            (&menu, "k", "motion::menu_up"),
            (&menu, "down", "motion::menu_down"),
            (&menu, "up", "motion::menu_up"),
            (&menu, "enter", "marketdata::menu_pick"),
            (&menu, "escape", "marketdata::menu_close"),
            (&menu, ".", "marketdata::menu_close"),
        ] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }

    /// `v`/`shift+v` start a selection from normal mode and switch or clear
    /// it from visual mode, where the consuming verbs are single keys (a
    /// doubled `y y`/`d d` prefix would leave `y` and `d` waiting there).
    #[test]
    fn v_and_shift_v_start_the_two_selections_and_visual_binds_single_key_verbs() {
        let keymap = keymap_with_builtins();
        let (normal, visual) = (panel_stack("normal"), panel_stack("visual"));
        for (stack, spec, expected) in [
            (&normal, "v", "marketdata::visual_block"),
            (&normal, "shift+v", "marketdata::visual_rows"),
            (&visual, "v", "marketdata::visual_block"),
            (&visual, "shift+v", "marketdata::visual_rows"),
            (&visual, "j", "motion::down"),
            (&visual, "y", "marketdata::yank"),
            (&visual, "d", "marketdata::delete_row"),
            (&visual, "i", "marketdata::edit"),
            (&visual, "escape", "marketdata::escape"),
        ] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }

    /// A user keymap written against a retired motion id keeps binding the
    /// shared id it became.
    #[test]
    fn every_retired_motion_id_renames_to_its_shared_id() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = MarketDataFactory::new(data, Arc::clone(&CVI), Duration::from_secs(60));
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        factory.register_actions(&mut registry);
        for (old, new) in [
            ("marketdata::down", "motion::down"),
            ("marketdata::up", "motion::up"),
            ("marketdata::left", "motion::left"),
            ("marketdata::right", "motion::right"),
            ("marketdata::top", "motion::top"),
            ("marketdata::bottom", "motion::bottom"),
            ("marketdata::page_down", "motion::half_page_down"),
            ("marketdata::page_up", "motion::half_page_up"),
            ("marketdata::page_down_full", "motion::page_down"),
            ("marketdata::page_up_full", "motion::page_up"),
            ("marketdata::first_col", "motion::line_start"),
            ("marketdata::menu_down", "motion::menu_down"),
            ("marketdata::menu_up", "motion::menu_up"),
            ("marketdata::last_col", "motion::line_end"),
        ] {
            assert_eq!(
                registry.renamed(&ActionId(old.into())),
                Some(&ActionId(new.into())),
                "{old}"
            );
        }
    }

    /// Kind-specific action IDs must be registered for palette and keymap access.
    #[test]
    fn the_kind_actions_are_registered() {
        let mut registry = ActionRegistry::default();
        let (data, _rx) = DataHandle::for_tests();
        MarketDataFactory::new(data, Arc::clone(&CVI), Duration::from_secs(60))
            .register_actions(&mut registry);
        for a in &CVI.actions {
            assert!(
                registry.get(&ActionId(a.id.to_string())).is_some(),
                "{}",
                a.id
            );
        }
    }
}
