//! Shell integration through a [`TileContent`] wrapper and [`BlotterFactory`].
//! The app supplies the factory's data handle; the shell creates and hosts
//! tiles through the module contract without depending on the data service.

use crate::tile::{ACTIONS, BlotterTile};
use geode_core::colour::NamedColours;
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;
use geode_core::view::ViewSpec;
use geode_data::DataHandle;
use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::Frame;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{
    Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant,
};
use geode_shell::tiling::TileId;
use geode_shell::vimfind::FindStyle;
use gpui::prelude::*;
use gpui::{App, Entity, SharedString, Window};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Compatibility aliases for user keymaps. `blotter::visual` resolves to
/// `blotter::visual_rows` with a rename warning, preserving whole-row
/// selection. `blotter::visual_block` selects a rectangular cell range.
pub const RENAMED_ACTIONS: &[(&str, &str)] = &[("blotter::visual", "blotter::visual_rows")];

/// Default bindings returned through [`ModuleFactory::default_keymap`].
/// The app combines this fragment with the shell's built-ins; desk and user
/// layers can override it. This crate owns both the fragment and the actions
/// registered by its factory.
///
/// The contexts match `BlotterTile::key_context`: `normal` has the full grammar;
/// `visual` has motions on both axes, yank, selection toggles, and Escape.
/// `v` selects a block and `V` selects rows. Pressing the active kind again
/// clears the selection; pressing the other kind switches it, keeping the anchor.
/// `^`/`$` and `home`/`end` move to the first/last column. Shifted punctuation
/// binds as the bare character without a separate `shift` modifier.
/// `g m` invokes the shell's `tile::open_with` using the cursor row's context.
/// Module fragments may bind shell actions within the module's own context.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "blotter && mode == normal"
[bindings.keys]
"j" = "blotter::down"
"k" = "blotter::up"
"h" = "blotter::left"
"l" = "blotter::right"
"g g" = "blotter::top"
"g m" = "tile::open_with"
"shift+g" = "blotter::bottom"
"ctrl+d" = "blotter::page_down"
"ctrl+u" = "blotter::page_up"
"ctrl+f" = "blotter::page_down_full"
"ctrl+b" = "blotter::page_up_full"
"pagedown" = "blotter::page_down_full"
"pageup" = "blotter::page_up_full"
"home" = "blotter::first_col"
"end" = "blotter::last_col"
"^" = "blotter::first_col"
"$" = "blotter::last_col"
"z o" = "blotter::expand"
"z c" = "blotter::collapse"
"z a" = "blotter::toggle"
"z shift+r" = "blotter::expand_all"
"z shift+m" = "blotter::collapse_all"
"space" = "blotter::toggle"
"v" = "blotter::visual_block"
"shift+v" = "blotter::visual_rows"
"y" = "blotter::yank"
"n" = "blotter::find_next"
"shift+n" = "blotter::find_prev"
"s" = "blotter::sort_cycle"
"shift+s" = "blotter::sort_cycle_abs"
"escape" = "blotter::escape"

[[bindings]]
context = "blotter && mode == visual"
[bindings.keys]
"j" = "blotter::down"
"k" = "blotter::up"
"h" = "blotter::left"
"l" = "blotter::right"
"g g" = "blotter::top"
"shift+g" = "blotter::bottom"
"ctrl+d" = "blotter::page_down"
"ctrl+u" = "blotter::page_up"
"ctrl+f" = "blotter::page_down_full"
"ctrl+b" = "blotter::page_up_full"
"pagedown" = "blotter::page_down_full"
"pageup" = "blotter::page_up_full"
"home" = "blotter::first_col"
"end" = "blotter::last_col"
"^" = "blotter::first_col"
"$" = "blotter::last_col"
"y" = "blotter::yank"
"v" = "blotter::visual_block"
"shift+v" = "blotter::visual_rows"
"escape" = "blotter::escape"
"#;

pub struct BlotterContent {
    tile: Entity<BlotterTile>,
}

impl BlotterContent {
    /// The content wrapper over an existing tile, so a test drives the
    /// shell's own door (`TileContent`) rather than the tile's methods.
    #[cfg(test)]
    pub(crate) fn for_tile(tile: Entity<BlotterTile>) -> BlotterContent {
        BlotterContent { tile }
    }
}

