//! The tile hosted the way the shell hosts it: built by the factory over a
//! `for_tests` data handle (the test is the data service), under a shell
//! stand-in that resolves keys through the app's keymap, in a `Root`.
//! Keys go through the stand-in, answers through the content's `deliver`,
//! config through the factory's `set_config`, as the bridge pushes it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use geode_core::calibration::{BUILTIN_CALIBRATION, CALIBRATION_DOC, check_against, from_doc};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::egress_config::EgressSpec;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::schema::SchemaSpec;
use geode_core::scopes::SavedScopes;
use geode_data::{DataHandle, Request};
use geode_shell::actions::{ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{ConfigEdit, Frame, FrameRef, TileNotice};
use geode_shell::keymap::{KeyContext, Keymap, MatchResult, Matcher, build_keymap};
use geode_shell::module::{Delivery, ModuleFactory, ModuleRoster, TileContent};
use geode_shell::tiling::{TileId, WorkspaceIx};
use gpui::{AppContext as _, Entity, Window};

use crate::content::{CalibrationConfig, CalibrationFactory, stage_tables};
use crate::tile::CalibrationTile;

/// The demo schema: the datasets the builtin pipeline's sources read and
/// its publish stage writes are document datasets here.
const DEMO_DATASETS: &str = include_str!("../../../../../examples/demo-config/datasets.toml");

/// What the shell root is to a tile for keys: a focused ancestor whose
/// listener matches each keystroke against the app's keymap under the
/// stack `[workspace, tile, <tile's context>]` and dispatches the match
/// through the tile's `TileContent` door, as the shell does. In `insert`
/// mode it follows the shell's insert branch: a bare key resolves against
/// the tile's context alone, so a field's `enter` and `escape` reach its
/// verbs and typing stays text (chords in insert mode are not modelled).
///
/// Focus: as the shell's render does, it takes the keyboard back when no
/// live handle holds it (a field the tile blurred on close), so the next
/// key reaches the keymap rather than nothing.
struct ShellStandIn {
    focus: gpui::FocusHandle,
    tile: Entity<CalibrationTile>,
    content: Rc<dyn TileContent>,
    keymap: Rc<Keymap>,
    matcher: Matcher,
    /// Every action id the keymap matched, in order, whether or not the
    /// tile carried it out.
    dispatched: Rc<RefCell<Vec<String>>>,
}

impl gpui::Render for ShellStandIn {
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
        if window.focused(cx).is_none() {
            self.focus.focus(window, cx);
        }
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
                let stack = match context.get("mode") {
                    Some("insert") if !keystroke.mods.is_chord() => vec![context],
                    Some("insert") => return,
                    _ => vec![
                        KeyContext::new("workspace"),
                        KeyContext::new("tile"),
                        context,
                    ],
                };
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
fn app_keymap(factory: &Rc<CalibrationFactory>) -> Keymap {
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

#[allow(dead_code, reason = "the graph, data and verb tests drive the rest")]
pub(super) struct Harness {
    pub tile: Entity<CalibrationTile>,
    /// Driven through the trait: the shell's door.
    pub content: Rc<dyn TileContent>,
    pub factory: Rc<CalibrationFactory>,
    pub data: DataHandle,
    /// What the tile asked of the data service; the test answers.
    pub rx: RefCell<Option<Receiver<Request>>>,
    pub frame: Entity<Frame>,
    pub diagnostics: Entity<Diagnostics>,
    dispatched: Rc<RefCell<Vec<String>>>,
}

struct Built {
    content: Rc<dyn TileContent>,
    tile: Entity<CalibrationTile>,
    shell_focus: gpui::FocusHandle,
    frame: Entity<Frame>,
    diagnostics: Entity<Diagnostics>,
}

/// The demo schema, read as the app reads `datasets.toml`.
pub(super) fn demo_schema() -> Arc<SchemaSpec> {
    let doc = merge_docs(
        "datasets",
        &[LayerDoc::builtin("datasets", DEMO_DATASETS).expect("well-formed TOML")],
    );
    let (schema, diags) = SchemaSpec::from_doc(&doc);
    assert!(diags.is_empty(), "{diags:?}");
    Arc::new(schema)
}

/// The demo `sophis` target, accepting the rate curve the builtin pipeline
/// publishes.
fn demo_egress() -> Vec<EgressSpec> {
    vec![EgressSpec {
        name: "sophis".into(),
        adapter: "demo_bus".into(),
        documents: vec![(
            "rate_curve".into(),
            "marketdata/ratecurve/{key}/NOTIFY".into(),
        )],
    }]
}

/// `text` read and checked as the bridge reads a calibration document,
/// against the demo schema and target.
pub(super) fn config_from(text: &str) -> CalibrationConfig {
    let doc = merge_docs(
        CALIBRATION_DOC,
        &[LayerDoc::builtin(CALIBRATION_DOC, text).expect("well-formed TOML")],
    );
    let (pipelines, mut diags) = from_doc(&doc);
    let schema = demo_schema();
    let egress = demo_egress();
    let (pipelines, d) = check_against(pipelines, &schema, &egress);
    diags.extend(d);
    CalibrationConfig {
        pipelines: Arc::new(pipelines),
        schema,
        egress: Arc::new(
            egress
                .into_iter()
                .map(|e| (e.name, e.documents.into_iter().map(|(d, _)| d).collect()))
                .collect(),
        ),
        diagnostics: Arc::new(diags),
        sources: Arc::new(stage_tables(&doc)),
    }
}

/// The builtin document: `implied_forwards`, clean against the demo schema.
pub(super) fn builtin_config() -> CalibrationConfig {
    let config = config_from(BUILTIN_CALIBRATION);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    assert_eq!(config.pipelines.pipelines.len(), 1);
    config
}

/// The builtin document with both Kalman fits' `knots` reduced to one
/// value, which `from_doc` refuses: `implied_forwards` is dropped whole,
/// the first finding at `stages.repo_fit.knots`, and no pipeline is kept.
pub(super) fn dropped_config() -> CalibrationConfig {
    let text = BUILTIN_CALIBRATION.replace(
        "knots             = [0.25, 0.5, 1.0, 2.0, 3.0, 5.0]",
        "knots             = [1.0]",
    );
    assert_ne!(text, BUILTIN_CALIBRATION, "the fixture edits the knots");
    let config = config_from(&text);
    assert!(
        config.pipelines.pipelines.is_empty(),
        "the pipeline is dropped"
    );
    assert_eq!(
        config.diagnostics[0].path.as_deref(),
        Some("stages.repo_fit.knots"),
        "{:?}",
        config.diagnostics
    );
    config
}

/// The builtin document plus a pipeline `check_against` drops with a
/// warning: one finding, one pipeline kept.
pub(super) fn warned_config() -> CalibrationConfig {
    let text = format!(
        "{BUILTIN_CALIBRATION}\n[pipelines.p]\n\
         [stages.src]\npipeline = \"p\"\nkind = \"source\"\ndataset = \"nope\"\n"
    );
    let config = config_from(&text);
    assert_eq!(config.diagnostics.len(), 1, "{:?}", config.diagnostics);
    assert_eq!(config.pipelines.pipelines.len(), 1);
    config
}

/// A session record with the given keys.
pub(super) fn restored(
    underlying: Option<&str>,
    pipeline: Option<&str>,
    cursor: Option<&str>,
) -> toml::Table {
    let mut t = toml::Table::new();
    for (key, value) in [
        ("underlying", underlying),
        ("pipeline", pipeline),
        ("cursor", cursor),
    ] {
        if let Some(v) = value {
            t.insert(key.into(), toml::Value::String(v.to_string()));
        }
    }
    t
}

/// A tile built by the factory after `config` was pushed (none pushed
/// when `None`), with `restored` as its record, announced visible as the
/// shell announces a fresh occupant, and focused.
pub(super) fn open_with(
    cx: &mut gpui::TestAppContext,
    config: Option<CalibrationConfig>,
    restored: Option<toml::Table>,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::init);
    cx.update(geode_shell::shell::dialog::init_reclaimed_keybindings);
    // Displayed dates and times read the app's clock; UTC keeps them fixed.
    cx.update(|cx| cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::utc())));
    let (data, rx) = DataHandle::for_tests();
    let factory = Rc::new(CalibrationFactory::new(data.clone()));
    if let Some(config) = config {
        cx.update(|cx| factory.set_config(config, cx));
    }
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
                    TileId(Harness::TILE),
                    restored.as_ref(),
                    FrameRef::for_tile(frame.clone(), WorkspaceIx::FIRST, TileId(Harness::TILE)),
                    diagnostics.clone(),
                    window,
                    cx,
                );
                assert_eq!(occupant.kind, crate::KIND);
                let tile = occupant.view.clone().downcast::<CalibrationTile>().unwrap();
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

