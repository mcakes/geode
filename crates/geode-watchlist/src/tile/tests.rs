use super::*;
use crate::content::{WatchlistConfig, WatchlistFactory};
use geode_core::config::Layer;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::query::ReferenceTable;
use geode_core::reference::ReferenceData;
use geode_core::scopes::SavedScopes;
use geode_core::watchlist::members::{Member, Origin};
use geode_core::watchlist::state::{Status, WatchlistState};
use geode_core::watchlist::{Rule, Watchlist};
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

/// `europe`: two rules, rule 1 supplying DAX and SPX, rule 2 SPX and UKX;
/// NDX included by hand, UKX excluded by hand.
fn resolved() -> WatchlistState {
    let member = |name: &str, origin: Origin| Member {
        name: name.into(),
        origin,
    };
    WatchlistState {
        definition: Watchlist {
            include: vec!["NDX".into()],
            exclude: vec!["UKX".into()],
            rules: vec![Rule::default(), Rule::default()],
        },
        layer: Some(Layer::Desk),
        shadowed: None,
        rule_errors: vec![],
        members: vec![
            member("DAX", Origin::Rules(vec![0])),
            member("NDX", Origin::Manual),
            member("SPX", Origin::Both(vec![0, 1])),
            member(
                "UKX",
                Origin::Excluded {
                    rules: vec![1],
                    manual: false,
                },
            ),
        ],
        resolved_at: None,
        status: Status::Current,
    }
}

fn europe() -> WatchlistSnapshot {
    let mut snap = WatchlistSnapshot::default();
    snap.lists.insert("europe".into(), resolved());
    snap
}

/// `underlyings` with a `name` column: SPX and DAX named, NDX with a NULL
/// name, UKX absent.
fn reference() -> ReferenceData {
    let table = ReferenceTable {
        columns: vec!["underlying_ref".into(), "name".into()],
        rows: vec![
            vec![Some("SPX".into()), Some("S&P 500".into())],
            vec![Some("DAX".into()), Some("DAX 40".into())],
            vec![Some("NDX".into()), None],
        ],
        gen_id: 1,
        source_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
    };
    ReferenceData::default()
        .with_table(crate::core::rows::REFERENCE_DATASET, &table, 1)
        .unwrap()
}

fn publish_reference(cx: &mut gpui::App, data: ReferenceData) {
    cx.set_global(ReferenceGlobal(Arc::new(data)));
}

