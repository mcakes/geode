//! Shell integration: [`VolsliceFactory`] builds tiles over the app's data
//! handle, and [`VolsliceContent`] is each tile's door for keys, commands
//! and deliveries.

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

use crate::KIND;
use crate::tile::VolsliceTile;

/// Registered actions and their palette titles. [`DEFAULT_KEYMAP`] binds
/// every one; the palette lists them too.
pub const ACTIONS: &[(&str, &str)] = &[
    ("volslice::strip_down", "Next expiry"),
    ("volslice::strip_up", "Previous expiry"),
    ("volslice::solo", "Show only this expiry"),
    ("volslice::toggle_expiry", "Show/hide this expiry"),
    ("volslice::kind_1", "Show/hide trace kind 1"),
    ("volslice::kind_2", "Show/hide trace kind 2"),
    ("volslice::kind_3", "Show/hide trace kind 3"),
    ("volslice::kind_4", "Show/hide trace kind 4"),
    ("volslice::kind_5", "Show/hide trace kind 5"),
    ("volslice::kind_6", "Show/hide trace kind 6"),
    ("volslice::kind_7", "Show/hide trace kind 7"),
    ("volslice::kind_8", "Show/hide trace kind 8"),
    ("volslice::kind_9", "Show/hide trace kind 9"),
    ("volslice::coordinate", "Cycle x coordinate"),
    ("volslice::density", "Toggle densities"),
    ("volslice::diff", "Difference\u{2026}"),
    ("volslice::underlying", "Underlying\u{2026}"),
    ("volslice::pan_left", "Pan left"),
    ("volslice::pan_right", "Pan right"),
    ("volslice::zoom_in", "Zoom in"),
    ("volslice::zoom_out", "Zoom out"),
    ("volslice::reset_view", "Reset view"),
    ("volslice::split_shrink", "Shrink the upper pane"),
    ("volslice::split_grow", "Grow the upper pane"),
    // The popup verbs: registered beside the rest so the fragment binds
    // them and the palette lists them.
    ("volslice::commit", "Commit"),
    ("volslice::cancel", "Cancel"),
    // The underlying picker's row steps. Its field types every bare key,
    // so the shared `j`/`k` list steps cannot reach it; the arrows can.
    ("volslice::list_down", "Next row"),
    ("volslice::list_up", "Previous row"),
];

/// Bindings scoped to the factory's `volslice` context. Fragment validation
/// requires each predicate to be a conjunction beginning with that context.
/// Within the tile these bindings take precedence over workspace bindings,
/// so `shift+d` toggles densities here rather than duplicating the tile.
///
/// The bare digits are kind toggles. They reach these bindings only because
/// the tile's key context does not opt into counts
/// (`VolsliceTile::key_context`); a counting context would read a digit as
/// a pending count and the toggle would never fire.
pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "volslice && mode == normal"
[bindings.keys]
"j" = "volslice::strip_down"
"down" = "volslice::strip_down"
"k" = "volslice::strip_up"
"up" = "volslice::strip_up"
"enter" = "volslice::solo"
"space" = "volslice::toggle_expiry"
"1" = "volslice::kind_1"
"2" = "volslice::kind_2"
"3" = "volslice::kind_3"
"4" = "volslice::kind_4"
"5" = "volslice::kind_5"
"6" = "volslice::kind_6"
"7" = "volslice::kind_7"
"8" = "volslice::kind_8"
"9" = "volslice::kind_9"
"x" = "volslice::coordinate"
"shift+d" = "volslice::density"
"d" = "volslice::diff"
"u" = "volslice::underlying"
"h" = "volslice::pan_left"
"l" = "volslice::pan_right"
# `=` and `-` are the zoom keys, and there is no second spelling for
# zoom in: the plus key cannot be bound here at all, because
# `parse_keystroke` splits a binding on `+` (so a literal `"+"` is an
# "empty segment" error) and `shift+=` would never match anything either,
# because both platforms deliver shift+punctuation as the shifted character
# with the shift modifier CLEARED, so that key arrives as `+`, `shift:
# false` (`geode_shell::defaults`' module doc, verified there against the
# pinned platform sources).
"=" = "volslice::zoom_in"
"-" = "volslice::zoom_out"
"0" = "volslice::reset_view"
"[" = "volslice::split_shrink"
"]" = "volslice::split_grow"

# The underlying picker: a field, so only these keys are claimed and
# every other bare key types.
[[bindings]]
context = "volslice && mode == insert"
[bindings.keys]
"enter" = "volslice::commit"
"escape" = "volslice::cancel"
"down" = "volslice::list_down"
"up" = "volslice::list_up"

# The diff chooser: a fieldless list. The tile publishes `tilelist` while
# it is up, so the shell's shared `j`/`k` and arrows step its rows; the
# strip's own `j`/`k` are normal-mode bindings and stay out.
[[bindings]]
context = "volslice && mode == menu"
[bindings.keys]
"enter" = "volslice::commit"
"escape" = "volslice::cancel"
"#;

