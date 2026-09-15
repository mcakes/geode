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
use geode_shell::module::{Delivery, FindEvent, ModuleFactory, TileContent, TileOccupant};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Entity, Window};
use std::cell::Cell;
use std::rc::Rc;
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
    ("marketdata::yank", "Yank cell"),
    ("marketdata::yank_row", "Yank row"),
    ("marketdata::yank_col", "Yank column"),
    ("marketdata::edit", "Edit cell"),
    ("marketdata::commit", "Commit edit"),
    ("marketdata::cancel", "Cancel edit"),
    ("marketdata::find_next", "Find next"),
    ("marketdata::find_prev", "Find previous"),
    ("marketdata::escape", "Escape"),
];

/// This module's default bindings (market-data spec §8.3/§8.6), handed to
/// the app through [`ModuleFactory::default_keymap`] and spliced above the
/// compiled-in shell keymap and below every desk and user layer — so a
/// trader's own override still wins.
///
/// Two contexts, matching what [`MarketDataTile::key_context`] pushes.
/// `normal` is the whole grammar. `insert` is deliberately just the two
/// ways out: while the cell editor holds the keyboard the shell resolves
/// bare keys ONLY against the contexts that carry `mode == insert` (spec
/// §8.6), so every key not bound here is a character the trader is typing
/// into a cell — which is the point.
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
"y" = "marketdata::yank"
"y y" = "marketdata::yank_row"
"y c" = "marketdata::yank_col"
"i" = "marketdata::edit"
"enter" = "marketdata::edit"
"n" = "marketdata::find_next"
"shift+n" = "marketdata::find_prev"
"escape" = "marketdata::escape"

[[bindings]]
context = "marketdata && mode == insert"
[bindings.keys]
"enter" = "marketdata::commit"
"escape" = "marketdata::cancel"
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
    fn command(&self, line: &str, _window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, cx))
    }
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor, cx)
    }
    fn find(&self, event: FindEvent, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, cx))
    }
    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
        }
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx))
    }
    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize()
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
        }
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
        Some(DEFAULT_KEYMAP)
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
            MarketDataTile::new(
                tile,
                self.spec,
                frame,
                diagnostics,
                self.data.clone(),
                self.stale_after.clone(),
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
    use crate::core::CVI;
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

    /// The twin of `geode_blotter::content`'s own fragment test, and the
    /// same two directions: every id the fragment binds is registered
    /// here (a `build_keymap` diagnostic IS that failure), and every
    /// registered action is reachable from some key.
    #[test]
    fn the_default_keymap_binds_exactly_the_actions_this_module_registers() {
        let doc = fragment_doc(CVI.kind, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &["marketdata"]);
        assert!(
            diags.is_empty(),
            "every fragment binding must name this module's own context: {diags:?}"
        );
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry());
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
    /// cell.
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
        ] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, &stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }
}
