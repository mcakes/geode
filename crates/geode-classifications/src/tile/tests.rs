use super::*;
use crate::content::{ClassificationsConfig, ClassificationsFactory};
use geode_core::config::{Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::query::{AsOf, DistinctOutcome, DistinctParams, QueryKey};
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;
use geode_core::textfile::{TextFileOp, TextFileOutcome, TextFileParams, TextFileResult};
use geode_data::DataHandle;
use geode_data::Request;
use geode_shell::actions::ActionRegistry;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameRef};
use geode_shell::keymap::{KeyContext, Keymap, MatchResult, Matcher, build_keymap};
use geode_shell::module::{Delivery, FindEvent};
use geode_shell::module::{ModuleFactory, ModuleRoster, TileContent};
use geode_shell::tiling::{TileId, WorkspaceIx};
use gpui::{Entity, Window};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;

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
        schema: Rc::new(schema()),
        ..ClassificationsConfig::default()
    }
}

/// The dataset the fixtures' classifications map: `underlying_ref` and
/// `book` are groupable text columns a classification may be made over
/// (the position grain's measure carries them); `delta` is not.
fn schema() -> geode_core::schema::SchemaSpec {
    let text = r#"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.delta]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
    let (schema, diags) = geode_core::schema::SchemaSpec::from_doc(&merge_docs(
        "datasets",
        &[LayerDoc::builtin("datasets", text).unwrap()],
    ));
    assert!(
        diags
            .iter()
            .all(|d| d.severity != geode_core::config::Severity::Error),
        "{diags:?}"
    );
    schema
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
    frame: Entity<Frame>,
}

struct Harness {
    tile: Entity<ClassificationsTile>,
    /// Driven through the trait: the shell's own door is what a key and a
    /// title read arrive through.
    content: Rc<dyn TileContent>,
    factory: Rc<ClassificationsFactory>,
    /// What the tile asked of the data tier: the test is the service.
    requests: Receiver<Request>,
    /// The tile's data handle, to fill its queue mid-test.
    data: DataHandle,
    /// The frame the tile queues its config writes on and hears back from.
    frame: Entity<Frame>,
    shell_focus: gpui::FocusHandle,
}

/// A tile built by the factory after `config` was pushed, with `restored`
/// as its record, hosted under the shell stand-in in a `Root`.
fn open_with(
    cx: &mut gpui::TestAppContext,
    config: ClassificationsConfig,
    restored: Option<toml::Table>,
) -> (Harness, gpui::VisualTestContext) {
    open_over(cx, config, restored, false)
}

/// [`open_with`] over a data tier that refuses `Busy` until the test
/// drains its queue (`busy`).
fn open_over(
    cx: &mut gpui::TestAppContext,
    config: ClassificationsConfig,
    restored: Option<toml::Table>,
    busy: bool,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::init);
    let (data, requests) = DataHandle::for_tests();
    if busy {
        data.fill_for_tests();
    }
    let factory = Rc::new(ClassificationsFactory::new(data.clone()));
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
                    FrameRef::for_tile(frame.clone(), WorkspaceIx::FIRST, TileId(TILE)),
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
    });
    (
        Harness {
            tile: built.tile,
            content: built.content,
            factory,
            requests,
            data,
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
    /// The values reads asked since the last call, in order. Other
    /// requests (the `Busy` filler's cancels) are not the tile's.
    fn distinct_requests(&self) -> Vec<DistinctParams> {
        self.requests
            .try_iter()
            .filter_map(|r| match r {
                Request::Distinct(p) => Some(p),
                _ => None,
            })
            .collect()
    }
    /// Answer through the shell's door, as the bridge routes it.
    fn deliver(
        &self,
        vcx: &mut gpui::VisualTestContext,
        tag: u64,
        column: &str,
        values: Result<Vec<(&str, u64)>, &str>,
    ) {
        let outcome = DistinctOutcome {
            key: QueryKey(TILE),
            tag,
            column: column.into(),
            values: values
                .map(|v| v.into_iter().map(|(s, n)| (s.to_string(), n)).collect())
                .map_err(str::to_string),
        };
        vcx.update(|window, cx| h_deliver(&self.content, outcome, window, cx));
    }
    fn shown(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.shown_sources())
    }
    fn targets(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.targets())
    }
    fn cursor(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.grid.cursor_source().map(str::to_string))
    }
    fn notices(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.notice_texts())
    }
    fn mode(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.key_context().get("mode").map(str::to_string))
    }
    fn find(&self, vcx: &mut gpui::VisualTestContext, event: FindEvent) {
        vcx.update(|window, cx| self.content.find(event, window, cx));
    }
    fn command(&self, vcx: &mut gpui::VisualTestContext, line: &str) -> Result<(), String> {
        vcx.update(|window, cx| self.content.command(line, window, cx))
    }
}

fn h_deliver(
    content: &Rc<dyn TileContent>,
    outcome: DistinctOutcome,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    content.deliver(Delivery::Distinct(outcome), window, cx);
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

/// The values read for `region` from the fixture's data: one value the map
/// does not hold (NKY) and the two it does.
const REGION_VALUES: [(&str, u64); 3] = [("DAX", 5), ("NKY", 7), ("SX5E", 3)];

/// A tile restored on `region` whose values read was answered.
fn region_with_values(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    let asked = h.distinct_requests();
    assert_eq!(asked.len(), 1);
    h.deliver(
        &mut vcx,
        asked[0].tag,
        "underlying_ref",
        Ok(REGION_VALUES.to_vec()),
    );
    (h, vcx)
}

#[gpui::test]
fn opening_a_classification_asks_for_its_source_values_by_tile_key(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), None);
    assert!(
        h.distinct_requests().is_empty(),
        "nothing shown, nothing asked"
    );
    vcx.simulate_keystrokes("j enter");
    let asked = h.distinct_requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let p = &asked[0];
    assert_eq!(p.key, QueryKey(TILE));
    assert_eq!(p.column, "underlying_ref", "the base column, not the name");
    assert_eq!(p.scope, Scope::default());
    assert_eq!(p.as_of, AsOf::Live);
    // Switching to another classification asks about its column.
    vcx.simulate_keystrokes("g c k enter");
    let asked2 = h.distinct_requests();
    assert_eq!(asked2.len(), 1, "{asked2:?}");
    assert_eq!(asked2[0].column, "book");
    assert!(asked2[0].tag > p.tag, "each read carries a newer tag");
    // A reload that leaves the shown classification as it was asks nothing.
    vcx.update(|_, cx| h.factory.set_config(config(TWO), cx));
    assert!(h.distinct_requests().is_empty());
}

/// A reload that moves the shown classification to another source column
/// asks again, and drops the values counted for the old one.
#[gpui::test]
fn a_reload_changing_the_source_column_asks_again(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = region_with_values(cx);
    assert_eq!(h.shown(&vcx).len(), 3);
    let moved = "[region]\nfrom = \"lhu\"\n[region.values]\nEurope = [\"SX5E\"]\n";
    vcx.update(|_, cx| h.factory.set_config(config(moved), cx));
    let asked = h.distinct_requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].column, "lhu");
    assert_eq!(h.shown(&vcx), ["SX5E"], "the map alone until lhu answers");
}

