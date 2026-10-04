//! Offline user-guide tile. The app registers [`GuideFactory`] independently
//! of data setup; the guide reads no configuration, files, or data service.

mod document;
mod search;
mod tile;

use geode_shell::actions::{ActionDef, ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{
    CloseHandle, Delivery, FindEvent, ModuleFactory, StackHandle, TileContent, TileOccupant,
};
use geode_shell::tiling::TileId;
use gpui::{App, AppContext as _, Entity, SharedString, Window};
use tile::GuideTile;

const ACTIONS: &[(&str, &str)] = &[
    ("guide::menu", "Guide actions…"),
    ("guide::contents", "Guide contents…"),
    ("guide::next", "Next guide section"),
    ("guide::previous", "Previous guide section"),
    ("guide::down", "Scroll guide down"),
    ("guide::up", "Scroll guide up"),
    ("guide::page_down", "Page down in guide"),
    ("guide::page_up", "Page up in guide"),
    ("guide::top", "Top of guide section"),
    ("guide::bottom", "Bottom of guide section"),
    ("guide::next_match", "Next matching guide section"),
    ("guide::previous_match", "Previous matching guide section"),
    ("guide::pick", "Open guide chapter"),
    ("guide::cancel", "Close guide contents"),
    ("guide::copy", "Copy guide section"),
    ("guide::clear_find", "Clear guide search"),
];

const KEYMAP: &str = r#"
[[bindings]]
context = "guide && mode == normal"
[bindings.keys]
"." = "guide::menu"
"escape" = "guide::clear_find"
"c" = "guide::contents"
"[" = "guide::previous"
"]" = "guide::next"
"j" = "guide::down"
"down" = "guide::down"
"k" = "guide::up"
"up" = "guide::up"
"ctrl+d" = "guide::page_down"
"ctrl+u" = "guide::page_up"
"ctrl+f" = "guide::page_down"
"ctrl+b" = "guide::page_up"
"pagedown" = "guide::page_down"
"pageup" = "guide::page_up"
"g g" = "guide::top"
"home" = "guide::top"
"shift+g" = "guide::bottom"
"end" = "guide::bottom"
"n" = "guide::next_match"
"shift+n" = "guide::previous_match"
"ctrl+c" = "guide::copy"
"y" = "guide::copy"

[[bindings]]
context = "guide && mode == menu"
[bindings.keys]
"enter" = "guide::pick"
"escape" = "guide::cancel"
"c" = "guide::cancel"
"." = "guide::cancel"
"#;

struct GuideContent(Entity<GuideTile>);

impl TileContent for GuideContent {
    fn key_context(&self, cx: &App) -> KeyContext {
        self.0.read(cx).key_context()
    }
    fn dispatch(
        &self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.0
            .update(cx, |tile, cx| tile.dispatch(action, count, window, cx))
    }
    fn command(&self, line: &str, window: &mut Window, cx: &mut App) -> Result<(), String> {
        self.0.update(cx, |tile, cx| tile.command(line, window, cx))
    }
    fn completions(&self, line: &str, cursor: usize, _: &App) -> Vec<String> {
        let word = geode_shell::commandline::word_at(line, cursor);
        let before = line[..word.start].trim();
        if before == "section" {
            document::sections()
                .iter()
                .map(|s| s.anchor.to_string())
                .collect()
        } else if before.is_empty() {
            vec!["section".into()]
        } else {
            Vec::new()
        }
    }
    fn find(&self, event: FindEvent, window: &mut Window, cx: &mut App) {
        self.0.update(cx, |tile, cx| tile.find(event, window, cx));
    }
    fn deliver(&self, delivery: Delivery, _: &mut Window, _: &mut App) {
        // No query, source or file belongs to this bundled document.
        match delivery {
            Delivery::Query(_)
            | Delivery::Price(_)
            | Delivery::VolSlices(_)
            | Delivery::Series(_)
            | Delivery::SeriesFetched { .. }
            | Delivery::Upload(_)
            | Delivery::Distinct(_)
            | Delivery::TextFile(_) => {}
        }
    }
    fn set_visible(&self, visible: bool, cx: &mut App) {
        self.0.update(cx, |tile, cx| tile.set_visible(visible, cx));
    }
    fn closed(&self, cx: &mut App) {
        self.0.update(cx, |tile, cx| tile.set_visible(false, cx));
    }
    fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App) {
        self.0.update(cx, |tile, cx| {
            tile.stack = stack;
            cx.notify();
        });
    }
    fn set_close(&self, close: CloseHandle, cx: &mut App) {
        self.0.update(cx, |tile, cx| {
            tile.close = Some(close);
            cx.notify();
        });
    }
    fn title(&self, _: &App) -> SharedString {
        "User guide".into()
    }
    fn serialize(&self, cx: &App) -> toml::Table {
        let mut record = toml::Table::new();
        record.insert(
            "section".into(),
            document::sections()[self.0.read(cx).section]
                .anchor
                .to_string()
                .into(),
        );
        record
    }
}

/// Creates independent readers of the guide compiled into this build.
pub struct GuideFactory;

impl ModuleFactory for GuideFactory {
    fn kind(&self) -> &'static str {
        "guide"
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        for &(id, title) in ACTIONS {
            registry
                .register(ActionDef {
                    id: ActionId(id.into()),
                    title: title.into(),
                    category: "User guide".into(),
                })
                .expect("guide action ids are unique");
        }
    }
    fn default_keymap(&self) -> Option<&'static str> {
        Some(KEYMAP)
    }
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        _: Entity<Diagnostics>,
        _: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        let view = cx.new(|cx| GuideTile::new(tile, frame, restored, cx));
        TileOccupant {
            kind: self.kind(),
            view: view.clone().into(),
            content: Box::new(GuideContent(view)),
        }
    }
}

#[cfg(test)]
mod tests;
