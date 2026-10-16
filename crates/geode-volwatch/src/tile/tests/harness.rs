//! The tile hosted the way the shell hosts it: built by the factory over a
//! `for_tests` data handle (the test is the data service), under a shell
//! stand-in that resolves keys through the app's keymap, in a `Root`.
//! Keys go through the stand-in, answers through the content's `deliver`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use chrono::{TimeZone as _, Utc};
use geode_core::config::Layer;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::query::{HistoryKeysOutcome, HistoryKeysParams, QueryKey};
use geode_core::scopes::SavedScopes;
use geode_core::volboard::{
    Cell, Change, Lookback, VolBoard, VolBoardOutcome, VolBoardParams, VolRow,
};
use geode_core::watchlist::Watchlist;
use geode_core::watchlist::members::{Member, Origin};
use geode_core::watchlist::state::{Status, WatchlistSnapshot, WatchlistState};
use geode_data::{DataHandle, Request};
use geode_shell::actions::{ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameRef};
use geode_shell::keymap::{KeyContext, Keymap, MatchResult, Matcher, build_keymap};
use geode_shell::module::{Delivery, FindEvent, ModuleFactory, ModuleRoster, TileContent};
use geode_shell::tiling::{TileId, WorkspaceIx};
use geode_shell::watchlist::WatchlistGlobal;
use gpui::{AppContext as _, Entity, Window};

use crate::content::{VolwatchFactory, VolwatchSettings};
use crate::core::views::{Ask, View};
use crate::tile::VolwatchTile;

pub(super) const TILE: u64 = 7;
pub(super) const KEY: QueryKey = QueryKey(TILE);
/// The list every fixture shows.
pub(super) const LIST: &str = "europe_risk";
/// The members the surface fixture holds: any other is `missing`.
const IN_SURFACE: &[&str] = &["DAX", "SX5E", "UKX", "SPX"];

/// What the shell root is to a tile for keys: a focused ancestor whose
/// listener matches each keystroke against the app's keymap under the
/// stack `[workspace, tile, <tile's context>]` and dispatches the match
/// through the tile's door.
struct ShellStandIn {
    focus: gpui::FocusHandle,
    tile: Entity<VolwatchTile>,
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
                let stack = vec![
                    KeyContext::new("workspace"),
                    KeyContext::new("tile"),
                    context,
                ];
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
/// builtin layer with this module's fragment spliced in.
pub(super) fn app_keymap(factory: &Rc<VolwatchFactory>) -> Keymap {
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

#[allow(dead_code, reason = "the key, chrome and band tests drive the rest")]
pub(super) struct Harness {
    pub tile: Entity<VolwatchTile>,
    /// Driven through the trait: the shell's door.
    pub content: Rc<dyn TileContent>,
    pub factory: Rc<VolwatchFactory>,
    pub data: DataHandle,
    /// What the tile asked of the data service; the test answers.
    pub rx: RefCell<Option<Receiver<Request>>>,
    pub frame: Entity<Frame>,
    pub diagnostics: Entity<Diagnostics>,
}

struct Built {
    content: Rc<dyn TileContent>,
    tile: Entity<VolwatchTile>,
    shell_focus: gpui::FocusHandle,
    frame: Entity<Frame>,
    diagnostics: Entity<Diagnostics>,
}

/// A tile built by the factory after `snap` was published, with
/// `restored` as its record, announced visible as the shell announces a
/// fresh occupant, and focused.
pub(super) fn open_with(
    cx: &mut gpui::TestAppContext,
    snap: WatchlistSnapshot,
    restored: Option<toml::Table>,
    settings: VolwatchSettings,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::init);
    cx.update(|cx| cx.set_global(WatchlistGlobal(Arc::new(snap))));
    let (data, rx) = DataHandle::for_tests();
    let factory = Rc::new(VolwatchFactory::new(data.clone(), settings));
    let keymap = Rc::new(app_keymap(&factory));
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
                    diagnostics.clone(),
                    window,
                    cx,
                );
                assert_eq!(occupant.kind, crate::KIND);
                let tile = occupant.view.clone().downcast::<VolwatchTile>().unwrap();
                let content: Rc<dyn TileContent> = occupant.content.into();
                // The shell announces initial visibility right after create.
                content.set_visible(true, cx);
                let shell_focus = cx.focus_handle();
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
                    diagnostics,
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
        built.content.set_focused(true, cx);
    });
    (
        Harness {
            tile: built.tile,
            content: built.content,
            factory,
            data,
            rx: RefCell::new(Some(rx)),
            frame: built.frame,
            diagnostics: built.diagnostics,
        },
        vcx,
    )
}

