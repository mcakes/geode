use super::*;
use crate::content::{WatchlistConfig, WatchlistFactory};
use geode_core::config::Layer;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::query::ReferenceTable;
use geode_core::reference::ReferenceData;
use geode_core::scopes::SavedScopes;
use geode_core::watchlist::fold::RuleError;
use geode_core::watchlist::members::{Member, Origin};
use geode_core::watchlist::state::{Status, WatchlistState};
use geode_core::watchlist::{Rule, Watchlist, to_toml};
use geode_shell::actions::{ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{ConfigEdit, Frame, FrameRef, TileNotice};
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
/// NDX and SPX included by hand, UKX excluded by hand.
fn resolved() -> WatchlistState {
    let member = |name: &str, origin: Origin| Member {
        name: name.into(),
        origin,
    };
    WatchlistState {
        definition: Watchlist {
            include: vec!["NDX".into(), "SPX".into()],
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
    reference_with(&[])
}

/// [`reference`] with `extra` keys beyond the three, each named after
/// itself: names the typeahead offers that no list holds.
fn reference_with(extra: &[&str]) -> ReferenceData {
    let mut rows = vec![
        vec![Some("SPX".into()), Some("S&P 500".into())],
        vec![Some("DAX".into()), Some("DAX 40".into())],
        vec![Some("NDX".into()), None],
    ];
    rows.extend(
        extra
            .iter()
            .map(|k| vec![Some(k.to_string()), Some(format!("{k} index"))]),
    );
    let table = ReferenceTable {
        columns: vec!["underlying_ref".into(), "name".into()],
        rows,
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
                // `menu` and `rules` are fieldless list popups and `visual`
                // a live row selection: the shell routes them all as normal
                // mode, over the whole stack.
                let stack = match context.get("mode") {
                    Some("normal") | Some("visual") | Some("menu") | Some("rules") => vec![
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
    frame: Entity<Frame>,
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
    /// The frame the tile's config writes are queued on and its notices
    /// posted to.
    frame: Entity<Frame>,
    /// The shell root's focus, where the keyboard goes back to once a
    /// field is gone.
    shell_focus: gpui::FocusHandle,
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
                    frame,
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
            frame: built.frame,
            shell_focus: built.shell_focus,
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

impl Harness {
    /// Keys as the trader types them. A committed or cancelled field
    /// blurs itself, and the shell then puts the keyboard back on its
    /// root; the stand-in has no such path, so the test does it.
    fn press(&self, vcx: &mut gpui::VisualTestContext, keys: &str) {
        vcx.update(|window, cx| {
            if window.focused(cx).is_none() {
                self.shell_focus.focus(window, cx);
            }
        });
        vcx.simulate_keystrokes(keys);
    }
    /// A registered action, the way the palette reaches the tile: through
    /// its door.
    fn act(&self, vcx: &mut gpui::VisualTestContext, id: &str) {
        let id = ActionId(id.to_string());
        vcx.update(|window, cx| {
            self.content.dispatch(&id, None, window, cx);
        });
        self.draw(vcx);
    }
    /// What reached the frame's config door since the last call: the
    /// shell's drain takes exactly this.
    fn edits(&self, vcx: &mut gpui::VisualTestContext) -> Vec<ConfigEdit> {
        self.frame.update(vcx, |f, _| f.take_pending_config_edits())
    }
    /// Post a notice for this tile the way the shell's drain does: on the
    /// frame, then one notify.
    fn shell_says(&self, vcx: &mut gpui::VisualTestContext, notice: TileNotice) {
        self.frame.update(vcx, |f, cx| {
            f.post_tile_notice_for_test(TileId(TILE), notice);
            cx.notify();
        });
        vcx.run_until_parked();
    }
    /// The open field: its text, its refusal and its ranked options.
    fn prompt(
        &self,
        vcx: &gpui::VisualTestContext,
    ) -> Option<(String, Option<String>, Vec<String>)> {
        self.tile.read_with(vcx, |t, cx| t.prompt_state(cx))
    }
    /// The origin column of the shown row `name`.
    fn origin(&self, vcx: &gpui::VisualTestContext, name: &str) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.origin_of(name))
    }
    /// The header notices not dismissed.
    fn visible_notices(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.visible_notice_texts())
    }
    /// The open field's label (`clone europe as`).
    fn label(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.prompt.as_ref().map(|p| p.label.to_string()))
    }
    /// Replace the open field's text with `text`, as the trader would by
    /// clearing it and typing.
    fn retype(&self, vcx: &mut gpui::VisualTestContext, text: &str) {
        let input = self
            .tile
            .read_with(vcx, |t, _| t.prompt.as_ref().map(|p| p.input.clone()))
            .expect("the field is open");
        vcx.update(|window, cx| input.update(cx, |s, cx| s.set_value("", window, cx)));
        vcx.simulate_input(text);
    }
    /// The armed y/n question.
    fn question(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.question())
    }
    /// Whether a create, clone, rename or delete is still awaited.
    fn awaiting(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile.read_with(vcx, |t, _| t.awaiting.is_some())
    }
    /// Show `name` through the switcher's rows.
    fn switch_to(&self, vcx: &mut gpui::VisualTestContext, name: &str) {
        self.press(vcx, "g w");
        let rows = self.switcher(vcx).expect("the switcher is open");
        let at = rows.iter().position(|(n, _)| n == name).expect("listed");
        let from = rows.iter().position(|(_, on)| *on).unwrap_or(0);
        let steps = if at >= from {
            " j".repeat(at - from)
        } else {
            " k".repeat(from - at)
        };
        self.press(vcx, &format!("g g{steps} enter"));
        assert_eq!(self.title(vcx), format!("Watchlist: {name}"));
    }
    /// Put the cursor on `name` with the grid's own motions.
    fn goto(&self, vcx: &mut gpui::VisualTestContext, name: &str) {
        let at = self
            .shown(vcx)
            .iter()
            .position(|s| s == name)
            .expect("the row is shown");
        self.press(vcx, &format!("g g{}", " j".repeat(at)));
        assert_eq!(self.cursor(vcx).as_deref(), Some(name));
    }
}

/// The edit the tile queues for `europe`: the whole object, from this tile.
fn edit_of(next: &Watchlist) -> ConfigEdit {
    ConfigEdit {
        doc: geode_core::watchlist::WATCHLISTS_DOC,
        object: "europe".into(),
        value: Some(to_toml(next)),
        origin: Some(TileId(TILE)),
    }
}

/// `europe`'s definition as the snapshot holds it.
fn europe_def() -> Watchlist {
    resolved().definition
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
fn the_actions_menu_lists_the_verbs_with_their_gates(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, two(), restored("a"));
    vcx.simulate_keystrokes(".");
    let none = |t: &str| (t.to_string(), None);
    let off = |t: &str, why: &str| (t.to_string(), Some(why.to_string()));
    // `a` is the desk's: Rename… and Delete… say so in their lane, and
    // Revert… is not listed (no copy beneath a desk list).
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
            off(
                "Rename\u{2026}",
                "a is defined in desk config; Geode cannot rename it"
            ),
            off(
                "Delete\u{2026}",
                "a is defined in desk config; Geode cannot delete it"
            ),
        ])
    );
    assert_eq!(h.mode(&vcx).as_deref(), Some("menu"));
    // `.` again closes it; `escape` too.
    vcx.simulate_keystrokes(".");
    assert_eq!(h.actions(&vcx), None);
    vcx.simulate_keystrokes(". escape");
    assert_eq!(h.actions(&vcx), None);
    // Add name picked from the menu opens the add field; escape closes it
    // unwritten.
    h.press(&mut vcx, ". enter");
    assert_eq!(h.actions(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(h.prompt(&vcx).is_some());
    h.press(&mut vcx, "escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert!(h.edits(&mut vcx).is_empty());
    // The disabled Undo and Redo rows are stepped over: four steps reach
    // Switch…, which opens the switcher.
    h.press(&mut vcx, ". j j j j enter");
    assert_eq!(
        h.switcher(&vcx),
        Some(vec![("a".into(), true), ("b".into(), false)])
    );
    h.press(&mut vcx, "escape");
    // New… picked from the menu opens the name field; escape closes it.
    h.press(&mut vcx, ". j j j j j enter");
    assert_eq!(h.actions(&vcx), None);
    assert_eq!(h.label(&vcx).as_deref(), Some("New watchlist"));
    h.press(&mut vcx, "escape");
    assert_eq!(h.prompt(&vcx), None);
    assert!(h.edits(&mut vcx).is_empty());
    // An edit enables Undo; undone, Redo.
    h.press(&mut vcx, "x .");
    let reasons = h.reasons(&vcx).unwrap();
    assert_eq!(reasons[4], none("Undo"));
    assert_eq!(reasons[5], off("Redo", NOTHING_TO_REDO));
    h.press(&mut vcx, "escape u .");
    let reasons = h.reasons(&vcx).unwrap();
    assert_eq!(reasons[4], off("Undo", NOTHING_TO_UNDO));
    assert_eq!(reasons[5], none("Redo"));
    h.edits(&mut vcx);
    // With nothing shown the member verbs say so; Remove with no row too.
    vcx.update(|_, cx| publish(cx, WatchlistSnapshot::default()));
    h.press(&mut vcx, "escape .");
    let reasons = h.reasons(&vcx).unwrap();
    assert_eq!(reasons[0], off("Add name", NOTHING_SHOWN));
    assert_eq!(reasons[1], off("Remove name", NOTHING_SHOWN));
    assert_eq!(reasons[3], off("Resolve now", NOTHING_SHOWN));
    assert_eq!(reasons[7], none("New\u{2026}"), "New… needs no list");
    assert_eq!(reasons[8], off("Clone\u{2026}", NOTHING_SHOWN));
    assert_eq!(reasons[9], off("Rename\u{2026}", NOTHING_SHOWN));
    assert_eq!(reasons[10], off("Delete\u{2026}", NOTHING_SHOWN));
    assert_eq!(reasons.len(), 11);
    h.press(&mut vcx, "escape");
    let mut empty = two();
    empty.lists.insert("a".into(), list(&[]));
    vcx.update(|_, cx| publish(cx, empty));
    h.press(&mut vcx, ".");
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
    // A snapshot still resolving is not the answer the seed waits for: a
    // current one without the name is, and the seed is forgotten, so the
    // session saves the row the cursor rests on and a later re-add does
    // not snap the cursor to it.
    let seeded = || {
        crate::core::session::to_table(&crate::core::session::State {
            name: Some("europe".into()),
            sort: None,
            cursor: Some("NDX".into()),
        })
    };
    let without_ndx = |status: Status| {
        let mut snap = europe();
        let list = snap.lists.get_mut("europe").unwrap();
        list.members.retain(|m| m.name != "NDX");
        list.status = status;
        snap
    };
    let (h, mut vcx) = open_with(cx, without_ndx(Status::Resolving), Some(seeded()));
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("NDX"), "resolving: waits");
    vcx.update(|_, cx| publish(cx, without_ndx(Status::Failed("timed out".into()))));
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("NDX"), "failed: waits");
    vcx.update(|_, cx| publish(cx, without_ndx(Status::Current)));
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("DAX"), "the resting row");
    vcx.update(|_, cx| publish(cx, europe()));
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("DAX"),
        "NDX's return is not followed"
    );
    // Empty list, current: nothing rests anywhere, and nothing is saved.
    let (h, mut vcx) = open_with(cx, WatchlistSnapshot::default(), Some(seeded()));
    vcx.update(|_, cx| {
        let mut snap = WatchlistSnapshot::default();
        snap.lists.insert("europe".into(), list(&[]));
        publish(cx, snap)
    });
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert!(!saved.contains_key("cursor"), "{saved:?}");
    vcx.update(|_, cx| publish(cx, europe()));
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("DAX"),
        "resting on the top row"
    );
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