/// A tile on `europe` with the reference table published.
fn europe_shown(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    cx.update(|cx| publish_reference(cx, reference()));
    open_with(cx, europe(), restored("europe"))
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
    fn shown(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.shown_names())
    }
    fn targets(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.targets())
    }
    fn cursor(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.grid.cursor_name().map(str::to_string))
    }
    fn reasons(&self, vcx: &gpui::VisualTestContext) -> Option<Vec<(String, Option<String>)>> {
        self.tile.read_with(vcx, |t, _| t.action_reasons())
    }
    /// The painted rows as the delegate holds them.
    fn prepared(&self, vcx: &gpui::VisualTestContext) -> Vec<table::PreparedRow> {
        self.tile.read_with(vcx, |t, cx| {
            t.table.read(cx).delegate().prepared().rows.clone()
        })
    }
    fn find(&self, vcx: &mut gpui::VisualTestContext, event: FindEvent) {
        vcx.update(|window, cx| self.content.find(event, window, cx));
    }
    fn command(&self, vcx: &mut gpui::VisualTestContext, line: &str) -> Result<(), String> {
        vcx.update(|window, cx| self.content.command(line, window, cx))
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

/// `.` opens the `⋯` menu with the member verbs, the switcher and the
/// list's own verbs; a verb not built yet says so as a status notice.
#[gpui::test]
fn the_actions_menu_lists_the_verbs_and_a_pick_says_not_yet(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, two(), restored("a"));
    vcx.simulate_keystrokes(".");
    let none = |t: &str| (t.to_string(), None);
    let off = |t: &str, why: &str| (t.to_string(), Some(why.to_string()));
    assert_eq!(
        h.reasons(&vcx),
        Some(vec![
            none("Add name"),
            none("Remove name"),
            none("Rules\u{2026}"),
            none("Resolve now"),
            off("Undo", NOTHING_TO_UNDO),
            off("Redo", NOTHING_TO_REDO),
            none("Switch\u{2026}"),
            none("New\u{2026}"),
            none("Clone\u{2026}"),
            none("Rename\u{2026}"),
            none("Delete\u{2026}"),
            none("Revert\u{2026}"),
        ])
    );
    assert_eq!(h.mode(&vcx).as_deref(), Some("menu"));
    // `.` again closes it; `escape` too.
    vcx.simulate_keystrokes(".");
    assert_eq!(h.actions(&vcx), None);
    vcx.simulate_keystrokes(". escape");
    assert_eq!(h.actions(&vcx), None);
    // Add name picked from the menu: not built yet.
    vcx.simulate_keystrokes(". enter");
    assert_eq!(h.actions(&vcx), None);
    assert_eq!(h.notices(&vcx), vec![NOT_YET.to_string()]);
    // The disabled Undo and Redo rows are stepped over: four steps reach
    // Switch…, which opens the switcher.
    vcx.simulate_keystrokes(". j j j j enter");
    assert_eq!(
        h.switcher(&vcx),
        Some(vec![("a".into(), true), ("b".into(), false)])
    );
    vcx.simulate_keystrokes("escape");
    // New… picked from the menu.
    vcx.simulate_keystrokes(". j j j j j enter");
    assert_eq!(h.actions(&vcx), None);
    assert_eq!(h.notices(&vcx), vec![NOT_YET.to_string()]);
    // With nothing shown the member verbs say so; Remove with no row too.
    vcx.update(|_, cx| publish(cx, WatchlistSnapshot::default()));
    vcx.simulate_keystrokes("escape .");
    let reasons = h.reasons(&vcx).unwrap();
    assert_eq!(reasons[0], off("Add name", NOTHING_SHOWN));
    assert_eq!(reasons[1], off("Remove name", NOTHING_SHOWN));
    assert_eq!(reasons[3], off("Resolve now", NOTHING_SHOWN));
    vcx.simulate_keystrokes("escape");
    let mut empty = two();
    empty.lists.insert("a".into(), list(&[]));
    vcx.update(|_, cx| publish(cx, empty));
    vcx.simulate_keystrokes(".");
    let reasons = h.reasons(&vcx).unwrap();
    assert_eq!(reasons[0], none("Add name"));
    assert_eq!(reasons[1], off("Remove name", NO_ROW));
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

#[gpui::test]
fn the_grid_lists_members_by_name_with_excluded_last(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    assert_eq!(h.shown(&vcx), ["DAX", "NDX", "SPX", "UKX"]);
    // The header counts the live rows from the grid.
    assert_eq!(
        h.header(&vcx),
        "Watchlist: europe \u{00b7} 3 names \u{00b7} 2 rules \u{00b7} desk"
    );
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("DAX"),
        "resting on the top row"
    );
    // What each row paints: the reference name, the mark and the origin.
    let rows = h.prepared(&vcx);
    let paint: Vec<(&str, Option<&str>, bool, &str, bool)> = rows
        .iter()
        .map(|r| {
            (
                r.name.as_ref(),
                r.reference.as_deref(),
                r.in_reference,
                r.origin.as_ref(),
                r.excluded,
            )
        })
        .collect();
    assert_eq!(
        paint,
        [
            ("DAX", Some("DAX 40"), true, "rule 1", false),
            ("NDX", None, true, "manual", false),
            ("SPX", Some("S&P 500"), true, "manual + rules 1, 2", false),
            ("UKX", None, false, "excluded (rule 2)", true),
        ]
    );
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-table-7").is_some());
    assert!(vcx.debug_bounds("watchlist-row-UKX").is_some());
    assert!(vcx.debug_bounds("watchlist-empty-7").is_none());
    // A reference change re-reads the names.
    vcx.update(|_, cx| publish_reference(cx, ReferenceData::default()));
    let rows = h.prepared(&vcx);
    assert!(
        rows.iter()
            .all(|r| r.reference.is_none() && !r.in_reference)
    );
    // A snapshot change keeps the cursor on its row.
    vcx.simulate_keystrokes("j j");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SPX"));
    let mut wider = europe();
    wider.lists.get_mut("europe").unwrap().members.insert(
        0,
        Member {
            name: "CAC".into(),
            origin: Origin::Rules(vec![0]),
        },
    );
    vcx.update(|_, cx| publish(cx, wider));
    assert_eq!(h.shown(&vcx), ["CAC", "DAX", "NDX", "SPX", "UKX"]);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SPX"));
    assert!(h.header(&vcx).contains("4 names"), "{}", h.header(&vcx));
    // A list with no members paints the grid's empty state.
    vcx.update(|_, cx| {
        let mut snap = europe();
        snap.lists.insert("europe".into(), list(&[]));
        publish(cx, snap)
    });
    assert!(h.shown(&vcx).is_empty());
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-grid-empty").is_some());
}

