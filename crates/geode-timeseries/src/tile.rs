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
//! and nowhere else, where it means "you may promote".

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Utc};
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
use gpui::prelude::*;
use gpui::{App, Context, ElementId, Entity, Hsla, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, Theme, v_flex};

use crate::commands::{self, Command};
use crate::core::model::{Changed, Colour, Model, SlotState};
use crate::core::{Range, chart, request, resolve, session};
use crate::header::{self, HeaderModel};
use crate::popup::{Popup, SeriesPopup, render_series_popup};

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
}

pub struct TimeseriesTile {
    id: TileId,
    frame: Entity<Frame>,
    /// Tasks 8–10: the catalog (a fetch source's identities) the add
    /// picker ranks over, and the health a slot's chip reports.
    #[allow(dead_code)]
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
    /// The tile's one overlay: the series list here, the add picker,
    /// the expression editor and the range dialog in Tasks 9–10. Its
    /// rows are PREPARED in `rebuild_chrome`, never formatted in
    /// `render`.
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
                // Gated on a REAL as-of move, not on `follows_changed`
                // (Task 8, folded review fix): that answers TRUE while
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
            0,
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
            result_seq: 0,
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
            in_flight: HashSet::new(),
            in_flight_range: None,
            notice: (!notices.is_empty()).then(|| notices.join("; ").into()),
            header,
            title,
            stack: None,
            popup: None,
            footer: header::footer_text(cx),
        }
    }

    // ---- what the shell reads ----------------------------------------

    /// `insert` exactly while a popup holds a text field (Tasks 9–10);
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
            self.in_flight.clear();
        }
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// This tile has no find; `/` belongs to the series list (Task 9).
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = (event, window, cx);
    }

    // ---- deliveries --------------------------------------------------

    /// A series answer for this tile's key.
    pub fn deliver(&mut self, outcome: SeriesOutcome, cx: &mut Context<Self>) {
        if outcome.tag != self.tag {
            // Stale: a newer request is out — and deliberately NOT an
            // arrival. A barrier waits for the versions this tile last
            // ACTED under, which is the newer request's; arriving here
            // would answer for a question still in flight, and that
            // outcome's own delivery is what answers it.
            return;
        }
        let acted = self.acted;
        match outcome.result {
            Ok(result) => {
                // Phase 4a §3.10: while a barrier still wants this key,
                // STAGE rather than paint. A chart's own answer may land
                // well before every blotter's, and a chart painting the
                // new as-of beside a blotter still on the old one is
                // exactly the half-updated screen the barrier exists to
                // prevent.
                let wants = acted.is_some_and(|acted| {
                    self.frame
                        .read(cx)
                        .barrier_wants(QueryKey(self.id.0), acted)
                });
                if wants {
                    let acted = acted.expect("`wants` is false without one");
                    self.staged = Some((result, acted));
                    // `arrived` may empty the barrier right here — when
                    // it does, promote at once rather than waiting for
                    // the `flip` bump to reach this tile's own observer
                    // on a later notify pass.
                    if self.arrive_and_release(cx) {
                        self.promote(cx);
                    }
                } else {
                    self.apply_result(result, cx);
                    self.arrive(cx);
                }
            }
            Err(e) => {
                // Last good stays on screen: a failed query says nothing
                // about the points already painted. It still counts as
                // an arrival — one broken tile must never hold every
                // other tile open until the deadline.
                self.notice = Some(e.into());
                self.arrive(cx);
            }
        }
        cx.notify();
    }

    /// A fetch finished for one `(source, identity)` pair. Keyed by the
    /// pair, so a tile that holds it marks every slot over it and a tile
    /// that does not is left alone by `set_pair_state`'s own `NONE`.
    ///
    /// Any `Ok` requeries — `Ok(0)` included, which means the span was
    /// already covered rather than that nothing is there.
    pub fn on_fetched(
        &mut self,
        source: &str,
        identity: &str,
        result: Result<u64, String>,
        cx: &mut Context<Self>,
    ) {
        // ABOVE the early return (review round 1, I-2): a pair this tile
        // no longer holds answers `NONE`, and leaving its entry behind
        // would make the set claim a fetch is still out for a pair that
        // could be re-added a moment later — which `fetch_pending` would
        // then skip, leaving a `Fetching` chip with nothing coming.
        self.in_flight
            .remove(&(source.to_string(), identity.to_string()));
        let state = match &result {
            Ok(_) => SlotState::Idle,
            Err(why) => SlotState::Failed(why.clone()),
        };
        let changed = self.model.set_pair_state(source, identity, state);
        if changed.is_none() {
            return;
        }
        if result.is_ok() && self.visible {
            self.requery(cx);
        }
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
        // A popup closes before any verb that is not its own (the
        // market-data panel's rule): a trader who pans, zooms or adds
        // with the list up meant the tile, not the list, and an overlay
        // left open over the answer is the confusing half.
        //
        // The keep-list carries Tasks 9–10's verbs already — the field
        // popups' `commit`/`cancel`/`insert_*` and the three that OPEN
        // one — so those tasks add a variant and its handler without
        // touching this gate. `close_popup_with_window`, never a
        // `Window`-less closer: a popup whose field holds the keyboard
        // must be blurred before it is dropped (CLAUDE.md), and this is
        // the path every such verb reaches it by.
        if self.popup.is_some()
            && !matches!(
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
                    | "commit"
                    | "cancel"
                    | "insert_up"
                    | "insert_down"
            )
        {
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

    /// Every popup verb. The series list is built here; Tasks 9–10 add
    /// the add picker, the expression editor and the range dialog, whose
    /// verbs still answer `false`.
    ///
    /// `e`'s refusal on a SOURCE slot stays this method's: there is no
    /// expression to open, and a trader who pressed it deserves the
    /// reason rather than a dead key.
    fn popup_verb(
        &mut self,
        verb: &str,
        n: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let series_open = matches!(self.popup, Some(Popup::Series(_)));
        match verb {
            // A second `L` closes it — one key for both halves, the way
            // the market-data menu's own `menu` verb toggles.
            "list" => {
                if self.popup.is_some() {
                    self.close_popup_with_window(window, cx);
                } else {
                    self.open_series_popup(cx);
                }
                true
            }
            // The list's cursor IS the chips' cursor, so `j`/`k` are
            // `tab`/`shift+tab` under another name — and wrap the same
            // way. Guarded on the list being open: the fragment binds
            // them only there, but the palette can reach any action.
            "list_down" if series_open => {
                let changed = self.model.cursor_next(n);
                self.apply_changed(changed, cx);
                true
            }
            "list_up" if series_open => {
                let changed = self.model.cursor_prev(n);
                self.apply_changed(changed, cx);
                true
            }
            "list_close" if series_open => {
                self.close_popup_with_window(window, cx);
                true
            }
            "edit" => {
                if let Some(number) = header::cursor_is_source(&self.model) {
                    self.notice = Some(format!("s{number} is not an expression").into());
                    cx.notify();
                }
                false
            }
            _ => false,
        }
    }

    /// Open the series list (spec §9.5). The rows are prepared by the
    /// ONE door that prepares every other piece of chrome, so an empty
    /// popup can never be painted: `rebuild_chrome` fills it in the same
    /// update it is opened in.
    fn open_series_popup(&mut self, cx: &mut Context<Self>) {
        self.popup = Some(Popup::Series(SeriesPopup::default()));
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// The ONE closer (the market-data panel's rule): every path that
    /// drops a popup comes through here, because a popup whose own field
    /// holds the keyboard has to be blurred BEFORE it is dropped — an
    /// unblurred dead handle leaves `Window::focused` pointing at
    /// nothing for the rest of the session, and the shell's focus-return
    /// net never fires.
    ///
    /// The series list holds no field, so today the blur is a no-op;
    /// Tasks 9–10's variants are what make the `window` parameter earn
    /// its keep, and the door exists now so they add an arm rather than
    /// a second closer.
    pub(crate) fn close_popup_with_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let own_field_focused = match &self.popup {
            Some(Popup::Series(_)) | None => false,
        };
        if own_field_focused {
            window.blur(cx);
        }
        self.popup = None;
        cx.notify();
    }

    /// The mouse's form of `tab` (spec §9.3): a chip click moves the
    /// cursor onto its slot — and so does a click on the series list's
    /// row, which is the same slot under another painting. Whatever
    /// popup is open STAYS open: the list's own highlight is this
    /// cursor, so a row click that closed it would take the thing it
    /// just moved off the screen.
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

    // ---- the data flow -----------------------------------------------

    /// Ask for every source slot that is waiting for data and has no
    /// fetch out already (see [`Self::in_flight`]), one request per
    /// PAIR: two slots over the same `identity@source` are one span.
    ///
    /// The whole visible range is asked for every time; the data tier
    /// subtracts what a pair already covers and queues one span per gap,
    /// so re-asking costs a round trip to `DataService` and nothing
    /// upstream.
    fn fetch_pending(&mut self, cx: &mut Context<Self>) {
        if self.in_flight_range.as_ref() != Some(self.model.range()) {
            self.in_flight.clear();
            self.in_flight_range = Some(self.model.range().clone());
        }
        let (now, as_of) = self.now_and_as_of(cx);
        let (from, to) = self.model.range().resolve(now, &as_of);
        let mut pending: Vec<(String, String)> = Vec::new();
        for slot in self.model.slots() {
            let SlotKind::Source {
                source, identity, ..
            } = &slot.kind
            else {
                continue;
            };
            if slot.state != SlotState::Fetching {
                continue;
            }
            let pair = (source.clone(), identity.clone());
            if self.in_flight.contains(&pair) || pending.contains(&pair) {
                continue;
            }
            pending.push(pair);
        }
        for (source, identity) in pending {
            let queued = self.data.fetch(FetchParams {
                key: QueryKey(self.id.0),
                source: source.clone(),
                identity: identity.clone(),
                from,
                to,
            });
            if queued {
                self.in_flight.insert((source, identity));
            } else {
                // Nothing is coming, and a chip left `Fetching` for ever
                // would say the opposite.
                self.model.set_pair_state(
                    &source,
                    &identity,
                    SlotState::Failed("fetch refused: the data service is busy or gone".into()),
                );
            }
        }
    }

    /// Submit this tile's series request, keyed by the tile so two
    /// charts never supersede each other. The stats ride over the
    /// VISIBLE window and the points over the whole range, which is what
    /// `request::params` builds from the current result's buckets.
    /// Drop every in-flight entry for a pair the model no longer holds
    /// (review round 1, I-2). Called wherever slots LEAVE — `remove`
    /// (which takes an operand's dependants with it) and `:clear` —
    /// because an answer for a pair the tile has dropped never clears
    /// its own entry through the model, and a stale entry is
    /// indistinguishable from a live fetch: the same pair, re-added,
    /// would be skipped for the tile's whole life.
    fn prune_in_flight(&mut self) {
        let model = &self.model;
        self.in_flight
            .retain(|(source, identity)| model.holds_pair(source, identity));
    }

    fn requery(&mut self, cx: &mut Context<Self>) {
        // A fresh question supersedes whatever was staged for the old
        // one, whether or not `promote`'s own version check would have
        // caught it.
        self.staged = None;
        let (as_of, versions) = {
            let frame = self.frame.read(cx);
            (frame.as_of().clone(), frame.versions())
        };
        self.tag += 1;
        self.acted = Some(versions);
        let params = {
            let buckets = self.result.as_ref().map(|r| r.buckets.as_slice());
            request::params(
                &self.model,
                QueryKey(self.id.0),
                self.tag,
                Utc::now(),
                &as_of,
                buckets.unwrap_or(&[]),
            )
        };
        let submitted = match params {
            Some(params) => {
                let queued = self.data.series(params);
                if !queued {
                    self.notice =
                        Some("series request refused: the data service is busy or gone".into());
                }
                queued
            }
            // Nothing to ask about (no slot, or no dataset yet).
            None => false,
        };
        if !submitted {
            // Nothing is coming: arrive, or an open barrier holds every
            // other tile to the 250 ms deadline waiting for an outcome
            // that will never exist — then clear `acted`, so the next
            // frame change retries rather than deciding this tile is
            // already up to date. In that order: `arrive` reads `acted`.
            self.arrive(cx);
            self.acted = None;
        }
        cx.notify();
    }

    /// Whether the frame has moved in a way a series request depends on.
    /// `None` (nothing asked yet) is always a change.
    fn follows_changed(&self, now: FrameVersions) -> bool {
        let Some(acted) = self.acted else {
            return true;
        };
        Self::differs_on_followed(acted, now)
    }

    /// `as_of` and nothing else (spec §6.5). The one comparison
    /// [`Self::follows_changed`] and [`Self::promote`]'s own gate both go
    /// through, so "what this tile requeries for" and "what invalidates
    /// something it has already staged" cannot drift apart.
    fn differs_on_followed(versions: FrameVersions, now: FrameVersions) -> bool {
        versions.as_of != now.as_of
    }

    /// Answer an open flip barrier for a change this tile is NOT going to
    /// requery for (a scope or grouping bump, or no slot to ask about).
    ///
    /// `ShellView::visible_tile_keys` cannot know which tiles follow
    /// which counters, so every visible occupant is in the barrier's key
    /// set. Left unanswered, this tile would hold every blotter on
    /// screen open until `FLIP_DEADLINE` — 250 ms — on every scope
    /// keystroke, with nothing of its own coming.
    fn self_arrive(&mut self, now: FrameVersions, cx: &mut Context<Self>) {
        let key = QueryKey(self.id.0);
        if self.frame.read(cx).barrier_wants(key, now) {
            self.frame.update(cx, |f, cx| {
                if f.arrived(key, now) {
                    cx.notify();
                }
            });
        }
    }

    /// Tell an open barrier this tile's own outcome has landed, under the
    /// versions the request was made with — a failed outcome counts too.
    fn arrive(&mut self, cx: &mut Context<Self>) {
        let _ = self.arrive_and_release(cx);
    }

    /// [`Self::arrive`], answering whether this arrival is what EMPTIED
    /// the barrier — the caller uses that to promote its own staged
    /// result at once rather than waiting for the `flip` bump to reach
    /// its observer on a later notify pass.
    fn arrive_and_release(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(acted) = self.acted else {
            return false;
        };
        let key = QueryKey(self.id.0);
        self.frame.update(cx, |f, cx| {
            let released = f.arrived(key, acted);
            if released {
                cx.notify();
            }
            released
        })
    }

    /// Put a staged result on screen once the flip released it — unless a
    /// counter this tile follows moved under it, in which case it answers
    /// a question nobody is asking any more (reachable while hidden,
    /// where no requery replaces it).
    fn promote(&mut self, cx: &mut Context<Self>) {
        let Some((result, versions)) = self.staged.take() else {
            return;
        };
        if !Self::differs_on_followed(versions, self.frame.read(cx).versions()) {
            self.apply_result(result, cx);
            cx.notify();
        }
    }

    /// Install a delivered result: the new full extent, the view, the
    /// points and the chart model built from them.
    fn apply_result(&mut self, result: SeriesResult, cx: &mut Context<Self>) {
        let full = self.full_of(&result);
        self.model.set_full(full);
        if std::mem::take(&mut self.reset_view) {
            self.model.reset_view();
        }
        // Assigned OVER the old `Arc`, never through a `None` first: a
        // tile that dropped its only result mid-update would paint an
        // empty chart on any frame drawn in between.
        self.result_seq += 1;
        self.result = Some(Arc::new(result));
        self.notice = None;
        self.rebuild_chrome(cx);
    }

    /// The x extent a result spans, in the units the current axis mode
    /// counts in: bucket INDICES under a session axis, epoch micros
    /// under a continuous one (where the last bucket's own width is part
    /// of the extent, since a bucket is drawn from its start).
    fn full_of(&self, result: &SeriesResult) -> (f64, f64) {
        match self.model.axis_mode() {
            AxisMode::Session => (0.0, result.buckets.len() as f64),
            AxisMode::Continuous => {
                let step = (self.model.frequency().seconds() * 1_000_000) as f64;
                (
                    result.buckets.first().copied().unwrap_or(0) as f64,
                    result.buckets.last().copied().unwrap_or(0) as f64 + step,
                )
            }
        }
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
        let key = chart_key(
            &self.model,
            self.result_seq,
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
        // The popup is anchored off a zero-size, absolutely positioned
        // sibling at the header's own right edge (the market-data
        // panel's §6.1 placement) — `relative()` on the wrapper is what
        // makes that position read against the HEADER rather than the
        // window, and `deferred` inside it is what lifts the list above
        // the chart and the neighbouring tiles.
        let popup = self.popup.as_ref().map(|p| match p {
            Popup::Series(s) => render_series_popup(s, self.header.cursor, &tile, tile_id, cx),
        });
        let header = div()
            .relative()
            .w_full()
            .child(header::render_header(
                &self.header,
                theme,
                &tile,
                tile_id,
                self.stack.as_ref(),
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
            .child(body)
            .child(header::render_footer(self.footer.clone(), theme))
    }
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
    use crate::popup::SeriesRow;
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
            let id = ActionId(format!("timeseries::{verb}"));
            vcx.update(|window, cx| self.content.dispatch(&id, count, window, cx));
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
        fn deliver_series(
            &self,
            vcx: &mut gpui::VisualTestContext,
            tag: u64,
            result: SeriesResult,
        ) {
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
        fn acted(
            &self,
            vcx: &gpui::VisualTestContext,
        ) -> Option<geode_shell::frame::FrameVersions> {
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
                None => Vec::new(),
            })
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
    fn click_at(
        vcx: &mut gpui::VisualTestContext,
        at: gpui::Point<gpui::Pixels>,
        click_count: usize,
    ) {
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
        // (`list` is no longer one of those: Task 8 built it, and a
        // handled verb clears the notice — `add` and `range` are the
        // ones still waiting for Tasks 9–10.)
        h.dispatch(&mut vcx, "add", None);
        h.dispatch(&mut vcx, "range", None);
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
        assert!(
            matches!(&h.model(&vcx).slots()[0].state, SlotState::Failed(e) if e == "kdb: timeout")
        );
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
        // span (Task 8, folded review fix): `acted` is `None` — this
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
    fn a_delivery_becomes_the_chart_model_and_a_stale_tag_is_dropped(
        cx: &mut gpui::TestAppContext,
    ) {
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
    fn a_query_change_requeries_and_a_range_change_fetches_and_queries(
        cx: &mut gpui::TestAppContext,
    ) {
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
    fn shift_l_opens_the_series_popup_whose_cursor_is_the_chips_cursor(
        cx: &mut gpui::TestAppContext,
    ) {
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
        h.dispatch(&mut vcx, "list", None);
        assert!(h.popup_is_series(&vcx));
        h.dispatch(&mut vcx, "list", None);
        assert!(h.popup_is_none(&vcx), "a second L closes it");
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
}