#[gpui::test]
fn delivered_values_fill_the_grid_with_unclassified_rows_first(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E"], "the map before the values");
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Ok(REGION_VALUES.to_vec()));
    assert_eq!(h.shown(&vcx), ["NKY", "DAX", "SX5E"]);
    let header = h.header(&vcx);
    assert!(
        header.ends_with("3 values \u{00b7} 1 unclassified \u{00b7} desk"),
        "{header}"
    );
    // Painted: the rows by source value, the unclassified count in the
    // header.
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("classifications-row-NKY").is_some());
    assert!(vcx.debug_bounds("classifications-unclassified-7").is_some());
    let painted = h.tile.read_with(&vcx, |t, cx| {
        let p = t.table.read(cx).delegate().prepared().clone();
        p.rows
            .iter()
            .map(|r| {
                (
                    r.source.to_string(),
                    r.label.as_ref().map(|l| l.to_string()),
                    r.count.to_string(),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        painted,
        [
            ("NKY".into(), None, "7".into()),
            ("DAX".into(), Some("Europe".into()), "5".into()),
            ("SX5E".into(), Some("Europe".into()), "3".into()),
        ]
    );
}

#[gpui::test]
fn a_stale_tag_is_ignored(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    let first = h.distinct_requests()[0].tag;
    vcx.simulate_keystrokes("shift-r");
    let second = h.distinct_requests()[0].tag;
    assert!(second > first);
    h.deliver(
        &mut vcx,
        first,
        "underlying_ref",
        Ok(REGION_VALUES.to_vec()),
    );
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E"], "overtaken: dropped");
    // The current tag for another column is not this read's answer either.
    h.deliver(&mut vcx, second, "book", Ok(REGION_VALUES.to_vec()));
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E"]);
    h.deliver(
        &mut vcx,
        second,
        "underlying_ref",
        Ok(REGION_VALUES.to_vec()),
    );
    assert_eq!(h.shown(&vcx), ["NKY", "DAX", "SX5E"]);
}

#[gpui::test]
fn a_refused_or_failed_read_shows_the_map_alone_with_a_notice_and_shift_r_retries(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_over(cx, config(TWO), restored("region"), true);
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E"], "the map alone");
    assert_eq!(
        h.notices(&vcx),
        ["values not loaded: the data service is busy \u{2014} shift+r retries"]
    );
    assert!(
        h.distinct_requests().is_empty(),
        "the refused read never queued"
    );
    vcx.simulate_keystrokes("shift-r");
    let asked = h.distinct_requests();
    assert_eq!(asked.len(), 1, "the retry was admitted");
    assert!(
        h.notices(&vcx).is_empty(),
        "an admitted read clears the notice"
    );
    // A read that fails says why and keeps the map on screen.
    h.deliver(
        &mut vcx,
        asked[0].tag,
        "underlying_ref",
        Err("no such column"),
    );
    assert_eq!(
        h.notices(&vcx),
        ["values not loaded: no such column \u{2014} shift+r retries"]
    );
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E"]);
    vcx.simulate_keystrokes("shift-r");
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Ok(REGION_VALUES.to_vec()));
    assert!(h.notices(&vcx).is_empty());
    assert_eq!(h.shown(&vcx).len(), 3);
}

#[gpui::test]
fn slash_filters_the_rows_and_escape_restores(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = region_with_values(cx);
    h.find(&mut vcx, FindEvent::Changed("nk".into()));
    assert_eq!(h.shown(&vcx), ["NKY"]);
    h.find(&mut vcx, FindEvent::Cancelled);
    assert_eq!(h.shown(&vcx), ["NKY", "DAX", "SX5E"], "escape restores");
    // The label column is searched too.
    h.find(&mut vcx, FindEvent::Changed("eur".into()));
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E"]);
    h.find(&mut vcx, FindEvent::Committed("eur".into()));
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E"], "enter keeps it");
    // A later cancelled search restores the committed filter, not none.
    h.find(&mut vcx, FindEvent::Changed("dax".into()));
    assert_eq!(h.shown(&vcx), ["DAX"]);
    h.find(&mut vcx, FindEvent::Cancelled);
    assert_eq!(h.shown(&vcx), ["DAX", "SX5E"]);
    // Painted highlights follow the filter.
    let marks = h.tile.read_with(&vcx, |t, cx| {
        t.table.read(cx).delegate().prepared().rows[0]
            .label_marks
            .clone()
    });
    assert_eq!(marks, [std::ops::Range { start: 0, end: 3 }]);
}

#[gpui::test]
fn j_moves_the_cursor_and_v_selects_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = region_with_values(cx);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NKY"));
    vcx.simulate_keystrokes("j");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    vcx.simulate_keystrokes("k shift-v j");
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    assert_eq!(h.targets(&vcx), ["NKY", "DAX"]);
    vcx.simulate_keystrokes("escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert_eq!(h.targets(&vcx), ["DAX"]);
    // Counted keys: `2 j` steps two rows, `5 k` clamps at the top rather
    // than wrap.
    vcx.simulate_keystrokes("k 2 j");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SX5E"));
    vcx.simulate_keystrokes("5 k");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NKY"));
    // The cursor is what the session saves.
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("NKY"));
}

#[gpui::test]
fn sort_command_orders_by_rows_and_bare_sort_restores(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = region_with_values(cx);
    h.command(&mut vcx, "sort rows desc").unwrap();
    assert_eq!(h.shown(&vcx), ["NKY", "DAX", "SX5E"]);
    h.command(&mut vcx, "sort rows").unwrap();
    assert_eq!(
        h.shown(&vcx),
        ["SX5E", "DAX", "NKY"],
        "a bare column is asc"
    );
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    let (state, _) = crate::core::session::from_table(&saved);
    assert_eq!(
        state.sort,
        Some((SortCol::Rows, false)),
        "the session saves it"
    );
    h.command(&mut vcx, "sort source desc").unwrap();
    assert_eq!(h.shown(&vcx), ["SX5E", "NKY", "DAX"]);
    h.command(&mut vcx, "sort").unwrap();
    assert_eq!(h.shown(&vcx), ["NKY", "DAX", "SX5E"], "the default order");
    for bad in [
        "sort colour",
        "sort rows up",
        "sort rows asc extra",
        "grep x",
    ] {
        assert!(h.command(&mut vcx, bad).is_err(), "{bad}");
    }
    let complete = |line: &str, vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| h.content.completions(line, line.len(), cx))
    };
    assert_eq!(complete("so", &mut vcx), ["sort"]);
    assert_eq!(complete("sort ", &mut vcx), ["source", "label", "rows"]);
    assert_eq!(complete("sort rows d", &mut vcx), ["asc", "desc"]);
}

/// The sort icon carries no debug selector to press headless, so the press
/// is driven through the delegate's `perform_sort` hook, which the icon's
/// click calls.
#[gpui::test]
fn a_header_click_cycles_the_sort(cx: &mut gpui::TestAppContext) {
    use gpui_component::table::{ColumnSort, TableDelegate as _};
    let (h, mut vcx) = region_with_values(cx);
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
    // The pricer's header cycle: desc → asc → the default order.
    click(2, &mut vcx);
    assert_eq!(sort(&vcx), Some((SortCol::Rows, true)));
    assert_eq!(h.shown(&vcx), ["NKY", "DAX", "SX5E"]);
    click(2, &mut vcx);
    assert_eq!(sort(&vcx), Some((SortCol::Rows, false)));
    assert_eq!(h.shown(&vcx), ["SX5E", "DAX", "NKY"]);
    click(2, &mut vcx);
    assert_eq!(sort(&vcx), None);
    assert_eq!(h.shown(&vcx), ["NKY", "DAX", "SX5E"]);
    // Another column starts its own cycle at desc.
    click(2, &mut vcx);
    click(0, &mut vcx);
    assert_eq!(sort(&vcx), Some((SortCol::Source, true)));
    assert_eq!(h.shown(&vcx), ["SX5E", "NKY", "DAX"]);
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

/// A failed values read is not the answer a restored cursor waits for:
/// `R` brings its row and the cursor still lands there.
#[gpui::test]
fn a_failed_read_keeps_the_restored_cursor_waiting(cx: &mut gpui::TestAppContext) {
    let record = crate::core::session::to_table(&crate::core::session::State {
        name: Some("region".into()),
        cursor: Some("NKY".into()),
        ..Default::default()
    });
    let (h, mut vcx) = open_with(cx, config(TWO), Some(record));
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Err("timed out"));
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("NKY"), "still waiting");
    vcx.simulate_keystrokes("shift-r");
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Ok(REGION_VALUES.to_vec()));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NKY"));
}

/// A selection started on a resting cursor keeps its span when the values
/// arrive and reorder the rows beneath it.
#[gpui::test]
fn a_selection_made_before_the_values_keeps_its_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    vcx.simulate_keystrokes("shift-v");
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Ok(REGION_VALUES.to_vec()));
    assert_eq!(h.targets(&vcx), ["DAX"]);
}

/// A dragged column width survives what rebuilds the table: the drag is
/// recorded in the delegate (the table's `ColumnWidthsChanged`, emitted
/// as its resize handle does), and a filter keystroke or a values answer
/// does not refresh the columns at all. The table's own laid-out widths
/// are private, so the test reads what `column()` reports, which is what a
/// refresh lays out, and counts the tile's refreshes.
#[gpui::test]
fn a_dragged_column_width_survives_filter_values_and_sort(cx: &mut gpui::TestAppContext) {
    use gpui_component::table::TableDelegate as _;
    let (h, mut vcx) = region_with_values(cx);
    h.draw(&mut vcx);
    let table = h.tile.read_with(&vcx, |t, _| t.table.clone());
    let widths = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| {
            let d = table.read(cx).delegate();
            (0..3)
                .map(|c| f32::from(d.column(c, cx).width))
                .collect::<Vec<_>>()
        })
    };
    let before = widths(&mut vcx);
    let mut dragged: Vec<gpui::Pixels> = before.iter().map(|w| gpui::px(*w)).collect();
    dragged[1] = gpui::px(before[1] + 40.0);
    table.update(&mut vcx, |_, cx| {
        cx.emit(gpui_component::table::TableEvent::ColumnWidthsChanged(
            dragged,
        ))
    });
    vcx.run_until_parked();
    assert_eq!(widths(&mut vcx)[1], before[1] + 40.0);
    let refreshes = |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, _| t.refreshes);
    let at = refreshes(&vcx);
    h.find(&mut vcx, FindEvent::Changed("e".into()));
    h.find(&mut vcx, FindEvent::Cancelled);
    vcx.simulate_keystrokes("shift-r");
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Ok(REGION_VALUES.to_vec()));
    assert_eq!(refreshes(&vcx), at, "no refresh for a filter or values");
    // A sort does refresh (its header marks), and the drag survives it.
    h.command(&mut vcx, "sort rows").unwrap();
    assert_eq!(refreshes(&vcx), at + 1);
    let after = widths(&mut vcx);
    assert_eq!(after[1], before[1] + 40.0);
    assert_eq!(after[0], before[0], "an untouched column is not pinned");
}

#[gpui::test]
fn a_restored_cursor_lands_on_its_source_when_rows_arrive(cx: &mut gpui::TestAppContext) {
    let record = crate::core::session::to_table(&crate::core::session::State {
        name: Some("region".into()),
        sort: Some((SortCol::Rows, true)),
        cursor: Some("NKY".into()),
    });
    let (h, mut vcx) = open_with(cx, config(TWO), Some(record));
    // NKY is only in the data: the map's rows rest the cursor elsewhere,
    // and the session still names NKY while it waits.
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("NKY"));
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Ok(REGION_VALUES.to_vec()));
    assert_eq!(h.shown(&vcx), ["NKY", "DAX", "SX5E"], "the restored sort");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NKY"));
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.grid.cursor()), Some(0));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t.table.read(cx).selected_row()),
        Some(0),
        "the table paints it there"
    );
}

