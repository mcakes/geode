use super::*;
use crate::commands;
use crate::content::{ACTIONS, DEFAULT_KEYMAP, TimeseriesFactory};
use crate::core::model::SlotState;
use crate::core::{Colour, Model, Preset, Range};
use crate::popup::{PickerStage, SeriesRow, Which};
use geode_chart::{Axis, ChartModel};
use geode_core::colour::NamedColours;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::query::QueryKey;
use geode_core::scopes::SavedScopes;
use geode_core::series::{BucketRule, Frequency, SlotKind, SlotProvenance, SlotResult};
use geode_data::{DataHandle, Request};
use geode_shell::actions::{ActionId, ActionRegistry};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::Frame;
use geode_shell::module::{Delivery, ModuleFactory, ModuleRoster, TileContent, TileOccupant};
use geode_shell::series::{FetchSource, SeriesSettings};
use geode_shell::tiling::TileId;
use geode_widgets::datefield::Segment;
use gpui::{Entity, SharedString, Window};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;

const TILE: u64 = 7;

/// What the window closure hands back: it can return only one value,
/// so everything a test drives or reads is parked here on the way
/// out.
struct Built {
    content: Box<dyn TileContent>,
    tile: Entity<TimeseriesTile>,
    frame: Entity<Frame>,
    diagnostics: Entity<Diagnostics>,
}

struct Harness {
    tile: Entity<TimeseriesTile>,
    /// Driven through the trait, never by poking the entity: the
    /// shell's own door is what a key, a `:` line and a delivery all
    /// arrive through.
    content: Box<dyn TileContent>,
    frame: Entity<Frame>,
    diagnostics: Entity<Diagnostics>,
    factory: Rc<TimeseriesFactory>,
    /// Every `Request` the tile submitted, in order. Held for the
    /// whole harness's life: dropping the receiver closes the
    /// channel and `DataHandle::send` starts answering `false` —
    /// which is exactly what [`Harness::close_channel`] does on
    /// purpose, and why this is an `Option`.
    rx: RefCell<Option<Receiver<Request>>>,
}

/// A `Box<dyn ModuleFactory>` over the harness's own `Rc` — the
/// roster takes ownership, and the factory test still needs its
/// handle afterwards.
struct Handle(Rc<TimeseriesFactory>);

impl ModuleFactory for Handle {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }
    fn contexts(&self) -> Vec<&'static str> {
        self.0.contexts()
    }
    fn default_keymap(&self) -> Option<&'static str> {
        self.0.default_keymap()
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        self.0.register_actions(registry)
    }
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> TileOccupant {
        self.0
            .create(tile, restored, frame, diagnostics, window, cx)
    }
}

/// A `colours.toml` holding one name, `spx`, at `degrees` on the
/// wheel — what `:colour s1 spx` resolves against, and what a
/// reload redefines.
/// `n` HOURLY buckets ending an hour before the current hour, one
/// `SlotResult` per number. Hourly and recent on purpose: every
/// range this module's tests use (`1y`, `1w`) contains the span, and
/// the last bucket sits strictly inside `range.1 = now`, so a zoomed
/// window really is a proper subset of the range.
fn result_with(slots: &[u8], n: usize) -> SeriesResult {
    const STEP: i64 = 3_600_000_000;
    let last = (chrono::Utc::now().timestamp_micros() / STEP) * STEP - STEP;
    SeriesResult {
        buckets: (0..n)
            .map(|i| last - (n as i64 - 1 - i as i64) * STEP)
            .collect(),
        slots: slots
            .iter()
            .map(|&slot| SlotResult {
                slot,
                values: (0..n).map(|i| i as f64).collect(),
                percentiles: vec![(0.05, 1.0), (0.5, 2.0), (0.95, 3.0)],
                bins: Vec::new(),
                provenance: SlotProvenance {
                    loaded: None,
                    latest_received_at: None,
                    health: None,
                },
            })
            .collect(),
    }
}

/// The tag of the LAST series request in a drained batch.
fn h_last_tag(reqs: &[Request]) -> u64 {
    reqs.iter()
        .rev()
        .find_map(|r| match r {
            Request::Series(p) => Some(p.tag),
            _ => None,
        })
        .expect("a series request in the batch")
}

/// The `as_of` mutation plus the `open_flip` a scope-bar as-of change
/// makes, in ONE update block — the shell's own frame observer is
/// registered before any occupant's, so this is the order a tile
/// really sees.
fn open_barrier_on_as_of(
    h: &Harness,
    vcx: &mut gpui::VisualTestContext,
    keys: &[QueryKey],
    at: DateTime<Utc>,
) {
    let keys = keys.to_vec();
    h.frame.update(vcx, |f, cx| {
        f.set_as_of(AsOf::At(at));
        f.open_flip(keys, std::time::Instant::now());
        cx.notify();
    });
}

/// A catalogue delivery, as the bridge makes one: the snapshot into
/// `Diagnostics` plus the `cx.notify()` that is the only way an OPEN
/// picker ever hears about it.
fn seed_catalog(h: &Harness, vcx: &mut gpui::VisualTestContext, sources: &[(&str, &[&str])]) {
    let identities: Vec<(String, Vec<String>)> = sources
        .iter()
        .map(|(source, ids)| {
            (
                source.to_string(),
                ids.iter().map(|i| i.to_string()).collect(),
            )
        })
        .collect();
    h.diagnostics.update(vcx, |d, cx| {
        d.set_catalog(geode_core::query::CatalogSnapshot {
            identities,
            ..Default::default()
        });
        cx.notify();
    });
}

fn named_colours(degrees: f32) -> NamedColours {
    let mut c = NamedColours::default();
    c.insert(
        "spx".to_string(),
        geode_core::colour::Definition::hue(degrees, geode_core::colour::Tone::Normal),
    );
    c
}

fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    open_full(cx, None, Some("demo_kdb"), demo_sources())
}

fn open_with(
    cx: &mut gpui::TestAppContext,
    restored: Option<toml::Table>,
) -> (Harness, gpui::VisualTestContext) {
    open_full(cx, restored, Some("demo_kdb"), demo_sources())
}

fn open_with_default_source(
    cx: &mut gpui::TestAppContext,
    default_source: Option<&str>,
) -> (Harness, gpui::VisualTestContext) {
    open_full(cx, None, default_source, demo_sources())
}

/// A build with `[sources]` holding no fetch source at all — what a
/// desk that has not configured one yet actually has.
fn open_without_sources(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    open_full(cx, None, None, Vec::new())
}

fn demo_sources() -> Vec<FetchSource> {
    vec![
        FetchSource {
            name: "demo_kdb".into(),
            dataset: "series".into(),
        },
        FetchSource {
            name: "demo_rest".into(),
            dataset: "series".into(),
        },
    ]
}

fn open_full(
    cx: &mut gpui::TestAppContext,
    restored: Option<toml::Table>,
    default_source: Option<&str>,
    sources: Vec<FetchSource>,
) -> (Harness, gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    // The module's own key reclaim, exactly as `main.rs` will call
    // it: without it `Root`'s window-wide `tab` binding eats the
    // range popup's field switch before any listener runs.
    cx.update(crate::init);
    let default_source = default_source.map(str::to_string);
    cx.update(move |cx| {
        cx.set_global(SeriesSettings {
            default_source,
            sources,
        })
    });
    let (data, rx) = DataHandle::for_tests();
    let factory = Rc::new(TimeseriesFactory::new(data, named_colours(0.0)));
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
                    frame.clone(),
                    diagnostics.clone(),
                    window,
                    cx,
                );
                let tile = occupant.view.clone().downcast::<TimeseriesTile>().unwrap();
                *slot.borrow_mut() = Some(Built {
                    content: occupant.content,
                    tile: tile.clone(),
                    frame,
                    diagnostics,
                });
                // Wrapped in `Root`, exactly as `main.rs` wraps the
                // shell: gpui-component registers the focused
                // `InputState` on the `Root`, so a tile that opens a
                // field needs one for focus to behave
                // here as it does in the app.
                cx.new(|cx| gpui_component::Root::new(tile, window, cx))
            })
        })
        .unwrap();
    let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
    let built = slot.borrow_mut().take().expect("the factory built one");
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (
        Harness {
            tile: built.tile,
            content: built.content,
            frame: built.frame,
            diagnostics: built.diagnostics,
            factory,
            rx: RefCell::new(Some(rx)),
        },
        vcx,
    )
}

