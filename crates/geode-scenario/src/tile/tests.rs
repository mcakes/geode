//! The window harness: a tile built by its factory, hosted under a shell
//! stand-in that routes keys through the production keymap, and driven
//! only through the shell's doors.

mod keys;

use super::*;
use crate::content::ScenarioFactory;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::scopes::SavedScopes;
use geode_data::{DataHandle, Request};
use geode_shell::actions::ActionRegistry;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameRef};
use geode_shell::keymap::{KeyContext, Keymap, MatchResult, Matcher, build_keymap};
use geode_shell::module::{ModuleFactory, ModuleRoster, TileContent};
use geode_shell::tiling::{TileId, WorkspaceIx};
use gpui::{Entity, SharedString, Window};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

const TILE: u64 = 7;

/// What the shell root is to a tile, for focus and for keys.
///
/// Focus: a `track_focus`ed ancestor, so a press that would move focus has
/// somewhere to move it, as in the app.
///
/// Keys: the shell's route. A keystroke that reaches this element's
/// listener is converted, matched against the real keymap (the builtin
/// layer with this module's fragment spliced in) under the tile's live key
/// context, and dispatched through the tile's own door, so
/// `simulate_keystrokes` drives the tile the way a trader's keys do and a
/// key that reaches no binding reaches nothing. `normal`, `visual` (a live
/// selection) and `menu` (a fieldless list) all route over the whole
/// stack.
///
/// Draw-time doors: the shell tells an occupant its visibility and focus,
/// and that it is closing, from inside its own render, where a notify is
/// dropped. A test queues any of them here and the next draw delivers it
/// from this render, on the path production takes. Like the shell's, this
/// render reads the frame.
struct ShellStandIn {
    frame: Entity<Frame>,
    focus: gpui::FocusHandle,
    tile: Entity<ScenarioTile>,
    content: Rc<dyn TileContent>,
    keymap: Rc<Keymap>,
    matcher: Matcher,
    pending_visible: Option<bool>,
    pending_focused: Option<bool>,
    pending_closed: bool,
}

