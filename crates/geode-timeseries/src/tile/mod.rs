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
//! **The data half** hangs off the first two: a FETCH asks the data tier
//! for every pair still waiting (`fetch_pending`), a `SeriesFetched Ok`
//! sends the query, a QUERY asks for points over the range and stats
//! over the visible window (`requery`), and the answer lands through
//! `deliver` — staged behind the flip barrier when one is open over this
//! tile, painted at once when it is not. `as_of` is the only frame
//! counter followed (spec §6.5); `flip` is read in the frame observer
//! and nowhere else, where it means "you may promote". That flow lives in
//! [`data`]; every popup's open, keys, commit and close in [`popups`].

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
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Theme, v_flex};

use crate::commands::{self, Command};
use crate::core::model::{Changed, Colour, Model, SlotState};
use crate::core::{Preset, Range, chart, menu, request, resolve, session};
use crate::header::{self, HeaderModel};
use crate::popup::{
    DateFieldPaint, ExprField, MenuState, PickerStage, PickerState, Popup, RangePopup, SeriesPopup,
    Which, render_menu, render_picker, render_range, render_series_popup,
};
use crate::tile::pointer::{ChartBounds, Drag};

mod data;
mod pointer;
mod popups;

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
/// source slot's `source`/`identity` (its LABEL is read, and a slot
/// number is never reused while any slot lives — `:clear` restarts the
/// numbering, but it also installs an empty key, so a re-added number
/// can never match a pre-clear entry), its `rule` and the model's
/// `percentiles` (neither reaches the
/// chart model — they shape the REQUEST, and the answer arrives as a new
/// `result`), and the view (the element takes it beside the model).
#[derive(Clone, PartialEq)]
struct ChartKey {
    /// The result's identity: [`TimeseriesTile::result_seq`], bumped on
    /// every install. A monotonic counter and NOT the `Arc`'s address,
    /// which is ABA-prone — the allocator hands the same block back when
    /// one result replaces another between two frames, and the chart
    /// would then paint the old points at the new model's key. `0` for
    /// no result.
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
    /// The catalogue the add picker's identities stage ranks over, and
    /// the load-lane health a slot's popup row reports. Observed as well
    /// as read: a fresh catalogue while that stage is open re-ranks it.
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
    /// The last flip counter this tile promoted at.
    last_flip: u64,
    visible: bool,
    /// The next delivery resets the view to the new full range.
    reset_view: bool,
    /// The `(source, identity)` pairs whose fetch has been submitted and
    /// not yet answered.
    ///
    /// `SlotState::Fetching` alone cannot decide what to ask for: it
    /// means "this slot is waiting for data", and an `add` leaves every
    /// EARLIER unanswered slot in that state too, so a second add would
    /// re-ask for the first one's span on every keystroke. This set is
    /// the "already asked" half, cleared wherever the answer stops
    /// applying — a hide (which drops what is in flight), a show (whose
    /// contract is that every show refetches) and a range change
    /// ([`Self::in_flight_range`], since the span itself moved).
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
    /// The tile's one overlay: the series list, the add picker, the
    /// expression editor or the range dialog — one at a time, which is
    /// what this being an `Option<Popup>` rather than four fields
    /// enforces. The list's rows are PREPARED in `rebuild_chrome` and
    /// the range dialog's segments in its own key handler, never
    /// formatted in `render`.
    popup: Option<Popup>,
    footer: SharedString,
    /// The chart surface's last painted bounds (`tile::pointer`).
    chart_bounds: ChartBounds,
    /// The pointer gesture in progress, if any (`tile::pointer`).
    drag: Option<Drag>,
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
        // Nothing here opens a field, so `window` is unused; the
        // parameter is the roster's own `create` signature, kept so a
        // tile that one day restores an open editor is a body change.
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
        // The chart's displayed times follow the app clock (as-of dialog
        // spec §6.1): a `[time] zone` reload moves `offset_secs`, which
        // `ChartKey` carries, so this rebuild is a real one.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.rebuild_chrome(cx);
            cx.notify();
        })
        .detach();
        cx.observe(&frame, |this, frame, cx| {
            // A flip released (Phase 4a §3.10): promote whatever is
            // staged, REGARDLESS of visibility — a tile hidden between
            // staging and the flip must not come back showing the old
            // as-of's points. `flip` is read here and nowhere else: it
            // means "you may promote", never "requery" (CLAUDE.md).
            let now = frame.read(cx).versions();
            if now.flip != this.last_flip {
                this.last_flip = now.flip;
                this.promote(cx);
            }
            if !this.visible {
                return;
            }
            // `as_of` is the ONLY followed counter (spec §6.5): a scope
            // keystroke bumps `scope` on every character, and a CSV
            // publish bumps `data` for datasets this chart never reads —
            // neither may cost a series round trip.
            if !this.model.slots().is_empty() && this.follows_changed(now) {
                // An as-of moves the span's LEFT edge as well as its
                // right — `AsOf::At(t)` resolves to `(t − preset, t)`,
                // and live fetching never covered anything before
                // `now − preset` — so the gaps are asked for before the
                // points are (review round 1, I-1). The explicit clear
                // is load-bearing: `fetch_pending` drops the in-flight
                // set only when the RANGE moved, and an as-of change
                // leaves `Range` identical.
                //
                // Gated on a REAL as-of move, not on `follows_changed`:
                // that answers TRUE while
                // `acted` is `None` — a tile that has never asked a
                // query — so on a freshly shown tile whose first fetch
                // is still out, any frame notify at all (a scope
                // keystroke, say) re-marked every slot and asked for
                // each pair's span a second time. The `requery` below
                // stays on `follows_changed`, where "never asked" really
                // does mean "ask".
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

        // A fresh catalogue matters LIVE only while the picker's
        // identities stage is open (spec §9.6): a closed picker asks for
        // one on the way in, and the sources stage ranks over the
        // config, not the catalogue. This observer fires on EVERY
        // notification the entity emits (a source's health ticks about
        // twice a second with a diagnostics tile open), so the common
        // case is one `matches!` and nothing else, and even an open
        // picker compares the option list before touching the ranking —
        // re-ranking would move a highlight the trader had placed.
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
        }
    }

    // ---- what the shell reads ----------------------------------------

    /// `insert` exactly while a popup holds a text field (the picker,
    /// the expression field and the range popup);
    /// `normal` otherwise — plus the `popup` pair a fieldless popup
    /// adds. The series list is the fieldless one: it keeps the tile's
    /// own keyboard, so `j`/`k`/`enter`/`escape` reach the matcher as
    /// ordinary normal-mode keys and its fragment layer
    /// (`timeseries && mode == normal && popup == series`) is what tells
    /// them apart from `h`/`l` and the rest.
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

    /// Every show refetches and, once there is something to re-ask for,
    /// requeries; a hide cancels what is in flight (spec §9.10).
    ///
    /// The refetch is cheap by construction: the data tier subtracts the
    /// pair's existing coverage, so a span already held answers `Ok(0)`
    /// without touching the upstream, and the `Ok` is what sends the
    /// query. That is why a tile with no result yet does NOT requery
    /// here — its first paint always arrives through `SeriesFetched`,
    /// and asking before the fetch answers would only draw an empty
    /// chart a beat sooner.
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
            // And the cancelled request's own `acted` goes with it: it
            // records "this tile has already asked under these
            // versions", which is no longer true of anything that will
            // arrive. Left set, a tile hidden mid-round-trip comes back
            // deciding it is up to date.
            self.acted = None;
            self.query_in_flight = false;
            self.in_flight.clear();
        }
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// This tile has no find of its own: `/` belongs to the series
    /// list, which filters through its own `ChoiceList` field.
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
        // A notice belongs to the last action that ACTED (review round
        // 1, MIN-3): the next handled verb clears it before it can set
        // one of its own, but a verb this tile does NOT handle — a popup
        // verb with no popup open, and anything unrecognised — must
        // leave the text still on screen alone, or the state says
        // "cleared" while the trader reads the old line. So it is taken
        // here and put back on the two unhandled paths.
        let previous = self.notice.take();
        let n = count.unwrap_or(1).max(1) as usize;
        // A popup closes before any verb that is not its own (the
        // market-data panel's rule): a trader who pans, zooms or adds
        // with the list up meant the tile, not the list, and an overlay
        // left open over the answer is the confusing half.
        //
        // The keep-list is STAGE-AWARE: a
        // popup that holds the KEYBOARD keeps only its own four verbs.
        // Everything else — including the verbs the series list happily
        // stays open through — closes it first, because the palette can
        // dispatch any action over an open field (`ctrl+k` is a chord,
        // so it opens over one) and a verb that ran with the field still
        // installed would leave `key_context` reporting `insert` with
        // nothing focused: a tile deaf to every bare key until `escape`.
        //
        // `close_popup_with_window`, never a `Window`-less closer: a
        // popup whose field holds the keyboard must be blurred before it
        // is dropped (CLAUDE.md), and this is the path every such verb
        // reaches it by.
        let popup_survives = match &self.popup {
            None => true,
            Some(p) if p.is_insert() => {
                matches!(verb, "commit" | "cancel" | "insert_up" | "insert_down")
            }
            // The action menu keeps only its own keys: a pick closes it
            // itself before re-dispatching, so any other verb reaching
            // here came from the palette or a chord and means the tile.
            Some(Popup::Menu(_)) => matches!(
                verb,
                "menu" | "list_down" | "list_up" | "list_close" | "menu_pick"
            ),
            // The series list holds no field: a trader who cycles a
            // colour or an axis with it up meant the list to stay and
            // show the change (spec §9.5).
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
            // Every popup verb, through the one door (`popups.rs`).
            "add" | "expr" | "edit" | "list" | "range" | "list_down" | "list_up" | "list_close"
            | "commit" | "cancel" | "insert_up" | "insert_down" | "menu" | "menu_pick" => {
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

    /// `window` is unused: no `:` verb this tile has touches a popup or
    /// a field. It stays in the signature because
    /// [`TileContent::command`] is spelled that way for every module.
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

    /// A change to WHAT is plotted. The SESSION bit needs nothing here —
    /// the shell serialises on its own schedule.
    ///
    /// FETCH runs BEFORE QUERY, and a range change carries both: the
    /// gaps are asked for and the part already cached is re-queried in
    /// the same breath, so the chart repaints over what is held while
    /// the rest arrives (§9.10). An `add` carries FETCH without QUERY on
    /// purpose — its slot has no points yet, and `SeriesFetched Ok` is
    /// what sends the query.
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

    /// A pan, a zoom or a jump. Deliberately NOT `apply_changed`: the
    /// header does not read the view and neither does the chart model,
    /// so rebuilding either would bump `ChartModel::version` and throw
    /// away every cached path in `geode-chart` for a frame that only
    /// scrolled. It DOES requery when `changed.query()` says so: the
    /// percentiles and the density are computed over the VISIBLE window,
    /// so with either on a pan is a new question (and with both off, the
    /// model answers CHROME alone and nothing is asked).
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

    /// The one removal door `d` and `:remove` share — including the
    /// notice naming what went with the slot (spec §7: removing an
    /// operand removes every expression that reads it).
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
        // ABOVE the chart-key early return: the list's rows read the
        // model, the last result and the theme, none of which the chart
        // key covers on its own — an `axis_next` with the list open
        // moves a row's letter without touching a single chart input.
        if matches!(self.popup, Some(Popup::Series(_))) {
            let rows = SeriesPopup::prepare(
                &self.model,
                self.result.as_deref(),
                default_source.as_deref(),
                &colour_of,
            );
            self.popup = Some(Popup::Series(rows));
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
    /// The chart and its pointer surface (`tile::pointer`): the element
    /// itself, a zero-cost canvas that records the surface's bounds for
    /// the listeners, a resize-cursor band over the pane divider while
    /// there are two panes, and — only while a drag is armed — an
    /// occluding catcher that owns every move and release until the
    /// button comes up.
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
            header::render_empty(theme, &tile, tile_id).into_any_element()
        } else {
            self.render_chart_surface(&tile, tile_id, window)
                .into_any_element()
        };
        let menu_open = matches!(self.popup, Some(Popup::Menu(_)));
        // The popup is anchored off a zero-size, absolutely positioned
        // sibling at the header's own right edge (the market-data
        // panel's §6.1 placement) — `relative()` on the wrapper is what
        // makes that position read against the HEADER rather than the
        // window, and `deferred` inside it is what lifts the list above
        // the chart and the neighbouring tiles.
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
            // the body, below.
            Some(Popup::Expr(_)) | None => None,
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

/// The trader's clock offset, for the chart's displayed times — every
/// DISPLAYED time is local (Phase 4a ruling), while everything stored
/// and queried is UTC. The clock is the app's (`[time] zone`, as-of
/// dialog spec §6.1) through the `AppClock` global, never the machine's
/// own clock (banned by `geode_core::clock`'s sweep); `try_global`
/// because a module test fixture may never have installed it.
fn local_offset_secs(cx: &App) -> i32 {
    let clock = cx
        .try_global::<geode_shell::clock::AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
    clock.local(Utc::now()).offset().fix().local_minus_utc()
}

#[cfg(test)]
mod tests;
