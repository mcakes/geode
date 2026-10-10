use super::*;
use crate::content::{WatchlistConfig, WatchlistFactory};
use geode_core::config::Layer;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::scopes::SavedScopes;
use geode_core::watchlist::Watchlist;
use geode_core::watchlist::members::{Member, Origin};
use geode_core::watchlist::state::{Status, WatchlistState};
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

/// A resolved list of `names`, every one manual, from the desk layer.
fn list(names: &[&str]) -> WatchlistState {
    WatchlistState {
        definition: Watchlist {
            include: names.iter().map(|n| n.to_string()).collect(),
            ..Watchlist::default()
        },
        layer: Some(Layer::Desk),
        shadowed: None,
        rule_errors: vec![],
        members: names
            .iter()
            .map(|n| Member {
                name: n.to_string(),
                origin: Origin::Manual,
            })
            .collect(),
        resolved_at: None,
        status: Status::Current,
    }
}

/// A snapshot holding `lists`, written out of alphabetical order so the
/// switcher's order is its own.
fn snapshot(lists: &[(&str, &[&str])]) -> WatchlistSnapshot {
    let mut snap = WatchlistSnapshot::default();
    for (name, names) in lists.iter().rev() {
        snap.lists.insert(name.to_string(), list(names));
    }
    snap
}

fn two() -> WatchlistSnapshot {
    snapshot(&[("a", &["SPX", "NDX"]), ("b", &["DAX"])])
}

fn publish(cx: &mut gpui::App, snap: WatchlistSnapshot) {
    cx.set_global(WatchlistGlobal(Arc::new(snap)));
}