impl TileContent for BlotterContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context(cx)
    }
    fn dispatch(
        &self,
        action: &ActionId,
        count: Option<u32>,
        _window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.tile.update(cx, |t, cx| t.dispatch(action, count, cx))
    }
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, window, cx))
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
    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor, cx)
    }
    fn find(&self, event: FindEvent, _window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, cx))
    }
    fn deliver(&self, delivery: Delivery, _window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
            // This tile never prices; an outcome addressed here is a routing bug.
            Delivery::Price(_) => {}
            // This tile asks no series query and holds no
            // `(identity, source)` pair.
            Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}
            // This tile never uploads; an outcome addressed here is a routing bug.
            Delivery::Upload(_) => {}
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
    fn title(&self, cx: &App) -> SharedString {
        self.tile.read(cx).title()
    }
    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize(cx)
    }
    fn launch_context(&self, cx: &App) -> geode_core::launch::LaunchContext {
        self.tile.read(cx).launch_context(cx)
    }
    fn tile_columns(&self, cx: &App) -> Option<geode_core::tile_columns::TileColumns> {
        self.tile.read(cx).tile_columns(cx)
    }
}

pub struct BlotterFactory {
    data: DataHandle,
    views: Rc<RefCell<Vec<ViewSpec>>>,
    /// Named colors shared with every tile and replaced by the app on reload.
    /// Delegates use the inner `Arc`'s identity to invalidate resolved colors
    /// without comparing the definitions on every snapshot.
    colours: Rc<RefCell<Arc<NamedColours>>>,
    /// Shared schema and derived dimensions used to validate tile-local
    /// `:filter` commands. The app replaces them on configuration reload.
    schema: Rc<RefCell<SchemaSpec>>,
    dims: Rc<RefCell<DerivedDimensions>>,
    find_style: Rc<Cell<FindStyle>>,
    stale_after: Rc<Cell<Duration>>,
}

impl BlotterFactory {
    pub fn new(
        data: DataHandle,
        views: Vec<ViewSpec>,
        colours: NamedColours,
        schema: SchemaSpec,
        dims: DerivedDimensions,
        find_style: FindStyle,
        stale_after: Duration,
    ) -> BlotterFactory {
        BlotterFactory {
            data,
            views: Rc::new(RefCell::new(views)),
            colours: Rc::new(RefCell::new(Arc::new(colours))),
            schema: Rc::new(RefCell::new(schema)),
            dims: Rc::new(RefCell::new(dims)),
            find_style: Rc::new(Cell::new(find_style)),
            stale_after: Rc::new(Cell::new(stale_after)),
        }
    }

    /// Replace the shared views. Existing tiles read them on their next
    /// requery; the app's reload handler advances the frame's config counter
    /// to trigger that work.
    pub fn set_views(&self, views: Vec<ViewSpec>) {
        *self.views.borrow_mut() = views;
    }

    /// Replace named colors with a fresh `Arc` so delegates invalidate their
    /// resolved colors. Existing tiles pick up the definitions on their next
    /// applied snapshot, triggered by the reload handler's config bump.
    pub fn set_colours(&self, colours: NamedColours) {
        *self.colours.borrow_mut() = Arc::new(colours);
    }

    /// Current named color definitions shared with tiles.
    pub fn colours(&self) -> Arc<NamedColours> {
        Arc::clone(&self.colours.borrow())
    }

    /// Replace the shared schema used by every tile's next `:filter` command.
    pub fn set_schema(&self, schema: SchemaSpec) {
        *self.schema.borrow_mut() = schema;
    }

    /// Replace the shared derived dimensions used by filter validation.
    pub fn set_dims(&self, dims: DerivedDimensions) {
        *self.dims.borrow_mut() = dims;
    }

    pub fn set_find_style(&self, style: FindStyle) {
        self.find_style.set(style);
    }

    /// Set `[app] blotter.stale_after` for existing and future tiles.
    /// Tiles read the shared threshold when evaluating freshness; this
    /// setter does not itself request a repaint.
    pub fn set_stale_after(&self, d: Duration) {
        self.stale_after.set(d);
    }
}