impl Harness {
    fn command(&self, vcx: &mut gpui::VisualTestContext, line: &str) -> Result<(), String> {
        vcx.update(|window, cx| self.content.command(line, window, cx))
    }
    fn dispatch(&self, vcx: &mut gpui::VisualTestContext, verb: &str, count: Option<u32>) {
        self.dispatch_handled(vcx, verb, count);
    }
    /// [`Harness::dispatch`] keeping the answer — "did this tile
    /// handle it?", which is what decides whether a standing notice
    /// survives the keystroke.
    fn dispatch_handled(
        &self,
        vcx: &mut gpui::VisualTestContext,
        verb: &str,
        count: Option<u32>,
    ) -> bool {
        let id = ActionId(format!("timeseries::{verb}"));
        vcx.update(|window, cx| self.content.dispatch(&id, count, window, cx))
    }
    fn visible(&self, vcx: &mut gpui::VisualTestContext, visible: bool) {
        vcx.update(|_, cx| self.content.set_visible(visible, cx));
    }
    /// Drop the receiver, which is how `DataHandle::send` is made to
    /// refuse: it answers `false` on a `Disconnected` channel
    /// exactly as it does on a full one.
    fn close_channel(&self) {
        self.rx.borrow_mut().take();
    }
    /// Everything submitted since the last drain, `Cancel` included.
    fn raw_requests(&self) -> Vec<Request> {
        match self.rx.borrow().as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => Vec::new(),
        }
    }
    /// Everything submitted since the last drain except a `Cancel` —
    /// what a test asserting on ORDER reads, since a hide's cancel is
    /// housekeeping rather than a question.
    fn requests(&self) -> Vec<Request> {
        self.raw_requests()
            .into_iter()
            .filter(|r| !matches!(r, Request::Cancel { .. }))
            .collect()
    }
    /// The FIRST fetch submitted since the last drain, skipping (and
    /// consuming) every other kind. Never panics: `None` means none
    /// was sent, which is a claim tests make as often as its
    /// opposite.
    fn fetch_request(&self) -> Option<geode_data::FetchParams> {
        let rx = self.rx.borrow();
        let rx = rx.as_ref()?;
        while let Ok(request) = rx.try_recv() {
            if let Request::Fetch(params) = request {
                return Some(params);
            }
        }
        None
    }
    /// [`Self::fetch_request`]'s twin for a series query.
    fn series_request(&self) -> Option<geode_core::series::SeriesParams> {
        let rx = self.rx.borrow();
        let rx = rx.as_ref()?;
        while let Ok(request) = rx.try_recv() {
            if let Request::Series(params) = request {
                return Some(params);
            }
        }
        None
    }
    fn deliver_series(&self, vcx: &mut gpui::VisualTestContext, tag: u64, result: SeriesResult) {
        self.deliver_outcome(vcx, tag, Ok(result));
    }
    fn deliver_series_err(&self, vcx: &mut gpui::VisualTestContext, tag: u64, text: &str) {
        self.deliver_outcome(vcx, tag, Err(text.to_string()));
    }
    fn deliver_outcome(
        &self,
        vcx: &mut gpui::VisualTestContext,
        tag: u64,
        result: Result<SeriesResult, String>,
    ) {
        let outcome = SeriesOutcome {
            key: QueryKey(TILE),
            tag,
            submitted: std::time::Instant::now(),
            result,
        };
        vcx.update(|window, cx| self.content.deliver(Delivery::Series(outcome), window, cx));
    }
    fn deliver_fetched(
        &self,
        vcx: &mut gpui::VisualTestContext,
        source: &str,
        identity: &str,
        result: Result<u64, String>,
    ) {
        let delivery = Delivery::SeriesFetched {
            source: source.to_string(),
            identity: identity.to_string(),
            result,
        };
        vcx.update(|window, cx| self.content.deliver(delivery, window, cx));
    }
    fn model(&self, vcx: &gpui::VisualTestContext) -> Model {
        self.tile.read_with(vcx, |t, _| t.model().clone())
    }
    fn notice(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile
            .read_with(vcx, |t, _| t.notice().map(|n| n.to_string()))
    }
    fn chart(&self, vcx: &gpui::VisualTestContext) -> std::sync::Arc<ChartModel> {
        self.tile.read_with(vcx, |t, _| t.chart().clone())
    }
    fn acted(&self, vcx: &gpui::VisualTestContext) -> Option<geode_shell::frame::FrameVersions> {
        self.tile.read_with(vcx, |t, _| t.acted())
    }
    fn barrier_wants(&self, vcx: &gpui::VisualTestContext, versions: FrameVersions) -> bool {
        self.frame
            .read_with(vcx, |f, _| f.barrier_wants(QueryKey(TILE), versions))
    }
    fn title(&self, vcx: &mut gpui::VisualTestContext) -> SharedString {
        vcx.update(|_, cx| self.content.title(cx))
    }
    /// What the header PAINTS, read off the prepared model rather
    /// than the pixels: `range · freq`, then every chip's label and
    /// axis letter, plus the empty hint while the tile holds no
    /// slot. Painted pixels stay the display check's.
    fn painted_text(&self, vcx: &mut gpui::VisualTestContext) -> String {
        self.tile.read_with(vcx, |t, _| {
            let h = t.header();
            let mut parts = vec![h.range_freq.to_string()];
            for chip in &h.chips {
                parts.push(chip.label.to_string());
                parts.push(chip.axis.to_string());
            }
            if h.empty {
                parts.push(crate::header::EMPTY_HINT.to_string());
            }
            parts.join(" ")
        })
    }
    fn key_context_mode(&self, vcx: &mut gpui::VisualTestContext) -> String {
        self.tile.read_with(vcx, |_, cx| {
            self.content
                .key_context(cx)
                .get("mode")
                .unwrap_or("")
                .to_string()
        })
    }
    fn factory_handle(&self) -> Box<dyn ModuleFactory> {
        Box::new(Handle(self.factory.clone()))
    }
    /// The first chip's resolved swatch — what a colour reload has
    /// to move.
    fn swatch(&self, vcx: &gpui::VisualTestContext) -> gpui::Hsla {
        self.tile.read_with(vcx, |t, _| t.header().chips[0].swatch)
    }
    fn draw(&self, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    /// Whether the series list is what the tile has open.
    fn popup_is_series(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile
            .read_with(vcx, |t, _| matches!(t.popup(), Some(Popup::Series(_))))
    }
    fn popup_is_none(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile.read_with(vcx, |t, _| t.popup().is_none())
    }
    /// The list's PREPARED rows — the popup's own "painted text",
    /// read the way [`Harness::painted_text`] reads the header's.
    fn series_rows(&self, vcx: &gpui::VisualTestContext) -> Vec<SeriesRow> {
        self.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Series(p)) => p.rows.clone(),
            _ => Vec::new(),
        })
    }
    /// The picker's ranked options, as spelled — the picker's own
    /// "painted text", read the way [`Harness::series_rows`] reads
    /// the list's rows.
    fn picker_rows(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
        self.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Picker(p)) => p.ranked_options(),
            _ => Vec::new(),
        })
    }
    /// The already-loaded mark for each RANKED row, in the order
    /// [`Harness::picker_rows`] hands them back.
    fn picker_loaded_marks(&self, vcx: &gpui::VisualTestContext) -> Vec<bool> {
        self.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Picker(p)) => p.ranked_loaded(),
            _ => Vec::new(),
        })
    }
    fn picker_add_row(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Picker(p)) => p.add_row.clone(),
            _ => None,
        })
    }
    fn picker_is_source_stage(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile.read_with(vcx, |t, _| {
            matches!(
                t.popup(),
                Some(Popup::Picker(p)) if matches!(p.stage, PickerStage::Sources { .. })
            )
        })
    }
    fn picker_highlighted(&self, vcx: &gpui::VisualTestContext) -> String {
        self.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Picker(p)) => p.list.highlighted_text().unwrap_or("").to_string(),
            _ => String::new(),
        })
    }
    fn popup_is_expr(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile
            .read_with(vcx, |t, _| matches!(t.popup(), Some(Popup::Expr(_))))
    }
    /// The expression field's inline parse error.
    fn expr_error(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Expr(f)) => f.error.as_ref().map(|e| e.to_string()),
            _ => None,
        })
    }
    fn popup_is_range(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile
            .read_with(vcx, |t, _| matches!(t.popup(), Some(Popup::Range(_))))
    }
    /// Where the range popup's keyboard is: which field, and which
    /// of that field's segments — the popup's own "painted text",
    /// read off the state the painter takes.
    fn range_active_segment(&self, vcx: &gpui::VisualTestContext) -> (Which, Segment) {
        self.tile
            .read_with(vcx, |t, _| match t.popup() {
                Some(Popup::Range(r)) => Some((r.active, r.active_field().segment())),
                _ => None,
            })
            .expect("the range popup is open")
    }
    /// The two dates the range popup's fields hold right now —
    /// committed values, so a segment mid-entry is not in them.
    fn range_dates(&self, vcx: &gpui::VisualTestContext) -> (chrono::NaiveDate, chrono::NaiveDate) {
        self.tile
            .read_with(vcx, |t, _| match t.popup() {
                Some(Popup::Range(r)) => Some((r.from.date(), r.to.date())),
                _ => None,
            })
            .expect("the range popup is open")
    }
    /// The range popup's inline refusal — a backwards range, an
    /// unfinished segment or the point cap.
    fn range_error(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Range(r)) => r.error.as_ref().map(|e| e.to_string()),
            _ => None,
        })
    }
    /// The open popup's own field, as it reads right now.
    fn input_text(&self, vcx: &gpui::VisualTestContext) -> String {
        self.tile
            .read_with(vcx, |t, cx| match t.popup() {
                Some(Popup::Picker(p)) => Some(p.input.read(cx).value().to_string()),
                Some(Popup::Expr(f)) => Some(f.input.read(cx).value().to_string()),
                _ => None,
            })
            .expect("a field popup is open")
    }
    /// Write the field the way nothing in the app does — through
    /// `InputState::set_value`, which emits NO `Change` event
    /// (CLAUDE.md's trap): every commit path has to re-feed the
    /// field's live text itself, and this is what proves it does.
    fn set_input_text(&self, vcx: &mut gpui::VisualTestContext, text: &str) {
        let text = text.to_string();
        vcx.update(|window, cx| {
            let input = self
                .tile
                .read(cx)
                .popup()
                .and_then(|p| match p {
                    Popup::Picker(p) => Some(p.input.clone()),
                    Popup::Expr(f) => Some(f.input.clone()),
                    Popup::Series(_) | Popup::Range(_) | Popup::Menu(_) => None,
                })
                .expect("a field popup is open");
            input.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
        });
    }
    /// One pair off the tile's live key context. `KeyContext` reads a
    /// pair BY KEY and offers no iterator over its pairs, so this
    /// takes the key rather than handing back a map — the assertion
    /// ("`popup` is `series`") is the same either way, and the shell
    /// keeps its own surface.
    fn key_context_pair(&self, vcx: &mut gpui::VisualTestContext, key: &str) -> Option<String> {
        self.tile.read_with(vcx, |_, cx| {
            self.content.key_context(cx).get(key).map(str::to_string)
        })
    }
    /// A real click on a PAINTED element, by debug selector: the
    /// bounds come out of the drawn frame, so a listener that is not
    /// actually wired to the element it looks wired to fails here.
    fn click(&self, vcx: &mut gpui::VisualTestContext, selector: &str) {
        let at = centre_of(vcx, selector);
        click_at(vcx, at, 1);
        self.draw(vcx);
    }
}

/// Paints the tile and hands back the centre of one painted element
/// by its debug selector (`geode-marketdata`'s own helper).
fn centre_of(vcx: &mut gpui::VisualTestContext, selector: &str) -> gpui::Point<gpui::Pixels> {
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    // `debug_bounds` wants a `&'static str`; a formatted selector is
    // not one, so it is leaked — a test-only cost, once per call.
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    vcx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is painted"))
        .center()
}

/// A left mouse-down/up pair at `at` carrying `click_count` — gpui's
/// own `simulate_click` hardwires a count of 1.
fn click_at(vcx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>, click_count: usize) {
    vcx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count,
    });
}

#[gpui::test]
fn the_factory_is_kind_timeseries_with_its_fragment_and_actions(cx: &mut gpui::TestAppContext) {
    let (h, _vcx) = open(cx);
    assert_eq!(h.factory.kind(), "timeseries");
    assert_eq!(h.factory.contexts(), vec!["timeseries"]);
    assert!(
        h.factory
            .default_keymap()
            .unwrap()
            .contains("timeseries && mode == normal")
    );
    let mut registry = ActionRegistry::default();
    h.factory.register_actions(&mut registry);
    for (id, _) in ACTIONS {
        assert!(registry.get(&ActionId(id.to_string())).is_some(), "{id}");
    }
    // Every key in the fragment names a registered action (the keymap
    // builder drops an unregistered one silently).
    let (docs, diags) = {
        let mut r = ModuleRoster::new();
        r.add(h.factory_handle());
        r.keymap_fragments()
    };
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(docs.len(), 1);
    // The keymap builder drops a binding whose action is
    // unregistered SILENTLY, so the fragment is checked against the
    // registry key by key rather than trusted.
    let fragment: toml::Table = toml::from_str(DEFAULT_KEYMAP).unwrap();
    let mut bound = 0;
    for group in fragment["bindings"].as_array().unwrap() {
        for (key, action) in group["keys"].as_table().unwrap() {
            let id = ActionId(action.as_str().unwrap().to_string());
            assert!(
                registry.get(&id).is_some(),
                "`{key}` binds an unregistered action: {id}"
            );
            bound += 1;
        }
    }
    assert!(
        bound >= ACTIONS.len(),
        "{bound} keys for {} actions",
        ACTIONS.len()
    );
    // And every key SPELLS. `check_fragment` only reads predicates,
    // and the two checks above only read action ids, so an
    // unparseable keystroke used to reach the running app and be
    // dropped there with an error diagnostic in the trader's
    // diagnostics tile — which is where `"+"` was found, on the
    // first `--demo` boot after the module was registered.
    // `build_keymap` over the real spliced docs is the production
    // path and the one that reports it.
    let docs = geode_shell::keymap::fragments::splice(
        &[geode_core::config::LayerDoc::builtin("keymap", "").unwrap()],
        &docs,
    );
    let (_, diags) =
        geode_shell::keymap::build_keymap(&docs, geode_shell::defaults::default_mod(), &registry);
    assert!(diags.is_empty(), "{diags:?}");
}

