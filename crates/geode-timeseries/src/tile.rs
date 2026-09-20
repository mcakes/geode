//! The timeseries tile (spec §9.1–§9.4, §9.9, §9.11): the entity the
//! shell hosts, its normal-mode verbs, its `:` vocabulary, the header it
//! prepares and the chart element it paints.
//!
//! **Three tails, not one.** Every mutation answers a
//! [`Changed`](crate::core::Changed) bitset and ends at exactly one of
//! them:
//!
//! - [`TimeseriesTile::apply_changed`] — a change to WHAT is plotted:
//!   re-prepare the header, rebuild the chart model (bumping its
//!   `version`, which is what invalidates `geode-chart`'s path and
//!   chrome caches), notify.
//! - [`TimeseriesTile::view_moved`] — a pan, a zoom, a jump: the header
//!   does not depend on the view and neither does the chart MODEL (the
//!   element takes `model.view()` beside it), so rebuilding either here
//!   would throw away every cached path for a frame that only scrolled.
//! - a refusal — the notice, and nothing else.
//!
//! Task 7 fills the data half in: `apply_changed`'s FETCH and QUERY
//! arms, `deliver`, `on_fetched`'s requery, the frame observer and the
//! flip barrier. Every field those need is already here, so Task 7 adds
//! method bodies and no fields.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use geode_chart::core::palette::Palette;
use geode_chart::{Axis, AxisMode, ChartElement, ChartModel};
use geode_core::colour::NamedColours;
use geode_core::query::AsOf;
use geode_core::series::{Frequency, SeriesOutcome, SeriesResult};
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::series::SeriesSettings;
use geode_shell::shell::colours::{
    anchors_from_theme, theme_signature, to_hsla, tokens_from_theme,
};
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{App, Context, ElementId, Entity, Hsla, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, Theme, v_flex};

use crate::commands::{self, Command};
use crate::core::model::{Changed, Colour, Model, SlotState};
use crate::core::{chart, resolve, session};
use crate::header::{self, HeaderModel};
use crate::popup::Popup;

/// Exactly what [`chart::build`] reads, and nothing else — the memo key
/// that decides whether a chrome rebuild also rebuilds the chart model
/// (review round 1, I-2).
///
/// It exists because a chart model is EXPENSIVE and most chrome changes
/// do not touch one: `chart::build` clones every slot's `values` and the
/// whole bucket vector, so at the 500,000-point cap a `tab`, a chip
/// click, a `set_visible` or a finished fetch would each copy several
/// megabytes and — through the `version` bump — throw away every path
/// `geode-chart` has cached, for a model identical to the one it
/// replaced. Comparing this instead costs a handful of small clones per
/// slot.
///
/// **A field `chart::build` reads must appear here**, or a change to it
/// paints stale — the same rule `shell::colours::theme_signature`
/// carries, for the same reason. Note what is deliberately absent: a
/// source slot's `source`/`identity` (its LABEL is read, but a slot
/// number is never reused for the tile's life, so `number` pins the
/// pair), its `rule` and the model's `percentiles` (neither reaches the
/// chart model — they shape the REQUEST, and the answer arrives as a new
/// `result`), and the view (the element takes it beside the model).
#[derive(Clone, PartialEq)]
struct ChartKey {
    /// The result's identity. A pointer, not the data: a `SeriesResult`
    /// is installed whole and never mutated in place, so a new pointer
    /// is exactly "new data". `0` for no result.
    result: usize,
    /// Per slot: everything `chart::build` copies out of it.
    slots: Vec<(u8, Colour, Axis, bool, Option<String>)>,
    frequency: Frequency,
    axis_mode: AxisMode,
    /// Bit pattern, because `f32` is not `Eq` and a split is compared,
    /// never arithmetic'd, here.
    split: u32,
    density: bool,
    /// Read by `Model::label` for a slot whose source is not the default.
    default_source: Option<String>,
    /// The two inputs to `colour_fn`: a slot's colour is resolved INTO
    /// the chart model, so a theme change or a reloaded `colours.toml`
    /// (a fresh `Arc`, which is what `set_colours` swaps in) is a chart
    /// change.
    theme: [Hsla; 28],
    colours: usize,
}

