//! What the shell hosts (Phase 3 spec §3.1, §3.2; market-data spec §8.1):
//! the `TileContent` wrapper over a [`MarketDataTile`] entity, and one
//! factory per [`PanelSpec`] — so the roster kind is the panel's own
//! (`cvi`, and the palette reads "CVI: Split") while the KEY CONTEXT every
//! panel shares is `marketdata`. The factory carries the data handle
//! (§2.1); the shell never sees it.

use crate::core::PanelSpec;
use crate::tile::MarketDataTile;
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
use gpui::{App, Entity, SharedString, Window};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Every action this module registers, with its palette title. One list,
/// in one crate, that [`DEFAULT_KEYMAP`] binds and `register_actions`
/// registers — the two cannot drift, which is exactly what the shell's
/// retired mirrored tables could not promise.
pub const ACTIONS: &[(&str, &str)] = &[
    ("marketdata::down", "Cursor down"),
    ("marketdata::up", "Cursor up"),
    ("marketdata::left", "Cursor left"),
    ("marketdata::right", "Cursor right"),
    ("marketdata::top", "Cursor to top"),
    ("marketdata::bottom", "Cursor to bottom"),
    ("marketdata::first_col", "First column"),
    ("marketdata::last_col", "Last column"),
    ("marketdata::page_down", "Half page down"),
    ("marketdata::page_up", "Half page up"),
    ("marketdata::page_down_full", "Full page down"),
    ("marketdata::page_up_full", "Full page up"),
    ("marketdata::yank", "Yank cell"),
    ("marketdata::yank_row", "Yank row"),
    ("marketdata::yank_col", "Yank column"),
    ("marketdata::edit", "Edit cell"),
    ("marketdata::commit", "Commit edit"),
    ("marketdata::cancel", "Cancel edit"),
    ("marketdata::find_next", "Find next"),
    ("marketdata::find_prev", "Find previous"),
    ("marketdata::escape", "Escape"),
    ("marketdata::menu", "Actions menu"),
    ("marketdata::menu_down", "Menu: next"),
    ("marketdata::menu_up", "Menu: previous"),
    ("marketdata::menu_pick", "Menu: pick"),
    ("marketdata::menu_close", "Menu: close"),
    ("marketdata::insert_up", "Insert: up"),
    ("marketdata::insert_down", "Insert: down"),
    ("marketdata::insert_up_big", "Insert: up (big)"),
    ("marketdata::insert_down_big", "Insert: down (big)"),
    ("marketdata::load_underlying", "Load underlying…"),
    // A `Choice` cell's in-place step (dividend spec §4.4): the dialogs'
    // own `space`/`shift+space`, brought to a grid cell.
    ("marketdata::step", "Step value"),
    ("marketdata::step_back", "Step value back"),
    // The row verbs (dividend spec §5.3): vim's own `o`/`O`/`dd`.
    ("marketdata::insert_below", "Insert row below"),
    ("marketdata::insert_above", "Insert row above"),
    ("marketdata::delete_row", "Delete row"),
    ("marketdata::upload", "Upload"),
    ("marketdata::revert", "Revert edits"),
    ("marketdata::rebase", "Rebase"),
    // The update policy (spec §8.4, 2026-09-19): palette rows and the
    // menu's `On new document` section, no default key — a setting a
    // trader changes a few times a day is not worth a chord.
    ("marketdata::auto_hold", "Auto: hold edits"),
    ("marketdata::auto_rebase", "Auto: rebase edits"),
    ("marketdata::auto_replace", "Auto: replace edits"),
];