#[gpui::test]
fn a_fresh_tile_paints_the_empty_hint_and_its_title(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    assert_eq!(h.title(&mut vcx).as_ref(), "timeseries · 1y · 1d");
    assert!(
        h.painted_text(&mut vcx)
            .contains("no series — a adds one, x composes")
    );
    assert_eq!(h.key_context_mode(&mut vcx).as_str(), "normal");
}

#[gpui::test]
fn colon_add_makes_a_fetching_slot_and_the_header_chip(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    let m = h.model(&vcx);
    assert_eq!(m.slots().len(), 1);
    assert!(
        matches!(&m.slots()[0].kind, SlotKind::Source { source, identity, .. }
                if source == "demo_kdb" && identity == "SPX.close")
    );
    assert!(matches!(m.slots()[0].state, SlotState::Fetching));
    assert!(h.painted_text(&mut vcx).contains("SPX.close"), "the chip");
    assert!(h.painted_text(&mut vcx).contains("L"), "its axis letter");
    h.command(&mut vcx, "add VIX@demo_rest").unwrap();
    assert!(
        h.painted_text(&mut vcx).contains("VIX@demo_rest"),
        "source shown when not the default"
    );
    assert_eq!(
        h.title(&mut vcx).as_ref(),
        "timeseries · 1y · 1d · 2 series"
    );
}

#[gpui::test]
fn colon_add_without_a_default_source_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with_default_source(cx, None);
    assert_eq!(
        h.command(&mut vcx, "add SPX.close").unwrap_err(),
        "name a source or set a default: add SPX.close@<source>"
    );
    assert!(h.command(&mut vcx, "add SPX.close@demo_kdb").is_ok());
    assert_eq!(
        h.command(&mut vcx, "add X@nope").unwrap_err(),
        "'nope' is not a fetch source (have: demo_kdb, demo_rest)"
    );
}

#[gpui::test]
fn normal_mode_verbs_drive_the_model_and_bump_the_chart_version(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    let v0 = h.chart(&vcx).version;
    h.dispatch(&mut vcx, "axis_next", None);
    assert_eq!(h.model(&vcx).slots()[1].axis, Axis::Right);
    assert!(
        h.chart(&vcx).version > v0,
        "a chrome change rebuilds the chart model (§8.5's version contract)"
    );
    // A count steps the cycle that many times: from `Right`, two
    // steps is `BottomLeft` then `BottomRight` (the brief's `Left`
    // predates the four-axis cycle the model builds).
    h.dispatch(&mut vcx, "axis_next", Some(2));
    assert_eq!(h.model(&vcx).slots()[1].axis, Axis::BottomRight);
    h.dispatch(&mut vcx, "prev", None);
    assert_eq!(h.model(&vcx).cursor(), Some(0));
    h.dispatch(&mut vcx, "toggle_visible", None);
    assert!(!h.model(&vcx).slots()[0].visible);
    assert!(!h.chart(&vcx).slots[0].visible);
    h.dispatch(&mut vcx, "colour", None);
    assert_eq!(h.model(&vcx).slots()[0].colour, Colour::Palette(1));
    h.dispatch(&mut vcx, "rule", None);
    assert!(matches!(
        &h.model(&vcx).slots()[0].kind,
        SlotKind::Source {
            rule: BucketRule::First,
            ..
        }
    ));
    h.dispatch(&mut vcx, "split_shrink", None);
    assert!((h.model(&vcx).split() - 0.65).abs() < 1e-6);
    h.dispatch(&mut vcx, "density", None);
    assert_eq!(h.model(&vcx).density(), None);
    h.dispatch(&mut vcx, "percentiles", None);
    assert!(h.model(&vcx).percentiles().is_empty());
    h.dispatch(&mut vcx, "freq_coarser", None);
    assert_eq!(h.model(&vcx).frequency(), Frequency::W1);
    h.dispatch(&mut vcx, "freq_finer", Some(2));
    assert_eq!(h.model(&vcx).frequency(), Frequency::H1);
    assert_eq!(
        h.title(&mut vcx).as_ref(),
        "timeseries · 1y · 1h · 2 series"
    );
    let v = h.chart(&vcx).version;
    h.dispatch(&mut vcx, "zoom_in", None);
    assert!(
        h.chart(&vcx).version == v,
        "a view move does not rebuild the model — the element takes the view beside it"
    );
    h.dispatch(&mut vcx, "remove", None);
    assert_eq!(h.model(&vcx).slots().len(), 1);
}

#[gpui::test]
fn a_capped_frequency_step_is_refused_with_the_cap_message(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "freq 1h").unwrap();
    h.dispatch(&mut vcx, "freq_finer", Some(3));
    assert_eq!(h.model(&vcx).frequency(), Frequency::H1);
    assert!(h.notice(&vcx).unwrap().starts_with("1m over 1y is"));
    assert!(
        h.command(&mut vcx, "freq 1m")
            .unwrap_err()
            .contains("the cap is 500,000")
    );
}

#[gpui::test]
fn d_on_an_operand_removes_the_dependants_with_one_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    h.command(&mut vcx, "expr s1 / s2").unwrap();
    h.command(&mut vcx, "expr s3 * 100").unwrap();
    h.dispatch(&mut vcx, "prev", Some(2)); // cursor on s2
    h.dispatch(&mut vcx, "remove", None);
    let left: Vec<u8> = h.model(&vcx).slots().iter().map(|s| s.number).collect();
    assert_eq!(left, vec![1]);
    assert_eq!(
        h.notice(&vcx).as_deref(),
        Some("removed s2 and, with it, s3, s4")
    );
}

#[gpui::test]
fn the_edit_verb_on_a_source_slot_says_so(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.notice(&vcx).as_deref(), Some("s1 is not an expression"));
    // A verb this tile does not handle leaves the notice on screen:
    // clearing it in state while the old text is still painted is a
    // lie (review round 1, MIN-3).
    // (Every popup verb is handled somewhere, so the unhandled one
    // here is a list key with no list open — `popup_verb`'s guards
    // fall through to `false` exactly as an unrecognised verb does.)
    h.dispatch(&mut vcx, "list_down", None);
    assert_eq!(h.notice(&vcx).as_deref(), Some("s1 is not an expression"));
    // A handled one takes it away and speaks for itself.
    h.dispatch(&mut vcx, "next", None);
    assert_eq!(h.notice(&vcx), None);
}

#[gpui::test]
fn only_a_change_the_chart_model_reads_rebuilds_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    let v = h.chart(&vcx).version;
    // A cursor move, a chip click, a visibility change and a
    // finished fetch all touch the HEADER and nothing the chart
    // model carries — rebuilding one would clone every slot's
    // points and flush `geode-chart`'s path cache for an identical
    // model (review round 1, I-2).
    h.dispatch(&mut vcx, "next", None);
    h.dispatch(&mut vcx, "prev", None);
    assert_eq!(h.chart(&vcx).version, v, "a cursor move");
    vcx.update(|_, cx| h.content.set_visible(true, cx));
    assert_eq!(h.chart(&vcx).version, v, "a visibility change");
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Err("no route".into()));
    assert_eq!(h.chart(&vcx).version, v, "a fetch failure");
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header().chips[0].tone),
        geode_shell::shell::chip::Tone::Danger,
        "the header still moved"
    );
    // Everything the chart model DOES read still bumps it.
    h.dispatch(&mut vcx, "axis_next", None);
    let after_axis = h.chart(&vcx).version;
    assert!(after_axis > v, "an axis change");
    h.dispatch(&mut vcx, "colour", None);
    assert!(h.chart(&vcx).version > after_axis, "a colour change");
}

#[gpui::test]
fn a_reloaded_colours_doc_reaches_an_open_tile(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "colour s1 spx").unwrap();
    h.draw(&mut vcx);
    let before = h.swatch(&vcx);
    let version = h.chart(&vcx).version;
    // The factory's own door, as the app's bridge calls it on a
    // `ConfigReloaded`: it swaps a fresh `Arc` into the cell every
    // open tile shares, and the next frame is what notices.
    h.factory.set_colours(named_colours(180.0));
    h.draw(&mut vcx);
    assert_ne!(h.swatch(&vcx), before, "the chip's swatch follows");
    assert!(
        h.chart(&vcx).version > version,
        "and so does the line's colour, which lives in the chart model"
    );
    // The frame after that changes nothing.
    let settled = h.chart(&vcx).version;
    h.draw(&mut vcx);
    assert_eq!(h.chart(&vcx).version, settled);
}

#[gpui::test]
fn every_colon_command_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    let lines = [
        "add NKY.close",
        "expr s1 / s2",
        "remove s3",
        "rule s1 mean",
        "colour s1 2",
        "axis time",
        "freq 1h",
        "range 6m",
        "pct 10 90",
        "density 20",
        "yaxis s2 right",
        "split 0.6",
        "clear",
    ];
    for word in commands::VERBS {
        assert!(
            lines
                .iter()
                .any(|l| l.split_whitespace().next() == Some(word)),
            "no sweep line for `:{word}`"
        );
    }
    let before = h.frame.read_with(&vcx, |f, _| f.versions());
    for line in lines {
        assert!(commands::parse(line).is_ok(), "`{line}` no longer parses");
        let _ = vcx.update(|window, cx| h.content.command(line, window, cx));
        let after = h.frame.read_with(&vcx, |f, _| f.versions());
        assert_eq!(
            (after.scope, after.grouping, after.as_of),
            (before.scope, before.grouping, before.as_of),
            "`:{line}` moved the frame"
        );
        let persists = h.frame.update(&mut vcx, |f, _| {
            (f.take_pending_persist(), f.take_pending_scope_persist())
        });
        assert!(
            persists.0.is_none() && persists.1.is_none(),
            "`:{line}` asked the shell to persist something"
        );
        let (level, overlay) = h.diagnostics.update(&mut vcx, |d, _| {
            (d.take_pending_level(), d.take_pending_overlay_toggle())
        });
        assert!(level.is_none() && !overlay, "`:{line}` reached the app");
    }
}

#[gpui::test]
fn serialize_and_restore_round_trip_the_model(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX@demo_rest").unwrap();
    h.command(&mut vcx, "expr s1 / s2").unwrap();
    h.command(&mut vcx, "yaxis s2 bottomleft").unwrap();
    h.command(&mut vcx, "range 3m").unwrap();
    h.dispatch(&mut vcx, "zoom_in", None);
    let table = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(table.get("slots").unwrap().as_array().unwrap().len(), 3);
    assert!(
        table.get("view").is_none(),
        "the view is not persisted (§9.11)"
    );
    let (h2, mut vcx2) = open_with(cx, Some(table.clone()));
    let m = h2.model(&vcx2);
    assert_eq!(m.slots().len(), 3);
    assert_eq!(m.slots()[1].axis, Axis::BottomLeft);
    assert_eq!(m.slots()[2].text.as_deref(), Some("s1 / s2"));
    assert_eq!(m.range(), &Range::Relative(Preset::M3));
    assert_eq!(vcx2.update(|_, cx| h2.content.serialize(cx)), table);
    assert!(h2.painted_text(&mut vcx2).contains("s1 / s2"));
}

// ---- the data flow -----------------------------------------------