#[gpui::test]
fn o_then_enter_queues_one_write_with_the_name_in_include(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.press(&mut vcx, "o");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    let (text, error, options) = h.prompt(&vcx).expect("the field is open");
    assert_eq!(text, "");
    assert_eq!(error, None);
    // The typeahead: the reference table's keys and every list's names,
    // once each, sorted; a name already here is not hidden.
    assert_eq!(options, ["DAX", "NDX", "SPX", "UKX"]);
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-prompt-7").is_some());
    assert!(vcx.debug_bounds("watchlist-prompt-list-7").is_some());
    assert!(vcx.debug_bounds("watchlist-prompt-row-DAX").is_some());
    vcx.simulate_input("HSI");
    let (text, _, options) = h.prompt(&vcx).unwrap();
    assert_eq!(text, "HSI");
    assert!(
        options.is_empty(),
        "nothing matches: enter adds it as typed"
    );
    h.press(&mut vcx, "enter");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert_eq!(h.prompt(&vcx), None);
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    let edits = h.edits(&mut vcx);
    let mut next = europe_def();
    next.include.push("HSI".into());
    assert_eq!(edits, [edit_of(&next)]);
    let value = edits[0].value.as_ref().unwrap();
    assert_eq!(
        value["include"].as_array().unwrap().len(),
        3,
        "the whole object: NDX, SPX and the new name"
    );
    assert_eq!(value["include"][2].as_str(), Some("HSI"));
    // Shown at once, marked pending, counted.
    assert_eq!(
        h.origin(&vcx, "HSI").as_deref(),
        Some("manual \u{b7} pending")
    );
    assert_eq!(h.notices(&vcx), ["added HSI"]);
    assert!(h.header(&vcx).contains("4 names"), "{}", h.header(&vcx));
    // The reload carrying it drops the mark.
    let mut snap = europe();
    let s = snap.lists.get_mut("europe").unwrap();
    s.definition = next.clone();
    s.members.push(Member {
        name: "HSI".into(),
        origin: Origin::Manual,
    });
    vcx.update(|_, cx| publish(cx, snap));
    assert_eq!(h.origin(&vcx, "HSI").as_deref(), Some("manual"));
    // A case variant of a listed name takes the listed spelling: UKX is
    // excluded, so adding it restores it, and `exclude` empties.
    h.press(&mut vcx, "o");
    vcx.simulate_input("ukx");
    h.press(&mut vcx, "enter");
    let mut restored = next.clone();
    restored.exclude.clear();
    assert_eq!(h.edits(&mut vcx), [edit_of(&restored)]);
    assert_eq!(h.notices(&vcx), ["restored UKX"]);
    assert_eq!(
        h.origin(&vcx, "UKX").as_deref(),
        Some("rule 2 \u{b7} pending")
    );
}

#[gpui::test]
fn add_of_a_rule_supplied_name_is_refused_under_the_field(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.press(&mut vcx, "o");
    vcx.simulate_input("DAX");
    h.press(&mut vcx, "enter");
    let (text, error, _) = h.prompt(&vcx).expect("the field stays open");
    assert_eq!(text, "DAX");
    assert_eq!(error.as_deref(), Some("DAX is already here from rule 1"));
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(h.edits(&mut vcx).is_empty());
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-prompt-error-7").is_some());
    // A manual name, and a blank, are refused too.
    h.press(&mut vcx, "backspace backspace backspace");
    vcx.simulate_input("spx");
    h.press(&mut vcx, "enter");
    let (_, error, _) = h.prompt(&vcx).unwrap();
    assert_eq!(error.as_deref(), Some("SPX is already here"));
    h.press(&mut vcx, "backspace backspace backspace enter");
    let (text, error, _) = h.prompt(&vcx).unwrap();
    assert_eq!(text, "");
    assert_eq!(error.as_deref(), Some(crate::core::prompt::TYPE_A_NAME));
    // Escape closes it with nothing written.
    h.press(&mut vcx, "escape");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    assert!(h.edits(&mut vcx).is_empty());
    assert!(h.notices(&vcx).is_empty());
}

#[gpui::test]
fn x_on_each_origin_kind_writes_the_right_object(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    // No row under the cursor (the filter hides every row): nothing to
    // remove.
    h.find(&mut vcx, FindEvent::Changed("zzz".into()));
    assert!(h.targets(&vcx).is_empty());
    h.press(&mut vcx, "x");
    assert_eq!(h.notices(&vcx), [NOTHING_TO_REMOVE]);
    assert!(h.edits(&mut vcx).is_empty());
    h.find(&mut vcx, FindEvent::Cancelled);
    // DAX: rule 1 supplies it, so it is excluded.
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    h.press(&mut vcx, "x");
    let mut e1 = europe_def();
    e1.exclude.push("DAX".into());
    assert_eq!(h.edits(&mut vcx), [edit_of(&e1)]);
    assert_eq!(
        h.notices(&vcx),
        ["excluded DAX \u{2014} rule 1 still supplies it; x again restores"]
    );
    assert_eq!(
        h.origin(&vcx, "DAX").as_deref(),
        Some("excluded (rule 1) \u{b7} pending")
    );
    // The cursor keeps its shown index: DAX sorts last now, NDX is under it.
    assert_eq!(h.shown(&vcx), ["NDX", "SPX", "DAX", "UKX"]);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NDX"));
    // NDX: manual, so it leaves `include`; the write builds on the first.
    h.press(&mut vcx, "x");
    let mut e2 = e1.clone();
    e2.include.retain(|n| n != "NDX");
    assert_eq!(h.edits(&mut vcx), [edit_of(&e2)]);
    assert_eq!(h.notices(&vcx), ["removed NDX"]);
    assert!(h.origin(&vcx, "NDX").is_none(), "gone at once");
    // UKX: excluded, so it is restored.
    h.goto(&mut vcx, "UKX");
    h.press(&mut vcx, "x");
    let mut e3 = e2.clone();
    e3.exclude.retain(|n| n != "UKX");
    assert_eq!(h.edits(&mut vcx), [edit_of(&e3)]);
    assert_eq!(h.notices(&vcx), ["restored UKX"]);
    assert_eq!(
        h.origin(&vcx, "UKX").as_deref(),
        Some("rule 2 \u{b7} pending")
    );
    // DAX again: the verb sees the pending exclusion and restores it,
    // back to the snapshot's state, so the row is no longer pending.
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "x");
    let mut e4 = e3.clone();
    e4.exclude.clear();
    assert_eq!(h.edits(&mut vcx), [edit_of(&e4)]);
    assert_eq!(h.notices(&vcx), ["restored DAX"]);
    assert_eq!(h.origin(&vcx, "DAX").as_deref(), Some("rule 1"));
    // A selection: one write, counted.
    h.press(&mut vcx, "g g v j");
    assert_eq!(h.targets(&vcx), ["DAX", "SPX"]);
    h.press(&mut vcx, "x");
    let mut e5 = e4.clone();
    e5.include.retain(|n| n != "SPX");
    e5.exclude.extend(["DAX".to_string(), "SPX".to_string()]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&e5)]);
    assert_eq!(
        h.notices(&vcx),
        ["excluded 1 name, removed and excluded 1 name"]
    );
    assert_eq!(
        h.mode(&vcx).as_deref(),
        Some("normal"),
        "the selection ended"
    );
}