#[allow(dead_code, reason = "the graph, data and verb tests drive the rest")]
impl Harness {
    pub(super) const TILE: u64 = 7;

    /// Every action id the keymap matched so far, in order.
    pub(super) fn dispatched(&self) -> Vec<String> {
        self.dispatched.borrow().clone()
    }

    /// Paint a frame, so debug bounds follow the state.
    pub(super) fn draw(&self, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// Every request the tile made since the last call. Submission is
    /// synchronous, so the channel already holds them.
    pub(super) fn requests(&self) -> Vec<Request> {
        match self.rx.borrow().as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => Vec::new(),
        }
    }

    pub(super) fn deliver(&self, vcx: &mut gpui::VisualTestContext, d: Delivery) {
        vcx.update(|window, cx| self.content.deliver(d, window, cx));
        vcx.run_until_parked();
    }

    /// Keys as the trader types them, through the stand-in, then a frame:
    /// the window redraws between a trader's keys, and a field a key
    /// opened must be painted before the next key can reach it.
    pub(super) fn press(&self, vcx: &mut gpui::VisualTestContext, keys: &str) {
        vcx.simulate_keystrokes(keys);
        vcx.run_until_parked();
        self.draw(vcx);
    }

    /// A registered action, the way the palette reaches the tile.
    pub(super) fn act(&self, vcx: &mut gpui::VisualTestContext, id: &str) -> bool {
        let id = ActionId(id.to_string());
        vcx.update(|window, cx| self.content.dispatch(&id, None, window, cx))
    }