#[gpui::test]
fn an_add_fetches_the_resolved_range_when_visible_and_defers_while_hidden(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    assert!(h.fetch_request().is_none(), "hidden: nothing is asked");
    h.visible(&mut vcx, true);
    let f = h.fetch_request().expect("shown: the pending fetch");
    assert_eq!(
        (f.source.as_str(), f.identity.as_str(), f.key),
        ("demo_kdb", "SPX.close", QueryKey(TILE))
    );
    assert!((f.to - chrono::Utc::now()).num_seconds().abs() < 5);
    assert_eq!(
        f.to.checked_sub_months(chrono::Months::new(12)).unwrap(),
        f.from,
        "1y"
    );
    assert!(
        h.series_request().is_none(),
        "no query until the fetch answers"
    );
    h.command(&mut vcx, "add VIX").unwrap();
    let f = h.fetch_request().unwrap();
    assert_eq!(f.identity, "VIX");
    assert!(
        h.fetch_request().is_none(),
        "only the NEW slot: SPX's own fetch is still in flight"
    );
}

#[gpui::test]
fn a_fetched_ok_marks_the_pair_idle_and_queries_once_and_an_err_marks_it_failed(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add SPX.close").unwrap(); // a second slot of the same pair (§9.6)
    h.requests(); // drain the fetch
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(0));
    let m = h.model(&vcx);
    assert!(m.slots().iter().all(|s| s.state == SlotState::Idle));
    let q = h
        .series_request()
        .expect("Ok(0) still requeries: the span is covered");
    assert_eq!(q.series.len(), 2);
    assert_eq!(q.dataset, "series");
    assert!(
        h.series_request().is_none(),
        "ONE query for the pair, not one per slot"
    );
    h.deliver_fetched(&mut vcx, "demo_kdb", "NKY.close", Ok(9));
    assert!(
        h.series_request().is_none(),
        "a pair this tile does not hold is ignored"
    );
    h.deliver_fetched(
        &mut vcx,
        "demo_kdb",
        "SPX.close",
        Err("kdb: timeout".into()),
    );
    assert!(matches!(&h.model(&vcx).slots()[0].state, SlotState::Failed(e) if e == "kdb: timeout"));
    assert!(h.series_request().is_none(), "nothing is sent on Err");
}

#[gpui::test]
fn a_range_change_refetches_a_pair_whose_fetch_has_not_answered(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    let first = h.fetch_request().expect("the add's own fetch");
    // A frame notify carrying nothing this tile follows, while that
    // first fetch is still unanswered, must not re-ask for the same
    // span: `acted` is `None` — this
    // tile has never asked a QUERY — so `follows_changed` says true,
    // and the refetch trio hanging off it alone fired a duplicate
    // `Fetch` per pair on the first scope keystroke after a show.
    // The trio is gated on a REAL as-of move instead.
    h.frame.update(&mut vcx, |f, cx| {
        f.set_scope(geode_core::scope::Scope {
            text: Some("spx".into()),
            ..Default::default()
        });
        cx.notify();
    });
    assert!(
        h.fetch_request().is_none(),
        "a scope bump sends no second fetch"
    );
    // Deliberately unanswered: the pair is still in flight, and the
    // new range is a DIFFERENT span, so the in-flight set must not
    // suppress it (`in_flight_range`).
    h.command(&mut vcx, "range 1w").unwrap();
    let second = h
        .fetch_request()
        .expect("the new span is asked for, answered or not");
    assert_eq!(second.identity, "SPX.close");
    assert!(second.from > first.from, "a narrower span");
}

/// The as-of observer's own `in_flight.clear()`. `fetch_pending`
/// drops the in-flight set only when the RANGE moved, and an as-of
/// change leaves `Range` identical — so without the explicit clear a
/// pair whose FIRST fetch is still unanswered as the as-of moves
/// keeps its entry, and the new span (`AsOf::At(t)` resolves to
/// `t − preset .. t`, a different left edge) is never asked for at
/// all: the chart paints a truncated left edge with nothing on
/// screen to say so.
#[gpui::test]
fn an_as_of_change_refetches_a_pair_whose_fetch_has_not_answered(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    // The first pair ANSWERS, which is what gives the tile an
    // `acted` — the refetch trio is gated on a real as-of move
    // against one, so a tile that has never queried never reaches
    // the clear at all.
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    // The second is deliberately left in flight: its entry is what
    // the clear has to remove.
    h.command(&mut vcx, "add VIX").unwrap();
    h.requests();
    let at = chrono::Utc::now() - chrono::Duration::days(30);
    open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE)], at);
    let fetched: Vec<(String, chrono::DateTime<chrono::Utc>)> = h
        .requests()
        .into_iter()
        .filter_map(|r| match r {
            Request::Fetch(p) => Some((p.identity, p.to)),
            _ => None,
        })
        .collect();
    let vix = fetched
        .iter()
        .find(|(identity, _)| identity == "VIX")
        .expect(
            "the unanswered pair's new span is asked for, not suppressed by its stale \
in-flight entry",
        );
    assert!(vix.1 <= at, "over the as-of's own span");
    // And the answered pair, whose entry the answer already removed,
    // is asked for too — one fetch each, no more.
    assert_eq!(fetched.len(), 2, "one fetch per pair: {fetched:?}");
}

#[gpui::test]
fn a_removed_pair_leaves_no_ghost_and_a_re_add_fetches_again(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.command(&mut vcx, "remove s1").unwrap();
    // The answer lands for a pair this tile no longer holds — the
    // one path that used to leave an entry behind for ever.
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    h.command(&mut vcx, "add SPX.close").unwrap();
    let f = h
        .fetch_request()
        .expect("a re-added pair is fetched again, not skipped as in flight");
    assert_eq!(f.identity, "SPX.close");
    // The same, with `:clear` doing the removing and no answer at
    // all in between.
    h.command(&mut vcx, "clear").unwrap();
    h.requests();
    h.command(&mut vcx, "add SPX.close").unwrap();
    assert!(
        h.fetch_request().is_some(),
        "`:clear` drops the in-flight set with the slots"
    );
}

#[gpui::test]
fn a_delivery_becomes_the_chart_model_and_a_stale_tag_is_dropped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    let tag = h.series_request().unwrap().tag;
    h.deliver_series(&mut vcx, tag, result_with(&[1], 5));
    let c = h.chart(&vcx);
    assert_eq!(c.buckets.len(), 5);
    assert_eq!(c.slots[0].values.len(), 5);
    assert_eq!(
        (h.model(&vcx).view().lo, h.model(&vcx).view().hi),
        (0.0, 5.0),
        "the view is the whole range"
    );
    h.deliver_series(&mut vcx, tag - 1, result_with(&[1], 50));
    assert_eq!(h.chart(&vcx).buckets.len(), 5, "stale");
    // A stale tag is dropped AND does not arrive (review round 1,
    // MIN-2): the barrier is waiting for the NEWER request's own
    // answer, and an early arrival would release it — the second key
    // keeps it open so a real arrival is visibly different from
    // this.
    let at = chrono::Utc::now() - chrono::Duration::days(30);
    open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), QueryKey(99)], at);
    let fresh = h.series_request().expect("the as-of change requeried").tag;
    let acted = h.acted(&vcx).expect("the request recorded its versions");
    assert!(h.barrier_wants(&vcx, acted), "the barrier is open over it");
    h.frame.update(&mut vcx, |frame, cx| {
        frame.note_published(geode_shell::frame::Publish {
            dataset: "unrelated".into(),
            batch: "EOD".into(),
            books: 0,
            at: chrono::Utc::now(),
        });
        cx.notify();
    });
    assert!(
        h.series_request().is_none(),
        "a publication is not a series dependency"
    );
    assert!(
        h.barrier_wants(&vcx, acted),
        "an unrelated publication cannot answer an in-flight query"
    );
    h.deliver_series(&mut vcx, fresh - 1, result_with(&[1], 50));
    assert_eq!(h.chart(&vcx).buckets.len(), 5, "still stale");
    assert!(
        h.barrier_wants(&vcx, acted),
        "a stale delivery must not arrive: the answer it would stand in for is still in flight"
    );
    h.deliver_series_err(
        &mut vcx,
        fresh,
        "1m over 3y is 1,170,000 points; the cap is 500,000",
    );
    assert_eq!(h.chart(&vcx).buckets.len(), 5, "last good stays");
    assert!(h.notice(&vcx).unwrap().contains("the cap is 500,000"));
    assert!(
        !h.barrier_wants(&vcx, acted),
        "a FAILED outcome does arrive — one broken tile must not hold the rest to the deadline"
    );
}

#[gpui::test]
fn a_refused_submit_notices_and_still_answers_the_barrier(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    h.requests();
    // The data service is gone: every submit from here answers
    // `false`.
    h.close_channel();
    let at = chrono::Utc::now() - chrono::Duration::days(30);
    open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE)], at);
    assert!(
        h.notice(&vcx)
            .unwrap()
            .starts_with("series request refused"),
        "the refusal is named, not swallowed: {:?}",
        h.notice(&vcx)
    );
    assert!(
        !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
        "nothing is coming, so the tile arrives at once rather than \
             holding every other tile to the 250 ms deadline"
    );
    assert_eq!(
        h.acted(&vcx),
        None,
        "and it forgets it asked, so the next frame change retries"
    );
}

#[gpui::test]
fn a_query_change_requeries_and_a_range_change_fetches_and_queries(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    h.requests();
    h.dispatch(&mut vcx, "rule", None);
    let q = h.series_request().expect("a rule change is a query");
    assert!(matches!(
        q.series[0].kind,
        SlotKind::Source {
            rule: BucketRule::First,
            ..
        }
    ));
    h.dispatch(&mut vcx, "axis_next", None);
    assert!(h.series_request().is_none(), "an axis is chrome");
    // Hourly, so the buckets `result_with` builds are a frequency
    // step wide and a zoomed window lands strictly inside `1w`.
    h.command(&mut vcx, "freq 1h").unwrap();
    h.requests();
    h.command(&mut vcx, "range 1w").unwrap();
    let reqs = h.requests();
    assert!(
        matches!(reqs[0], Request::Fetch(_)),
        "the range fetches the gaps…"
    );
    assert!(
        matches!(reqs[1], Request::Series(_)),
        "…and queries the cached part at once (§9.10)"
    );
    // Stats over the visible window: a pan requeries while
    // percentiles are on.
    h.deliver_series(&mut vcx, h_last_tag(&reqs), result_with(&[1], 20));
    h.dispatch(&mut vcx, "zoom_in", None);
    let q = h.series_request().expect("stats follow the view");
    assert!(q.window.0 > q.range.0 && q.window.1 < q.range.1);
    h.dispatch(&mut vcx, "percentiles", None);
    h.series_request().unwrap();
    h.dispatch(&mut vcx, "density", None);
    h.series_request().unwrap();
    h.dispatch(&mut vcx, "zoom_out", None);
    assert!(
        h.series_request().is_none(),
        "with both off, a view move asks nothing"
    );
}