/// Review focus: a `manual + rule` row is one write, and one undo step.
#[gpui::test]
fn remove_of_a_manual_and_rule_name_writes_once_and_undoes_whole(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.goto(&mut vcx, "SPX");
    assert_eq!(
        h.origin(&vcx, "SPX").as_deref(),
        Some("manual + rules 1, 2")
    );
    h.press(&mut vcx, "x");
    let mut next = europe_def();
    next.include.retain(|n| n != "SPX");
    next.exclude.push("SPX".into());
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)], "one write does both");
    assert_eq!(h.notices(&vcx), ["removed and excluded SPX"]);
    assert_eq!(
        h.origin(&vcx, "SPX").as_deref(),
        Some("excluded (rules 1, 2) \u{b7} pending")
    );
    h.press(&mut vcx, "u");
    assert_eq!(
        h.edits(&mut vcx),
        [edit_of(&europe_def())],
        "one undo restores both"
    );
    assert_eq!(h.notices(&vcx), ["undid 1 change"]);
    assert_eq!(
        h.origin(&vcx, "SPX").as_deref(),
        Some("manual + rules 1, 2")
    );
    h.press(&mut vcx, "ctrl-r");
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)]);
    assert_eq!(h.notices(&vcx), ["redid 1 change"]);
}

/// Review focus: a resolution answering under the same definition is not
/// the reload the edit waits for.
#[gpui::test]
fn a_members_answer_keeps_a_pending_manual_add_painted(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.press(&mut vcx, "o");
    vcx.simulate_input("HSI");
    h.press(&mut vcx, "enter");
    assert_eq!(h.edits(&mut vcx).len(), 1);
    assert_eq!(
        h.origin(&vcx, "HSI").as_deref(),
        Some("manual \u{b7} pending")
    );
    // Rule 1 now also supplies CAC; the definition is as it was.
    let mut snap = europe();
    snap.lists.get_mut("europe").unwrap().members.push(Member {
        name: "CAC".into(),
        origin: Origin::Rules(vec![0]),
    });
    vcx.update(|_, cx| publish(cx, snap));
    assert_eq!(
        h.origin(&vcx, "HSI").as_deref(),
        Some("manual \u{b7} pending"),
        "the edit is still in flight"
    );
    assert_eq!(h.origin(&vcx, "CAC").as_deref(), Some("rule 1"));
    assert_eq!(h.shown(&vcx), ["CAC", "DAX", "HSI", "NDX", "SPX", "UKX"]);
    // The next edit builds on the pending one.
    h.goto(&mut vcx, "CAC");
    h.press(&mut vcx, "x");
    let mut next = europe_def();
    next.include.push("HSI".into());
    next.exclude.push("CAC".into());
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)]);
    // Another surface's write to the list is the truth: the pending edits go.
    let mut snap = europe();
    snap.lists
        .get_mut("europe")
        .unwrap()
        .definition
        .include
        .push("FTSE".into());
    vcx.update(|_, cx| publish(cx, snap));
    assert!(h.origin(&vcx, "HSI").is_none());
    assert_eq!(h.shown(&vcx), ["DAX", "NDX", "SPX", "UKX"]);
}

/// Review focus: the field goes with its list, blurred, nothing written.
#[gpui::test]
fn a_reload_removing_the_shown_list_closes_an_open_field_and_writes_nothing(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = europe_shown(cx);
    h.press(&mut vcx, "o");
    vcx.simulate_input("HSI");
    assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    vcx.update(|_, cx| publish(cx, WatchlistSnapshot::default()));
    vcx.run_until_parked();
    assert_eq!(h.prompt(&vcx), None);
    assert_ne!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    assert!(
        vcx.update(|window, cx| window.focused(cx).is_none()),
        "blurred before it was dropped"
    );
    assert!(h.edits(&mut vcx).is_empty());
    assert!(
        h.empty(&vcx)
            .is_some_and(|t| t.starts_with("europe no longer exists"))
    );
    // `enter` now asks to add to nothing: refused, still nothing written.
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    assert!(h.notices(&vcx).contains(&NOTHING_SHOWN.to_string()));
    assert!(h.edits(&mut vcx).is_empty());
}

#[gpui::test]
fn u_and_ctrl_r_replay_and_report_skipped_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.press(&mut vcx, "u");
    assert_eq!(h.notices(&vcx), [NOTHING_TO_UNDO]);
    h.press(&mut vcx, "ctrl-r");
    assert_eq!(h.notices(&vcx), [NOTHING_TO_REDO]);
    h.goto(&mut vcx, "NDX");
    h.press(&mut vcx, "x");
    let mut removed = europe_def();
    removed.include.retain(|n| n != "NDX");
    assert_eq!(h.edits(&mut vcx), [edit_of(&removed)]);
    h.press(&mut vcx, "u");
    // Undo puts NDX back by hand, so it lands at the end of `include`:
    // the object is restored, not the file's order.
    let mut restored = removed.clone();
    restored.include.push("NDX".into());
    assert_eq!(h.edits(&mut vcx), [edit_of(&restored)]);
    assert_eq!(h.notices(&vcx), ["undid 1 change"]);
    assert_eq!(h.origin(&vcx, "NDX").as_deref(), Some("manual"));
    h.press(&mut vcx, "ctrl-r");
    assert_eq!(h.edits(&mut vcx), [edit_of(&removed)]);
    assert_eq!(h.notices(&vcx), ["redid 1 change"]);
    assert!(h.origin(&vcx, "NDX").is_none());
    // Another surface excluded DAX and left NDX in meanwhile: that reload
    // is the truth, and undo finds NDX no longer as the entry left it.
    let mut snap = europe();
    let s = snap.lists.get_mut("europe").unwrap();
    s.definition.exclude.push("DAX".into());
    s.members[0].origin = Origin::Excluded {
        rules: vec![0],
        manual: false,
    };
    vcx.update(|_, cx| publish(cx, snap));
    assert_eq!(h.origin(&vcx, "NDX").as_deref(), Some("manual"));
    h.press(&mut vcx, "u");
    assert!(h.edits(&mut vcx).is_empty(), "nothing to write");
    assert_eq!(
        h.notices(&vcx),
        ["undid 0 changes \u{2014} 1 changed elsewhere"]
    );
    // The tile's own refused write is said as not saved, not blamed on
    // another surface.
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "x");
    assert_eq!(h.notices(&vcx), ["restored DAX"]);
    assert_eq!(h.edits(&mut vcx).len(), 1);
    h.shell_says(
        &mut vcx,
        TileNotice::Refused("watchlists not written".into()),
    );
    h.press(&mut vcx, "u");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.notices(&vcx), ["undid 0 changes \u{2014} 1 not saved"]);
}

