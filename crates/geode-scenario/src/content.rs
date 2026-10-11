//! The shell's door: one `ScenarioFactory` per accepted shape (kind = the
//! entry name, all sharing the `scenario` context and these actions), and
//! `ScenarioContent`, the `TileContent` wrapper over the entity.

use crate::CONTEXT;
use crate::tile::ScenarioTile;
use geode_core::colour::NamedColours;
use geode_core::context::DimensionContext;
use geode_core::dimensions::DerivedDimensions;
use geode_core::link::UNDERLYING;
use geode_core::scenario::ShapeSpec;
use geode_core::schema::SchemaSpec;
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
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Every `scenario::` action, shared by all kinds; registered once per
/// factory (the registry ignores a second registration of the same id).
pub const ACTIONS: &[(&str, &str)] = &[
    ("scenario::visual_rows", "Select rows"),
    ("scenario::visual_block", "Select block"),
    ("scenario::yank", "Yank"),
    ("scenario::currency", "Toggle USD / native"),
    ("scenario::paint", "Cycle heat / plain"),
    ("scenario::diff", "Toggle diff / absolute"),
    ("scenario::escape", "Escape"),
    ("scenario::menu", "Actions\u{2026}"),
    ("scenario::menu_pick", "Menu: pick"),
    ("scenario::menu_close", "Menu: close"),
];

/// Only the module's own verbs: `h j k l`, `^ $ home end`, `g g`, `G`,
/// pages and counts are the shell's shared `grid` motions, which this
/// context opts into through `.grid().counts()`.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "scenario && mode == normal"
[bindings.keys]
"shift+v" = "scenario::visual_rows"
"v" = "scenario::visual_block"
"y" = "scenario::yank"
"ctrl+c" = "scenario::yank"
"c" = "scenario::currency"
"p" = "scenario::paint"
"m" = "scenario::diff"
"escape" = "scenario::escape"
"." = "scenario::menu"

[[bindings]]
context = "scenario && mode == visual"
[bindings.keys]
"shift+v" = "scenario::visual_rows"
"v" = "scenario::visual_block"
"y" = "scenario::yank"
"ctrl+c" = "scenario::yank"
"c" = "scenario::currency"
"p" = "scenario::paint"
"m" = "scenario::diff"
"escape" = "scenario::escape"
"." = "scenario::menu"

[[bindings]]
context = "scenario && mode == menu"
[bindings.keys]
"enter" = "scenario::menu_pick"
"escape" = "scenario::menu_close"
"." = "scenario::menu_close"
"#;

pub struct ScenarioFactory {
    data: DataHandle,
    shape: Arc<ShapeSpec>,
    kind: &'static str,
    ships_keymap: bool,
    stale_after: Rc<Cell<Duration>>,
    schema: Rc<RefCell<SchemaSpec>>,
    dims: Rc<RefCell<DerivedDimensions>>,
    colours: NamedColours,
}

impl ScenarioFactory {
    pub fn new(data: DataHandle, shape: Arc<ShapeSpec>, stale_after: Duration) -> ScenarioFactory {
        // The kind lives as long as the process, as a panel's does: the
        // roster hands out `&'static str` kinds.
        let kind: &'static str = Box::leak(shape.kind.clone().into_boxed_str());
        ScenarioFactory {
            data,
            shape,
            kind,
            ships_keymap: true,
            stale_after: Rc::new(Cell::new(stale_after)),
            schema: Rc::new(RefCell::new(SchemaSpec::default())),
            dims: Rc::new(RefCell::new(DerivedDimensions::default())),
            colours: NamedColours::default(),
        }
    }

    /// The startup schema and derived dimensions `:filter` validates against.
    pub fn with_schema(
        mut self,
        schema: Rc<RefCell<SchemaSpec>>,
        dims: Rc<RefCell<DerivedDimensions>>,
    ) -> Self {
        self.schema = schema;
        self.dims = dims;
        self
    }

    /// The startup named colors a shape's `format.color` resolves against.
    pub fn with_colours(mut self, colours: NamedColours) -> Self {
        self.colours = colours;
        self
    }

    /// Every kind declares the same context and bindings; a second copy
    /// would splice a duplicate layer. The app calls this on all but the first.
    pub fn without_keymap(mut self) -> Self {
        self.ships_keymap = false;
        self
    }

    /// The age past which a dataset time reads stale, shared with every
    /// tile this factory built.
    pub fn set_stale_after(&self, after: Duration) {
        self.stale_after.set(after);
    }

    pub fn shape(&self) -> &Arc<ShapeSpec> {
        &self.shape
    }
}

impl ModuleFactory for ScenarioFactory {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Scenario".to_string(),
            });
        }
    }

    fn contexts(&self) -> Vec<&'static str> {
        vec![CONTEXT]
    }

    fn default_keymap(&self) -> Option<&'static str> {
        self.ships_keymap.then_some(DEFAULT_KEYMAP)
    }

    /// A scenario tile narrows to one underlying when opened on a row.
    fn accepts(&self) -> &'static [&'static str] {
        &[UNDERLYING]
    }

    /// `[filter.dimensions] underlying_ref = ["<u>"]`: the tile's filter
    /// layer, which `:filter clear` removes and `:filter` edits.
    fn launch_state(&self, ctx: &DimensionContext) -> Option<toml::Table> {
        let u = ctx.get(UNDERLYING)?.to_string();
        let mut dims = toml::Table::new();
        dims.insert(
            UNDERLYING.into(),
            toml::Value::Array(vec![toml::Value::String(u)]),
        );
        let mut filter = toml::Table::new();
        filter.insert("dimensions".into(), toml::Value::Table(dims));
        let mut t = toml::Table::new();
        t.insert("filter".into(), toml::Value::Table(filter));
        Some(t)
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
        let entity = cx.new(|cx| {
            ScenarioTile::new(
                tile,
                self.shape.clone(),
                restored,
                frame,
                self.data.clone(),
                diagnostics,
                self.schema.clone(),
                self.dims.clone(),
                self.colours.clone(),
                window,
                cx,
            )
        });
        TileOccupant {
            kind: self.kind,
            view: entity.clone().into(),
            content: Box::new(ScenarioContent { tile: entity }),
        }
    }
}