#[gpui::test]
fn the_tile_follows_as_of_only_and_stages_under_an_open_barrier(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    let tag = h.series_request().unwrap().tag;
    h.deliver_series(&mut vcx, tag, result_with(&[1], 5));
    // A scope bump: nothing, and it must not hold the barrier either.
    h.frame.update(&mut vcx, |f, cx| {
        f.set_scope(geode_core::scope::Scope {
            text: Some("spx".into()),
            ..Default::default()
        });
        f.open_flip([QueryKey(TILE)], std::time::Instant::now());
        cx.notify();
    });
    assert!(h.series_request().is_none(), "scope is not followed");
    assert!(
        !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
        "and the tile self-arrives rather than holding every other tile to the deadline"
    );
    // An as-of change opens a barrier over this tile and requeries.
    let at = chrono::Utc::now() - chrono::Duration::days(30);
    open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE)], at);
    let reqs = h.requests();
    // The as-of moves the span's LEFT edge too, and live fetching
    // never covered anything before `now − preset` (review round 1,
    // I-1): the gaps are asked for before the points are.
    let Request::Fetch(f) = &reqs[0] else {
        panic!("an as-of change refetches first: {reqs:?}");
    };
    assert_eq!(
        (f.source.as_str(), f.identity.as_str()),
        ("demo_kdb", "SPX.close")
    );
    assert!(f.to <= at, "over the as-of's own span");
    assert_eq!(
        reqs.iter()
            .filter(|r| matches!(r, Request::Fetch(_)))
            .count(),
        1,
        "one fetch per pair"
    );
    let Some(Request::Series(q)) = reqs.into_iter().nth(1) else {
        panic!("…and then asks");
    };
    assert_eq!(q.as_of, AsOf::At(at));
    assert!(q.range.1 <= at, "the as-of clips the visible end");
    h.deliver_series(&mut vcx, q.tag, result_with(&[1], 3));
    assert_eq!(
        h.chart(&vcx).buckets.len(),
        3,
        "the only awaited tile: arriving released the barrier and promoted at once"
    );
    // With a second awaited key the delivery is STAGED until the flip.
    open_barrier_on_as_of(
        &h,
        &mut vcx,
        &[QueryKey(TILE), QueryKey(99)],
        at - chrono::Duration::days(1),
    );
    let q = h.series_request().unwrap();
    h.deliver_series(&mut vcx, q.tag, result_with(&[1], 7));
    assert_eq!(h.chart(&vcx).buckets.len(), 3, "staged");
    h.frame.update(&mut vcx, |f, cx| {
        f.sweep(std::time::Instant::now() + geode_shell::frame::FLIP_DEADLINE);
        cx.notify();
    });
    assert_eq!(h.chart(&vcx).buckets.len(), 7, "promoted on the flip");
}

#[gpui::test]
fn a_hidden_tile_cancels_and_a_shown_one_requeries_and_a_restored_one_refetches_once(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    let tag = h.series_request().unwrap().tag;
    // A tile with a result on screen is what a hide/show round trip
    // is about; a tile that has never been answered comes back
    // through its fetch, which the restore half below pins.
    h.deliver_series(&mut vcx, tag, result_with(&[1], 5));
    h.requests();
    h.visible(&mut vcx, false);
    assert!(
        matches!(h.raw_requests().last(), Some(Request::Cancel { key }) if *key == QueryKey(TILE))
    );
    h.visible(&mut vcx, true);
    let reqs = h.requests();
    assert!(
        reqs.iter().any(|r| matches!(r, Request::Fetch(_))),
        "shown: refetch (§9.10)…"
    );
    assert!(
        reqs.iter().any(|r| matches!(r, Request::Series(_))),
        "…and requery"
    );
    let table = vcx.update(|_, cx| h.content.serialize(cx));
    let (h2, mut vcx2) = open_with(cx, Some(table));
    assert!(
        matches!(h2.model(&vcx2).slots()[0].state, SlotState::Idle),
        "restored: not yet asked"
    );
    h2.visible(&mut vcx2, true);
    assert!(matches!(
        h2.model(&vcx2).slots()[0].state,
        SlotState::Fetching
    ));
    let f = h2.fetch_request().expect("a restored tile refetches once");
    assert_eq!(f.identity, "SPX.close");
    assert!(h2.fetch_request().is_none());
    h2.visible(&mut vcx2, false);
    h2.visible(&mut vcx2, true);
    assert!(
        h2.fetch_request().is_some(),
        "every show refetches (coverage subtraction makes it cheap)"
    );
}

#[gpui::test]
fn shift_l_opens_the_series_popup_whose_cursor_is_the_chips_cursor(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX@demo_rest").unwrap();
    h.command(&mut vcx, "rule s2 mean").unwrap();
    h.dispatch(&mut vcx, "list", None);
    assert!(h.popup_is_series(&vcx));
    assert_eq!(
        h.key_context_pair(&mut vcx, "popup").as_deref(),
        Some("series")
    );
    assert_eq!(
        h.key_context_mode(&mut vcx),
        "normal",
        "the list holds no field: normal mode with a popup pair"
    );
    let rows = h.series_rows(&vcx);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].label.as_ref(), "VIX@demo_rest");
    assert_eq!(rows[1].source_rule.as_ref(), "demo_rest · mean");
    assert_eq!(rows[1].axis, "L");
    assert_eq!(rows[1].state.as_ref(), "fetching");
    h.dispatch(&mut vcx, "list_up", None);
    assert_eq!(h.model(&vcx).cursor(), Some(0));
    h.dispatch(&mut vcx, "list_down", Some(3));
    assert_eq!(h.model(&vcx).cursor(), Some(1), "wraps like the chips");
    h.dispatch(&mut vcx, "axis_next", None);
    assert!(h.popup_is_series(&vcx), "a popup verb keeps it open");
    assert_eq!(h.series_rows(&vcx)[1].axis, "R");
    h.dispatch(&mut vcx, "pan_left", None);
    assert!(!h.popup_is_series(&vcx), "any other verb closes it first");
    h.dispatch(&mut vcx, "list", None);
    h.dispatch(&mut vcx, "list_close", None);
    assert!(h.popup_is_none(&vcx));
    assert_eq!(
        h.key_context_pair(&mut vcx, "popup"),
        None,
        "the pair goes with the popup: `j` is nothing again"
    );
    h.dispatch(&mut vcx, "list", None);
    assert!(h.popup_is_series(&vcx));
    h.dispatch(&mut vcx, "list", None);
    assert!(h.popup_is_none(&vcx), "a second L closes it");
}

#[gpui::test]
fn l_on_an_empty_model_opens_the_list_on_its_empty_hint(cx: &mut gpui::TestAppContext) {
    // `L` does not refuse on an empty tile: the list opens, paints
    // the hint naming the keys that end the state, and stays open —
    // a popup that refused would leave the trader nothing to read.
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "list", None);
    assert!(h.popup_is_series(&vcx));
    assert!(h.series_rows(&vcx).is_empty());
    // Painted, not merely in state: `centre_of` panics on an element
    // the frame does not carry.
    let _ = centre_of(&mut vcx, &format!("ts-list-{TILE}"));
    h.dispatch(&mut vcx, "list_close", None);
    assert!(h.popup_is_none(&vcx));
}

#[gpui::test]
fn a_chip_click_moves_the_cursor_without_opening_the_popup_and_a_row_click_moves_it_too(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    // The chips are keyed by SLOT NUMBER, not by index: the first
    // chip is `s1`.
    h.click(&mut vcx, &format!("timeseries-chip-{TILE}-1"));
    assert_eq!(h.model(&vcx).cursor(), Some(0));
    assert!(h.popup_is_none(&vcx));
    h.dispatch(&mut vcx, "list", None);
    h.click(&mut vcx, &format!("ts-list-row-{TILE}-1"));
    assert_eq!(h.model(&vcx).cursor(), Some(1));
    assert!(
        h.popup_is_series(&vcx),
        "a row click moves the cursor and keeps the list"
    );
}

#[gpui::test]
fn chip_tones_are_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
    // The chips paint through `chip_paint` (`Neutral` cursor,
    // `Warning` fetching, `Danger` failed), which the shell already
    // sweeps; this pins that THIS module uses those tones and no
    // other.
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header().chips[0].tone),
        geode_shell::shell::chip::Tone::Warning
    );
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Err("no route".into()));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header().chips[0].tone),
        geode_shell::shell::chip::Tone::Danger
    );
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(3));
    assert_eq!(
        h.tile.read_with(&vcx, |t, _| t.header().chips[0].tone),
        geode_shell::shell::chip::Tone::Neutral,
        "idle AND the cursor"
    );
    // Every tone this module can paint is one of those three — no
    // fourth tone, and no hand-rolled `warning_foreground` over a
    // tint.
    h.command(&mut vcx, "add VIX").unwrap();
    let tones = h.tile.read_with(&vcx, |t, _| {
        t.header().chips.iter().map(|c| c.tone).collect::<Vec<_>>()
    });
    for tone in tones {
        assert!(
            matches!(
                tone,
                geode_shell::shell::chip::Tone::Warning
                    | geode_shell::shell::chip::Tone::Danger
                    | geode_shell::shell::chip::Tone::Neutral
            ),
            "{tone:?}"
        );
    }
}

// ---- the picker and the expression field (spec §9.6, §9.7) --------

#[gpui::test]
fn a_opens_the_picker_over_the_catalogue_and_enter_adds_the_highlighted_pair(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    seed_catalog(
        &h,
        &mut vcx,
        &[("demo_kdb", &["SPX.close", "VIX", "NKY.close"])],
    ); // demo_rest has no catalogue
    h.visible(&mut vcx, true);
    h.dispatch(&mut vcx, "add", None);
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
    assert!(
        vcx.update(|w, cx| h.content.holds_focus(w, cx)),
        "the picker's field holds the keyboard"
    );
    assert_eq!(
        h.picker_rows(&vcx),
        vec!["NKY.close@demo_kdb", "SPX.close@demo_kdb", "VIX@demo_kdb"],
        "identity first, sorted, every fetch source with a catalogue"
    );
    // The field has to be PAINTED before a keystroke can reach it:
    // gpui dispatches against the focus path of the last frame.
    h.draw(&mut vcx);
    vcx.simulate_input("vi");
    assert_eq!(h.picker_rows(&vcx)[0], "VIX@demo_kdb");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.key_context_mode(&mut vcx), "normal");
    assert!(
        !vcx.update(|w, cx| h.content.holds_focus(w, cx)),
        "blurred before the drop"
    );
    assert!(h.model(&vcx).holds_pair("demo_kdb", "VIX"));
    assert!(h.fetch_request().is_some());
    // The loaded row is marked and still pickable.
    h.dispatch(&mut vcx, "add", None);
    assert!(h.picker_loaded_marks(&vcx).contains(&true));
    h.dispatch(&mut vcx, "cancel", None);
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.model(&vcx).slots().len(), 1);
}

#[gpui::test]
fn an_unmatched_text_offers_the_add_row_which_opens_the_source_stage(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    seed_catalog(&h, &mut vcx, &[("demo_kdb", &["SPX.close"])]);
    h.dispatch(&mut vcx, "add", None);
    h.draw(&mut vcx);
    vcx.simulate_input("/v1/px?sym=SPX");
    assert!(h.picker_rows(&vcx).is_empty());
    assert_eq!(
        h.picker_add_row(&vcx).as_deref(),
        Some("add \"/v1/px?sym=SPX\"…")
    );
    // Painted, not merely in state — `centre_of` panics on an
    // element the frame does not carry.
    let _ = centre_of(&mut vcx, &format!("ts-picker-add-{TILE}"));
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.picker_is_source_stage(&vcx));
    let _ = centre_of(&mut vcx, &format!("ts-picker-row-{TILE}-0"));
    assert_eq!(h.picker_rows(&vcx), vec!["demo_kdb", "demo_rest"]);
    assert_eq!(
        h.picker_highlighted(&vcx),
        "demo_kdb",
        "the default source is highlighted"
    );
    assert_eq!(h.input_text(&vcx), "", "the field is cleared for the stage");
    assert!(
        vcx.update(|w, cx| h.content.holds_focus(w, cx)),
        "and it keeps the keyboard across the stage"
    );
    h.dispatch(&mut vcx, "insert_down", None);
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.model(&vcx).holds_pair("demo_rest", "/v1/px?sym=SPX"));
    // A text already spelled identity@known-source skips the stage.
    h.dispatch(&mut vcx, "add", None);
    h.draw(&mut vcx);
    vcx.simulate_input("EURUSD@demo_rest");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.model(&vcx).holds_pair("demo_rest", "EURUSD"));
    assert!(h.popup_is_none(&vcx));
}