/// This module's default bindings (market-data spec §8.3/§8.6), handed to
/// the app through [`ModuleFactory::default_keymap`] and spliced above the
/// compiled-in shell keymap and below every desk and user layer — so a
/// trader's own override still wins.
///
/// Two contexts, matching what [`MarketDataTile::key_context`] pushes.
/// `normal` is the whole grammar. `insert` is deliberately narrow: while
/// the cell editor OR the underlying picker holds the keyboard, the shell
/// resolves BARE keys ONLY against the contexts that carry `mode ==
/// insert` (spec §8.6), so every key not bound here is a character the
/// trader is typing into a cell or the picker's field — which is the
/// point. `enter`/`escape` commit/cancel whichever of the two is open
/// (`MarketDataTile::dispatch`'s own routing); bare `up`/`down` and
/// `shift+up`/`shift+down` are the neutral `insert_up`/`insert_down`
/// (`_big`) pair (2026-09-17), whose meaning follows which input is open
/// — the picker's highlight step (spec §7) while the picker holds the
/// keyboard, a nudge of the editor's number by one unit of its painted
/// precision (ten with `shift`) while the cell or attribute editor does
/// — and "not handled" with neither. Shift alone is typing to the shell's
/// insert branch (`Modifiers::is_chord`), so `shift+up` resolves here as
/// a bare key exactly as `up` does.
///
/// **`ctrl+j`/`ctrl+k` are deliberately NOT bound here (controller
/// ruling, superseding this crate's own first attempt and spec §7's
/// original wording).** A bare key resolves only against insert-carrying
/// contexts, but a CHORD resolves against the WHOLE stack (spec §8.6
/// again) — and `ctrl+k` ships bound to `palette::toggle` at the
/// workspace level. CLAUDE.md's standing rule for a module's insert-mode
/// field is that a shipped chord still fires from inside it: the whole
/// point of resolving chords against the full stack is that a module
/// must never take one away, and scoping the shadow to "only while the
/// picker is open" still takes the palette away exactly when a trader is
/// typing into the picker — no better than taking it from the cell
/// editor. `up`/`down` (bare, already above) are the picker's whole
/// highlight vocabulary; Task 8 records the amendment.
///
/// `^` and `$` sit beside `home`/`end` as the column-extreme pair (user
/// ruling 2026-09-12: a general navigation grammar, the blotter its first
/// surface and this panel its second — the same two keys, not `0`); both
/// are shifted punctuation on a US layout, so they bind as the bare
/// character with no `shift` modifier.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "marketdata && mode == normal"
[bindings.keys]
"j" = "marketdata::down"
"k" = "marketdata::up"
"h" = "marketdata::left"
"l" = "marketdata::right"
"g g" = "marketdata::top"
"shift+g" = "marketdata::bottom"
"^" = "marketdata::first_col"
"$" = "marketdata::last_col"
"home" = "marketdata::first_col"
"end" = "marketdata::last_col"
"ctrl+d" = "marketdata::page_down"
"ctrl+u" = "marketdata::page_up"
"ctrl+f" = "marketdata::page_down_full"
"ctrl+b" = "marketdata::page_up_full"
"pagedown" = "marketdata::page_down_full"
"pageup" = "marketdata::page_up_full"
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
"j" = "marketdata::menu_down"
"k" = "marketdata::menu_up"
"down" = "marketdata::menu_down"
"up" = "marketdata::menu_up"
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
    /// The `window` is forwarded rather than dropped: `marketdata::edit`
    /// creates the cell editor's `InputState` and focuses it, and
    /// `commit`/`cancel` blur it (spec §8.6) — none of which is reachable
    /// from `&mut App` alone.
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
    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
            // This tile never prices; an outcome addressed here is a routing bug.
            Delivery::Price(_) => {}
            // This tile asks no series query and holds no
            // `(identity, source)` pair.
            Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}
            // An upload outcome: `Sent` on `Ok`, the header's
            // `upload failed` on `Err`, a stale tag ignored.
            Delivery::Upload(u) => self.tile.update(cx, |t, cx| t.deliver_upload(u, cx)),
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

/// Builds one panel's tiles. One factory per [`PanelSpec`] — the roster
/// keys on the kind, and a second document kind is a second factory over
/// the same tile and the same context.
pub struct MarketDataFactory {
    data: DataHandle,
    spec: &'static PanelSpec,
    /// `[app] blotter.stale_after` (spec §6.5), shared with every tile
    /// this factory has built exactly as `BlotterFactory`'s is, so a
    /// config reload reaches them all without recreating any.
    stale_after: Rc<Cell<Duration>>,
    /// Whether [`default_keymap`](ModuleFactory::default_keymap) answers
    /// [`DEFAULT_KEYMAP`] or `None` (see [`Self::without_keymap`]).
    /// `true` by default: the first, and ordinarily the only, factory
    /// over this crate's one shared `marketdata` context ships it.
    ships_keymap: bool,
    /// Every resolved egress target, as name → accepted document names in
    /// `egress.toml` order (Task 8) — empty until `geode-app` calls
    /// [`Self::with_egress`]. `Arc`, not `Rc`: `geode-app` builds one list
    /// from `egress.toml` and shares it, unmodified, between the CVI and
    /// dividend factories built over the same resolved targets. `create`
    /// narrows it to this spec's own document with [`targets_for`];
    /// `:upload` resolves and completes its target from the narrowed list.
    egress: Arc<Vec<(String, Vec<String>)>>,
}