pub struct ScenarioContent {
    tile: Entity<ScenarioTile>,
}

impl TileContent for ScenarioContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context(cx)
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
        self.tile.read(cx).completions(line, cursor, cx)
    }

    /// The grid has no rows to search by text; `/` changes nothing here.
    fn find(&self, _event: FindEvent, _window: &mut Window, _cx: &mut App) {}

    fn deliver(&self, delivery: Delivery, window: &mut Window, cx: &mut App) {
        let _ = window;
        match delivery {
            // The main and totals snapshots.
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver_query(outcome, cx)),
            // The axis domains.
            Delivery::Distinct(outcome) => self
                .tile
                .update(cx, |t, cx| t.deliver_distinct(outcome, cx)),
            // This tile prices nothing, asks no slices or series and uploads
            // nothing, so none of these is ever addressed to it.
            Delivery::Price(_) => {}
            Delivery::VolSlices(_) => {}
            Delivery::Series(_) => {}
            Delivery::SeriesFetched { .. } => {}
            Delivery::Upload(_) => {}
            // This tile reads and writes no files.
            Delivery::TextFile(_) => {}
        }
    }

    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.set_visible(visible, cx));
    }

    fn closed(&self, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.closed(cx));
    }

    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
        self.tile.update(cx, |t, cx| {
            t.stack = stack;
            cx.notify();
        });
    }

    fn set_close(&self, close: CloseHandle, cx: &mut App) {
        self.tile.update(cx, |t, cx| {
            t.close = Some(close);
            cx.notify();
        });
    }

    fn title(&self, cx: &App) -> SharedString {
        self.tile.read(cx).title(cx)
    }

    fn serialize(&self, cx: &App) -> toml::Table {
        self.tile.read(cx).serialize(cx)
    }

    fn autosize_columns(
        &self,
        reset: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<(), &'static str> {
        self.tile
            .update(cx, |t, cx| t.autosize_columns(reset, window, cx))
    }

    /// A scenario tile's queries take the frame's scope, so following a
    /// link group changes what it shows.
    fn follows(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session;
    use geode_shell::defaults::{default_mod, register_builtin_actions};
    use geode_shell::keymap::build_keymap;
    use geode_shell::keymap::fragments::{check_fragment, fragment_doc};

    /// The fragment validates under the shared context, binds only this
    /// module's actions, and compiles without diagnostics.
    #[test]
    fn the_fragment_binds_only_registered_actions_under_the_scenario_context() {
        let doc = fragment_doc(CONTEXT, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &[CONTEXT]);
        assert!(diags.is_empty(), "{diags:?}");
        let ids: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        let bindings = doc.table["bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 3, "normal, visual and menu");
        for b in bindings {
            for (key, action) in b["keys"].as_table().unwrap() {
                let action = action.as_str().unwrap();
                assert!(ids.contains(&action), "{key} = {action} is not registered");
            }
        }
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let (data, _rx) = DataHandle::for_tests();
        let factory = ScenarioFactory::new(
            data,
            crate::core::spec::builtin_shape("vega_kt"),
            Duration::from_secs(60),
        );
        factory.register_actions(&mut registry);
        let (_keymap, diags) = build_keymap(&[doc], default_mod(), &registry);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn without_keymap_ships_no_fragment_and_every_kind_shares_the_context() {
        let (data, _rx) = DataHandle::for_tests();
        let first = ScenarioFactory::new(
            data.clone(),
            crate::core::spec::builtin_shape("vega_kt"),
            Duration::from_secs(60),
        );
        let second = ScenarioFactory::new(
            data,
            crate::core::spec::builtin_shape("rho"),
            Duration::from_secs(60),
        )
        .without_keymap();
        assert_eq!(first.default_keymap(), Some(DEFAULT_KEYMAP));
        assert_eq!(second.default_keymap(), None);
        assert_eq!(first.contexts(), vec![CONTEXT]);
        assert_eq!(second.contexts(), vec![CONTEXT]);
    }

    #[test]
    fn launch_state_narrows_to_the_underlying_as_a_filter_selection() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = ScenarioFactory::new(
            data,
            crate::core::spec::builtin_shape("rho"),
            Duration::from_secs(60),
        );
        assert_eq!(factory.kind(), "rho");
        assert_eq!(factory.accepts(), &[UNDERLYING]);
        let ctx = DimensionContext::of(&[("lhu", "7"), (UNDERLYING, "SPX")]);
        let state = factory
            .launch_state(&ctx)
            .expect("a state for an underlying");
        let (s, notices) = session::from_table(factory.shape(), &state);
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(s.filter, geode_core::scope::Scope::one(UNDERLYING, "SPX"));
        assert_eq!(
            factory.launch_state(&DimensionContext::of(&[("lhu", "7")])),
            None
        );
    }
}