#[gpui::test]
fn opening_the_picker_asks_for_a_catalog_and_re_ranks_when_it_lands(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "add", None);
    assert!(
        h.diagnostics
            .update(&mut vcx, |d, _| d.take_pending_catalog_request()),
        "no catalog yet: one is requested (with cx.notify, the CLAUDE.md trap)"
    );
    assert!(h.picker_rows(&vcx).is_empty());
    seed_catalog(&h, &mut vcx, &[("demo_kdb", &["VIX"])]);
    assert_eq!(
        h.picker_rows(&vcx),
        vec!["VIX@demo_kdb"],
        "the open picker re-ranked on the Diagnostics notify"
    );
}

#[gpui::test]
fn x_opens_the_expression_field_and_enter_adds_or_reports_inline(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    h.dispatch(&mut vcx, "expr", None);
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
    h.draw(&mut vcx);
    vcx.simulate_input("s1 ^ s2");
    assert!(
        !h.dispatch_handled(&mut vcx, "commit", None),
        "a parse error is UNHANDLED, like an inert enter: the field              stays open and `dispatch`'s tail puts a standing notice back"
    );
    assert!(h.popup_is_expr(&vcx), "a parse error keeps the field open");
    assert!(
        vcx.update(|w, cx| h.content.holds_focus(w, cx)),
        "and keeps the keyboard, so the text can be corrected in place"
    );
    assert!(h.expr_error(&vcx).unwrap().contains("arithmetic only"));
    assert_eq!(h.model(&vcx).slots().len(), 2);
    h.set_input_text(&mut vcx, "s1 / s2");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.model(&vcx).slots()[2].text.as_deref(), Some("s1 / s2"));
    // `e` reopens the cursor's expression prefilled; `escape`
    // discards.
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.input_text(&vcx), "s1 / s2");
    h.draw(&mut vcx);
    vcx.simulate_input(" * 2");
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(
        h.model(&vcx).slots()[2].text.as_deref(),
        Some("s1 / s2"),
        "escape discards"
    );
    h.dispatch(&mut vcx, "edit", None);
    h.set_input_text(&mut vcx, "s1 - s2");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(
        h.model(&vcx).slots()[2].text.as_deref(),
        Some("s1 - s2"),
        "replaced in place, same number"
    );
    assert_eq!(h.model(&vcx).slots()[2].number, 3);
    h.dispatch(&mut vcx, "prev", None);
    h.dispatch(&mut vcx, "edit", None);
    assert!(h.popup_is_none(&vcx), "`e` on a source slot does nothing");
    assert_eq!(h.notice(&vcx).as_deref(), Some("s2 is not an expression"));
}

/// The palette can dispatch any action over an open field (`ctrl+k`
/// is a chord, so it opens with the picker up, and `commit_selected`
/// closes the palette and dispatches). A verb that ran with the
/// field still installed would leave `key_context` reporting
/// `insert` with nothing focused — a tile deaf to every bare key
/// until `escape`.
#[gpui::test]
fn any_verb_but_the_fields_own_four_closes_an_insert_popup_first(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    let before = h.model(&vcx).slots()[0].colour.clone();
    h.dispatch(&mut vcx, "add", None);
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
    // The palette's path: an action the picker has no verb for.
    h.dispatch(&mut vcx, "colour", None);
    assert!(h.popup_is_none(&vcx), "the picker closed first");
    assert_eq!(h.key_context_mode(&mut vcx), "normal");
    assert!(!vcx.update(|w, cx| h.content.holds_focus(w, cx)));
    assert_ne!(
        h.model(&vcx).slots()[0].colour,
        before,
        "and the verb itself ran"
    );
    // The series list, which holds no field, is unchanged: it stays
    // open through exactly the verbs it always did (spec §9.5).
    h.dispatch(&mut vcx, "list", None);
    h.dispatch(&mut vcx, "colour", None);
    assert!(h.popup_is_series(&vcx));
}

/// A desk that has configured no fetch source at all: the second
/// stage would have nothing to choose from, so the commit says why
/// and closes rather than parking the trader in a dead list.
#[gpui::test]
fn the_source_stage_refuses_when_no_fetch_source_is_configured(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_without_sources(cx);
    h.dispatch(&mut vcx, "add", None);
    h.draw(&mut vcx);
    vcx.simulate_input("VIX");
    assert_eq!(h.picker_add_row(&vcx).as_deref(), Some("add \"VIX\"…"));
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.popup_is_none(&vcx));
    assert_eq!(
        h.notice(&vcx).as_deref(),
        Some("no fetch source is configured")
    );
    assert!(h.model(&vcx).slots().is_empty());
}

/// A query of nothing but spaces ranks nothing, but names no
/// identity either: no add row, and `enter` is UNHANDLED, so a
/// notice already on screen survives it.
#[gpui::test]
fn a_blank_query_offers_no_add_row_and_an_inert_enter_keeps_the_notice(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.notice(&vcx).as_deref(), Some("s1 is not an expression"));
    h.dispatch(&mut vcx, "add", None);
    h.draw(&mut vcx);
    vcx.simulate_input("   ");
    assert_eq!(h.picker_add_row(&vcx), None, "spaces name no identity");
    assert!(h.picker_rows(&vcx).is_empty());
    // UNHANDLED: `dispatch`'s own tail is what puts a standing
    // notice back, and it only does so for a verb that answers
    // `false`.
    assert!(!h.dispatch_handled(&mut vcx, "commit", None));
    assert!(
        h.tile.read_with(&vcx, |t, _| t.popup().is_some()),
        "an inert enter leaves the picker open"
    );
    assert_eq!(h.model(&vcx).slots().len(), 1, "and adds nothing");
    assert!(
        h.dispatch_handled(&mut vcx, "cancel", None),
        "while a verb the field does own is handled"
    );
}

/// One keystroke, two halves: `L` over the picker closes the field
/// (through the ONE closer, so the keyboard comes back) and opens
/// the list — the gate runs first and `popup_verb` then sees an
/// empty slot.
#[gpui::test]
fn l_over_an_open_picker_closes_it_and_opens_the_series_list(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.dispatch(&mut vcx, "add", None);
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
    h.dispatch(&mut vcx, "list", None);
    assert!(
        h.popup_is_series(&vcx),
        "the picker closed and the list opened in one keystroke"
    );
    assert_eq!(h.key_context_mode(&mut vcx), "normal");
    assert!(
        !vcx.update(|w, cx| h.content.holds_focus(w, cx)),
        "blurred on the way through"
    );
}

// ---- the range popup (spec §9.8) ---------------------------------

#[gpui::test]
fn r_opens_the_range_popup_on_from_day_and_a_digit_commits_a_preset(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.visible(&mut vcx, true);
    h.requests();
    h.dispatch(&mut vcx, "range", None);
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
    assert!(vcx.update(|w, cx| h.content.holds_focus(w, cx)));
    assert_eq!(h.range_active_segment(&vcx), (Which::From, Segment::Day));
    // The popup has to be PAINTED before a keystroke can reach its
    // own listener: gpui dispatches against the LAST frame's focus
    // path (the picker tests' own rule).
    h.draw(&mut vcx);
    vcx.simulate_keystrokes("3");
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.model(&vcx).range(), &Range::Relative(Preset::M3));
    assert!(
        h.requests().iter().any(|r| matches!(r, Request::Fetch(_))),
        "a committed range fetches"
    );
}

#[gpui::test]
fn tab_moves_between_the_fields_and_enter_commits_an_absolute_range(cx: &mut gpui::TestAppContext) {
    use chrono::Datelike as _;
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    // `from` opens on today minus the current preset; type a year.
    vcx.simulate_keystrokes("left left"); // year segment
    assert_eq!(h.range_active_segment(&vcx).1, Segment::Year);
    vcx.simulate_keystrokes("2 0 2 6");
    vcx.simulate_keystrokes("tab");
    assert_eq!(h.range_active_segment(&vcx).0, Which::To);
    vcx.simulate_keystrokes("shift-tab");
    assert_eq!(h.range_active_segment(&vcx).0, Which::From);
    vcx.simulate_keystrokes("enter");
    assert!(h.popup_is_none(&vcx));
    assert!(matches!(h.model(&vcx).range(), Range::Absolute { from, .. } if from.year() == 2026));
    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    vcx.simulate_keystrokes("escape");
    assert!(h.popup_is_none(&vcx));
    assert!(!vcx.update(|w, cx| h.content.holds_focus(w, cx)));
}

#[gpui::test]
fn a_preset_click_commits_at_once_and_a_backwards_range_is_refused_inline(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    // Not the default preset: a click that did nothing at all would
    // otherwise leave `1y` standing and pass.
    h.command(&mut vcx, "range 3m").unwrap();
    h.dispatch(&mut vcx, "range", None);
    h.click(&mut vcx, &format!("ts-range-preset-{TILE}-1y"));
    assert_eq!(h.model(&vcx).range(), &Range::Relative(Preset::Y1));
    assert!(h.popup_is_none(&vcx), "a preset click commits and closes");
    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    // Move `to` before `from` and commit.
    vcx.simulate_keystrokes("tab");
    vcx.simulate_keystrokes("left left");
    vcx.simulate_keystrokes("1 9 9 0");
    vcx.simulate_keystrokes("enter");
    assert!(h.popup_is_range(&vcx), "refused: still open");
    assert!(h.range_error(&vcx).unwrap().contains("before"));
    assert_eq!(
        h.model(&vcx).range(),
        &Range::Relative(Preset::Y1),
        "and nothing was written"
    );
}

/// `enter` over a segment still mid-entry that cannot stand alone:
/// refused inline, naming the field and the segment, with the popup
/// still open on the date that caused it.
#[gpui::test]
fn an_unfinished_segment_is_refused_inline_and_r_reopens_on_a_fresh_seed(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    // `0` is no preset (`Preset::digit` has no zero), so it types —
    // and a day of `0` waits for a second digit it never gets.
    vcx.simulate_keystrokes("0");
    assert_eq!(h.range_active_segment(&vcx), (Which::From, Segment::Day));
    vcx.simulate_keystrokes("enter");
    assert!(h.popup_is_range(&vcx), "refused: still open");
    let error = h.range_error(&vcx).expect("named");
    assert!(error.contains("day") && error.contains("from"), "{error}");
    // A keystroke answers a refusal about a date that has moved on.
    vcx.simulate_keystrokes("backspace");
    assert_eq!(h.range_error(&vcx), None);
    // `r` over the open popup REOPENS it on a fresh seed rather than
    // toggling it shut: it is an insert popup, so `dispatch`'s gate
    // closes it before the arm runs (spec §9.8 gives `r` no toggle;
    // `escape` is the close).
    vcx.simulate_keystrokes("left");
    assert_eq!(h.range_active_segment(&vcx).1, Segment::Month);
    h.dispatch(&mut vcx, "range", None);
    assert_eq!(
        h.range_active_segment(&vcx),
        (Which::From, Segment::Day),
        "reopened on the day of a fresh seed"
    );
}

