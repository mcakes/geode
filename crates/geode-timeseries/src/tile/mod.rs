//! The shell-hosted timeseries entity: slot model, requests, prepared header,
//! chart input, and one tile-owned popup.
//!
//! Model changes carry [`Changed`](crate::core::Changed) flags into
//! [`TimeseriesTile::apply_changed`], which schedules fetches and queries and
//! refreshes prepared content. View movement uses [`TimeseriesTile::view_moved`]
//! to retain the chart's cached paths; only visible-window statistics need a
//! new query. Refusals become notices or inline popup errors.
//!
//! [`data`] owns fetch tracking, tagged series delivery, and flip-barrier
//! staging. Only the frame's as-of counter invalidates an established series
//! request; flip releases staged results without triggering a query.
//! [`popups`] owns opening, input, commit, and dismissal for local editors.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Offset as _, Utc};
use geode_chart::core::palette::Palette;
use geode_chart::{Axis, AxisMode, ChartElement, ChartModel};
use geode_core::colour::NamedColours;
use geode_core::query::{AsOf, QueryKey};
use geode_core::series::{Frequency, SeriesOutcome, SeriesResult, SlotKind};
use geode_data::{DataHandle, FetchParams};
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::series::SeriesSettings;
use geode_shell::shell::colours::{
    anchors_from_theme, theme_signature, to_hsla, tokens_from_theme,
};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::vimnav::NavCommand;
use geode_widgets::datefield::{DateTimeField, FieldKey, Precision, Segment, route};
use gpui::prelude::*;
use gpui::{
    App, Context, ElementId, Entity, Focusable as _, Hsla, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, ScrollWheelEvent, SharedString, Window, canvas, div, px,
};
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Sizable as _, Theme, v_flex};

use crate::commands::{self, Command};
use crate::core::model::{Changed, Colour, Model, SlotState};
use crate::core::{
    Preset, Range, Rgb8, chart, colour_from_pick, menu, request, resolve, session, within_a_step,
};
use crate::header::{self, HeaderModel};
use crate::popup::{
    ColourPick, DateFieldPaint, ExprField, MenuState, PickContext, PickerStage, PickerState, Popup,
    RangePopup, SeriesPopup, Which, render_menu, render_picker, render_range, render_series_popup,
};
use crate::tile::pointer::{ChartBounds, Drag};

mod data;
mod pointer;
mod popups;

/// Inputs used to decide whether prepared chart data must be rebuilt.
/// `chart::build` clones buckets and per-slot values, so unchanged keys avoid
/// large copies and preserve the chart element's path caches.
///
/// Every input read by `chart::build` must be represented here or by result
/// identity. Rules and percentiles affect requests and arrive through a new
/// result. View bounds are supplied separately to the chart element.
/// Slot numbers remain unique while slots exist; clearing installs an empty
/// key before numbering can restart.
#[derive(Clone, PartialEq)]
struct ChartKey {
    /// Result installation sequence. Using an allocation address would permit
    /// a freed result's address to be reused for different points.
    result: u64,
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
    /// The app clock's offset (`local_offset_secs`): a `[time] zone`
    /// reload moves every displayed time, and `chart::build` bakes the
    /// offset into the model.
    offset_secs: i32,
}