#[gpui::test]
fn a_restored_cursor_on_a_mapped_value_survives_the_values(cx: &mut gpui::TestAppContext) {
    let record = crate::core::session::to_table(&crate::core::session::State {
        name: Some("region".into()),
        cursor: Some("SX5E".into()),
        ..Default::default()
    });
    let (h, mut vcx) = open_with(cx, config(TWO), Some(record));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SX5E"));
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Ok(REGION_VALUES.to_vec()));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SX5E"));
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.grid.cursor()), Some(2));
}

#[gpui::test]
fn a_row_click_moves_the_cursor(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = region_with_values(cx);
    h.draw(&mut vcx);
    let sx5e = vcx
        .debug_bounds("classifications-row-SX5E")
        .expect("the row is painted");
    vcx.simulate_click(sx5e.center(), gpui::Modifiers::none());
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SX5E"));
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, cx| t.table.read(cx).selected_row()),
        Some(2),
        "the table paints the cursor row"
    );
    // Shift-click extends a row selection from the cursor.
    h.draw(&mut vcx);
    let nky = vcx.debug_bounds("classifications-row-NKY").unwrap();
    vcx.simulate_click(nky.center(), gpui::Modifiers::shift());
    assert_eq!(h.targets(&vcx), ["NKY", "DAX", "SX5E"]);
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    // A plain click ends it.
    h.draw(&mut vcx);
    let dax = vcx.debug_bounds("classifications-row-DAX").unwrap();
    vcx.simulate_click(dax.center(), gpui::Modifiers::none());
    assert_eq!(h.targets(&vcx), ["DAX"]);
}

/// A label that is blank after trimming is unclassified everywhere: the
/// header counts it, and it paints as the unclassified mark.
#[gpui::test]
fn a_blank_label_counts_and_paints_as_unclassified(cx: &mut gpui::TestAppContext) {
    let blank = "[region]\nfrom = \"underlying_ref\"\n[region.values]\n\"  \" = [\"DAX\"]\n";
    let (h, vcx) = open_with(cx, config(blank), restored("region"));
    let header = h.header(&vcx);
    assert!(
        header.contains("1 value") && header.contains("1 unclassified"),
        "{header}"
    );
    let label = h.tile.read_with(&vcx, |t, cx| {
        t.table.read(cx).delegate().prepared().rows[0].label.clone()
    });
    assert_eq!(label, None);
}

// ---- editing labels ----

/// `region` with two labels in use.
const EDIT: &str = r#"
[region]
from = "underlying_ref"
[region.values]
Americas = ["SPX"]
Europe = ["SX5E", "DAX"]
"#;

/// Two unclassified values (NKY, HSI) beside the mapped three. The
/// default order: NKY, HSI, SPX (Americas), DAX, SX5E (Europe).
const EDIT_VALUES: [(&str, u64); 5] = [("DAX", 5), ("HSI", 2), ("NKY", 7), ("SPX", 9), ("SX5E", 3)];

/// A tile restored on `dims`'s `name` whose values read answered `values`.
fn shown_with(
    cx: &mut gpui::TestAppContext,
    dims: &str,
    name: &str,
    values: &[(&str, u64)],
) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open_with(cx, config(dims), restored(name));
    let asked = h.distinct_requests();
    let column = asked[0].column.clone();
    h.deliver(&mut vcx, asked[0].tag, &column, Ok(values.to_vec()));
    (h, vcx)
}

fn editing(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, vcx) = shown_with(cx, EDIT, "region", &EDIT_VALUES);
    assert_eq!(h.shown(&vcx), ["NKY", "HSI", "SPX", "DAX", "SX5E"]);
    (h, vcx)
}

/// `region` as an object, for comparing what was written.
fn region(pairs: &[(&str, &str)]) -> DerivedDimension {
    DerivedDimension {
        name: "region".into(),
        from: "underlying_ref".into(),
        values: pairs
            .iter()
            .map(|(s, l)| (s.to_string(), l.to_string()))
            .collect(),
    }
}

/// The edit the tile queues for `dim`: the whole object, from this tile.
fn edit_of(dim: &DerivedDimension) -> ConfigEdit {
    ConfigEdit {
        doc: DIMENSIONS_DOC,
        object: dim.name.clone(),
        value: Some(classification::to_toml(dim)),
        origin: Some(TileId(TILE)),
    }
}

impl Harness {
    /// Keys as the trader types them. A committed or cancelled editor
    /// blurs its field, and the shell then puts the keyboard back on its
    /// root; the stand-in has no such path, so the test does it.
    fn press(&self, vcx: &mut gpui::VisualTestContext, keys: &str) {
        vcx.update(|window, cx| {
            if window.focused(cx).is_none() {
                self.shell_focus.focus(window, cx);
            }
        });
        vcx.simulate_keystrokes(keys);
    }
    /// What reached the frame's config door since the last call: the
    /// shell's drain takes exactly this.
    fn edits(&self, vcx: &mut gpui::VisualTestContext) -> Vec<ConfigEdit> {
        self.frame.update(vcx, |f, _| f.take_pending_config_edits())
    }
    /// The label the grid paints for `source`.
    fn label(&self, vcx: &gpui::VisualTestContext, source: &str) -> Option<String> {
        self.tile.read_with(vcx, |t, cx| {
            let p = t.table.read(cx).delegate().prepared().clone();
            let row = p.rows.iter().find(|r| r.source == source);
            row.expect("the row is shown")
                .label
                .as_ref()
                .map(|l| l.to_string())
        })
    }
    fn editor(
        &self,
        vcx: &gpui::VisualTestContext,
    ) -> Option<(String, Vec<String>, Option<String>)> {
        self.tile.read_with(vcx, |t, cx| t.editor_state(cx))
    }
    /// Put the cursor on `source` with the grid's own motions.
    fn goto(&self, vcx: &mut gpui::VisualTestContext, source: &str) {
        let at = self
            .shown(vcx)
            .iter()
            .position(|s| s == source)
            .expect("the row is shown");
        self.press(vcx, &format!("g g{}", " j".repeat(at)));
        assert_eq!(self.cursor(vcx).as_deref(), Some(source));
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
}

#[gpui::test]
fn enter_opens_the_editor_prefilled_and_enter_writes_the_whole_object(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "enter");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    let (text, labels, lit) = h.editor(&vcx).expect("the editor is open");
    assert_eq!(text, "Americas");
    assert_eq!(labels, ["Americas", "Europe"]);
    assert_eq!(lit.as_deref(), Some("Americas"));
    assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    // Painted in the cursor row's label cell, its list hung under it.
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("classifications-editor-7").is_some());
    assert!(vcx.debug_bounds("classifications-editor-list-7").is_some());
    assert!(
        vcx.debug_bounds("classifications-editor-row-Europe")
            .is_some()
    );
    // The prefill is selected: typing replaces it.
    vcx.simulate_input("Europe");
    h.press(&mut vcx, "enter");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    let next = region(&[("SPX", "Europe"), ("SX5E", "Europe"), ("DAX", "Europe")]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&next)]);
    assert_eq!(
        h.label(&vcx, "SPX").as_deref(),
        Some("Europe"),
        "shown at once"
    );
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    // The reload carrying it keeps it; a reload without it (a revert
    // elsewhere) is what the grid then shows.
    let carried = "[region]\nfrom = \"underlying_ref\"\n[region.values]\nEurope = [\"SPX\", \"SX5E\", \"DAX\"]\n";
    vcx.update(|_, cx| h.factory.set_config(config(carried), cx));
    assert_eq!(h.label(&vcx, "SPX").as_deref(), Some("Europe"));
    vcx.update(|_, cx| h.factory.set_config(config(EDIT), cx));
    assert_eq!(h.label(&vcx, "SPX").as_deref(), Some("Americas"));
}

#[gpui::test]
fn typing_a_case_variant_of_an_existing_label_takes_the_existing_one_only_when_highlighted(
    cx: &mut gpui::TestAppContext,
) {
    let tech = "[sector]\nfrom = \"underlying_ref\"\n[sector.values]\nTech = [\"AAPL\"]\n";
    let (h, mut vcx) = shown_with(cx, tech, "sector", &[("AAPL", 1), ("MSFT", 2)]);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("MSFT"));
    h.press(&mut vcx, "enter");
    vcx.simulate_input("tech");
    h.press(&mut vcx, "enter");
    let edits = h.edits(&mut vcx);
    assert_eq!(edits.len(), 1);
    let written = edits[0].value.as_ref().unwrap().to_string();
    assert!(written.contains("Tech = [\"AAPL\", \"MSFT\"]"), "{written}");
    // Typed past every label: written as typed, never re-cased.
    h.goto(&mut vcx, "AAPL");
    h.press(&mut vcx, "enter");
    vcx.simulate_input("techx");
    h.press(&mut vcx, "enter");
    let edits = h.edits(&mut vcx);
    let written = edits[0].value.as_ref().unwrap().to_string();
    assert!(written.contains("techx = [\"AAPL\"]"), "{written}");
    assert_eq!(h.label(&vcx, "AAPL").as_deref(), Some("techx"));
}