/// What the shell root is to a tile, for focus and for keys: a
/// `track_focus`ed ancestor, and the shell's normal-mode route. A keystroke
/// that reaches this element's listener is matched against the real keymap
/// (the builtin layer with this module's fragment spliced in) under the
/// tile's live key context, and dispatched through the tile's own door, so
/// `simulate_keystrokes` drives the tile the way a trader's keys do.
struct ShellStandIn {
    focus: gpui::FocusHandle,
    tile: Entity<WatchlistTile>,
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
                // `menu` is a fieldless list popup and `visual` a live row
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

/// The keymap the running app resolves this tile's keys through.
fn app_keymap(factory: &Rc<WatchlistFactory>) -> Keymap {
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
    tile: Entity<WatchlistTile>,
    shell_focus: gpui::FocusHandle,
}

struct Harness {
    tile: Entity<WatchlistTile>,
    /// Driven through the trait: the shell's own door is what a key and a
    /// title read arrive through.
    content: Rc<dyn TileContent>,
    factory: Rc<WatchlistFactory>,
    /// The keymap the stand-in resolves keys through and the `Chords`
    /// global is published from.
    keymap: Rc<Keymap>,
}

/// A tile built by the factory after `snap` was published and a default
/// config pushed, with `restored` as its record, hosted under the shell
/// stand-in in a `Root`.
fn open_with(
    cx: &mut gpui::TestAppContext,
    snap: WatchlistSnapshot,
    restored: Option<toml::Table>,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::init);
    cx.update(|cx| publish(cx, snap));
    let factory = Rc::new(WatchlistFactory::new());
    cx.update(|cx| factory.set_config(WatchlistConfig::default(), cx));
    let keymap = Rc::new(app_keymap(&factory));
    // The shell publishes the keymap the menus' hints and the empty
    // state's chord resolve against.
    cx.update(|cx| {
        cx.set_global(geode_shell::tips::Chords(Arc::new(
            keymap.bindings().to_vec(),
        )))
    });
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
                    FrameRef::for_tile(frame.clone(), WorkspaceIx::FIRST, TileId(TILE)),
                    diagnostics,
                    window,
                    cx,
                );
                assert_eq!(occupant.kind, crate::KIND);
                let tile = occupant.view.clone().downcast::<WatchlistTile>().unwrap();
                let shell_focus = cx.focus_handle();
                let content: Rc<dyn TileContent> = occupant.content.into();
                let host = cx.new(|_| ShellStandIn {
                    focus: shell_focus.clone(),
                    tile: tile.clone(),
                    keymap: keymap.clone(),
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
        // The shell's render tells the focused tile it is, as it does a
        // fresh occupant on its first frame.
        built.content.set_focused(true, cx);
    });
    (
        Harness {
            tile: built.tile,
            content: built.content,
            factory,
            keymap,
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
    fn actions(&self, vcx: &gpui::VisualTestContext) -> Option<Vec<String>> {
        self.tile.read_with(vcx, |t, _| t.action_titles())
    }
    fn highlighted(&self, vcx: &gpui::VisualTestContext) -> Option<usize> {
        self.tile.read_with(vcx, |t, _| t.highlighted())
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
    fn notices(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.notice_texts())
    }
    fn mode(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.key_context().get("mode").map(str::to_string))
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
fn a_new_tile_opens_its_switcher_on_the_first_list(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, two(), None);
    assert_eq!(h.switcher(&vcx), rows(&["a", "b"]), "alphabetical");
    assert_eq!(h.highlighted(&vcx), Some(0), "open on the first list");
    assert_eq!(h.title(&mut vcx), "Watchlists");
    assert_eq!(h.mode(&vcx).as_deref(), Some("menu"));
    // Painted under the header.
    assert!(vcx.debug_bounds("watchlist-switcher").is_some());
    // The shared list step and the fragment's commit pick `b`.
    vcx.simulate_keystrokes("j enter");
    assert_eq!(h.switcher(&vcx), None, "the pick closes it");
    assert_eq!(h.title(&mut vcx), "Watchlist: b");
    assert_eq!(
        h.header(&vcx),
        "Watchlist: b \u{00b7} 1 name \u{00b7} 0 rules \u{00b7} desk"
    );
    assert_eq!(h.empty(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    // The pick is what the session saves.
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["name"].as_str(), Some("b"));
}

/// `g w` reopens the switcher with the shown list ticked and highlighted;
/// `escape` closes it and changes nothing.
#[gpui::test]
fn the_switch_key_opens_the_switcher_on_the_current_one(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, two(), restored("b"));
    assert_eq!(h.switcher(&vcx), None, "a restored tile lands on its list");
    assert_eq!(h.title(&mut vcx), "Watchlist: b");
    vcx.simulate_keystrokes("g w");
    assert_eq!(
        h.switcher(&vcx),
        Some(vec![("a".into(), false), ("b".into(), true)])
    );
    assert_eq!(h.highlighted(&vcx), Some(1));
    vcx.simulate_keystrokes("enter");
    assert_eq!(h.title(&mut vcx), "Watchlist: b");
    vcx.simulate_keystrokes("g w escape");
    assert_eq!(h.switcher(&vcx), None);
    assert_eq!(h.title(&mut vcx), "Watchlist: b");
}

/// The header's name is the switcher's pointer route.
#[gpui::test]
fn pressing_the_name_opens_the_switcher(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, two(), restored("a"));
    h.draw(&mut vcx);
    let name = vcx
        .debug_bounds("watchlist-switch-7")
        .expect("the name is painted");
    vcx.simulate_mouse_down(
        name.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(h.switcher(&vcx).is_some());
    // The layer is badged.
    assert!(vcx.debug_bounds("watchlist-layer-7-a").is_some());
}

#[gpui::test]
fn a_snapshot_change_removing_the_shown_list_shows_the_empty_state_and_opens_the_switcher(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_with(cx, two(), restored("b"));
    assert_eq!(h.switcher(&vcx), None);
    vcx.update(|_, cx| publish(cx, snapshot(&[("a", &["SPX"])])));
    let empty = h.empty(&vcx).expect("an empty state");
    assert_eq!(empty, "b no longer exists \u{2014} g w switches");
    assert_eq!(h.switcher(&vcx), rows(&["a"]));
    assert_eq!(h.title(&mut vcx), "Watchlists");
    // The chord is the keymap's, live: unbound, the palette title names
    // the route.
    vcx.update(|_, cx| cx.set_global(geode_shell::tips::Chords(Arc::new(Vec::new()))));
    assert_eq!(
        h.empty(&vcx).as_deref(),
        Some("b no longer exists \u{2014} Watchlist: Switch switches")
    );
    vcx.update(|_, cx| {
        cx.set_global(geode_shell::tips::Chords(Arc::new(
            h.keymap.bindings().to_vec(),
        )))
    });
    assert_eq!(h.empty(&vcx).as_deref(), Some(&*empty));
    // Every list gone: the empty state names the verb that makes one, and
    // there is no switcher to open.
    vcx.update(|_, cx| publish(cx, WatchlistSnapshot::default()));
    assert_eq!(
        h.empty(&vcx).as_deref(),
        Some("b no longer exists \u{2014} Watchlist: New\u{2026} creates one")
    );
    assert_eq!(h.switcher(&vcx), None);
    vcx.simulate_keystrokes("g w");
    assert_eq!(h.notices(&vcx), vec![NOTHING_TO_SWITCH.to_string()]);
}

/// A tile restored before the bridge publishes the lists (the app opens
/// the window first) opens its switcher once they arrive, if its list is
/// not among them; and lands on its list without one if it is.
#[gpui::test]
fn a_tile_restored_before_the_first_snapshot_settles_on_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, WatchlistSnapshot::default(), restored("b"));
    assert_eq!(h.switcher(&vcx), None);
    assert_eq!(
        h.empty(&vcx).as_deref(),
        Some("b no longer exists \u{2014} Watchlist: New\u{2026} creates one")
    );
    vcx.update(|_, cx| publish(cx, two()));
    assert_eq!(h.switcher(&vcx), None, "its list is there");
    assert_eq!(h.title(&mut vcx), "Watchlist: b");
    let (h, mut vcx) = open_with(cx, WatchlistSnapshot::default(), restored("c"));
    vcx.update(|_, cx| publish(cx, two()));
    assert_eq!(h.switcher(&vcx), rows(&["a", "b"]));
    assert_eq!(
        h.empty(&vcx).as_deref(),
        Some("c no longer exists \u{2014} g w switches")
    );
}