impl MarketDataFactory {
    pub fn new(
        data: DataHandle,
        spec: &'static PanelSpec,
        stale_after: Duration,
    ) -> MarketDataFactory {
        MarketDataFactory {
            data,
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

    /// A second document kind's factory over the same shared
    /// `marketdata` context (CLAUDE.md's market-data documents section,
    /// "the second panel ships no second fragment"): every panel's
    /// [`DEFAULT_KEYMAP`] is the identical vocabulary — one `marketdata`
    /// context, not one per kind — so a second factory that also shipped
    /// it would splice a second, byte-identical `<module:{kind}>` layer
    /// binding the same keys in the same context a second time. One
    /// factory ships the fragment; every later one built through this
    /// door ships only its own actions (`register_actions` is already
    /// tolerant of the resulting duplicate `marketdata::*` ids) and, for
    /// [`PanelSpec::actions`], its own kind-specific ones.
    pub fn without_keymap(mut self) -> MarketDataFactory {
        self.ships_keymap = false;
        self
    }

    /// A reloaded staleness threshold: every open tile picks it up
    /// immediately, since they all share this `Rc<Cell<_>>` (the same
    /// door `BlotterFactory::set_stale_after` is).
    pub fn set_stale_after(&self, d: Duration) {
        self.stale_after.set(d);
    }

    pub fn spec(&self) -> &'static PanelSpec {
        self.spec
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
        self.spec.kind
    }

    /// **Not the kind.** The trait's default answers `kind()` — `cvi`
    /// here — and a fragment binding in a context the factory does not
    /// declare is dropped with an error diagnostic, so taking the default
    /// would leave this panel with no keys at all. One vocabulary
    /// (`marketdata`) is shared by every document kind's panel, which is
    /// the whole reason `contexts()` is a separate answer from `kind()`.
    fn contexts(&self) -> Vec<&'static str> {
        vec!["marketdata"]
    }