#[gpui::test]
fn a_refused_write_drops_the_pending_object_and_says_so(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.goto(&mut vcx, "NDX");
    h.press(&mut vcx, "x");
    assert!(h.origin(&vcx, "NDX").is_none(), "optimistic");
    assert_eq!(h.edits(&mut vcx).len(), 1);
    let why = "watchlists not written: the user layer is read-only";
    h.shell_says(&mut vcx, TileNotice::Refused(why.into()));
    assert_eq!(h.notices(&vcx), [why]);
    assert_eq!(
        h.origin(&vcx, "NDX").as_deref(),
        Some("manual"),
        "back to the snapshot's object"
    );
    assert!(h.edits(&mut vcx).is_empty(), "nothing was re-queued");
    // The cursor kept its shown index when NDX went, and the rows coming
    // back kept it on its row: SPX. A fork is news, nothing more: the
    // pending edit stands.
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SPX"));
    h.goto(&mut vcx, "NDX");
    h.press(&mut vcx, "x");
    assert_eq!(h.edits(&mut vcx).len(), 1);
    let forked = "copied 'europe' to your config";
    h.shell_says(&mut vcx, TileNotice::Forked(forked.into()));
    assert_eq!(h.notices(&vcx), ["removed NDX", forked]);
    assert!(h.origin(&vcx, "NDX").is_none(), "still pending");
    // A notice for another tile is not this tile's.
    h.frame.update(&mut vcx, |f, cx| {
        f.post_tile_notice_for_test(TileId(TILE + 1), TileNotice::Refused("other".into()));
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(h.origin(&vcx, "NDX").is_none());
}

#[gpui::test]
fn shift_r_calls_the_factory_hook_with_the_shown_name(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.press(&mut vcx, "shift-r");
    assert_eq!(h.notices(&vcx), [NOT_WIRED]);
    let asked: Rc<RefCell<Vec<String>>> = Rc::default();
    let seen = asked.clone();
    h.factory.set_refresh(Rc::new(move |name, _| {
        seen.borrow_mut().push(name.to_string())
    }));
    h.press(&mut vcx, "shift-r");
    assert_eq!(*asked.borrow(), ["europe"]);
    assert!(h.notices(&vcx).is_empty());
    // Resolve now from the `⋯` menu, the fourth row.
    h.press(&mut vcx, ". j j j enter");
    assert_eq!(h.actions(&vcx), None);
    assert_eq!(*asked.borrow(), ["europe", "europe"]);
    // Nothing shown: refused, the hook not called.
    vcx.update(|_, cx| publish(cx, WatchlistSnapshot::default()));
    h.press(&mut vcx, "shift-r");
    assert_eq!(asked.borrow().len(), 2);
    assert!(h.notices(&vcx).contains(&NOTHING_SHOWN.to_string()));
}

#[gpui::test]
fn escape_peels_field_then_selection_then_notices(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    // A failed resolution and a bad rule stand in the header.
    let mut snap = europe();
    let s = snap.lists.get_mut("europe").unwrap();
    s.status = Status::Failed("timed out".into());
    s.rule_errors.push(RuleError {
        index: 1,
        reason: "no such dataset".into(),
    });
    vcx.update(|_, cx| publish(cx, snap));
    let failed = "not resolved: timed out \u{2014} shift+r retries";
    let bad = "rule 2 failed: no such dataset \u{2014} shift+r retries";
    assert_eq!(h.notices(&vcx), [failed, bad]);
    // A selection, then the add field over it (from the palette: no bare
    // letter opens it in visual mode).
    h.press(&mut vcx, "v j");
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    h.act(&mut vcx, "watchlist::add");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    vcx.simulate_input("HSI");
    // The field owns the keys: escape closes it, unwritten, and the
    // selection stands.
    h.press(&mut vcx, "escape");
    assert_eq!(h.prompt(&vcx), None);
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    assert_eq!(h.targets(&vcx), ["DAX", "NDX"]);
    // Then the selection.
    h.press(&mut vcx, "escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert_eq!(h.visible_notices(&vcx), [failed, bad], "not touched yet");
    // Then the standing notices, as a click on each would: hidden, still
    // reported, until their text changes.
    h.press(&mut vcx, "escape");
    assert_eq!(h.visible_notices(&vcx), Vec::<String>::new());
    assert_eq!(h.notices(&vcx), [failed, bad]);
    let mut snap = europe();
    snap.lists.get_mut("europe").unwrap().status = Status::Failed("refused".into());
    vcx.update(|_, cx| publish(cx, snap));
    let refused = "not resolved: refused \u{2014} shift+r retries";
    assert_eq!(h.visible_notices(&vcx), [refused]);
    // A verb's own word comes first, the standing notice after it.
    h.press(&mut vcx, "u");
    assert_eq!(h.visible_notices(&vcx), [NOTHING_TO_UNDO, refused]);
    // A current resolution clears it.
    vcx.update(|_, cx| publish(cx, europe()));
    assert_eq!(h.visible_notices(&vcx), [NOTHING_TO_UNDO]);
}

impl Harness {
    /// The open field's highlighted option.
    fn highlight(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.prompt_highlight())
    }
}

/// A highlight the trader moved (`up`/`down` through the insert bindings)
/// is a choice: enter adds the listed name, not the typed text; a press on
/// a listed row adds it at once.
#[gpui::test]
fn a_moved_highlight_or_a_row_press_adds_the_listed_name(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| publish_reference(cx, reference_with(&["CAC", "HSI"])));
    let (h, mut vcx) = open_with(cx, europe(), restored("europe"));
    h.press(&mut vcx, "o");
    let (_, _, options) = h.prompt(&vcx).unwrap();
    assert_eq!(options, ["CAC", "DAX", "HSI", "NDX", "SPX", "UKX"]);
    assert_eq!(h.highlight(&vcx).as_deref(), Some("CAC"));
    h.press(&mut vcx, "down");
    assert_eq!(h.highlight(&vcx).as_deref(), Some("DAX"));
    h.press(&mut vcx, "down up");
    assert_eq!(h.highlight(&vcx).as_deref(), Some("DAX"));
    h.press(&mut vcx, "up");
    assert_eq!(h.highlight(&vcx).as_deref(), Some("CAC"));
    // Typed `h` ranks HSI alone. As a guess, enter would add `h` as typed;
    // moved to, the highlight is the answer.
    vcx.simulate_input("h");
    assert_eq!(h.highlight(&vcx).as_deref(), Some("HSI"));
    h.press(&mut vcx, "down");
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    let mut next = europe_def();
    next.include.push("HSI".into());
    assert_eq!(
        h.edits(&mut vcx),
        [edit_of(&next)],
        "the listed name, not `h`"
    );
    assert_eq!(h.notices(&vcx), ["added HSI"]);
    // A press on a listed row picks it at once.
    h.press(&mut vcx, "o");
    h.draw(&mut vcx);
    let row = vcx
        .debug_bounds("watchlist-prompt-row-CAC")
        .expect("the list is painted");
    vcx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    next.include.push("CAC".into());
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)]);
    assert_eq!(h.notices(&vcx), ["added CAC"]);
    assert_eq!(
        h.origin(&vcx, "CAC").as_deref(),
        Some("manual \u{b7} pending")
    );
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
}

// ---- the rules popup and the rule prompt ----------------------------------

use crate::core::prompt::{EXPRESSION, WHOLE_DATASET};
use crate::core::rules::RuleRow;
use crate::tile::rules::{NO_RULE, NO_RULE_DATASET, RULES_UNCHANGED};

fn rule(dataset: &str, scope: Option<&str>, expression: Option<&str>) -> Rule {
    Rule {
        dataset: dataset.into(),
        scope: scope.map(str::to_string),
        expression: expression.map(str::to_string),
    }
}

/// [`europe`] with real rules: rule 1 over the whole of `risk`, rule 2
/// `risk` under the saved scope `eu`.
fn europe_rules() -> WatchlistSnapshot {
    let mut snap = europe();
    snap.lists.get_mut("europe").unwrap().definition.rules =
        vec![rule("risk", None, None), rule("risk", Some("eu"), None)];
    snap
}

/// `europe`'s definition as [`europe_rules`] holds it.
fn europe_def_with_rules() -> Watchlist {
    with_rules(vec![
        rule("risk", None, None),
        rule("risk", Some("eu"), None),
    ])
}

/// `europe`'s definition with `rules`.
fn with_rules(rules: Vec<Rule>) -> Watchlist {
    Watchlist {
        rules,
        ..europe_def()
    }
}

/// The factory's configuration: the prompt tests' schema (`risk`, `cvi`
/// eligible; `fx`, `underlyings` not) and saved scopes (`eu` over `book`,
/// `fx_only` over `pair`, `nothing` impossible).
fn config() -> WatchlistConfig {
    WatchlistConfig {
        schema: Rc::new(crate::core::prompt::tests::schema()),
        dims: Default::default(),
        saved: crate::core::prompt::tests::saved(),
        named: Default::default(),
    }
}

/// A tile on `snap`'s `europe` with the reference table and the
/// configuration pushed.
fn rules_shown(
    cx: &mut gpui::TestAppContext,
    snap: WatchlistSnapshot,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(|cx| publish_reference(cx, reference()));
    let (h, mut vcx) = open_with(cx, snap, restored("europe"));
    vcx.update(|_, cx| h.factory.set_config(config(), cx));
    (h, vcx)
}