pub struct TimeseriesTile {
    id: TileId,
    frame: Entity<Frame>,
    /// Task 7: the catalog (a fetch source's identities) the add picker
    /// ranks over, and the health a slot's chip reports.
    #[allow(dead_code)]
    diagnostics: Entity<Diagnostics>,
    /// Task 7: `Request::Fetch` and `Request::Series` go through it.
    #[allow(dead_code)]
    data: DataHandle,
    colours: Rc<RefCell<Arc<NamedColours>>>,
    model: Model,
    /// The last good result; the chart model is built from it.
    /// Task 7 installs one per delivery.
    #[allow(dead_code)]
    result: Option<Arc<SeriesResult>>,
    chart: Arc<ChartModel>,
    chart_version: u64,
    /// What [`Self::chart`] was built from. `rebuild_chrome` rebuilds the
    /// chart model only when this differs — see [`ChartKey`].
    last_chart_key: Option<ChartKey>,
    /// Rebuild the chrome on the next render if the theme moved — the
    /// chips' swatches and the chart model's line colours are both
    /// resolved against it.
    theme_key: Option<[Hsla; 28]>,
    /// Task 7: the tag of the request in flight, so a stale answer is
    /// dropped.
    #[allow(dead_code)]
    tag: u64,
    /// Task 7: the frame versions the request in flight was made under.
    #[allow(dead_code)]
    acted: Option<FrameVersions>,
    /// Task 7: a delivery staged behind the flip barrier.
    #[allow(dead_code)]
    staged: Option<(SeriesResult, FrameVersions)>,
    /// Task 7: the last flip counter this tile promoted at.
    #[allow(dead_code)]
    last_flip: u64,
    /// Task 7 requeries on the first `set_visible(true)`; until then
    /// this only records what the shell said.
    #[allow(dead_code)]
    visible: bool,
    /// Task 7: the next delivery resets the view to the new full range.
    #[allow(dead_code)]
    reset_view: bool,
    notice: Option<SharedString>,
    /// Prepared text: the range/frequency readout and one chip per slot.
    header: HeaderModel,
    title: SharedString,
    stack: Option<StackHandle>,
    /// Tasks 8–10; always `None` in this task (the enum is uninhabited).
    popup: Option<Popup>,
    footer: SharedString,
}

