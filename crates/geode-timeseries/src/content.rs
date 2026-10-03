//! Shell integration through a [`TileContent`] wrapper and [`TimeseriesFactory`].
//! The app supplies the factory's data handle and shared named colors; the
//! shell creates and hosts tiles through the module contract.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use geode_core::colour::NamedColours;
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

use crate::tile::TimeseriesTile;

/// Registered actions and their palette titles. [`DEFAULT_KEYMAP`] binds
/// the keyboard subset; menus and the palette also expose actions.
pub const ACTIONS: &[(&str, &str)] = &[
    ("timeseries::add", "Add a series…"),
    ("timeseries::expr", "Compose an expression…"),
    ("timeseries::next", "Next series"),
    ("timeseries::prev", "Previous series"),
    ("timeseries::toggle_visible", "Show/hide series"),
    ("timeseries::axis_next", "Cycle series axis"),
    ("timeseries::axis_prev", "Cycle series axis back"),
    ("timeseries::split_shrink", "Shrink the upper pane"),
    ("timeseries::split_grow", "Grow the upper pane"),
    ("timeseries::color", "Cycle series color"),
    // Registered without a default binding. The menu and palette open the
    // picker; :color supplies the direct keyboard route to the same color model.
    ("timeseries::pick_color", "Pick series color…"),
    ("timeseries::rule", "Cycle bucket rule"),
    ("timeseries::remove", "Remove series"),
    ("timeseries::edit", "Edit expression…"),
    ("timeseries::list", "Series…"),
    ("timeseries::range", "Range…"),
    ("timeseries::range_custom", "Custom dates…"),
    ("timeseries::freq", "Frequency…"),
    ("timeseries::density", "Toggle density"),
    ("timeseries::percentiles", "Toggle percentiles"),
    ("timeseries::pan_left", "Pan left"),
    ("timeseries::pan_right", "Pan right"),
    ("timeseries::zoom_in", "Zoom in"),
    ("timeseries::zoom_out", "Zoom out"),
    ("timeseries::reset_view", "Reset view"),
    ("timeseries::jump_start", "Jump to start"),
    ("timeseries::jump_end", "Jump to end"),
    // The popup verbs: registered beside the rest so the fragment
    // below binds them and the palette lists them.
    ("timeseries::menu", "Actions…"),
    ("timeseries::list_close", "List: close"),
    ("timeseries::menu_pick", "Menu: pick"),
    ("timeseries::commit", "Commit"),
    ("timeseries::cancel", "Cancel"),
    ("timeseries::insert_up", "Up"),
    ("timeseries::insert_down", "Down"),
];

/// Retired action ids and their successors: a user keymap that still names
/// an old id binds the new one, with a warning (`ActionRegistry::renamed`).
pub const RENAMED_ACTIONS: &[(&str, &str)] = &[
    ("timeseries::colour", "timeseries::color"),
    ("timeseries::pick_colour", "timeseries::pick_color"),
    ("timeseries::list_down", "motion::menu_down"),
    ("timeseries::list_up", "motion::menu_up"),
];

/// Bindings scoped to the factory's `timeseries` context. Fragment validation
/// requires each predicate to be a conjunction beginning with that context.
/// Within the tile, these bindings take precedence over workspace bindings;
/// outside it, the fragment does not participate in key resolution.
///
/// The popups' row steps are not here: the tile publishes `tilelist` while
/// the series list or a menu is open, and the shell's builtin keymap binds
/// the shared `motion::menu_down`/`menu_up` (`j`/`k` and the arrows) there.
/// The tile never publishes `grid`, so the grid motions never reach it and
/// its own `h`/`l` pan and `g`/`shift+g` jump stand.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "timeseries && mode == normal"
[bindings.keys]
"a" = "timeseries::add"
"x" = "timeseries::expr"
"tab" = "timeseries::next"
"shift+tab" = "timeseries::prev"
"v" = "timeseries::toggle_visible"
"y" = "timeseries::axis_next"
"shift+y" = "timeseries::axis_prev"
"[" = "timeseries::split_shrink"
"]" = "timeseries::split_grow"
"c" = "timeseries::color"
"b" = "timeseries::rule"
"d" = "timeseries::remove"
"e" = "timeseries::edit"
"shift+l" = "timeseries::list"
"r" = "timeseries::range"
"f" = "timeseries::freq"
"shift+d" = "timeseries::density"
"p" = "timeseries::percentiles"
"h" = "timeseries::pan_left"
"l" = "timeseries::pan_right"
# `=` and `-` are the zoom keys, and there is no second spelling for
# zoom in: the plus key cannot be bound here at all, because
# `parse_keystroke` splits a binding on `+` (so a literal `"+"` is an
# "empty segment" error) and `shift+=` would never match anything either
# — both platforms deliver shift+punctuation as the shifted character
# with the shift modifier CLEARED, so that key arrives as `+`, `shift:
# false` (`geode_shell::defaults`' module doc, lines 31-46, verified
# there against the pinned platform sources).
"=" = "timeseries::zoom_in"
"-" = "timeseries::zoom_out"
"0" = "timeseries::reset_view"
"g" = "timeseries::jump_start"
"shift+g" = "timeseries::jump_end"
"." = "timeseries::menu"