impl ModuleFactory for BlotterFactory {
    fn kind(&self) -> &'static str {
        "blotter"
    }

    /// The one context `BlotterTile::key_context` names — the `mode` pair
    /// it also sets is a pair inside this context, not a second one.
    fn contexts(&self) -> Vec<&'static str> {
        vec!["blotter"]
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Blotter".to_string(),
            });
        }
        for (old, new) in RENAMED_ACTIONS {
            let _ = registry.register_rename(old, new);
        }
    }

    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        _diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        let entity = cx.new(|cx| {
            BlotterTile::new(
                tile,
                frame,
                self.data.clone(),
                self.views.clone(),
                self.colours.clone(),
                self.schema.clone(),
                self.dims.clone(),
                self.find_style.clone(),
                self.stale_after.clone(),
                restored,
                window,
                cx,
            )
        });
        TileOccupant {
            kind: "blotter",
            view: entity.clone().into(),
            content: Box::new(BlotterContent { tile: entity }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::scopes::SavedScopes;
    use geode_shell::defaults::default_mod;
    use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
    use geode_shell::keymap::{KeyContext, MatchResult, Matcher, build_keymap, parse_keystroke};

    /// Every default binding targets a registered action, and every action
    /// is reachable from a default key. Unknown targets produce diagnostics
    /// and are dropped by `build_keymap`.
    #[test]
    fn the_default_keymap_binds_exactly_the_actions_this_module_registers() {
        let doc = fragment_doc("blotter", DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &["blotter"]);
        assert!(
            diags.is_empty(),
            "every fragment binding must name this module's own context: {diags:?}"
        );
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        for (id, title) in ACTIONS {
            registry
                .register(ActionDef {
                    id: ActionId((*id).to_string()),
                    title: (*title).to_string(),
                    category: "Blotter".to_string(),
                })
                .expect("no duplicate ids");
        }
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry);
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
        for action in &bound {
            assert!(
                action.starts_with("blotter::") || *action == "tile::open_with",
                "{action} is a shell action this module's keymap binds but does not name; \
                 only tile::open_with is deliberately named here"
            );
        }
    }

    /// `^`/`$` move to the first/last column. Both are shifted punctuation
    /// on a US layout, so they bind as bare characters with `shift` cleared.
    /// Parsing alone cannot detect a binding with the wrong modifier.
    #[test]
    fn caret_and_dollar_resolve_to_the_column_extremes() {
        let doc = fragment_doc("blotter", DEFAULT_KEYMAP).unwrap();
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Blotter".to_string(),
            });
        }
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let stack = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("blotter").pair("mode", "normal").counts(),
        ];
        for (spec, expected) in [("^", "blotter::first_col"), ("$", "blotter::last_col")] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, &stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }

    /// The compatibility alias `blotter::visual` binds whole-row selection
    /// and reports the current action id in a warning, rather than dropping
    /// the user's binding.
    #[test]
    fn a_user_binding_on_the_retired_visual_id_binds_visual_rows() {
        let (handle, _rx) = geode_data::DataHandle::for_tests();
        let factory = BlotterFactory::new(
            handle,
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        );
        let mut registry = ActionRegistry::default();
        factory.register_actions(&mut registry);
        let doc = geode_core::config::LayerDoc::builtin(
            "keymap",
            "[[bindings]]\ncontext = \"blotter\"\n[bindings.keys]\n\"shift+x\" = \"blotter::visual\"\n",
        )
        .unwrap();
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("blotter::visual_rows")),
            "the rename warns, naming the current id: {diags:?}"
        );
        let stack = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("blotter").pair("mode", "normal").counts(),
        ];
        let keystroke = parse_keystroke("shift+x", default_mod()).unwrap();
        match Matcher::default().press(&keymap, keystroke, &stack) {
            MatchResult::Matched { action, .. } => assert_eq!(action.0, "blotter::visual_rows"),
            other => panic!("expected the renamed binding, got {other:?}"),
        }
    }

    #[test]
    fn v_and_shift_v_start_the_two_selections_and_h_moves_in_visual() {
        let doc = fragment_doc("blotter", DEFAULT_KEYMAP).unwrap();
        let mut registry = ActionRegistry::default();
        // `g m` names the shell's `tile::open_with`; without the builtins
        // registered the keymap build would warn and skip it.
        geode_shell::defaults::register_builtin_actions(&mut registry);
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Blotter".to_string(),
            });
        }
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let normal = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("blotter").pair("mode", "normal").counts(),
        ];
        let visual = [
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("blotter").pair("mode", "visual").counts(),
        ];
        for (stack, spec, expected) in [
            (&normal, "v", "blotter::visual_block"),
            (&normal, "shift+v", "blotter::visual_rows"),
            (&visual, "v", "blotter::visual_block"),
            (&visual, "shift+v", "blotter::visual_rows"),
            (&visual, "h", "blotter::left"),
            (&visual, "l", "blotter::right"),
        ] {
            let keystroke = parse_keystroke(spec, default_mod()).unwrap();
            match Matcher::default().press(&keymap, keystroke, stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{spec}"),
                other => panic!("{spec}: expected a match, got {other:?}"),
            }
        }
    }

    /// The app obtains the keymap through the factory. A valid fragment
    /// must also be exposed through that interface to install any bindings.
    #[test]
    fn the_factory_ships_the_fragment_and_declares_the_blotter_context() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = BlotterFactory::new(
            data,
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::Vim,
            Duration::from_secs(1),
        );
        assert_eq!(factory.default_keymap(), Some(DEFAULT_KEYMAP));
        assert_eq!(factory.contexts(), vec!["blotter"]);
    }

    /// `g m` sits beside `g g` in normal mode and names the shell's action.
    #[test]
    fn g_m_opens_with_context_in_normal_mode() {
        let t: toml::Table = DEFAULT_KEYMAP.parse().unwrap();
        let normal = t["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["context"].as_str() == Some("blotter && mode == normal"))
            .unwrap();
        assert_eq!(normal["keys"]["g m"].as_str(), Some("tile::open_with"));
        assert_eq!(normal["keys"]["g g"].as_str(), Some("blotter::top"));
    }

    /// One tile per open window, its own `VisualTestContext`.
    fn open_tile(
        factory: &BlotterFactory,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<BlotterTile>, gpui::VisualTestContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let occupant = factory.create(TileId(1), None, frame, diagnostics, window, cx);
                    occupant.view.downcast::<BlotterTile>().unwrap()
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let tile = window.root(&mut vcx).unwrap();
        (tile, vcx)
    }

    /// Created tiles use their factory's configured freshness threshold.
    /// The same reading is stale at one second and fresh at fifteen minutes.
    #[gpui::test]
    fn a_tiles_stale_threshold_is_the_factorys_configured_value(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(crate::init);

        let (short_data, _short_rx) = DataHandle::for_tests();
        let short = BlotterFactory::new(
            short_data,
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::Vim,
            Duration::from_secs(1),
        );
        let (short_tile, short_cx) = open_tile(&short, cx);
        assert_eq!(
            short_tile.read_with(&short_cx, |t, _| t.stale_after.get()),
            Duration::from_secs(1)
        );

        let (default_data, _default_rx) = DataHandle::for_tests();
        let default = BlotterFactory::new(
            default_data,
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::Vim,
            crate::tile::DEFAULT_STALE_AFTER,
        );
        let (default_tile, default_cx) = open_tile(&default, cx);
        assert_eq!(
            default_tile.read_with(&default_cx, |t, _| t.stale_after.get()),
            crate::tile::DEFAULT_STALE_AFTER
        );

        // The same reading — 5 seconds old — is stale under the 1s
        // factory's tile and not under the default (15m) factory's.
        let now = chrono::Utc::now();
        let as_of = (now - chrono::Duration::seconds(5)).to_rfc3339();
        assert!(short_tile.read_with(&short_cx, |t, _| t.is_stale(Some(&as_of), now)));
        assert!(!default_tile.read_with(&default_cx, |t, _| t.is_stale(Some(&as_of), now)));
    }
}