impl Harness {
    /// The open popup's cursor and rows; `None` while it is closed.
    fn rules(&self, vcx: &gpui::VisualTestContext) -> Option<(usize, Vec<RuleRow>)> {
        self.tile.read_with(vcx, |t, _| t.rules_state())
    }
    /// The popup's rows as `(index, dataset, scope text, error)`.
    fn rule_rows(
        &self,
        vcx: &gpui::VisualTestContext,
    ) -> Vec<(usize, String, String, Option<String>)> {
        self.rules(vcx)
            .map(|(_, rows)| {
                rows.into_iter()
                    .map(|r| (r.index, r.dataset, r.scope_text, r.error))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn rules_cursor(&self, vcx: &gpui::VisualTestContext) -> Option<usize> {
        self.rules(vcx).map(|(c, _)| c)
    }
    /// The open field's hint line and whether it is a warning.
    fn hint(&self, vcx: &gpui::VisualTestContext) -> Option<(String, bool)> {
        self.tile.read_with(vcx, |t, _| t.prompt_hint())
    }
}

#[gpui::test]
fn r_opens_the_rules_popup_on_the_first_rule_and_j_k_step(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = rules_shown(cx, europe_rules());
    assert_eq!(h.rules(&vcx), None);
    h.press(&mut vcx, "r");
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    assert_eq!(h.rules_cursor(&vcx), Some(0));
    let row = |i: usize, scope: &str| (i, "risk".to_string(), scope.to_string(), None);
    assert_eq!(
        h.rule_rows(&vcx),
        [row(0, WHOLE_DATASET), row(1, "scope eu")]
    );
    // Painted hung from the header's rules item, one row per rule.
    h.draw(&mut vcx);
    let item = vcx
        .debug_bounds("watchlist-rules-7")
        .expect("the rules item is painted");
    let popup = vcx
        .debug_bounds("watchlist-rules-popup-7")
        .expect("the popup is painted");
    assert!(
        (popup.origin.x - item.origin.x).abs() <= gpui::px(1.0)
            && (popup.origin.y - item.bottom_left().y).abs() <= gpui::px(1.0),
        "{popup:?} hangs under {item:?}"
    );
    assert!(vcx.debug_bounds("watchlist-rule-row-7-1").is_some());
    assert!(vcx.debug_bounds("watchlist-rules-empty-7").is_none());
    // `j`/`k` step, clamped at either end.
    h.press(&mut vcx, "j");
    assert_eq!(h.rules_cursor(&vcx), Some(1));
    h.press(&mut vcx, "j");
    assert_eq!(h.rules_cursor(&vcx), Some(1), "clamped");
    h.press(&mut vcx, "k k");
    assert_eq!(h.rules_cursor(&vcx), Some(0));
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("DAX"),
        "the grid's cursor did not move"
    );
    // `r` again closes it; so does `escape`.
    h.press(&mut vcx, "r");
    assert_eq!(h.rules(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    h.press(&mut vcx, "r escape");
    assert_eq!(h.rules(&vcx), None);
    // The `⋯` menu's Rules… row and the palette open it too.
    h.press(&mut vcx, ". j j enter");
    assert_eq!(h.actions(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "watchlist::rules");
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    // A verb that leaves the popup closes it (the refresh, from the
    // palette: the popup's own context binds no `shift+r`).
    h.act(&mut vcx, "watchlist::refresh");
    assert_eq!(h.rules(&vcx), None);
    // A press on the grid closes it.
    h.press(&mut vcx, "r");
    h.draw(&mut vcx);
    let spx = vcx.debug_bounds("watchlist-row-SPX").unwrap();
    vcx.simulate_mouse_down(
        spx.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(h.rules(&vcx), None);
    // With no rules the popup says so.
    let mut snap = europe();
    snap.lists
        .get_mut("europe")
        .unwrap()
        .definition
        .rules
        .clear();
    vcx.update(|_, cx| publish(cx, snap));
    h.press(&mut vcx, "r");
    assert_eq!(h.rule_rows(&vcx), []);
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-rules-empty-7").is_some());
    h.press(&mut vcx, "x");
    assert_eq!(h.notices(&vcx), [NO_RULE]);
    assert!(h.edits(&mut vcx).is_empty());
    // Nothing shown: refused.
    vcx.update(|_, cx| publish(cx, WatchlistSnapshot::default()));
    assert_eq!(h.rules(&vcx), None, "the list went: the popup with it");
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "watchlist::rules");
    assert!(h.notices(&vcx).contains(&NOTHING_SHOWN.to_string()));
}

#[gpui::test]
fn o_dataset_then_whole_dataset_queues_a_rules_write(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = rules_shown(cx, europe_rules());
    h.press(&mut vcx, "r o");
    // The dataset step: a closed choice over the eligible datasets, the
    // popup still painted beneath the field.
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    let (text, error, rows) = h.prompt(&vcx).expect("the dataset field is open");
    assert_eq!((text.as_str(), error), ("", None));
    assert_eq!(rows, ["risk", "cvi"], "fx and underlyings are not eligible");
    assert_eq!(h.highlight(&vcx).as_deref(), Some("risk"));
    assert!(h.rules(&vcx).is_some(), "the popup stays open");
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-prompt-7").is_some());
    assert!(vcx.debug_bounds("watchlist-rules-popup-7").is_some());
    // A closed list: enter takes the highlight with nothing typed.
    h.press(&mut vcx, "enter");
    let (_, error, rows) = h.prompt(&vcx).expect("the scope field is open");
    assert_eq!(error, None);
    assert_eq!(rows, [WHOLE_DATASET, "eu", EXPRESSION]);
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(h.edits(&mut vcx).is_empty(), "nothing written yet");
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    let next = with_rules(vec![
        rule("risk", None, None),
        rule("risk", Some("eu"), None),
        rule("risk", None, None),
    ]);
    let edits = h.edits(&mut vcx);
    assert_eq!(edits, [edit_of(&next)]);
    let rules = edits[0].value.as_ref().unwrap()["rules"]
        .as_array()
        .unwrap();
    assert_eq!(rules.len(), 3, "the whole object");
    let added = rules[2].as_table().unwrap();
    assert_eq!(added.keys().collect::<Vec<_>>(), ["dataset"]);
    assert_eq!(added["dataset"].as_str(), Some("risk"));
    assert_eq!(h.notices(&vcx), ["added rule 3"]);
    // The popup stays, on the new rule, listing the pending rules; the
    // header counts them.
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    assert_eq!(h.rules_cursor(&vcx), Some(2));
    assert_eq!(h.rule_rows(&vcx).len(), 3);
    assert!(h.header(&vcx).contains("3 rules"), "{}", h.header(&vcx));
    // Escape peels the field first, then the popup.
    h.press(&mut vcx, "o");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    h.press(&mut vcx, "escape");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    h.press(&mut vcx, "escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert!(h.edits(&mut vcx).is_empty());
    // A dataset that is not eligible is refused under the field: typed
    // text that matches nothing is the answer as typed.
    h.press(&mut vcx, "r o");
    vcx.simulate_input("fx");
    h.press(&mut vcx, "enter");
    let (text, error, _) = h.prompt(&vcx).unwrap();
    assert_eq!(text, "fx");
    assert_eq!(
        error.as_deref(),
        Some("'fx' is not a dataset a rule may read")
    );
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(h.edits(&mut vcx).is_empty());
    // With no eligible dataset `o` is refused outright.
    vcx.update(|_, cx| h.factory.set_config(WatchlistConfig::default(), cx));
    h.press(&mut vcx, "escape");
    h.press(&mut vcx, "o");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(h.notices(&vcx), [NO_RULE_DATASET]);
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
}

#[gpui::test]
fn o_dataset_then_saved_scope_writes_scope(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = rules_shown(cx, europe_rules());
    h.press(&mut vcx, "r o enter");
    // Typed out, the saved scope is the highlight; enter takes it.
    vcx.simulate_input("eu");
    assert_eq!(h.highlight(&vcx).as_deref(), Some("eu"));
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    let next = with_rules(vec![
        rule("risk", None, None),
        rule("risk", Some("eu"), None),
        rule("risk", Some("eu"), None),
    ]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)]);
    assert_eq!(h.notices(&vcx), ["added rule 3"]);
    assert_eq!(h.rule_rows(&vcx)[2].2, "scope eu");
    // A saved scope the dataset cannot honour is not offered: typed, it
    // matches nothing, and is refused with the fold's reason; a name that
    // is no saved scope with the step's.
    h.press(&mut vcx, "o enter");
    vcx.simulate_input("fx_only");
    let (_, _, rows) = h.prompt(&vcx).unwrap();
    assert!(rows.is_empty(), "{rows:?}");
    h.press(&mut vcx, "enter");
    let (_, error, _) = h.prompt(&vcx).unwrap();
    assert!(
        error.as_deref().is_some_and(|e| e.contains("pair")),
        "{error:?}"
    );
    assert!(h.edits(&mut vcx).is_empty());
    h.press(&mut vcx, "backspace ".repeat(7).trim_end());
    vcx.simulate_input("whatever");
    h.press(&mut vcx, "enter");
    let (_, error, _) = h.prompt(&vcx).unwrap();
    assert_eq!(
        error.as_deref(),
        Some("'whatever' is not whole dataset, a saved scope or expression\u{2026}")
    );
    assert!(h.edits(&mut vcx).is_empty());
    // `cvi` has no `book`: `eu` is not offered over it.
    h.press(&mut vcx, "escape");
    h.press(&mut vcx, "o");
    vcx.simulate_input("cvi");
    h.press(&mut vcx, "enter");
    let (_, _, rows) = h.prompt(&vcx).unwrap();
    assert_eq!(rows, [WHOLE_DATASET, EXPRESSION]);
    h.press(&mut vcx, "escape");
}

#[gpui::test]
fn o_dataset_then_expression_validated_against_that_dataset(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = rules_shown(cx, europe_rules());
    h.press(&mut vcx, "r o enter down down");
    assert_eq!(h.highlight(&vcx).as_deref(), Some(EXPRESSION));
    h.press(&mut vcx, "enter");
    // The expression step: the completion over risk's columns alone.
    let (text, error, rows) = h.prompt(&vcx).expect("the expression field is open");
    assert_eq!((text.as_str(), error), ("", None));
    assert_eq!(rows, ["book", "underlying_ref", "npv", "not", "("]);
    assert_eq!(h.hint(&vcx), Some(("column".into(), false)));
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    // A column the dataset lacks is refused under the field, by the fold.
    vcx.simulate_input("pair = 'EURUSD'");
    assert_eq!(
        h.hint(&vcx),
        Some(("unknown column 'pair'".into(), true)),
        "the completion warns as it is typed"
    );
    h.press(&mut vcx, "enter");
    let (text, error, _) = h.prompt(&vcx).unwrap();
    assert_eq!(text, "pair = 'EURUSD'");
    assert!(
        error.as_deref().is_some_and(|e| e.contains("pair")),
        "{error:?}"
    );
    assert!(h.edits(&mut vcx).is_empty());
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-prompt-error-7").is_some());
    assert!(vcx.debug_bounds("watchlist-prompt-hint-7").is_some());
    // A valid one writes `expression`.
    h.press(&mut vcx, "backspace ".repeat(15).trim_end());
    vcx.simulate_input("book = 'BK000'");
    assert_eq!(
        h.hint(&vcx),
        Some(("and / or, or enter to apply".into(), false))
    );
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    let next = with_rules(vec![
        rule("risk", None, None),
        rule("risk", Some("eu"), None),
        rule("risk", None, Some("book = 'BK000'")),
    ]);
    let edits = h.edits(&mut vcx);
    assert_eq!(edits, [edit_of(&next)]);
    let added = edits[0].value.as_ref().unwrap()["rules"][2]
        .as_table()
        .unwrap();
    assert_eq!(added.keys().collect::<Vec<_>>(), ["dataset", "expression"]);
    assert_eq!(h.rule_rows(&vcx)[2].2, "book = 'BK000'");
    // A blank expression is refused.
    h.press(&mut vcx, "o enter down down enter enter");
    let (_, error, _) = h.prompt(&vcx).unwrap();
    assert_eq!(
        error.as_deref(),
        Some(crate::core::prompt::TYPE_AN_EXPRESSION)
    );
    h.press(&mut vcx, "escape");
}

#[gpui::test]
fn enter_on_a_rule_replaces_its_scope_in_place(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = rules_shown(cx, europe_rules());
    h.press(&mut vcx, "r j enter");
    // The scope step over the rule's own dataset; no dataset step.
    let (_, _, rows) = h.prompt(&vcx).expect("the scope field is open");
    assert_eq!(rows, [WHOLE_DATASET, "eu", EXPRESSION]);
    assert!(h.rules(&vcx).is_some());
    h.press(&mut vcx, "enter");
    let next = with_rules(vec![rule("risk", None, None), rule("risk", None, None)]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)], "rule 2 in place");
    assert_eq!(h.notices(&vcx), ["changed rule 2"]);
    assert_eq!(h.rules_cursor(&vcx), Some(1));
    assert_eq!(h.rule_rows(&vcx)[1].2, WHOLE_DATASET);
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    // The same scope again changes nothing and writes nothing.
    h.press(&mut vcx, "enter enter");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.notices(&vcx), [RULES_UNCHANGED]);
    // An expression in place, over the pending object.
    h.press(&mut vcx, "k enter down down enter");
    vcx.simulate_input("npv > 1");
    h.press(&mut vcx, "enter");
    let next = with_rules(vec![
        rule("risk", None, Some("npv > 1")),
        rule("risk", None, None),
    ]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)]);
    assert_eq!(h.notices(&vcx), ["changed rule 1"]);
    assert_eq!(h.rules_cursor(&vcx), Some(0));
    // Edit from the palette, with the popup closed, acts on nothing.
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "watchlist::rule_edit");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(h.notices(&vcx), [NO_RULE]);
}