impl TimeseriesTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TileId,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        data: DataHandle,
        colours: Rc<RefCell<Arc<NamedColours>>>,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Tasks 8–10 open a field from here (a restored draft's editor);
        // the parameter is kept so that arrives as a body change.
        let _ = window;
        let settings = cx
            .try_global::<SeriesSettings>()
            .cloned()
            .unwrap_or_default();
        let (model, notices) = match restored {
            Some(t) => session::from_table(
                t,
                &|s| settings.dataset_of(s).map(str::to_string),
                settings.default_source.as_deref(),
            ),
            None => (Model::new(), Vec::new()),
        };
        // `[timeseries] default_source` decides what a chip's label says
        // (an `@source` is shown only when it is NOT the default), so a
        // settings change is a chrome change even with no slot touched.
        cx.observe_global::<SeriesSettings>(|this, cx| {
            this.rebuild_chrome(cx);
            cx.notify();
        })
        .detach();
        // The footer names live chords, so a keymap reload re-resolves
        // it — once, here, never per frame.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.footer = header::footer_text(cx);
            cx.notify();
        })
        .detach();
        // Task 7 adds the frame observer: promote a staged result when
        // the flip releases, requery when `as_of`/`data` move, self-arrive
        // otherwise.

        // One derivation feeds both the chips' swatches and the chart
        // model's line colours — they are the same five colours, and
        // building the wheel twice is the thing `rebuild_chrome` exists
        // to avoid.
        let colours_ptr = Arc::as_ptr(&colours.borrow()) as usize;
        let colour_of = colour_fn(Arc::clone(&colours.borrow()), cx.theme());
        let header = HeaderModel::prepare(&model, settings.default_source.as_deref(), &colour_of);
        let title = header::title_text(&model);
        let chart = Arc::new(chart::build(
            &SeriesResult::default(),
            &model,
            1,
            local_offset_secs(),
            &colour_of,
            settings.default_source.as_deref(),
        ));
        let last_chart_key = Some(chart_key(
            &model,
            None,
            settings.default_source.clone(),
            theme_signature(cx.theme()),
            colours_ptr,
        ));
        TimeseriesTile {
            id,
            frame,
            diagnostics,
            data,
            colours,
            model,
            result: None,
            chart,
            chart_version: 1,
            last_chart_key,
            theme_key: None,
            tag: 0,
            acted: None,
            staged: None,
            last_flip: 0,
            visible: false,
            reset_view: false,
            notice: (!notices.is_empty()).then(|| notices.join("; ").into()),
            header,
            title,
            stack: None,
            popup: None,
            footer: header::footer_text(cx),
        }
    }

    // ---- what the shell reads ----------------------------------------

    /// `insert` exactly while a popup holds a text field (Tasks 8–10);
    /// `normal` otherwise. Task 8 adds the `popup == series` pair for
    /// the series list's own three keys — its fragment layer is already
    /// shipped, and binds nothing until a `Popup` variant can set it.
    pub fn key_context(&self) -> KeyContext {
        let mode = if self.popup.as_ref().is_some_and(Popup::is_insert) {
            "insert"
        } else {
            "normal"
        };
        KeyContext::new("timeseries").pair("mode", mode).counts()
    }

    /// The ownership half of the shell's insert-focus predicate: does
    /// one of THIS tile's own fields hold window focus right now?
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.popup
            .as_ref()
            .is_some_and(|p| p.holds_focus(window, cx))
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    pub fn serialize(&self) -> toml::Table {
        session::to_table(&self.model)
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    /// Task 7 refetches and requeries on the first `true`; here the flag
    /// is only recorded and the chrome re-prepared.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// This tile has no find; `/` belongs to the series list (Task 9).
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = (event, window, cx);
    }

    // ---- deliveries --------------------------------------------------

    /// Task 7 installs the result, promotes behind the flip barrier and
    /// re-clamps the view. Until then a series answer changes nothing.
    pub fn deliver(&mut self, outcome: SeriesOutcome, cx: &mut Context<Self>) {
        let _ = (outcome, cx);
    }

    /// A fetch finished for one `(source, identity)` pair — the STATE
    /// half (Task 7 adds the requery an `Ok` owes). Keyed by the pair,
    /// so a tile that holds it marks every slot over it and a tile that
    /// does not is left alone by `set_pair_state`'s own `NONE`.
    pub fn on_fetched(
        &mut self,
        source: &str,
        identity: &str,
        result: Result<u64, String>,
        cx: &mut Context<Self>,
    ) {
        let state = match &result {
            Ok(_) => SlotState::Idle,
            Err(why) => SlotState::Failed(why.clone()),
        };
        let changed = self.model.set_pair_state(source, identity, state);
        if changed.is_none() {
            return;
        }
        // Task 7 adds the requery: an `Ok` — `Ok(0)` included — means the
        // span is covered and the tile should ask for its points again.
        self.rebuild_chrome(cx);
        cx.notify();
    }

    // ---- keys --------------------------------------------------------

    /// `window` is forwarded for Tasks 8–10 alone: the popup verbs
    /// create a field and give the keyboard back up, neither of which is
    /// reachable from `&mut App`.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(verb) = action.0.strip_prefix("timeseries::") else {
            return false;
        };
        // A notice belongs to the last action that ACTED (review round
        // 1, MIN-3): the next handled verb clears it before it can set
        // one of its own, but a verb this tile does NOT handle — every
        // popup verb until Tasks 8–10, and anything unrecognised — must
        // leave the text still on screen alone, or the state says
        // "cleared" while the trader reads the old line. So it is taken
        // here and put back on the two unhandled paths.
        let previous = self.notice.take();
        let n = count.unwrap_or(1).max(1) as usize;
        // Task 8: a popup closes first unless the verb is its own.
        let (now, as_of) = self.now_and_as_of(cx);
        // A view move ends at `view_moved`, never `apply_changed` — see
        // the module doc's three tails.
        let view_move = matches!(
            verb,
            "pan_left"
                | "pan_right"
                | "zoom_in"
                | "zoom_out"
                | "reset_view"
                | "jump_start"
                | "jump_end"
        );
        let changed = match verb {
            "next" => self.model.cursor_next(n),
            "prev" => self.model.cursor_prev(n),
            "toggle_visible" => self.model.toggle_visible(),
            "axis_next" => self.model.cycle_axis(true, n),
            "axis_prev" => self.model.cycle_axis(false, n),
            "split_shrink" => self.model.step_split(false, n),
            "split_grow" => self.model.step_split(true, n),
            "colour" => self.model.cycle_colour(),
            "rule" => self.model.cycle_rule(),
            "remove" => self.remove_at_cursor(),
            "density" => self.model.toggle_density(),
            "percentiles" => self.model.toggle_percentiles(),
            "freq_finer" => {
                let r = self.model.step_frequency(true, n, now, &as_of);
                self.noticed(r)
            }
            "freq_coarser" => {
                let r = self.model.step_frequency(false, n, now, &as_of);
                self.noticed(r)
            }
            "pan_left" => self.model.pan(-(n as i32)),
            "pan_right" => self.model.pan(n as i32),
            "zoom_in" => self.model.zoom_in(n),
            "zoom_out" => self.model.zoom_out(n),
            "reset_view" => self.model.reset_view(),
            "jump_start" => self.model.jump_start(),
            "jump_end" => self.model.jump_end(),
            // Tasks 8–10; `false` until then.
            "add" | "expr" | "edit" | "list" | "range" | "list_down" | "list_up" | "list_close"
            | "commit" | "cancel" | "insert_up" | "insert_down" => {
                let handled = self.popup_verb(verb, n, window, cx);
                // `e` on a source slot sets its own; anything else did
                // nothing and gives the standing notice back.
                if !handled && self.notice.is_none() {
                    self.notice = previous;
                }
                return handled;
            }
            _ => {
                self.notice = previous;
                return false;
            }
        };
        if view_move {
            self.view_moved(changed, cx);
        } else {
            self.apply_changed(changed, cx);
        }
        true
    }

    /// Every popup verb, answered `false` until Tasks 8–10 build the
    /// overlay. The one thing this task owes is `e`'s refusal on a
    /// SOURCE slot: there is no expression to open, and a trader who
    /// pressed it deserves the reason rather than a dead key.
    fn popup_verb(
        &mut self,
        verb: &str,
        n: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let _ = (n, window);
        if verb == "edit"
            && let Some(number) = header::cursor_is_source(&self.model)
        {
            self.notice = Some(format!("s{number} is not an expression").into());
            cx.notify();
        }
        false
    }

    /// The mouse's form of `tab` (spec §9.3): a chip click moves the
    /// cursor onto its slot.
    pub(crate) fn chip_clicked(&mut self, index: usize, cx: &mut Context<Self>) {
        let changed = self.model.set_cursor(index);
        self.apply_changed(changed, cx);
    }

    // ---- the `:` line ------------------------------------------------

    pub fn command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let _ = window;
        self.notice = None;
        let cmd = commands::parse(line)?;
        let (now, as_of) = self.now_and_as_of(cx);
        let settings = cx
            .try_global::<SeriesSettings>()
            .cloned()
            .unwrap_or_default();
        let changed = match cmd {
            Command::Add { identity, source } => {
                let source = match source.or_else(|| settings.default_source.clone()) {
                    Some(s) => s,
                    None => {
                        return Err(format!(
                            "name a source or set a default: add {identity}@<source>"
                        ));
                    }
                };
                let dataset = settings
                    .dataset_of(&source)
                    .ok_or_else(|| {
                        format!(
                            "'{source}' is not a fetch source (have: {})",
                            settings.names().join(", ")
                        )
                    })?
                    .to_string();
                self.model.add_source(&identity, &source, &dataset)?.1
            }
            Command::Expr(text) => {
                let e = resolve(
                    &text,
                    self.model.slots(),
                    settings.default_source.as_deref(),
                    None,
                )?;
                self.model.add_expr(&text, e)?.1
            }
            Command::Remove(n) => self.remove(n)?,
            Command::Rule(n, r) => self.model.set_rule(n, r)?,
            Command::Colour(n, name) => {
                let colour = self.colour_named(&name)?;
                self.model.set_colour(n, colour)?
            }
            Command::AxisMode(m) => self.model.set_axis_mode(m),
            Command::Freq(f) => self.model.set_frequency(f, now, &as_of)?,
            Command::Range(r) => self.model.set_range(r, now, &as_of)?,
            Command::Pct(p) => self.model.set_percentiles(p)?,
            Command::Density(d) => self.model.set_density(d)?,
            Command::YAxis(n, a) => self.model.set_axis(n, a)?,
            Command::Split(s) => self.model.set_split(s)?,
            Command::Clear => self.model.clear(),
        };
        self.apply_changed(changed, cx);
        Ok(())
    }

    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        let slots: Vec<u8> = self.model.slots().iter().map(|s| s.number).collect();
        let sources = cx
            .try_global::<SeriesSettings>()
            .map(|s| s.names())
            .unwrap_or_default();
        let colours: Vec<String> = self.colours.borrow().names().map(str::to_string).collect();
        commands::completions(line, cursor, &slots, &sources, &colours)
    }

    /// `1`..`5` is a palette index; anything else is a `[colours]` name.
    fn colour_named(&self, name: &str) -> Result<Colour, String> {
        if let Ok(i) = name.parse::<usize>()
            && (1..=Palette::LEN).contains(&i)
        {
            return Ok(Colour::Palette(i - 1));
        }
        if self.colours.borrow().get(name).is_some() {
            Ok(Colour::Named(name.into()))
        } else {
            Err(format!(
                "no colour named '{name}' — 1..{} or a [colours] entry",
                Palette::LEN
            ))
        }
    }

    // ---- the tails ---------------------------------------------------

    /// A change to WHAT is plotted. Task 7 adds the FETCH arm (fetch
    /// every source slot over the new range, and set `reset_view`) and
    /// the QUERY arm (requery); the SESSION bit needs nothing here — the
    /// shell serialises on its own schedule.
    fn apply_changed(&mut self, changed: Changed, cx: &mut Context<Self>) {
        if let Some(n) = self.model.take_notice() {
            self.notice = Some(n.into());
        }
        if changed.chrome() || changed.query() || changed.fetch() {
            self.rebuild_chrome(cx);
        }
        cx.notify();
    }

    /// A pan, a zoom or a jump. Deliberately NOT `apply_changed`: the
    /// header does not read the view and neither does the chart model,
    /// so rebuilding either would bump `ChartModel::version` and throw
    /// away every cached path in `geode-chart` for a frame that only
    /// scrolled. Task 7 adds the requery `changed.query()` asks for when
    /// the stats are on (they are computed over the VISIBLE window).
    fn view_moved(&mut self, changed: Changed, cx: &mut Context<Self>) {
        let _ = changed;
        cx.notify();
    }

    /// A refused verb: the reason becomes the notice and the tile
    /// repaints, nothing else.
    fn noticed(&mut self, r: Result<Changed, String>) -> Changed {
        match r {
            Ok(c) => c,
            Err(e) => {
                self.notice = Some(e.into());
                Changed::CHROME
            }
        }
    }

    fn remove_at_cursor(&mut self) -> Changed {
        let Some(number) = self.model.cursor_slot().map(|s| s.number) else {
            return Changed::NONE;
        };
        match self.remove(number) {
            Ok(changed) => changed,
            Err(e) => {
                self.notice = Some(e.into());
                Changed::CHROME
            }
        }
    }

    /// The one removal door `d` and `:remove` share — including the
    /// notice naming what went with the slot (spec §7: removing an
    /// operand removes every expression that reads it).
    fn remove(&mut self, number: u8) -> Result<Changed, String> {
        let removal = self.model.remove(number)?;
        if removal.removed.len() > 1 {
            let rest = removal.removed[1..]
                .iter()
                .map(|n| format!("s{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            self.notice = Some(format!("removed s{number} and, with it, {rest}").into());
        }
        Ok(removal.changed)
    }

    /// Re-prepare everything painted from the model: the header and the
    /// title always, the chart model only when [`ChartKey`] says one of
    /// its own inputs moved. The ONE door, so the colour wheel is
    /// derived once per change and the chips agree with the lines by
    /// construction rather than by two call sites keeping step.
    ///
    /// The chart model is immutable input the element caches against, so
    /// it is built here and never in `render`; every field the element's
    /// caches do not key on (`axis_mode`, `step_us`) rides on `version`,
    /// which is why an actual rebuild bumps it — and why a skipped one
    /// must not (a bump with no new model is a cache flush for nothing).
    fn rebuild_chrome(&mut self, cx: &mut Context<Self>) {
        let default_source = cx
            .try_global::<SeriesSettings>()
            .and_then(|s| s.default_source.clone());
        let theme = theme_signature(cx.theme());
        let colours_ptr = Arc::as_ptr(&self.colours.borrow()) as usize;
        let colour_of = colour_fn(Arc::clone(&self.colours.borrow()), cx.theme());
        self.header = HeaderModel::prepare(&self.model, default_source.as_deref(), &colour_of);
        self.title = header::title_text(&self.model);
        let key = chart_key(
            &self.model,
            self.result.as_ref(),
            default_source.clone(),
            theme,
            colours_ptr,
        );
        if self.last_chart_key.as_ref() == Some(&key) {
            return;
        }
        self.chart_version += 1;
        let empty = SeriesResult::default();
        let result = self.result.as_deref().unwrap_or(&empty);
        let chart = chart::build(
            result,
            &self.model,
            self.chart_version,
            local_offset_secs(),
            &colour_of,
            default_source.as_deref(),
        );
        self.chart = Arc::new(chart);
        self.last_chart_key = Some(key);
    }

    fn now_and_as_of(&self, cx: &App) -> (DateTime<Utc>, AsOf) {
        (Utc::now(), self.frame.read(cx).as_of().clone())
    }

    // ---- test accessors ----------------------------------------------

    /// The prepared header — what the display check looks at, and what
    /// this crate's own tests read in place of pixels.
    #[cfg(test)]
    pub(crate) fn header(&self) -> &HeaderModel {
        &self.header
    }

    #[cfg(test)]
    pub(crate) fn model(&self) -> &Model {
        &self.model
    }

    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn chart(&self) -> &Arc<ChartModel> {
        &self.chart
    }
}

impl Render for TimeseriesTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A theme change moves every slot's colour, and those colours
        // live INSIDE the chart model and the prepared chips — so the
        // models, not just the paint, have to be rebuilt. The full
        // 28-value signature is the memo key (`shell::colours`' own
        // rule): anything less and a theme that moves only an anchor
        // paints stale. On the steady path this is 28 `Hsla` copies and
        // 28 compares, and nothing else.
        //
        // The named colours are checked beside it, against the pointer
        // the last chart model was built from: `TimeseriesFactory::
        // set_colours` swaps a fresh `Arc` into the cell this tile
        // shares, and nothing else would ever tell an OPEN tile that a
        // reloaded `colours.toml` redefined a name it paints (review
        // round 1, MIN-4).
        let signature = theme_signature(cx.theme());
        let colours_ptr = Arc::as_ptr(&self.colours.borrow()) as usize;
        let colours_moved = self
            .last_chart_key
            .as_ref()
            .is_none_or(|k| k.colours != colours_ptr);
        if self.theme_key != Some(signature) || colours_moved {
            self.theme_key = Some(signature);
            self.rebuild_chrome(cx);
        }
        let theme = cx.theme();
        let tile = cx.entity();
        let tile_id = self.id.0;
        let body = if self.model.slots().is_empty() {
            header::render_empty(theme).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .child(ChartElement::new(
                    self.chart.clone(),
                    self.model.view(),
                    window.rem_size().as_f32(),
                    // Unique per tile: `Buffers` and both path caches
                    // hang off this id, and two charts sharing one serve
                    // each other's paths.
                    ElementId::NamedInteger(SharedString::new_static("ts-chart"), tile_id),
                ))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(header::render_header(
                &self.header,
                theme,
                &tile,
                tile_id,
                self.stack.as_ref(),
            ))
            .when_some(self.notice.clone(), |el, n| {
                el.child(header::render_notice(&n, theme))
            })
            .child(body)
            .child(header::render_footer(self.footer.clone(), theme))
        // Tasks 8–10 add the popup layer here.
    }
}