#[gpui::test]
fn an_empty_entry_clears_and_escape_cancels(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "enter backspace enter");
    assert_eq!(
        h.edits(&mut vcx),
        [edit_of(&region(&[("SX5E", "Europe"), ("DAX", "Europe")]))]
    );
    assert_eq!(h.label(&vcx, "SPX"), None);
    // Escape closes with nothing written, whatever was typed.
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "enter");
    vcx.simulate_input("Asia");
    h.press(&mut vcx, "escape");
    assert_eq!(h.editor(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.label(&vcx, "DAX").as_deref(), Some("Europe"));
}

#[gpui::test]
fn editing_a_selection_prefills_only_a_unanimous_label(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "shift-v j");
    assert_eq!(h.targets(&vcx), ["SPX", "DAX"]);
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.editor(&vcx).unwrap().0,
        "",
        "Americas and Europe: no prefill"
    );
    h.press(&mut vcx, "escape");
    // Escape left the editor, not the selection.
    assert_eq!(h.targets(&vcx), ["SPX", "DAX"]);
    h.press(&mut vcx, "j");
    assert_eq!(h.targets(&vcx), ["SPX", "DAX", "SX5E"]);
    h.press(&mut vcx, "k k");
    h.press(&mut vcx, "escape");
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "shift-v j enter");
    assert_eq!(h.editor(&vcx).unwrap().0, "Europe", "both Europe");
    h.press(&mut vcx, "escape");
    h.press(&mut vcx, "escape");
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "shift-v j enter");
    vcx.simulate_input("X");
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.edits(&mut vcx),
        [edit_of(&region(&[
            ("SPX", "X"),
            ("DAX", "X"),
            ("SX5E", "Europe")
        ]))],
        "both rows in one write"
    );
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"), "the verb ends it");
}

#[gpui::test]
fn x_clears_and_yy_p_copies_a_label(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    // Nothing copied yet: `p` says so and writes nothing.
    h.press(&mut vcx, "p");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.notices(&vcx), ["nothing copied: y y copies a label"]);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "y y");
    h.goto(&mut vcx, "NKY");
    h.press(&mut vcx, "p");
    assert_eq!(h.notices(&vcx), Vec::<String>::new(), "a verb clears it");
    let pasted = region(&[
        ("SPX", "Americas"),
        ("NKY", "Americas"),
        ("SX5E", "Europe"),
        ("DAX", "Europe"),
    ]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&pasted)]);
    assert_eq!(h.label(&vcx, "NKY").as_deref(), Some("Americas"));
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "x");
    let cleared = region(&[("SPX", "Americas"), ("NKY", "Americas"), ("SX5E", "Europe")]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&cleared)]);
    assert_eq!(h.label(&vcx, "DAX"), None);
    // An unclassified row copies as unclassified: pasting it clears.
    h.goto(&mut vcx, "HSI");
    h.press(&mut vcx, "y y");
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "p");
    assert_eq!(h.label(&vcx, "SPX"), None);
}

#[gpui::test]
fn u_and_ctrl_r_undo_and_redo_through_the_door(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    let cleared = region(&[("SX5E", "Europe"), ("DAX", "Europe")]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&cleared)]);
    h.press(&mut vcx, "u");
    let original = region(&[("SPX", "Americas"), ("SX5E", "Europe"), ("DAX", "Europe")]);
    assert_eq!(h.edits(&mut vcx), [edit_of(&original)]);
    assert_eq!(h.label(&vcx, "SPX").as_deref(), Some("Americas"));
    h.press(&mut vcx, "ctrl-r");
    assert_eq!(h.edits(&mut vcx), [edit_of(&cleared)]);
    assert_eq!(h.label(&vcx, "SPX"), None);
    h.press(&mut vcx, "ctrl-r");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.notices(&vcx), ["nothing to redo"]);
}

/// Undo replays over the configuration as it is now: a row another
/// surface changed since is left alone, and the notice says how many.
#[gpui::test]
fn undo_skips_a_row_changed_elsewhere_and_says_so(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    h.edits(&mut vcx);
    // Another tile labelled SPX since.
    let foreign = "[region]\nfrom = \"underlying_ref\"\n[region.values]\nAsia = [\"SPX\"]\nEurope = [\"SX5E\", \"DAX\"]\n";
    vcx.update(|_, cx| h.factory.set_config(config(foreign), cx));
    h.press(&mut vcx, "u");
    assert!(h.edits(&mut vcx).is_empty(), "nothing left to change");
    assert_eq!(
        h.notices(&vcx),
        ["1 row changed elsewhere was left as it is"]
    );
    assert_eq!(h.label(&vcx, "SPX").as_deref(), Some("Asia"));
}

/// The ruling the label loop rests on: after a verb the cursor keeps its
/// shown index, so labelling the top unclassified row leaves the cursor on
/// the next one, even after the trader moved the cursor about.
#[gpui::test]
fn labelling_the_top_unclassified_row_leaves_the_cursor_on_the_next(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.press(&mut vcx, "j k");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NKY"));
    h.press(&mut vcx, "enter");
    vcx.simulate_input("Asia");
    h.press(&mut vcx, "enter");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.grid.cursor()), Some(0));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("HSI"));
    // And again: the next unclassified row is labelled from the same place.
    h.press(&mut vcx, "enter");
    vcx.simulate_input("Asia");
    h.press(&mut vcx, "enter");
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.grid.cursor()), Some(0));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SPX"));
}

/// `down` moves the highlight, and a moved highlight is what enter takes,
/// whatever was typed; a row press picks its label at once.
#[gpui::test]
fn a_moved_highlight_or_a_row_press_picks_the_label(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NKY"));
    h.press(&mut vcx, "enter");
    assert_eq!(h.editor(&vcx).unwrap().2.as_deref(), Some("Americas"));
    h.press(&mut vcx, "down");
    assert_eq!(h.editor(&vcx).unwrap().2.as_deref(), Some("Europe"));
    h.press(&mut vcx, "enter");
    assert_eq!(h.label(&vcx, "NKY").as_deref(), Some("Europe"));
    h.edits(&mut vcx);
    // The pointer route: the row press writes its label.
    h.goto(&mut vcx, "HSI");
    h.press(&mut vcx, "enter");
    h.draw(&mut vcx);
    let americas = vcx
        .debug_bounds("classifications-editor-row-Americas")
        .expect("the list is painted");
    vcx.simulate_mouse_down(
        americas.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(h.editor(&vcx), None);
    assert_eq!(h.label(&vcx, "HSI").as_deref(), Some("Americas"));
    assert_eq!(h.edits(&mut vcx).len(), 1);
}

#[gpui::test]
fn a_fork_notice_from_the_shell_shows_once(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.press(&mut vcx, "x");
    h.edits(&mut vcx);
    let text = "copied 'region' to your config — Revert… restores the builtin copy";
    h.shell_says(&mut vcx, TileNotice::Forked(text.into()));
    assert_eq!(h.notices(&vcx), [text]);
    // A later frame notification brings nothing new.
    h.frame.update(&mut vcx, |_, cx| cx.notify());
    vcx.run_until_parked();
    assert_eq!(h.notices(&vcx), [text]);
    // A notice for another tile is not this tile's.
    h.frame.update(&mut vcx, |f, cx| {
        f.post_tile_notice_for_test(TileId(TILE + 1), TileNotice::Forked("other".into()));
        cx.notify();
    });
    vcx.run_until_parked();
    assert_eq!(h.notices(&vcx), [text]);
}

#[gpui::test]
fn a_refusal_notice_drops_the_optimistic_edit(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    assert_eq!(h.label(&vcx, "SPX"), None, "optimistic");
    let why = "dimensions not written: the user layer is read-only";
    h.shell_says(&mut vcx, TileNotice::Refused(why.into()));
    assert_eq!(h.notices(&vcx), [why]);
    assert_eq!(
        h.label(&vcx, "SPX").as_deref(),
        Some("Americas"),
        "back to the configuration's"
    );
}

#[gpui::test]
fn an_invalid_source_classification_is_never_written(cx: &mut gpui::TestAppContext) {
    // Hand-written over a measure: no classification may map it.
    let bad = "[bad]\nfrom = \"delta\"\n[bad.values]\nX = [\"1\"]\n";
    let (h, mut vcx) = open_with(cx, config(bad), restored("bad"));
    h.press(&mut vcx, "x");
    assert!(h.edits(&mut vcx).is_empty());
    let notices = h.notices(&vcx);
    assert!(
        notices
            .iter()
            .any(|n| n == "not saved: 'delta' is not a groupable text column"),
        "{notices:?}"
    );
    assert_eq!(h.label(&vcx, "1").as_deref(), Some("X"));
    h.press(&mut vcx, "enter");
    vcx.simulate_input("Y");
    h.press(&mut vcx, "enter");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.label(&vcx, "1").as_deref(), Some("X"));
}

// ---- notice and switcher lifecycle ----

/// A restore notice is read once: the trader's first action clears it, and
/// a reload before then does not.
#[gpui::test]
fn a_restore_notice_clears_on_the_first_action(cx: &mut gpui::TestAppContext) {
    let mut record = restored("region").unwrap();
    record.insert("sort".into(), toml::Value::Integer(3));
    let (h, mut vcx) = open_with(cx, config(TWO), Some(record));
    let restore = |vcx: &gpui::VisualTestContext| {
        h.notices(vcx)
            .iter()
            .filter(|n| n.starts_with("session: dropped sort"))
            .count()
    };
    assert_eq!(restore(&vcx), 1);
    vcx.update(|_, cx| h.factory.set_config(config(TWO), cx));
    assert_eq!(restore(&vcx), 1, "a reload is not the trader acting");
    h.press(&mut vcx, "j");
    assert_eq!(restore(&vcx), 0);
}