[[bindings]]
context = "timeseries && mode == normal && popup == series"
[bindings.keys]
"enter" = "timeseries::list_close"
"escape" = "timeseries::list_close"

# The menus — the action list, the range menu and the frequency menu —
# hold no field, so they keep the tile's own keyboard like the series
# list does; the shared menu steps (`j`/`k` and the arrows, bound under
# `tilelist`) move their rows and `enter` picks the highlighted one. `r`
# and `f` stay the normal layer's: each toggles its own menu shut, or swaps
# the menu that is up for its own.
[[bindings]]
context = "timeseries && mode == normal && popup == menu"
[bindings.keys]
"enter" = "timeseries::menu_pick"
"escape" = "timeseries::list_close"
"." = "timeseries::list_close"

# The range menu's `Custom dates…` row. Bindings resolve last-wins, so
# this layer sits after the normal one, whose `c` cycles a color.
[[bindings]]
context = "timeseries && mode == normal && popup == menu && menu == range"
[bindings.keys]
"c" = "timeseries::range_custom"

[[bindings]]
context = "timeseries && mode == insert"
[bindings.keys]
"enter" = "timeseries::commit"
"escape" = "timeseries::cancel"
"up" = "timeseries::insert_up"
"down" = "timeseries::insert_down"
"#;

pub struct TimeseriesContent {
    tile: Entity<TimeseriesTile>,
}

impl TileContent for TimeseriesContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.tile.read(cx).key_context()
    }

    /// Forward the window so popup actions can create, focus, and blur inputs.
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

    /// Forward tile-local commands through the module's command handler.
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.tile.update(cx, |t, cx| t.command(line, window, cx))
    }

    fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        self.tile.read(cx).completions(line, cursor, cx)
    }

    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.find(event, window, cx))
    }

    fn deliver(&self, delivery: Delivery, window: &mut Window, cx: &mut App) {
        match delivery {
            Delivery::Series(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
            Delivery::SeriesFetched {
                source,
                identity,
                result,
            } => self
                .tile
                .update(cx, |t, cx| t.on_fetched(&source, &identity, result, cx)),
            // This tile asks no view query and prices nothing; either
            // here is a routing bug.
            Delivery::Query(_) | Delivery::Price(_) => {}
            // This tile asks no vol slices; an outcome addressed here is a routing bug.
            Delivery::VolSlices(_) => {}
            // This tile never uploads; an outcome addressed here is a routing bug.
            Delivery::Upload(_) => {}
        }
        let _ = window;
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
        self.tile.read(cx).serialize()
    }

    fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.tile.read(cx).holds_focus(window, cx)
    }
}

/// Creates timeseries tiles sharing the data handle and named colors.
/// The factory uses the default module context, matching its `timeseries` kind.
pub struct TimeseriesFactory {
    data: DataHandle,
    /// Definitions shared with existing and future tiles. Replacing the
    /// inner `Arc` invalidates each tile's chart cache through `ChartKey`.
    colours: Rc<RefCell<Arc<NamedColours>>>,
}

impl TimeseriesFactory {
    pub fn new(data: DataHandle, colours: NamedColours) -> TimeseriesFactory {
        TimeseriesFactory {
            data,
            colours: Rc::new(RefCell::new(Arc::new(colours))),
        }
    }

    /// Replace shared named colors. The fresh `Arc` changes the chart cache
    /// key so the next chart preparation resolves colors from this set.
    pub fn set_colours(&self, colours: NamedColours) {
        *self.colours.borrow_mut() = Arc::new(colours);
    }
}

impl ModuleFactory for TimeseriesFactory {
    fn kind(&self) -> &'static str {
        "timeseries"
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Timeseries".to_string(),
            });
        }
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
        let entity = cx.new(|cx| {
            TimeseriesTile::new(
                tile,
                frame,
                diagnostics,
                self.data.clone(),
                self.colours.clone(),
                restored,
                window,
                cx,
            )
        });
        TileOccupant {
            kind: self.kind(),
            view: entity.clone().into(),
            content: Box::new(TimeseriesContent { tile: entity }),
        }
    }
}

/// The keymap a running app resolves this module's menu hints through: the
/// builtin actions and this module's, the builtin keymap with this fragment
/// spliced in (the popups' shared steps live only in the former), and an
/// optional user layer over it. Tests read menu lanes against it rather
/// than against no keymap, where every chord hint is (correctly) empty.
#[cfg(test)]
pub(crate) fn test_bindings(user: Option<&str>) -> Vec<geode_shell::keymap::Binding> {
    test_keymap(user).bindings().to_vec()
}