pub struct VolsliceContent {
    tile: Entity<VolsliceTile>,
}

impl TileContent for VolsliceContent {
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

    /// The tile has no rows to search; `/` changes nothing here.
    fn find(&self, _event: FindEvent, _window: &mut Window, _cx: &mut App) {}

    fn deliver(&self, delivery: Delivery, window: &mut Window, cx: &mut App) {
        let _ = window;
        match delivery {
            // The documents' snapshots and the vol batch's answer.
            Delivery::Query(outcome) => self.tile.update(cx, |t, cx| t.deliver_query(outcome, cx)),
            Delivery::VolSlices(outcome) => {
                self.tile.update(cx, |t, cx| t.deliver_vol(outcome, cx))
            }
            // This tile prices nothing, asks no series and uploads nothing,
            // so none of these is ever addressed to it; one that were would
            // answer nothing it asked.
            Delivery::Price(_) => {}
            Delivery::Series(_) => {}
            Delivery::SeriesFetched { .. } => {}
            Delivery::Upload(_) => {}
        }
    }

    /// The shell calls this inside its draw, where the tile's notify is
    /// dropped and its self-observer never runs: the chrome is refreshed
    /// here, so the show's own notices and strip are painted this frame.
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.tile.update(cx, |t, cx| {
            t.set_visible(visible, cx);
            t.refresh_chrome(cx);
        })
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

    /// Kept for the strip: a press acts only on a focused tile, and the
    /// cursor row is lit only on one.
    fn set_focused(&self, focused: bool, cx: &mut App) {
        self.tile.update(cx, |t, _| t.set_focused(focused))
    }

    /// A tile added with no underlying is useless: it asks for one at once.
    fn launched(&self, window: &mut Window, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.launched(window, cx))
    }

    /// A slice viewer reads its underlying from the group it follows, so
    /// following changes what it shows. The answer is the kind's
    /// capability, not whether an underlying is set: the shell asks right
    /// after create and drops a restored membership on `false`.
    fn follows(&self) -> bool {
        true
    }
}

/// Creates slice viewer tiles sharing the app's data handle.
pub struct VolsliceFactory {
    data: DataHandle,
}

impl VolsliceFactory {
    pub fn new(data: DataHandle) -> VolsliceFactory {
        VolsliceFactory { data }
    }
}

impl ModuleFactory for VolsliceFactory {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn register_actions(&self, registry: &mut ActionRegistry) {
        for (id, title) in ACTIONS {
            let _ = registry.register(ActionDef {
                id: ActionId((*id).to_string()),
                title: (*title).to_string(),
                category: "Vol slice".to_string(),
            });
        }
    }

    fn default_keymap(&self) -> Option<&'static str> {
        Some(DEFAULT_KEYMAP)
    }

    /// A slice viewer shows one underlying's smiles, so it opens on one.
    fn accepts(&self) -> &'static [&'static str] {
        &[geode_core::link::UNDERLYING]
    }

    /// `{ underlying = "<u>" }`: the key the tile restores its underlying
    /// from, so a launched tile starts exactly as a restored one would.
    fn launch_state(&self, ctx: &DimensionContext) -> Option<toml::Table> {
        let u = ctx.get(geode_core::link::UNDERLYING)?.to_string();
        let mut t = toml::Table::new();
        t.insert("underlying".into(), toml::Value::String(u));
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
            VolsliceTile::new(
                tile,
                restored,
                frame,
                self.data.clone(),
                diagnostics,
                window,
                cx,
            )
        });
        TileOccupant {
            kind: KIND,
            view: entity.clone().into(),
            content: Box::new(VolsliceContent { tile: entity }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fragment passes the shell's validator as the roster runs it, and
    /// every binding names an action this module registers: `build_keymap`
    /// drops a binding to an unregistered id.
    #[test]
    fn the_fragment_parses_and_every_binding_names_a_registered_action() {
        use geode_shell::keymap::fragments::{check_fragment, fragment_doc};
        let doc = fragment_doc(crate::KIND, DEFAULT_KEYMAP).expect("the fragment parses");
        let (doc, diags) = check_fragment(doc, &[crate::KIND]);
        assert!(diags.is_empty(), "{diags:?}");
        let ids: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        let bindings = doc.table["bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 3, "normal, insert and menu");
        let mut bound = 0;
        for b in bindings {
            for (key, action) in b["keys"].as_table().unwrap() {
                let action = action.as_str().unwrap();
                assert!(ids.contains(&action), "{key} = {action} is not registered");
                bound += 1;
            }
        }
        assert_eq!(bound, 32);
        // And the registry the app builds accepts every binding.
        let (data, _rx) = DataHandle::for_tests();
        let mut registry = ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        VolsliceFactory::new(data).register_actions(&mut registry);
        let (_, diags) = geode_shell::keymap::build_keymap(
            &[doc],
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
    }
}