/// A reload that keeps the list updates its header in place, and leaves
/// a closed switcher closed.
#[gpui::test]
fn a_snapshot_change_refreshes_the_header(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, two(), restored("a"));
    assert!(h.header(&vcx).contains("2 names"), "{}", h.header(&vcx));
    let mut wider = two();
    wider.lists.insert("a".into(), list(&["SPX", "NDX", "RTY"]));
    wider.lists.get_mut("a").unwrap().status = Status::Resolving;
    vcx.update(|_, cx| publish(cx, wider));
    let header = h.header(&vcx);
    assert!(header.contains("3 names"), "{header}");
    assert!(header.contains(header::RESOLVING), "{header}");
    assert_eq!(h.switcher(&vcx), None);
    assert_eq!(h.title(&mut vcx), "Watchlist: a");
    // A config push changes nothing shown.
    vcx.update(|_, cx| h.factory.set_config(WatchlistConfig::default(), cx));
    assert_eq!(h.header(&vcx), header);
}

/// `.` opens the `⋯` menu with the switcher and the list's own verbs; a
/// verb not built yet says so as a status notice.
#[gpui::test]
fn the_actions_menu_lists_the_verbs_and_a_pick_says_not_yet(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, two(), restored("a"));
    vcx.simulate_keystrokes(".");
    assert_eq!(
        h.actions(&vcx),
        Some(
            [
                "Switch\u{2026}",
                "New\u{2026}",
                "Clone\u{2026}",
                "Rename\u{2026}",
                "Delete\u{2026}",
                "Revert\u{2026}"
            ]
            .map(str::to_string)
            .to_vec()
        )
    );
    assert_eq!(h.mode(&vcx).as_deref(), Some("menu"));
    // `.` again closes it; `escape` too.
    vcx.simulate_keystrokes(".");
    assert_eq!(h.actions(&vcx), None);
    vcx.simulate_keystrokes(". escape");
    assert_eq!(h.actions(&vcx), None);
    // New… picked from the menu.
    vcx.simulate_keystrokes(". j enter");
    assert_eq!(h.actions(&vcx), None);
    assert_eq!(h.notices(&vcx), vec![NOT_YET.to_string()]);
    // The Switch row opens the switcher.
    vcx.simulate_keystrokes(". enter");
    assert_eq!(
        h.switcher(&vcx),
        Some(vec![("a".into(), true), ("b".into(), false)])
    );
}

/// The session restore's notices last until the trader's first key.
#[gpui::test]
fn an_unreadable_session_key_is_noticed_until_the_first_key(cx: &mut gpui::TestAppContext) {
    let mut table = restored("a").unwrap();
    table.insert("cursor".into(), toml::Value::Integer(3));
    let (h, mut vcx) = open_with(cx, two(), Some(table));
    let notices = h.notices(&vcx);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with("session: dropped cursor"),
        "{notices:?}"
    );
    assert_eq!(h.title(&mut vcx), "Watchlist: a", "the rest is kept");
    vcx.simulate_keystrokes("g w");
    assert!(h.notices(&vcx).is_empty());
}