impl gpui::Render for ShellStandIn {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
        let _ = self.frame.read(cx).data_version();
        if let Some(visible) = self.pending_visible.take() {
            self.content.set_visible(visible, cx);
        }
        if let Some(focused) = self.pending_focused.take() {
            self.content.set_focused(focused, cx);
        }
        if std::mem::take(&mut self.pending_closed) {
            self.content.closed(cx);
        }
        gpui::div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let context = this.tile.read(cx).key_context(cx);
                let Some(keystroke) = geode_shell::shell::keys::convert_keystroke(&event.keystroke)
                else {
                    return;
                };
                // `menu` is a fieldless list popup and `visual` a live
                // selection: the shell routes both as normal mode, over the
                // whole stack.
                let stack = match context.get("mode") {
                    Some("normal") | Some("visual") | Some("menu") => vec![
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

/// The keymap the running app resolves this tile's keys through: the
/// builtin layer with this module's fragment spliced in, over the builtin
/// actions and this module's own.
fn app_keymap(factory: &Rc<ScenarioFactory>) -> Keymap {
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

/// What the window closure hands back: it can return only one value, so
/// everything a test drives or reads is parked here on the way out.
struct Built {
    content: Rc<dyn TileContent>,
    host: Entity<ShellStandIn>,
    tile: Entity<ScenarioTile>,
    frame: Entity<Frame>,
    diagnostics: Entity<Diagnostics>,
    shell_focus: gpui::FocusHandle,
}

// Every shell door is kept, so each test file drives the same set; the
// key tests use only some of them.
#[allow(dead_code)]
struct Harness {
    tile: Entity<ScenarioTile>,
    /// Driven through the trait, never by poking the entity: the shell's
    /// own door is what a key, a `:` line and a delivery all arrive through.
    content: Rc<dyn TileContent>,
    /// The shell stand-in, for the draw-time doors.
    host: Entity<ShellStandIn>,
    factory: Rc<ScenarioFactory>,
    /// The frame entity the tile's handle reads, for the shell doors a
    /// test drives (`link_for_test`, `post_for_test`, `open_flip`).
    frame: Entity<Frame>,
    /// The factory's data handle, for the refusal knob.
    data: DataHandle,
    /// The diagnostics entity the tile reads its health from.
    diagnostics: Entity<Diagnostics>,
    /// Every `Request` the tile submitted, in order. Held for the harness's
    /// whole life: dropping the receiver closes the channel and
    /// `DataHandle::send` starts refusing.
    rx: RefCell<Option<Receiver<Request>>>,
}

/// The default kind the harness opens.
const KIND: &str = "vega_kt";

fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    open_shape(cx, KIND)
}

/// A builtin shape as the app hands it over: checked against the demo
/// schema, axes and twins filled.
fn open_shape(cx: &mut gpui::TestAppContext, name: &str) -> (Harness, gpui::VisualTestContext) {
    open_framed(
        cx,
        crate::core::spec::builtin_shape_checked(name),
        None,
        |frame| FrameRef::new(frame, WorkspaceIx::FIRST),
    )
}

/// A fixture shape (the pivot fixtures' own, already checked).
fn open_with_shape(
    cx: &mut gpui::TestAppContext,
    shape: geode_core::scenario::ShapeSpec,
) -> (Harness, gpui::VisualTestContext) {
    open_framed(cx, Arc::new(shape), None, |frame| {
        FrameRef::new(frame, WorkspaceIx::FIRST)
    })
}

/// A tile over `shape` built by the factory with `restored` as its record
/// and its frame handle built by `bind`, hosted under the shell stand-in
/// in a `Root`.
fn open_framed(
    cx: &mut gpui::TestAppContext,
    shape: Arc<geode_core::scenario::ShapeSpec>,
    restored: Option<toml::Table>,
    bind: fn(Entity<Frame>) -> FrameRef,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::init);
    let (data, rx) = DataHandle::for_tests();
    let factory = Rc::new(ScenarioFactory::new(
        data.clone(),
        shape.clone(),
        Duration::from_secs(60),
    ));
    let keymap = Rc::new(app_keymap(&factory));
    let slot: Rc<RefCell<Option<Built>>> = Rc::new(RefCell::new(None));
    let window = cx
        .update(|cx| {
            let slot = slot.clone();
            let factory = factory.clone();
            let keymap = keymap.clone();
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let frame =
                    cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                let occupant = factory.create(
                    TileId(TILE),
                    restored.as_ref(),
                    bind(frame.clone()),
                    diagnostics.clone(),
                    window,
                    cx,
                );
                assert_eq!(occupant.kind, shape.kind);
                let tile = occupant.view.clone().downcast::<ScenarioTile>().unwrap();
                let shell_focus = cx.focus_handle();
                let content: Rc<dyn TileContent> = occupant.content.into();
                let host = cx.new(|_| ShellStandIn {
                    frame: frame.clone(),
                    focus: shell_focus.clone(),
                    tile: tile.clone(),
                    content: content.clone(),
                    keymap,
                    matcher: Matcher::default(),
                    pending_visible: None,
                    pending_focused: None,
                    pending_closed: false,
                });
                *slot.borrow_mut() = Some(Built {
                    content,
                    host: host.clone(),
                    tile,
                    frame: frame.clone(),
                    diagnostics,
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
            host: built.host,
            factory,
            frame: built.frame,
            data,
            diagnostics: built.diagnostics,
            rx: RefCell::new(Some(rx)),
        },
        vcx,
    )
}

#[allow(dead_code)]
impl Harness {
    /// Show the tile and answer its reads with a 4 × 3 vega K/T picture.
    /// The read chain is not wired yet, so the tests that need a picture
    /// are ignored and this is never reached.
    fn show_picture(&self, _vcx: &mut gpui::VisualTestContext) {
        unimplemented!("the read chain supplies the picture")
    }

    /// Deliver visibility and focus as the shell does: from inside its
    /// render, on the next draw.
    fn in_draw(
        &self,
        vcx: &mut gpui::VisualTestContext,
        visible: Option<bool>,
        focused: Option<bool>,
    ) {
        self.host.update(vcx, |h, cx| {
            h.pending_visible = visible;
            h.pending_focused = focused;
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// Close the tile as the shell does: from inside its render, on the
    /// next draw.
    fn close_in_draw(&self, vcx: &mut gpui::VisualTestContext) {
        self.host.update(vcx, |h, cx| {
            h.pending_closed = true;
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// Everything submitted since the last drain.
    fn requests(&self) -> Vec<Request> {
        match self.rx.borrow().as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => Vec::new(),
        }
    }

    fn title(&self, vcx: &mut gpui::VisualTestContext) -> SharedString {
        vcx.update(|_, cx| self.content.title(cx))
    }

    /// The action ids the tile's dispatch door received, in order.
    fn dispatched(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| {
            t.dispatch_log.iter().map(|a| a.0.clone()).collect()
        })
    }

    fn state(&self, vcx: &gpui::VisualTestContext) -> State {
        self.tile.read_with(vcx, |t, _| t.state.clone())
    }

    fn delegate<R>(
        &self,
        vcx: &gpui::VisualTestContext,
        f: impl FnOnce(&ScenarioDelegate) -> R,
    ) -> R {
        self.tile
            .read_with(vcx, |t, cx| f(t.table.read(cx).delegate()))
    }
}