/// The switcher's refusal stands only while there is nothing to switch to.
#[gpui::test]
fn the_nothing_to_switch_to_refusal_clears_once_there_is(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(""), None);
    h.press(&mut vcx, "g c");
    assert_eq!(h.notices(&vcx), ["no classifications to switch to"]);
    vcx.update(|_, cx| h.factory.set_config(config(""), cx));
    assert_eq!(h.notices(&vcx), ["no classifications to switch to"]);
    vcx.update(|_, cx| h.factory.set_config(config(TWO), cx));
    assert!(h.notices(&vcx).is_empty());
    assert_eq!(h.switcher(&vcx), None, "nothing shown went away");
}

/// The switcher opens on construction and when the shown classification
/// goes away, not on a reload that finds the tile still showing nothing
/// after the trader closed it.
#[gpui::test]
fn a_closed_switcher_stays_closed_across_an_unrelated_reload(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), None);
    assert!(h.switcher(&vcx).is_some());
    h.press(&mut vcx, "escape");
    assert_eq!(h.switcher(&vcx), None);
    vcx.update(|_, cx| h.factory.set_config(config(TWO), cx));
    assert_eq!(h.switcher(&vcx), None);
    // Shown, then gone: it opens.
    h.press(&mut vcx, "g c j enter");
    assert_eq!(h.title(&mut vcx), "Classification: region");
    vcx.update(|_, cx| {
        h.factory
            .set_config(config("[desk]\nfrom = \"book\"\n"), cx)
    });
    assert_eq!(h.switcher(&vcx), rows(&["desk"]));
}

// ---- no-op verbs and reloads inside the debounce ----

/// A verb that changes nothing (`x` on rows already unclassified) is not a
/// relabel: the live selection stands.
#[gpui::test]
fn a_verb_that_changes_nothing_keeps_the_selection(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.press(&mut vcx, "shift-v j");
    assert_eq!(h.targets(&vcx), ["NKY", "HSI"]);
    h.press(&mut vcx, "x");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.mode(&vcx).as_deref(), Some("visual"));
    assert_eq!(h.targets(&vcx), ["NKY", "HSI"]);
}

/// `u` with nothing to undo, and a replay that skips every row, change
/// nothing either: a restored cursor still waiting for its row keeps
/// waiting.
#[gpui::test]
fn an_undo_that_changes_nothing_keeps_a_waiting_cursor(cx: &mut gpui::TestAppContext) {
    let record = crate::core::session::to_table(&crate::core::session::State {
        name: Some("region".into()),
        cursor: Some("NKY".into()),
        ..Default::default()
    });
    let (h, mut vcx) = open_with(cx, config(EDIT), Some(record));
    // Only the map's rows: the cursor rests on the first, SPX, while NKY
    // waits for the values.
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SPX"));
    h.press(&mut vcx, "u");
    assert_eq!(h.notices(&vcx), ["nothing to undo"]);
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("NKY"), "still waiting");
    h.press(&mut vcx, "x");
    h.edits(&mut vcx);
    // Another tile labelled SPX since; undo has nothing left to do.
    let foreign = "[region]\nfrom = \"underlying_ref\"\n[region.values]\nAsia = [\"SPX\"]\nEurope = [\"SX5E\", \"DAX\"]\n";
    vcx.update(|_, cx| h.factory.set_config(config(foreign), cx));
    // A cursor waiting for a row the values have not brought yet, as a
    // restore leaves it.
    h.tile
        .update(&mut vcx, |t, _| t.grid.seed_cursor("NKY".into()));
    h.press(&mut vcx, "u");
    assert!(h.edits(&mut vcx).is_empty());
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["cursor"].as_str(), Some("NKY"), "still waiting");
}

/// A reload that changes something else inside the write's debounce does
/// not drop the optimistic edit: the label does not flash back, and the
/// next edit is built over the first.
#[gpui::test]
fn an_unrelated_reload_keeps_the_pending_edit_and_the_next_edit_builds_on_it(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    h.edits(&mut vcx);
    let unrelated = format!("{EDIT}\n[desk]\nfrom = \"book\"\n");
    vcx.update(|_, cx| h.factory.set_config(config(&unrelated), cx));
    assert_eq!(h.label(&vcx, "SPX"), None, "no flash back");
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "x");
    assert_eq!(
        h.edits(&mut vcx),
        [edit_of(&region(&[("SX5E", "Europe")]))],
        "both edits in the queued object"
    );
}

// ---- new, rename, delete, revert ----

/// `dims` with `user` defined in the user layer, and `shadowed` of those
/// over a copy in the given lower layer.
fn config_layered(dims: &str, user: &[&str], shadowed: &[(&str, Layer)]) -> ClassificationsConfig {
    let mut c = config(dims);
    for name in user {
        c.layers.insert(name.to_string(), Layer::User);
    }
    c.shadowed = shadowed.iter().map(|(n, l)| (n.to_string(), *l)).collect();
    c
}

/// The prompt field as the tile holds it.
#[derive(Debug, PartialEq)]
struct PromptSeen {
    prompt: crate::core::prompt::Prompt,
    text: String,
    placeholder: String,
    error: Option<String>,
    /// The closed choice's ranked options, for the column step.
    options: Option<Vec<String>>,
}

impl Harness {
    /// A registered action, the way the palette and the `⋯` menu reach the
    /// tile: through its door.
    fn act(&self, vcx: &mut gpui::VisualTestContext, id: &str) {
        let id = ActionId(id.to_string());
        vcx.update(|window, cx| {
            self.content.dispatch(&id, None, window, cx);
        });
        self.draw(vcx);
    }
    fn prompt(&self, vcx: &gpui::VisualTestContext) -> Option<PromptSeen> {
        self.tile.read_with(vcx, |t, cx| {
            let p = t.prompt.as_ref()?;
            let input = p.input.read(cx);
            Some(PromptSeen {
                prompt: p.prompt.clone(),
                text: input.value().to_string(),
                placeholder: input.presentation().placeholder().to_string(),
                error: p.error.as_ref().map(|e| e.to_string()),
                options: p.list.as_ref().map(|l| {
                    l.ranked()
                        .iter()
                        .map(|r| l.options()[r.row].clone())
                        .collect()
                }),
            })
        })
    }
    fn confirm(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| {
            t.confirm.as_ref().map(|c| c.prompt_text().to_string())
        })
    }
    /// The open `⋯` menu's rows: each title with its disabled lane text,
    /// `---` for a separator.
    fn action_menu(&self, vcx: &gpui::VisualTestContext) -> Option<Vec<(String, Option<String>)>> {
        self.tile.read_with(vcx, |t, _| {
            let (MenuKind::Actions, m) = t.menu.as_ref()? else {
                return None;
            };
            Some(
                m.rows()
                    .iter()
                    .map(|r| match r {
                        Row::Action(a) => (
                            a.title().to_string(),
                            match a.trailing() {
                                menu::Trailing::Text(t) if !a.is_enabled() => Some(t.to_string()),
                                _ => None,
                            },
                        ),
                        _ => ("---".to_string(), None),
                    })
                    .collect(),
            )
        })
    }
    fn shown_name(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.state.name.clone())
    }
}

fn removal(name: &str) -> ConfigEdit {
    ConfigEdit {
        doc: DIMENSIONS_DOC,
        object: name.into(),
        value: None,
        origin: Some(TileId(TILE)),
    }
}

#[gpui::test]
fn new_asks_a_name_then_a_column_and_creates_an_empty_classification(
    cx: &mut gpui::TestAppContext,
) {
    use crate::core::prompt::Prompt;
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    h.act(&mut vcx, "classifications::new");
    let seen = h.prompt(&vcx).expect("the prompt is open");
    assert_eq!(seen.prompt, Prompt::NewName);
    assert_eq!(seen.placeholder, "name");
    assert_eq!(seen.text, "");
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    assert!(vcx.update(|window, cx| h.content.holds_focus(window, cx)));
    assert!(vcx.debug_bounds("classifications-prompt-7").is_some());
    vcx.simulate_input("sector");
    h.press(&mut vcx, "enter");
    let seen = h.prompt(&vcx).expect("the column step");
    assert_eq!(
        seen.prompt,
        Prompt::NewColumn {
            name: "sector".into()
        }
    );
    assert_eq!(seen.text, "", "a fresh answer");
    assert_eq!(
        seen.options.as_deref(),
        Some(&["underlying_ref", "book", "position_ref", "instrument_ref"].map(String::from)[..]),
        "the columns a classification may map"
    );
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("classifications-prompt-row-book")
            .is_some()
    );
    vcx.simulate_input("under");
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    let sector = DerivedDimension {
        name: "sector".into(),
        from: "underlying_ref".into(),
        values: Default::default(),
    };
    assert_eq!(h.edits(&mut vcx), [edit_of(&sector)]);
    assert_eq!(h.shown_name(&vcx).as_deref(), Some("sector"));
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    // Until the reload carries it, the body says it is on its way.
    let empty = h.empty(&vcx).expect("not defined yet");
    assert!(empty.contains("sector"), "{empty}");
    assert!(!empty.contains("no longer exists"), "{empty}");
    let with_sector = format!("{TWO}\n[sector]\nfrom = \"underlying_ref\"\n");
    vcx.update(|_, cx| h.factory.set_config(config(&with_sector), cx));
    assert_eq!(h.title(&mut vcx), "Classification: sector");
}