#[gpui::test]
fn x_removes_the_cursor_rule_and_undo_restores_the_whole_rules_vector(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = rules_shown(cx, europe_rules());
    h.press(&mut vcx, "r j x");
    let removed = with_rules(vec![rule("risk", None, None)]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&removed)]);
    assert_eq!(h.notices(&vcx), ["removed rule 2"]);
    assert_eq!(h.rules_cursor(&vcx), Some(0));
    assert_eq!(h.rule_rows(&vcx).len(), 1);
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    assert!(
        h.header(&vcx).contains("1 rule \u{b7}"),
        "{}",
        h.header(&vcx)
    );
    // Undo restores the rules whole, in their order (`u` leaves the
    // popup first).
    h.press(&mut vcx, "escape");
    h.press(&mut vcx, "u");
    assert_eq!(h.edits(&mut vcx), [edit_of(&europe_def_with_rules())]);
    assert_eq!(h.notices(&vcx), ["undid 1 change"]);
    h.press(&mut vcx, "r");
    assert_eq!(h.rule_rows(&vcx).len(), 2);
    h.press(&mut vcx, "escape");
    h.press(&mut vcx, "ctrl-r");
    assert_eq!(h.edits(&mut vcx), [edit_of(&removed)]);
    // The last rule removed leaves the empty row; x then has no rule.
    h.press(&mut vcx, "r x");
    assert_eq!(h.edits(&mut vcx), [edit_of(&with_rules(vec![]))]);
    assert_eq!(h.rule_rows(&vcx), []);
    h.press(&mut vcx, "x");
    assert_eq!(h.notices(&vcx), [NO_RULE]);
    assert!(h.edits(&mut vcx).is_empty());
    // A reload changing the rules elsewhere (not the object the edits
    // started from, which the in-flight chain keeps): undo skips the whole
    // change.
    let mut foreign = europe();
    foreign.lists.get_mut("europe").unwrap().definition.rules = vec![rule("cvi", None, None)];
    vcx.update(|_, cx| publish(cx, foreign));
    assert_eq!(
        h.rule_rows(&vcx).len(),
        1,
        "the foreign rules are the truth"
    );
    h.press(&mut vcx, "escape");
    h.press(&mut vcx, "u");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(
        h.notices(&vcx),
        ["undid 0 changes \u{2014} 1 changed elsewhere"]
    );
}

/// Review focus: a rule over a dataset the startup schema lacks is listed
/// with its error and removable; `o` never offers that dataset.
#[gpui::test]
fn an_ineligible_rule_is_shown_with_its_error_and_removable(cx: &mut gpui::TestAppContext) {
    let mut snap = europe();
    let s = snap.lists.get_mut("europe").unwrap();
    s.definition.rules = vec![rule("risk", None, None), rule("gone", None, None)];
    let why = "no dataset 'gone' (a dataset added since launch needs a restart)";
    s.rule_errors = vec![RuleError {
        index: 1,
        reason: why.into(),
    }];
    let (h, mut vcx) = rules_shown(cx, snap);
    h.press(&mut vcx, "r");
    assert_eq!(
        h.rule_rows(&vcx),
        [
            (0, "risk".into(), WHOLE_DATASET.into(), None),
            (1, "gone".into(), WHOLE_DATASET.into(), Some(why.into())),
        ]
    );
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-rule-error-7-1").is_some());
    assert!(vcx.debug_bounds("watchlist-rule-error-7-0").is_none());
    // `o` offers the eligible datasets only.
    h.press(&mut vcx, "o");
    let (_, _, rows) = h.prompt(&vcx).unwrap();
    assert_eq!(rows, ["risk", "cvi"]);
    h.press(&mut vcx, "escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    // `x` writes the rules without it.
    h.press(&mut vcx, "j x");
    let next = with_rules(vec![rule("risk", None, None)]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)]);
    // The standing notice about rule 2 stays until the reload folds the
    // rules without it.
    assert_eq!(h.notices(&vcx)[0], "removed rule 2");
    assert_eq!(h.rule_rows(&vcx).len(), 1);
    // The pending rules are not the snapshot's: its errors do not apply
    // to them until the reload folds them.
    assert_eq!(h.rule_rows(&vcx)[0].3, None);
    assert!(
        h.header(&vcx).contains("1 rule \u{b7}"),
        "{}",
        h.header(&vcx)
    );
}

#[gpui::test]
fn expression_completion_offers_only_the_rules_dataset_columns(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = rules_shown(cx, europe_rules());
    h.press(&mut vcx, "r o");
    vcx.simulate_input("cvi");
    assert_eq!(h.highlight(&vcx).as_deref(), Some("cvi"));
    // cvi has no saved scope to offer: one step down is the expression.
    h.press(&mut vcx, "enter down enter");
    let (_, _, rows) = h.prompt(&vcx).expect("the expression field is open");
    assert_eq!(
        rows,
        ["underlying_ref", "term", "atm", "not", "("],
        "cvi's columns, not risk's"
    );
    // Typing ranks; a moved highlight is a completion enter writes into
    // the field, which stays open at the next position.
    vcx.simulate_input("te");
    let (_, _, rows) = h.prompt(&vcx).unwrap();
    assert_eq!(rows, ["term"]);
    h.press(&mut vcx, "down enter");
    let (text, error, rows) = h.prompt(&vcx).unwrap();
    assert_eq!(text, "term ");
    assert_eq!(error, None);
    assert_eq!(
        rows,
        ["=", "!=", "<", "<=", ">", ">=", "in"],
        "a number's operators"
    );
    assert_eq!(h.hint(&vcx), Some(("operator for term".into(), false)));
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    // A press on a completion row writes it too, and keeps the field.
    h.draw(&mut vcx);
    let row = vcx
        .debug_bounds("watchlist-prompt-row->")
        .expect("the operator row is painted");
    vcx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    let (text, _, _) = h.prompt(&vcx).unwrap();
    assert_eq!(text, "term > ");
    assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    assert_eq!(
        h.hint(&vcx),
        Some(("value for term \u{b7} a number, e.g. 1000".into(), false))
    );
    // Enter with an unmoved highlight is the answer.
    vcx.simulate_input("1");
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    let next = with_rules(vec![
        rule("risk", None, None),
        rule("risk", Some("eu"), None),
        rule("cvi", None, Some("term > 1")),
    ]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)]);
    assert_eq!(h.notices(&vcx), ["added rule 3"]);
}