/// `:sort` through the shell's command door, bare `:sort` back to the
/// default, then the header's sort control cycling desc → asc → default.
/// The sort icon carries no debug selector to press headless, so the press
/// is driven through the delegate's `perform_sort` hook, which the icon's
/// click calls.
#[gpui::test]
fn sort_by_origin_and_back_to_default_via_colon_sort_and_header_click(
    cx: &mut gpui::TestAppContext,
) {
    use gpui_component::table::{ColumnSort, TableDelegate as _};
    let (h, mut vcx) = europe_shown(cx);
    h.command(&mut vcx, "sort origin desc").unwrap();
    assert_eq!(h.shown(&vcx), ["DAX", "SPX", "NDX", "UKX"]);
    h.command(&mut vcx, "sort origin").unwrap();
    assert_eq!(
        h.shown(&vcx),
        ["UKX", "NDX", "SPX", "DAX"],
        "a bare column is asc"
    );
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    let (state, _) = crate::core::session::from_table(&saved);
    assert_eq!(
        state.sort,
        Some((SortCol::Origin, false)),
        "the session saves it"
    );
    h.command(&mut vcx, "sort reference").unwrap();
    assert_eq!(h.shown(&vcx), ["DAX", "SPX", "NDX", "UKX"], "no name last");
    h.command(&mut vcx, "sort name desc").unwrap();
    assert_eq!(h.shown(&vcx), ["UKX", "SPX", "NDX", "DAX"]);
    h.command(&mut vcx, "sort").unwrap();
    assert_eq!(
        h.shown(&vcx),
        ["DAX", "NDX", "SPX", "UKX"],
        "the default order"
    );
    for bad in [
        "sort rows",
        "sort name up",
        "sort name asc extra",
        "grep x",
        "",
    ] {
        assert!(h.command(&mut vcx, bad).is_err(), "{bad}");
    }
    let complete = |line: &str, vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| h.content.completions(line, line.len(), cx))
    };
    assert_eq!(complete("so", &mut vcx), ["sort"]);
    assert_eq!(complete("sort ", &mut vcx), ["name", "origin", "reference"]);
    assert_eq!(complete("sort origin d", &mut vcx), ["asc", "desc"]);
    assert!(complete("sort origin desc ", &mut vcx).is_empty());
    // The header's control: the pricer's cycle, desc → asc → default.
    let table = h.tile.read_with(&vcx, |t, _| t.table.clone());
    let click = |col: usize, vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            table.update(cx, |t, cx| {
                t.delegate_mut()
                    .perform_sort(col, ColumnSort::Default, window, cx)
            })
        });
    };
    let sort = |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, _| t.state.sort);
    click(2, &mut vcx);
    assert_eq!(sort(&vcx), Some((SortCol::Origin, true)));
    assert_eq!(h.shown(&vcx), ["DAX", "SPX", "NDX", "UKX"]);
    click(2, &mut vcx);
    assert_eq!(sort(&vcx), Some((SortCol::Origin, false)));
    assert_eq!(h.shown(&vcx), ["UKX", "NDX", "SPX", "DAX"]);
    click(2, &mut vcx);
    assert_eq!(sort(&vcx), None);
    assert_eq!(h.shown(&vcx), ["DAX", "NDX", "SPX", "UKX"]);
    // Another column starts its own cycle at desc.
    click(2, &mut vcx);
    click(0, &mut vcx);
    assert_eq!(sort(&vcx), Some((SortCol::Name, true)));
    assert_eq!(h.shown(&vcx), ["UKX", "SPX", "NDX", "DAX"]);
    // The header marks the column in force.
    let marked = vcx.update(|_, cx| {
        let d = table.read(cx).delegate();
        (0..3).map(|c| d.column(c, cx).sort).collect::<Vec<_>>()
    });
    assert_eq!(
        marked,
        [
            Some(ColumnSort::Descending),
            Some(ColumnSort::Default),
            Some(ColumnSort::Default)
        ]
    );
}