/// `edited` is what turns the preset digits off, so
/// only a keystroke that actually MOVED something may set it — a key
/// that did nothing must leave the presets reachable. Two such keys,
/// both of which used to disable them:
///
/// - `right` on `from`'s day, already the last segment under
///   `Precision::Date` (`DateTimeField::apply` answers `false`), and
/// - `tab`, which `RangePopup::switch` has always documented as "a
///   trader who tabbed over to read the other date has typed nothing"
///   — pinned here, since nothing else would notice it starting to
///   count.
#[gpui::test]
fn a_key_that_moves_nothing_leaves_the_preset_digits_live(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    vcx.simulate_keystrokes("right");
    assert_eq!(
        h.range_active_segment(&vcx),
        (Which::From, Segment::Day),
        "`right` on the last segment moves nothing"
    );
    vcx.simulate_keystrokes("3");
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.model(&vcx).range(), &Range::Relative(Preset::M3));

    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    vcx.simulate_keystrokes("tab");
    assert_eq!(h.range_active_segment(&vcx).0, Which::To);
    vcx.simulate_keystrokes("4");
    assert!(h.popup_is_none(&vcx), "`tab` is not an edit either");
    assert_eq!(h.model(&vcx).range(), &Range::Relative(Preset::M6));
}

/// `tab` answers a standing refusal, exactly as every other field key
/// does (`apply_range_key`'s own rule): the error names a date, and
/// the trader has just moved the keyboard onto the other one.
#[gpui::test]
fn tab_clears_the_inline_error(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    vcx.simulate_keystrokes("0"); // no preset; a day mid-entry
    vcx.simulate_keystrokes("enter");
    assert!(h.range_error(&vcx).is_some(), "fixture check: refused");
    vcx.simulate_keystrokes("tab");
    assert_eq!(h.range_error(&vcx), None);
}

/// Ruling: an `Absolute` range seeds the popup from
/// its STORED dates, as typed, and only a `Relative` one resolves
/// against now/as-of — so reopening `r` under a historical as-of, or
/// after the clock has rolled over midnight, is lossless. Seeding
/// both through `resolve` used to round-trip an absolute range
/// through a half-open end and back.
#[gpui::test]
fn an_absolute_range_reopens_on_the_dates_it_stores(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "range 2026-01-05 2026-02-05").unwrap();
    h.dispatch(&mut vcx, "range", None);
    let (from, to) = h.range_dates(&vcx);
    assert_eq!(
        (from.to_string(), to.to_string()),
        ("2026-01-05".to_string(), "2026-02-05".to_string())
    );
    h.dispatch(&mut vcx, "cancel", None);

    // The case the ruling is actually about: an as-of INSIDE the
    // stored span. `Range::resolve` clips its end to the as-of by
    // design (ruling 4) — which is right for what is fetched and
    // queried, and wrong for what the popup seeds: a trader who
    // opened `r` here and pressed `enter` would silently have their
    // `to` rewritten to the as-of's own day.
    h.frame.update(&mut vcx, |f, cx| {
        f.set_as_of(AsOf::At(
            "2026-01-20T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
        ));
        cx.notify();
    });
    h.dispatch(&mut vcx, "range", None);
    let (from, to) = h.range_dates(&vcx);
    assert_eq!(
        (from.to_string(), to.to_string()),
        ("2026-01-05".to_string(), "2026-02-05".to_string()),
        "an absolute range seeds from its stored dates, as-of or not"
    );
}

/// The keymap path and the listener path must agree about what an
/// arrow means: `timeseries::insert_up` (the `mode == insert`
/// fragment's `up`) and the listener's own `up`/`down` are the same
/// `FieldKey::Step`.
#[gpui::test]
fn an_arrow_steps_the_active_segment_through_either_door(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    let (from, to) = h.range_dates(&vcx);
    h.dispatch(&mut vcx, "insert_up", None);
    assert_eq!(
        h.range_dates(&vcx),
        (from + chrono::Duration::days(1), to),
        "the keymap's up steps the active field's day"
    );
    vcx.simulate_keystrokes("down down");
    assert_eq!(
        h.range_dates(&vcx),
        (from - chrono::Duration::days(1), to),
        "and the listener's down steps it the same way"
    );
}

#[gpui::test]
fn every_closer_blurs_before_dropping_the_focused_handle(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "add", None);
    assert!(vcx.update(|w, cx| w.focused(cx).is_some()));
    h.dispatch(&mut vcx, "cancel", None);
    assert!(
        vcx.update(|w, cx| w.focused(cx).is_none()),
        "picker: blurred, then dropped"
    );
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.dispatch(&mut vcx, "expr", None);
    assert!(vcx.update(|w, cx| w.focused(cx).is_some()));
    h.dispatch(&mut vcx, "cancel", None);
    assert!(
        vcx.update(|w, cx| w.focused(cx).is_none()),
        "expression field: blurred, then dropped"
    );
    // The range popup holds a bare handle rather than an
    // `InputState`, and the rule is the same: an unblurred dead
    // handle leaves `Window::focused` pointing at nothing and the
    // shell's own focus-return net never fires (CLAUDE.md).
    h.dispatch(&mut vcx, "range", None);
    assert!(vcx.update(|w, cx| w.focused(cx).is_some()));
    h.dispatch(&mut vcx, "cancel", None);
    assert!(
        vcx.update(|w, cx| w.focused(cx).is_none()),
        "range popup: blurred, then dropped"
    );
    // …and through the listener's own `escape`, the other door.
    h.dispatch(&mut vcx, "range", None);
    h.draw(&mut vcx);
    vcx.simulate_keystrokes("escape");
    assert!(
        vcx.update(|w, cx| w.focused(cx).is_none()),
        "range popup: the listener's escape takes the same closer"
    );
}

/// The chart's `offset_secs` is the APP clock's (`[time] zone`), never
/// the machine's: installing a Tokyo clock as `AppClock` moves the
/// offset to +9h and rebuilds the model (the offset is a `ChartKey`
/// input), so a zone reload repaints the axis labels.
#[gpui::test]
fn the_chart_offset_follows_the_app_clock(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    let before = h.chart(&vcx);
    vcx.update(|_, cx| {
        cx.set_global(geode_shell::clock::AppClock(
            geode_core::clock::Clock::in_zone_named("Asia/Tokyo"),
        ));
    });
    let after = h.chart(&vcx);
    assert_eq!(after.offset_secs, 9 * 3600, "Tokyo has no DST");
    assert!(
        after.version > before.version,
        "a moved offset is a real rebuild"
    );
    assert_ne!(before.offset_secs, after.offset_secs);
}

// ---- the mouse pass (2026-09-24) ---------------------------------

/// A loaded tile: one source, one delivered result of `n` hourly
/// buckets, painted once so the chart surface has bounds.
fn open_loaded(cx: &mut gpui::TestAppContext, n: usize) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    let tag = h.series_request().unwrap().tag;
    h.deliver_series(&mut vcx, tag, result_with(&[1], n));
    h.draw(&mut vcx);
    (h, vcx)
}

impl Harness {
    fn drag(&self, vcx: &gpui::VisualTestContext) -> Option<Drag> {
        self.tile.read_with(vcx, |t, _| t.drag())
    }
    fn popup_is_menu(&self, vcx: &gpui::VisualTestContext) -> bool {
        self.tile
            .read_with(vcx, |t, _| matches!(t.popup(), Some(Popup::Menu(_))))
    }
    /// The menu's rows as `(title, highlighted)` — its own "painted
    /// text".
    fn menu_rows(&self, vcx: &gpui::VisualTestContext) -> Vec<(String, bool)> {
        self.tile.read_with(vcx, |t, _| match t.popup() {
            Some(Popup::Menu(m)) => m
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let title = match r {
                        menu::MenuRow::Action { title, .. } => title.to_string(),
                        menu::MenuRow::Separator => "---".into(),
                        menu::MenuRow::Section(s) => format!("[{s}]"),
                    };
                    (title, i == m.highlighted)
                })
                .collect(),
            _ => Vec::new(),
        })
    }
    fn menu_row_index(&self, vcx: &gpui::VisualTestContext, title: &str) -> usize {
        self.menu_rows(vcx)
            .iter()
            .position(|(t, _)| t == title)
            .unwrap_or_else(|| panic!("{title} is a menu row"))
    }
    fn view(&self, vcx: &gpui::VisualTestContext) -> (f64, f64) {
        let v = self.model(vcx).view();
        (v.lo, v.hi)
    }
    /// A real right-button press and release on a painted element.
    fn right_click(&self, vcx: &mut gpui::VisualTestContext, selector: &str) {
        let at = centre_of(vcx, selector);
        vcx.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Right,
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Right,
            click_count: 1,
        });
        self.draw(vcx);
    }
    fn wheel(
        &self,
        vcx: &mut gpui::VisualTestContext,
        at: gpui::Point<gpui::Pixels>,
        dx: f32,
        dy: f32,
    ) {
        vcx.simulate_event(gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(dx), gpui::px(dy))),
            modifiers: gpui::Modifiers::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        self.draw(vcx);
    }
}

/// The painted chart surface's bounds.
fn chart_bounds(vcx: &mut gpui::VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
    vcx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let selector: &'static str = Box::leak(format!("timeseries-chart-{TILE}").into_boxed_str());
    vcx.debug_bounds(selector)
        .expect("the chart surface is painted")
}

/// A point inside the upper plot: the surface's centre is always in
/// it (the axis column is 44 px wide, the x strip 18 px tall).
fn plot_point(vcx: &mut gpui::VisualTestContext, dx: f32) -> gpui::Point<gpui::Pixels> {
    let b = chart_bounds(vcx);
    gpui::point(b.center().x + gpui::px(dx), b.origin.y + gpui::px(20.))
}

#[gpui::test]
fn a_wheel_over_the_plot_zooms_about_the_pointer_and_a_sideways_wheel_pans(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_loaded(cx, 100);
    assert_eq!(h.view(&vcx), (0.0, 100.0));
    let at = plot_point(&mut vcx, 0.);
    // Rolled away (positive y): in.
    h.wheel(&mut vcx, at, 0., 48.);
    let (lo, hi) = h.view(&vcx);
    assert!(
        (hi - lo - 80.0).abs() < 1e-6,
        "one ZOOM_FACTOR step: {lo}..{hi}"
    );
    // About the pointer, which sat near the plot's middle: the window
    // shrank from both ends.
    assert!(lo > 0.0 && hi < 100.0, "{lo}..{hi}");
    // The dominant axis wins: a sideways wheel pans and does not zoom.
    let before = h.view(&vcx);
    h.wheel(&mut vcx, at, -40., 5.);
    let after = h.view(&vcx);
    assert!(
        (after.1 - after.0 - (before.1 - before.0)).abs() < 1e-6,
        "span kept"
    );
    assert!(
        after.0 > before.0,
        "content dragged left shows later buckets"
    );
    // Rolled toward (negative y): out, and it never leaves the full range.
    h.wheel(&mut vcx, at, 0., -480.);
    assert_eq!(h.view(&vcx), (0.0, 100.0));
    // Over the x-axis strip (below the plot) nothing answers.
    let b = chart_bounds(&mut vcx);
    let strip = gpui::point(b.center().x, b.origin.y + b.size.height - gpui::px(4.));
    h.wheel(&mut vcx, strip, 0., 48.);
    assert_eq!(h.view(&vcx), (0.0, 100.0), "the strip is not a plot");
}