#[gpui::test]
fn new_refuses_a_shadowing_name_and_keeps_the_field_open(cx: &mut gpui::TestAppContext) {
    use crate::core::prompt::Prompt;
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    h.act(&mut vcx, "classifications::new");
    vcx.simulate_input("book");
    h.press(&mut vcx, "enter");
    let seen = h.prompt(&vcx).expect("still open");
    assert_eq!(seen.prompt, Prompt::NewName);
    assert_eq!(
        seen.error.as_deref(),
        Some("'book' is already a dataset column ('book')")
    );
    assert!(h.edits(&mut vcx).is_empty());
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("classifications-prompt-error-7").is_some());
    // Escape closes it with nothing written.
    h.press(&mut vcx, "escape");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
}

#[gpui::test]
fn rename_confirms_with_the_reference_count_and_writes_one_batch(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(TWO, &["region"], &[]),
        restored("region"),
    );
    h.frame.update(&mut vcx, |f, _| {
        let mut slots = GroupingSlots::default();
        slots.set(2, vec!["region".into(), "underlying_ref".into()]);
        f.replace_slots(slots);
    });
    h.act(&mut vcx, "classifications::rename");
    let seen = h.prompt(&vcx).expect("the rename field");
    assert_eq!(seen.text, "region", "seeded, selected: typing replaces it");
    vcx.simulate_input("zone");
    h.press(&mut vcx, "enter");
    assert_eq!(h.prompt(&vcx), None);
    assert_eq!(
        h.confirm(&vcx).as_deref(),
        Some("rename region \u{2192} zone: 1 grouping still says 'region' \u{2014} y renames")
    );
    assert_eq!(h.mode(&vcx).as_deref(), Some("insert"));
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("classifications-confirm-7-bar").is_some());
    assert!(h.edits(&mut vcx).is_empty(), "nothing before y");
    vcx.simulate_keystrokes("y");
    let zone = DerivedDimension {
        name: "zone".into(),
        ..region(&[("SX5E", "Europe"), ("DAX", "Europe")])
    };
    assert_eq!(h.edits(&mut vcx), [edit_of(&zone), removal("region")]);
    assert_eq!(h.shown_name(&vcx).as_deref(), Some("zone"));
    assert_eq!(h.confirm(&vcx), None);
}

/// A rename the shell refuses never landed: the tile goes back to the old
/// name, which the switcher lists again.
#[gpui::test]
fn a_refused_rename_shows_the_old_name_again(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(TWO, &["region"], &[]),
        restored("region"),
    );
    h.act(&mut vcx, "classifications::rename");
    vcx.simulate_input("zone");
    h.press(&mut vcx, "enter");
    vcx.simulate_keystrokes("y");
    assert_eq!(h.shown_name(&vcx).as_deref(), Some("zone"));
    let why = "dimensions not written: the user layer is read-only";
    h.shell_says(&mut vcx, TileNotice::Refused(why.into()));
    assert_eq!(h.shown_name(&vcx).as_deref(), Some("region"));
    assert_eq!(h.title(&mut vcx), "Classification: region");
    assert_eq!(h.notices(&vcx), [why]);
}

#[gpui::test]
fn delete_confirms_and_removes_a_user_object(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(TWO, &["region"], &[]),
        restored("region"),
    );
    h.act(&mut vcx, "classifications::delete");
    assert_eq!(
        h.confirm(&vcx).as_deref(),
        Some("delete region: nothing refers to it \u{2014} y deletes")
    );
    vcx.simulate_keystrokes("y");
    assert_eq!(h.edits(&mut vcx), [removal("region")]);
    assert_eq!(h.shown_name(&vcx), None);
    assert_eq!(h.switcher(&vcx), rows(&["desk"]), "the deleted one is gone");
}

#[gpui::test]
fn delete_refuses_a_desk_object(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("desk"));
    h.act(&mut vcx, "classifications::delete");
    assert_eq!(h.confirm(&vcx), None);
    assert_eq!(
        h.notices(&vcx),
        ["desk is defined in desk config; Geode cannot remove it from there"]
    );
    assert!(h.edits(&mut vcx).is_empty());
}

#[gpui::test]
fn revert_is_offered_only_for_a_shadowed_user_copy_and_removes_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(TWO, &["region"], &[("region", Layer::Desk)]),
        restored("region"),
    );
    h.press(&mut vcx, ".");
    let titles: Vec<String> = h
        .action_menu(&vcx)
        .expect("the menu")
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert!(titles.iter().any(|t| t == "Revert\u{2026}"), "{titles:?}");
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "classifications::revert");
    assert_eq!(
        h.confirm(&vcx).as_deref(),
        Some("revert region to the desk copy \u{2014} y reverts")
    );
    vcx.simulate_keystrokes("y");
    assert_eq!(h.edits(&mut vcx), [removal("region")]);
    assert_eq!(h.shown_name(&vcx).as_deref(), Some("region"));
}

/// Without a desk copy under it there is nothing to revert to: no row, and
/// the palette's revert refuses.
#[gpui::test]
fn revert_is_not_offered_without_a_desk_copy(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(TWO, &["region"], &[]),
        restored("region"),
    );
    h.press(&mut vcx, ".");
    let titles: Vec<String> = h
        .action_menu(&vcx)
        .unwrap()
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert!(!titles.iter().any(|t| t == "Revert\u{2026}"), "{titles:?}");
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "classifications::revert");
    assert_eq!(h.confirm(&vcx), None);
    assert!(h.edits(&mut vcx).is_empty());
}

#[gpui::test]
fn n_cancels_a_confirm_and_nothing_is_written(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(TWO, &["region"], &[]),
        restored("region"),
    );
    h.act(&mut vcx, "classifications::delete");
    assert!(h.confirm(&vcx).is_some());
    vcx.simulate_keystrokes("n");
    assert_eq!(h.confirm(&vcx), None);
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.shown_name(&vcx).as_deref(), Some("region"));
    assert_eq!(h.mode(&vcx).as_deref(), Some("normal"));
    // The keyboard is the tile's again.
    h.press(&mut vcx, "g c");
    assert!(h.switcher(&vcx).is_some());
}

#[gpui::test]
fn the_dot_menu_lists_actions_with_disabled_reasons(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("desk"));
    h.press(&mut vcx, ".");
    let s = |t: &str| (t.to_string(), None);
    assert_eq!(
        h.action_menu(&vcx).expect("the menu"),
        [
            s("Set label"),
            s("Clear label"),
            s("Copy label"),
            ("Paste label".into(), Some("nothing copied".into())),
            s("---"),
            s("New\u{2026}"),
            (
                "Rename\u{2026}".into(),
                Some("defined in desk config".into())
            ),
            ("Delete".into(), Some("defined in desk config".into())),
            s("Refresh values"),
            s("---"),
            s("Export CSV\u{2026}"),
            (
                "Export CSV with unclassified\u{2026}".into(),
                Some("values not loaded".into())
            ),
        ]
    );
    // A disabled row says why in full and keeps the menu open.
    let pick = |row: usize, vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| h.tile.update(cx, |t, cx| t.menu_pick(row, window, cx)));
        h.draw(vcx);
    };
    pick(7, &mut vcx);
    assert!(h.action_menu(&vcx).is_some());
    assert_eq!(
        h.notices(&vcx),
        ["desk is defined in desk config; Geode cannot remove it from there"]
    );
    // An enabled row runs the palette's action.
    pick(5, &mut vcx);
    assert_eq!(h.action_menu(&vcx), None);
    assert!(h.prompt(&vcx).is_some(), "New\u{2026} opened the prompt");
}

/// A user copy over a builtin definition (every demo classification is
/// builtin) names the builtin layer in its revert and its refusals.
#[gpui::test]
fn a_builtin_shadowed_copy_names_the_builtin_layer(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(TWO, &["region"], &[("region", Layer::Builtin)]),
        restored("region"),
    );
    h.act(&mut vcx, "classifications::delete");
    assert_eq!(h.confirm(&vcx), None);
    assert_eq!(
        h.notices(&vcx),
        [
            "region has a builtin copy under yours, which a delete would leave in place \u{2014} Revert\u{2026} removes yours"
        ]
    );
    h.press(&mut vcx, ".");
    let rows = h.action_menu(&vcx).expect("the menu");
    assert!(
        rows.contains(&(
            "Delete".to_string(),
            Some("a builtin copy stands under it".to_string())
        )),
        "{rows:?}"
    );
    h.press(&mut vcx, "escape");
    h.act(&mut vcx, "classifications::revert");
    assert_eq!(
        h.confirm(&vcx).as_deref(),
        Some("revert region to the builtin copy \u{2014} y reverts")
    );
    vcx.simulate_keystrokes("y");
    assert_eq!(h.edits(&mut vcx), [removal("region")]);
}

/// A label edit still waiting for its reload goes with the rename: the new
/// object carries it, rather than the old name's removal taking it away.
#[gpui::test]
fn a_rename_carries_a_label_edit_not_yet_reloaded(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(TWO, &["region"], &[]),
        restored("region"),
    );
    assert_eq!(h.cursor(&vcx).as_deref(), Some("DAX"));
    h.press(&mut vcx, "x");
    h.edits(&mut vcx);
    h.act(&mut vcx, "classifications::rename");
    vcx.simulate_input("zone");
    h.press(&mut vcx, "enter");
    vcx.simulate_keystrokes("y");
    let zone = DerivedDimension {
        name: "zone".into(),
        ..region(&[("SX5E", "Europe")])
    };
    assert_eq!(h.edits(&mut vcx), [edit_of(&zone), removal("region")]);
}