pub struct TimeseriesTile {
    id: TileId,
    frame: Entity<Frame>,
    /// Catalogue used by the add picker. The observer updates an open identity
    /// list when its options change; series-row provenance comes from results.
    diagnostics: Entity<Diagnostics>,
    /// `Request::Fetch`, `Request::Series` and `Request::Cancel` go
    /// through it.
    data: DataHandle,
    colours: Rc<RefCell<Arc<NamedColours>>>,
    model: Model,
    /// The last good result; the chart model is built from it.
    result: Option<Arc<SeriesResult>>,
    /// Bumped on every install, and the result's identity in
    /// [`ChartKey`].
    result_seq: u64,
    chart: Arc<ChartModel>,
    chart_version: u64,
    /// What [`Self::chart`] was built from. `rebuild_chrome` rebuilds the
    /// chart model only when this differs — see [`ChartKey`].
    last_chart_key: Option<ChartKey>,
    /// Rebuild the chrome on the next render if the theme moved — the
    /// chips' swatches and the chart model's line colours are both
    /// resolved against it.
    theme_key: Option<[Hsla; 28]>,
    /// The tag of the request in flight, so a stale answer is dropped.
    tag: u64,
    /// The frame versions the request in flight was made under.
    acted: Option<FrameVersions>,
    query_in_flight: bool,
    /// A delivery staged behind the flip barrier.
    staged: Option<(SeriesResult, FrameVersions)>,
    /// Most recently observed flip generation, whether or not a result was staged.
    last_flip: u64,
    visible: bool,
    /// The next delivery resets the view to the new full range.
    reset_view: bool,
    /// Submitted `(source, identity)` fetches awaiting an answer.
    /// A Fetching slot means it needs data; this set prevents a later add from
    /// resubmitting its outstanding span. Visibility and range/as-of transitions
    /// clear tracking when a new span must be eligible for submission.
    in_flight: HashSet<(String, String)>,
    /// The range [`Self::in_flight`] was populated under. A range change
    /// asks for a different span, so an unanswered fetch over the old
    /// one must not suppress it.
    in_flight_range: Option<Range>,
    notice: Option<SharedString>,
    /// Prepared text: the range/frequency readout and one chip per slot.
    header: HeaderModel,
    title: SharedString,
    stack: Option<StackHandle>,
    /// One local popup: series list, add picker, expression editor, or range
    /// editor. List rows are prepared with chrome; date segments are prepared
    /// by field transitions rather than formatted during render.
    popup: Option<Popup>,
    footer: SharedString,
    /// The chart surface's last painted bounds (`tile::pointer`).
    chart_bounds: ChartBounds,
    /// The pointer gesture in progress, if any (`tile::pointer`).
    drag: Option<Drag>,
    /// Lazily created reusable component state with one set of subscriptions.
    /// The header renders its trigger while Popup::Colour is active.
    colour_picker: Option<Entity<ColorPickerState>>,
    /// What the picker's commits are written against; outlives the
    /// popup on purpose (see [`PickContext`]), replaced at each open.
    pick_context: Option<PickContext>,
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
        // Construction opens no editor; retain the factory's window parameter.
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
        // A clock reload changes the chart's offset input, invalidating its key.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.rebuild_chrome(cx);
            cx.notify();
        })
        .detach();
        cx.observe(&frame, |this, frame, cx| {
            // Process flip releases before the visibility guard so hidden tiles can
            // promote staged data. Promotion still checks the followed as-of version.
            let now = frame.read(cx).versions();
            if now.flip != this.last_flip {
                this.last_flip = now.flip;
                this.promote(cx);
            }
            if !this.visible {
                return;
            }
            // Established series requests follow as-of only. Frame scope, grouping,
            // and unrelated dataset publications do not change their inputs.
            if !this.model.slots().is_empty() && this.follows_changed(now) {
                // Changing as-of can move both ends of a relative range. Clear fetch
                // tracking even though the stored Range is unchanged, then ask for gaps
                // before querying cached points.
                //
                // Only a known prior as-of triggers this refetch. `acted == None` also
                // makes `follows_changed` true, but resubmitting its unanswered fetches on
                // every unrelated frame notification would duplicate work.
                if this
                    .acted
                    .is_some_and(|acted| Self::differs_on_followed(acted, now))
                {
                    this.in_flight.clear();
                    this.model.mark_all_fetching();
                    this.fetch_pending(cx);
                }
                // The barrier is answered on delivery instead, under the
                // versions this request was made with.
                this.requery(cx);
                // `mark_all_fetching` moved every chip's tone; nothing
                // else on this path re-prepares them.
                this.rebuild_chrome(cx);
            } else {
                this.self_arrive(now, cx);
            }
        })
        .detach();

        // Only the identities stage reads catalogue options. Ignore other
        // notifications, and retain the highlight when the option list is unchanged.
        // The source stage instead lists configured sources.
        cx.observe(&diagnostics, |this, _diagnostics, cx| {
            if !matches!(
                this.popup,
                Some(Popup::Picker(PickerState {
                    stage: PickerStage::Identities,
                    ..
                }))
            ) {
                return;
            }
            let options = this.catalog_options(cx);
            let loaded = this.loaded_marks(&options);
            let Some(Popup::Picker(p)) = &mut this.popup else {
                return;
            };
            if p.list.options() == options.as_slice() {
                return;
            }
            p.set_options(options, loaded);
            cx.notify();
        })
        .detach();

        // One derivation feeds both the chips' swatches and the chart
        // model's line colours — they are the same five colours, and
        // building the wheel twice is the thing `rebuild_chrome` exists
        // to avoid.
        let colours_ptr = Arc::as_ptr(&colours.borrow()) as usize;
        let colour_of = colour_fn(Arc::clone(&colours.borrow()), cx.theme());
        let header = HeaderModel::prepare(&model, settings.default_source.as_deref(), &colour_of);
        let title = header::title_text(&model);
        let offset_secs = local_offset_secs(cx);
        let chart = Arc::new(chart::build(
            &SeriesResult::default(),
            &model,
            1,
            offset_secs,
            &colour_of,
            settings.default_source.as_deref(),
        ));
        let last_chart_key = Some(chart_key(
            &model,
            0,
            settings.default_source.clone(),
            theme_signature(cx.theme()),
            colours_ptr,
            offset_secs,
        ));
        TimeseriesTile {
            id,
            frame,
            diagnostics,
            data,
            colours,
            model,
            result: None,
            result_seq: 0,
            chart,
            chart_version: 1,
            last_chart_key,
            theme_key: None,
            tag: 0,
            acted: None,
            query_in_flight: false,
            staged: None,
            last_flip: 0,
            visible: false,
            reset_view: false,
            in_flight: HashSet::new(),
            in_flight_range: None,
            notice: (!notices.is_empty()).then(|| notices.join("; ").into()),
            header,
            title,
            stack: None,
            popup: None,
            footer: header::footer_text(cx),
            chart_bounds: ChartBounds::default(),
            drag: None,
            colour_picker: None,
            pick_context: None,
        }
    }

    // ---- what the shell reads ----------------------------------------

    /// Add, expression, range, and colour editors use insert routing. Fieldless
    /// series/menu lists keep normal mode with their popup pair. Actual focus
    /// ownership is checked separately, including colour-picker descendants.
    pub fn key_context(&self) -> KeyContext {
        let mode = if self.popup.as_ref().is_some_and(Popup::is_insert) {
            "insert"
        } else {
            "normal"
        };
        let mut ctx = KeyContext::new("timeseries").pair("mode", mode).counts();
        if let Some(pair) = self.popup.as_ref().and_then(Popup::context_pair) {
            ctx = ctx.pair("popup", pair);
        }
        ctx
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

    /// On a hidden-to-visible transition, refetch every source pair. With a
    /// retained result and changed followed versions, also query immediately.
    /// Otherwise a successful fetch completion triggers the query, including
    /// `Ok(0)` when the data tier already covers the span.
    /// Hiding attempts query cancellation and clears request/fetch tracking.
    /// Cancellation has no acknowledgement and does not stop upstream fetches
    /// or retract results already emitted by the data tier.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            if self.model.source_slots().next().is_some() {
                self.in_flight.clear();
                self.model.mark_all_fetching();
                self.fetch_pending(cx);
            }
            let now = self.frame.read(cx).versions();
            if self.result.is_some() && !self.model.slots().is_empty() && self.follows_changed(now)
            {
                self.requery(cx);
            }
        } else {
            // An in-flight query nothing will paint is a round trip
            // spent for nothing.
            self.data.cancel(QueryKey(self.id.0));
            // Clear acted versions so showing a tile cannot treat its cancelled
            // request as completed work.
            self.acted = None;
            self.query_in_flight = false;
            self.in_flight.clear();
        }
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// This tile ignores the shell's find events; its local popup actions own
    /// series selection and identity filtering.
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = (event, window, cx);
    }

    // ---- keys --------------------------------------------------------

    /// `window` is forwarded for the popup verbs alone: they create a
    /// field and give the keyboard back up, neither of which is
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
        // Handled verbs replace the standing notice. Restore it on unhandled
        // paths so an inert action does not silently erase the last refusal.
        let previous = self.notice.take();
        let n = count.unwrap_or(1).max(1) as usize;
        // Close a popup before dispatching an action outside its allowed set.
        // An insert popup retains only commit, cancel, and insert navigation; a
        // palette-dispatched tile action must not leave an unfocused editor
        // reporting insert mode. Blur through the window-aware closer first.
        let popup_survives = match &self.popup {
            None => true,
            Some(p) if p.is_insert() => {
                matches!(verb, "commit" | "cancel" | "insert_up" | "insert_down")
            }
            // Keep the menu only for its own navigation, pick, and toggle actions.
            // A menu pick closes before dispatching its action; other tile actions also
            // close it before changing the model.
            Some(Popup::Menu(_)) => matches!(
                verb,
                "menu" | "list_down" | "list_up" | "list_close" | "menu_pick"
            ),
            // The fieldless series list stays open while slot properties change.
            Some(_) => matches!(
                verb,
                "list"
                    | "list_down"
                    | "list_up"
                    | "list_close"
                    | "toggle_visible"
                    | "axis_next"
                    | "axis_prev"
                    | "colour"
                    | "rule"
                    | "remove"
                    | "edit"
                    | "add"
                    | "expr"
                    | "range"
            ),
        };
        if !popup_survives {
            self.close_popup_with_window(window, cx);
        }
        let (now, as_of) = self.now_and_as_of(cx);
        // View movement has a separate path to preserve cached chart geometry.
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
            // Every popup verb, through the one door (`popups.rs`).
            "add" | "expr" | "edit" | "list" | "range" | "list_down" | "list_up" | "list_close"
            | "commit" | "cancel" | "insert_up" | "insert_down" | "menu" | "menu_pick"
            | "pick_colour" => {
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

    // ---- the `:` line ------------------------------------------------

    /// Commands may remove the colour picker's target. Forward the window so
    /// that orphaned popup can close through the focus-aware closer.
    pub fn command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
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
                // The picker's door too (`Self::add_pair`), which is why
                // this arm returns rather than falling through to the
                // shared tail: the door applies its own change.
                return self.add_pair(&identity, &source, cx);
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
            Command::AxisMode(m) => {
                let changed = self.model.set_axis_mode(m);
                // `full` is in the axis mode's own units — indices under
                // a session axis, micros under a continuous one — so the
                // extent has to be re-derived from the points already
                // held rather than waiting for the next delivery, which
                // a mode change does not ask for.
                if !changed.is_none()
                    && let Some(result) = self.result.clone()
                {
                    let full = self.full_of(&result);
                    self.model.set_full(full);
                    self.model.reset_view();
                }
                changed
            }
            Command::Freq(f) => self.model.set_frequency(f, now, &as_of)?,
            Command::Range(r) => self.model.set_range(r, now, &as_of)?,
            Command::Pct(p) => self.model.set_percentiles(p)?,
            Command::Density(d) => self.model.set_density(d)?,
            Command::YAxis(n, a) => self.model.set_axis(n, a)?,
            Command::Split(s) => self.model.set_split(s)?,
            Command::Clear => {
                let changed = self.model.clear();
                self.prune_in_flight();
                changed
            }
        };
        self.apply_changed(changed, cx);
        // `:remove` and `:clear` can take the slot an open picker was
        // opened for; a pick would then have nowhere to land.
        self.close_orphaned_colour_picker(window, cx);
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

    /// `1`..`5` is a palette index, `#rrggbb` an absolute colour,
    /// anything else a `[colours]` name (`commands::colour_arg`).
    fn colour_named(&self, name: &str) -> Result<Colour, String> {
        let colours = self.colours.borrow();
        commands::colour_arg(name, |n| colours.get(n).is_some())
    }

    // ---- the tails ---------------------------------------------------

    /// Apply model flags. Session serialization follows the shell's schedule.
    /// Submit FETCH before QUERY: range changes ask for missing coverage and
    /// query cached points immediately. Source adds request a fetch first;
    /// successful fetch completion supplies their query trigger.
    fn apply_changed(&mut self, changed: Changed, cx: &mut Context<Self>) {
        if let Some(n) = self.model.take_notice() {
            self.notice = Some(n.into());
        }
        if changed.fetch() {
            // The full range is about to change under the next delivery,
            // so the view follows it rather than staying where a
            // narrower range left it.
            self.reset_view = true;
            if self.visible {
                self.fetch_pending(cx);
            }
        }
        if changed.query() && self.visible && !self.model.slots().is_empty() {
            self.requery(cx);
        }
        if changed.chrome() || changed.query() || changed.fetch() {
            self.rebuild_chrome(cx);
        }
        cx.notify();
    }

    /// Apply a view move without rebuilding prepared chart data or its header.
    /// Query again only when visible-window statistics require it; with density
    /// and percentiles off, movement can reuse the current points and paths.
    fn view_moved(&mut self, changed: Changed, cx: &mut Context<Self>) {
        if changed.query() && self.visible && !self.model.slots().is_empty() {
            self.requery(cx);
        }
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

    /// Shared keyboard/command removal. Remove dependent expressions with their
    /// operand and name the additional removed slots in a notice.
    fn remove(&mut self, number: u8) -> Result<Changed, String> {
        let removal = self.model.remove(number)?;
        self.prune_in_flight();
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

    /// Prepare header, title, and open series-list rows. Rebuild the immutable
    /// chart input only when [`ChartKey`] changes, bumping its version so the
    /// chart element invalidates geometry derived from that input.
    /// Resolve one colour mapping for both chip swatches and chart lines.
    fn rebuild_chrome(&mut self, cx: &mut Context<Self>) {
        let default_source = cx
            .try_global::<SeriesSettings>()
            .and_then(|s| s.default_source.clone());
        let theme = theme_signature(cx.theme());
        let colours_ptr = Arc::as_ptr(&self.colours.borrow()) as usize;
        let colour_of = colour_fn(Arc::clone(&self.colours.borrow()), cx.theme());
        self.header = HeaderModel::prepare(&self.model, default_source.as_deref(), &colour_of);
        self.title = header::title_text(&self.model);
        // List rows have inputs outside the chart key, including fetch state and
        // provenance. Refresh them even when chart geometry can be reused.
        if matches!(self.popup, Some(Popup::Series(_))) {
            let rows = SeriesPopup::prepare(
                &self.model,
                self.result.as_deref(),
                default_source.as_deref(),
                &colour_of,
            );
            self.popup = Some(Popup::Series(rows));
        }
        // The menu's rows read the cursor slot the same way, and a `:`
        // line runs under an open menu (the menu context leaves `:` to
        // the tile). The highlight stays on its row where that row is
        // still an action, else lands on the first enabled one.
        if matches!(self.popup, Some(Popup::Menu(_))) {
            let rows = self.menu_rows(cx);
            if let Some(Popup::Menu(m)) = &mut self.popup {
                m.highlighted = menu::step(&rows, m.highlighted, 0);
                m.rows = rows;
            }
        }
        let offset_secs = local_offset_secs(cx);
        let key = chart_key(
            &self.model,
            self.result_seq,
            default_source.clone(),
            theme,
            colours_ptr,
            offset_secs,
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
            offset_secs,
            &colour_of,
            default_source.as_deref(),
        );
        self.chart = Arc::new(chart);
        self.last_chart_key = Some(key);
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

    /// What the tile has open — the popup's own "painted text" is its
    /// PREPARED rows, read here the way the header's chips are.
    #[cfg(test)]
    pub(crate) fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn pick_context(&self) -> Option<&PickContext> {
        self.pick_context.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn chart(&self) -> &Arc<ChartModel> {
        &self.chart
    }

    /// The versions the request in flight was made under — what an open
    /// barrier is keyed by, and so what a test asking "did that delivery
    /// arrive?" has to hand `barrier_wants`.
    #[cfg(test)]
    pub(crate) fn acted(&self) -> Option<FrameVersions> {
        self.acted
    }
}

impl TimeseriesTile {
    /// Render the chart, record its bounds during canvas prepaint, and show a
    /// divider cursor where two panes meet. During a drag, an occluding catcher
    /// handles moves and releases over the surface plus releases outside it.
    fn render_chart_surface(
        &self,
        tile: &Entity<TimeseriesTile>,
        tile_id: u64,
        window: &Window,
    ) -> impl IntoElement {
        let rem_px = window.rem_size().as_f32();
        let bounds_cell = self.chart_bounds.clone();
        let divider = self.divider_rect(rem_px);
        let drag = self.drag;
        div()
            .id(ElementId::Name(SharedString::new_static(
                "ts-chart-surface",
            )))
            .debug_selector(move || format!("timeseries-chart-{tile_id}"))
            .relative()
            .flex_1()
            .min_h_0()
            .child(ChartElement::new(
                self.chart.clone(),
                self.model.view(),
                rem_px,
                // Unique per tile: `Buffers` and both path caches
                // hang off this id, and two charts sharing one serve
                // each other's paths.
                ElementId::NamedInteger(SharedString::new_static("ts-chart"), tile_id),
            ))
            .child(
                canvas(
                    move |bounds, _window, _cx| bounds_cell.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .on_scroll_wheel({
                let tile = tile.clone();
                move |event: &ScrollWheelEvent, window, cx| {
                    tile.update(cx, |t, cx| t.wheel(event, window, cx));
                }
            })
            .on_mouse_down(MouseButton::Left, {
                let tile = tile.clone();
                move |event: &MouseDownEvent, window, cx| {
                    tile.update(cx, |t, cx| t.chart_pressed(event, window, cx));
                }
            })
            // The divider affordance: no listener of its own — the
            // surface's press hit-tests the band — just the cursor that
            // says the gap can be dragged.
            .when_some(divider, |el, band| {
                el.child(
                    div()
                        .absolute()
                        .left(px(band.x))
                        .top(px(band.y))
                        .w(px(band.w))
                        .h(px(band.h))
                        .cursor_row_resize()
                        .debug_selector(move || format!("timeseries-divider-{tile_id}")),
                )
            })
            .when_some(drag, |el, drag| {
                el.child(
                    div()
                        .id(ElementId::Name(SharedString::new_static("ts-drag-catcher")))
                        .debug_selector(move || format!("timeseries-drag-catcher-{tile_id}"))
                        .absolute()
                        .inset_0()
                        .occlude()
                        .map(|el| match drag {
                            Drag::Pan { .. } => el.cursor_grabbing(),
                            Drag::Split => el.cursor_row_resize(),
                        })
                        .on_mouse_move({
                            let tile = tile.clone();
                            move |event: &MouseMoveEvent, window, cx| {
                                tile.update(cx, |t, cx| t.drag_moved(event, window, cx));
                            }
                        })
                        .on_mouse_up(MouseButton::Left, {
                            let tile = tile.clone();
                            move |_, _window, cx| {
                                tile.update(cx, |t, cx| t.drag_finished(cx));
                            }
                        })
                        .on_mouse_up_out(MouseButton::Left, {
                            let tile = tile.clone();
                            move |_, _window, cx| {
                                tile.update(cx, |t, cx| t.drag_finished(cx));
                            }
                        }),
                )
            })
    }
}

impl Render for TimeseriesTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Prepared chip and chart colours depend on the full theme signature.
        // Also check the shared named-colour Arc for reloads. A changed key rebuilds
        // prepared content here; unchanged renders retain it.
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
            header::render_empty(theme, &tile, tile_id).into_any_element()
        } else {
            self.render_chart_surface(&tile, tile_id, window)
                .into_any_element()
        };
        let menu_open = matches!(self.popup, Some(Popup::Menu(_)));
        // Anchor the deferred popup at the header's right edge. The relative
        // wrapper supplies its positioning context; deferral paints over the chart.
        let popup = match self.popup.as_ref() {
            Some(Popup::Series(s)) => Some(render_series_popup(
                s,
                self.header.cursor,
                &tile,
                tile_id,
                cx,
            )),
            Some(Popup::Picker(p)) => Some(render_picker(p, &tile, tile_id, cx)),
            Some(Popup::Range(r)) => Some(render_range(r, &tile, tile_id, cx)),
            Some(Popup::Menu(m)) => Some(render_menu(m, &tile, tile_id, cx)),
            // The expression field is not an overlay: it is a strip in
            // the body, below; the colour picker is drawn in its target
            // chip, by the header.
            Some(Popup::Expr(_)) | Some(Popup::Colour(_)) | None => None,
        };
        // Render the component trigger in its target chip only while open. State
        // and subscriptions persist on the tile; the popover element state is transient.
        let colour_picker = match self.popup.as_ref() {
            Some(Popup::Colour(c)) => {
                let target = c.target;
                Some((
                    target,
                    div()
                        .debug_selector(move || {
                            format!("timeseries-colour-picker-{tile_id}-{target}")
                        })
                        .child(
                            ColorPicker::new(&c.picker)
                                .featured_colors(c.swatches.clone())
                                .accessibility_label(SharedString::new_static("Series colour"))
                                .xsmall(),
                        )
                        .into_any_element(),
                ))
            }
            _ => None,
        };
        let expr_field = match self.popup.as_ref() {
            Some(Popup::Expr(f)) => Some(header::render_expr_field(f, theme)),
            _ => None,
        };
        let header = div()
            .relative()
            .w_full()
            .child(header::render_header(
                &self.header,
                theme,
                &tile,
                tile_id,
                self.stack.as_ref(),
                menu_open,
                colour_picker,
            ))
            .when_some(popup, |el, popup_el| {
                el.child(
                    div()
                        .absolute()
                        .right_0()
                        .top(scale::design(header::HEADER_HEIGHT))
                        .child(popup_el),
                )
            });
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(header)
            .when_some(self.notice.clone(), |el, n| {
                el.child(header::render_notice(&n, theme))
            })
            .when_some(expr_field, |el, f| el.child(f))
            .child(body)
            .child(header::render_footer(self.footer.clone(), theme))
    }
}

/// What one `enter` in the picker turns out to mean, decided while the
/// popup is borrowed and acted on once it is not.
enum Commit {
    /// Inert: nothing ranked and nothing typed. The picker stays open.
    Nothing,
    Add(String, String),
    /// The typed identity opens the source stage.
    Stage(String),
}

/// One of the picker's OWN options, built here as `{identity}@{source}`:
/// the source is the last `@` piece, so an identity carrying an `@` (a
/// REST path a catalogue offered) splits correctly. A bare option — the
/// sources stage's — is its own identity with no source, which only the
/// `loaded` marks ever ask about.
fn split_option(option: &str) -> (&str, &str) {
    option.rsplit_once('@').unwrap_or((option, ""))
}

/// `identity@source` as a trader TYPED it: exactly one `@`, both sides
/// non-empty. Anything else is not a pair — a text with no `@` is an
/// identity awaiting a source, and one with two is neither.
fn parse_pair(text: &str) -> Option<(&str, &str)> {
    let (identity, source) = text.split_once('@')?;
    if identity.is_empty() || source.is_empty() || source.contains('@') {
        return None;
    }
    Some((identity, source))
}

/// Build a [`ChartKey`] from everything `chart::build` will read. A free
/// function, not a method, so the constructor — which has no `Self` yet
/// — records the same key the first model was built from.
fn chart_key(
    model: &Model,
    result: u64,
    default_source: Option<String>,
    theme: [Hsla; 28],
    colours: usize,
    offset_secs: i32,
) -> ChartKey {
    ChartKey {
        result,
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
        offset_secs,
    }
}

/// A slot's colour on this theme: a palette index through the floored
/// five chart colours, a `[colours]` name through the shared wheel, an
/// absolute colour as itself, and a name the trader has since deleted
/// back to the first palette colour rather than an error — a stale name
/// costs a colour, never a tile.
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
        // Absolute: no theme, no readability floor — what was picked.
        Colour::Custom(c) => c.to_hsla(),
    }
}

/// One display offset sampled at the current instant from `AppClock`.
/// Stored/query timestamps remain UTC. Without an installed global, use
/// `Clock::machine`; the chart does not resolve historical offsets per point.
fn local_offset_secs(cx: &App) -> i32 {
    let clock = cx
        .try_global::<geode_shell::clock::AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
    clock.local(Utc::now()).offset().fix().local_minus_utc()
}

#[cfg(test)]
mod tests;