    fn default_keymap(&self) -> Option<&'static str> {
        self.ships_keymap.then_some(DEFAULT_KEYMAP)
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
        // The kind's own verbs (spec §6.3): registered so the palette
        // lists them and a keymap can bind them, exactly as `ACTIONS`
        // above — a second document kind's own `KindAction`s land here
        // too, since this loop is per-spec rather than per-crate.
        for a in self.spec.actions {
            let _ = registry.register(ActionDef {
                id: ActionId(a.id.to_string()),
                title: a.title.to_string(),
                category: "Market data".to_string(),
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
        let egress_targets = targets_for(&self.egress, self.spec.document);
        let entity = cx.new(|cx| {
            MarketDataTile::new(
                tile,
                self.spec,
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
            kind: self.spec.kind,
            view: entity.clone().into(),
            content: Box::new(MarketDataContent { tile: entity }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CVI, DIVIDEND};
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

    /// Registered ids the default keymap deliberately binds NO key to
    /// (spec §6.2): each is reachable from the palette (it is registered)
    /// and from the `:` line, and the four draft verbs also through the
    /// action list's own `enter`/click — an internal
    /// [`crate::tile::MarketDataTile::dispatch`] call, never a keymap
    /// binding. Binding a bare key to a verb the menu already carries
    /// would be a second door onto something spec §6.1 keeps to exactly
    /// one. [`PanelSpec::actions`] (CVI's `cvi_reanchor`/
    /// `cvi_recalc_forward`) are unbound BY DESIGN: a per-kind verb lists
    /// in the menu's own section and the palette, and answers "not built
    /// yet" until egress gives it something to send.
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

    /// The twin of `geode_blotter::content`'s own fragment test, made
    /// EXACT by the final review (A2): every id the fragment binds is
    /// registered here (a `build_keymap` diagnostic IS that failure),
    /// [`NO_DEFAULT_KEY`] is a subset of what is registered (a stale
    /// entry fails), and the set of registered ids the keymap leaves
    /// unbound EQUALS [`NO_DEFAULT_KEY`] in both directions — a new
    /// action with no key and no entry here fails, and so does an entry
    /// for an action that has since gained a key.
    #[test]
    fn the_default_keymap_binds_exactly_the_actions_this_module_registers() {
        use std::collections::BTreeSet;
        let doc = fragment_doc(CVI.kind, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &["marketdata"]);
        assert!(
            diags.is_empty(),
            "every fragment binding must name this module's own context: {diags:?}"
        );
        let mut reg = registry();
        let (data, _rx) = DataHandle::for_tests();
        MarketDataFactory::new(data, &CVI, Duration::from_secs(60)).register_actions(&mut reg);
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
        let factory = MarketDataFactory::new(data, &CVI, Duration::from_secs(60));
        assert_eq!(factory.kind(), "cvi");
        assert_eq!(factory.contexts(), vec!["marketdata"]);
        assert_eq!(factory.default_keymap(), Some(DEFAULT_KEYMAP));
    }

    /// A second document kind's factory (CLAUDE.md's "the second panel
    /// ships no second fragment"): `without_keymap` answers `None` from
    /// `default_keymap` — so `ModuleRoster::keymap_fragments` produces
    /// no `<module:dividend>` doc at all — while leaving `kind()` and
    /// `contexts()` untouched and `register_actions` still populating
    /// the registry: the actions belong to the shared vocabulary, not to
    /// whichever factory happens to ship the fragment.
    #[test]
    fn without_keymap_ships_no_fragment_and_still_registers_actions() {
        let (data, _rx) = DataHandle::for_tests();
        let factory =
            MarketDataFactory::new(data, &DIVIDEND, Duration::from_secs(60)).without_keymap();
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

    /// Task 8: `targets_for` narrows the resolved `egress.toml` list to
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

    /// `^`/`$` are the column extremes, exactly the blotter's pair (the
    /// first build bound `0` here and miscited the ruling): both bind as
    /// the bare character with `shift` cleared, with counts enabled on the
    /// context so the test proves neither is read as a count digit.
    #[test]
    fn caret_and_dollar_resolve_to_the_column_extremes() {
        let doc = fragment_doc(CVI.kind, DEFAULT_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let stack = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("marketdata")
                .pair("mode", "normal")
                .counts(),
        ];
        for (spec, expected) in [
            ("^", "marketdata::first_col"),
            ("$", "marketdata::last_col"),
        ] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, &stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }

    /// The insert-mode half of the fragment (spec §8.6): `enter` and
    /// `escape` must resolve in `marketdata && mode == insert`, because
    /// while the cell editor holds focus the shell resolves ONLY the
    /// contexts carrying `mode == insert` for a bare key — a fragment
    /// that bound them in normal mode alone would leave no way out of a
    /// cell. Bare `up`/`down` and `shift+up`/`shift+down` — the neutral
    /// `insert_*` pair (2026-09-17) — are here for the same reason: the
    /// underlying picker's field holds the keyboard exactly as the cell
    /// editor does, and its highlight has to move somehow, while over the
    /// editor the same keys nudge the number. Shift alone is not a chord
    /// to the shell (`Modifiers::is_chord`), so `shift+up` is a bare key
    /// resolving against this block exactly as `up` does.
    /// No chord is bound here at all (controller ruling — see
    /// `ctrl_k_still_opens_the_palette_from_the_open_picker`): a bare key
    /// is confined to insert-carrying contexts, but a chord resolves
    /// against the whole stack, and this module must never take one a
    /// trader could reach from anywhere else.
    #[test]
    fn enter_and_escape_resolve_in_insert_mode() {
        let doc = fragment_doc(CVI.kind, DEFAULT_KEYMAP).unwrap();
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

    /// The real invariant (controller ruling): pressed against the REAL
    /// builtin keymap (not the fragment alone — a fragment-only keymap
    /// has no `palette::toggle` binding to shadow in the first place),
    /// `ctrl+k` still opens the palette with the underlying picker OPEN
    /// — not just with the plain cell editor — because this module binds
    /// no chord in insert mode at all. A module must never take a shipped
    /// chord away from a trader, and "only while the picker is open" is
    /// still taking it away exactly when a trader is typing into the
    /// picker.
    #[test]
    fn ctrl_k_still_opens_the_palette_from_the_open_picker() {
        use geode_core::config::LayerDoc;
        let builtin = LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP)
            .expect("builtin keymap TOML is well-formed");
        let fragment = fragment_doc(CVI.kind, DEFAULT_KEYMAP).unwrap();
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

    /// `.` and `u` are the normal-mode doors onto the action list (spec
    /// §6.1); once it is open, `mode == menu` is the whole of the menu's
    /// own grammar — `j`/`k` and the arrow keys (final review, A7: the
    /// picker already took `down`/`up`, and the menu is the same list
    /// shape), `enter`/`escape`, and `.` toggling it closed again.
    #[test]
    fn dot_and_u_bind_in_normal_mode_and_the_menu_keys_in_menu_mode() {
        let doc = fragment_doc(CVI.kind, DEFAULT_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
        assert!(diags.is_empty(), "{diags:?}");
        let normal = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("marketdata")
                .pair("mode", "normal")
                .counts(),
        ];
        let menu = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("marketdata").pair("mode", "menu").counts(),
        ];
        for (stack, spec, expected) in [
            (&normal, ".", "marketdata::menu"),
            (&normal, "u", "marketdata::load_underlying"),
            (&menu, "j", "marketdata::menu_down"),
            (&menu, "k", "marketdata::menu_up"),
            (&menu, "down", "marketdata::menu_down"),
            (&menu, "up", "marketdata::menu_up"),
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

    /// [`PanelSpec::actions`] rides `register_actions` exactly as
    /// [`ACTIONS`] does (spec §6.3): the palette and a keymap can only
    /// reach a `KindAction` if the registry actually knows its id.
    #[test]
    fn the_kind_actions_are_registered() {
        let mut registry = ActionRegistry::default();
        let (data, _rx) = DataHandle::for_tests();
        MarketDataFactory::new(data, &CVI, Duration::from_secs(60)).register_actions(&mut registry);
        for a in CVI.actions {
            assert!(
                registry.get(&ActionId(a.id.to_string())).is_some(),
                "{}",
                a.id
            );
        }
    }
}