/// With no layer recorded for it, the tile cannot tell a removal would
/// remove anything: it refuses rather than wait on a no-op write.
#[gpui::test]
fn delete_refuses_a_classification_with_no_recorded_layer(cx: &mut gpui::TestAppContext) {
    let mut c = config(TWO);
    c.layers.remove("region");
    let (h, mut vcx) = open_with(cx, c, restored("region"));
    h.act(&mut vcx, "classifications::delete");
    assert_eq!(h.confirm(&vcx), None);
    assert_eq!(h.notices(&vcx), ["can't tell where region is defined"]);
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(h.shown_name(&vcx).as_deref(), Some("region"));
}

/// The second clear is queued after the first write fired; the reload
/// carrying only the first must not flash the second off, and the third
/// clear builds on both.
#[gpui::test]
fn a_reload_of_the_first_write_keeps_a_later_edit_in_flight(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "x");
    h.edits(&mut vcx);
    // The first write lands: SPX cleared, DAX still Europe.
    let first =
        "[region]\nfrom = \"underlying_ref\"\n[region.values]\nEurope = [\"SX5E\", \"DAX\"]\n";
    vcx.update(|_, cx| h.factory.set_config(config(first), cx));
    assert_eq!(h.label(&vcx, "DAX"), None, "the second edit stays");
    h.goto(&mut vcx, "SX5E");
    h.press(&mut vcx, "x");
    assert_eq!(
        h.edits(&mut vcx),
        [edit_of(&region(&[]))],
        "the third edit carries all three"
    );
}

// ---- final review fixes ----

/// The shell refuses a batch carrying two edits (a failed write, or a
/// merge kept last good): both optimistic labels go, not just the last.
#[gpui::test]
fn a_refusal_after_two_edits_drops_both(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "x");
    assert_eq!((h.label(&vcx, "SPX"), h.label(&vcx, "DAX")), (None, None));
    h.edits(&mut vcx);
    let why =
        "saved to disk \u{00b7} rejected by the merge \u{2014} showing the configuration in force";
    h.shell_says(&mut vcx, TileNotice::Refused(why.into()));
    assert_eq!(h.notices(&vcx), [why]);
    assert_eq!(h.label(&vcx, "SPX").as_deref(), Some("Americas"));
    assert_eq!(h.label(&vcx, "DAX").as_deref(), Some("Europe"));
}

/// The editor opened on a cursor nobody moved: a reload reordering the
/// rows under it keeps the editor on its row, and the commit writes that
/// row over the reloaded object.
#[gpui::test]
fn a_reload_reordering_rows_keeps_the_open_editor_on_its_row(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("NKY"));
    h.press(&mut vcx, "enter");
    assert!(h.editor(&vcx).is_some());
    // Another surface labels NKY: it leaves the unclassified rows at the top.
    let moved = "[region]\nfrom = \"underlying_ref\"\n[region.values]\nAmericas = [\"SPX\"]\nAsia = [\"NKY\"]\nEurope = [\"SX5E\", \"DAX\"]\n";
    vcx.update(|_, cx| h.factory.set_config(config(moved), cx));
    assert_eq!(h.shown(&vcx)[0], "HSI", "the rows moved");
    assert!(h.editor(&vcx).is_some(), "a reload keeps the editor");
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("NKY"),
        "the editor paints on the row it writes"
    );
    vcx.simulate_input("Pacific");
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.edits(&mut vcx),
        [edit_of(&region(&[
            ("SPX", "Americas"),
            ("NKY", "Pacific"),
            ("SX5E", "Europe"),
            ("DAX", "Europe"),
        ]))]
    );
}

/// A revert in the debounce window: a label verb on the reverting
/// classification is refused, so no edit built on the user copy replaces
/// the revert's removal in the shell's batch; the reload brings the verbs
/// back.
#[gpui::test]
fn a_verb_while_a_revert_is_on_its_way_does_not_undo_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(
        cx,
        config_layered(EDIT, &["region"], &[("region", Layer::Desk)]),
        restored("region"),
    );
    let asked = h.distinct_requests();
    h.deliver(
        &mut vcx,
        asked[0].tag,
        "underlying_ref",
        Ok(EDIT_VALUES.to_vec()),
    );
    h.act(&mut vcx, "classifications::revert");
    vcx.simulate_keystrokes("y");
    assert_eq!(h.edits(&mut vcx), [removal("region")]);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    h.press(&mut vcx, "enter");
    assert!(h.edits(&mut vcx).is_empty(), "nothing replaces the removal");
    assert_eq!(h.editor(&vcx), None);
    assert_eq!(h.notices(&vcx), ["reverting region\u{2026}"]);
    // The reload carrying the revert: the desk copy, no user copy over it.
    let mut desk = config(TWO);
    desk.layers.insert("region".into(), Layer::Desk);
    vcx.update(|_, cx| h.factory.set_config(desk, cx));
    h.goto(&mut vcx, "DAX");
    h.press(&mut vcx, "x");
    assert_eq!(h.edits(&mut vcx).len(), 1, "the verbs are back");
}

/// A right press on a row moves the cursor there and opens the `⋯` menu
/// hung from the pointer.
#[gpui::test]
fn a_right_click_on_a_row_opens_the_action_menu_there(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.draw(&mut vcx);
    let row = vcx
        .debug_bounds("classifications-row-SPX")
        .expect("the row is painted");
    let at = row.center();
    vcx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    h.draw(&mut vcx);
    assert_eq!(h.cursor(&vcx).as_deref(), Some("SPX"));
    let rows = h.action_menu(&vcx).expect("the menu opened");
    assert_eq!(rows[0].0, "Set label");
    let menu = vcx
        .debug_bounds("classifications-menu")
        .expect("the menu is painted");
    assert!(
        (menu.origin.x - at.x).abs() < gpui::px(2.) && (menu.origin.y - at.y).abs() < gpui::px(2.),
        "hung from the pointer: {menu:?} vs {at:?}"
    );
    // Its rows act on the row pressed.
    h.press(&mut vcx, "j enter");
    assert_eq!(h.label(&vcx, "SPX"), None, "Clear label cleared SPX");
    release_the_table_menu(&mut vcx, at);
}

/// gpui-component's table builds its (empty) context menu on every right
/// press, and that menu's dismiss subscription holds it in a cycle only
/// the table's next right press breaks, so a test ending after a right
/// press leaks it (the pricer's tests have the same helper). One more
/// right press, whose deferred rebuild never runs because the window
/// closes in the same update, breaks it.
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

/// The header says the values are loading while the read is on its way,
/// and stops on its answer, a failure, or a refused read.
#[gpui::test]
fn the_header_marks_a_pending_values_read(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), restored("region"));
    assert!(
        h.header(&vcx).contains("loading values\u{2026}"),
        "{}",
        h.header(&vcx)
    );
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Err("no such column"));
    assert!(!h.header(&vcx).contains("loading"), "{}", h.header(&vcx));
    vcx.simulate_keystrokes("shift-r");
    assert!(h.header(&vcx).contains("loading values\u{2026}"));
    let tag = h.distinct_requests()[0].tag;
    h.deliver(&mut vcx, tag, "underlying_ref", Ok(REGION_VALUES.to_vec()));
    assert!(!h.header(&vcx).contains("loading"), "{}", h.header(&vcx));
}

/// A refused read was never sent: nothing is loading.
#[gpui::test]
fn a_refused_values_read_shows_no_loading_mark(cx: &mut gpui::TestAppContext) {
    let (h, vcx) = open_over(cx, config(TWO), restored("region"), true);
    assert!(!h.header(&vcx).contains("loading"), "{}", h.header(&vcx));
}

/// Undo after a refusal skips the refused row, and says it was not saved
/// rather than blame another surface.
#[gpui::test]
fn undo_after_a_refusal_says_the_row_was_not_saved(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    h.edits(&mut vcx);
    h.shell_says(&mut vcx, TileNotice::Refused("not saved".into()));
    h.press(&mut vcx, "u");
    assert!(h.edits(&mut vcx).is_empty());
    assert_eq!(
        h.notices(&vcx),
        ["1 row was not saved and is left as it is"]
    );
}

// ---- export ----

const EXPORT: &str = "classifications::export";
const EXPORT_ALL: &str = "classifications::export_all";

impl Harness {
    /// The file requests asked since the last call, in order.
    fn file_requests(&self) -> Vec<TextFileParams> {
        self.requests
            .try_iter()
            .filter_map(|r| match r {
                Request::TextFile(p) => Some(p),
                _ => None,
            })
            .collect()
    }
    /// Answer the save dialog with `name` in the directory it opened on,
    /// or cancel it with `None`, then let the tile's wait run.
    fn save_as(&self, vcx: &mut gpui::VisualTestContext, name: Option<&str>) {
        vcx.simulate_new_path_selection(|dir| name.map(|n| dir.join(n)));
        vcx.run_until_parked();
        self.draw(vcx);
    }
    /// Answer a file request through the shell's door, as the bridge
    /// routes it.
    fn answer_file(
        &self,
        vcx: &mut gpui::VisualTestContext,
        p: &TextFileParams,
        result: TextFileResult,
    ) {
        let outcome = TextFileOutcome {
            key: p.key,
            tag: p.tag,
            path: p.path.clone(),
            result,
        };
        vcx.update(|window, cx| {
            self.content
                .deliver(Delivery::TextFile(outcome), window, cx)
        });
        self.draw(vcx);
    }
}

