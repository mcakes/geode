use super::*;
use crate::content::{ClassificationsConfig, ClassificationsFactory};
use geode_core::config::{Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::scopes::SavedScopes;
use geode_data::DataHandle;
use geode_shell::actions::ActionRegistry;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameRef};
use geode_shell::keymap::{KeyContext, Keymap, MatchResult, Matcher, build_keymap};
use geode_shell::module::{ModuleFactory, ModuleRoster, TileContent};
use geode_shell::tiling::{TileId, WorkspaceIx};
use gpui::{Entity, Window};
use std::cell::RefCell;
use std::rc::Rc;

const TILE: u64 = 7;

/// Two classifications, written out of alphabetical order so the
/// switcher's order is its own.
const TWO: &str = r#"
[region]
from = "underlying_ref"
[region.values]
Europe = ["SX5E", "DAX"]

[desk]
from = "book"
[desk.values]
Flow = ["B1"]
"#;

fn config(dims: &str) -> ClassificationsConfig {
    let doc = merge_docs(
        "dimensions",
        &[LayerDoc::builtin("dimensions", dims).expect("the dimensions parse")],
    );
    let (dims, diags) = DerivedDimensions::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    let layers = dims.all().map(|d| (d.name.clone(), Layer::Desk)).collect();
    ClassificationsConfig {
        dims,
        layers,
        ..ClassificationsConfig::default()
    }
}

/// What the shell root is to a tile, for focus and for keys: a
/// `track_focus`ed ancestor, and the shell's normal-mode route. A keystroke
/// that reaches this element's listener is matched against the real keymap
/// (the builtin layer with this module's fragment spliced in) under the
/// tile's live key context, and dispatched through the tile's own door, so
/// `simulate_keystrokes` drives the tile the way a trader's keys do.
struct ShellStandIn {
    focus: gpui::FocusHandle,
    tile: Entity<ClassificationsTile>,
    keymap: Rc<Keymap>,
    matcher: Matcher,
}

impl gpui::Render for ShellStandIn {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
        gpui::div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let context = this.tile.read(cx).key_context();
                let Some(keystroke) = geode_shell::shell::keys::convert_keystroke(&event.keystroke)
                else {
                    return;
                };
                // `menu` is a fieldless list popup: the shell routes it as
                // normal mode, over the whole stack.
                let stack = match context.get("mode") {
                    Some("normal") | Some("menu") => vec![
                        KeyContext::new("workspace"),
                        KeyContext::new("tile"),
                        context,
                    ],
                    Some("insert") if !keystroke.mods.is_chord() => vec![context],
                    _ => return,
                };
                if let MatchResult::Matched { action, count } =
                    this.matcher.press(&this.keymap, keystroke, &stack)
                {
                    this.tile
                        .update(cx, |t, cx| t.dispatch(&action, count, window, cx));
                    cx.stop_propagation();
                }
            }))
            .child(self.tile.clone())
    }
}

/// The keymap the running app resolves this tile's keys through.
fn app_keymap(factory: &Rc<ClassificationsFactory>) -> Keymap {
    let mut roster = ModuleRoster::new();
    roster.add(Box::new(factory.clone()));
    let (fragments, diags) = roster.keymap_fragments();
    assert!(diags.is_empty(), "{diags:?}");
    let builtin =
        geode_core::config::LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP)
            .expect("the builtin keymap parses");
    let docs = geode_shell::keymap::fragments::splice(&[builtin], &fragments);
    let mut registry = ActionRegistry::default();
    geode_shell::defaults::register_builtin_actions(&mut registry);
    factory.register_actions(&mut registry);
    let (keymap, diags) = build_keymap(&docs, geode_shell::defaults::default_mod(), &registry);
    assert!(diags.is_empty(), "{diags:?}");
    keymap
}

struct Built {
    content: Rc<dyn TileContent>,
    tile: Entity<ClassificationsTile>,
    shell_focus: gpui::FocusHandle,
}

struct Harness {
    tile: Entity<ClassificationsTile>,
    /// Driven through the trait: the shell's own door is what a key and a
    /// title read arrive through.
    content: Rc<dyn TileContent>,
    factory: Rc<ClassificationsFactory>,
}

/// A tile built by the factory after `config` was pushed, with `restored`
/// as its record, hosted under the shell stand-in in a `Root`.
fn open_with(
    cx: &mut gpui::TestAppContext,
    config: ClassificationsConfig,
    restored: Option<toml::Table>,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    let (data, _rx) = DataHandle::for_tests();
    let factory = Rc::new(ClassificationsFactory::new(data));
    cx.update(|cx| factory.set_config(config, cx));
    let keymap = Rc::new(app_keymap(&factory));
    let slot: Rc<RefCell<Option<Built>>> = Rc::new(RefCell::new(None));
    let window = cx
        .update(|cx| {
            let slot = slot.clone();
            let factory = factory.clone();
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let frame =
                    cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                let occupant = factory.create(
                    TileId(TILE),
                    restored.as_ref(),
                    FrameRef::new(frame, WorkspaceIx::FIRST),
                    diagnostics,
                    window,
                    cx,
                );
                assert_eq!(occupant.kind, crate::KIND);
                let tile = occupant
                    .view
                    .clone()
                    .downcast::<ClassificationsTile>()
                    .unwrap();
                let shell_focus = cx.focus_handle();
                let content: Rc<dyn TileContent> = occupant.content.into();
                let host = cx.new(|_| ShellStandIn {
                    focus: shell_focus.clone(),
                    tile: tile.clone(),
                    keymap,
                    matcher: Matcher::default(),
                });
                *slot.borrow_mut() = Some(Built {
                    content,
                    tile,
                    shell_focus,
                });
                cx.new(|cx| gpui_component::Root::new(host, window, cx))
            })
        })
        .unwrap();
    let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
    let built = slot.borrow_mut().take().expect("the factory built one");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
        built.shell_focus.focus(window, cx);
    });
    (
        Harness {
            tile: built.tile,
            content: built.content,
            factory,
        },
        vcx,
    )
}

