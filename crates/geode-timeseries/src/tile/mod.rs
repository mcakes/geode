//! The shell-hosted timeseries entity: slot model, requests, prepared header,
//! chart input, and one tile-owned popup.
//!
//! Model changes carry [`Changed`] flags into
//! `TimeseriesTile::apply_changed`, which schedules fetches and queries and
//! refreshes prepared content. View movement uses `TimeseriesTile::view_moved`
//! to retain the chart's cached paths; only visible-window statistics need a
//! new query. Refusals become notices or inline popup errors.
//!
//! `data` owns fetch tracking and tagged series delivery over
//! `geode_tile::following` (the flip-barrier staging). Only the frame's as-of
//! counter invalidates an established series request; flip releases staged
//! results without triggering a query.
//! `popups` owns opening, input, commit, and dismissal for local editors.

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
use crate::core::complete::{Write, expand_unique};
use crate::core::menu::MenuKind;
use crate::core::model::{Changed, Color, Model, SlotState};
use crate::core::{
    Range, Rgb8, chart, color_from_pick, menu, request, resolve, session, within_a_step,
};
use crate::header::{self, HeaderModel};
use crate::popup::{
    ColorPick, DateFieldPaint, ExprField, MenuState, PickContext, PickerStage, PickerState, Popup,
    PopupKind, RangePopup, SeriesPopup, Which, render_picker, render_range, render_series_popup,
};
use crate::tile::pointer::{ChartBounds, Drag};
use geode_tile::following::{Delivered, FollowingQuery, FrameDoor, Promotion, Unanswered};
use geode_tile::menu::{Menu, MenuHost, MenuIds, Row};

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
    slots: Vec<(u8, Color, Axis, bool, Option<String>)>,
    frequency: Frequency,
    axis_mode: AxisMode,
    /// Bit pattern, because `f32` is not `Eq` and a split is compared,
    /// never arithmetic'd, here.
    split: u32,
    density: bool,
    /// Read by `Model::label` for a slot whose source is not the default.
    default_source: Option<String>,
    /// The two inputs to `color_fn`: a slot's color is resolved INTO
    /// the chart model, so a theme change or a reloaded `colors.toml`
    /// (a fresh `Arc`, which is what `set_colours` swaps in) is a chart
    /// change.
    theme: [Hsla; 28],
    colors: usize,
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
    colors: Rc<RefCell<Arc<NamedColours>>>,
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
    /// chips' swatches and the chart model's line colors are both
    /// resolved against it.
    theme_key: Option<[Hsla; 28]>,
    /// The series query under the flip barrier (see `geode_tile::following`):
    /// only the frame's as-of invalidates it.
    following: FollowingQuery<SeriesResult>,
    /// A view move is waiting for the request in flight to answer before it
    /// asks for its own window's statistics. See [`Self::view_moved`].
    view_waiting: bool,
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
    /// One local popup: series list, add picker, expression editor, custom dates
    /// editor, menu, or color picker. List and menu rows are prepared with chrome;
    /// date segments are prepared by field transitions rather than during render.
    popup: Option<Popup>,
    footer: Vec<header::FooterHint>,
    /// The chart surface's last painted bounds (`tile::pointer`).
    chart_bounds: ChartBounds,
    /// The pointer gesture in progress, if any (`tile::pointer`).
    drag: Option<Drag>,
    /// Lazily created reusable component state with one set of subscriptions.
    /// The header renders its trigger while Popup::Color is active.
    color_picker: Option<Entity<ColorPickerState>>,
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
        colors: Rc<RefCell<Arc<NamedColours>>>,
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
            // The default source decides which labels are bare, so an open
            // expression field's list is relabelled rather than left
            // offering a name that no longer resolves.
            this.refresh_expr_completion(cx);
            this.rebuild_chrome(cx);
            cx.notify();
        })
        .detach();
        // The footer and an open menu name live chords, so a keymap reload
        // re-resolves both — once, here, never per frame.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.footer = header::footer_hints(cx);
            if let Some(Popup::Menu(m)) = &mut this.popup {
                m.menu.rehint(&geode_tile::menu::live_bindings(cx));
            }
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
            // The post-step: every promotion that took something releases a
            // view move waiting behind it.
            match this.following.on_flip(now, Self::differs_on_followed) {
                Promotion::Empty => {}
                Promotion::Superseded => this.release_view(cx),
                Promotion::Apply(result) => {
                    this.apply_result(result, cx);
                    cx.notify();
                    this.release_view(cx);
                }
            }
            // An open frequency menu's disabled rows are the point cap
            // over the range AS RESOLVED under the frame's as-of, so any
            // frame change may move them — and the chrome rebuild below
            // runs only for a visible tile holding series. Six cap checks,
            // and a repaint only when a row actually moved.
            if this.refresh_menu_rows(cx) {
                cx.notify();
            }
            if !this.visible {
                return;
            }
            // Established series requests follow as-of only. Frame scope, grouping,
            // and unrelated dataset publications do not change their inputs.
            if !this.model.slots().is_empty()
                && this
                    .following
                    .follows_changed(now, Self::differs_on_followed)
            {
                // Changing as-of can move both ends of a relative range. Clear fetch
                // tracking even though the stored Range is unchanged, then ask for gaps
                // before querying cached points.
                //
                // Only a known prior as-of triggers this refetch. `acted == None` also
                // makes `follows_changed` true, but resubmitting its unanswered fetches on
                // every unrelated frame notification would duplicate work.
                if this
                    .following
                    .acted()
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
                let key = QueryKey(this.id.0);
                this.following
                    .self_arrive(&mut FrameDoor::new(&this.frame, cx), key, now);
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

        // Share theme-derived palette and named-color inputs between chip
        // swatches and chart lines so their colors agree without duplicate setup.
        let colors_ptr = Arc::as_ptr(&colors.borrow()) as usize;
        let color_of = color_fn(Arc::clone(&colors.borrow()), cx.theme());
        let header = HeaderModel::prepare(&model, settings.default_source.as_deref(), &color_of);
        let title = header::title_text(&model);
        let offset_secs = local_offset_secs(cx);
        let chart = Arc::new(chart::build(
            &SeriesResult::default(),
            &model,
            1,
            offset_secs,
            &color_of,
            settings.default_source.as_deref(),
        ));
        let last_chart_key = Some(chart_key(
            &model,
            0,
            settings.default_source.clone(),
            theme_signature(cx.theme()),
            colors_ptr,
            offset_secs,
        ));
        TimeseriesTile {
            id,
            frame,
            diagnostics,
            data,
            colors,
            model,
            result: None,
            result_seq: 0,
            chart,
            chart_version: 1,
            last_chart_key,
            theme_key: None,
            following: FollowingQuery::new(),
            view_waiting: false,
            visible: false,
            reset_view: false,
            in_flight: HashSet::new(),
            in_flight_range: None,
            notice: (!notices.is_empty()).then(|| notices.join("; ").into()),
            header,
            title,
            stack: None,
            popup: None,
            footer: header::footer_hints(cx),
            chart_bounds: ChartBounds::default(),
            drag: None,
            color_picker: None,
            pick_context: None,
        }
    }

    // ---- what the shell reads ----------------------------------------

    /// Add, expression, dates-editor, and color editors use insert routing.
    /// Fieldless series/menu lists keep normal mode with their popup pair, and a
    /// menu adds a `menu` pair naming its kind. Actual focus ownership is checked
    /// separately, including color-picker descendants.
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
        if let Some(kind) = self.popup.as_ref().and_then(Popup::menu_pair) {
            ctx = ctx.pair("menu", kind);
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
    /// Hiding keeps the series query; closing (`closed`) cancels it.
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
            if self.result.is_some()
                && !self.model.slots().is_empty()
                && self
                    .following
                    .follows_changed(now, Self::differs_on_followed)
            {
                self.requery(cx);
            }
        } else {
            // Hidden tiles hear no fetch completions (the shell broadcasts
            // `SeriesFetched` to visible tiles only), so fetch tracking is
            // dropped and every show refetches. The series query itself is
            // kept: its answer applies when it lands.
            self.view_waiting = false;
            self.in_flight.clear();
        }
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// The shell is removing this tile: cancel the series query by key and
    /// answer any barrier still waiting on it. Fetches run on; their
    /// completions reach no one. Runs inside the shell's occupant
    /// reconciliation, so it updates only the frame and the data handle.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        let key = QueryKey(self.id.0);
        self.data.cancel(key);
        self.following
            .close(&mut FrameDoor::new(&self.frame, cx), key);
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
            // Keep a menu only for its own navigation, pick, and toggle actions,
            // plus the three openers, so each can toggle its own menu shut or swap
            // one menu for another. A menu pick closes before dispatching its
            // action; other tile actions also close it before changing the model.
            Some(Popup::Menu(_)) => matches!(
                verb,
                "menu"
                    | "list_down"
                    | "list_up"
                    | "list_close"
                    | "menu_pick"
                    | "range"
                    | "freq"
                    | "range_custom"
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
                    | "color"
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
            "color" => self.model.cycle_color(),
            "rule" => self.model.cycle_rule(),
            "remove" => self.remove_at_cursor(cx),
            "density" => self.model.toggle_density(),
            "percentiles" => self.model.toggle_percentiles(),
            "pan_left" => self.model.pan(-(n as i32)),
            "pan_right" => self.model.pan(n as i32),
            "zoom_in" => self.model.zoom_in(n),
            "zoom_out" => self.model.zoom_out(n),
            "reset_view" => self.model.reset_view(),
            "jump_start" => self.model.jump_start(),
            "jump_end" => self.model.jump_end(),
            // Every popup verb, through the one door (`popups.rs`).
            "add" | "expr" | "edit" | "list" | "range" | "range_custom" | "freq" | "list_down"
            | "list_up" | "list_close" | "commit" | "cancel" | "insert_up" | "insert_down"
            | "menu" | "menu_pick" | "pick_color" => {
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

    /// Commands may remove the color picker's target. Forward the window so
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
                )?;
                self.model.add_expr(&text, e)?.1
            }
            Command::Remove(name) => self.remove(self.target(name, &settings)?, cx)?,
            Command::Rule(name, r) => self.model.set_rule(self.target(name, &settings)?, r)?,
            Command::Color(name, word) => {
                let n = self.target(name, &settings)?;
                let color = self.color_named(&word)?;
                self.model.set_color(n, color)?
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
            Command::YAxis(name, a) => self.model.set_axis(self.target(name, &settings)?, a)?,
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
        self.close_orphaned_color_picker(window, cx);
        Ok(())
    }

    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        let settings = cx.try_global::<SeriesSettings>();
        let names = self
            .model
            .series_names(settings.and_then(|s| s.default_source.as_deref()));
        let sources = settings.map(|s| s.names()).unwrap_or_default();
        let colors: Vec<String> = self.colors.borrow().names().map(str::to_string).collect();
        commands::completions(line, cursor, &names, &sources, &colors)
    }

    /// The slot a series verb acts on: the named source series, else the
    /// selection (`Model::target`).
    fn target(&self, name: Option<String>, settings: &SeriesSettings) -> Result<u8, String> {
        self.model
            .target(name.as_deref(), settings.default_source.as_deref())
    }

    /// `1`..`5` is a palette index, `#rrggbb` an absolute color,
    /// anything else a `[colors]` name (`commands::color_arg`).
    fn color_named(&self, name: &str) -> Result<Color, String> {
        let colors = self.colors.borrow();
        commands::color_arg(name, |n| colors.get(n).is_some())
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
    /// and percentiles off, movement reuses the current points. The chart
    /// element rebuilds paths for the changed viewport.
    ///
    /// While a request is in flight the move waits for its answer instead of
    /// superseding it: the pool interrupts a superseded query, so wheel or drag
    /// events arriving faster than one query runs would leave the statistics
    /// frozen until the pan stopped. The answer releases one request for the
    /// latest view ([`Self::release_view`]).
    fn view_moved(&mut self, changed: Changed, cx: &mut Context<Self>) {
        if changed.query() && self.visible && !self.model.slots().is_empty() {
            if self.following.in_flight() {
                self.view_waiting = true;
            } else {
                self.requery(cx);
            }
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

    fn remove_at_cursor(&mut self, cx: &App) -> Changed {
        let Some(number) = self.model.cursor_slot().map(|s| s.number) else {
            return Changed::NONE;
        };
        match self.remove(number, cx) {
            Ok(changed) => changed,
            Err(e) => {
                self.notice = Some(e.into());
                Changed::CHROME
            }
        }
    }

    /// Shared keyboard/command removal. Remove dependent expressions with their
    /// operand and name the additional removed slots in a notice.
    fn remove(&mut self, number: u8, cx: &App) -> Result<Changed, String> {
        // Labelled before the removal: the removed slots are gone after it.
        let default_source = cx
            .try_global::<SeriesSettings>()
            .and_then(|s| s.default_source.clone());
        let label_of = |n: u8| {
            self.model
                .slot_by_number(n)
                .map(|s| s.label(default_source.as_deref()))
        };
        let named = label_of(number);
        let dependants: Vec<String> = self
            .model
            .dependants(number)
            .into_iter()
            .filter_map(label_of)
            .collect();
        let removal = self.model.remove(number)?;
        self.prune_in_flight();
        if let Some(named) = named
            && !dependants.is_empty()
        {
            self.notice =
                Some(format!("removed {named} and, with it, {}", dependants.join(", ")).into());
        }
        Ok(removal.changed)
    }

    /// Rebuild an open menu's rows over the model and the frame as they are
    /// now, keeping the highlight on its row where that row is still an action,
    /// else snapping it to the nearest action. Answers whether the rows moved.
    fn refresh_menu_rows(&mut self, cx: &App) -> bool {
        let Some(Popup::Menu(m)) = &self.popup else {
            return false;
        };
        let rows = self.menu_rows(m.kind, cx);
        let bindings = geode_tile::menu::live_bindings(cx);
        let Some(Popup::Menu(m)) = &mut self.popup else {
            return false;
        };
        m.menu.replace_rows(rows, &bindings)
    }

    /// Prepare header, title, open series-list rows and an open menu's rows.
    /// Rebuild the immutable chart input only when [`ChartKey`] changes, bumping
    /// its version so the chart element invalidates geometry derived from that
    /// input. Resolve one color mapping for both chip swatches and chart lines.
    fn rebuild_chrome(&mut self, cx: &mut Context<Self>) {
        let default_source = cx
            .try_global::<SeriesSettings>()
            .and_then(|s| s.default_source.clone());
        let theme = theme_signature(cx.theme());
        let colors_ptr = Arc::as_ptr(&self.colors.borrow()) as usize;
        let color_of = color_fn(Arc::clone(&self.colors.borrow()), cx.theme());
        self.header = HeaderModel::prepare(&self.model, default_source.as_deref(), &color_of);
        self.title = header::title_text(&self.model);
        // List rows have inputs outside the chart key, including fetch state and
        // provenance. Refresh them even when chart geometry can be reused.
        if matches!(self.popup, Some(Popup::Series(_))) {
            let rows = SeriesPopup::prepare(
                &self.model,
                self.result.as_deref(),
                default_source.as_deref(),
                &color_of,
            );
            self.popup = Some(Popup::Series(rows));
        }
        // A menu's rows read the model the same way — the action list
        // the cursor slot, the range menu the range, the frequency menu
        // the frequency and the cap over the range — and a `:` line runs
        // under an open menu (the menu context leaves `:` to the tile).
        // The highlight stays on its row where that row is still an
        // action, else snaps to the nearest action.
        self.refresh_menu_rows(cx);
        let offset_secs = local_offset_secs(cx);
        let key = chart_key(
            &self.model,
            self.result_seq,
            default_source.clone(),
            theme,
            colors_ptr,
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
            &color_of,
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
        self.following.acted()
    }
}

/// Which header control owns the popup that is up — each paints its
/// open state while it does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TriggersOpen {
    /// `⋯`: the action list.
    pub actions: bool,
    /// The range trigger: the range menu, or the dates editor opened
    /// from it.
    pub range: bool,
    /// The frequency trigger: the frequency menu.
    pub frequency: bool,
}

impl TimeseriesTile {
    /// Which trigger owns the open popup (`TriggersOpen`).
    pub(crate) fn triggers_open(&self) -> TriggersOpen {
        match &self.popup {
            Some(Popup::Menu(m)) => TriggersOpen {
                actions: m.kind == MenuKind::Actions,
                range: m.kind == MenuKind::Range,
                frequency: m.kind == MenuKind::Frequency,
            },
            Some(Popup::Range(_)) => TriggersOpen {
                range: true,
                ..TriggersOpen::default()
            },
            _ => TriggersOpen::default(),
        }
    }

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
        // Prepared chip and chart colors depend on the full theme signature.
        // Also check the shared named-color Arc for reloads. A changed key rebuilds
        // prepared content here; unchanged renders retain it.
        let signature = theme_signature(cx.theme());
        let colors_ptr = Arc::as_ptr(&self.colors.borrow()) as usize;
        let colors_moved = self
            .last_chart_key
            .as_ref()
            .is_none_or(|k| k.colors != colors_ptr);
        if self.theme_key != Some(signature) || colors_moved {
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
        let open = self.triggers_open();
        // The range menu and the dates editor hang under the range trigger, the
        // frequency menu under the frequency trigger; the header places them.
        let under_range = match self.popup.as_ref() {
            Some(Popup::Range(r)) => Some(render_range(r, &tile, tile_id, cx).into_any_element()),
            Some(Popup::Menu(m)) if m.kind == MenuKind::Range => {
                Some(paint_menu(m, &tile, cx).into_any_element())
            }
            _ => None,
        };
        let under_freq = match self.popup.as_ref() {
            Some(Popup::Menu(m)) if m.kind == MenuKind::Frequency => {
                Some(paint_menu(m, &tile, cx).into_any_element())
            }
            _ => None,
        };
        // Anchor every other deferred popup at the header's right edge. The
        // relative wrapper supplies its positioning context; deferral paints over
        // the chart.
        let popup = match self.popup.as_ref() {
            Some(Popup::Series(s)) => Some(render_series_popup(
                s,
                self.header.cursor,
                &tile,
                tile_id,
                cx,
            )),
            Some(Popup::Picker(p)) => Some(render_picker(p, &tile, tile_id, cx)),
            Some(Popup::Menu(m)) if m.kind == MenuKind::Actions => Some(paint_menu(m, &tile, cx)),
            // The expression field is not an overlay: it is a strip in
            // the body, below; the color picker is drawn in its target
            // chip, and the range and frequency popups under their
            // triggers, by the header.
            Some(Popup::Range(_))
            | Some(Popup::Menu(_))
            | Some(Popup::Expr(_))
            | Some(Popup::Color(_))
            | None => None,
        };
        // Render the component trigger in its target chip only while open. State
        // and subscriptions persist on the tile; the popover element state is transient.
        let color_picker = match self.popup.as_ref() {
            Some(Popup::Color(c)) => {
                let target = c.target;
                Some((
                    target,
                    div()
                        .debug_selector(move || {
                            format!("timeseries-color-picker-{tile_id}-{target}")
                        })
                        .child(
                            ColorPicker::new(&c.picker)
                                .featured_colors(c.swatches.clone())
                                .accessibility_label(SharedString::new_static("Series color"))
                                .xsmall(),
                        )
                        .into_any_element(),
                ))
            }
            _ => None,
        };
        let expr_field = match self.popup.as_ref() {
            Some(Popup::Expr(f)) => Some(header::render_expr_field(f, &tile, tile_id, cx)),
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
                header::HeaderPopups {
                    menu_open: open.actions,
                    range_open: open.range,
                    freq_open: open.frequency,
                    color_picker,
                    under_range,
                    under_freq,
                },
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
            .child(header::render_footer(&self.footer, theme))
    }
}

/// Paint an open menu through the door: the action list hangs from the
/// header's right edge, the range and frequency menus under their triggers.
/// A press outside closes it only while THIS menu is still up: a press on
/// another menu's trigger runs first (capture phase) and has already swapped
/// its own menu in, which this press must not close.
fn paint_menu(m: &MenuState, tile: &Entity<TimeseriesTile>, cx: &App) -> gpui::Deferred {
    let kind = m.kind;
    geode_tile::menu::render_menu(
        &m.menu,
        &m.ids,
        match kind {
            MenuKind::Actions => gpui::Anchor::TopRight,
            MenuKind::Range | MenuKind::Frequency => gpui::Anchor::TopLeft,
        },
        tile,
        move |t: &mut TimeseriesTile, window, cx| {
            t.outside_press(PopupKind::Menu(kind), window, cx)
        },
        cx,
    )
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
    colors: usize,
    offset_secs: i32,
) -> ChartKey {
    ChartKey {
        result,
        slots: model
            .slots()
            .iter()
            .map(|s| (s.number, s.color.clone(), s.axis, s.visible, s.text.clone()))
            .collect(),
        frequency: model.frequency(),
        axis_mode: model.axis_mode(),
        split: model.split().to_bits(),
        density: model.density().is_some(),
        default_source,
        theme,
        colors,
        offset_secs,
    }
}

/// A slot's color on this theme: a palette index through the floored
/// five chart colors, a `[colors]` name through the shared wheel, an
/// absolute color as itself, and a name the trader has since deleted
/// back to the first palette color rather than an error — a stale name
/// costs a color, never a tile.
///
/// Takes the definitions by `Arc` and the theme's derived pair by value
/// so the returned closure borrows NOTHING: `rebuild_chrome` holds
/// it while it assigns `self.chart`.
fn color_fn(colors: Arc<NamedColours>, theme: &Theme) -> impl Fn(&Color) -> Hsla {
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
    move |color| match color {
        Color::Palette(i) => palette.colour(*i),
        Color::Named(name) => match colors.get(name) {
            Some(def) => to_hsla(geode_core::colour::resolve(def, &anchors, &tokens)),
            None => palette.colour(0),
        },
        // Absolute: no theme, no readability floor — what was picked.
        Color::Custom(c) => c.to_hsla(),
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