#[gpui::test]
fn a_drag_on_the_plot_pans_and_ends_on_release_or_a_buttonless_move(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_loaded(cx, 100);
    h.dispatch(&mut vcx, "zoom_in", None);
    let before = h.view(&vcx);
    let at = plot_point(&mut vcx, 0.);
    vcx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    h.draw(&mut vcx);
    assert!(matches!(h.drag(&vcx), Some(Drag::Pan { .. })), "armed");
    let to = gpui::point(at.x + gpui::px(40.), at.y);
    vcx.simulate_mouse_move(to, gpui::MouseButton::Left, gpui::Modifiers::default());
    h.draw(&mut vcx);
    let after = h.view(&vcx);
    assert!(
        (after.1 - after.0 - (before.1 - before.0)).abs() < 1e-6,
        "a pan keeps the span"
    );
    assert!(
        after.0 < before.0,
        "dragged right: earlier buckets ({before:?} → {after:?})"
    );
    vcx.simulate_event(gpui::MouseUpEvent {
        position: to,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 1,
    });
    h.draw(&mut vcx);
    assert_eq!(h.drag(&vcx), None, "released");
    // A second press of a double-click arms nothing.
    vcx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    h.draw(&mut vcx);
    assert_eq!(h.drag(&vcx), None, "the shell owns the double-click");
    // A missed release: the next buttonless move ends the drag.
    click_at_down(&mut vcx, at);
    h.draw(&mut vcx);
    assert!(h.drag(&vcx).is_some());
    vcx.simulate_mouse_move(to, None, gpui::Modifiers::default());
    h.draw(&mut vcx);
    assert_eq!(h.drag(&vcx), None, "a buttonless move is the release");
}

/// A left press only, for the drag tests.
fn click_at_down(vcx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    vcx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
}

#[gpui::test]
fn a_drag_on_the_divider_moves_the_split(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_loaded(cx, 20);
    h.command(&mut vcx, "add VIX").unwrap();
    h.command(&mut vcx, "yaxis s2 bottomleft").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "VIX", Ok(1));
    let tag = h.series_request().unwrap().tag;
    h.deliver_series(&mut vcx, tag, result_with(&[1, 2], 20));
    h.draw(&mut vcx);
    // The band paints only once the surface's bounds are known — the
    // frame after the first paint.
    h.draw(&mut vcx);
    let before = h.model(&vcx).split();
    let band = centre_of(&mut vcx, &format!("timeseries-divider-{TILE}"));
    click_at_down(&mut vcx, band);
    h.draw(&mut vcx);
    assert_eq!(
        h.drag(&vcx),
        Some(Drag::Split),
        "the band arms a split drag"
    );
    let to = gpui::point(band.x, band.y + gpui::px(40.));
    vcx.simulate_mouse_move(to, gpui::MouseButton::Left, gpui::Modifiers::default());
    h.draw(&mut vcx);
    let after = h.model(&vcx).split();
    assert!(
        after > before,
        "dragged down: a taller upper pane ({before} → {after})"
    );
    assert!(
        ((after / 0.01).round() * 0.01 - after).abs() < 1e-6,
        "quantised: {after}"
    );
    vcx.simulate_event(gpui::MouseUpEvent {
        position: to,
        modifiers: gpui::Modifiers::default(),
        button: gpui::MouseButton::Left,
        click_count: 1,
    });
    h.draw(&mut vcx);
    assert_eq!(h.drag(&vcx), None);
    // With one pane there is no band at all.
    h.command(&mut vcx, "yaxis s2 left").unwrap();
    h.draw(&mut vcx);
    h.draw(&mut vcx);
    let selector: &'static str = Box::leak(format!("timeseries-divider-{TILE}").into_boxed_str());
    assert!(vcx.debug_bounds(selector).is_none(), "one pane: no divider");
}

#[gpui::test]
fn the_actions_button_toggles_the_menu_and_a_row_click_dispatches_or_explains(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.click(&mut vcx, &format!("timeseries-menu-button-{TILE}"));
    assert!(h.popup_is_menu(&vcx));
    assert_eq!(
        h.key_context_pair(&mut vcx, "popup").as_deref(),
        Some("menu")
    );
    assert_eq!(h.key_context_mode(&mut vcx), "normal");
    let rows = h.menu_rows(&vcx);
    assert_eq!(rows[0], ("Add series…".to_string(), true), "{rows:?}");
    assert!(rows.contains(&("[no series]".to_string(), false)));
    // A disabled row explains and stays.
    let remove = h.menu_row_index(&vcx, "Remove");
    h.click(&mut vcx, &format!("ts-menu-row-{TILE}-{remove}"));
    assert!(h.popup_is_menu(&vcx), "a disabled row keeps the menu");
    assert_eq!(h.notice(&vcx).as_deref(), Some("add a series first"));
    // The button closes it.
    h.click(&mut vcx, &format!("timeseries-menu-button-{TILE}"));
    assert!(h.popup_is_none(&vcx), "a second click closes");
    // An enabled row closes the menu and takes the verb's own path.
    h.click(&mut vcx, &format!("timeseries-menu-button-{TILE}"));
    let range = h.menu_row_index(&vcx, "Range…");
    h.click(&mut vcx, &format!("ts-menu-row-{TILE}-{range}"));
    assert!(h.popup_is_range(&vcx), "the row opened the range popup");
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
}

#[gpui::test]
fn the_menu_keys_step_over_action_rows_pick_and_close(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.dispatch(&mut vcx, "menu", None);
    assert!(h.popup_is_menu(&vcx));
    // Three down from `Add series…` lands on `Range…`; one more skips
    // the separator and the section and lands on `Hide`.
    h.dispatch(&mut vcx, "list_down", Some(3));
    assert!(h.menu_rows(&vcx)[3].1, "{:?}", h.menu_rows(&vcx));
    h.dispatch(&mut vcx, "list_down", None);
    let rows = h.menu_rows(&vcx);
    let lit = rows.iter().position(|(_, on)| *on).unwrap();
    assert_eq!(rows[lit].0, "Hide");
    // Pick: the slot hides, the menu is gone.
    h.dispatch(&mut vcx, "menu_pick", None);
    assert!(h.popup_is_none(&vcx));
    assert!(!h.model(&vcx).slots()[0].visible);
    // `escape`'s verb closes; `.` toggles.
    h.dispatch(&mut vcx, "menu", None);
    assert!(h.popup_is_menu(&vcx));
    h.dispatch(&mut vcx, "list_close", None);
    assert!(h.popup_is_none(&vcx));
    h.dispatch(&mut vcx, "menu", None);
    h.dispatch(&mut vcx, "menu", None);
    assert!(h.popup_is_none(&vcx), "a second `.` closes");
    // Any other verb closes the menu first and then acts.
    h.dispatch(&mut vcx, "menu", None);
    h.dispatch(&mut vcx, "zoom_in", None);
    assert!(h.popup_is_none(&vcx));
}

#[gpui::test]
fn a_menu_row_hover_moves_the_highlight(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "menu", None);
    let range = h.menu_row_index(&vcx, "Range…");
    let at = centre_of(&mut vcx, &format!("ts-menu-row-{TILE}-{range}"));
    vcx.simulate_mouse_move(at, None, gpui::Modifiers::default());
    h.draw(&mut vcx);
    assert!(h.menu_rows(&vcx)[range].1, "the pointer's row is lit");
}

#[gpui::test]
fn a_right_click_on_a_chip_selects_it_and_opens_the_menu_on_it(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    assert_eq!(h.model(&vcx).cursor(), Some(1));
    h.right_click(&mut vcx, &format!("timeseries-chip-{TILE}-1"));
    assert_eq!(h.model(&vcx).cursor(), Some(0));
    assert!(h.popup_is_menu(&vcx));
    assert!(
        h.menu_rows(&vcx)
            .contains(&("[SPX.close]".to_string(), false)),
        "{:?}",
        h.menu_rows(&vcx)
    );
}

#[gpui::test]
fn a_swatch_click_toggles_visibility_and_the_readout_opens_the_range_popup(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    h.click(&mut vcx, &format!("timeseries-swatch-{TILE}-1"));
    let m = h.model(&vcx);
    assert!(!m.slots()[0].visible, "hidden");
    assert_eq!(m.cursor(), Some(0), "the toggled slot is the cursor");
    assert!(
        h.painted_text(&mut vcx).contains("SPX.close"),
        "still in the strip"
    );
    h.click(&mut vcx, &format!("timeseries-swatch-{TILE}-1"));
    assert!(h.model(&vcx).slots()[0].visible, "shown again");
    h.click(&mut vcx, &format!("timeseries-range-{TILE}"));
    assert!(h.popup_is_range(&vcx));
    // A second click closes rather than reseeding over typed dates
    // (`left` onto the month, then a digit typed into it — a bare
    // digit on an unedited popup would be a preset and commit).
    vcx.simulate_keystrokes("left 3");
    assert!(h.popup_is_range(&vcx));
    h.click(&mut vcx, &format!("timeseries-range-{TILE}"));
    assert!(h.popup_is_none(&vcx), "the readout toggles");
}

#[gpui::test]
fn an_outside_click_closes_the_menu_and_the_menu_follows_the_cursor_slot(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_loaded(cx, 20);
    h.command(&mut vcx, "add VIX").unwrap();
    h.dispatch(&mut vcx, "menu", None);
    assert!(h.menu_rows(&vcx).contains(&("[VIX]".to_string(), false)));
    // A `:` line under the open menu moves the cursor slot: the rows
    // follow it.
    h.command(&mut vcx, "remove s2").unwrap();
    assert!(h.popup_is_menu(&vcx), "`:` leaves the menu up");
    let rows = h.menu_rows(&vcx);
    assert!(
        rows.contains(&("[SPX.close]".to_string(), false)),
        "{rows:?}"
    );
    assert!(rows.iter().any(|(_, on)| *on), "the highlight survived");
    // A click on the chart, outside the menu, closes it; the press
    // that closed it arms no lingering drag once released.
    let at = plot_point(&mut vcx, 0.);
    click_at(&mut vcx, at, 1);
    h.draw(&mut vcx);
    assert!(h.popup_is_none(&vcx), "an outside click closes the menu");
    assert_eq!(h.drag(&vcx), None);
}

#[gpui::test]
fn a_frequency_chip_writes_at_once_keeps_the_popup_open_and_refuses_inline(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.dispatch(&mut vcx, "range", None);
    h.click(&mut vcx, &format!("ts-range-freq-{TILE}-1h"));
    assert_eq!(h.model(&vcx).frequency(), Frequency::H1);
    assert!(h.popup_is_range(&vcx), "a setting, not a commit");
    assert!(h.painted_text(&mut vcx).contains("1y · 1h"));
    // A year of minutes is over the point cap: refused inline, the
    // frequency untouched.
    h.click(&mut vcx, &format!("ts-range-freq-{TILE}-1m"));
    assert_eq!(h.model(&vcx).frequency(), Frequency::H1);
    assert!(h.range_error(&vcx).is_some(), "the cap refusal is inline");
    assert!(h.popup_is_range(&vcx));
    // A good chip clears it.
    h.click(&mut vcx, &format!("ts-range-freq-{TILE}-1d"));
    assert_eq!(h.range_error(&vcx), None);
    assert_eq!(h.model(&vcx).frequency(), Frequency::D1);
}

#[gpui::test]
fn the_empty_state_buttons_open_the_picker_and_the_expression_field(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.click(&mut vcx, &format!("timeseries-empty-{TILE}-0"));
    assert!(
        h.tile
            .read_with(&vcx, |t, _| matches!(t.popup(), Some(Popup::Picker(_))))
    );
    h.dispatch(&mut vcx, "cancel", None);
    h.click(&mut vcx, &format!("timeseries-empty-{TILE}-1"));
    assert!(h.popup_is_expr(&vcx));
    h.dispatch(&mut vcx, "cancel", None);
    // Loaded, the buttons are gone with the hint.
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.draw(&mut vcx);
    let selector: &'static str = Box::leak(format!("timeseries-empty-{TILE}-0").into_boxed_str());
    assert!(vcx.debug_bounds(selector).is_none());
}
