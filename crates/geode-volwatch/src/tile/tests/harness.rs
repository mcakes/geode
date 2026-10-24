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
use geode_core::config::{Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::query::{HistoryKeysOutcome, HistoryKeysParams, QueryKey, ReferenceTable};
use geode_core::reference::ReferenceData;
use geode_core::scopes::SavedScopes;
use geode_core::volboard::{
    Cell, Change, Role, Stats, VolBoard, VolBoardOutcome, VolBoardParams, VolRow,
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
use geode_shell::reference::ReferenceGlobal;
use geode_shell::tiling::{TileId, WorkspaceIx};
use geode_shell::watchlist::WatchlistGlobal;
use gpui::{AppContext as _, Entity, Window};

use crate::content::{VolwatchFactory, VolwatchSettings};
use crate::core::lens::LensState;
use crate::core::views::Ask;
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
/// through the tile's `TileContent` door, as the shell does.
struct ShellStandIn {
    focus: gpui::FocusHandle,
    tile: Entity<VolwatchTile>,
    content: Rc<dyn TileContent>,
    keymap: Rc<Keymap>,
    matcher: Matcher,
    /// Every action id the keymap matched, in order, whether or not the
    /// tile carried it out.
    dispatched: Rc<RefCell<Vec<String>>>,
}

impl gpui::Render for ShellStandIn {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
        gpui::div()
            .size_full()
            // The shell root's context, under which the app reclaims tab
            // from gpui-component's Root focus cycling.
            .key_context("GeodeShell")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let context = this.content.key_context(cx);
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
                    this.dispatched.borrow_mut().push(action.0.clone());
                    this.content.dispatch(&action, count, window, cx);
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
    dispatched: Rc<RefCell<Vec<String>>>,
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
    cx.update(geode_shell::shell::dialog::init_reclaimed_keybindings);
    cx.update(|cx| cx.set_global(WatchlistGlobal(Arc::new(snap))));
    // Displayed dates and times read the app's clock; UTC keeps them fixed.
    cx.update(|cx| cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::utc())));
    let (data, rx) = DataHandle::for_tests();
    let factory = Rc::new(VolwatchFactory::new(data.clone(), settings));
    let keymap = Rc::new(app_keymap(&factory));
    cx.update(|cx| {
        cx.set_global(geode_shell::tips::Chords(Arc::new(
            keymap.bindings().to_vec(),
        )))
    });
    let slot: Rc<RefCell<Option<Built>>> = Rc::new(RefCell::new(None));
    let dispatched: Rc<RefCell<Vec<String>>> = Rc::default();
    let window = cx
        .update(|cx| {
            let slot = slot.clone();
            let factory = factory.clone();
            let dispatched = dispatched.clone();
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
                    content: content.clone(),
                    keymap: keymap.clone(),
                    matcher: Matcher::default(),
                    dispatched,
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
            dispatched,
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
/// 91st-percentile rank of 251 with a mean of 14.5 and an sd of 1.2 when
/// a window was asked, 30 points per sparkline; a member outside the
/// surface fixture is `missing`, its row blank.
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
                stats: (present && asked.window.is_some()).then_some(Stats {
                    mean: 14.5,
                    sd: 1.2,
                    count: 251,
                }),
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

/// The session record showing [`LIST`] in `view`.
pub(super) fn restored_on(view: &str) -> toml::Table {
    crate::core::session::to_table(&crate::core::session::State {
        list: Some(LIST.into()),
        view: Some(view.into()),
        ..Default::default()
    })
}

/// A tile restored on [`LIST`] in `view`, its first board asked and not
/// answered.
pub(super) fn open_on(
    cx: &mut gpui::TestAppContext,
    view: &str,
) -> (Harness, gpui::VisualTestContext) {
    open_with(
        cx,
        one_list(),
        Some(restored_on(view)),
        VolwatchSettings::default(),
    )
}

/// Blank every cell of `board` that reads `role`, as a failed dataset
/// leaves them.
pub(super) fn blank_role(board: &mut VolBoard, role: Role) {
    for row in &mut board.rows {
        for c in &mut row.cells {
            if c.measure.roles().contains(&role) {
                c.current = None;
                c.current_at = None;
                c.changes.iter_mut().for_each(|ch| *ch = None);
                c.pctl = None;
                c.stats = None;
            }
        }
    }
}

/// The header's prepared chip texts in paint order.
pub(super) fn chip_texts(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
    h.tile.read_with(vcx, |t, _| t.header.chip_texts())
}

pub(super) fn tag_of(h: &Harness, vcx: &gpui::VisualTestContext) -> u64 {
    h.tag_of(vcx)
}

pub(super) fn asked_of(h: &Harness, vcx: &gpui::VisualTestContext) -> Ask {
    h.asked_of(vcx)
}

pub(super) fn column_index(h: &Harness, vcx: &gpui::VisualTestContext, id: &str) -> usize {
    h.column_index(vcx, id)
}

/// A tile on [`LIST`] (`DAX, SX5E, NEW`) in the `vol` view whose first
/// board has landed, with `edit` applied to that board first; the
/// requests are drained.
pub(super) fn landed_board(
    cx: &mut gpui::TestAppContext,
    edit: impl FnOnce(&mut VolBoard),
) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_on(cx, "vol");
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.asked_of(&vcx);
    let mut board = board_for(&["DAX", "SX5E", "NEW"], &asked);
    edit(&mut board);
    h.answer_board(&mut vcx, tag, board);
    h.requests();
    (h, vcx)
}

/// [`landed_board`] with the fixture board as it is.
pub(super) fn landed(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    landed_board(cx, |_| {})
}

/// [`landed`] with an `underlyings` reference table naming `names`.
pub(super) fn landed_with_reference(
    cx: &mut gpui::TestAppContext,
    names: &[(&str, &str)],
) -> (Harness, gpui::VisualTestContext) {
    let table = ReferenceTable {
        columns: vec!["underlying_ref".into(), "name".into()],
        rows: names
            .iter()
            .map(|(k, n)| vec![Some(k.to_string()), Some(n.to_string())])
            .collect(),
        gen_id: 1,
        source_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
    };
    let data = ReferenceData::default()
        .with_table("underlyings", &table, 1)
        .unwrap();
    cx.update(|cx| cx.set_global(ReferenceGlobal(Arc::new(data))));
    landed(cx)
}

/// `region` (Core = DAX, SX5E; Isles = UKX) and `sector` (Index = all
/// three) over the underlying; `desk` over `book`, which never applies.
pub(super) fn europe_dims() -> DerivedDimensions {
    dims_from(
        r#"
[region]
from = "underlying_ref"
[region.values]
Core = ["DAX", "SX5E"]
Isles = ["UKX"]

[sector]
from = "underlying_ref"
[sector.values]
Index = ["DAX", "SX5E", "UKX"]

[desk]
from = "book"
[desk.values]
Flow = ["b1"]
"#,
    )
}

pub(super) fn dims_from(toml: &str) -> DerivedDimensions {
    let doc = merge_docs(
        "dimensions",
        &[LayerDoc::builtin("dimensions", toml).unwrap()],
    );
    let (dims, diags) = DerivedDimensions::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    dims
}

/// [`landed`] with `record` as the tile's session record (the fixture
/// board answered as in `landed_board`).
pub(super) fn landed_with_record(
    cx: &mut gpui::TestAppContext,
    record: toml::Table,
) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_with(cx, one_list(), Some(record), VolwatchSettings::default());
    let tag = Harness::boards(&h.requests())[0].tag;
    let asked = h.asked_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    h.requests();
    (h, vcx)
}

#[allow(dead_code, reason = "the key, chrome and band tests drive the rest")]
impl Harness {
    /// Every action id the keymap matched so far, in order.
    pub(super) fn dispatched(&self, _vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.dispatched.borrow().clone()
    }

    /// Paint a frame, so debug bounds follow the state.
    pub(super) fn draw(&self, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// The open menu's action titles, in order.
    pub(super) fn menu_titles(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.menu_titles())
    }

    /// The latest board ask's tag.
    pub(super) fn tag_of(&self, vcx: &gpui::VisualTestContext) -> u64 {
        self.tile.read_with(vcx, |t, _| t.tag)
    }

    /// The latest board ask admitted.
    pub(super) fn asked_of(&self, vcx: &gpui::VisualTestContext) -> Ask {
        self.tile
            .read_with(vcx, |t, _| t.asked.clone().expect("a board was asked"))
    }

    /// The index of column `id` in the view's columns.
    pub(super) fn column_index(&self, vcx: &gpui::VisualTestContext, id: &str) -> usize {
        self.tile.read_with(vcx, |t, _| {
            t.columns
                .iter()
                .position(|c| c.id == id)
                .unwrap_or_else(|| panic!("no column {id}"))
        })
    }

    /// Column `id`'s width as the table paints it, in real px.
    pub(super) fn column_width(&self, vcx: &gpui::VisualTestContext, id: &str) -> f32 {
        let ix = self.column_index(vcx, id);
        self.tile.read_with(vcx, |t, cx| {
            f32::from(t.table.read(cx).delegate().width(ix))
        })
    }

    /// Press `l` or `h` until the cursor column is `id`.
    pub(super) fn move_to_column(&self, vcx: &mut gpui::VisualTestContext, id: &str) {
        let want = self.column_index(vcx, id);
        for _ in 0..64 {
            let at = self.tile.read_with(vcx, |t, _| t.cursor_col);
            if at == want {
                return;
            }
            self.press(vcx, if at < want { "l" } else { "h" });
        }
        panic!("the cursor never reached {id}");
    }

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

    /// The shown view's name; empty with no views.
    pub(super) fn view(&self, vcx: &gpui::VisualTestContext) -> String {
        self.tile.read_with(vcx, |t, _| {
            t.shown_view().map(|v| v.name).unwrap_or_default()
        })
    }

    pub(super) fn lens(&self, vcx: &gpui::VisualTestContext) -> LensState {
        self.tile.read_with(vcx, |t, _| t.state.lens)
    }

    /// The `:` line's completions at `cursor`, through the content door.
    pub(super) fn completions(
        &self,
        vcx: &gpui::VisualTestContext,
        line: &str,
        cursor: usize,
    ) -> Vec<String> {
        vcx.read(|cx| self.content.completions(line, cursor, cx))
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

    /// Push `dims` through the factory, as the bridge does on a reload.
    pub(super) fn set_dims(&self, vcx: &mut gpui::VisualTestContext, dims: DerivedDimensions) {
        vcx.update(|_, cx| self.factory.set_dims(dims, cx));
    }

    /// Make `chain` the workspace lane's ad hoc grouping, as the Grouping
    /// dialog does; an empty chain returns the lane to the view default.
    pub(super) fn set_frame_chain(&self, vcx: &mut gpui::VisualTestContext, chain: &[&str]) {
        let chain: Vec<String> = chain.iter().map(|s| s.to_string()).collect();
        self.frame.update(vcx, |f, cx| {
            let mut lane = f.view_mut(WorkspaceIx::FIRST);
            if chain.is_empty() {
                lane.forget_ad_hoc();
            } else {
                lane.set_ad_hoc(chain);
            }
            cx.notify();
        });
    }

    /// Store `chain` in grouping slot `slot` (the other slots kept).
    pub(super) fn set_slot(&self, vcx: &mut gpui::VisualTestContext, slot: u8, chain: &[&str]) {
        let chain: Vec<String> = chain.iter().map(|s| s.to_string()).collect();
        self.frame.update(vcx, |f, cx| {
            let mut slots = f.slots().clone();
            slots.set(slot, chain);
            f.replace_slots(slots);
            cx.notify();
        });
    }

    /// The header's grouping chip text.
    pub(super) fn grouping_chip(&self, vcx: &gpui::VisualTestContext) -> String {
        self.tile
            .read_with(vcx, |t, _| t.header.grouping.to_string())
    }
}

/// The vega queries among `reqs`.
pub(super) fn queries(reqs: &[Request]) -> Vec<&geode_data::QueryParams> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::Query(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// The settings that ask book vega from `wide`'s `vega01_usd`.
pub(super) fn vega_settings() -> VolwatchSettings {
    VolwatchSettings {
        vega: Some(crate::content::VegaSetting {
            view: "wide".into(),
            column: "vega01_usd".into(),
        }),
        ..Default::default()
    }
}

/// A grouped answer: one depth-1 row per `(name, value)` and a depth-0
/// total carrying a number no member has.
pub(super) fn vega_snapshot(rows: &[(&'static str, f64)]) -> geode_core::snapshot::Snapshot {
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    let meta = |name: &str, summable: bool| ColumnMeta {
        name: name.into(),
        attribution_by_depth: vec![Attribution::Additive; 2],
        scope_semantics: ScopeSemantics::Direct,
        summable,
        mixed_flag: None,
    };
    let mut names: Vec<Option<&'static str>> = rows.iter().map(|(n, _)| Some(*n)).collect();
    let mut values: Vec<Option<f64>> = rows.iter().map(|(_, v)| Some(*v)).collect();
    let mut depth = vec![1; rows.len()];
    // The depth-0 total carries the first member's label: a join that
    // read past depth 1 would overwrite that member's vega with the total.
    names.push(rows.first().map(|(n, _)| *n));
    values.push(Some(9_999_999.0));
    depth.push(0);
    Snapshot::for_tests(
        vec![
            (meta("underlying_ref", false), TestColumn::Str(names)),
            (meta("vega01_usd", true), TestColumn::F64(values)),
            (meta("row_depth", false), TestColumn::I32(depth)),
        ],
        1,
    )
}

/// A vega-configured tile on [`LIST`] whose board has landed; the vega
/// query is still out and not drained.
pub(super) fn landed_with_vega(
    cx: &mut gpui::TestAppContext,
) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_with(cx, one_list(), Some(restored(LIST)), vega_settings());
    let tag = h.tag_of(&vcx);
    let asked = h.asked_of(&vcx);
    h.answer_board(&mut vcx, tag, board_for(&["DAX", "SX5E", "NEW"], &asked));
    (h, vcx)
}