/// [`test_bindings`]' keymap itself, for resolving keys against it.
#[cfg(test)]
pub(crate) fn test_keymap(user: Option<&str>) -> geode_shell::keymap::Keymap {
    let mut registry = ActionRegistry::default();
    geode_shell::defaults::register_builtin_actions(&mut registry);
    for (id, title) in ACTIONS {
        registry
            .register(ActionDef {
                id: ActionId(id.to_string()),
                title: title.to_string(),
                category: "Timeseries".into(),
            })
            .unwrap();
    }
    let builtin =
        geode_core::config::LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP)
            .unwrap();
    let fragment =
        geode_shell::keymap::fragments::fragment_doc("timeseries", DEFAULT_KEYMAP).unwrap();
    let mut docs = geode_shell::keymap::fragments::splice(&[builtin], &[fragment]);
    if let Some(text) = user {
        docs.push(geode_core::config::LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".into(),
            file: "user/keymap.toml".into(),
            table: text.parse().unwrap(),
        });
    }
    let (keymap, diags) =
        geode_shell::keymap::build_keymap(&docs, geode_shell::defaults::default_mod(), &registry);
    assert!(diags.is_empty(), "{diags:?}");
    keymap
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Layer, LayerDoc, Severity};

    /// Compatibility color-action names resolve to their current IDs through
    /// factory registration, with a warning for each alias.
    #[test]
    fn a_user_binding_naming_an_old_color_action_binds_the_new_id() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = TimeseriesFactory::new(data, NamedColours::default());
        let mut registry = ActionRegistry::default();
        factory.register_actions(&mut registry);
        let user = LayerDoc {
            layer: Layer::User,
            name: "keymap".into(),
            file: "user/keymap.toml".into(),
            table: "[[bindings]]\ncontext = \"timeseries\"\n[bindings.keys]\n\
                    \"q\" = \"timeseries::colour\"\n\"w\" = \"timeseries::pick_colour\"\n"
                .parse()
                .unwrap(),
        };
        let (keymap, diags) = geode_shell::keymap::build_keymap(
            &[user],
            geode_shell::defaults::default_mod(),
            &registry,
        );
        let bound: Vec<&str> = keymap
            .bindings()
            .iter()
            .map(|b| b.action.0.as_str())
            .collect();
        assert_eq!(bound, vec!["timeseries::color", "timeseries::pick_color"]);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
    }

    /// A user keymap written against a retired list step keeps binding the
    /// shared menu step it became.
    #[test]
    fn every_retired_list_step_renames_to_its_shared_id() {
        let (data, _rx) = DataHandle::for_tests();
        let factory = TimeseriesFactory::new(data, NamedColours::default());
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        factory.register_actions(&mut registry);
        for (old, new) in [
            ("timeseries::list_down", "motion::menu_down"),
            ("timeseries::list_up", "motion::menu_up"),
        ] {
            assert_eq!(
                registry.renamed(&ActionId(old.into())),
                Some(&ActionId(new.into())),
                "{old}"
            );
            assert!(registry.get(&ActionId(old.into())).is_none(), "{old}");
        }
    }

    /// The popups' steps are the builtin keymap's shared menu keys under
    /// `tilelist`; the fragment keeps each popup's enter, escape and dot.
    #[test]
    fn the_popup_steps_are_the_shared_keys_under_tilelist() {
        use geode_shell::keymap::{KeyContext, MatchResult, Matcher, parse_keystroke};
        let keymap = test_keymap(None);
        let popup = |pair: &str| {
            [
                KeyContext::new("workspace"),
                KeyContext::new("tile"),
                KeyContext::new("timeseries")
                    .tilelist()
                    .pair("mode", "normal")
                    .pair("popup", pair)
                    .counts(),
            ]
        };
        let (series, menu) = (popup("series"), popup("menu"));
        for (stack, key, expected) in [
            (&series, "j", "motion::menu_down"),
            (&series, "k", "motion::menu_up"),
            (&series, "down", "motion::menu_down"),
            (&series, "up", "motion::menu_up"),
            (&series, "enter", "timeseries::list_close"),
            (&series, "escape", "timeseries::list_close"),
            (&menu, "j", "motion::menu_down"),
            (&menu, "k", "motion::menu_up"),
            (&menu, "enter", "timeseries::menu_pick"),
            (&menu, ".", "timeseries::list_close"),
            // Not a grid: h/l and shift+g stay the tile's own.
            (&series, "h", "timeseries::pan_left"),
            (&series, "shift+g", "timeseries::jump_end"),
        ] {
            let ks = parse_keystroke(key, geode_shell::defaults::default_mod()).unwrap();
            match Matcher::default().press(&keymap, ks, stack) {
                MatchResult::Matched { action, .. } => assert_eq!(action.0, expected, "{key}"),
                other => panic!("{key}: expected a match, got {other:?}"),
            }
        }
    }
}
