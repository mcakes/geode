use super::*;
use crate::content::VolsliceFactory;
use geode_core::context::DimensionContext;
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

const TILE: u64 = 7;

/// What the shell root is to a tile, for focus and for keys.
///
/// Focus: a `track_focus`ed ancestor. gpui's `div` answers a mouse-down's
/// bubble phase on such an element by focusing it unless a listener called
/// `prevent_default`, so a popup a tile opens and focuses from a press loses
/// the keyboard to the root a moment later. Without this ancestor the
/// harness has nothing to steal focus and cannot see that loss.
///
/// Keys: the shell's normal-mode route. A keystroke that reaches this
/// element's listener is converted, matched against the real keymap (the
/// builtin layer with this module's fragment spliced in) under the tile's
/// live key context, and dispatched through the tile's own door, so
/// `simulate_keystrokes` drives the tile the way a trader's keys do and a
/// key that reaches no binding reaches nothing. In `insert` mode it follows
/// the shell's insert branch for bare keys: they resolve against the tile
/// context alone, so a field's `enter` and `escape` reach its verbs and
/// typing stays text. Chords in insert mode are not modelled.
///
/// Draw-time doors: the shell tells an occupant its visibility and focus,
/// and that it is closing, from inside its own render, where a notify is
/// dropped. A test queues any of them here and the next draw delivers it
/// from this render, so the tile is exercised on the path production takes.
///
/// Like the shell's, this render reads the frame, so the window tracks it
/// and a notify the frame is sent during a draw is dropped here as it is in
/// the app: a frame write from a draw-time door is heard only if deferred.
struct ShellStandIn {
    frame: Entity<Frame>,
    focus: gpui::FocusHandle,
    tile: Entity<VolsliceTile>,
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

/// The keymap the running app resolves this tile's keys through: the
/// builtin layer with this module's fragment spliced in, over the builtin
/// actions and this module's own.
fn app_keymap(factory: &Rc<VolsliceFactory>) -> Keymap {
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
    tile: Entity<VolsliceTile>,
    frame: Entity<Frame>,
    diagnostics: Entity<Diagnostics>,
    shell_focus: gpui::FocusHandle,
}

struct Harness {
    tile: Entity<VolsliceTile>,
    /// Driven through the trait, never by poking the entity: the shell's
    /// own door is what a key, a `:` line and a delivery all arrive through.
    content: Rc<dyn TileContent>,
    /// The shell stand-in, for the draw-time doors.
    host: Entity<ShellStandIn>,
    factory: Rc<VolsliceFactory>,
    /// The frame entity the tile's handle reads, for the shell doors a
    /// test drives (`link_for_test`, `post_for_test`, `open_flip`).
    frame: Entity<Frame>,
    /// The factory's data handle, for the refusal knob.
    data: DataHandle,
    /// The diagnostics entity the tile reads its catalog and health from.
    diagnostics: Entity<Diagnostics>,
    /// Every `Request` the tile submitted, in order. Held for the harness's
    /// whole life: dropping the receiver closes the channel and
    /// `DataHandle::send` starts refusing.
    rx: RefCell<Option<Receiver<Request>>>,
}

fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    open_framed(cx, None, |frame| FrameRef::new(frame, WorkspaceIx::FIRST))
}

/// A tile built by the factory with `restored` as its record and its frame
/// handle built by `bind`, hosted under the shell stand-in in a `Root`.
fn open_framed(
    cx: &mut gpui::TestAppContext,
    restored: Option<toml::Table>,
    bind: fn(Entity<Frame>) -> FrameRef,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    let (data, rx) = DataHandle::for_tests();
    let factory = Rc::new(VolsliceFactory::new(data.clone()));
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
                assert_eq!(occupant.kind, crate::KIND);
                let tile = occupant.view.clone().downcast::<VolsliceTile>().unwrap();
                let shell_focus = cx.focus_handle();
                let content: Rc<dyn TileContent> = occupant.content.into();
                // Wrapped in `Root`, as `main.rs` wraps the shell: a tile
                // that opens a field needs one for focus to behave here as
                // it does in the app.
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

impl Harness {
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
}

/// The key context opts out of counts, so a bare digit is the kind toggle
/// it is bound to. Opted in, the matcher would take `2` as a pending count
/// and the toggle would never fire.
#[gpui::test]
fn bare_digits_reach_the_tile_not_a_count(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    vcx.simulate_keystrokes("2");
    assert_eq!(h.dispatched(&vcx), vec!["volslice::kind_2".to_string()]);
    vcx.simulate_keystrokes("9 x");
    assert_eq!(
        h.dispatched(&vcx),
        [
            "volslice::kind_2",
            "volslice::kind_9",
            "volslice::coordinate"
        ]
    );
}

/// The shell offers the follow rows only to a tile whose content answers
/// `follows()` right after create; `tile::open_with` offers the kind on an
/// underlying and restores it from the factory's launch table.
#[gpui::test]
fn the_factory_follows_and_accepts_an_underlying(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    assert!(h.content.follows());
    assert!(!h.content.emits());
    assert!(h.factory.accepts().contains(&"underlying_ref"));
    let state = h
        .factory
        .launch_state(&DimensionContext::of(&[("underlying_ref", "SPX.Z")]))
        .expect("a state for an underlying");
    assert_eq!(
        state.get("underlying"),
        Some(&toml::Value::String("SPX.Z".into()))
    );
    assert_eq!(h.factory.launch_state(&DimensionContext::default()), None);
    assert_eq!(h.title(&mut vcx).as_ref(), "vol slice");
}

/// A fresh tile is hidden until the shell says otherwise, so creating one
/// asks the data tier nothing, and it paints its empty state.
#[gpui::test]
fn a_fresh_tile_asks_nothing_and_shows_no_underlying(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    assert!(h.requests().is_empty());
    assert!(
        vcx.debug_bounds("volslice-empty-7").is_some(),
        "the empty state is painted"
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.empty_text()),
        SharedString::new_static("no underlying")
    );
}

/// The header paints the shell's close button last once the handle is
/// delivered, and a press runs it.
#[gpui::test]
fn the_header_paints_the_close_button(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    let pressed = std::rc::Rc::new(std::cell::Cell::new(false));
    // Delivered through the shell's door, `TileContent::set_close`.
    let close = {
        let pressed = pressed.clone();
        geode_shell::module::CloseHandle::new(move |_, _| pressed.set(true))
    };
    vcx.update(|_, cx| h.content.set_close(close, cx));
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let x = vcx.debug_bounds("tile-close-7").expect("painted");
    let header = vcx.debug_bounds("volslice-header-7").unwrap();
    assert!(
        header.right() - x.right() < gpui::px(20.0),
        "last in the strip"
    );
    vcx.simulate_mouse_down(x.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
    assert!(pressed.get());
}

mod data;