#[gpui::test]
fn slash_narrows_over_name_and_reference_and_escape_restores(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.find(&mut vcx, FindEvent::Changed("500".into()));
    assert_eq!(h.shown(&vcx), ["SPX"], "the reference name is searched");
    h.find(&mut vcx, FindEvent::Cancelled);
    assert_eq!(
        h.shown(&vcx),
        ["DAX", "NDX", "SPX", "UKX"],
        "escape restores"
    );
    // Fuzzy over the name: `dx` is a subsequence of DAX and NDX.
    h.find(&mut vcx, FindEvent::Changed("dx".into()));
    assert_eq!(h.shown(&vcx), ["DAX", "NDX"], "in the default order");
    h.find(&mut vcx, FindEvent::Committed("dx".into()));
    assert_eq!(h.shown(&vcx), ["DAX", "NDX"], "enter keeps it");
    // A later cancelled search restores the committed filter, not none.
    h.find(&mut vcx, FindEvent::Changed("ndx".into()));
    assert_eq!(h.shown(&vcx), ["NDX"]);
    h.find(&mut vcx, FindEvent::Cancelled);
    assert_eq!(h.shown(&vcx), ["DAX", "NDX"]);
    // Painted highlights follow the filter: each word marks the column it
    // matched, the name and the reference name.
    h.find(&mut vcx, FindEvent::Changed("dax 4".into()));
    let rows = h.prepared(&vcx);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name_marks, [std::ops::Range { start: 0, end: 3 }]);
    assert_eq!(
        rows[0].reference_marks,
        [std::ops::Range { start: 4, end: 5 }]
    );
    // The counts ignore the filter.
    assert!(h.header(&vcx).contains("3 names"), "{}", h.header(&vcx));
    h.find(&mut vcx, FindEvent::Changed("zzz".into()));
    assert!(h.shown(&vcx).is_empty());
    assert!(h.targets(&vcx).is_empty(), "no row under the cursor");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.grid.cursor()),
        None,
        "nothing painted as the cursor row"
    );
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-grid-empty").is_some());
}