fn restored(name: &str) -> Option<toml::Table> {
    Some(crate::core::session::to_table(
        &crate::core::session::State {
            name: Some(name.into()),
            ..Default::default()
        },
    ))
}

impl Harness {
    fn switcher(&self, vcx: &gpui::VisualTestContext) -> Option<Vec<(String, bool)>> {
        self.tile.read_with(vcx, |t, _| t.switcher_rows())
    }
    fn header(&self, vcx: &gpui::VisualTestContext) -> String {
        self.tile.read_with(vcx, |t, _| t.title_text())
    }
    fn empty(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.empty_text())
    }
    fn title(&self, vcx: &mut gpui::VisualTestContext) -> String {
        vcx.update(|_, cx| self.content.title(cx).to_string())
    }
    fn draw(&self, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

fn rows(names: &[&str]) -> Option<Vec<(String, bool)>> {
    Some(names.iter().map(|n| (n.to_string(), false)).collect())
}

#[gpui::test]
fn a_new_tile_opens_the_switcher_over_every_classification(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), None);
    assert_eq!(h.switcher(&vcx), rows(&["desk", "region"]), "alphabetical");
    assert_eq!(h.title(&mut vcx), "Classifications");
    // Painted under the header, in menu mode.
    assert!(vcx.debug_bounds("classifications-switcher").is_some());
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.key_context().get("mode").map(str::to_string)),
        Some("menu".to_string())
    );
}

#[gpui::test]
fn picking_from_the_switcher_shows_that_classification(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), None);
    vcx.simulate_keystrokes("j enter");
    assert_eq!(h.switcher(&vcx), None, "the pick closes it");
    let header = h.header(&vcx);
    assert!(header.contains("region"), "{header}");
    assert!(header.contains("underlying_ref"), "{header}");
    assert!(header.contains("2 values"), "{header}");
    assert_eq!(h.title(&mut vcx), "Classification: region");
    assert_eq!(h.empty(&vcx), None);
    // The pick is what the session saves.
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["name"].as_str(), Some("region"));
}

#[gpui::test]
fn a_restored_tile_lands_on_its_classification(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("desk"));
    assert_eq!(h.switcher(&vcx), None);
    let header = h.header(&vcx);
    assert!(
        header.contains("desk") && header.contains("book"),
        "{header}"
    );
    assert!(header.contains("1 value"), "{header}");
    assert_eq!(h.title(&mut vcx), "Classification: desk");
    // The winning layer is badged.
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("classifications-layer-7-desk").is_some());
}

/// `g c` reopens the switcher with the shown classification ticked and
/// highlighted; `escape` closes it and changes nothing.
#[gpui::test]
fn the_switch_key_opens_the_switcher_on_the_current_one(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    vcx.simulate_keystrokes("g c");
    assert_eq!(
        h.switcher(&vcx),
        Some(vec![("desk".into(), false), ("region".into(), true)])
    );
    // Highlight starts on the current row: `enter` keeps it.
    vcx.simulate_keystrokes("enter");
    assert_eq!(h.title(&mut vcx), "Classification: region");
    vcx.simulate_keystrokes("g c escape");
    assert_eq!(h.switcher(&vcx), None);
    assert_eq!(h.title(&mut vcx), "Classification: region");
}

/// The header's name is the switcher's pointer route.
#[gpui::test]
fn pressing_the_name_opens_the_switcher(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("desk"));
    h.draw(&mut vcx);
    let name = vcx
        .debug_bounds("classifications-switch-7")
        .expect("the name is painted");
    vcx.simulate_mouse_down(
        name.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(h.switcher(&vcx).is_some());
}

#[gpui::test]
fn a_classification_removed_by_a_reload_leaves_an_empty_state_naming_it(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    assert_eq!(h.switcher(&vcx), None);
    let desk_only = "[desk]\nfrom = \"book\"\n";
    vcx.update(|_, cx| h.factory.set_config(config(desk_only), cx));
    let empty = h.empty(&vcx).expect("an empty state");
    assert!(empty.contains("region no longer exists"), "{empty}");
    assert_eq!(h.switcher(&vcx), rows(&["desk"]));
    assert_eq!(h.title(&mut vcx), "Classifications");
}

/// A reload that keeps the classification updates its header in place.
#[gpui::test]
fn a_reload_refreshes_the_header(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_with(cx, config(TWO), restored("desk"));
    let wider = "[desk]\nfrom = \"book\"\n[desk.values]\nFlow = [\"B1\", \"B2\", \"B3\"]\n";
    let mut vcx = vcx;
    vcx.update(|_, cx| h.factory.set_config(config(wider), cx));
    let header = h.header(&vcx);
    assert!(header.contains("3 values"), "{header}");
    assert_eq!(h.switcher(&vcx), None, "nothing to pick: it still exists");
}

#[gpui::test]
fn with_no_classifications_the_tile_says_how_to_make_one(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(""), None);
    let empty = h.empty(&vcx).expect("an empty state");
    assert!(empty.contains("Classification: New\u{2026}"), "{empty}");
    assert_eq!(h.switcher(&vcx), None, "nothing to switch to");
    // `g c` says why rather than painting an empty list.
    vcx.simulate_keystrokes("g c");
    assert_eq!(h.switcher(&vcx), None);
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("classifications-empty-7").is_some());
}