/// Build a [`ChartKey`] from everything `chart::build` will read. A free
/// function, not a method, so the constructor — which has no `Self` yet
/// — records the same key the first model was built from.
fn chart_key(
    model: &Model,
    result: Option<&Arc<SeriesResult>>,
    default_source: Option<String>,
    theme: [Hsla; 28],
    colours: usize,
) -> ChartKey {
    ChartKey {
        result: result.map(|r| Arc::as_ptr(r) as usize).unwrap_or(0),
        slots: model
            .slots()
            .iter()
            .map(|s| {
                (
                    s.number,
                    s.colour.clone(),
                    s.axis,
                    s.visible,
                    s.text.clone(),
                )
            })
            .collect(),
        frequency: model.frequency(),
        axis_mode: model.axis_mode(),
        split: model.split().to_bits(),
        density: model.density().is_some(),
        default_source,
        theme,
        colours,
    }
}

/// A slot's colour on this theme: a palette index through the floored
/// five chart colours, a `[colours]` name through the shared wheel, and
/// a name the trader has since deleted back to the first palette colour
/// rather than an error — a stale name costs a colour, never a tile.
///
/// Takes the definitions by `Arc` and the theme's derived pair by value
/// so the returned closure borrows NOTHING: `rebuild_chrome` holds
/// it while it assigns `self.chart`.
fn colour_fn(colours: Arc<NamedColours>, theme: &Theme) -> impl Fn(&Colour) -> Hsla {
    let palette = Palette::from_theme(
        [
            theme.chart_1,
            theme.chart_2,
            theme.chart_3,
            theme.chart_4,
            theme.chart_5,
        ],
        theme.background,
        theme.foreground,
    );
    let anchors = anchors_from_theme(theme);
    let tokens = tokens_from_theme(theme);
    move |colour| match colour {
        Colour::Palette(i) => palette.colour(*i),
        Colour::Named(name) => match colours.get(name) {
            Some(def) => to_hsla(geode_core::colour::resolve(def, &anchors, &tokens)),
            None => palette.colour(0),
        },
    }
}