#[gpui::test]
fn v_starts_a_row_selection_and_escape_ends_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    vcx.simulate_keystrokes("j");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NDX"));
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    vcx.simulate_keystrokes("v j");
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    assert_eq!(h.targets(&vcx), ["NDX", "SPX"]);
    // A live selection clamps a bare step rather than wrap past the anchor.
    vcx.simulate_keystrokes("j j");
    assert_eq!(h.targets(&vcx), ["NDX", "SPX", "UKX"]);
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert_eq!(h.targets(&vcx), ["UKX"]);
    // `shift+v` too.
    vcx.simulate_keystrokes("shift-v k");
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    assert_eq!(h.targets(&vcx), ["SPX", "UKX"]);
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert_eq!(h.targets(&vcx), ["SPX"]);
    // Counted keys: `2 k` steps two rows, `5 j` clamps at the foot rather
    // than wrap; `g g` and `shift+g` reach the ends.
    vcx.simulate_keystrokes("2 k");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    vcx.simulate_keystrokes("5 j");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("UKX"));
    vcx.simulate_keystrokes("g g");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    vcx.simulate_keystrokes("shift-g");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("UKX"));
    vcx.simulate_keystrokes("j");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"), "a bare step wraps");
    // The cursor is what the session saves, and the table paints it.
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("DAX"));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t.table.read(cx).selected_row()),
        Some(0)
    );
    // A rebuild removing the cursor's row ends a live selection.
    vcx.simulate_keystrokes("j v j");
    assert_eq!(h.targets(&vcx), ["NDX", "SPX"]);
    let mut without = europe();
    without
        .lists
        .get_mut("europe")
        .unwrap()
        .members
        .retain(|m| m.name != "SPX");
    vcx.update(|_, cx| publish(cx, without));
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert_eq!(h.targets(&vcx), ["UKX"]);
}

/// A restored cursor waits for the snapshot that holds its row.
#[gpui::test]
fn a_restored_cursor_lands_on_its_name_when_the_snapshot_arrives(cx: &mut gpui::TestAppContext) {
    let table = crate::core::session::to_table(&crate::core::session::State {
        name: Some("europe".into()),
        sort: Some((SortCol::Name, true)),
        cursor: Some("NDX".into()),
    });
    let (h, mut vcx) = open_with(cx, WatchlistSnapshot::default(), Some(table));
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("NDX"), "still waiting");
    vcx.update(|_, cx| publish(cx, europe()));
    assert_eq!(
        h.shown(&vcx),
        ["UKX", "SPX", "NDX", "DAX"],
        "the restored sort"
    );
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NDX"));
}

#[gpui::test]
fn a_row_press_moves_the_cursor_and_a_double_click_does_nothing_more(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = europe_shown(cx);
    h.draw(&mut vcx);
    let spx = vcx
        .debug_bounds("watchlist-row-SPX")
        .expect("the row is painted");
    vcx.simulate_click(spx.center(), gpui::Modifiers::none());
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SPX"));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t.table.read(cx).selected_row()),
        Some(2),
        "the table paints the cursor row"
    );
    // Shift-click extends a row selection from the cursor.
    h.draw(&mut vcx);
    let dax = vcx.debug_bounds("watchlist-row-DAX").unwrap();
    vcx.simulate_click(dax.center(), gpui::Modifiers::shift());
    assert_eq!(h.targets(&vcx), ["DAX", "NDX", "SPX"]);
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    // A plain click ends it; a double-click is two of them and no verb.
    h.draw(&mut vcx);
    let ndx = vcx.debug_bounds("watchlist-row-NDX").unwrap();
    vcx.simulate_click(ndx.center(), gpui::Modifiers::none());
    assert_eq!(h.targets(&vcx), ["NDX"]);
    vcx.simulate_click(ndx.center(), gpui::Modifiers::none());
    assert_eq!(h.targets(&vcx), ["NDX"]);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert!(h.notices(&vcx).is_empty(), "{:?}", h.notices(&vcx));
    assert_eq!(h.actions(&vcx), None);
}