/// A press on a row of the inline popup (a rule prompt open above it)
/// moves the popup's cursor and leaves the keyboard in the field.
#[gpui::test]
fn a_press_on_an_inline_rule_row_moves_the_cursor_and_keeps_the_field(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = rules_shown(cx, europe_rules());
    h.press(&mut vcx, "r o");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert_eq!(h.rules_cursor(&vcx), Some(0));
    h.draw(&mut vcx);
    let row = vcx
        .debug_bounds("watchlist-rule-row-7-1")
        .expect("the inline popup paints its rows");
    let field = vcx.debug_bounds("watchlist-prompt-7").unwrap();
    assert!(
        row.origin.y >= field.bottom_left().y,
        "painted beneath the field"
    );
    vcx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(h.rules_cursor(&vcx), Some(1));
    assert!(h.prompt(&vcx).is_some(), "the field is still open");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    // The field still takes the keys: the dataset step goes on.
    vcx.simulate_input("cvi");
    assert_eq!(h.highlight(&vcx).as_deref(), Some("cvi"));
    h.press(&mut vcx, "escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("rules"));
    assert_eq!(h.rules_cursor(&vcx), Some(1));
}

/// [`list`] owned by the user layer, over a lower copy when `shadowed`.
fn user_list(names: &[&str], shadowed: Option<Layer>) -> WatchlistState {
    WatchlistState {
        layer: Some(Layer::User),
        shadowed,
        ..list(names)
    }
}

/// [`two`] plus `mine` from the user layer, over a desk copy when
/// `shadowed`.
fn with_mine(shadowed: bool) -> WatchlistSnapshot {
    let mut snap = two();
    snap.lists.insert(
        "mine".into(),
        user_list(&["HSI", "NKY"], shadowed.then_some(Layer::Desk)),
    );
    snap
}

/// The whole object `list`, written under `name` from this tile.
fn set_of(name: &str, list: &Watchlist) -> ConfigEdit {
    ConfigEdit {
        doc: geode_core::watchlist::WATCHLISTS_DOC,
        object: name.into(),
        value: Some(to_toml(list)),
        origin: Some(TileId(TILE)),
    }
}

/// `name`'s user definition, removed, from this tile.
fn removal_of(name: &str) -> ConfigEdit {
    ConfigEdit {
        doc: geode_core::watchlist::WATCHLISTS_DOC,
        object: name.into(),
        value: None,
        origin: Some(TileId(TILE)),
    }
}

#[gpui::test]
fn new_asks_a_name_validates_it_and_writes_an_empty_list_then_shows_it(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_with(cx, two(), restored("a"));
    h.act(&mut vcx, "watchlist::new");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert_eq!(h.label(&vcx).as_deref(), Some("New watchlist"));
    let (text, error, options) = h.prompt(&vcx).expect("the field is open");
    assert_eq!((text.as_str(), error, options.len()), ("", None, 0));
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-prompt-7").is_some());
    assert!(
        vcx.debug_bounds("watchlist-prompt-list-7").is_none(),
        "a name field paints no list"
    );
    // A clash ignoring case names the other list; the field stays open.
    vcx.simulate_input("B");
    h.press(&mut vcx, "enter");
    let (text, error, _) = h.prompt(&vcx).expect("still open");
    assert_eq!(text, "B");
    assert_eq!(error.as_deref(), Some("'B' already exists ('b')"));
    assert!(h.edits(&mut vcx).is_empty());
    // A reserved word too.
    h.retype(&mut vcx, "and");
    h.press(&mut vcx, "enter");
    let (_, error, _) = h.prompt(&vcx).expect("still open");
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("reserved word")),
        "{error:?}"
    );
    // A good name, trimmed: one whole-object write of an empty list, and
    // the tile shows it ahead of the reload.
    h.retype(&mut vcx, " asia ");
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert_eq!(h.edits(&mut vcx), [set_of("asia", &Watchlist::default())]);
    assert_eq!(h.title(&mut vcx), "Watchlist: asia");
    assert_eq!(h.empty(&vcx).as_deref(), Some("saving asia\u{2026}"));
    assert_eq!(h.switcher(&vcx), None, "nothing went away");
    assert!(h.awaiting(&vcx));
    // The reload carrying it.
    let mut snap = two();
    snap.lists.insert("asia".into(), user_list(&[], None));
    vcx.update(|_, cx| publish(cx, snap));
    assert_eq!(h.empty(&vcx), None);
    assert!(
        h.header(&vcx).starts_with("Watchlist: asia"),
        "{}",
        h.header(&vcx)
    );
    assert!(!h.awaiting(&vcx));
    // A refused create goes back to what was shown before it.
    h.act(&mut vcx, "watchlist::new");
    vcx.simulate_input("pacific");
    h.press(&mut vcx, "enter");
    assert_eq!(h.edits(&mut vcx).len(), 1);
    assert_eq!(h.title(&mut vcx), "Watchlist: pacific");
    let why = "watchlists not written: the user layer is read-only";
    h.shell_says(&mut vcx, TileNotice::Refused(why.into()));
    assert_eq!(h.title(&mut vcx), "Watchlist: asia");
    assert_eq!(h.notices(&vcx), [why]);
    assert!(!h.awaiting(&vcx));
}

/// Review Focus 5: a clone of a desk-layer list with a pending edit is one
/// write of the pending definition under the new name, and nothing for
/// the list cloned, so the desk list is not forked.
#[gpui::test]
fn clone_writes_one_new_user_object_and_no_fork(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = europe_shown(cx);
    h.goto(&mut vcx, "NDX");
    h.press(&mut vcx, "x");
    assert_eq!(h.edits(&mut vcx).len(), 1, "the pending edit");
    assert!(h.origin(&vcx, "NDX").is_none(), "pending");
    h.act(&mut vcx, "watchlist::clone");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert_eq!(h.label(&vcx).as_deref(), Some("clone europe as"));
    vcx.simulate_input("Europe");
    h.press(&mut vcx, "enter");
    let (_, error, _) = h.prompt(&vcx).expect("refused, still open");
    assert_eq!(error.as_deref(), Some("'Europe' already exists ('europe')"));
    h.retype(&mut vcx, "europe2");
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    let mut pending = europe_def();
    pending.include.retain(|n| n != "NDX");
    let edits = h.edits(&mut vcx);
    assert_eq!(edits, [set_of("europe2", &pending)]);
    assert!(
        edits.iter().all(|e| e.object != "europe"),
        "from is never touched"
    );
    assert_eq!(h.title(&mut vcx), "Watchlist: europe2");
    assert_eq!(h.empty(&vcx).as_deref(), Some("saving europe2\u{2026}"));
    // The reload carrying it shows the clone.
    let mut snap = europe();
    let mut cloned = resolved();
    cloned.definition = pending.clone();
    cloned.layer = Some(Layer::User);
    cloned.members.retain(|m| m.name != "NDX");
    snap.lists.insert("europe2".into(), cloned);
    vcx.update(|_, cx| publish(cx, snap));
    assert_eq!(h.empty(&vcx), None);
    assert_eq!(h.shown(&vcx), ["DAX", "SPX", "UKX"]);
    assert!(!h.awaiting(&vcx));
    // Nothing shown: refused.
    vcx.update(|_, cx| publish(cx, WatchlistSnapshot::default()));
    h.act(&mut vcx, "watchlist::clone");
    assert_eq!(h.prompt(&vcx), None);
    assert!(h.notices(&vcx).contains(&NOTHING_SHOWN.to_string()));
}

#[gpui::test]
fn rename_is_refused_on_a_desk_list_and_confirms_on_a_user_one(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, with_mine(true), restored("a"));
    h.act(&mut vcx, "watchlist::rename");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(
        h.notices(&vcx),
        ["a is defined in desk config; Geode cannot rename it"]
    );
    assert!(h.edits(&mut vcx).is_empty());
    // A user copy over a desk one: Revert… is the verb.
    h.switch_to(&mut vcx, "mine");
    h.act(&mut vcx, "watchlist::rename");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(
        h.notices(&vcx),
        ["mine shadows the desk copy \u{2014} Revert\u{2026} removes it"]
    );
    // Owned outright: the name field, seeded with the name, selected.
    vcx.update(|_, cx| publish(cx, with_mine(false)));
    h.act(&mut vcx, "watchlist::rename");
    assert_eq!(h.label(&vcx).as_deref(), Some("rename mine to"));
    let (text, _, _) = h.prompt(&vcx).expect("open");
    assert_eq!(text, "mine", "seeded");
    h.press(&mut vcx, "enter");
    let (_, error, _) = h.prompt(&vcx).expect("still open");
    assert_eq!(error.as_deref(), Some("mine is already its name"));
    vcx.simulate_input("ours");
    let (text, _, _) = h.prompt(&vcx).expect("open");
    assert_eq!(text, "ours", "typing replaces the selected seed");
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(
        h.question(&vcx).as_deref(),
        Some("rename mine \u{2192} ours \u{2014} y renames")
    );
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    assert!(h.edits(&mut vcx).is_empty(), "nothing before y");
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("watchlist-confirm-7-bar").is_some());
    h.press(&mut vcx, "y");
    assert_eq!(h.question(&vcx), None);
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    // One batch of two: set `ours`, remove `mine`.
    let mine = with_mine(false).lists["mine"].definition.clone();
    assert_eq!(
        h.edits(&mut vcx),
        [set_of("ours", &mine), removal_of("mine")]
    );
    assert_eq!(h.title(&mut vcx), "Watchlist: ours");
    assert_eq!(h.empty(&vcx).as_deref(), Some("saving ours\u{2026}"));
    // The switcher leaves the old name out meanwhile.
    h.press(&mut vcx, "g w");
    assert_eq!(h.switcher(&vcx), rows(&["a", "b"]));
    h.press(&mut vcx, "escape");
    // The reload carrying both.
    let mut snap = two();
    snap.lists
        .insert("ours".into(), user_list(&["HSI", "NKY"], None));
    vcx.update(|_, cx| publish(cx, snap));
    assert_eq!(h.empty(&vcx), None);
    assert_eq!(h.shown(&vcx), ["HSI", "NKY"]);
    assert!(!h.awaiting(&vcx));
}