/// `region` as the export fixture maps it: SPX, SX5E and DAX.
fn edit_region() -> DerivedDimension {
    region(&[("SPX", "Americas"), ("SX5E", "Europe"), ("DAX", "Europe")])
}

fn observed(values: &[(&str, u64)]) -> Vec<(String, u64)> {
    values.iter().map(|(s, n)| (s.to_string(), *n)).collect()
}

#[gpui::test]
fn export_asks_for_a_path_then_writes_the_mapped_rows(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.file_requests();
    h.act(&mut vcx, EXPORT);
    assert!(vcx.did_prompt_for_new_path(), "a save dialog is up");
    assert!(
        h.file_requests().is_empty(),
        "nothing written before a path"
    );
    h.save_as(&mut vcx, Some("region.csv"));
    let asked = h.file_requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let p = &asked[0];
    assert_eq!(p.key, QueryKey(TILE));
    assert_eq!(p.path.file_name().unwrap(), "region.csv");
    assert_eq!(
        p.op,
        TextFileOp::Write {
            text: classification::export(&edit_region(), None)
        }
    );
    assert!(h.notices(&vcx).is_empty(), "nothing said before the answer");
    h.answer_file(&mut vcx, p, TextFileResult::Written(Ok(())));
    assert_eq!(h.notices(&vcx), ["exported 3 rows to region.csv"]);
}

/// The export reads the object as the tile shows it: a label edit still on
/// its way to the configuration is in the file.
#[gpui::test]
fn export_writes_an_edit_not_yet_reloaded(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.goto(&mut vcx, "SPX");
    h.press(&mut vcx, "x");
    h.edits(&mut vcx);
    h.file_requests();
    h.act(&mut vcx, EXPORT);
    h.save_as(&mut vcx, Some("region.csv"));
    let asked = h.file_requests();
    let cleared = region(&[("SX5E", "Europe"), ("DAX", "Europe")]);
    assert_eq!(
        asked[0].op,
        TextFileOp::Write {
            text: classification::export(&cleared, None)
        }
    );
}

#[gpui::test]
fn export_with_unclassified_adds_blank_rows_for_observed_values(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.file_requests();
    h.act(&mut vcx, EXPORT_ALL);
    h.save_as(&mut vcx, Some("region.csv"));
    let asked = h.file_requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let all = observed(&EDIT_VALUES);
    let text = classification::export(&edit_region(), Some(&all));
    assert!(text.contains("NKY,") && text.contains("HSI,"), "{text}");
    assert_eq!(asked[0].op, TextFileOp::Write { text });
    h.answer_file(&mut vcx, &asked[0], TextFileResult::Written(Ok(())));
    assert_eq!(h.notices(&vcx), ["exported 5 rows to region.csv"]);
}

#[gpui::test]
fn export_with_unclassified_refuses_before_values_load(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(EDIT), restored("region"));
    h.act(&mut vcx, EXPORT_ALL);
    assert!(!vcx.did_prompt_for_new_path());
    assert_eq!(
        h.notices(&vcx),
        ["values not loaded yet \u{2014} shift+r loads them"]
    );
    // The menu says so on the row, and the plain export stays open.
    h.press(&mut vcx, ".");
    let menu = h.action_menu(&vcx).expect("the menu");
    assert!(
        menu.contains(&(
            "Export CSV with unclassified\u{2026}".into(),
            Some("values not loaded".into())
        )),
        "{menu:?}"
    );
    assert!(
        menu.contains(&("Export CSV\u{2026}".into(), None)),
        "{menu:?}"
    );
}

/// A failed values read is no answer to export: the observed values are
/// not known.
#[gpui::test]
fn export_with_unclassified_refuses_after_a_failed_read(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(EDIT), restored("region"));
    let asked = h.distinct_requests();
    h.deliver(&mut vcx, asked[0].tag, "underlying_ref", Err("no table"));
    h.act(&mut vcx, EXPORT_ALL);
    assert!(!vcx.did_prompt_for_new_path());
}

#[gpui::test]
fn a_cancelled_save_dialog_does_nothing(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.file_requests();
    h.act(&mut vcx, EXPORT);
    h.save_as(&mut vcx, None);
    assert!(h.file_requests().is_empty());
    assert!(h.notices(&vcx).is_empty());
}

#[gpui::test]
fn a_failed_write_shows_the_error(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.file_requests();
    h.act(&mut vcx, EXPORT);
    h.save_as(&mut vcx, Some("region.csv"));
    let asked = h.file_requests();
    h.answer_file(
        &mut vcx,
        &asked[0],
        TextFileResult::Written(Err("region.csv: Permission denied".into())),
    );
    let notices = h.tile.read_with(&vcx, |t, _| t.chrome.notices.clone());
    assert_eq!(
        notices,
        [Notice::danger(
            "export failed: region.csv: Permission denied"
        )]
    );
}

#[gpui::test]
fn a_busy_service_refuses_the_export(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.act(&mut vcx, EXPORT);
    h.data.fill_for_tests();
    h.save_as(&mut vcx, Some("region.csv"));
    assert!(h.file_requests().is_empty());
    assert_eq!(
        h.notices(&vcx),
        ["the data service is busy \u{2014} try again"]
    );
}

/// Two exports: the first's answer was overtaken and says nothing; the
/// second dialog opens where the first file went.
#[gpui::test]
fn only_the_latest_file_operation_is_answered(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = editing(cx);
    h.file_requests();
    let dir = std::env::temp_dir().join("geode-export-test");
    h.act(&mut vcx, EXPORT);
    let first = dir.join("first.csv");
    vcx.simulate_new_path_selection(|_| Some(first.clone()));
    vcx.run_until_parked();
    h.act(&mut vcx, EXPORT);
    vcx.simulate_new_path_selection(|opened_on| {
        assert_eq!(opened_on, dir.as_path(), "the last file's directory");
        Some(dir.join("second.csv"))
    });
    vcx.run_until_parked();
    let asked = h.file_requests();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[1].tag > asked[0].tag);
    h.answer_file(&mut vcx, &asked[0], TextFileResult::Written(Ok(())));
    assert!(h.notices(&vcx).is_empty(), "overtaken: dropped");
    h.answer_file(&mut vcx, &asked[1], TextFileResult::Written(Ok(())));
    assert_eq!(h.notices(&vcx), ["exported 3 rows to second.csv"]);
    // Answered once: the same answer again says nothing more.
    h.tile.update(&mut vcx, |t, _| {
        t.notices.outcome.clear();
        t.rebuild_chrome();
    });
    h.answer_file(&mut vcx, &asked[1], TextFileResult::Written(Ok(())));
    assert!(h.notices(&vcx).is_empty());
}

#[gpui::test]
fn export_with_nothing_shown_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with(cx, config(TWO), None);
    h.act(&mut vcx, "classifications::cancel");
    h.act(&mut vcx, EXPORT);
    assert!(!vcx.did_prompt_for_new_path());
    assert_eq!(h.notices(&vcx), ["choose a classification to export"]);
    h.act(&mut vcx, EXPORT_ALL);
    assert!(!vcx.did_prompt_for_new_path());
    assert_eq!(h.notices(&vcx), ["choose a classification to export"]);
}

/// The save dialog is modeless: a classification switched to while it
/// stood must not go out under the first one's file name.
#[gpui::test]
fn an_export_answered_after_a_switch_is_refused(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = region_with_values(cx);
    h.file_requests();
    h.act(&mut vcx, EXPORT);
    h.press(&mut vcx, "g c k enter");
    assert_eq!(h.shown_name(&vcx).as_deref(), Some("desk"));
    h.save_as(&mut vcx, Some("region.csv"));
    assert!(h.file_requests().is_empty(), "nothing written");
    assert_eq!(
        h.notices(&vcx),
        ["region is no longer shown \u{2014} nothing exported"]
    );
}

/// Values from an earlier answer are not current while a newer read is on
/// its way or after one failed: the unclassified rows could be missing.
#[gpui::test]
fn export_with_unclassified_waits_for_current_values(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = region_with_values(cx);
    const STALE: &str = "values not current \u{2014} shift+r reloads them";
    h.press(&mut vcx, "shift-r");
    let pending = h.distinct_requests();
    h.act(&mut vcx, EXPORT_ALL);
    assert!(!vcx.did_prompt_for_new_path(), "a read is on its way");
    assert_eq!(h.notices(&vcx), [STALE]);
    h.deliver(&mut vcx, pending[0].tag, "underlying_ref", Err("no table"));
    h.act(&mut vcx, EXPORT_ALL);
    assert!(!vcx.did_prompt_for_new_path(), "the refresh failed");
    assert!(
        h.notices(&vcx).contains(&STALE.to_string()),
        "{:?}",
        h.notices(&vcx)
    );
    h.press(&mut vcx, ".");
    let menu = h.action_menu(&vcx).expect("the menu");
    assert!(
        menu.contains(&(
            "Export CSV with unclassified\u{2026}".into(),
            Some("values not current".into())
        )),
        "{menu:?}"
    );
    h.press(&mut vcx, "escape");
    h.press(&mut vcx, "shift-r");
    let retry = h.distinct_requests();
    h.deliver(
        &mut vcx,
        retry[0].tag,
        "underlying_ref",
        Ok(REGION_VALUES.to_vec()),
    );
    h.act(&mut vcx, EXPORT_ALL);
    assert!(vcx.did_prompt_for_new_path(), "current again");
}