/// The trader's own clock offset, for the chart's session axis — every
/// DISPLAYED time is local (Phase 4a ruling), while everything stored
/// and queried is UTC.
fn local_offset_secs() -> i32 {
    chrono::Local::now().offset().local_minus_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;
    use crate::content::{ACTIONS, DEFAULT_KEYMAP, TimeseriesFactory};
    use crate::core::model::SlotState;
    use crate::core::{Colour, Model, Preset, Range};
    use geode_chart::{Axis, ChartModel};
    use geode_core::colour::NamedColours;
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::scopes::SavedScopes;
    use geode_core::series::{BucketRule, Frequency, SlotKind};
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::{ActionId, ActionRegistry};
    use geode_shell::diagnostics::Diagnostics;
    use geode_shell::frame::Frame;
    use geode_shell::module::{Delivery, ModuleFactory, ModuleRoster, TileContent, TileOccupant};
    use geode_shell::series::{FetchSource, SeriesSettings};
    use geode_shell::tiling::TileId;
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
        /// Held, never read here: dropping the receiver would close the
        /// request channel, and Task 7's tests drain it.
        _rx: Receiver<Request>,
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
    fn named_colours(degrees: f32) -> NamedColours {
        let mut c = NamedColours::default();
        c.insert(
            "spx".to_string(),
            geode_core::colour::Definition::hue(degrees, geode_core::colour::Tone::Normal),
        );
        c
    }

    fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_full(cx, None, Some("demo_kdb"))
    }

    fn open_with(
        cx: &mut gpui::TestAppContext,
        restored: Option<toml::Table>,
    ) -> (Harness, gpui::VisualTestContext) {
        open_full(cx, restored, Some("demo_kdb"))
    }

    fn open_with_default_source(
        cx: &mut gpui::TestAppContext,
        default_source: Option<&str>,
    ) -> (Harness, gpui::VisualTestContext) {
        open_full(cx, None, default_source)
    }

    fn open_full(
        cx: &mut gpui::TestAppContext,
        restored: Option<toml::Table>,
        default_source: Option<&str>,
    ) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let default_source = default_source.map(str::to_string);
        cx.update(move |cx| {
            cx.set_global(SeriesSettings {
                default_source,
                sources: vec![
                    FetchSource {
                        name: "demo_kdb".into(),
                        dataset: "series".into(),
                    },
                    FetchSource {
                        name: "demo_rest".into(),
                        dataset: "series".into(),
                    },
                ],
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
                    // field (Tasks 8–10) needs one for focus to behave
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
                _rx: rx,
            },
            vcx,
        )
    }

    impl Harness {
        fn command(&self, vcx: &mut gpui::VisualTestContext, line: &str) -> Result<(), String> {
            vcx.update(|window, cx| self.content.command(line, window, cx))
        }
        fn dispatch(&self, vcx: &mut gpui::VisualTestContext, verb: &str, count: Option<u32>) {
            let id = ActionId(format!("timeseries::{verb}"));
            vcx.update(|window, cx| self.content.dispatch(&id, count, window, cx));
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
        // predates the four-axis cycle Task 2 built).
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
        h.dispatch(&mut vcx, "add", None);
        h.dispatch(&mut vcx, "list", None);
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
}