/// `names` as a current list's live members, every one manual.
fn state_of(names: &[&str]) -> WatchlistState {
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

/// [`LIST`] holding `names`.
pub(super) fn list_of<const N: usize>(names: [&str; N]) -> WatchlistSnapshot {
    let mut snap = WatchlistSnapshot::default();
    snap.lists.insert(LIST.into(), state_of(&names));
    snap
}

/// `europe_risk = [DAX, SX5E, NEW]`; NEW is not in the surface.
pub(super) fn one_list() -> WatchlistSnapshot {
    list_of(["DAX", "SX5E", "NEW"])
}

pub(super) fn restored(list: &str) -> toml::Table {
    crate::core::session::to_table(&crate::core::session::State {
        list: Some(list.into()),
        ..Default::default()
    })
}

/// A board answering `asked` for `members`: every asked cell 15.9 with a
/// +1.1 change from 14.8 on 2026-10-09 over the ask's one lookback, a
/// 91st-percentile rank of 251 when a window was asked, 30 points per
/// sparkline; a member outside the surface fixture is `missing`, its row
/// blank.
pub(super) fn board_for(members: &[&str], asked: &Ask) -> VolBoard {
    let at = Utc.with_ymd_and_hms(2026, 10, 9, 21, 0, 0).unwrap();
    let now = Utc::now();
    let mut board = VolBoard::default();
    for &name in members {
        let present = IN_SURFACE.contains(&name);
        let cells = asked
            .measures
            .iter()
            .map(|m| Cell {
                measure: m.clone(),
                current: present.then_some(15.9),
                current_at: present.then_some(now),
                changes: vec![present.then_some(Change {
                    value: 1.1,
                    from: 14.8,
                    at,
                })],
                pctl: (present && asked.window.is_some()).then_some((0.91, 251)),
            })
            .collect();
        let sparks = asked
            .sparklines
            .iter()
            .map(|_| present.then(|| (0..30).map(|i| 14.0 + f64::from(i) * 0.1).collect()))
            .collect();
        board.rows.push(VolRow {
            underlying: name.to_string(),
            as_of: present.then_some(now),
            cells,
            sparks,
        });
        if !present {
            board.missing.push(name.to_string());
        }
    }
    board
}

#[allow(dead_code, reason = "the key, chrome and band tests drive the rest")]
impl Harness {
    /// Every request the tile made since the last call. Submission is
    /// synchronous, so the channel already holds them.
    pub(super) fn requests(&self) -> Vec<Request> {
        match self.rx.borrow().as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => Vec::new(),
        }
    }

    pub(super) fn boards(reqs: &[Request]) -> Vec<&VolBoardParams> {
        reqs.iter()
            .filter_map(|r| match r {
                Request::VolBoard(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    pub(super) fn keys_asks(reqs: &[Request]) -> Vec<&HistoryKeysParams> {
        reqs.iter()
            .filter_map(|r| match r {
                Request::HistoryKeys(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    pub(super) fn deliver(&self, vcx: &mut gpui::VisualTestContext, d: Delivery) {
        vcx.update(|window, cx| self.content.deliver(d, window, cx));
        vcx.run_until_parked();
    }

    pub(super) fn answer_board(
        &self,
        vcx: &mut gpui::VisualTestContext,
        tag: u64,
        board: VolBoard,
    ) {
        self.deliver(
            vcx,
            Delivery::VolBoard(VolBoardOutcome {
                key: KEY,
                tag,
                submitted: Instant::now(),
                result: Ok(board),
            }),
        );
    }

    pub(super) fn fail_board(&self, vcx: &mut gpui::VisualTestContext, tag: u64, text: &str) {
        self.deliver(
            vcx,
            Delivery::VolBoard(VolBoardOutcome {
                key: KEY,
                tag,
                submitted: Instant::now(),
                result: Err(text.into()),
            }),
        );
    }

    pub(super) fn answer_keys(
        &self,
        vcx: &mut gpui::VisualTestContext,
        tag: u64,
        values: Option<Vec<&str>>,
    ) {
        self.deliver(
            vcx,
            Delivery::HistoryKeys(HistoryKeysOutcome {
                key: KEY,
                tag,
                dataset: crate::core::views::SURFACE.into(),
                column: "tenor".into(),
                values: values.map(|v| v.into_iter().map(str::to_string).collect()),
            }),
        );
    }

    /// Keys as the trader types them, through the stand-in.
    pub(super) fn press(&self, vcx: &mut gpui::VisualTestContext, keys: &str) {
        vcx.simulate_keystrokes(keys);
        vcx.run_until_parked();
    }

    /// A registered action, the way the palette reaches the tile.
    pub(super) fn act(&self, vcx: &mut gpui::VisualTestContext, id: &str) -> bool {
        let id = ActionId(id.to_string());
        vcx.update(|window, cx| self.content.dispatch(&id, None, window, cx))
    }

    pub(super) fn find(&self, vcx: &mut gpui::VisualTestContext, event: FindEvent) {
        vcx.update(|window, cx| self.content.find(event, window, cx));
    }

    pub(super) fn command(
        &self,
        vcx: &mut gpui::VisualTestContext,
        line: &str,
    ) -> Result<(), String> {
        vcx.update(|window, cx| self.content.command(line, window, cx))
    }

    /// The notices the header reports, dismissed ones included.
    pub(super) fn notices(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.notice_texts())
    }

    pub(super) fn title(&self, vcx: &gpui::VisualTestContext) -> String {
        vcx.read(|cx| self.content.title(cx).to_string())
    }

    pub(super) fn view(&self, vcx: &gpui::VisualTestContext) -> View {
        self.tile.read_with(vcx, |t, _| t.state.view)
    }

    pub(super) fn lookback(&self, vcx: &gpui::VisualTestContext) -> Lookback {
        self.tile.read_with(vcx, |t, _| t.state.lookback)
    }

    pub(super) fn cursor(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.grid.cursor_name().map(str::to_string))
    }

    /// The shown rows' names, in painted order.
    pub(super) fn shown(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.shown_names())
    }

    pub(super) fn band_open(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile.read_with(vcx, |t, _| t.state.band)
    }
}