#[gpui::test]
fn a_right_press_moves_the_cursor_and_opens_the_menu_at_the_pointer(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.draw(&mut vcx);
    let spx = vcx.debug_bounds("watchlist-row-SPX").unwrap();
    vcx.simulate_mouse_down(
        spx.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::none(),
    );
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SPX"));
    assert_eq!(h.mode(&vcx).as_deref(), Some("menu"));
    assert_eq!(
        h.actions(&vcx).map(|a| a[1].clone()),
        Some("Remove name".to_string())
    );
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.menu_at),
        Some(spx.center()),
        "hung from the pointer"
    );
    h.draw(&mut vcx);
    let menu = vcx
        .debug_bounds("watchlist-menu")
        .expect("the menu is painted");
    // The anchor snaps to whole pixels.
    let at = spx.center();
    assert!(
        (menu.origin.x - at.x).abs() <= gpui::px(1.0)
            && (menu.origin.y - at.y).abs() <= gpui::px(1.0),
        "{:?} is at the pointer {at:?}",
        menu.origin
    );
    // `escape` closes it; the menu's closer releases the pointer anchor.
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.actions(&vcx), None);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.menu_at), None);
    // A right press inside a live selection keeps the selection, which the
    // menu acts on; `escape` closes the menu first, then ends it.
    vcx.simulate_keystrokes("g g v j");
    assert_eq!(h.targets(&vcx), ["DAX", "NDX"]);
    h.draw(&mut vcx);
    let dax = vcx.debug_bounds("watchlist-row-DAX").unwrap();
    vcx.simulate_mouse_down(
        dax.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::none(),
    );
    assert_eq!(h.targets(&vcx), ["DAX", "NDX"]);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NDX"));
    assert_eq!(h.mode(&vcx).as_deref(), Some("menu"));
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    assert_eq!(h.targets(&vcx), ["DAX", "NDX"]);
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    // Outside the selection the press moves the cursor and ends it.
    vcx.simulate_keystrokes("v j");
    h.draw(&mut vcx);
    let ukx = vcx.debug_bounds("watchlist-row-UKX").unwrap();
    vcx.simulate_mouse_down(
        ukx.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::none(),
    );
    assert_eq!(h.targets(&vcx), ["UKX"]);
    assert_eq!(h.mode(&vcx).as_deref(), Some("menu"));
    // The header's `⋯` from the key opens the same menu hung from the
    // control instead.
    vcx.simulate_keystrokes("escape .");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.menu_at), None);
    assert!(h.actions(&vcx).is_some());
    vcx.simulate_keystrokes("escape");
    release_the_table_menu(&mut vcx, ukx.center());
}

/// gpui-component's table builds its (empty) context menu on every right
/// press, and that menu's dismiss subscription holds it in a cycle only
/// the table's next right press breaks, so a test ending after a right
/// press leaks it (the classifications and pricer tests have the same
/// helper). One more right press, whose deferred rebuild never runs
/// because the window closes in the same update, breaks it.
fn release_the_table_menu(vcx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    vcx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                button: gpui::MouseButton::Right,
                position: at,
                modifiers: gpui::Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.remove_window();
    });
    vcx.run_until_parked();
}

/// A filter hiding a put cursor's row rests the cursor on the nearest
/// shown row; escape restoring the filter returns it to its row.
#[gpui::test]
fn a_filter_hiding_the_cursor_rests_it_on_the_nearest_shown_row(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    vcx.simulate_keystrokes("shift-g");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("UKX"));
    h.find(&mut vcx, FindEvent::Changed("d".into()));
    assert_eq!(h.shown(&vcx), ["DAX", "NDX"]);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NDX"), "the nearest shown");
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t.table.read(cx).selected_row()),
        Some(1),
        "the table paints the resting row"
    );
    // The session still names the trader's row.
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("UKX"));
    h.find(&mut vcx, FindEvent::Cancelled);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("UKX"), "back on its row");
    // A move while hidden is the trader's new choice.
    h.find(&mut vcx, FindEvent::Changed("d".into()));
    vcx.simulate_keystrokes("k");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    h.find(&mut vcx, FindEvent::Cancelled);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    // Switching lists drops the filter and the cursor with it.
    let mut both = europe();
    both.lists.insert("a".into(), list(&["SPX", "NDX"]));
    vcx.update(|_, cx| publish(cx, both));
    h.find(&mut vcx, FindEvent::Committed("d".into()));
    // The switcher opens on `europe`; `k` is `a`.
    vcx.simulate_keystrokes("g w k enter");
    assert_eq!(h.title(&mut vcx), "Watchlist: a");
    assert_eq!(h.shown(&vcx), ["NDX", "SPX"]);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NDX"));
}
