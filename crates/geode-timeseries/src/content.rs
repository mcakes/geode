//! What the shell hosts (Phase 3 spec §3.1, §3.2; timeseries spec
//! §9.1): the [`TileContent`] wrapper over a [`TimeseriesTile`] entity,
//! and the factory that builds them. The factory carries the data handle
//! (§2.1) and the `colours` doc; the shell never sees either.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use geode_core::colour::NamedColours;
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

use crate::tile::TimeseriesTile;

/// Every action this module registers, with its palette title. One list
/// that [`DEFAULT_KEYMAP`] binds and `register_actions` registers, so the
/// two cannot drift.
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
    ("timeseries::colour", "Cycle series colour"),
    ("timeseries::rule", "Cycle bucket rule"),
    ("timeseries::remove", "Remove series"),
    ("timeseries::edit", "Edit expression…"),
    ("timeseries::list", "Series…"),
    ("timeseries::range", "Range…"),
    ("timeseries::freq_finer", "Finer frequency"),
    ("timeseries::freq_coarser", "Coarser frequency"),
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
    ("timeseries::list_down", "Series list: down"),
    ("timeseries::list_up", "Series list: up"),
    ("timeseries::list_close", "Series list: close"),
    ("timeseries::commit", "Commit"),
    ("timeseries::cancel", "Cancel"),
    ("timeseries::insert_up", "Up"),
    ("timeseries::insert_down", "Down"),
];

/// The module's keymap fragment. Every predicate is a plain conjunction
/// whose first identifier is `timeseries`, this factory's only context —
/// `keymap::fragments::check_fragment` refuses anything else, and a
/// fragment can therefore never shadow a shell binding. A bare `g` or
/// `0` here shadows nothing outside the tile: the shell's own `g g` is a
/// `workspace` binding the tile context sits above.
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
"c" = "timeseries::colour"
"b" = "timeseries::rule"
"d" = "timeseries::remove"
"e" = "timeseries::edit"
"shift+l" = "timeseries::list"
"r" = "timeseries::range"
"f" = "timeseries::freq_finer"
"shift+f" = "timeseries::freq_coarser"
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

[[bindings]]
context = "timeseries && mode == normal && popup == series"
[bindings.keys]
"j" = "timeseries::list_down"
"k" = "timeseries::list_up"
"enter" = "timeseries::list_close"
"escape" = "timeseries::list_close"

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

    /// `window` is forwarded rather than dropped: the popup verbs
    /// create a field, focus it and blur it, none of which is
    /// reachable from `&mut App` alone.
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

    /// `window` is unused here — no `:` verb this tile has touches a
    /// popup — and stays only because [`TileContent::command`] is
    /// spelled that way for every module; `TimeseriesTile::command`
    /// discards it with a `let _`.
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
            // This tile never uploads; an outcome addressed here is a routing bug.
            Delivery::Upload(_) => {}
        }
        let _ = window;
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

/// Builds this module's tiles. One factory for the crate: unlike the
/// market-data panel there is no per-document-kind spec, so kind and
/// context are the same word and `contexts()` takes the trait default.
pub struct TimeseriesFactory {
    data: DataHandle,
    /// `colours.toml`'s definitions, shared with every tile this factory
    /// has built exactly as `BlotterFactory`'s are, so a config reload
    /// reaches them all without recreating any. An `Arc` inside the cell
    /// so a tile clones a pointer rather than the map.
    colours: Rc<RefCell<Arc<NamedColours>>>,
}

impl TimeseriesFactory {
    pub fn new(data: DataHandle, colours: NamedColours) -> TimeseriesFactory {
        TimeseriesFactory {
            data,
            colours: Rc::new(RefCell::new(Arc::new(colours))),
        }
    }

    /// A reloaded `colours` doc: every open tile resolves its named
    /// colours from the new set on its next chrome rebuild. A fresh
    /// `Arc` every time, like the blotter's.
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