#[gpui::test]
fn delete_confirms_and_leaves_the_empty_state_with_the_switcher_open(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_with(cx, with_mine(false), restored("mine"));
    h.act(&mut vcx, "watchlist::delete");
    assert_eq!(
        h.question(&vcx).as_deref(),
        Some("delete mine \u{2014} y deletes")
    );
    assert!(h.edits(&mut vcx).is_empty(), "nothing before y");
    h.press(&mut vcx, "y");
    assert_eq!(h.question(&vcx), None);
    assert_eq!(h.edits(&mut vcx), [removal_of("mine")]);
    assert_eq!(h.title(&mut vcx), header::NONE_SHOWN);
    assert_eq!(
        h.empty(&vcx).as_deref(),
        Some("no watchlist shown \u{2014} g w switches")
    );
    assert_eq!(h.switcher(&vcx), rows(&["a", "b"]), "mine is out");
    assert_eq!(h.mode(&vcx).as_deref(), Some("menu"));
    // The reload without it keeps the switcher as it is.
    vcx.update(|_, cx| publish(cx, two()));
    assert_eq!(h.switcher(&vcx), rows(&["a", "b"]));
    assert!(!h.awaiting(&vcx));
    h.press(&mut vcx, "j enter");
    assert_eq!(h.title(&mut vcx), "Watchlist: b");
    // A desk list cannot be deleted.
    h.act(&mut vcx, "watchlist::delete");
    assert_eq!(h.question(&vcx), None);
    assert_eq!(
        h.notices(&vcx),
        ["b is defined in desk config; Geode cannot delete it"]
    );
    assert!(h.edits(&mut vcx).is_empty());
}

#[gpui::test]
fn revert_is_offered_only_over_a_shadowed_user_copy_and_gates_the_verbs(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_with(cx, with_mine(true), restored("a"));
    let none = |t: &str| (t.to_string(), None);
    let off = |t: &str, why: &str| (t.to_string(), Some(why.to_string()));
    h.press(&mut vcx, ".");
    let reasons = h.reasons(&vcx).unwrap();
    assert!(
        !reasons.iter().any(|(t, _)| t == "Revert\u{2026}"),
        "{reasons:?}"
    );
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "watchlist::revert");
    assert_eq!(h.question(&vcx), None);
    assert_eq!(
        h.notices(&vcx),
        ["a has no copy beneath yours to revert to"]
    );
    // Over a shadowed user copy the row is listed and the verb asks.
    h.switch_to(&mut vcx, "mine");
    h.press(&mut vcx, ".");
    let reasons = h.reasons(&vcx).unwrap();
    assert_eq!(reasons.last(), Some(&none("Revert\u{2026}")));
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "watchlist::revert");
    assert_eq!(
        h.question(&vcx).as_deref(),
        Some("revert mine to the desk copy \u{2014} y reverts")
    );
    h.press(&mut vcx, "y");
    assert_eq!(h.edits(&mut vcx), [removal_of("mine")]);
    assert_eq!(h.title(&mut vcx), "Watchlist: mine", "still shown");
    assert_eq!(h.shown(&vcx), ["HSI", "NKY"]);
    // Until the reload shows the desk copy, the member and rules verbs
    // are refused: nothing written.
    let reverting = "reverting mine\u{2026}";
    h.press(&mut vcx, "x");
    assert_eq!(h.notices(&vcx), [reverting]);
    assert!(h.edits(&mut vcx).is_empty());
    h.press(&mut vcx, "o");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(h.notices(&vcx), [reverting]);
    // The rules popup is a rules verb: the key, the palette and the row
    // agree with the lane.
    h.press(&mut vcx, "r");
    assert_eq!(h.rules(&vcx), None);
    assert_ne!(h.mode(&vcx).as_deref(), Some("rules"));
    assert_eq!(h.notices(&vcx), [reverting]);
    h.act(&mut vcx, "watchlist::rules");
    assert_eq!(h.rules(&vcx), None);
    assert_eq!(h.notices(&vcx), [reverting]);
    h.press(&mut vcx, ".");
    let reasons = h.reasons(&vcx).unwrap();
    assert_eq!(reasons[0], off("Add name", reverting));
    assert_eq!(reasons[1], off("Remove name", reverting));
    assert_eq!(reasons[2], off("Rules\u{2026}", reverting));
    assert_eq!(reasons[4], off("Undo", reverting));
    assert_eq!(reasons[3], none("Resolve now"), "shift+r stays");
    h.press(&mut vcx, "escape");
    // The reload: the desk copy wins, nothing beneath it.
    let mut snap = with_mine(false);
    snap.lists.get_mut("mine").unwrap().layer = Some(Layer::Desk);
    vcx.update(|_, cx| publish(cx, snap));
    h.press(&mut vcx, "x");
    assert_eq!(h.edits(&mut vcx).len(), 1, "the verbs act again");
    assert_eq!(h.notices(&vcx), ["removed HSI"]);
}

#[gpui::test]
fn n_on_the_confirm_bar_cancels_and_any_other_key_cancels_too(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, with_mine(false), restored("mine"));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("HSI"));
    h.act(&mut vcx, "watchlist::delete");
    assert!(h.question(&vcx).is_some());
    h.press(&mut vcx, "n");
    assert_eq!(h.question(&vcx), None);
    assert_eq!(h.notices(&vcx), ["mine not deleted"]);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert!(h.edits(&mut vcx).is_empty());
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    // Any other key answers no too, and is the answer alone: `j` moves no
    // cursor.
    for key in ["j", "escape", "x"] {
        h.act(&mut vcx, "watchlist::delete");
        assert!(h.question(&vcx).is_some(), "{key}");
        h.press(&mut vcx, key);
        assert_eq!(h.question(&vcx), None, "{key}");
        assert_eq!(h.notices(&vcx), ["mine not deleted"], "{key}");
        assert_eq!(h.cursor(&vcx).as_deref(), Some("HSI"), "{key}");
        assert!(h.edits(&mut vcx).is_empty(), "{key}");
    }
    // A verb from the palette under the question answers no first, then
    // acts; Cancel from the palette answers no and stops there.
    h.act(&mut vcx, "watchlist::delete");
    h.act(&mut vcx, "watchlist::add");
    assert_eq!(h.question(&vcx), None);
    assert!(h.prompt(&vcx).is_some(), "the add field opened");
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "watchlist::delete");
    h.act(&mut vcx, "watchlist::cancel");
    assert_eq!(h.question(&vcx), None);
    assert_eq!(
        h.visible_notices(&vcx),
        ["mine not deleted"],
        "not dismissed"
    );
    // A press on the Yes button is y.
    h.act(&mut vcx, "watchlist::delete");
    h.draw(&mut vcx);
    let at = vcx
        .debug_bounds("watchlist-confirm-7-yes")
        .expect("Yes")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    h.draw(&mut vcx);
    assert_eq!(h.question(&vcx), None);
    assert_eq!(h.edits(&mut vcx), [removal_of("mine")]);
}

/// A desk copy appearing under the name while the question stands: `y`
/// would remove the user copy and leave the desk one under the old name,
/// so the ownership is checked again at `y`, not only the name.
#[gpui::test]
fn y_on_a_rename_checks_ownership_again(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, with_mine(false), restored("mine"));
    h.act(&mut vcx, "watchlist::rename");
    vcx.simulate_input("ours");
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.question(&vcx).as_deref(),
        Some("rename mine \u{2192} ours \u{2014} y renames")
    );
    // The reload: `mine` now shadows a desk copy.
    vcx.update(|_, cx| publish(cx, with_mine(true)));
    assert!(h.question(&vcx).is_some(), "the question stands");
    h.press(&mut vcx, "y");
    assert_eq!(h.question(&vcx), None);
    assert!(h.edits(&mut vcx).is_empty(), "nothing written");
    assert_eq!(
        h.notices(&vcx),
        ["not renamed: mine shadows the desk copy \u{2014} Revert\u{2026} removes it"]
    );
    assert_eq!(h.title(&mut vcx), "Watchlist: mine");
    assert!(!h.awaiting(&vcx));
    // A clash appearing meanwhile is refused the same way.
    vcx.update(|_, cx| publish(cx, with_mine(false)));
    h.act(&mut vcx, "watchlist::rename");
    vcx.simulate_input("ours");
    h.press(&mut vcx, "enter");
    let mut snap = with_mine(false);
    snap.lists.insert("Ours".into(), user_list(&[], None));
    vcx.update(|_, cx| publish(cx, snap));
    h.press(&mut vcx, "y");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(
        h.notices(&vcx),
        ["not renamed: 'ours' already exists ('Ours')"]
    );
}

/// Clone… is allowed while a revert is on its way; its refusal is its own,
/// and the revert, still in the shell's batch, keeps its gate until the
/// revert's own refusal or its reload.
#[gpui::test]
fn a_refused_clone_does_not_release_the_revert_gate(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, with_mine(true), restored("mine"));
    h.act(&mut vcx, "watchlist::revert");
    h.press(&mut vcx, "y");
    assert_eq!(h.edits(&mut vcx), [removal_of("mine")]);
    let reverting = "reverting mine\u{2026}";
    h.act(&mut vcx, "watchlist::clone");
    vcx.simulate_input("mine2");
    h.press(&mut vcx, "enter");
    assert_eq!(h.edits(&mut vcx).len(), 1, "the clone");
    assert_eq!(h.title(&mut vcx), "Watchlist: mine2");
    // The clone's refusal: back to `mine`, still reverting.
    let why = "watchlists not written: the user layer is read-only";
    h.shell_says(&mut vcx, TileNotice::Refused(why.into()));
    assert_eq!(h.title(&mut vcx), "Watchlist: mine");
    assert_eq!(h.notices(&vcx), [why]);
    h.press(&mut vcx, "x");
    assert_eq!(h.notices(&vcx), [reverting]);
    assert!(h.edits(&mut vcx).is_empty());
    // The revert's own refusal ends the wait: the user copy stands.
    h.shell_says(&mut vcx, TileNotice::Refused(why.into()));
    h.press(&mut vcx, "x");
    assert_eq!(h.edits(&mut vcx).len(), 1, "the verbs act again");
    assert_eq!(h.notices(&vcx), ["removed HSI"]);
}