    pub(super) fn command(
        &self,
        vcx: &mut gpui::VisualTestContext,
        line: &str,
    ) -> Result<(), String> {
        vcx.update(|window, cx| self.content.command(line, window, cx))
    }

    /// Push `config` through the factory, as the bridge does at startup
    /// and on a reload.
    pub(super) fn set_config(&self, vcx: &mut gpui::VisualTestContext, config: CalibrationConfig) {
        vcx.update(|_, cx| self.factory.set_config(config, cx));
        vcx.run_until_parked();
    }

    pub(super) fn title(&self, vcx: &gpui::VisualTestContext) -> String {
        vcx.read(|cx| self.content.title(cx).to_string())
    }

    pub(super) fn serialize(&self, vcx: &gpui::VisualTestContext) -> toml::Table {
        vcx.read(|cx| self.content.serialize(cx))
    }

    pub(super) fn key_context(&self, vcx: &gpui::VisualTestContext) -> KeyContext {
        vcx.read(|cx| self.content.key_context(cx))
    }

    /// The header's prepared `{n} stages · {k} fits`, if a pipeline is shown.
    pub(super) fn counts(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.header_counts().map(|s| s.to_string()))
    }

    /// The body placeholder's text, `None` while a pipeline is shown.
    pub(super) fn body_text(&self, vcx: &gpui::VisualTestContext) -> Option<&'static str> {
        self.tile.read_with(vcx, |t, _| t.body_text())
    }

    /// The header notices that show (dismissed ones left out).
    pub(super) fn notices(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| t.notice_texts())
    }

    /// The configuration object the tile shows, through the shell's door.
    pub(super) fn config_object(
        &self,
        vcx: &gpui::VisualTestContext,
    ) -> Option<geode_shell::module::ConfigObject> {
        vcx.read(|cx| self.content.config_object(cx))
    }

    /// The actions the tile asked the shell to run since the last call,
    /// drained from the frame as the shell's observer drains them.
    pub(super) fn requested_actions(
        &self,
        vcx: &mut gpui::VisualTestContext,
    ) -> Vec<(TileId, ActionId)> {
        vcx.update(|_, cx| self.frame.update(cx, |f, _| f.take_requested_actions()))
    }

    /// The whole-object config edits the tile queued since the last call,
    /// drained from the frame as the shell's observer drains them.
    pub(super) fn config_edits(&self, vcx: &mut gpui::VisualTestContext) -> Vec<ConfigEdit> {
        vcx.update(|_, cx| self.frame.update(cx, |f, _| f.take_pending_config_edits()))
    }

    /// What the shell tells the tile of an edit it drained: posted on the
    /// frame and notified, as the shell's deferred post does.
    pub(super) fn post_notice(&self, vcx: &mut gpui::VisualTestContext, notice: TileNotice) {
        vcx.update(|_, cx| {
            self.frame.update(cx, |f, cx| {
                f.post_tile_notice_for_test(TileId(Harness::TILE), notice);
                cx.notify();
            })
        });
        vcx.run_until_parked();
    }
}
